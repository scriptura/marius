// =============================================================================
// marius-render · packfile_builder.rs
//
// Écrit les StorageRow + varlena d'une table dans un fichier binaire mmap-ready.
// Format : Header(64B) | StorageRow[] | ID Index | Varlena TOC | Varlena Heap
//
// INV-5 : pré-allocation à `capacity` dès new() — aucun resize en hot path.
// INV-6 : un seul open() + BufWriter<File> passé en paramètre à write().
// INV-7 : StorageRow est #[repr(C)] + bytemuck::Pod → cast_slice sans unsafe.
// =============================================================================

use std::io::{self, BufWriter, Seek, Write};
use std::marker::PhantomData;
use std::mem;

use bytemuck::Pod;

use marius_projection::{Projection, VarlenSlot};

// ── Header mmap-ready ─────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileStoreHeader {
    pub magic:                [u8; 8],  // b"MARIUSDB"
    pub version:              u32,      // = 1
    pub stride:               u32,      // sizeof(P::Record)
    pub row_count:            u64,
    pub varlena_field_count:  u16,
    pub _pad:                 [u8; 6],
    pub id_index_section:     u64,
    pub varlena_toc_section:  u64,
    pub varlena_heap_section: u64,
    pub varlena_heap_len:     u64,
}
// sizeof = 64B, align = 8. Exactement une cache line.

const _: () = assert!(
    mem::size_of::<PackfileStoreHeader>() == 64,
    "PackfileStoreHeader doit être exactement 64B"
);

// ── Builder ───────────────────────────────────────────────────────────────────

pub struct PackfileBuilder<P: Projection>
where
    P::Record: Pod,
{
    records:      Vec<P::Record>,
    id_index:     Vec<i64>,
    varlena_toc:  Vec<VarlenSlot>,
    varlena_heap: Vec<u8>,
    _proj:        PhantomData<P>,
}

impl<P: Projection> PackfileBuilder<P>
where
    P::Record: Pod,
{
    /// `capacity` : nombre d'enregistrements attendu (fetch_max_id() + marge).
    /// Pré-alloue toutes les structures — aucun resize si l'estimation est correcte.
    pub fn new(capacity: usize) -> Self {
        let vf = P::varlena_field_count() as usize;
        Self {
            records:      Vec::with_capacity(capacity),
            id_index:     Vec::with_capacity(capacity),
            varlena_toc:  Vec::with_capacity(capacity * vf),
            varlena_heap: Vec::with_capacity(capacity * 64), // heuristique : 64B/enreg.
            _proj:        PhantomData,
        }
    }

    /// Ingère un batch produit par `Projection::fetch_batch()`.
    /// Appelé N fois (une par batch) avant `write()`.
    pub fn push_batch(&mut self, batch: &[(P::Record, P::VarlenOwned)]) {
        for (record, owned) in batch {
            self.id_index.push(P::record_id(record));
            self.records.push(*record);
            P::encode_varlena(owned, &mut self.varlena_heap, &mut self.varlena_toc);
        }
        // Invariant post-push :
        debug_assert_eq!(self.records.len(), self.id_index.len());
        debug_assert_eq!(
            self.varlena_toc.len(),
            self.records.len() * P::varlena_field_count() as usize,
        );
    }

    /// Écrit le fichier binaire complet en une passe.
    /// Le writer doit être `Seek`-able (File). Pas de pipe.
    pub fn write<W: Write + Seek>(&self, writer: &mut BufWriter<W>) -> io::Result<()> {
        let row_count = self.records.len() as u64;
        let stride    = mem::size_of::<P::Record>() as u32;
        let vf        = P::varlena_field_count() as u64;

        // ── Calcul des offsets de section ────────────────────────────────────
        let rows_section         = 64u64;
        let id_index_section     = align8(rows_section + row_count * stride as u64);
        let varlena_toc_section  = align8(id_index_section + row_count * 8);
        let varlena_heap_section = align8(varlena_toc_section + row_count * vf * 8);
        let varlena_heap_len     = self.varlena_heap.len() as u64;

        // ── Header ───────────────────────────────────────────────────────────
        let header = PackfileStoreHeader {
            magic:                *b"MARIUSDB",
            version:              1,
            stride,
            row_count,
            varlena_field_count:  P::varlena_field_count(),
            _pad:                 [0u8; 6],
            id_index_section,
            varlena_toc_section,
            varlena_heap_section,
            varlena_heap_len,
        };
        writer.write_all(bytemuck::bytes_of(&header))?;

        // ── StorageRow array (zero-copy) ─────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.records))?;
        pad_to(writer, id_index_section - (rows_section + row_count * stride as u64))?;

        // ── ID Index ─────────────────────────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.id_index))?;
        pad_to(writer, varlena_toc_section - id_index_section - row_count * 8)?;

        // ── Varlena TOC ───────────────────────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.varlena_toc))?;
        pad_to(writer, varlena_heap_section - varlena_toc_section - row_count * vf * 8)?;

        // ── Varlena Heap ─────────────────────────────────────────────────────
        writer.write_all(&self.varlena_heap)?;

        Ok(())
    }

    #[inline(always)]
    pub fn row_count(&self) -> usize { self.records.len() }
}

// ── Helpers IO ────────────────────────────────────────────────────────────────

#[inline(always)]
const fn align8(x: u64) -> u64 { (x + 7) & !7 }

fn pad_to<W: Write>(writer: &mut W, n: u64) -> io::Result<()> {
    const ZERO: [u8; 8] = [0u8; 8];
    if n > 0 {
        writer.write_all(&ZERO[..n as usize])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_one_cache_line() {
        assert_eq!(std::mem::size_of::<PackfileStoreHeader>(), 64);
    }

    #[test]
    fn varlena_null_sentinel_is_u32_max() {
        let slot = VarlenSlot { offset: u32::MAX, len: 0 };
        assert!(slot.offset == u32::MAX && slot.len == 0);
    }
}
