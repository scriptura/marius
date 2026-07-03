// marius-projection
// Trait Projection — interface canonique entre l'Orchestrator et les
// implémentations générées par Bridge-Forge + Fragment-Forge.
//
// Ce crate est la frontière Core/Shell :
//   - Il référence sqlx::PgPool (Shell) pour fetch_batch
//   - Il porte PackfileStoreHeader + align8 : source de vérité unique du
//     protocole binaire (partagée par PackfileBuilder et PackfileReader).
//   - Phase 2 : PackfileReader exposé ici pour que marius_schema puisse
//     l'utiliser sans cycle de dépendance (marius_render → marius_schema).
//
// ─── ADR-003 : Dualité Record / VarlenOwned ───────────────────────────────────
//
//   Record      : struct #[repr(C)], fixed-length, layout miroir PostgreSQL.
//   VarlenOwned : struct possédée portant les données varlena (Option<String>).
//                 () pour les tables sans varlena.
//
// ─── Protocole binaire ────────────────────────────────────────────────────────
//
//   Défini ici (PackfileStoreHeader, align8) — importé par PackfileBuilder
//   (marius_render) et PackfileReader (ce crate). Toute modification du layout
//   se propage automatiquement aux deux côtés.

use std::path::PathBuf;

pub type BatchResult<P> =
    Result<Vec<(<P as Projection>::Record, <P as Projection>::VarlenOwned)>, sqlx::Error>;

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

    fn record_id(record: &Self::Record) -> i64;

    fn packfile_path() -> PathBuf;

    fn store_path() -> PathBuf;

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

        #[inline(always)]
        fn records(&self) -> &[P::Record] {
            let end = self.rows_offset + self.row_count * mem::size_of::<P::Record>();
            bytemuck::cast_slice(&self.mmap[self.rows_offset..end])
        }

        #[inline(always)]
        fn id_index(&self) -> &[i64] {
            let end = self.id_index_offset + self.row_count * mem::size_of::<i64>();
            bytemuck::cast_slice(&self.mmap[self.id_index_offset..end])
        }

        #[inline(always)]
        fn toc(&self) -> &[VarlenSlot] {
            let len = self.row_count * self.varlena_field_count * mem::size_of::<VarlenSlot>();
            bytemuck::cast_slice(&self.mmap[self.toc_offset..self.toc_offset + len])
        }

        #[inline(always)]
        fn heap(&self) -> &[u8] {
            &self.mmap[self.heap_offset..self.heap_offset + self.heap_len]
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
