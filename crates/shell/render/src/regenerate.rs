// =============================================================================
// crates/shell/render/src/regenerate.rs
//
// Interface d'écriture — specification-marius-render-shell.md §7.
// regenerate_and_swap<P> est l'unique point d'entrée AOT-compliant pour
// produire ou remplacer un packfile HTML : dump initial et Dispatcher
// l'appellent tous les deux, aucun des deux ne le contient (roadmap, Phase 4
// — "il n'appartient à aucun des deux fichiers existants").
//
// Trois divergences entre le pseudocode littéral de la spec §7 et le code
// réellement compilé des phases précédentes — documentées ici plutôt que
// silencieuses, même discipline que Phase 3 sur handlers.rs :
//
//   1. P::fetch_from_pg n'existe pas sur le trait Projection réel.
//      batch_renderer.rs (StubProjection, compilé Phase 1) et dispatcher.rs
//      (P::fetch_batch(&self.pool, &ids), compilé) confirment tous deux que
//      la méthode s'appelle fetch_batch.
//
//   2. write_packfile_footer(&mut writer, &full_index) — la spec omet
//      blob_len. La signature réelle (pack_html_format.rs, Phase 1,
//      compilée) est write_packfile_footer(writer, blob_len, index).
//
//   3. registry.indices[packfile_key].store(...) suppose le champ `indices`
//      public. Il est privé — résolu via registry.store(packfile_key,
//      Arc::new(new_index)), la méthode publique existante.
//
//   4. [PHASE 4.2] Réécriture complète → fusion incrémentale.
//      handoff-phase-4.2.md, point 0 : `ids` passé à cette fonction n'est
//      plus l'ensemble complet attendu dans le packfile, mais le DELTA du
//      tick courant (Collector::flush()) — entités insérées, modifiées ou
//      supprimées, jamais l'ensemble. L'ancienne stratégie (réécriture
//      complète du fichier à chaque appel) perdait silencieusement toute
//      entité non touchée par le tick. Cette version fusionne le delta
//      contre l'ancien packfile via `sweep::merge_sweep`, au lieu de
//      reconstruire le fichier en entier.
//
//      Conséquence structurelle sur cette fonction : elle reste `async`
//      (P::fetch_batch est .await-é, sqlx n'a pas de variante bloquante —
//      aucun pont Tokio disponible ni souhaité dans cette session pour ce
//      problème), mais toute la plomberie I/O physique qui suit (ftruncate,
//      mmap, merge_sweep, align8, footer, fsync/msync, rename) est isolée
//      dans `apply_merge_io_sync`, une fonction privée strictement
//      synchrone, zéro dépendance Tokio. C'est ce noyau, et lui seul, qu'un
//      futur `spawn_blocking` (Phase 4.3) encapsulera — signature de
//      `apply_merge_io_sync` conçue pour rester inchangée par cet
//      encapsulage.
// =============================================================================

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use marius_projection::Projection;

use crate::batch_renderer::BatchRenderer;
use crate::pack_html_format::{PackfileEntry, PackfileFooter};
use crate::pack_html_index::PackHtmlIndex;
use crate::registry::{packfile_path_for, LiveRegistry};
use crate::sweep::{merge_sweep, DeltaBatch, DeltaEntry};

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
            entries.push(DeltaEntry { entity_id: id, offset: 0, length: 0 });
        }
    }

    // C1 (sweep.rs) : delta.entries strictement trié par entity_id croissant.
    // Précondition reconstruite ici, pas reportée sur l'appelant — `ids` n'a
    // aucune obligation d'ordre côté Collector::flush() (hors scope de cette
    // session). Un doublon résiduel dans `ids` violerait C1 (tri strict, pas
    // large) et serait détecté par le debug_assert de merge_sweep, pas
    // silencieusement absorbé ici.
    entries.sort_unstable_by_key(|e| e.entity_id);

    Ok(DeltaBatch { entries, payload })
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
    let report = merge_sweep(old_blob, old_index, delta, &mut tmp_mmap[..], &mut out_index);

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
// Tests — Phase 4.2
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::write_packfile_footer;
    use arc_swap::ArcSwap;
    use std::collections::HashMap;
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

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
        PackfileEntry { id, offset, len, _pad: [0u8; 4] }
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

    fn compute_reference(old_blob: &[u8], old_index: &[PackfileEntry], delta: &DeltaBatch) -> Vec<u8> {
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
                DeltaEntry { entity_id: 1, offset: 0, length: 0 },
                DeltaEntry { entity_id: 2, offset: 0, length: 4 },
                DeltaEntry { entity_id: 4, offset: 4, length: 5 },
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
            entries: vec![DeltaEntry { entity_id: 2, offset: 0, length: 3 }],
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

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct StubRecord {
        id: i32,
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
            let db = DB.lock().unwrap();
            let batch: Vec<(StubRecord, ())> = ids
                .iter()
                .filter_map(|&id| {
                    db.iter()
                        .find(|&&(rid, _)| rid == id)
                        .map(|&(rid, generation)| (StubRecord { id: rid as i32, generation }, ()))
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
            record.id as i64
        }

        fn packfile_path() -> PathBuf {
            PathBuf::from("artifacts/unused_stub_pack.bin")
        }

        fn store_path() -> PathBuf {
            PathBuf::from("unused_stub_store.bin")
        }
    }

    /// Simule un échec de connexion/requête PostgreSQL — pour le test de
    /// robustesse à l'interruption (échoue avant toute écriture I/O).
    struct FailingProjection;

    impl Projection for FailingProjection {
        type Record = StubRecord;
        type VarlenOwned = ();

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = marius_projection::BatchResult<Self>> + Send
        {
            async { Err(sqlx::Error::Io(std::io::Error::other("échec PostgreSQL simulé"))) }
        }

        fn render(record: &StubRecord, _varlena: &(), buf: &mut String) {
            use std::fmt::Write as _;
            write!(buf, "<g{}>", record.generation).unwrap();
        }

        #[inline(always)]
        fn record_id(record: &StubRecord) -> i64 {
            record.id as i64
        }

        fn packfile_path() -> PathBuf {
            PathBuf::from("artifacts/unused_failing_pack.bin")
        }

        fn store_path() -> PathBuf {
            PathBuf::from("unused_failing_store.bin")
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
        idx.file().read_at(&mut buf, offset).expect("read_at fragment");
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
        assert_eq!(read_fragment(&gen1, 1), Some("<g0>".to_string()), "id=1 doit survivre, absent du delta du tick 1");
        assert_eq!(read_fragment(&gen1, 2), Some("<g1>".to_string()), "id=2 doit refléter le tick 1");
        assert_eq!(read_fragment(&gen1, 3), Some("<g0>".to_string()), "id=3 doit survivre, absent du delta du tick 1");

        // Tick 2 : seul id=3 touché.
        db_set(&[(1, 0), (2, 1), (3, 2)]);
        regenerate_and_swap::<StubProjection>(&pool, &[3], STUB_TOTAL_CAP, key, &registry, &io_sem)
            .await
            .expect("tick 2 doit réussir");

        let gen2 = registry.load(key).unwrap();
        assert_eq!(read_fragment(&gen2, 1), Some("<g0>".to_string()), "id=1 doit survivre deux cycles sans jamais figurer dans un delta — c'est précisément le bug que merge_sweep corrige");
        assert_eq!(read_fragment(&gen2, 2), Some("<g1>".to_string()), "id=2 doit survivre, absent du delta du tick 2");
        assert_eq!(read_fragment(&gen2, 3), Some("<g2>".to_string()), "id=3 doit refléter le tick 2");

        // Tick 3 : suppression de id=1 (disparaît de la base).
        db_set(&[(2, 1), (3, 2)]);
        regenerate_and_swap::<StubProjection>(&pool, &[1], STUB_TOTAL_CAP, key, &registry, &io_sem)
            .await
            .expect("tick 3 (suppression) doit réussir");

        let gen3 = registry.load(key).unwrap();
        assert_eq!(read_fragment(&gen3, 1), None, "id=1 doit avoir disparu après suppression");
        assert_eq!(read_fragment(&gen3, 2), Some("<g1>".to_string()), "id=2 doit survivre au tick de suppression");
        assert_eq!(read_fragment(&gen3, 3), Some("<g2>".to_string()), "id=3 doit survivre au tick de suppression");

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
        assert!(result.is_err(), "un échec fetch_batch doit remonter en Err, jamais être absorbé");

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
}
