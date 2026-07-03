// =============================================================================
// crates/shell/render/src/batch_renderer.rs
//
// Moteur d'exécution Packfile HTML — distinct du store.bin (PackfileBuilder/
// PackfileReader, marius_projection) qui porte les StorageRow brutes pour
// fetch_batch. Ce fichier produit l'artefact HTML servi en lecture par sendfile
// (ADR-006), pas les données sources.
//
// Invariants :
//   O(1) syscalls  : un seul open() par fichier — fd conservé, jamais réouvert
//                    par requête (côté lecture, voir specification-marius-render-shell.md).
//   Zéro-alloc     : buf.clear() entre records (capacity conservée).
//                    index pré-alloué à batch_len avant la boucle.
//   Index physique : Vec<PackfileEntry> corrélant chaque ID à (offset, len),
//                    bytemuck::Pod — castable directement depuis un mmap.
//
// Format on-disk complet : voir pack_html_format.rs (source de vérité unique —
// PackfileEntry, PackfileFooter, write_packfile_footer).
// =============================================================================

use std::io::{BufWriter, Write};
use std::marker::PhantomData;

use marius_projection::Projection;

use crate::pack_html_format::PackfileEntry;

// =============================================================================
// BatchRenderer<P>
// =============================================================================

/// Moteur de rendu batch : traite Vec<(Record, VarlenOwned)> en un passage
/// séquentiel, écrit dans un BufWriter unique, construit l'index physique.
///
/// Paramètre W: Write permet d'utiliser BufWriter<File> en production
/// et BufWriter<Vec<u8>> en test — sans changer la logique de rendu.
pub struct BatchRenderer<P: Projection> {
    /// Buffer de rendu réutilisé : alloué une fois, clear() entre records.
    buf: String,
    /// Index physique pré-alloué : push() sans réallocation dans la boucle.
    index: Vec<PackfileEntry>,
    /// Capacité cible du buffer (= {NAME}_TOTAL_CAP de Fragment-Forge).
    total_cap: usize,
    _proj: PhantomData<P>,
}

impl<P: Projection> BatchRenderer<P> {
    /// Alloue le buffer et pré-dimensionne l'index.
    ///
    /// `total_cap` : constante `{NAME}_TOTAL_CAP` produite par Fragment-Forge.
    ///               Doit correspondre exactement à la table traitée.
    /// `batch_len` : longueur du Vec retourné par fetch_batch —
    ///               garantit que Vec::push dans render_batch n'alloue pas.
    pub fn new(total_cap: usize, batch_len: usize) -> Self {
        Self {
            buf: String::with_capacity(total_cap),
            index: Vec::with_capacity(batch_len),
            total_cap,
            _proj: PhantomData,
        }
    }

    /// Traite un batch, écrit dans `writer`, remplit l'index physique.
    ///
    /// `offset_start` : offset courant dans le packfile avant ce batch.
    ///                  Permet de chaîner plusieurs batches dans le même fichier.
    ///
    /// Retourne l'offset final (= offset_start + Σ len des fragments écrits).
    ///
    /// # Invariant zéro-alloc
    ///
    ///   Valide si et seulement si :
    ///   1. total_cap == {NAME}_TOTAL_CAP (Fragment-Forge a calculé le pire cas).
    ///   2. batch_len == records.len() au moment de new().
    ///   3. P::render() n'alloue pas en interne (invariant Fragment-Forge).
    ///
    /// N'écrit jamais l'index ni le footer — voir `write_packfile_footer`,
    /// appelée une seule fois par l'orchestrateur après le dernier batch.
    pub fn render_batch<W: Write>(
        &mut self,
        records: &[(P::Record, P::VarlenOwned)],
        writer: &mut BufWriter<W>,
        offset_start: u64,
    ) -> std::io::Result<u64> {
        let mut offset = offset_start;

        for (record, varlena) in records {
            // Réinitialise len à 0 sans libérer la mémoire allouée.
            self.buf.clear();

            // Assertion debug : détecte une réallocation causée par TOTAL_CAP
            // sous-estimé. Silencieuse en release — zéro coût sur le hot path.
            debug_assert_eq!(
                self.buf.capacity(),
                self.total_cap,
                "BatchRenderer : réallocation détectée — TOTAL_CAP sous-estimé pour {}",
                std::any::type_name::<P>(),
            );

            P::render(record, varlena, &mut self.buf);

            let bytes = self.buf.as_bytes();
            let len = bytes.len() as u32;

            writer.write_all(bytes)?;

            // push() sans alloc : Vec::with_capacity(batch_len) garantit la capacité.
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

    /// Vide l'index et réinitialise l'offset sans désallouer les buffers.
    ///
    /// Permet de réutiliser le BatchRenderer pour un second batch sur la même
    /// table. ATTENTION : l'index interne est vidé — si plusieurs chunks
    /// composent un seul fichier final, l'appelant doit collecter `index()`
    /// (ou `into_index()` en clonant) AVANT chaque appel à `reset()`, et
    /// accumuler lui-même la liste complète à passer à
    /// `write_packfile_footer` une fois tous les chunks traités.
    pub fn reset(&mut self, next_batch_len: usize) {
        self.index.clear();
        if self.index.capacity() < next_batch_len {
            // Seul point d'allocation autorisé hors new() : capacité insuffisante.
            self.index.reserve(next_batch_len - self.index.capacity());
        }
    }

    /// Consomme le renderer, retourne l'index physique (du batch courant
    /// uniquement — voir avertissement sur `reset()`).
    pub fn into_index(self) -> Vec<PackfileEntry> {
        self.index
    }

    /// Référence à l'index courant sans consommer le renderer.
    pub fn index(&self) -> &[PackfileEntry] {
        &self.index
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::{PackfileFooter, write_packfile_footer};
    use std::io::BufWriter;
    use std::path::PathBuf;

    // ── Projection stub minimal ───────────────────────────────────────────────
    // Zéro dépendance DB. Simule une table à deux champs fixed-length.

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct StubRecord {
        id: i32,
        price: i64,
    }

    struct StubProjection;

    // TOTAL_CAP réel = taille du HTML généré par render() ci-dessous.
    // "<article id=\"0000000000\"><p>00000000000000000000</p></article>" = 64B.
    // On sur-alloue à 128B pour garantir no-realloc même sur les valeurs pires cas.
    const STUB_TOTAL_CAP: usize = 128;

    impl Projection for StubProjection {
        type Record = StubRecord;
        type VarlenOwned = ();

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = marius_projection::BatchResult<Self>> + Send
        {
            async { Ok(vec![]) }
        }

        fn render(record: &StubRecord, _varlena: &(), buf: &mut String) {
            buf.push_str("<article id=\"");
            use std::fmt::Write as _;
            write!(buf, "{}", record.id).unwrap();
            buf.push_str("\"><p>");
            write!(buf, "{}", record.price).unwrap();
            buf.push_str("</p></article>");
        }

        #[inline(always)]
        fn record_id(record: &StubRecord) -> i64 {
            record.id as i64
        }

        fn packfile_path() -> PathBuf {
            PathBuf::from("artifacts/stub_pack.bin")
        }

        fn store_path() -> ::std::path::PathBuf {
            ::std::path::PathBuf::from("stub_store.bin")
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_records(n: usize) -> Vec<(StubRecord, ())> {
        (0..n)
            .map(|i| {
                (
                    StubRecord {
                        id: i as i32,
                        price: i as i64 * 100,
                    },
                    (),
                )
            })
            .collect()
    }

    // ── Test 1 : index physique cohérent ─────────────────────────────────────

    #[test]
    fn index_offsets_are_contiguous() {
        let records = make_records(3);
        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, records.len());
        let mut sink = BufWriter::new(Vec::<u8>::new());

        let final_offset = renderer
            .render_batch(&records, &mut sink, 0)
            .expect("render_batch a échoué");

        let index = renderer.index();
        assert_eq!(index.len(), 3);

        let mut expected_offset: u64 = 0;
        for entry in index {
            assert_eq!(
                entry.offset, expected_offset,
                "offset incohérent pour id={}",
                entry.id
            );
            expected_offset += entry.len as u64;
        }
        assert_eq!(final_offset, expected_offset);
    }

    // ── Test 2 : contenu du packfile reconstituable depuis l'index ────────────

    #[test]
    fn packfile_content_matches_index() {
        let records = make_records(3);
        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, records.len());
        let mut sink = BufWriter::new(Vec::<u8>::new());

        renderer.render_batch(&records, &mut sink, 0).unwrap();
        let raw = sink.into_inner().unwrap();
        let index = renderer.index();

        for entry in index {
            let start = entry.offset as usize;
            let end = start + entry.len as usize;
            let fragment = std::str::from_utf8(&raw[start..end]).unwrap();

            assert!(
                fragment.starts_with("<article"),
                "fragment id={} ne commence pas par <article> : {fragment:?}",
                entry.id
            );
            assert!(
                fragment.ends_with("</article>"),
                "fragment id={} ne se termine pas par </article> : {fragment:?}",
                entry.id
            );
        }
    }

    // ── Test 3 : no-realloc — invariant capacité ──────────────────────────────

    #[test]
    fn buf_capacity_stable_across_batch() {
        let records: Vec<(StubRecord, ())> = vec![
            (
                StubRecord {
                    id: i32::MIN,
                    price: i64::MIN,
                },
                (),
            ),
            (
                StubRecord {
                    id: i32::MAX,
                    price: i64::MAX,
                },
                (),
            ),
            (StubRecord { id: 0, price: 0 }, ()),
        ];

        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, records.len());
        let initial_cap = renderer.buf.capacity();

        let mut sink = BufWriter::new(Vec::<u8>::new());
        renderer.render_batch(&records, &mut sink, 0).unwrap();

        assert_eq!(
            renderer.buf.capacity(),
            initial_cap,
            "REALLOC détecté : capacity avant={initial_cap}, après={}. \
             STUB_TOTAL_CAP={STUB_TOTAL_CAP} sous-estimé.",
            renderer.buf.capacity(),
        );
    }

    // ── Test 4 : reset() réutilise les buffers ────────────────────────────────

    #[test]
    fn reset_preserves_buf_capacity() {
        let records = make_records(4);
        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, records.len());
        let mut sink = BufWriter::new(Vec::<u8>::new());

        renderer.render_batch(&records, &mut sink, 0).unwrap();

        let cap_before_reset = renderer.buf.capacity();
        let idx_cap_before = renderer.index.capacity();

        renderer.reset(4);

        assert_eq!(renderer.buf.capacity(), cap_before_reset);
        assert_eq!(renderer.index.capacity(), idx_cap_before);
        assert_eq!(renderer.index.len(), 0, "index doit être vidé par reset()");
    }

    // ── Test 5 : chained batches — offsets contigus entre batches ────────────

    #[test]
    fn chained_batches_offsets_are_contiguous() {
        let batch1 = make_records(2);
        let batch2 = make_records(3);

        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, 3);
        let mut sink = BufWriter::new(Vec::<u8>::new());

        let offset_after_b1 = renderer.render_batch(&batch1, &mut sink, 0).unwrap();
        renderer.reset(3);
        let _offset_after_b2 = renderer
            .render_batch(&batch2, &mut sink, offset_after_b1)
            .unwrap();

        assert_eq!(
            renderer.index()[0].offset,
            offset_after_b1,
            "offset de début du batch2 incohérent avec la fin du batch1",
        );
    }

    // ── Test 6 : format on-disk complet — footer + index relisibles ──────────

    #[test]
    fn footer_and_index_roundtrip() {
        let records = make_records(3);
        let mut renderer = BatchRenderer::<StubProjection>::new(STUB_TOTAL_CAP, records.len());
        let mut sink = BufWriter::new(Vec::<u8>::new());

        // blob_len = valeur RETOURNÉE par render_batch, jamais interrogée sur
        // le writer : BufWriter tamponne en interne, sink.get_ref().len() peut
        // sous-compter tant qu'aucun flush (implicite ou explicite) n'a eu
        // lieu — source du second bug d'alignement, distinct du premier.
        let blob_len = renderer.render_batch(&records, &mut sink, 0).unwrap();
        let index = renderer.into_index();
        write_packfile_footer(&mut sink, blob_len, &index).unwrap();

        let raw = sink.into_inner().unwrap();

        // Footer = 32 derniers octets.
        let footer_start = raw.len() - std::mem::size_of::<PackfileFooter>();
        let footer: &PackfileFooter = bytemuck::from_bytes(&raw[footer_start..]);

        assert_eq!(&footer.magic, b"MARIUSPK");
        assert_eq!(footer.version, 1);
        assert_eq!(footer.entry_count, 3);
        assert_eq!(
            footer.index_len,
            3 * std::mem::size_of::<PackfileEntry>() as u64
        );

        // Index juste avant le footer.
        let index_start = footer_start - footer.index_len as usize;
        let read_index: &[PackfileEntry] = bytemuck::cast_slice(&raw[index_start..footer_start]);

        assert_eq!(read_index, index.as_slice());
    }
}
