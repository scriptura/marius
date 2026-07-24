// marius-render · crates/shell/render/src/merge_store.rs

//! Algorithme de Fusion Binaire AOT (*Merge Store*).
//!
//! Exécute la réconciliation Copy-on-Write entre une projection binaire existante (`store.bin`)
//! et un delta SQL transitoire (insertions, mutations, suppressions) au sein du pipeline réactif.
//!
//! ## Invariants & Performance CPU
//!
//! - **Isolât Mémoire $O(1)$ I/O :** Algorithme purement synchrone et in-memory. Zéro dépendance réseau,
//!   zéro appel système, zéro primitive asynchrone (`async`). Opère exclusivement sur des tranches
//!   de mémoire contiguous (`&[u8]`).
//! - **Copies d'Invariants par Bloc (*memcpy*) :** Les plages d'enregistrements non affectées par le delta
//!   sont transférées directement d'un tampon mémoire à l'autre via `PackfileBuilder::push_raw_run`.
//!   Aucun coût d'encodage ni de désérialisation n'est payé pour la donnée inerte.
//! - **Encodage Sélectif :** Seules les lignes mutées issues du delta passent par la passe d'encodage
//!   des variables de longueur dynamique (`P::encode_varlena`).
//!
//! ## Remarque d'Architecture (TODO Dette d'Emplacement)
//!
//! Déplacé temporairement dans `crates/shell/render` en raison de sa dépendance directe envers
//! `PackfileBuilder`. Candidat naturel à une migration AOT vers `crates/core/projection` 
//! lorsque le constructeur de paquets sera complètement extrait du Shell.

use bytemuck::Pod;
use marius_projection::packfile_reader::PackfileReader;
use marius_projection::{Projection, VarlenSlot};

use crate::packfile_builder::PackfileBuilder;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStoreReport {
    pub runs_count: usize,
    pub rows_copied_from_old: usize,
    pub rows_inserted_from_delta: usize,
    pub rows_updated: usize,
    pub rows_deleted: usize,
}

/// Fusionne `old` avec `delta`/`deleted_ids` dans `out`.
///
/// Préconditions, non revérifiées ici (responsabilité de l'appelant,
/// `ingest_and_swap`) :
/// - `old.id_index()` strictement trié croissant (garanti par le format).
/// - `delta` trié par id croissant (hérité de `ORDER BY` côté SQL généré).
/// - `deleted_ids` trié croissant.
/// - `delta` et `deleted_ids` disjoints (un id n'est jamais dans les deux).
pub fn merge_store<P: Projection>(
    old: &PackfileReader<P>,
    delta: &[(P::Record, P::VarlenOwned)],
    deleted_ids: &[i64],
    out: &mut PackfileBuilder<P>,
) -> MergeStoreReport
where
    P::Record: Pod,
{
    let old_ids = old.id_index();
    let old_records = old.records();
    let old_toc = old.toc();
    let old_heap = old.heap();
    let vf = old.varlena_field_count();

    let mut report = MergeStoreReport::default();

    let mut old_pos = 0usize;
    let n_old = old_ids.len();
    let mut delta_pos = 0usize;
    let mut del_pos = 0usize;

    // Frontière du run d'old actuellement en cours de constitution.
    let mut run_start: Option<usize> = None;

    macro_rules! flush_run {
        ($end:expr) => {
            if let Some(start) = run_start.take() {
                let end = $end;
                if end > start {
                    push_run(out, old_records, old_toc, old_heap, vf, start, end);
                    report.runs_count += 1;
                    report.rows_copied_from_old += end - start;
                }
            }
        };
    }

    while old_pos < n_old {
        let old_id = old_ids[old_pos];

        // Un id supprimé consomme la ligne old correspondante sans l'émettre.
        if del_pos < deleted_ids.len() && deleted_ids[del_pos] == old_id {
            flush_run!(old_pos);
            del_pos += 1;
            old_pos += 1;
            report.rows_deleted += 1;
            continue;
        }

        // Un id présent dans le delta remplace la ligne old correspondante.
        if delta_pos < delta.len() && P::record_id(&delta[delta_pos].0) == old_id {
            flush_run!(old_pos);
            out.push_batch(&delta[delta_pos..delta_pos + 1]);
            report.rows_updated += 1;
            delta_pos += 1;
            old_pos += 1;
            continue;
        }

        // Un id du delta strictement inférieur à old_id est une insertion :
        // à émettre maintenant, avant de continuer à avancer sur `old`.
        while delta_pos < delta.len() && P::record_id(&delta[delta_pos].0) < old_id {
            flush_run!(old_pos);
            out.push_batch(&delta[delta_pos..delta_pos + 1]);
            report.rows_inserted_from_delta += 1;
            delta_pos += 1;
        }

        // Ligne old non touchée : étend le run courant (aucune écriture ici).
        if run_start.is_none() {
            run_start = Some(old_pos);
        }
        old_pos += 1;
    }
    flush_run!(old_pos);

    // Insertions restantes, id strictement supérieur à toutes les ids de `old`.
    while delta_pos < delta.len() {
        out.push_batch(&delta[delta_pos..delta_pos + 1]);
        report.rows_inserted_from_delta += 1;
        delta_pos += 1;
    }

    report
}

/// Émet le run `[start, end)` de `old` par memcpy pur (`push_raw_run`).
fn push_run<P: Projection>(
    out: &mut PackfileBuilder<P>,
    old_records: &[P::Record],
    old_toc: &[VarlenSlot],
    old_heap: &[u8],
    vf: usize,
    start: usize,
    end: usize,
) where
    P::Record: Pod,
{
    let records = &old_records[start..end];
    let toc = &old_toc[start * vf..end * vf];

    let (heap_lo, heap_hi) = heap_span(toc);
    let heap = &old_heap[heap_lo as usize..heap_hi as usize];

    out.push_raw_run(records, toc, heap_lo, heap);
}

/// Borne min/max (offset .. offset+len) d'un ensemble de slots, sentinelles
/// (`u32::MAX`) ignorées. `(0, 0)` si tous sentinelles ou `toc` vide — la
/// tranche heap correspondante est alors vide, cohérent avec `push_raw_run`.
fn heap_span(toc: &[VarlenSlot]) -> (u32, u32) {
    let mut lo = u32::MAX;
    let mut hi = 0u32;
    for slot in toc {
        if slot.offset == u32::MAX {
            continue;
        }
        lo = lo.min(slot.offset);
        hi = hi.max(slot.offset + slot.len);
    }
    if lo == u32::MAX { (0, 0) } else { (lo, hi) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::BatchResult;
    use std::path::PathBuf;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct TestRecord {
        id: i64,
        val: i64,
    }

    struct TestProj;

    impl Projection for TestProj {
        type Record = TestRecord;
        type VarlenOwned = Option<String>;

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
            std::future::ready(Ok(Vec::new()))
        }

        fn render(_r: &Self::Record, _v: &Self::VarlenOwned, _buf: &mut String) {}

        fn record_id(record: &Self::Record) -> i64 {
            record.id
        }

        fn packfile_path() -> PathBuf {
            PathBuf::new()
        }

        fn store_path() -> PathBuf {
            PathBuf::new()
        }

        fn store_registry() -> &'static marius_projection::StoreRegistry<Self> {
            static REGISTRY: marius_projection::StoreRegistry<TestProj> =
                marius_projection::StoreRegistry::new();
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

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("merge_store_test_{}_{}", std::process::id(), name));
        p
    }

    /// Construit un store.bin réel via PackfileBuilder (pas de bytes à la
    /// main) — rows = (id, val, texte varlena optionnel).
    fn build_store(
        path: &std::path::Path,
        rows: &[(i64, i64, Option<&str>)],
    ) -> PackfileReader<TestProj> {
        let mut builder = PackfileBuilder::<TestProj>::new(rows.len());
        let batch: Vec<(TestRecord, Option<String>)> = rows
            .iter()
            .map(|(id, val, s)| (TestRecord { id: *id, val: *val }, s.map(|s| s.to_string())))
            .collect();
        builder.push_batch(&batch);

        let file = std::fs::File::create(path).unwrap();
        let mut w = std::io::BufWriter::new(file);
        builder.write(&mut w).unwrap();
        drop(w);

        PackfileReader::<TestProj>::open(path).unwrap()
    }

    fn all_rows(reader: &PackfileReader<TestProj>) -> Vec<(i64, i64, Option<String>)> {
        reader
            .id_index()
            .iter()
            .map(|&id| {
                let (rec, varlena) = reader.lookup(id).unwrap();
                (id, rec.val, varlena.get(0).map(|s| s.to_string()))
            })
            .collect()
    }

    #[test]
    fn empty_delta_preserves_old_exactly() {
        let old_path = tmp_path("empty_delta_old.bin");
        let out_path = tmp_path("empty_delta_out.bin");
        let old = build_store(
            &old_path,
            &[(1, 10, Some("un")), (2, 20, None), (3, 30, Some("trois"))],
        );

        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count());
        let report = merge_store(&old, &[], &[], &mut builder);

        assert_eq!(report.runs_count, 1); // toute la table est une seule run non touchée
        assert_eq!(report.rows_copied_from_old, 3);
        assert_eq!(report.rows_updated, 0);
        assert_eq!(report.rows_inserted_from_delta, 0);
        assert_eq!(report.rows_deleted, 0);

        let file = std::fs::File::create(&out_path).unwrap();
        let mut w = std::io::BufWriter::new(file);
        builder.write(&mut w).unwrap();
        drop(w);
        let out = PackfileReader::<TestProj>::open(&out_path).unwrap();

        assert_eq!(
            all_rows(&out),
            vec![
                (1, 10, Some("un".to_string())),
                (2, 20, None),
                (3, 30, Some("trois".to_string())),
            ]
        );

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn full_replace_zero_runs_copied() {
        let old_path = tmp_path("full_replace_old.bin");
        let out_path = tmp_path("full_replace_out.bin");
        let old = build_store(&old_path, &[(1, 1, Some("a")), (2, 2, Some("b"))]);

        let delta = vec![
            (
                TestRecord { id: 1, val: 100 },
                Some("A-nouveau".to_string()),
            ),
            (
                TestRecord { id: 2, val: 200 },
                Some("B-nouveau".to_string()),
            ),
        ];
        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count());
        let report = merge_store(&old, &delta, &[], &mut builder);

        assert_eq!(report.runs_count, 0);
        assert_eq!(report.rows_copied_from_old, 0);
        assert_eq!(report.rows_updated, 2);

        write_and_reopen(&builder, &out_path, |out| {
            assert_eq!(
                all_rows(&out),
                vec![
                    (1, 100, Some("A-nouveau".to_string())),
                    (2, 200, Some("B-nouveau".to_string())),
                ]
            );
        });

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn insertions_head_middle_tail() {
        let old_path = tmp_path("insert_old.bin");
        let out_path = tmp_path("insert_out.bin");
        let old = build_store(
            &old_path,
            &[
                (2, 20, Some("deux")),
                (4, 40, Some("quatre")),
                (6, 60, Some("six")),
            ],
        );

        let delta = vec![
            (TestRecord { id: 1, val: 10 }, Some("un".to_string())), // tête
            (TestRecord { id: 5, val: 50 }, Some("cinq".to_string())), // milieu
            (TestRecord { id: 7, val: 70 }, Some("sept".to_string())), // queue
        ];
        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count() + delta.len());
        let report = merge_store(&old, &delta, &[], &mut builder);

        assert_eq!(report.rows_inserted_from_delta, 3);
        assert_eq!(report.rows_copied_from_old, 3);

        write_and_reopen(&builder, &out_path, |out| {
            assert_eq!(
                all_rows(&out),
                vec![
                    (1, 10, Some("un".to_string())),
                    (2, 20, Some("deux".to_string())),
                    (4, 40, Some("quatre".to_string())),
                    (5, 50, Some("cinq".to_string())),
                    (6, 60, Some("six".to_string())),
                    (7, 70, Some("sept".to_string())),
                ]
            );
        });

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn update_with_longer_varlena_does_not_corrupt_following_rows() {
        // Vérifie explicitement le recalcul de shift : la ligne 1 est
        // remplacée par une valeur beaucoup plus longue, la ligne 2 (non
        // touchée, copiée en run) doit rester lisible correctement après.
        let old_path = tmp_path("shift_old.bin");
        let out_path = tmp_path("shift_out.bin");
        let old = build_store(
            &old_path,
            &[
                (1, 1, Some("court")),
                (2, 2, Some("valeur-de-la-ligne-2-inchangee")),
            ],
        );

        let longer = "x".repeat(500);
        let delta = vec![(TestRecord { id: 1, val: 999 }, Some(longer.clone()))];
        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count());
        merge_store(&old, &delta, &[], &mut builder);

        write_and_reopen(&builder, &out_path, |out| {
            assert_eq!(
                all_rows(&out),
                vec![
                    (1, 999, Some(longer)),
                    (2, 2, Some("valeur-de-la-ligne-2-inchangee".to_string())),
                ]
            );
        });

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn deletion_removes_row_and_preserves_others() {
        let old_path = tmp_path("delete_old.bin");
        let out_path = tmp_path("delete_out.bin");
        let old = build_store(
            &old_path,
            &[(1, 1, Some("a")), (2, 2, Some("b")), (3, 3, Some("c"))],
        );

        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count());
        let report = merge_store(&old, &[], &[2], &mut builder);

        assert_eq!(report.rows_deleted, 1);
        assert_eq!(builder.row_count(), 2);

        write_and_reopen(&builder, &out_path, |out| {
            assert_eq!(
                all_rows(&out),
                vec![(1, 1, Some("a".to_string())), (3, 3, Some("c".to_string()))]
            );
        });

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    #[test]
    fn mixed_insert_update_delete_untouched() {
        let old_path = tmp_path("mixed_old.bin");
        let out_path = tmp_path("mixed_out.bin");
        // old : 1,2,3,4,5 — 2 sera supprimé, 4 sera mis à jour, 3 et 5 intacts.
        let old = build_store(
            &old_path,
            &[
                (1, 1, Some("un")),
                (2, 2, Some("deux")),
                (3, 3, Some("trois")),
                (4, 4, Some("quatre")),
                (5, 5, Some("cinq")),
            ],
        );

        let delta = vec![
            (
                TestRecord { id: 4, val: 400 },
                Some("quatre-modifie".to_string()),
            ),
            (TestRecord { id: 6, val: 6 }, Some("six-insere".to_string())),
        ];
        let deleted = vec![2i64];

        let mut builder = PackfileBuilder::<TestProj>::new(old.row_count() + 1);
        let report = merge_store(&old, &delta, &deleted, &mut builder);

        assert_eq!(report.rows_deleted, 1);
        assert_eq!(report.rows_updated, 1);
        assert_eq!(report.rows_inserted_from_delta, 1);
        assert_eq!(report.rows_copied_from_old, 3); // 1, 3, 5

        write_and_reopen(&builder, &out_path, |out| {
            assert_eq!(
                all_rows(&out),
                vec![
                    (1, 1, Some("un".to_string())),
                    (3, 3, Some("trois".to_string())),
                    (4, 400, Some("quatre-modifie".to_string())),
                    (5, 5, Some("cinq".to_string())),
                    (6, 6, Some("six-insere".to_string())),
                ]
            );
        });

        let _ = std::fs::remove_file(&old_path);
        let _ = std::fs::remove_file(&out_path);
    }

    fn write_and_reopen(
        builder: &PackfileBuilder<TestProj>,
        path: &std::path::Path,
        check: impl FnOnce(PackfileReader<TestProj>),
    ) {
        let file = std::fs::File::create(path).unwrap();
        let mut w = std::io::BufWriter::new(file);
        builder.write(&mut w).unwrap();
        drop(w);
        let reader = PackfileReader::<TestProj>::open(path).unwrap();
        check(reader);
    }
}
