// =============================================================================
// crates/shell/render/src/regenerate.rs
//
// Interface d'écriture — specification-marius-render-shell.md §7.
// regenerate_and_swap<P> est l'unique point d'entrée AOT-compliant pour
// produire ou remplacer un packfile HTML : dump initial et Dispatcher
// l'appellent tous les deux, aucun des deux ne le contient (roadmap, Phase 4
// — "il n'appartient à aucun des deux fichiers existants").
//
// Séquence imposée par la spec, non négociable : fichier .tmp → blob rendu
// par BatchRenderer → footer (write_packfile_footer) → flush → fsync →
// rename() atomique → réouverture (PackHtmlIndex::open) → registry.store().
// LiveRegistry::store() est appelé en toute dernière étape, jamais avant que
// le nouvel index soit pleinement ouvert et validé — tout Err ci-dessus
// (fetch, écriture, fsync, rename, réouverture) laisse l'ancien Arc en place,
// servi sans interruption aux requêtes en vol.
//
// Trois divergences entre le pseudocode littéral de la spec §7 et le code
// réellement compilé des phases précédentes — documentées ici plutôt que
// silencieuses, même discipline que Phase 3 sur handlers.rs :
//
//   1. P::fetch_from_pg n'existe pas sur le trait Projection réel.
//      batch_renderer.rs (StubProjection, compilé Phase 1) et dispatcher.rs
//      (P::fetch_batch(&self.pool, &ids), compilé) confirment tous deux que
//      la méthode s'appelle fetch_batch. fetch_from_pg n'apparaît que dans
//      la prose de la spec, jamais dans du code vérifié — résolu en faveur
//      du code compilé.
//
//   2. write_packfile_footer(&mut writer, &full_index) — la spec omet
//      blob_len. La signature réelle (pack_html_format.rs, Phase 1,
//      compilée) est write_packfile_footer(writer, blob_len, index) —
//      blob_len requis pour le padding d'alignement 8B avant l'index. La
//      valeur est déjà disponible : c'est `offset`, l'accumulateur déjà
//      tenu par cette fonction (valeur de retour de
//      BatchRenderer::render_batch) — aucune divination nécessaire.
//
//   3. registry.indices[packfile_key].store(...) suppose le champ `indices`
//      public. Il est privé (registry.rs, encapsulation tranchée Phase 2,
//      confirmée Phase 3 pour load()). Résolu : registry.store(packfile_key,
//      Arc::new(new_index)) — la méthode publique existante, inchangée.
// =============================================================================

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::sync::Arc;

use marius_projection::Projection;

use crate::batch_renderer::BatchRenderer;
use crate::pack_html_format::write_packfile_footer;
use crate::pack_html_index::PackHtmlIndex;
use crate::registry::{packfile_path_for, LiveRegistry};

/// Taille de chunk pour le streaming fetch_batch → render_batch. Non
/// spécifiée par la spec §7 (qui écrit `ids.chunks(CHUNK_SIZE)` sans jamais
/// définir la constante) — valeur choisie ici, pas héritée d'un document.
/// Borne la taille de la clause SQL IN côté fetch_batch et la taille du
/// buffer de rendu en vol, sans incidence sur la correction du format
/// on-disk produit : le chunking est invisible une fois le footer écrit
/// (même propriété que chained_batches_offsets_are_contiguous,
/// batch_renderer.rs).
const CHUNK_SIZE: usize = 1024;

/// Régénère un packfile HTML complet pour une cible (`packfile_key`), et
/// bascule atomiquement le `LiveRegistry` vers la nouvelle version.
///
/// Appelée par le Dispatcher (mutation réactive, ADR-002) ou par un outil de
/// dump initial — jamais par le chemin de lecture. Générique sur `P` pour
/// rester partagée entre les deux usages (roadmap Phase 4).
///
/// `ids` : précondition critique, non vérifiée ici (spec §3 — même limite
/// que le format lui-même) — doit être trié ASC. L'appelant porte la
/// responsabilité du tri, comme `dumper.rs` le garantit déjà via
/// `ORDER BY id ASC` côté SQL.
///
/// `ids` doit représenter l'ensemble complet attendu dans ce packfile, pas
/// seulement les enregistrements modifiés depuis le dernier appel : cette
/// fonction réécrit le fichier en entier à chaque appel, elle ne fusionne
/// jamais avec le contenu précédent. Un appelant qui ne passerait qu'un
/// sous-ensemble (un delta) ferait disparaître du packfile tout id absent
/// de ce sous-ensemble au moment du swap — voir la note sur ce risque dans
/// le câblage de dispatcher.rs (Collector::flush()).
pub async fn regenerate_and_swap<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    total_cap: usize,
    packfile_key: &'static str,
    registry: &LiveRegistry,
) -> std::io::Result<()> {
    let final_path = packfile_path_for(packfile_key);
    let tmp_path = final_path.with_extension("tmp");

    // Cas dump initial : artifacts/ peut ne pas encore exister — absent du
    // pseudocode de la spec, ajouté ici pour que cette fonction reste
    // appelable avant tout cold_start() (roadmap : "le dump initial et la
    // mutation Dispatcher l'appellent tous les deux"). cold_start() ne crée
    // jamais ce répertoire lui-même (il échoue fort si absent, par
    // construction — registry.rs) ; cet appel n'est donc pas redondant.
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(&tmp_path)?;
    let mut writer = BufWriter::new(file);

    let mut renderer = BatchRenderer::<P>::new(total_cap, ids.len().min(CHUNK_SIZE));
    let mut full_index = Vec::with_capacity(ids.len());
    let mut offset = 0u64;

    for chunk in ids.chunks(CHUNK_SIZE) {
        let batch = P::fetch_batch(pool, chunk)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        offset = renderer.render_batch(&batch, &mut writer, offset)?;
        full_index.extend_from_slice(renderer.index());
        renderer.reset(CHUNK_SIZE);
    }

    // blob_len = offset accumulé, jamais sink.get_ref().len() (BufWriter
    // tamponne en interne — même piège déjà documenté dans
    // footer_and_index_roundtrip, batch_renderer.rs).
    write_packfile_footer(&mut writer, offset, &full_index)?;
    writer.flush()?;
    writer.get_ref().sync_all()?; // durabilité avant rename — un crash entre
                                   // les deux ne doit jamais laisser le
                                   // fichier final tronqué.

    fs::rename(&tmp_path, &final_path)?; // atomique (même filesystem, POSIX)

    let new_index = PackHtmlIndex::open(&final_path)?;

    // Dernière étape, sans exception : tout Err ci-dessus (fetch_batch,
    // écriture, fsync, rename, réouverture) retourne avant cette ligne —
    // l'ancien Arc reste servi, aucune requête en vol n'est interrompue.
    // Panique volontairement si packfile_key n'a jamais été provisionné à
    // la construction du LiveRegistry — invariant AOT de
    // LiveRegistry::store(), pas contourné ici.
    registry.store(packfile_key, Arc::new(new_index));

    Ok(())
}

// =============================================================================
// Tests — Jalon 4a
//
// Sans PostgreSQL, sans serveur réel — StubProjection (même pattern que
// batch_renderer.rs), appelée deux fois de suite avec des données
// différentes sur un LiveRegistry de test.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::PackfileEntry;
    use arc_swap::ArcSwap;
    use std::collections::HashMap;
    use std::os::unix::fs::FileExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ── Projection stub ──────────────────────────────────────────────────────
    //
    // Même structure que batch_renderer.rs::StubProjection, étendue pour
    // produire un contenu observable et distinct par génération : le stub de
    // batch_renderer.rs ignore pool/ids et renvoie toujours vec![], ce qui ne
    // permettrait pas de distinguer deux régénérations successives ici.
    //
    // CURRENT_GENERATION est le seul canal disponible pour faire varier le
    // contenu retourné par fetch_batch entre deux appels de
    // regenerate_and_swap dans ce test : le pool est un stub jamais
    // réellement interrogé (Jalon 4a : "sans PostgreSQL").

    static CURRENT_GENERATION: AtomicU64 = AtomicU64::new(0);

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct StubRecord {
        id: i32,
        generation: i64,
    }

    struct StubProjection;

    const STUB_TOTAL_CAP: usize = 32;

    impl Projection for StubProjection {
        type Record = StubRecord;
        type VarlenOwned = ();

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            ids: &[i64],
        ) -> impl std::future::Future<Output = marius_projection::BatchResult<Self>> + Send
        {
            let generation = CURRENT_GENERATION.load(Ordering::Relaxed) as i64;
            let batch = ids
                .iter()
                .map(|&id| (StubRecord { id: id as i32, generation }, ()))
                .collect::<Vec<_>>();
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
            // Non utilisé par regenerate_and_swap, qui passe par
            // packfile_path_for(packfile_key) — jamais P::packfile_path()
            // (voir note de module ci-dessus, point de fidélité à la spec
            // §7, pas une divergence). Présent uniquement parce que le
            // trait Projection l'exige, même obligation que dans
            // batch_renderer.rs::StubProjection.
            PathBuf::from("artifacts/unused_stub_pack.bin")
        }

        fn store_path() -> PathBuf {
            PathBuf::from("unused_stub_store.bin")
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn unique_test_key(label: &str) -> &'static str {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Box::leak(format!("jalon4a_{label}_{}_{n}", std::process::id()).into_boxed_str())
    }

    /// PgPool jamais réellement connecté ni interrogé — StubProjection::
    /// fetch_batch ignore ce paramètre. connect_lazy ne touche jamais le
    /// réseau tant qu'aucune requête n'est exécutée à travers lui, ce qui
    /// n'arrive jamais dans ce test (Jalon 4a : "sans PostgreSQL").
    fn stub_pool() -> sqlx::PgPool {
        sqlx::PgPool::connect_lazy("postgres://stub-unused-in-jalon-4a/db")
            .expect("connect_lazy ne touche jamais le réseau")
    }

    /// Construit un PackHtmlIndex hors de regenerate_and_swap, pour amorcer
    /// le LiveRegistry de test avant le premier appel réel — n'utilise pas
    /// regenerate_and_swap lui-même (éviterait de tester autre chose que ce
    /// qu'on amorce).
    fn bootstrap_packfile(key: &'static str, ids: &[i64], generation: u64) -> PackHtmlIndex {
        let frag = format!("<g{generation}>");
        let path = packfile_path_for(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("création artifacts/ de test");
        }

        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(ids.len());
        let mut offset = 0u64;
        for &id in ids {
            blob.extend_from_slice(frag.as_bytes());
            entries.push(PackfileEntry {
                id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        let file = File::create(&path).expect("création packfile bootstrap");
        let mut writer = BufWriter::new(file);
        writer.write_all(&blob).expect("écriture blob bootstrap");
        write_packfile_footer(&mut writer, offset, &entries).expect("écriture footer bootstrap");
        writer.flush().expect("flush bootstrap");

        PackHtmlIndex::open(&path).expect("réouverture bootstrap")
    }

    fn read_fragment(file: &File, offset: u64, len: u32) -> String {
        let mut buf = vec![0u8; len as usize];
        file.read_at(&mut buf, offset).expect("read_at fragment");
        String::from_utf8(buf).expect("fragment UTF-8 valide")
    }

    // ── Test principal — Jalon 4a ─────────────────────────────────────────────

    #[tokio::test]
    async fn regenerate_and_swap_twice_swaps_atomically_and_preserves_in_flight_reader() {
        let key = unique_test_key("twice");
        let pool = stub_pool();

        // LiveRegistry de test : la clé doit être provisionnée à l'avance
        // (with_indices) — store() panique sinon (invariant AOT,
        // registry.rs). Amorce : génération 0, id unique.
        let bootstrap = bootstrap_packfile(key, &[1], 0);
        let mut indices = HashMap::new();
        indices.insert(key, ArcSwap::from_pointee(bootstrap));
        let registry = LiveRegistry::with_indices(indices);

        let tmp_path = packfile_path_for(key).with_extension("tmp");

        // ── Première régénération réelle via regenerate_and_swap ──────────
        CURRENT_GENERATION.store(1, Ordering::Relaxed);
        regenerate_and_swap::<StubProjection>(&pool, &[1, 2, 3], STUB_TOTAL_CAP, key, &registry)
            .await
            .expect("première régénération doit réussir");

        assert!(
            !tmp_path.exists(),
            ".tmp ne doit jamais survivre à un rename() réussi (génération 1)"
        );

        let gen1_arc = registry.load(key).expect("clé provisionnée");
        assert_eq!(gen1_arc.entry_count(), 3, "génération 1 doit contenir 3 entrées");
        let (offset, len) = gen1_arc.lookup(2).expect("id=2 présent en génération 1");
        assert_eq!(
            read_fragment(gen1_arc.file(), offset, len),
            "<g1>",
            "fragment id=2 doit refléter la génération 1"
        );

        // Lecteur "en vol" : Arc cloné AVANT la seconde régénération — doit
        // continuer à fonctionner après le swap (3ᵉ point de vigilance,
        // fd/inode, repris du Jalon 2).
        let in_flight_reader = gen1_arc.clone();

        // ── Seconde régénération, données différentes ──────────────────────
        CURRENT_GENERATION.store(2, Ordering::Relaxed);
        regenerate_and_swap::<StubProjection>(
            &pool,
            &[1, 2, 3, 4],
            STUB_TOTAL_CAP,
            key,
            &registry,
        )
        .await
        .expect("seconde régénération doit réussir");

        assert!(
            !tmp_path.exists(),
            ".tmp ne doit jamais survivre à un rename() réussi (génération 2)"
        );

        // Assertion 1 : le nouvel index lu après le swap reflète les
        // nouvelles données (4 entrées, contenu de génération 2).
        let gen2_arc = registry.load(key).expect("clé toujours provisionnée");
        assert_eq!(gen2_arc.entry_count(), 4, "génération 2 doit contenir 4 entrées");
        let (offset, len) = gen2_arc
            .lookup(4)
            .expect("id=4 présent en génération 2 seulement");
        assert_eq!(
            read_fragment(gen2_arc.file(), offset, len),
            "<g2>",
            "fragment id=4 doit refléter la génération 2"
        );

        // Assertion 2 : le lecteur ayant chargé l'Arc avant le swap continue
        // de fonctionner sans coupure — toujours 3 entrées, toujours <g1>,
        // jamais de fd invalidé par le rename de la génération 2 (le fd
        // ouvert par in_flight_reader pointe sur l'inode renommé en .tmp
        // au moment de son ouverture, jamais sur le fichier final actuel).
        assert_eq!(
            in_flight_reader.entry_count(),
            3,
            "le lecteur en vol ne doit pas voir la nouvelle génération"
        );
        let (offset, len) = in_flight_reader
            .lookup(2)
            .expect("id=2 toujours résolu côté ancien Arc");
        assert_eq!(
            read_fragment(in_flight_reader.file(), offset, len),
            "<g1>",
            "le lecteur en vol doit continuer à lire la génération 1, pas la 2"
        );
    }

    // ── Conformité à l'invariant AOT de LiveRegistry::store() ────────────────

    #[tokio::test]
    #[should_panic(expected = "violation de l'invariant AOT")]
    async fn regenerate_and_swap_panics_on_unprovisioned_key() {
        // regenerate_and_swap ne doit pas contourner par un Err silencieux
        // l'invariant déjà posé par registry.rs : une clé jamais
        // provisionnée à la construction du LiveRegistry est un bug
        // interne, pas une erreur de requête — panic, pas Result.
        let pool = stub_pool();
        let registry = LiveRegistry::with_indices(HashMap::new());
        CURRENT_GENERATION.store(0, Ordering::Relaxed);
        let _ = regenerate_and_swap::<StubProjection>(
            &pool,
            &[1],
            STUB_TOTAL_CAP,
            "jamais_provisionnee_jalon4a",
            &registry,
        )
        .await;
    }
}
