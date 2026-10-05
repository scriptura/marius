// crates/shell/render/src/regenerate.rs

//! Interface d'Écriture AOT & Régénération des Packfiles HTML (`regenerate_and_swap`).
//!
//! Exécute la seconde étape du pipeline réactif (*Étage 2*) : lit le `store.bin`
//! fraîchement mis à jour par l'Étage 1 (`ingest_and_swap`), applique les deltas
//! de rendu HTML, et permute atomiquement l'index de lecture (`LiveRegistry`).
//!
//! ## Evolution de Design : Couplage au Pipeline CoW (Phase 4.2)
//!
//! - **Stratégie de Fusion Incrementale (*Sweep Merge*) :** Les identifiants transmis (`ids`)
//!   représentent exclusivement le delta d'un tick (flush du `Collector`), et non l'intégralité
//!   de la table. Au lieu de réécrire le packfile HTML de zéro, le pipeline fusionne
//!   le delta rendu avec l'ancien packfile via `sweep::merge_sweep`. Les entités inertes sont
//!   conservées intactes par copie de bloc sans passer par le pipeline de rendu.
//! - **Découpage Strict Asynchrone / Synchrone :**
//!   - `regenerate_and_swap` : Enveloppe `async` dédiée aux I/O asynchrones (requêtes SQLx via `P::fetch_batch`).
//!   - `apply_merge_io_sync` : Noyau d'exécution physique **strictement synchrone**, sans dépendance envers le runtime Tokio.
//!     Isole le cycle complet d'I/O disque (`ftruncate`, *mmap*, `merge_sweep`, alignement, footer, `fsync`/`msync`, `rename`).
//!     Cette isolation prépare l'encapsulation directe dans un thread dédié (`spawn_blocking`) sans altérer la signature métier.
//!
//! ## Écarts Documentés par Rapport à la Spécification
//!
//! 1. **Nommage de la Récupération :** Utilise `P::fetch_batch` (nom réel sur le trait `Projection`) au lieu de `P::fetch_from_pg`.
//! 2. **Signature du Footer :** Incorpore explicitement `blob_len` dans `write_packfile_footer(writer, blob_len, index)`.
//! 3. **Encapsulation du Registre :** La mise à jour du registre de lecture s'effectue via l'interface publique
//!    `registry.store(packfile_key, Arc::new(new_index))` au lieu de manipuler directement le champ interne `indices`.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use marius_projection::Projection;

use crate::batch_renderer::BatchRenderer;
use crate::pack_html_format::{PackfileEntry, PackfileFooter, write_packfile_footer};
use crate::pack_html_index::PackHtmlIndex;
use crate::registry::{LiveRegistry, packfile_path_for};
use crate::sweep::{DeltaBatch, DeltaEntry, merge_sweep};

/// Taille de chunk pour le streaming fetch_batch → render_batch. Borne la
/// clause SQL IN côté fetch_batch ; sans incidence sur le format produit —
/// tous les chunks alimentent le même buffer delta continu (cf.
/// `chained_batches_offsets_are_contiguous`, batch_renderer.rs).
const CHUNK_SIZE: usize = 1024;

/// Régénère un packfile HTML en fusionnant le delta du tick courant avec la
/// génération actuellement servie, puis bascule atomiquement le
/// `LiveRegistry` vers la nouvelle version.
///
/// `ids` : DELTA du tick courant (Collector::flush(), sémantique confirmée
/// Phase 4.2) — entités insérées, modifiées ou supprimées depuis le dernier
/// appel. Aucune contrainte de tri sur `ids` lui-même : le tri requis par
/// `merge_sweep` (C1) est reconstruit à l'intérieur de cette fonction,
/// indépendamment de l'ordre de production du delta côté Collector.
///
/// Panique si `packfile_key` n'a jamais été provisionné à la construction
/// du `LiveRegistry` — invariant AOT existant (`LiveRegistry::store`), pas
/// contourné ici par un `Result` silencieux : une clé absente est un bug
/// d'intégration, pas une erreur de requête.
///
/// `io_semaphore` : régule l'I/O disque (risque de dirty-page storm),
/// partagé entre tous les `Dispatcher` — singleton créé une fois en amont
/// (main.rs), jamais reconstruit ici. Portée du permis : juste avant
/// `spawn_blocking`, jamais avant le fetch Postgres (décision Phase 4.3,
/// point 1 — le fetch réseau n'a aucun rapport avec la pression disque que
/// ce sémaphore régule).
pub async fn regenerate_and_swap<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
    packfile_key: &'static str,
    registry: &LiveRegistry,
    io_semaphore: &tokio::sync::Semaphore,
) -> io::Result<()> {
    let final_path = packfile_path_for(packfile_key);
    let tmp_path = final_path.with_extension("tmp");

    // Cas dump initial : artifacts/ peut ne pas encore exister.
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Échec rapide et explicite plutôt qu'un Err qui contournerait
    // l'invariant déjà posé par LiveRegistry::store (même discipline que le
    // test `regenerate_and_swap_panics_on_unprovisioned_key`, Jalon 4a).
    let old = registry.load(packfile_key).unwrap_or_else(|| {
        panic!(
            "regenerate_and_swap: clé \"{packfile_key}\" absente de la topologie figée \
             à la construction — violation de l'invariant AOT (clé non provisionnée \
             par with_indices()/cold_start())"
        )
    });

    // ---- Segment 1 — fetch réseau Postgres. Hors périmètre du sémaphore. ---
    let t_fetch = Instant::now();
    let delta = fetch_delta_batch::<P>(pool, ids, total_cap).await?;
    let fetch_elapsed = t_fetch.elapsed();

    // ---- Segment 2 — attente du permis : signal de backpressure (ADR-002).
    // t0 côté Dispatcher::run() englobe cette attente par conception (le
    // Dispatcher est un filtre passe-bas sur l'amplification d'écriture
    // globale, pas une mesure du coût CPU propre du shard) ; décomposée ici
    // séparément pour le diagnostic uniquement.
    let t_wait = Instant::now();
    let _permit = io_semaphore
        .acquire()
        .await
        .map_err(|_| io::Error::other("io_semaphore fermé de manière inattendue"))?;
    let wait_io_elapsed = t_wait.elapsed();

    // ---- Segment 3 — noyau synchrone (Phase 4.2, boîte noire, inchangé)
    // déporté sur le pool de threads bloquants. `_permit` est tenu jusqu'à
    // la sortie de portée naturelle de ce bloc — après le `.await`, succès
    // ou erreur — pas de libération manuelle.
    let t_merge = Instant::now();
    let new_index = tokio::task::spawn_blocking(move || {
        apply_merge_io_sync(old.as_ref(), &delta, &tmp_path, &final_path)
    })
    .await
    .map_err(io::Error::other)??;
    let merge_io_elapsed = t_merge.elapsed();

    // Dernière étape, sans exception : tout Err ci-dessus (fetch, permis,
    // JoinError, I/O, fsync, rename, réouverture) retourne avant cette
    // ligne — l'ancien Arc reste servi, aucune requête en vol n'est
    // interrompue.
    registry.store(packfile_key, Arc::new(new_index));

    // Instrumentation diagnostic. Aucun import `tracing` ni appel
    // `tracing_subscriber::...::init()` détecté dans les deux fichiers
    // fournis à cette session (dispatcher.rs, regenerate.rs) — eprintln!
    // provisoire en conséquence. Si `tracing` est câblé ailleurs dans le
    // crate (main.rs ou un autre module non fourni), remplacer par
    // `tracing::debug!` à champs structurés.
    // TODO: migrer vers tracing une fois confirmé câblé dans le crate.
    let total = fetch_elapsed + wait_io_elapsed + merge_io_elapsed;
    eprintln!(
        "[{packfile_key}] total: {}ms (fetch: {}ms, wait_io: {}ms, merge_io: {}ms)",
        total.as_millis(),
        fetch_elapsed.as_millis(),
        wait_io_elapsed.as_millis(),
        merge_io_elapsed.as_millis(),
    );

    Ok(())
}

/// Construit le `DeltaBatch` (payload local + entries triées) depuis
/// PostgreSQL — seule section `async` de cette session.
///
/// Contrat de détection des suppressions (décision actée Phase 4.2,
/// résolution Blocage 2) : tout id de `ids` absent du résultat de
/// `P::fetch_batch` est une suppression — émis comme
/// `DeltaEntry { offset: 0, length: 0 }`, sentinelle déjà consommée par
/// `merge_sweep` (sweep.rs, branche `d.length == 0`).
///
/// `payload_writer` est un `BufWriter<Vec<u8>>` : obligation de signature de
/// `BatchRenderer::render_batch` (`&mut BufWriter<W>`, pas `&mut W`), pas un
/// choix de performance — sur un `Vec<u8>` en mémoire, le tampon de
/// `BufWriter` n'élimine aucun syscall, juste une indirection supplémentaire
/// déjà présente dans l'API consommée telle quelle.
async fn fetch_delta_batch<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
) -> io::Result<DeltaBatch> {
    let mut payload_writer = BufWriter::new(Vec::<u8>::new());
    let mut renderer = BatchRenderer::<P>::new(total_cap, ids.len().min(CHUNK_SIZE));
    let mut payload_index: Vec<PackfileEntry> = Vec::with_capacity(ids.len());
    let mut offset = 0u64;

    for chunk in ids.chunks(CHUNK_SIZE) {
        let batch = P::fetch_batch(pool, chunk)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        offset = renderer.render_batch(&batch, &mut payload_writer, offset)?;
        payload_index.extend_from_slice(renderer.index());
        renderer.reset(CHUNK_SIZE);
    }

    let payload = payload_writer
        .into_inner()
        .map_err(|e| io::Error::other(e.to_string()))?;

    Ok(build_delta_batch(payload_index, payload, ids))
}

/// Construit un `DeltaBatch` depuis un index physique déjà rendu (entries
/// triées C1 + détection des suppressions) — factorisé hors de
/// `fetch_delta_batch` (V2d) pour être réutilisé, à l'identique, par
/// `fetch_delta_batches_with_volatile_split` ci-dessous : même règle de
/// détection des suppressions pour le monolithique et pour head/tail,
/// jamais deux implémentations divergentes de la même logique.
fn build_delta_batch(payload_index: Vec<PackfileEntry>, payload: Vec<u8>, ids: &[i64]) -> DeltaBatch {
    let mut entries: Vec<DeltaEntry> = Vec::with_capacity(ids.len());
    for entry in &payload_index {
        debug_assert!(
            entry.offset <= u32::MAX as u64,
            "delta payload > 4 GiB sur un seul tick — hors hypothèse de \
             dimensionnement (DeltaEntry.offset est u32, local au buffer delta)"
        );
        entries.push(DeltaEntry {
            entity_id: entry.id,
            offset: entry.offset as u32,
            length: entry.len,
        });
    }

    // Suppressions : tout id demandé mais absent du résultat PostgreSQL.
    let present: HashSet<i64> = payload_index.iter().map(|e| e.id).collect();
    for &id in ids {
        if !present.contains(&id) {
            entries.push(DeltaEntry {
                entity_id: id,
                offset: 0,
                length: 0,
            });
        }
    }

    // C1 (sweep.rs) : delta.entries strictement trié par entity_id croissant.
    entries.sort_unstable_by_key(|e| e.entity_id);

    DeltaBatch { entries, payload }
}

// =============================================================================
// Région volatile (V2c/V2d) — extension minimale, PAS un moteur générique
// =============================================================================
//
// Exactement DEUX cibles supplémentaires fixes (head, tail), jamais une
// liste arbitraire de N artefacts : `SplitRenderTarget` est utilisé en
// paire (`Option<(SplitRenderTarget<P>, SplitRenderTarget<P>)>`), jamais en
// `Vec`. `fetch_delta_batches_with_volatile_split` appelle `P::fetch_batch`
// UNE fois par chunk — jamais une seconde ingestion pour head/tail : les
// trois rendus (monolithique + head + tail) partagent le même `batch`
// (`&[(P::Record, P::VarlenOwned)]`, emprunté, jamais cloné) déjà fetché.

/// Une cible de rendu supplémentaire (head OU tail) — clé de packfile,
/// capacité, fonction de rendu LIBRE (jamais une méthode du trait
/// `Projection` : `render_head`/`render_tail` restent hors trait, décision
/// V2c §5 — le trait reste celui du composant/table, inchangé).
pub struct SplitRenderTarget<P: Projection> {
    pub packfile_key: &'static str,
    pub total_cap: usize,
    pub render: fn(&P::Record, &P::VarlenOwned, &mut String),
}

/// Pendant minimal de `BatchRenderer` pour une fonction de rendu simple
/// (`String` direct, aucun `RenderChunk`) — jamais un remplacement de
/// `BatchRenderer` (celui-ci reste inchangé, utilisé tel quel pour le
/// rendu monolithique ci-dessous). `render_head`/`render_tail` n'ont pas
/// besoin de la mécanique segments empruntés/bufferisés de
/// `P::render_chunks` (V2c : aucune des deux moitiés ne porte le champ
/// `marius:large_content` en zéro-copie — documenté comme hors périmètre
/// du vertical slice, pas un oubli).
struct SimpleBatchRenderer<P: Projection> {
    buf: String,
    index: Vec<PackfileEntry>,
    _proj: PhantomData<P>,
}

impl<P: Projection> SimpleBatchRenderer<P> {
    fn new(total_cap: usize, batch_len: usize) -> Self {
        Self {
            buf: String::with_capacity(total_cap),
            index: Vec::with_capacity(batch_len),
            _proj: PhantomData,
        }
    }

    fn render_batch<W: Write>(
        &mut self,
        records: &[(P::Record, P::VarlenOwned)],
        render: fn(&P::Record, &P::VarlenOwned, &mut String),
        writer: &mut BufWriter<W>,
        offset_start: u64,
    ) -> io::Result<u64> {
        let mut offset = offset_start;
        for (record, varlena) in records {
            self.buf.clear();
            render(record, varlena, &mut self.buf);
            let bytes = self.buf.as_bytes();
            writer.write_all(bytes)?;
            let len = bytes.len() as u32;
            self.index.push(PackfileEntry {
                id: P::record_id(record),
                offset,
                len,
                _pad: [0u8; 4],
            });
            offset += len as u64;
        }
        Ok(offset)
    }

    fn reset(&mut self, next_batch_len: usize) {
        self.index.clear();
        if self.index.capacity() < next_batch_len {
            self.index.reserve(next_batch_len - self.index.capacity());
        }
    }

    fn index(&self) -> &[PackfileEntry] {
        &self.index
    }
}

/// Accumulateurs head/tail — un seul champ (`Option`) plutôt que deux
/// paramètres séparés, pour ne jamais désynchroniser la paire pendant la
/// boucle de chunks ci-dessous.
struct SplitAccumulator<P: Projection> {
    head_writer: BufWriter<Vec<u8>>,
    head_renderer: SimpleBatchRenderer<P>,
    head_index: Vec<PackfileEntry>,
    head_offset: u64,
    tail_writer: BufWriter<Vec<u8>>,
    tail_renderer: SimpleBatchRenderer<P>,
    tail_index: Vec<PackfileEntry>,
    tail_offset: u64,
}

/// Pendant de `fetch_delta_batch` — même contrat (une ingestion par chunk,
/// suppressions détectées de façon identique via `build_delta_batch`),
/// étendu pour produire, en plus du `DeltaBatch` monolithique, les deux
/// `DeltaBatch` head/tail quand `volatile_split` est fourni — à partir du
/// MÊME `batch` fetché, jamais d'un second appel à `P::fetch_batch`.
async fn fetch_delta_batches_with_volatile_split<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
    volatile_split: Option<&(SplitRenderTarget<P>, SplitRenderTarget<P>)>,
) -> io::Result<(DeltaBatch, Option<(DeltaBatch, DeltaBatch)>)> {
    let mut mono_writer = BufWriter::new(Vec::<u8>::new());
    let mut mono_renderer = BatchRenderer::<P>::new(total_cap, ids.len().min(CHUNK_SIZE));
    let mut mono_index: Vec<PackfileEntry> = Vec::with_capacity(ids.len());
    let mut mono_offset = 0u64;

    let mut split_acc: Option<SplitAccumulator<P>> = volatile_split.map(|(head, tail)| {
        let batch_len = ids.len().min(CHUNK_SIZE);
        SplitAccumulator {
            head_writer: BufWriter::new(Vec::<u8>::new()),
            head_renderer: SimpleBatchRenderer::new(head.total_cap, batch_len),
            head_index: Vec::with_capacity(ids.len()),
            head_offset: 0,
            tail_writer: BufWriter::new(Vec::<u8>::new()),
            tail_renderer: SimpleBatchRenderer::new(tail.total_cap, batch_len),
            tail_index: Vec::with_capacity(ids.len()),
            tail_offset: 0,
        }
    });

    for chunk in ids.chunks(CHUNK_SIZE) {
        // Ingestion UNIQUE pour ce chunk, partagée par les (jusqu'à) trois
        // rendus ci-dessous — `&batch` est emprunté par chaque
        // `render_batch`, jamais cloné, jamais refetché (contrainte V2d
        // impérative : une seule acquisition des données source).
        let batch = P::fetch_batch(pool, chunk)
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;

        mono_offset = mono_renderer.render_batch(&batch, &mut mono_writer, mono_offset)?;
        mono_index.extend_from_slice(mono_renderer.index());
        mono_renderer.reset(CHUNK_SIZE);

        if let (Some((head, tail)), Some(acc)) = (volatile_split, split_acc.as_mut()) {
            acc.head_offset =
                acc.head_renderer
                    .render_batch(&batch, head.render, &mut acc.head_writer, acc.head_offset)?;
            acc.head_index.extend_from_slice(acc.head_renderer.index());
            acc.head_renderer.reset(CHUNK_SIZE);

            acc.tail_offset =
                acc.tail_renderer
                    .render_batch(&batch, tail.render, &mut acc.tail_writer, acc.tail_offset)?;
            acc.tail_index.extend_from_slice(acc.tail_renderer.index());
            acc.tail_renderer.reset(CHUNK_SIZE);
        }
    }

    let mono_payload = mono_writer
        .into_inner()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mono_delta = build_delta_batch(mono_index, mono_payload, ids);

    let split_delta = match split_acc {
        None => None,
        Some(acc) => {
            let head_payload = acc
                .head_writer
                .into_inner()
                .map_err(|e| io::Error::other(e.to_string()))?;
            let tail_payload = acc
                .tail_writer
                .into_inner()
                .map_err(|e| io::Error::other(e.to_string()))?;
            Some((
                build_delta_batch(acc.head_index, head_payload, ids),
                build_delta_batch(acc.tail_index, tail_payload, ids),
            ))
        }
    };

    Ok((mono_delta, split_delta))
}

/// Une clé déjà résolue (chemin final, chemin temporaire, ancienne
/// génération) — même résolution que `regenerate_and_swap` (§ ci-dessus),
/// faite une fois par clé avant tout fetch réseau (fail-fast identique).
struct ResolvedTarget {
    packfile_key: &'static str,
    final_path: PathBuf,
    tmp_path: PathBuf,
    old: Arc<PackHtmlIndex>,
}

fn resolve_target(packfile_key: &'static str, registry: &LiveRegistry) -> io::Result<ResolvedTarget> {
    let final_path = packfile_path_for(packfile_key);
    let tmp_path = final_path.with_extension("tmp");
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let old = registry.load(packfile_key).unwrap_or_else(|| {
        panic!(
            "regenerate_and_swap_with_volatile_split: clé \"{packfile_key}\" absente de la \
             topologie figée à la construction — violation de l'invariant AOT (clé non \
             provisionnée par with_indices()/cold_start())"
        )
    });
    Ok(ResolvedTarget {
        packfile_key,
        final_path,
        tmp_path,
        old,
    })
}

/// Extension minimale du vertical slice Volatile (V2c/V2d) : régénère
/// l'artefact monolithique de `P` (comportement STRICTEMENT identique à
/// `regenerate_and_swap` ci-dessus quand `volatile_split` est `None` — K=1
/// inchangé) et, si `volatile_split` est fourni, les deux artefacts
/// head/tail associés — à partir d'une SEULE série d'appels à
/// `P::fetch_batch` (jamais une seconde ingestion, cf.
/// `fetch_delta_batches_with_volatile_split`).
///
/// PAS un mécanisme générique « N artifacts » : exactement zéro ou deux
/// cibles supplémentaires, jamais une liste. Le trait `Projection` reste
/// celui du composant/table actuel — aucune `ContentCoreHeadProjection`/
/// `ContentCoreTailProjection` introduite.
#[allow(clippy::too_many_arguments)]
pub async fn regenerate_and_swap_with_volatile_split<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
    packfile_key: &'static str,
    volatile_split: Option<&(SplitRenderTarget<P>, SplitRenderTarget<P>)>,
    registry: &LiveRegistry,
    io_semaphore: &tokio::sync::Semaphore,
) -> io::Result<()> {
    // ---- Résolution des clés — avant tout fetch réseau (fail-fast) --------
    let mono_target = resolve_target(packfile_key, registry)?;
    let split_targets = match volatile_split {
        None => None,
        Some((head, tail)) => Some((
            resolve_target(head.packfile_key, registry)?,
            resolve_target(tail.packfile_key, registry)?,
        )),
    };

    // ---- Segment 1 — fetch réseau Postgres, UNE fois pour les (jusqu'à)
    // trois cibles. Hors périmètre du sémaphore, même discipline que
    // regenerate_and_swap.
    let (mono_delta, split_delta) = fetch_delta_batches_with_volatile_split::<P>(
        pool,
        ids,
        total_cap,
        volatile_split,
    )
    .await?;

    // ---- Segment 2 — attente du permis : un seul acquire() pour l'écriture
    // physique des (jusqu'à) trois packfiles de ce tick, pas un par packfile
    // — même granularité de backpressure que le K=1 existant (un tick =
    // une acquisition), pas une nouvelle politique introduite ici.
    let _permit = io_semaphore
        .acquire()
        .await
        .map_err(|_| io::Error::other("io_semaphore fermé de manière inattendue"))?;

    // ---- Segment 3 — noyau synchrone, déporté sur spawn_blocking. Les
    // (jusqu'à) trois fusions/écritures sont indépendantes (packfiles
    // distincts) — regroupées dans le même spawn_blocking pour rester sous
    // le même permis que segment 2, jamais une politique de concurrence
    // nouvelle entre elles.
    let new_indices = tokio::task::spawn_blocking(move || -> io::Result<Vec<(&'static str, PackHtmlIndex)>> {
        let mut results = Vec::with_capacity(1 + split_targets.as_ref().map_or(0, |_| 2));
        results.push((
            mono_target.packfile_key,
            apply_merge_io_sync(
                mono_target.old.as_ref(),
                &mono_delta,
                &mono_target.tmp_path,
                &mono_target.final_path,
            )?,
        ));
        if let (Some((head_target, tail_target)), Some((head_delta, tail_delta))) =
            (split_targets, split_delta)
        {
            results.push((
                head_target.packfile_key,
                apply_merge_io_sync(
                    head_target.old.as_ref(),
                    &head_delta,
                    &head_target.tmp_path,
                    &head_target.final_path,
                )?,
            ));
            results.push((
                tail_target.packfile_key,
                apply_merge_io_sync(
                    tail_target.old.as_ref(),
                    &tail_delta,
                    &tail_target.tmp_path,
                    &tail_target.final_path,
                )?,
            ));
        }
        Ok(results)
    })
    .await
    .map_err(io::Error::other)??;

    // Dernière étape, sans exception — mêmes garanties que regenerate_and_swap :
    // tout Err ci-dessus retourne avant cette ligne, chaque ancien Arc reste
    // servi tel quel tant que son propre store() n'a pas eu lieu.
    for (key, new_index) in new_indices {
        registry.store(key, Arc::new(new_index));
    }

    Ok(())
}


/// Étage 2 du tick `Dispatcher::run()` (V2d) — l'unique point d'appel de
/// régénération du Dispatcher, extrait pour être testable sans `Collector`
/// ni étage 1 (`ingest_and_swap`).
///
/// - `volatile_split == None` : appelle `regenerate_and_swap` — le chemin
///   K=1 existant, littéralement inchangé (aucun passage par la variante
///   segmentée).
/// - `Some(paire)` : appelle `regenerate_and_swap_with_volatile_split` — un
///   seul `fetch_batch` par chunk, trois artefacts (monolithique + head +
///   tail).
///
/// Pas un moteur générique : le `match` est binaire (zéro ou une paire).
pub(crate) async fn regenerate_stage<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
    packfile_key: &'static str,
    volatile_split: Option<&(SplitRenderTarget<P>, SplitRenderTarget<P>)>,
    registry: &LiveRegistry,
    io_semaphore: &tokio::sync::Semaphore,
) -> io::Result<()> {
    match volatile_split {
        None => {
            regenerate_and_swap::<P>(pool, ids, total_cap, packfile_key, registry, io_semaphore)
                .await
        }
        Some(_) => {
            regenerate_and_swap_with_volatile_split::<P>(
                pool,
                ids,
                total_cap,
                packfile_key,
                volatile_split,
                registry,
                io_semaphore,
            )
            .await
        }
    }
}

/// Noyau fusion + I/O physique — strictement synchrone et bloquant, zéro
/// dépendance Tokio (résolution Blocage 1). Phase 4.3 encapsulera l'APPEL
/// (pas la fonction) dans un `spawn_blocking` ; signature inchangée par cet
/// encapsulage futur.
///
/// `old` : génération actuellement servie par le `LiveRegistry` pour cette
/// clé. Jamais mutée ici — `memmap2::Mmap` (immuable) sur son fichier, pas
/// `MmapMut` : la garantie "l'ancien packfile n'est jamais altéré avant la
/// finalisation" est portée par le système de types, pas par une discipline
/// de code à auditer. `PackHtmlIndex` s'interdisant volontairement de
/// mmaper le blob HTML (pack_html_index.rs — coût mémoire nul au cold path),
/// le mapping complet du fichier est reconstruit ici, localement,
/// uniquement pour la durée de cette fusion (résolution Blocage 3).
///
/// `delta` : produit de `fetch_delta_batch`, déjà trié (C1 satisfait).
///
/// Robustesse à l'interruption : propriété structurelle, pas procédurale.
/// Toute écriture a lieu sur `tmp_path` ; `final_path` n'est jamais ouvert
/// en écriture par cette fonction — seulement réouvert en lecture, après le
/// `rename`, pour construire l'index retourné. Un crash ou un retour
/// anticipé (`?`) à n'importe quel point avant le `rename` laisse l'ancien
/// packfile bit-à-bit intact ; un `.tmp` orphelin d'une exécution
/// interrompue est sans conséquence, `OpenOptions::truncate(true)` l'écrase
/// à la tentative suivante.
fn apply_merge_io_sync(
    old: &PackHtmlIndex,
    delta: &DeltaBatch,
    tmp_path: &Path,
    final_path: &Path,
) -> io::Result<PackHtmlIndex> {
    const ENTRY_SIZE: u64 = std::mem::size_of::<PackfileEntry>() as u64; // 24
    const FOOTER_SIZE: u64 = std::mem::size_of::<PackfileFooter>() as u64; // 32

    // ---- Ancien packfile : mmap lecture-seule temporaire --------------------
    let old_file = old.file();
    let old_file_len = old_file.metadata()?.len();
    let old_mmap = unsafe { memmap2::Mmap::map(old_file)? };

    let old_footer_start = old_file_len
        .checked_sub(FOOTER_SIZE)
        .ok_or_else(|| io::Error::other("ancien packfile trop court pour contenir un footer"))?;
    // entry_count() déjà validé par PackHtmlIndex::open (magic/version/
    // cohérence index_len) — pas reparsé ici, seulement réutilisé pour
    // localiser la région d'index dans CE mmap-ci (pack_html_index.rs ne
    // mmape jamais le blob, donc ne peut pas nous fournir la slice).
    let old_index_len = old.entry_count() as u64 * ENTRY_SIZE;
    let old_index_start = old_footer_start.checked_sub(old_index_len).ok_or_else(|| {
        io::Error::other("ancien packfile : index_len incohérent avec entry_count")
    })?;

    let old_blob: &[u8] = &old_mmap[0..old_index_start as usize];
    let old_index: &[PackfileEntry] =
        bytemuck::cast_slice(&old_mmap[old_index_start as usize..old_footer_start as usize]);

    // ---- Dimensionnement haut du .tmp : borne supérieure --------------------
    let cap = old_blob.len() as u64
        + delta.payload.len() as u64
        + 7
        + (old_index.len() + delta.entries.len()) as u64 * ENTRY_SIZE
        + FOOTER_SIZE;

    let tmp_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(tmp_path)?;
    tmp_file.set_len(cap)?; // ftruncate haut, avant tout mmap

    let mut tmp_mmap = unsafe { memmap2::MmapMut::map_mut(&tmp_file)? };

    // ---- merge_sweep : boîte noire pure (Phase 4.1), zéro-alloc interne ----
    let mut out_index: Vec<PackfileEntry> =
        Vec::with_capacity(old_index.len() + delta.entries.len());
    let report = merge_sweep(
        old_blob,
        old_index,
        delta,
        &mut tmp_mmap[..],
        &mut out_index,
    );

    // ---- align8 : padding explicite avant l'index ---------------------------
    let bytes_written = report.bytes_written;
    let aligned_len = (bytes_written + 7) & !7;
    tmp_mmap[bytes_written as usize..aligned_len as usize].fill(0);

    // ---- Sérialisation de l'index (24o/entrée), contiguë au padding --------
    let index_bytes: &[u8] = bytemuck::cast_slice(out_index.as_slice());
    let index_start = aligned_len as usize;
    let index_end = index_start + index_bytes.len();
    tmp_mmap[index_start..index_end].copy_from_slice(index_bytes);

    // ---- Footer canonique (32o), immédiatement après l'index ---------------
    let footer = PackfileFooter {
        magic: *b"MARIUSPK",
        version: 1,
        _pad: [0u8; 4],
        entry_count: out_index.len() as u64,
        index_len: index_bytes.len() as u64,
    };
    let footer_bytes = bytemuck::bytes_of(&footer);
    let footer_start = index_end;
    let footer_end = footer_start + footer_bytes.len();
    tmp_mmap[footer_start..footer_end].copy_from_slice(footer_bytes);

    let real_len = footer_end as u64; // == aligned_len + index_len + 32

    // ---- Durabilité, puis ftruncate bas, puis fsync ------------------------
    //
    // Ordre délibérément différent de la formulation littérale du handoff
    // ("ftruncate bas, PUIS fsync/msync") : msync ici précède le ftruncate
    // bas, pas l'inverse. Raison structurelle, pas une négligence —
    // `msync` après un `ftruncate` qui rétrécit le fichier porterait sur des
    // pages dont une partie du mapping (entre `real_len` et `cap`) n'est
    // plus garantie valide (sémantique POSIX dépendante du filesystem en
    // cas d'accès au-delà de la nouvelle EOF). `flush_range(0, real_len)`
    // élimine le problème : il ne synchronise QUE la région utile, identique
    // avant et après troncature — aucune page incertaine touchée. Le
    // `fsync(fd)` final, lui, a lieu APRÈS le ftruncate : c'est lui qui
    // couvre la durabilité du changement de taille (métadonnée), pas le
    // msync. Les deux propriétés exigées par la spec (données + métadonnée
    // durables avant tout retour de succès) sont garanties ; seul l'ordre
    // interne entre les deux mécanismes diffère du pseudocode.
    tmp_mmap.flush_range(0, real_len as usize)?;
    drop(tmp_mmap); // libère le mapping avant troncature/rename — hygiène

    tmp_file.set_len(real_len)?; // ftruncate bas, taille exacte réelle
    tmp_file.sync_all()?; // fsync(fd) — couvre la métadonnée de taille
    drop(tmp_file);

    fs::rename(tmp_path, final_path)?; // atomique (même filesystem, POSIX)

    // Réouverture : seul point où final_path est lu après le swap. Aucune
    // mutation du registre depuis cette fonction — voir regenerate_and_swap.
    PackHtmlIndex::open(final_path)
}

// =============================================================================
// Provisioning idempotent — specification-provisioning-projection.md,
// handoff-provisioning-projection.md §2.
//
// Couvre la branche "absent" de la classification à trois (spec §1) : un
// packfile jamais matérialisé n'est pas une incohérence, c'est l'état
// initial légitime d'un espace de projection. Voisin direct d'
// apply_merge_io_sync ci-dessus, même fichier responsable de la durabilité
// disque (spec §2) — emplacement tranché ici, pas pack_html_format.rs : la
// cohésion du module (chemin tmp/final, idiome fsync+rename, accès à
// packfile_path_for déjà importé) ne justifiait aucun déplacement.
//
// Délègue entièrement la sérialisation à write_packfile_footer
// (pack_html_format.rs, seule source de vérité du format on-disk) — aucun
// second site n'écrit un footer. cold_start() reste strictement inchangé :
// au moment où il s'exécute, ensure_provisioned garantit déjà que tout
// packfile_key référencé existe sous une forme au moins valide-vide ; la
// distinction absent/corrompu n'a donc plus besoin d'être faite dans
// cold_start lui-même (spec §5).
// =============================================================================

/// Issue d'un appel à `ensure_provisioned` — distingue le no-op (cas
/// dominant en régime établi, packfiles déjà présents) du provisioning
/// effectif (premier démarrage, ou `artifacts/` purgé). Ne porte aucune
/// autre information : la validité d'un fichier déjà présent n'est jamais
/// qualifiée ici, c'est le rôle de `cold_start`/`PackHtmlIndex::open` en
/// aval — pas celui de cette fonction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionOutcome {
    /// Le packfile existait déjà — aucune écriture, quel que soit son
    /// contenu (même invalide).
    AlreadyPresent,
    /// Le packfile était absent (`io::ErrorKind::NotFound`) : un packfile
    /// vide mais valide (`entry_count: 0, index_len: 0`) a été écrit
    /// atomiquement à sa place.
    Provisioned,
}

/// Corps synchrone — ne connaît qu'un chemin, pas de `PgPool` ni de
/// `LiveRegistry` (découplage requis par le séquencement au boot, spec §5 :
/// le provisioning précède `cold_start`, qui seul produit le
/// `LiveRegistry` — une dépendance dans l'autre sens serait circulaire).
/// Même idiome d'écriture atomique que `apply_merge_io_sync` ci-dessus
/// (tmp + fsync + rename), réduit au cas vierge : blob vide, index vide.
/// `write_packfile_footer(writer, 0, &[])` est un cas générique de la
/// primitive déjà existante, pas une branche ajoutée pour l'occasion (spec
/// §4/§7 point 1).
fn ensure_provisioned_sync(packfile_key: &'static str) -> io::Result<ProvisionOutcome> {
    let final_path = packfile_path_for(packfile_key);
    match fs::metadata(&final_path) {
        Ok(_) => Ok(ProvisionOutcome::AlreadyPresent),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let tmp_path = final_path.with_extension("tmp");
            if let Some(parent) = tmp_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            write_packfile_footer(&mut writer, 0, &[])?; // blob vide, index vide
            writer.flush()?;
            writer.into_inner().map_err(io::Error::other)?.sync_all()?;
            fs::rename(tmp_path, final_path)?;
            Ok(ProvisionOutcome::Provisioned)
        }
        Err(e) => Err(e), // tout le reste reste fatal — corruption, permission, etc.
    }
}

/// Point d'appel async — seule fonction visible depuis `main.rs`. Idempotent :
/// sans effet si le packfile existe déjà, quel que soit son contenu —
/// `cold_start()` qualifiera sa validité ensuite, ce n'est pas le rôle de
/// cette fonction. N'écrit jamais le format directement : délègue
/// entièrement à `write_packfile_footer`, seule source de vérité du format
/// on-disk.
///
/// `spawn_blocking` inconditionnel (spec §7 point 2, arbitrage handoff
/// point 5) : même discipline que les deux autres sites du système qui
/// touchent un appel système bloquant en contexte async (write path normal
/// via `apply_merge_io_sync` ci-dessus ; read path via `deliver`,
/// handlers.rs). La règle ne dépend pas du contexte de contention au moment
/// de l'appel — elle est inconditionnelle dès qu'un appel système bloquant
/// est en jeu, même si, comme ici, rien ne sert encore au moment de son
/// exécution.
pub async fn ensure_provisioned(packfile_key: &'static str) -> io::Result<ProvisionOutcome> {
    tokio::task::spawn_blocking(move || ensure_provisioned_sync(packfile_key))
        .await
        .map_err(io::Error::other)?
}

// =============================================================================
// Tests — Phase 4.2
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use arc_swap::ArcSwap;
    use std::collections::HashMap;
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ---------------------------------------------------------------------
    // Pas de harnais block_on artisanal ici : `sqlx::PgPool::connect_lazy`
    // exige un contexte Tokio réellement entré (tâche de fond du pool,
    // `Handle::current()` côté sqlx-core), pas seulement que la future
    // finisse par être pollée jusqu'au bout. Les deux tests qui appellent
    // `stub_pool()` sont donc `#[tokio::test]` (current_thread suffit —
    // aucune vraie E/S réseau n'a lieu, Stub/FailingProjection ignorent
    // `_pool`) ; les autres tests de ce module n'en ont pas besoin et
    // restent `#[test]`.
    // ---------------------------------------------------------------------

    // ── Helpers bas niveau — bytes bruts, sans Projection ────────────────────

    fn pe(id: i64, offset: u64, len: u32) -> PackfileEntry {
        PackfileEntry {
            id,
            offset,
            len,
            _pad: [0u8; 4],
        }
    }

    fn write_raw_packfile(path: &Path, blob: &[u8], entries: &[PackfileEntry]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("création répertoire de test");
        }
        let file = std::fs::File::create(path).expect("création packfile de test");
        let mut writer = BufWriter::new(file);
        writer.write_all(blob).expect("écriture blob");
        write_packfile_footer(&mut writer, blob.len() as u64, entries).expect("écriture footer");
        writer.flush().expect("flush");
    }

    fn unique_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "marius_regenerate_test_{label}_{}_{n}.bin",
            std::process::id()
        ))
    }

    fn unique_test_key(label: &str) -> &'static str {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Box::leak(format!("phase4_2_{label}_{}_{n}", std::process::id()).into_boxed_str())
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("tmp"));
    }

    // ── Test 1 : sortie bit-identique à une référence pur Vec<u8> ────────────
    //
    // Référence calculée INDÉPENDAMMENT de apply_merge_io_sync (même
    // merge_sweep, mais sérialisation footer/align8 réimplémentée sur un
    // Vec<u8> simple, pas réutilisée) — détecte une erreur de transcription
    // dans l'arithmétique mmap (bornes de slice, placement index/footer),
    // pas une tautologie qui réexécuterait le code testé.

    fn compute_reference(
        old_blob: &[u8],
        old_index: &[PackfileEntry],
        delta: &DeltaBatch,
    ) -> Vec<u8> {
        const ENTRY_SIZE: usize = std::mem::size_of::<PackfileEntry>();
        const FOOTER_SIZE: usize = std::mem::size_of::<PackfileFooter>();

        let cap = old_blob.len()
            + delta.payload.len()
            + 7
            + (old_index.len() + delta.entries.len()) * ENTRY_SIZE
            + FOOTER_SIZE;
        let mut buf = vec![0u8; cap];

        let mut out_index = Vec::with_capacity(old_index.len() + delta.entries.len());
        let report = merge_sweep(old_blob, old_index, delta, &mut buf[..], &mut out_index);

        let aligned = ((report.bytes_written + 7) & !7) as usize;
        // buf[bytes_written..aligned] déjà à zéro (vec![0u8; cap]).

        let index_bytes: &[u8] = bytemuck::cast_slice(out_index.as_slice());
        buf[aligned..aligned + index_bytes.len()].copy_from_slice(index_bytes);

        let footer = PackfileFooter {
            magic: *b"MARIUSPK",
            version: 1,
            _pad: [0u8; 4],
            entry_count: out_index.len() as u64,
            index_len: index_bytes.len() as u64,
        };
        let footer_bytes = bytemuck::bytes_of(&footer);
        let footer_start = aligned + index_bytes.len();
        buf[footer_start..footer_start + footer_bytes.len()].copy_from_slice(footer_bytes);

        buf.truncate(footer_start + footer_bytes.len());
        buf
    }

    #[test]
    fn apply_merge_io_sync_matches_pure_vec_reference_bit_for_bit() {
        // old : id=1 "A", id=2 "BB", id=3 "CCC".
        let old_blob = b"ABBCCC".to_vec();
        let old_index = vec![pe(1, 0, 1), pe(2, 1, 2), pe(3, 3, 3)];

        // delta : id=1 DELETE, id=2 UPDATE -> "BBBB", id=4 INSERT -> "DDDDD".
        // id=3 reste hors delta — copié depuis l'ancien, exercé par le même
        // test (pas seulement par le test de non-régression dédié).
        let delta = DeltaBatch {
            entries: vec![
                DeltaEntry {
                    entity_id: 1,
                    offset: 0,
                    length: 0,
                },
                DeltaEntry {
                    entity_id: 2,
                    offset: 0,
                    length: 4,
                },
                DeltaEntry {
                    entity_id: 4,
                    offset: 4,
                    length: 5,
                },
            ],
            payload: b"BBBBDDDDD".to_vec(),
        };

        let real_path = unique_path("bitexact");
        write_raw_packfile(&real_path, &old_blob, &old_index);
        let old = PackHtmlIndex::open(&real_path).expect("ouverture ancien packfile");
        let tmp_path = real_path.with_extension("tmp");

        let reference = compute_reference(&old_blob, &old_index, &delta);

        let _new_index = apply_merge_io_sync(&old, &delta, &tmp_path, &real_path)
            .expect("apply_merge_io_sync doit réussir");

        let on_disk = fs::read(&real_path).expect("lecture du packfile final");
        assert_eq!(
            on_disk, reference,
            "sortie de apply_merge_io_sync non bit-identique à la référence Vec<u8> \
             (payload + padding align8 + index + footer)"
        );

        cleanup(&real_path);
    }

    // ── Test 2 : alignement réel — cast_slice, pas une comparaison d'octets ──

    #[test]
    fn final_index_region_is_8byte_aligned_and_castable() {
        let old_blob = b"X".to_vec();
        let old_index = vec![pe(1, 0, 1)];
        let delta = DeltaBatch {
            entries: vec![DeltaEntry {
                entity_id: 2,
                offset: 0,
                length: 3,
            }],
            payload: b"YYY".to_vec(),
        };

        let real_path = unique_path("alignment");
        write_raw_packfile(&real_path, &old_blob, &old_index);
        let old = PackHtmlIndex::open(&real_path).expect("ouverture ancien packfile");
        let tmp_path = real_path.with_extension("tmp");

        apply_merge_io_sync(&old, &delta, &tmp_path, &real_path)
            .expect("apply_merge_io_sync doit réussir");

        let on_disk = fs::read(&real_path).expect("lecture packfile final");
        const FOOTER_SIZE: usize = std::mem::size_of::<PackfileFooter>();
        let footer_start = on_disk.len() - FOOTER_SIZE;
        let footer: PackfileFooter = bytemuck::pod_read_unaligned(&on_disk[footer_start..]);
        let index_start = footer_start - footer.index_len as usize;

        // L'assertion qui compte : cast_slice panique si l'alignement 8B
        // n'est pas respecté — pas une simple comparaison d'octets bruts.
        let entries: &[PackfileEntry] = bytemuck::cast_slice(&on_disk[index_start..footer_start]);
        assert_eq!(entries.len(), footer.entry_count as usize);
        assert_eq!(entries.len(), 2, "id=1 (copié) + id=2 (inséré)");

        cleanup(&real_path);
    }

    // ── Fixtures Projection pour les tests bout-en-bout (async) ─────────────
    //
    // DB simulée par un Mutex<Vec<(id, génération)>> statique — partagé par
    // tout le binaire de test de ce module, même contrainte opérationnelle
    // que ALIVE_INSTANCES (pack_html_index.rs) : exécuter isolément ou avec
    // --test-threads=1 pour des assertions fiables sur les tests qui suivent.

    static DB: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());

    fn db_set(rows: &[(i64, i64)]) {
        *DB.lock().unwrap() = rows.to_vec();
    }

    // Compteur d'appels — V2d, test de non-double-ingestion
    // (regenerate_and_swap_with_volatile_split). N'affecte aucun test
    // existant : incrémenté, jamais lu, sauf par les tests qui le
    // réinitialisent explicitement.
    static FETCH_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[repr(C)]
    #[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
    struct StubRecord {
        id: i64, // i32 -> i64 : derive(Pod) refuse un padding de repr(C) (i32+i64) — cf. rapport
        generation: i64,
    }

    const STUB_TOTAL_CAP: usize = 32;

    fn stub_pool() -> sqlx::PgPool {
        sqlx::PgPool::connect_lazy("postgres://stub-unused-phase4_2/db")
            .expect("connect_lazy ne touche jamais le réseau")
    }

    /// Simule `SELECT ... WHERE id = ANY($1)` : un id absent de `DB` est
    /// silencieusement omis du résultat — c'est CE comportement qui fonde
    /// le contrat de détection des suppressions de `fetch_delta_batch`
    /// (résolution Blocage 2).
    struct StubProjection;

    impl Projection for StubProjection {
        type Record = StubRecord;
        type VarlenOwned = ();

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            ids: &[i64],
        ) -> impl std::future::Future<Output = marius_projection::BatchResult<Self>> + Send
        {
            FETCH_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let db = DB.lock().unwrap();
            let batch: Vec<(StubRecord, ())> = ids
                .iter()
                .filter_map(|&id| {
                    db.iter()
                        .find(|&&(rid, _)| rid == id)
                        .map(|&(rid, generation)| {
                            (
                                StubRecord {
                                    id: rid,
                                    generation,
                                },
                                (),
                            )
                        })
                })
                .collect();
            async move { Ok(batch) }
        }

        fn render(record: &StubRecord, _varlena: &(), buf: &mut String) {
            use std::fmt::Write as _;
            write!(buf, "<g{}>", record.generation).unwrap();
        }

        #[inline(always)]
        fn record_id(record: &StubRecord) -> i64 {
            record.id
        }

        fn packfile_path() -> PathBuf {
            PathBuf::from("artifacts/unused_stub_pack.bin")
        }

        fn store_path() -> PathBuf {
            PathBuf::from("unused_stub_store.bin")
        }

        fn store_registry() -> &'static marius_projection::StoreRegistry<Self> {
            static REGISTRY: marius_projection::StoreRegistry<StubProjection> =
                marius_projection::StoreRegistry::new();
            &REGISTRY
        }
    }

    // ── Fonctions de rendu head/tail — V2d, mêmes conventions que
    //    `StubProjection::render` ci-dessus (préfixe distinctif pour
    //    différencier visuellement mono/head/tail dans les assertions).
    fn stub_render_head(record: &StubRecord, _varlena: &(), buf: &mut String) {
        use std::fmt::Write as _;
        write!(buf, "<head{}>", record.generation).unwrap();
    }

    fn stub_render_tail(record: &StubRecord, _varlena: &(), buf: &mut String) {
        use std::fmt::Write as _;
        write!(buf, "<tail{}>", record.generation).unwrap();
    }

    // =========================================================================
    // V2d — régénération multi-artefacts (content_core / _head / _tail)
    // =========================================================================

    /// Démontre les deux exigences centrales de V2d dans un seul test :
    /// (1) une SEULE ingestion (`fetch_batch`) alimente les trois rendus ;
    /// (2) `render_head`/`render_tail` (fonctions libres, pas des méthodes
    /// du trait) reçoivent bien les MÊMES données que le rendu monolithique
    /// — content_core_head/tail portent la même `generation` que
    /// content_core pour chaque id.
    #[tokio::test]
    async fn volatile_split_renders_head_and_tail_from_a_single_fetch_batch_call() {
        let mono_key = unique_test_key("split_mono");
        let head_key = unique_test_key("split_head");
        let tail_key = unique_test_key("split_tail");
        let pool = stub_pool();

        db_set(&[(1, 5), (2, 7)]);
        write_initial_packfile(mono_key, &[(1, ""), (2, "")]);
        write_initial_packfile(head_key, &[(1, ""), (2, "")]);
        write_initial_packfile(tail_key, &[(1, ""), (2, "")]);

        let mut indices = HashMap::new();
        for key in [mono_key, head_key, tail_key] {
            let idx = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
            indices.insert(key, ArcSwap::from_pointee(idx));
        }
        let registry = LiveRegistry::with_indices(indices);
        let io_sem = tokio::sync::Semaphore::new(1);

        FETCH_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);

        let split = Some((
            SplitRenderTarget::<StubProjection> {
                packfile_key: head_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_head,
            },
            SplitRenderTarget::<StubProjection> {
                packfile_key: tail_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_tail,
            },
        ));

        regenerate_and_swap_with_volatile_split::<StubProjection>(
            &pool,
            &[1, 2],
            STUB_TOTAL_CAP,
            mono_key,
            split.as_ref(),
            &registry,
            &io_sem,
        )
        .await
        .expect("le tick segmenté doit réussir");

        assert_eq!(
            FETCH_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "une seule ingestion (un seul chunk pour 2 ids) doit alimenter \
             les trois rendus — jamais une par cible"
        );

        let mono = registry.load(mono_key).unwrap();
        let head = registry.load(head_key).unwrap();
        let tail = registry.load(tail_key).unwrap();

        assert_eq!(read_fragment(&mono, 1), Some("<g5>".to_string()));
        assert_eq!(read_fragment(&mono, 2), Some("<g7>".to_string()));
        // head/tail portent la MÊME generation que le monolithique pour
        // chaque id — même Record, jamais une donnée divergente.
        assert_eq!(read_fragment(&head, 1), Some("<head5>".to_string()));
        assert_eq!(read_fragment(&head, 2), Some("<head7>".to_string()));
        assert_eq!(read_fragment(&tail, 1), Some("<tail5>".to_string()));
        assert_eq!(read_fragment(&tail, 2), Some("<tail7>".to_string()));
    }

    /// K=1 strictement inchangé : `regenerate_and_swap_with_volatile_split`
    /// avec `volatile_split: None` doit produire un résultat identique,
    /// octet pour octet, à `regenerate_and_swap` — la nouvelle fonction
    /// n'est pas un chemin parallèle divergent pour le cas non segmenté.
    #[tokio::test]
    async fn volatile_split_none_matches_plain_regenerate_and_swap_byte_for_byte() {
        let key_a = unique_test_key("k1_plain");
        let key_b = unique_test_key("k1_via_split_fn");
        let pool = stub_pool();

        db_set(&[(1, 3)]);
        write_initial_packfile(key_a, &[(1, "")]);
        write_initial_packfile(key_b, &[(1, "")]);

        let mut indices = HashMap::new();
        for key in [key_a, key_b] {
            let idx = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
            indices.insert(key, ArcSwap::from_pointee(idx));
        }
        let registry = LiveRegistry::with_indices(indices);
        let io_sem = tokio::sync::Semaphore::new(1);

        regenerate_and_swap::<StubProjection>(&pool, &[1], STUB_TOTAL_CAP, key_a, &registry, &io_sem)
            .await
            .expect("regenerate_and_swap (existant) doit réussir");
        regenerate_and_swap_with_volatile_split::<StubProjection>(
            &pool,
            &[1],
            STUB_TOTAL_CAP,
            key_b,
            None,
            &registry,
            &io_sem,
        )
        .await
        .expect("regenerate_and_swap_with_volatile_split(None) doit réussir");

        let a = registry.load(key_a).unwrap();
        let b = registry.load(key_b).unwrap();
        assert_eq!(
            read_fragment(&a, 1),
            read_fragment(&b, 1),
            "même contenu, que le tick passe par l'ancienne ou la nouvelle fonction"
        );
        assert_eq!(read_fragment(&a, 1), Some("<g3>".to_string()));
    }

    /// Convention de nommage réellement en vigueur (rapport de session,
    /// `route_derive.rs`/`regenerate_and_swap` — jamais `P::packfile_path()`,
    /// toujours `packfile_path_for(clé)`) : les fichiers physiques
    /// s'appellent exactement `{clé}.bin`, jamais `{clé}_pack.bin` — vérifié
    /// ici pour content_core_head/content_core_tail spécifiquement, pas
    /// seulement affirmé.
    #[tokio::test]
    async fn head_and_tail_packfiles_use_the_artifact_key_convention_not_the_pack_suffix() {
        let mono_key = "content_core"; // clé réelle (ArtifactKey), pas de suffixe
        let head_key = "content_core_head";
        let tail_key = unique_test_key("naming_tail"); // clé unique pour éviter toute collision inter-tests sur le fichier physique partagé "content_core_tail.bin"
        let pool = stub_pool();

        // mono_key/head_key utilisent volontairement les clés RÉELLES du
        // vertical slice (pas unique_test_key) pour vérifier le chemin
        // physique exact qu'utilisera la production — au prix de devoir
        // nettoyer explicitement en fin de test (pas de clé jetable ici).
        db_set(&[(1, 9)]);
        write_initial_packfile(mono_key, &[(1, "")]);
        write_initial_packfile(head_key, &[(1, "")]);
        write_initial_packfile(tail_key, &[(1, "")]);

        assert_eq!(
            packfile_path_for(head_key),
            std::path::PathBuf::from("artifacts/content_core_head.bin"),
            "convention réelle : {{clé}}.bin, jamais {{clé}}_pack.bin"
        );

        let mut indices = HashMap::new();
        for key in [mono_key, head_key, tail_key] {
            let idx = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
            indices.insert(key, ArcSwap::from_pointee(idx));
        }
        let registry = LiveRegistry::with_indices(indices);
        let io_sem = tokio::sync::Semaphore::new(1);

        let split = Some((
            SplitRenderTarget::<StubProjection> {
                packfile_key: head_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_head,
            },
            SplitRenderTarget::<StubProjection> {
                packfile_key: tail_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_tail,
            },
        ));
        regenerate_and_swap_with_volatile_split::<StubProjection>(
            &pool,
            &[1],
            STUB_TOTAL_CAP,
            mono_key,
            split.as_ref(),
            &registry,
            &io_sem,
        )
        .await
        .expect("doit réussir");

        assert!(
            std::path::Path::new("artifacts/content_core_head.bin").exists(),
            "le fichier physique doit exister exactement à ce chemin"
        );

        cleanup(&packfile_path_for(mono_key));
        cleanup(&packfile_path_for(head_key));
        cleanup(&packfile_path_for(tail_key));
    }

    // =========================================================================
    // V2d — `regenerate_stage` : l'étage 2 réellement appelé par
    // `Dispatcher::run()` (chemin réel de régénération, moins le Collector
    // et l'étage 1 ingest_and_swap, qui ne concernent pas le nombre de
    // fetch_batch de l'étage 2).
    // =========================================================================

    /// Avec une paire de cibles : 1 fetch_batch → monolithique + head + tail.
    #[tokio::test]
    async fn regenerate_stage_with_split_produces_three_artifacts_from_one_fetch() {
        let mono_key = unique_test_key("stage_mono");
        let head_key = unique_test_key("stage_head");
        let tail_key = unique_test_key("stage_tail");
        let pool = stub_pool();

        db_set(&[(1, 4), (2, 6)]);
        for key in [mono_key, head_key, tail_key] {
            write_initial_packfile(key, &[(1, ""), (2, "")]);
        }
        let mut indices = HashMap::new();
        for key in [mono_key, head_key, tail_key] {
            let idx = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
            indices.insert(key, ArcSwap::from_pointee(idx));
        }
        let registry = LiveRegistry::with_indices(indices);
        let io_sem = tokio::sync::Semaphore::new(1);

        let split: (SplitRenderTarget<StubProjection>, SplitRenderTarget<StubProjection>) = (
            SplitRenderTarget {
                packfile_key: head_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_head,
            },
            SplitRenderTarget {
                packfile_key: tail_key,
                total_cap: STUB_TOTAL_CAP,
                render: stub_render_tail,
            },
        );

        FETCH_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        regenerate_stage::<StubProjection>(
            &pool,
            &[1, 2],
            STUB_TOTAL_CAP,
            mono_key,
            Some(&split),
            &registry,
            &io_sem,
        )
        .await
        .expect("le tick doit réussir");

        assert_eq!(
            FETCH_CALLS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "1 fetch_batch → monolithique + head + tail"
        );
        let (mono, head, tail) = (
            registry.load(mono_key).unwrap(),
            registry.load(head_key).unwrap(),
            registry.load(tail_key).unwrap(),
        );
        assert_eq!(read_fragment(&mono, 2), Some("<g6>".to_string()));
        assert_eq!(read_fragment(&head, 2), Some("<head6>".to_string()));
        assert_eq!(read_fragment(&tail, 2), Some("<tail6>".to_string()));
    }

    /// Sans paire : chemin K=1 existant — 1 fetch_batch, seul l'artefact
    /// monolithique est réécrit, les clés head/tail (présentes au registre
    /// mais non ciblées) restent strictement intactes.
    #[tokio::test]
    async fn regenerate_stage_without_split_is_the_unchanged_k1_path() {
        let mono_key = unique_test_key("stage_k1_mono");
        let head_key = unique_test_key("stage_k1_head");
        let pool = stub_pool();

        db_set(&[(1, 8)]);
        write_initial_packfile(mono_key, &[(1, "")]);
        write_initial_packfile(head_key, &[(1, "H0")]);
        let mut indices = HashMap::new();
        for key in [mono_key, head_key] {
            let idx = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
            indices.insert(key, ArcSwap::from_pointee(idx));
        }
        let registry = LiveRegistry::with_indices(indices);
        let io_sem = tokio::sync::Semaphore::new(1);

        FETCH_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
        regenerate_stage::<StubProjection>(
            &pool,
            &[1],
            STUB_TOTAL_CAP,
            mono_key,
            None,
            &registry,
            &io_sem,
        )
        .await
        .expect("le tick K=1 doit réussir");

        assert_eq!(FETCH_CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            read_fragment(&registry.load(mono_key).unwrap(), 1),
            Some("<g8>".to_string())
        );
        assert_eq!(
            read_fragment(&registry.load(head_key).unwrap(), 1),
            Some("H0".to_string()),
            "sans paire, aucune clé annexe n'est touchée"
        );
    }


    struct FailingProjection;

    impl Projection for FailingProjection {
        type Record = StubRecord;
        type VarlenOwned = ();

        async fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> marius_projection::BatchResult<Self> {
            Err(sqlx::Error::Io(std::io::Error::other(
                "échec PostgreSQL simulé",
            )))
        }

        fn render(record: &StubRecord, _varlena: &(), buf: &mut String) {
            use std::fmt::Write as _;
            write!(buf, "<g{}>", record.generation).unwrap();
        }

        #[inline(always)]
        fn record_id(record: &StubRecord) -> i64 {
            record.id
        }

        fn packfile_path() -> PathBuf {
            PathBuf::from("artifacts/unused_failing_pack.bin")
        }

        fn store_path() -> PathBuf {
            PathBuf::from("unused_failing_store.bin")
        }

        fn store_registry() -> &'static marius_projection::StoreRegistry<Self> {
            static REGISTRY: marius_projection::StoreRegistry<FailingProjection> =
                marius_projection::StoreRegistry::new();
            &REGISTRY
        }
    }

    fn write_initial_packfile(key: &'static str, rows: &[(i64, &str)]) {
        let path = packfile_path_for(key);
        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(rows.len());
        let mut offset = 0u64;
        for &(id, frag) in rows {
            blob.extend_from_slice(frag.as_bytes());
            entries.push(pe(id, offset, frag.len() as u32));
            offset += frag.len() as u64;
        }
        write_raw_packfile(&path, &blob, &entries);
    }

    fn read_fragment(idx: &PackHtmlIndex, id: i64) -> Option<String> {
        let (offset, len) = idx.lookup(id)?;
        let mut buf = vec![0u8; len as usize];
        idx.file()
            .read_at(&mut buf, offset)
            .expect("read_at fragment");
        Some(String::from_utf8(buf).expect("fragment UTF-8 valide"))
    }

    // ── Test 3 : non-régression — entités non touchées + suppression ────────

    #[tokio::test]
    async fn untouched_entities_survive_successive_incremental_merges_then_delete() {
        let key = unique_test_key("nonreg");
        let pool = stub_pool();

        db_set(&[(1, 0), (2, 0), (3, 0)]);
        write_initial_packfile(key, &[(1, "<g0>"), (2, "<g0>"), (3, "<g0>")]);

        let bootstrap = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
        let mut indices = HashMap::new();
        indices.insert(key, ArcSwap::from_pointee(bootstrap));
        let registry = LiveRegistry::with_indices(indices);

        // io_semaphore : 1 permis, aucune contention attendue dans ce test
        // mono-tâche — vérifie seulement le câblage, pas le comportement
        // sous charge (cf. test dédié "borne de concurrence").
        let io_sem = tokio::sync::Semaphore::new(1);

        // Tick 1 : seul id=2 touché.
        db_set(&[(1, 0), (2, 1), (3, 0)]);
        regenerate_and_swap::<StubProjection>(&pool, &[2], STUB_TOTAL_CAP, key, &registry, &io_sem)
            .await
            .expect("tick 1 doit réussir");

        let gen1 = registry.load(key).unwrap();
        assert_eq!(
            read_fragment(&gen1, 1),
            Some("<g0>".to_string()),
            "id=1 doit survivre, absent du delta du tick 1"
        );
        assert_eq!(
            read_fragment(&gen1, 2),
            Some("<g1>".to_string()),
            "id=2 doit refléter le tick 1"
        );
        assert_eq!(
            read_fragment(&gen1, 3),
            Some("<g0>".to_string()),
            "id=3 doit survivre, absent du delta du tick 1"
        );

        // Tick 2 : seul id=3 touché.
        db_set(&[(1, 0), (2, 1), (3, 2)]);
        regenerate_and_swap::<StubProjection>(&pool, &[3], STUB_TOTAL_CAP, key, &registry, &io_sem)
            .await
            .expect("tick 2 doit réussir");

        let gen2 = registry.load(key).unwrap();
        assert_eq!(
            read_fragment(&gen2, 1),
            Some("<g0>".to_string()),
            "id=1 doit survivre deux cycles sans jamais figurer dans un delta — c'est précisément le bug que merge_sweep corrige"
        );
        assert_eq!(
            read_fragment(&gen2, 2),
            Some("<g1>".to_string()),
            "id=2 doit survivre, absent du delta du tick 2"
        );
        assert_eq!(
            read_fragment(&gen2, 3),
            Some("<g2>".to_string()),
            "id=3 doit refléter le tick 2"
        );

        // Tick 3 : suppression de id=1 (disparaît de la base).
        db_set(&[(2, 1), (3, 2)]);
        regenerate_and_swap::<StubProjection>(&pool, &[1], STUB_TOTAL_CAP, key, &registry, &io_sem)
            .await
            .expect("tick 3 (suppression) doit réussir");

        let gen3 = registry.load(key).unwrap();
        assert_eq!(
            read_fragment(&gen3, 1),
            None,
            "id=1 doit avoir disparu après suppression"
        );
        assert_eq!(
            read_fragment(&gen3, 2),
            Some("<g1>".to_string()),
            "id=2 doit survivre au tick de suppression"
        );
        assert_eq!(
            read_fragment(&gen3, 3),
            Some("<g2>".to_string()),
            "id=3 doit survivre au tick de suppression"
        );

        cleanup(&packfile_path_for(key));
    }

    // ── Test 4 : robustesse — échec fetch_batch avant toute écriture ────────
    //
    // Interruption réaliste et atteignable par l'API publique (perte de
    // connexion PostgreSQL en cours de tick), pas une panne injectée dans
    // le noyau synchrone : ce dernier n'écrit jamais `final_path` avant son
    // unique `rename` final (propriété structurelle — type Mmap immuable
    // sur `old`, séparation tmp/final — documentée dans apply_merge_io_sync,
    // pas vérifiable autrement qu'en lecture de code sans instrumentation
    // interne dédiée).

    #[tokio::test]
    async fn fetch_failure_leaves_old_packfile_and_registry_untouched() {
        let key = unique_test_key("fetchfail");
        let pool = stub_pool();

        write_initial_packfile(key, &[(1, "<g0>")]);
        let bootstrap = PackHtmlIndex::open(&packfile_path_for(key)).expect("ouverture amorce");
        let mut indices = HashMap::new();
        indices.insert(key, ArcSwap::from_pointee(bootstrap));
        let registry = LiveRegistry::with_indices(indices);

        let before = registry.load(key).unwrap();
        let before_fragment = read_fragment(&before, 1);

        let io_sem = tokio::sync::Semaphore::new(1);
        let result = regenerate_and_swap::<FailingProjection>(
            &pool,
            &[1],
            STUB_TOTAL_CAP,
            key,
            &registry,
            &io_sem,
        )
        .await;
        assert!(
            result.is_err(),
            "un échec fetch_batch doit remonter en Err, jamais être absorbé"
        );

        let after = registry.load(key).unwrap();
        assert!(
            Arc::ptr_eq(&before, &after),
            "le registre ne doit jamais avoir été swappé : même Arc avant/après l'échec"
        );
        assert_eq!(
            read_fragment(&after, 1),
            before_fragment,
            "le packfile servi par le registre ne doit pas changer après un échec de fetch_batch"
        );
        assert!(
            !packfile_path_for(key).with_extension("tmp").exists(),
            ".tmp ne doit jamais être créé si fetch_batch échoue avant toute écriture physique"
        );

        cleanup(&packfile_path_for(key));
    }

    // =========================================================================
    // Tests — ensure_provisioned (provisioning idempotent)
    //
    // specification-provisioning-projection.md §8 / handoff-provisioning-
    // projection.md, mission point 4, premier niveau ("tests unitaires de
    // ensure_provisioned"). Le second niveau (test de bout en bout,
    // sous-processus réel, environnement vierge complet) vit dans
    // crates/shell/server/tests/phase5_3_supervision.rs, à la suite des
    // tests de supervision Phase 5.3 — emplacement acté par le handoff
    // (point 2), convention déjà établie de séparation sous-processus/
    // in-process.
    // =========================================================================

    #[tokio::test]
    async fn ensure_provisioned_on_missing_path_writes_valid_empty_packfile() {
        let key = unique_test_key("provision_missing");
        let path = packfile_path_for(key);
        assert!(
            !path.exists(),
            "précondition : aucun fichier ne doit préexister à ce chemin"
        );

        let outcome = ensure_provisioned(key)
            .await
            .expect("ensure_provisioned doit réussir sur un chemin absent");
        assert_eq!(outcome, ProvisionOutcome::Provisioned);

        // Preuve par le lecteur réel, pas par l'intuition de l'écrivain
        // (handoff mission point 4) : PackHtmlIndex::open() doit accepter
        // le fichier produit, avec entry_count() == 0.
        let index = PackHtmlIndex::open(&path)
            .expect("le fichier provisionné doit être un packfile valide selon le lecteur réel");
        assert_eq!(
            index.entry_count(),
            0,
            "un packfile provisionné doit être vide"
        );

        cleanup(&path);
    }

    #[tokio::test]
    async fn ensure_provisioned_on_present_path_never_overwrites_even_if_invalid() {
        let key = unique_test_key("provision_present");
        let path = packfile_path_for(key);
        fs::create_dir_all(
            path.parent()
                .expect("packfile_path_for retourne toujours un chemin avec parent"),
        )
        .expect("création du répertoire artifacts/ de test");

        // Fixture délibérément invalide — ensure_provisioned ne doit jamais
        // juger de la validité d'un fichier déjà présent, seulement de sa
        // présence (spec §1, ligne "présent, invalide" : ce n'est pas son
        // rôle, c'est celui de cold_start()/PackHtmlIndex::open en aval).
        fs::write(&path, b"contenu arbitraire, pas un packfile bien forme")
            .expect("écriture de la fixture invalide");
        let content_before = fs::read(&path).expect("lecture de la fixture avant l'appel");

        let outcome = ensure_provisioned(key)
            .await
            .expect("ensure_provisioned doit réussir (no-op) sur un chemin déjà présent");
        assert_eq!(outcome, ProvisionOutcome::AlreadyPresent);

        let content_after = fs::read(&path).expect("lecture de la fixture après l'appel");
        assert_eq!(
            content_before, content_after,
            "un fichier déjà présent, même invalide, ne doit jamais être écrasé"
        );

        cleanup(&path);
    }

    #[tokio::test]
    async fn ensure_provisioned_is_idempotent_across_two_successive_calls() {
        let key = unique_test_key("provision_idempotent");
        let path = packfile_path_for(key);
        assert!(!path.exists(), "précondition : chemin initialement absent");

        let first = ensure_provisioned(key)
            .await
            .expect("le premier appel doit réussir");
        assert_eq!(first, ProvisionOutcome::Provisioned);
        let content_after_first = fs::read(&path).expect("lecture après le premier appel");

        let second = ensure_provisioned(key)
            .await
            .expect("le second appel doit réussir");
        assert_eq!(
            second,
            ProvisionOutcome::AlreadyPresent,
            "le second appel doit constater la présence créée par le premier, pas réécrire"
        );
        let content_after_second = fs::read(&path).expect("lecture après le second appel");

        assert_eq!(
            content_after_first, content_after_second,
            "le fichier ne doit pas changer entre les deux appels"
        );

        cleanup(&path);
    }
}
