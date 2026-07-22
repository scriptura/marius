// marius-render · crates/shell/render/src/ingest_and_swap.rs
//
// cf. DFS-phase1-reactivite-cow.md §3.4, §6
//
// Étage 1 du pipeline réactif : fetch_from_pg(pool, ids) → merge_store →
// écriture .tmp + fsync → VALIDATION → rename atomique → swap StoreRegistry.
//
// ── Correction au design gelé (DESIGN-store-registry.md §6), documentée ici
// plutôt que silencieusement appliquée ──────────────────────────────────────
// §6 prévoyait une réouverture de validation APRÈS le rename. Implémentation
// réelle : la validation a lieu AVANT le rename, sur le fichier .tmp. Sur un
// rename same-filesystem (garanti par construction : .tmp et le fichier
// final partagent le même répertoire), rename() est purement une opération
// de métadonnées — un fichier qui valide avant rename valide identiquement
// après. Valider avant élimine toute fenêtre, même théorique, où un fichier
// non revalidé pourrait se trouver au chemin canonique — strictement plus
// sûr, y compris pour la reprise après crash (cold_start ne peut jamais
// tomber sur un fichier renommé mais non validé). Le handle de lecture déjà
// obtenu par cette validation est réutilisé pour le swap : rename() ne
// change pas l'inode, donc ce handle reste correct après coup — pas de
// second open().
//
// ── Transactionnalité des effets de bord — analyse ─────────────────────────
// Tout échec avant le rename (fetch SQL, écriture, fsync, validation) laisse
// store.bin ET le StoreRegistry strictement inchangés — le fichier .tmp est
// nettoyé au mieux (best-effort), jamais exposé. Le seul état intermédiaire
// possible est la fenêtre entre rename() (réussi) et swap() (pas encore
// exécuté) : le fichier sur disque est déjà la nouvelle version, le
// StoreRegistry sert encore l'ancienne. Cette fenêtre est sans risque —
// fetch_batch reste cohérent (ancienne version, valide) pendant sa durée, et
// un crash pendant cette fenêtre laisse un état repartable : au redémarrage,
// cold_start lit le fichier déjà renommé (donc déjà validé), jamais un état
// torn. Il n'existe donc aucun scénario d'échec qui expose une donnée
// invalide, que ce soit en mémoire ou sur disque.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytemuck::Pod;

use marius_projection::Projection;
use marius_projection::packfile_reader::PackfileReader;

use crate::merge_store::{MergeStoreReport, merge_store};
use crate::packfile_builder::PackfileBuilder;

pub async fn ingest_and_swap<P: Projection>(
    pool: &sqlx::PgPool,
    ids: &[i64],
    io_semaphore: &tokio::sync::Semaphore,
) -> io::Result<MergeStoreReport>
where
    P::Record: Pod,
{
    // ── Fetch SQL live — async, hors du permis d'I/O disque (celui-ci ne
    // régule que la pression disque, pas le réseau ; même découpage que
    // regenerate_and_swap, qui acquiert son permis juste avant spawn_blocking
    // et jamais avant le fetch Postgres). ────────────────────────────────────
    let delta = P::fetch_from_pg(pool, ids)
        .await
        .map_err(io::Error::other)?;

    // delta trié par id (ORDER BY côté SQL généré) → recherche binaire O(log n)
    // pour déterminer les suppressions, sans re-trier.
    let delta_ids: Vec<i64> = delta.iter().map(|(r, _)| P::record_id(r)).collect();
    let deleted_ids: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|id| delta_ids.binary_search(id).is_err())
        .collect();

    let store_path = P::store_path();

    // ── Permis d'I/O disque — partagé avec regenerate_and_swap (même
    // instance de Semaphore transmise par Dispatcher::run), pour que la
    // pression disque totale du tick (deux étages) reste bornée par un seul
    // budget, pas doublée. ───────────────────────────────────────────────────
    let _permit = io_semaphore
        .acquire()
        .await
        .map_err(|_| io::Error::other("io_semaphore fermé de manière inattendue"))?;

    let registry = P::store_registry();

    tokio::task::spawn_blocking(move || {
        ingest_and_swap_sync::<P>(&store_path, &delta, &deleted_ids, registry)
    })
    .await
    .map_err(io::Error::other)?
}

/// Portion synchrone, bloquante — mmap/fichier/fsync/rename, aucun await.
/// Prend `old` via `registry.load()` en tête, une seule fois : la fusion
/// entière est résolue contre une unique version de store.bin (même
/// discipline qu'INV-5 côté lecture, appliquée ici côté écriture).
fn ingest_and_swap_sync<P: Projection>(
    store_path: &Path,
    delta: &[(P::Record, P::VarlenOwned)],
    deleted_ids: &[i64],
    registry: &marius_projection::StoreRegistry<P>,
) -> io::Result<MergeStoreReport>
where
    P::Record: Pod,
{
    let old = registry.load();

    let mut builder = PackfileBuilder::<P>::new(old.row_count() + delta.len());
    let report = merge_store(&old, delta, deleted_ids, &mut builder);
    drop(old); // ne conserve pas la référence au-delà de la fusion — inutile ensuite.

    let tmp_path = tmp_path_for(store_path);

    // Écriture + fsync — tout échec ici laisse store_path/registry intacts.
    let result = (|| -> io::Result<()> {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = std::io::BufWriter::new(file);
        builder.write(&mut writer)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(())
    })();

    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp_path); // best-effort, jamais fatal
        return Err(e);
    }

    // Validation AVANT rename (cf. en-tête) — échec = store_path jamais touché.
    let validated = match PackfileReader::<P>::open(&tmp_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    };

    if let Err(e) = std::fs::rename(&tmp_path, store_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // rename() ne change pas l'inode : `validated` reste correct après coup,
    // pas de second open() nécessaire.
    registry.swap(Arc::new(validated));

    Ok(report)
}

fn tmp_path_for(store_path: &Path) -> PathBuf {
    store_path.with_extension("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::{BatchResult, StoreRegistry, VarlenSlot};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct TestRecord {
        id: i64,
        val: i64,
    }

    // ── État de test global, réinitialisé au début de chaque test ─────────
    // Exécution en séquentiel (--test-threads=1) : pas de race sur ces statics.
    static FETCH_RESULT: Mutex<Vec<(i64, i64, Option<String>)>> = Mutex::new(Vec::new());
    static FETCH_SHOULD_FAIL: AtomicBool = AtomicBool::new(false);
    static STORE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    struct IngestTestProj;

    impl Projection for IngestTestProj {
        type Record = TestRecord;
        type VarlenOwned = Option<String>;

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
            std::future::ready(Ok(Vec::new()))
        }

        fn fetch_from_pg(
            _pool: &sqlx::PgPool,
            ids: &[i64],
        ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
            let should_fail = FETCH_SHOULD_FAIL.load(Ordering::SeqCst);
            let ids = ids.to_vec();
            std::future::ready(if should_fail {
                Err(sqlx::Error::Configuration("échec SQL simulé".into()))
            } else {
                let all = FETCH_RESULT.lock().unwrap();
                Ok(all
                    .iter()
                    .filter(|(id, _, _)| ids.contains(id))
                    .map(|(id, val, s)| (TestRecord { id: *id, val: *val }, s.clone()))
                    .collect())
            })
        }

        fn render(_r: &Self::Record, _v: &Self::VarlenOwned, _buf: &mut String) {}

        fn record_id(record: &Self::Record) -> i64 {
            record.id
        }

        fn packfile_path() -> PathBuf {
            PathBuf::new()
        }

        fn store_path() -> PathBuf {
            STORE_PATH
                .lock()
                .unwrap()
                .clone()
                .expect("STORE_PATH non initialisé par le test")
        }

        fn store_registry() -> &'static StoreRegistry<Self> {
            static REGISTRY: StoreRegistry<IngestTestProj> = StoreRegistry::new();
            &REGISTRY
        }

        fn varlena_field_count() -> u16 {
            1
        }

        fn encode_varlena(
            varlena: &Self::VarlenOwned,
            heap: &mut Vec<u8>,
            toc: &mut Vec<VarlenSlot>,
        ) {
            match varlena {
                Some(s) => {
                    let offset = heap.len() as u32;
                    heap.extend_from_slice(s.as_bytes());
                    toc.push(VarlenSlot {
                        offset,
                        len: s.len() as u32,
                    });
                }
                None => toc.push(VarlenSlot {
                    offset: u32::MAX,
                    len: 0,
                }),
            }
        }
    }

    fn tmp_test_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ingest_and_swap_test_{}_{}",
            std::process::id(),
            name
        ));
        p
    }

    /// Réinitialise l'état global de test et construit un store.bin initial.
    fn setup(name: &str, initial_rows: &[(i64, i64, Option<&str>)]) -> PathBuf {
        FETCH_SHOULD_FAIL.store(false, Ordering::SeqCst);
        *FETCH_RESULT.lock().unwrap() = Vec::new();

        let path = tmp_test_path(name);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("tmp"));
        *STORE_PATH.lock().unwrap() = Some(path.clone());

        let mut builder = PackfileBuilder::<IngestTestProj>::new(initial_rows.len());
        let batch: Vec<(TestRecord, Option<String>)> = initial_rows
            .iter()
            .map(|(id, val, s)| (TestRecord { id: *id, val: *val }, s.map(|s| s.to_string())))
            .collect();
        builder.push_batch(&batch);
        let file = std::fs::File::create(&path).unwrap();
        let mut w = std::io::BufWriter::new(file);
        builder.write(&mut w).unwrap();
        drop(w);

        IngestTestProj::store_registry().cold_start(&path).unwrap();
        path
    }

    fn all_rows_on_disk(path: &Path) -> Vec<(i64, i64, Option<String>)> {
        let reader = PackfileReader::<IngestTestProj>::open(path).unwrap();
        reader
            .id_index()
            .iter()
            .map(|&id| {
                let (rec, varlena) = reader.lookup(id).unwrap();
                (id, rec.val, varlena.get(0).map(|s| s.to_string()))
            })
            .collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn success_path_updates_disk_and_registry_consistently() {
        let path = setup(
            "success",
            &[
                (1, 1, Some("un")),
                (2, 2, Some("deux")),
                (3, 3, Some("trois")),
            ],
        );

        // id 2 mis à jour, id 3 supprimé (absent du résultat), id 4 inséré.
        *FETCH_RESULT.lock().unwrap() = vec![
            (2, 200, Some("deux-modifie".to_string())),
            (4, 400, Some("quatre-nouveau".to_string())),
        ];

        let held_before = IngestTestProj::store_registry().load(); // INV-3, sur le vrai chemin

        // connect_lazy : ne se connecte pas immédiatement (parse l'URL
        // seulement), valide même sans base réelle disponible — ces tests
        // n'exécutent jamais de requête via ce pool (Projection de test,
        // fetch_from_pg contrôlé par état en mémoire). sqlx::PgPool est un
        // type alias (Pool<Postgres>), pas une struct unitaire : ne jamais
        // écrire `sqlx::PgPool` comme valeur.
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/marius_test")
            .expect("connect_lazy ne doit jamais échouer avant la première requête");
        let sem = tokio::sync::Semaphore::new(1);
        let report = ingest_and_swap::<IngestTestProj>(&pool, &[2, 3, 4], &sem)
            .await
            .expect("ingest_and_swap doit réussir");

        assert_eq!(report.rows_copied_from_old, 1); // id 1
        assert_eq!(report.rows_updated, 1); // id 2
        assert_eq!(report.rows_deleted, 1); // id 3
        assert_eq!(report.rows_inserted_from_delta, 1); // id 4

        // Le disque et le registre convergent tous deux vers le même état.
        let expected = vec![
            (1, 1, Some("un".to_string())),
            (2, 200, Some("deux-modifie".to_string())),
            (4, 400, Some("quatre-nouveau".to_string())),
        ];
        assert_eq!(all_rows_on_disk(&path), expected);

        let after = IngestTestProj::store_registry().load();
        let rows_in_registry: Vec<_> = after
            .id_index()
            .iter()
            .map(|&id| {
                let (rec, v) = after.lookup(id).unwrap();
                (id, rec.val, v.get(0).map(|s| s.to_string()))
            })
            .collect();
        assert_eq!(rows_in_registry, expected);

        // INV-3 : la référence détenue avant l'appel voit toujours l'ancien état.
        assert_eq!(held_before.lookup(2).unwrap().0.val, 2);
        assert!(held_before.lookup(4).is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_failure_leaves_disk_and_registry_untouched() {
        let path = setup("fetch_fail", &[(1, 1, Some("un"))]);
        FETCH_SHOULD_FAIL.store(true, Ordering::SeqCst);

        let before = all_rows_on_disk(&path);

        // connect_lazy : ne se connecte pas immédiatement (parse l'URL
        // seulement), valide même sans base réelle disponible — ces tests
        // n'exécutent jamais de requête via ce pool (Projection de test,
        // fetch_from_pg contrôlé par état en mémoire). sqlx::PgPool est un
        // type alias (Pool<Postgres>), pas une struct unitaire : ne jamais
        // écrire `sqlx::PgPool` comme valeur.
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/marius_test")
            .expect("connect_lazy ne doit jamais échouer avant la première requête");
        let sem = tokio::sync::Semaphore::new(1);
        let result = ingest_and_swap::<IngestTestProj>(&pool, &[1], &sem).await;

        assert!(result.is_err());
        assert_eq!(all_rows_on_disk(&path), before); // disque intact
        assert_eq!(
            IngestTestProj::store_registry()
                .load()
                .lookup(1)
                .unwrap()
                .0
                .val,
            1
        ); // registre intact
        assert!(!path.with_extension("tmp").exists()); // aucun .tmp créé (échec avant écriture)

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_failure_leaves_disk_and_registry_untouched() {
        // Répertoire parent inexistant → File::create(.tmp) échoue avant
        // toute écriture réelle — exerce le chemin d'échec le plus précoce
        // après le fetch SQL.
        let path = setup("write_fail", &[(1, 1, Some("un"))]);
        *STORE_PATH.lock().unwrap() = Some(PathBuf::from("/chemin/inexistant/store.bin"));
        *FETCH_RESULT.lock().unwrap() = vec![(1, 999, Some("ecrasee".to_string()))];

        // connect_lazy : ne se connecte pas immédiatement (parse l'URL
        // seulement), valide même sans base réelle disponible — ces tests
        // n'exécutent jamais de requête via ce pool (Projection de test,
        // fetch_from_pg contrôlé par état en mémoire). sqlx::PgPool est un
        // type alias (Pool<Postgres>), pas une struct unitaire : ne jamais
        // écrire `sqlx::PgPool` comme valeur.
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/marius_test")
            .expect("connect_lazy ne doit jamais échouer avant la première requête");
        let sem = tokio::sync::Semaphore::new(1);
        let result = ingest_and_swap::<IngestTestProj>(&pool, &[1], &sem).await;

        assert!(result.is_err());
        // Le registre (monté sur l'ancien `path`, valide) reste inchangé —
        // la tentative visait un chemin différent qui n'a jamais pu s'écrire.
        assert_eq!(
            IngestTestProj::store_registry()
                .load()
                .lookup(1)
                .unwrap()
                .0
                .val,
            1
        );
        assert_eq!(all_rows_on_disk(&path)[0].1, 1); // fichier d'origine intact

        let _ = std::fs::remove_file(&path);
    }
}
