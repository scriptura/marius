//! # marius-projection — crates/core/projection/src/lib.rs
//!
//! Trait Projection — interface canonique entre l'Orchestrator et les
//! implémentations générées par Bridge-Forge + Fragment-Forge.
//!
//! Ce crate est la frontière Core/Shell :
//!   - Il référence sqlx::PgPool (Shell) pour fetch_batch
//!   - Il porte PackfileStoreHeader + align8 : source de vérité unique du
//!     protocole binaire (partagée par PackfileBuilder et PackfileReader).
//!   - Phase 2 : PackfileReader exposé ici pour que marius_schema puisse
//!     l'utiliser sans cycle de dépendance (marius_render → marius_schema).
//!
//! ─── ADR-003 : Dualité Record / VarlenOwned ───────────────────────────────────
//!
//!   Record      : struct #[repr(C)], fixed-length, layout miroir PostgreSQL.
//!   VarlenOwned : struct possédée portant les données varlena (Option<String>).
//!                 () pour les tables sans varlena.
//!
//! ─── Protocole binaire ────────────────────────────────────────────────────────
//!
//!   Défini ici (PackfileStoreHeader, align8) — importé par PackfileBuilder
//!   (marius_render) et PackfileReader (ce crate). Toute modification du layout
//!   se propage automatiquement aux deux côtés.

use std::path::PathBuf;

pub type BatchResult<P> =
    Result<Vec<(<P as Projection>::Record, <P as Projection>::VarlenOwned)>, sqlx::Error>;

/// Un fragment ordonné du résultat d'un rendu — CONTRAT-implementation-
/// projection-segmentee.md, Étape 2 (corrigé en session, 23/07/2026 : ce type
/// vit dans `marius_projection`, pas dans `marius_fragment_forge` — c'est ici
/// que le trait `Projection` le consomme, et `marius_render`/le crate généré
/// dépendent déjà de ce crate ; `fragment-forge` est un outil de build-time,
/// jamais une dépendance runtime).
///
/// Généralise au-delà du cas des varlena volumineux (ADR-010 §7) : `Segment`
/// ne code en dur aucune notion de « gros champ HTML » — un composant sans
/// champ `marius:large_content` produit toujours un unique `Segment::Buffered`
/// couvrant tout `buf` (implémentation par défaut de
/// `Projection::render_segments` ci-dessous), sans changement de comportement.
///
/// Pourquoi `Buffered { start, end }` et non `Buffered(&'a str)` (arbitré en
/// session, 23/07/2026) : `render_segments` continue d'écrire dans `buf`
/// après avoir logiquement « produit » un premier segment (ex. en-tête déjà
/// écrit, pied écrit plus tard dans le même appel). Un `&'a str` emprunté sur
/// `buf` et conservé dans la séquence de segments retournée maintiendrait un
/// prêt immuable vivant pendant que la fonction continue de faire
/// `buf.push_str(...)` pour la suite — prêt immuable et mutation simultanés
/// sur le même `buf`, rejeté par le borrow checker à raison : `String` peut
/// réallouer, ce qui invaliderait toute `&str` prise avant la dernière
/// écriture. Les indices diffèrent la vue en `&str` jusqu'à ce que `buf` soit
/// stable — après le retour de `render_segments`, quand l'appelant (qui
/// possède déjà `buf` dans son intégralité) peut re-trancher
/// `&buf[start..end]` sans risque. Ce n'est pas une fuite de représentation
/// interne : l'appelant ne gagne aucune information qu'il ne pourrait déjà
/// déduire, puisqu'il possède `buf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Plage déjà écrite dans le buffer partagé réutilisé (`buf[start..end]`).
    /// Jamais valide après que `buf` a été vidé/réutilisé pour l'enregistrement
    /// suivant — à consommer avant le prochain appel à `render_segments`.
    Buffered { start: usize, end: usize },
    /// Référence empruntée, zéro copie — jamais recopiée dans `buf`. Portée
    /// par la donnée déjà possédée du composant (`VarlenOwned`), jamais par
    /// `buf` lui-même.
    Borrowed(&'a str),
}

#[cfg(test)]
mod tests_segment {
    use super::Segment;

    #[test]
    fn buffered_variants_compare_by_value() {
        let a = Segment::Buffered { start: 0, end: 10 };
        let b = Segment::Buffered { start: 0, end: 10 };
        let c = Segment::Buffered { start: 0, end: 11 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn borrowed_variants_compare_by_value() {
        let a = Segment::Borrowed("abc");
        let b = Segment::Borrowed("abc");
        let c = Segment::Borrowed("abcd");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn buffered_and_borrowed_are_never_equal() {
        let a = Segment::Buffered { start: 0, end: 3 };
        let b = Segment::Borrowed("abc");
        assert_ne!(a, b);
    }
}

pub trait Projection: Sized + Send + Sync + 'static {
    type Record: Sized + Send + 'static;
    type VarlenOwned: Sized + Send + 'static;

    // ── Voie d'Extraction (cold path — marius-dump) ───────────────────────────
    //
    // Accès PostgreSQL direct via SQLx. Allocations autorisées.
    // Appelée exclusivement par dumper::dump_table pour peupler le store.bin.
    // Default : retourne Err — la Forge génère l'override pour chaque Projection.
    fn fetch_from_pg(
        _pool: &sqlx::PgPool,
        _ids: &[i64],
    ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
        std::future::ready(Err(sqlx::Error::Configuration(
            "[fetch_from_pg] override SQLx non généré — exécuter cargo build".into(),
        )))
    }

    // ── Voie d'Exécution (hot path — serveur) ────────────────────────────────
    //
    // Lecture mmap via OnceLock<PackfileReader>. Zéro allocation.
    // Fail-fast si store.bin absent : exécuter marius-dump d'abord.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids: &[i64],
    ) -> impl std::future::Future<Output = BatchResult<Self>> + Send;

    fn render(record: &Self::Record, varlena: &Self::VarlenOwned, buf: &mut String);

    /// Nombre maximal de segments produits par un enregistrement de ce
    /// composant — CONTRAT-implementation-projection-segmentee.md, Étape 3.
    /// Connu statiquement, généré par db-forge/fragment-forge selon le
    /// template compilé (propriété du template, exprimée ici via le
    /// mécanisme de surcharge du trait — `impl Projection for {Name}Projection`
    /// est lui-même entièrement généré). Défaut `1` : un composant sans champ
    /// `marius:large_content` produit toujours exactement un segment.
    /// Permet à `BatchRenderer` de pré-allouer son `Vec<Segment>` une seule
    /// fois, jamais de resize en boucle de rendu (même discipline que
    /// `buf`/`total_cap`, INV-5/INV-6 de `PackfileBuilder`).
    const MAX_SEGMENTS: usize = 1;

    /// Par défaut, délègue à `render()` — un seul segment `Buffered` couvrant
    /// tout `buf`. Composants sans champ `marius:large_content` : comportement
    /// inchangé, coût additionnel négligeable (un `push()` dans un `Vec`
    /// pré-alloué à `MAX_SEGMENTS`).
    ///
    /// Les composants générés avec un champ `marius:large_content` reçoivent
    /// une implémentation réelle multi-segments (générée par
    /// `fragment-forge`/`db-forge`, Étape 5 du Contrat) qui ne délègue jamais
    /// à cette valeur par défaut — le champ volumineux y devient un
    /// `Segment::Borrowed` autonome, jamais concaténé dans `buf`.
    ///
    /// **Contrat sur `buf` (précisé en session, 23/07/2026)** : `buf` arrive
    /// déjà vide — le nettoyage est la responsabilité exclusive de
    /// l'appelant (`BatchRenderer::render_batch`), jamais de cette méthode ni
    /// de `render()`, exactement comme aujourd'hui pour `render()` seul. Ce
    /// n'est pas cosmétique : une implémentation multi-segments doit pouvoir
    /// écrire l'en-tête dans `buf`, laisser `buf` intact pendant qu'un segment
    /// emprunté est produit, puis **continuer à écrire** le pied à la suite
    /// dans le même `buf` sans le vider entre-temps — sans quoi le premier
    /// `Segment::Buffered` référencerait des octets déjà écrasés. Un
    /// `buf.clear()` interne à cette méthode casserait ce cas pour toute
    /// implémentation réelle multi-segments.
    ///
    /// `render()` reste la seule méthode que `render_segments` appelle pour
    /// produire du contenu dans le cas par défaut — cette méthode ne connaît
    /// toujours que `&mut String`, jamais `Write`/socket/fichier (invariant
    /// préservé, cf. ADR-010 §3).
    fn render_segments<'a>(
        record: &Self::Record,
        varlena: &'a Self::VarlenOwned,
        buf: &mut String,
        segments: &mut Vec<Segment<'a>>,
    ) {
        Self::render(record, varlena, buf);
        segments.push(Segment::Buffered {
            start: 0,
            end: buf.len(),
        });
    }

    fn record_id(record: &Self::Record) -> i64;

    fn packfile_path() -> PathBuf;

    fn store_path() -> PathBuf;

    /// Accès générique au registre atomiquement remplaçable de cette
    /// Projection — nécessaire à tout code générique `<P: Projection>`
    /// (`ingest_and_swap`) qui doit appeler `.swap()` sans connaître la
    /// `static` propre à P, invisible depuis une fonction générique.
    /// `cold_start_store()` (généré, méthode inhérente hors trait) et cette
    /// méthode ciblent la même `static` — cf. codegen/projection.rs.
    fn store_registry() -> &'static StoreRegistry<Self>
    where
        Self: Sized,
        Self::Record: bytemuck::Pod;

    #[inline(always)]
    fn varlena_field_count() -> u16 {
        0
    }

    #[inline(always)]
    fn encode_varlena(
        _varlena: &Self::VarlenOwned,
        _heap: &mut Vec<u8>,
        _toc: &mut Vec<VarlenSlot>,
    ) {
    }
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct VarlenSlot {
    pub offset: u32,
    pub len: u32,
}

// =============================================================================
// Protocole binaire — source de vérité unique
//
// PackfileStoreHeader et align8 sont définis ici et importés par :
//   - marius_render::packfile_builder (écriture)
//   - marius_projection::packfile_reader (lecture)
//
// Toute modification de layout est répercutée sur les deux côtés sans risque
// de dérive silencieuse.
// =============================================================================

/// Header du store.bin — exactement 64B (une cache line).
/// Placé en tête du fichier, lu au montage par PackfileReader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileStoreHeader {
    pub magic: [u8; 8], // b"MARIUSDB"
    pub version: u32,   // = 1
    pub stride: u32,    // sizeof(P::Record)
    pub row_count: u64,
    pub varlena_field_count: u16,
    pub _pad: [u8; 6],
    pub id_index_section: u64,
    pub varlena_toc_section: u64,
    pub varlena_heap_section: u64,
    pub varlena_heap_len: u64,
}

const _: () = assert!(
    std::mem::size_of::<PackfileStoreHeader>() == 64,
    "PackfileStoreHeader doit être exactement 64B"
);

/// Arrondit `x` au prochain multiple de 8.
/// Utilisé par Builder (écriture des sections) et Reader (validation des offsets).
#[inline(always)]
pub const fn align8(x: u64) -> u64 {
    (x + 7) & !7
}

// =============================================================================
// PackfileReader — lecteur zero-copie via memmap2
// =============================================================================

mod store_registry;
pub use store_registry::StoreRegistry;

pub mod packfile_reader {
    use std::fs::File;
    use std::marker::PhantomData;
    use std::mem;
    use std::path::Path;

    use bytemuck::Pod;
    use memmap2::Mmap;

    use super::{PackfileStoreHeader, Projection, VarlenSlot};

    /// Vue sur les champs varlena d'un enregistrement.
    /// Zéro copie — lifetime lié au PackfileReader.
    pub struct VarlenRefs<'a> {
        toc: &'a [VarlenSlot],
        heap: &'a [u8],
    }

    impl<'a> VarlenRefs<'a> {
        /// Accès par index (0-based, ordre attnum).
        /// None si sentinel (offset == u32::MAX) ou index hors bornes.
        #[inline(always)]
        pub fn get(&self, field_idx: usize) -> Option<&'a str> {
            let slot = self.toc.get(field_idx)?;
            if slot.offset == u32::MAX {
                return None;
            }
            let start = slot.offset as usize;
            let end = start + slot.len as usize;
            std::str::from_utf8(self.heap.get(start..end)?).ok()
        }
    }

    /// Lecteur zero-copie d'un store.bin produit par PackfileBuilder<P>.
    ///
    /// Conçu pour être stocké dans un OnceLock<PackfileReader<P>> statique.
    /// memmap2::Mmap est Send + Sync.
    pub struct PackfileReader<P: Projection>
    where
        P::Record: Pod,
    {
        mmap: Mmap,
        row_count: usize,
        varlena_field_count: usize,
        rows_offset: usize,
        id_index_offset: usize,
        toc_offset: usize,
        heap_offset: usize,
        heap_len: usize,
        _proj: PhantomData<P>,
    }

    impl<P: Projection> PackfileReader<P>
    where
        P::Record: Pod,
    {
        /// Ouvre `path`, le mappe en lecture seule, valide le header.
        /// Appelle madvise(MADV_WILLNEED) pour pré-charger les pages en RAM
        /// dès le montage — élimine les page faults en hot path Tokio.
        ///
        /// # Safety (mmap)
        /// store.bin est produit atomiquement par marius-dump (INV-6).
        /// Il n'est pas modifié pendant l'exécution du serveur.
        pub fn open(path: &Path) -> std::io::Result<Self> {
            let file = File::open(path)?;
            let mmap = unsafe { Mmap::map(&file)? };

            // Pré-chargement des pages — hint non bloquant, sans privilège requis.
            // Élimine les page faults lors des premiers lookups en hot path.
            let _ = mmap.advise(memmap2::Advice::WillNeed);

            let header_size = mem::size_of::<PackfileStoreHeader>();

            if mmap.len() < header_size {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] fichier trop court : {}B < {}B",
                    mmap.len(),
                    header_size,
                )));
            }

            let header: &PackfileStoreHeader = bytemuck::from_bytes(&mmap[..header_size]);

            if &header.magic != b"MARIUSDB" {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] magic invalide : {:?}",
                    header.magic,
                )));
            }
            if header.version != 1 {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] version non supportée : {}",
                    header.version,
                )));
            }

            let expected_stride = mem::size_of::<P::Record>() as u32;
            if header.stride != expected_stride {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] stride incohérent : header={}B, sizeof(Record)={}B",
                    header.stride, expected_stride,
                )));
            }

            let expected_len = (header.varlena_heap_section + header.varlena_heap_len) as usize;
            if mmap.len() != expected_len {
                return Err(std::io::Error::other(format!(
                    "[PackfileReader] taille incohérente : {}B != {}B (header)",
                    mmap.len(),
                    expected_len,
                )));
            }

            Ok(Self {
                row_count: header.row_count as usize,
                varlena_field_count: header.varlena_field_count as usize,
                rows_offset: header_size,
                id_index_offset: header.id_index_section as usize,
                toc_offset: header.varlena_toc_section as usize,
                heap_offset: header.varlena_heap_section as usize,
                heap_len: header.varlena_heap_len as usize,
                mmap,
                _proj: PhantomData,
            })
        }

        /// Tranche brute des enregistrements, triée par position (pas par id).
        /// Exposé pour merge_store (marius-render) — memcpy de runs, zéro copie.
        #[inline(always)]
        pub fn records(&self) -> &[P::Record] {
            let end = self.rows_offset + self.row_count * mem::size_of::<P::Record>();
            bytemuck::cast_slice(&self.mmap[self.rows_offset..end])
        }

        /// Index des ids, trié croissant — invariant déjà exploité par `lookup`.
        #[inline(always)]
        pub fn id_index(&self) -> &[i64] {
            let end = self.id_index_offset + self.row_count * mem::size_of::<i64>();
            bytemuck::cast_slice(&self.mmap[self.id_index_offset..end])
        }

        /// TOC varlena brut, `row_count * varlena_field_count` entrées.
        #[inline(always)]
        pub fn toc(&self) -> &[VarlenSlot] {
            let len = self.row_count * self.varlena_field_count * mem::size_of::<VarlenSlot>();
            bytemuck::cast_slice(&self.mmap[self.toc_offset..self.toc_offset + len])
        }

        /// Heap varlena brut, tassé — les offsets du TOC y pointent directement.
        #[inline(always)]
        pub fn heap(&self) -> &[u8] {
            &self.mmap[self.heap_offset..self.heap_offset + self.heap_len]
        }

        /// Nombre de champs varlena par ligne — nécessaire à l'appelant pour
        /// calculer les bornes d'un slice `toc()` par plage de lignes.
        #[inline(always)]
        pub fn varlena_field_count(&self) -> usize {
            self.varlena_field_count
        }

        /// Recherche par ID — O(log N) binary search.
        /// Zéro allocation — toutes les références pointent dans le mmap.
        #[inline]
        pub fn lookup(&self, id: i64) -> Option<(&P::Record, VarlenRefs<'_>)> {
            let pos = self.id_index().binary_search(&id).ok()?;
            let record = &self.records()[pos];
            let toc_all = self.toc();
            let heap = self.heap();

            let toc_base = pos * self.varlena_field_count;
            let toc_slice = &toc_all[toc_base..toc_base + self.varlena_field_count];

            Some((
                record,
                VarlenRefs {
                    toc: toc_slice,
                    heap,
                },
            ))
        }

        #[inline(always)]
        pub fn row_count(&self) -> usize {
            self.row_count
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::VarlenSlot;

        #[test]
        fn sentinel_returns_none() {
            let slots = [VarlenSlot {
                offset: u32::MAX,
                len: 0,
            }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: &[],
            };
            assert_eq!(refs.get(0), None);
        }

        #[test]
        fn valid_slot_returns_str() {
            let slots = [VarlenSlot { offset: 0, len: 5 }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: b"hello",
            };
            assert_eq!(refs.get(0), Some("hello"));
        }

        #[test]
        fn out_of_bounds_field_returns_none() {
            let slots = [VarlenSlot { offset: 0, len: 2 }];
            let refs = VarlenRefs {
                toc: &slots,
                heap: b"hi",
            };
            assert_eq!(refs.get(1), None);
        }
    }
}
