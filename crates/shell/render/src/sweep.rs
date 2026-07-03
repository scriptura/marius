//! Phase 4.1 — Moteur algorithmique en mémoire (pattern Sans-I/O).
//!
//! Aucune dépendance mmap/fichier/runtime async ici. Tout opère sur des
//! slices et `Vec` fournis par l'appelant. Les phases 4.2/4.3 consomment
//! `merge_sweep` comme boîte noire pure.

// Type physique importé depuis la source de vérité du format disque —
// 24 octets (id: i64, offset: u64, len: u32, _pad: [u8;4]), Pod/Zeroable.
// Plus de définition locale : une seule forme physique, pas deux structures
// à synchroniser manuellement (cf. handoff Phase 4.2, point 4).
use crate::pack_html_format::PackfileEntry;

#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct DeltaEntry {
    pub entity_id: i64,
    pub offset: u32,
    pub length: u32,
}

pub struct DeltaBatch {
    pub entries: Vec<DeltaEntry>,
    pub payload: Vec<u8>,
}

#[derive(Default)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct MergeReport {
    pub bytes_written: u64,
    pub entries_written: u32,
    pub deletes_applied: u32,
    pub inserts_applied: u32,
    pub updates_applied: u32,
    pub runs_count: u32,
    pub bytes_copied_from_old: u64,
    pub bytes_inserted_from_delta: u64,
}

/// Zéro-allocation dans la boucle interne. `out_index` pré-réservé par
/// l'appelant via `with_capacity`. `out_blob` pré-dimensionné par
/// l'appelant (borne supérieure).
///
/// Invariants préservés par construction (pas vérifiés a posteriori) :
/// - `out_pos` est threadé de manière strictement séquentielle à travers
///   `flush_run` et `copy_insert` (chaque appel reçoit le curseur courant,
///   renvoie `curseur + octets_écrits`). L'offset attribué à une entrée
///   est toujours la valeur de `out_pos` juste avant son écriture physique
///   → continuité automatique : `offset[k+1] == offset[k] + length[k]`.
/// - L'ordre d'écriture dans `out_index` suit l'ordre du sweep (`entity_id`
///   croissant), donc ordre physique du blob == ordre logique de l'index.
pub fn merge_sweep(
    old_blob: &[u8],
    old_index: &[PackfileEntry],
    delta: &DeltaBatch,
    out_blob: &mut [u8],
    out_index: &mut Vec<PackfileEntry>,
) -> MergeReport {
    // Contrats d'entrée C1/C2 — coût nul en release (corps non évalué hors
    // debug_assertions), vital en débogage pour quiconque branche un
    // producteur de old_index/delta non conforme.
    debug_assert!(
        old_index.windows(2).all(|w| w[0].id < w[1].id),
        "C2 violé : old_index n'est pas strictement trié par id"
    );
    debug_assert!(
        delta
            .entries
            .windows(2)
            .all(|w| w[0].entity_id < w[1].entity_id),
        "C1 violé : delta.entries n'est pas strictement trié par entity_id"
    );

    let mut report = MergeReport::default();

    let old_len = old_index.len();
    let delta_len = delta.entries.len();

    let mut i = 0usize; // curseur old_index
    let mut j = 0usize; // curseur delta.entries
    let mut run_start = 0usize; // début de la run ouverte (index dans old_index)
    let mut out_pos = 0usize; // curseur d'écriture dans out_blob

    loop {
        let old_avail = i < old_len;
        let delta_avail = j < delta_len;

        if !old_avail && !delta_avail {
            out_pos = flush_run(
                old_blob,
                old_index,
                run_start,
                i,
                out_blob,
                out_pos,
                out_index,
                &mut report,
            );
            break;
        }

        // Flux épuisé == traité comme infini (ligne "Drainage" de la table).
        let old_lt_delta = match (old_avail, delta_avail) {
            (true, true) => old_index[i].id < delta.entries[j].entity_id,
            (true, false) => true, // delta infini -> old reste toujours "plus petit" -> extension de run
            (false, _) => false,   // old infini -> jamais "plus petit"
        };

        if old_lt_delta {
            i += 1; // extension de la run, pas de flush
            continue;
        }

        // Condition de run rompue (ou old épuisé) -> flush obligatoire avant
        // de traiter l'entrée delta. Note : pour des delta consécutifs sans
        // ré-avance de `i` (Greater répétés), run_start == i déjà, donc cet
        // appel se résout en court-circuit (cf. flush_run) — coût d'une
        // comparaison, branche parfaitement prédictible.
        out_pos = flush_run(
            old_blob,
            old_index,
            run_start,
            i,
            out_blob,
            out_pos,
            out_index,
            &mut report,
        );

        // delta_avail garanti vrai ici : si delta_avail était faux, old_avail
        // serait vrai (sinon double-épuisement déjà intercepté plus haut),
        // et (true, false) => true aurait pris la branche old_lt_delta.
        let d = &delta.entries[j];
        let old_gt_delta = !old_avail || old_index[i].id > d.entity_id;

        if old_gt_delta {
            if d.length == 0 {
                // DELETE sur entité absente de old_index : no-op pur.
            } else {
                out_pos = emit_fragment(delta, d, out_blob, out_pos, out_index);
                report.inserts_applied += 1;
                report.bytes_inserted_from_delta += d.length as u64;
            }
            j += 1;
        } else {
            // old_index[i].id == d.entity_id
            if d.length == 0 {
                report.deletes_applied += 1; // DELETE effectif (entité vivante supprimée)
            } else {
                out_pos = emit_fragment(delta, d, out_blob, out_pos, out_index);
                report.updates_applied += 1;
                report.bytes_inserted_from_delta += d.length as u64;
            }
            i += 1;
            j += 1;
        }

        run_start = i;
    }

    report.bytes_written = out_pos as u64;
    report.entries_written = out_index.len() as u32;
    report
}

/// Flush d'une run `[run_start, run_end)` de `old_index` : un seul memcpy
/// sur le payload, un seul `extend_from_slice` sur l'index. Renvoie le
/// nouveau `out_pos` (pas de `&mut out_pos` — transitions d'état explicites).
#[allow(clippy::too_many_arguments)]
fn flush_run(
    old_blob: &[u8],
    old_index: &[PackfileEntry],
    run_start: usize,
    run_end: usize,
    out_blob: &mut [u8],
    out_pos: usize,
    out_index: &mut Vec<PackfileEntry>,
    report: &mut MergeReport,
) -> usize {
    if run_start == run_end {
        return out_pos; // run vide : rien à flusher
    }

    let first = &old_index[run_start];
    let last = &old_index[run_end - 1];

    // C2 (old_index trié) + invariant de continuité physique hérité de
    // old_index garantissent que [run_start..run_end) est une plage
    // contiguë dans old_blob : byte_end du dernier == offset+length du
    // dernier, sans trou avec les entrées intermédiaires. D'où le memcpy
    // unique, sans boucle entrée par entrée sur le payload.
    let byte_start = first.offset as usize;
    let byte_end = last.offset as usize + last.len as usize;
    let run_len = byte_end - byte_start;

    out_blob[out_pos..out_pos + run_len].copy_from_slice(&old_blob[byte_start..byte_end]);

    let out_idx_start = out_index.len();
    out_index.extend_from_slice(&old_index[run_start..run_end]);

    // delta_offset reste CONSTANT pour toute la run : la continuité
    // physique fait que la position relative de chaque entrée à l'intérieur
    // du bloc source (offset_old - byte_start) est strictement identique à
    // sa position relative dans le bloc destination (offset_new - out_pos).
    // Le décalage global ne dépend donc que du point d'ancrage de la run
    // (byte_start, out_pos), jamais de l'entrée individuelle — un seul
    // calcul, appliqué uniformément, au lieu d'un recalcul par entrée.
    // Peut être négatif : si des DELETE précédents ont retiré plus
    // d'octets que les INSERT n'en ont ajouté, out_pos < byte_start.
    let shift: i64 = out_pos as i64 - byte_start as i64;
    if shift != 0 {
        for entry in &mut out_index[out_idx_start..] {
            let new_offset = entry.offset as i64 + shift;
            debug_assert!(
                new_offset >= 0,
                "Violation d'invariant : offset négatif calculé"
            );
            entry.offset = new_offset as u64;
        }
    }

    report.runs_count += 1;
    report.bytes_copied_from_old += run_len as u64;

    out_pos + run_len
}

/// Émet un fragment physique depuis le delta (INSERT ou UPDATE — l'action
/// mémoire est identique : copie + nouvelle entrée d'index ; seul le
/// compteur de télémétrie incrémenté par l'appelant diffère selon le cas
/// logique).
fn emit_fragment(
    delta: &DeltaBatch,
    d: &DeltaEntry,
    out_blob: &mut [u8],
    out_pos: usize,
    out_index: &mut Vec<PackfileEntry>,
) -> usize {
    let src_start = d.offset as usize;
    let src_end = src_start + d.length as usize;

    out_blob[out_pos..out_pos + d.length as usize]
        .copy_from_slice(&delta.payload[src_start..src_end]);

    out_index.push(PackfileEntry {
        id: d.entity_id,
        offset: out_pos as u64,
        len: d.length,
        _pad: [0u8; 4],
    });

    out_pos + d.length as usize
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- Instrumentation allocateur (test uniquement) -----------------------
    // ATTENTION : #[global_allocator] est unique par binaire de test. Si ce
    // module est intégré à un crate qui possède déjà un allocateur global
    // de test ailleurs, déplacer ce bloc dans un module de test partagé
    // unique plutôt que de le dupliquer ici.
    struct CountingAllocator;
    static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOC_COUNT.fetch_add(1, Ordering::SeqCst);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    fn alloc_count() -> usize {
        ALLOC_COUNT.load(Ordering::SeqCst)
    }

    // --- Helpers de construction ---------------------------------------------
    fn pe(id: i64, offset: u64, length: u32) -> PackfileEntry {
        PackfileEntry {
            id,
            offset,
            len: length,
            _pad: [0u8; 4],
        }
    }
    fn de(id: i64, offset: u32, length: u32) -> DeltaEntry {
        DeltaEntry {
            entity_id: id,
            offset,
            length,
        }
    }

    // --- DELETE (avec shift négatif) -----------------------------------------
    #[test]
    fn delete_entity_and_negative_shift() {
        // old: id1[0..10]=0xAA, id2[10..20]=0xBB, id3[20..30]=0xCC
        let old_blob: Vec<u8> = [vec![0xAA; 10], vec![0xBB; 10], vec![0xCC; 10]].concat();
        let old_index = vec![pe(1, 0, 10), pe(2, 10, 10), pe(3, 20, 10)];
        let delta = DeltaBatch {
            entries: vec![de(2, 0, 0)],
            payload: vec![],
        };

        let mut out_blob = vec![0u8; 30];
        let mut out_index = Vec::with_capacity(3);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, vec![pe(1, 0, 10), pe(3, 10, 10)]); // shift = -10
        assert_eq!(
            &out_blob[..20],
            &[vec![0xAA; 10], vec![0xCC; 10]].concat()[..]
        );
        assert_eq!(report.deletes_applied, 1);
        assert_eq!(report.runs_count, 2);
        assert_eq!(report.bytes_written, 20);
        assert_eq!(report.bytes_copied_from_old, 20);
        assert_eq!(report.entries_written, 2);
    }

    // --- INSERT ---------------------------------------------------------------
    #[test]
    fn insert_new_entity() {
        let old_blob = vec![0xAA; 5];
        let old_index = vec![pe(1, 0, 5)];
        let delta = DeltaBatch {
            entries: vec![de(2, 0, 5)],
            payload: vec![0xDD; 5],
        };

        let mut out_blob = vec![0u8; 10];
        let mut out_index = Vec::with_capacity(2);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, vec![pe(1, 0, 5), pe(2, 5, 5)]);
        assert_eq!(
            &out_blob[..10],
            &[vec![0xAA; 5], vec![0xDD; 5]].concat()[..]
        );
        assert_eq!(report.inserts_applied, 1);
        assert_eq!(report.bytes_inserted_from_delta, 5);
        assert_eq!(report.runs_count, 1);
    }

    // --- UPDATE -----------------------------------------------------------
    #[test]
    fn update_existing_entity() {
        let old_blob: Vec<u8> = [vec![0xAA; 5], vec![0xBB; 5]].concat();
        let old_index = vec![pe(1, 0, 5), pe(2, 5, 5)];
        let delta = DeltaBatch {
            entries: vec![de(2, 0, 8)],
            payload: vec![0xEE; 8],
        };

        let mut out_blob = vec![0u8; 13];
        let mut out_index = Vec::with_capacity(2);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, vec![pe(1, 0, 5), pe(2, 5, 8)]);
        assert_eq!(
            &out_blob[..13],
            &[vec![0xAA; 5], vec![0xEE; 8]].concat()[..]
        );
        assert_eq!(report.updates_applied, 1);
        assert_eq!(report.bytes_inserted_from_delta, 8);
        assert_eq!(report.runs_count, 1);
    }

    // --- Run longue / delta vide / drainage gauche -----------------------
    #[test]
    fn long_run_with_empty_delta() {
        let old_blob: Vec<u8> = (0..20u8).collect(); // 5 entrées * 4 octets
        let old_index = vec![
            pe(1, 0, 4),
            pe(2, 4, 4),
            pe(3, 8, 4),
            pe(4, 12, 4),
            pe(5, 16, 4),
        ];
        let delta = DeltaBatch {
            entries: vec![],
            payload: vec![],
        };

        let mut out_blob = vec![0u8; 20];
        let mut out_index = Vec::with_capacity(5);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, old_index);
        assert_eq!(out_blob, old_blob);
        assert_eq!(report.runs_count, 1); // une seule run pour les 5 entrées
        assert_eq!(report.bytes_copied_from_old, 20);
        assert_eq!(report.entries_written, 5);
        assert_eq!(
            report.deletes_applied + report.inserts_applied + report.updates_applied,
            0
        );
    }

    // --- Drainage flux droit (old vide) -----------------------------------
    #[test]
    fn drain_right_stream_old_empty() {
        let old_blob: Vec<u8> = vec![];
        let old_index: Vec<PackfileEntry> = vec![];
        let delta = DeltaBatch {
            entries: vec![de(1, 0, 3), de(2, 3, 3), de(3, 6, 3)],
            payload: (0..9u8).collect(),
        };

        let mut out_blob = vec![0u8; 9];
        let mut out_index = Vec::with_capacity(3);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, vec![pe(1, 0, 3), pe(2, 3, 3), pe(3, 6, 3)]);
        assert_eq!(out_blob, delta.payload);
        assert_eq!(report.inserts_applied, 3);
        assert_eq!(report.runs_count, 0); // aucun flush réel : old toujours vide
    }

    // --- Bucket vide (old vide ET delta vide) -----------------------------
    #[test]
    fn both_empty() {
        let old_blob: Vec<u8> = vec![];
        let old_index: Vec<PackfileEntry> = vec![];
        let delta = DeltaBatch {
            entries: vec![],
            payload: vec![],
        };

        let mut out_blob: Vec<u8> = vec![];
        let mut out_index = Vec::new();
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(report, MergeReport::default());
        assert!(out_index.is_empty());
    }

    // --- Recouvrement total (chaque old touché par le delta) --------------
    #[test]
    fn full_overlap() {
        let old_blob: Vec<u8> = (0..12u8).collect(); // 3 entrées * 4 octets
        let old_index = vec![pe(1, 0, 4), pe(2, 4, 4), pe(3, 8, 4)];
        let delta = DeltaBatch {
            entries: vec![de(1, 0, 5), de(2, 0, 0), de(3, 5, 6)],
            payload: (100..111u8).collect(), // 11 octets : 5 + 6
        };

        let mut out_blob = vec![0u8; 11];
        let mut out_index = Vec::with_capacity(2);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        assert_eq!(out_index, vec![pe(1, 0, 5), pe(3, 5, 6)]);
        assert_eq!(out_blob, delta.payload); // rien copié depuis old
        assert_eq!(report.updates_applied, 2);
        assert_eq!(report.deletes_applied, 1);
        assert_eq!(report.runs_count, 0); // aucune extension de run possible : égalité à chaque pas
        assert_eq!(report.bytes_copied_from_old, 0);
    }

    // --- Entrelacement fragmenté : Run -Update -Run -Update -Run ----------
    // Cas réaliste : trois runs séparés par deux UPDATE isolés, avec
    // accumulation d'un shift POSITIF sur deux runs successives (chaque
    // UPDATE injecte plus d'octets qu'il n'en consomme), à distinguer du
    // shift négatif déjà couvert par `delete_entity_and_negative_shift`.
    #[test]
    fn interleaved_run_update_run_update_run() {
        let old_blob: Vec<u8> = (0..22u8).collect(); // 11 entrées * 2 octets
        let old_index = vec![
            pe(1, 0, 2),
            pe(2, 2, 2),
            pe(3, 4, 2),
            pe(4, 6, 2),
            pe(5, 8, 2),
            pe(6, 10, 2),
            pe(7, 12, 2),
            pe(8, 14, 2),
            pe(9, 16, 2),
            pe(10, 18, 2),
            pe(11, 20, 2),
        ];
        let delta = DeltaBatch {
            entries: vec![de(4, 0, 5), de(8, 5, 3)], // deux UPDATE isolés
            payload: [vec![0xF0; 5], vec![0xF1; 3]].concat(),
        };

        let mut out_blob = vec![0u8; 26];
        let mut out_index = Vec::with_capacity(11);
        let report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);

        let expected_index = vec![
            pe(1, 0, 2),
            pe(2, 2, 2),
            pe(3, 4, 2),
            pe(4, 6, 5), // run1 + update id4
            pe(5, 11, 2),
            pe(6, 13, 2),
            pe(7, 15, 2),
            pe(8, 17, 3), // run2 (shift +3) + update id8
            pe(9, 20, 2),
            pe(10, 22, 2),
            pe(11, 24, 2), // run3 (shift +4)
        ];
        assert_eq!(out_index, expected_index);

        let expected_blob: Vec<u8> = [
            &old_blob[0..6],
            &delta.payload[0..5],
            &old_blob[8..14],
            &delta.payload[5..8],
            &old_blob[16..22],
        ]
        .concat();
        assert_eq!(out_blob, expected_blob);

        assert_eq!(report.runs_count, 3);
        assert_eq!(report.updates_applied, 2);
        assert_eq!(report.deletes_applied, 0);
        assert_eq!(report.inserts_applied, 0);
        assert_eq!(report.bytes_written, 26);
        assert_eq!(report.entries_written, 11);
    }

    // --- Zéro-allocation dans la boucle interne ----------------------------
    // Remarque : la mesure repose sur un allocateur global instrumenté
    // (cf. plus haut). En exécution parallèle de la suite de tests,
    // d'autres threads peuvent allouer pendant la fenêtre de mesure et
    // produire un faux négatif. Exécuter cette suite avec
    // `cargo test -- --test-threads=1` pour une mesure fiable.
    #[test]
    fn zero_allocation_inside_merge_sweep() {
        let old_blob: Vec<u8> = [vec![0xAA; 10], vec![0xBB; 10], vec![0xCC; 10]].concat();
        let old_index = vec![pe(1, 0, 10), pe(2, 10, 10), pe(3, 20, 10)];
        let delta = DeltaBatch {
            entries: vec![de(2, 0, 0)],
            payload: vec![],
        };

        // Buffers pré-alloués AVANT la fenêtre de mesure.
        let mut out_blob = vec![0u8; 30];
        let mut out_index = Vec::with_capacity(3);

        let before = alloc_count();
        let _report = merge_sweep(&old_blob, &old_index, &delta, &mut out_blob, &mut out_index);
        let after = alloc_count();

        assert_eq!(
            before, after,
            "allocation détectée à l'intérieur de merge_sweep"
        );
    }
}
