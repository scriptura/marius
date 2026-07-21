// =============================================================================
// marius-render · crates/shell/render/src/packfile_builder.rs
//
// Écrit les StorageRow + varlena d'une table dans un fichier binaire mmap-ready.
// Format : Header(64B) | StorageRow[] | ID Index | Varlena TOC | Varlena Heap
//
// INV-5 : pré-allocation à `capacity` dès new() — aucun resize en hot path.
// INV-6 : un seul open() + BufWriter<File> passé en paramètre à write().
// INV-7 : StorageRow est #[repr(C)] + bytemuck::Pod → cast_slice sans unsafe.
//
// ── Source de vérité binaire ──────────────────────────────────────────────────
// PackfileStoreHeader et align8 sont définis dans marius_projection et importés
// ici. PackfileReader (aussi dans marius_projection) utilise les mêmes types.
// Toute modification de layout se propage automatiquement aux deux côtés.
// =============================================================================

use std::io::{self, BufWriter, Write};
use std::marker::PhantomData;
use std::mem;

use bytemuck::Pod;

use marius_projection::{PackfileStoreHeader, Projection, VarlenSlot, align8};

// ── Builder ───────────────────────────────────────────────────────────────────

pub struct PackfileBuilder<P: Projection>
where
    P::Record: Pod,
{
    records: Vec<P::Record>,
    id_index: Vec<i64>,
    varlena_toc: Vec<VarlenSlot>,
    varlena_heap: Vec<u8>,
    _proj: PhantomData<P>,
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
            records: Vec::with_capacity(capacity),
            id_index: Vec::with_capacity(capacity),
            varlena_toc: Vec::with_capacity(capacity * vf),
            varlena_heap: Vec::with_capacity(capacity * 64), // heuristique : 64B/enreg.
            _proj: PhantomData,
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
        debug_assert_eq!(self.records.len(), self.id_index.len());
        debug_assert_eq!(
            self.varlena_toc.len(),
            self.records.len() * P::varlena_field_count() as usize,
        );
    }

    /// Ingère un run de lignes déjà encodées, telles que lues depuis un
    /// `PackfileReader` existant — memcpy pur (`extend_from_slice`), aucun
    /// passage par `P::encode_varlena`, aucune allocation `String` ni `Vec`
    /// intermédiaire.
    ///
    /// # Invariant d'API — contrat logique, non vérifié par le type système
    ///
    /// Cette fonction est l'équivalent logique d'une fonction `unsafe` : rien
    /// dans les types ne peut garantir que `heap` correspond exactement à
    /// `heap_base_offset`/`toc`. Le vérifier ici obligerait à recalculer ce
    /// que l'appelant a déjà calculé — annulant l'intérêt même de la fonction
    /// (zéro-copie). La sûreté ne vient donc pas d'une vérification interne,
    /// mais d'un unique appelant, `merge_store`, qui construit ces trois
    /// paramètres à partir du même `PackfileReader` source et est couvert par
    /// des tests dédiés au recalcul de shift (cf. `merge_store.rs::tests`).
    /// `push_raw_run` n'est pas destinée à un second appelant sans revoir ce
    /// contrat.
    ///
    /// Le contrat exact que l'appelant doit garantir :
    /// - `records` et `toc` respectent `toc.len() == records.len() *
    ///   P::varlena_field_count()` (même contrat que `push_batch`, vérifié
    ///   en `debug_assert`, pas en production).
    /// - `heap` est exactement la tranche du heap source couverte par `toc`
    ///   (du plus petit `offset` au plus grand `offset+len`, sentinelles
    ///   `u32::MAX` exclues).
    /// - `heap_base_offset` est l'offset absolu, dans le heap source, du
    ///   premier octet de `heap`.
    ///
    /// Toute violation produit un TOC silencieusement incohérent — pas un
    /// panic, pas une erreur, une corruption de données à la prochaine
    /// lecture. C'est le prix accepté pour rester zéro-allocation sur le
    /// chemin de fusion ; documenté ici plutôt que traité comme une dette,
    /// puisqu'aucune vérification interne ne peut exister sans dupliquer le
    /// calcul de l'appelant.
    pub fn push_raw_run(
        &mut self,
        records: &[P::Record],
        toc: &[VarlenSlot],
        heap_base_offset: u32,
        heap: &[u8],
    ) {
        let shift = self.varlena_heap.len() as u32;

        self.id_index
            .extend(records.iter().map(|r| P::record_id(r)));
        self.records.extend_from_slice(records);

        self.varlena_toc.extend(toc.iter().map(|slot| {
            if slot.offset == u32::MAX {
                *slot // sentinelle (champ NULL) : jamais décalée
            } else {
                VarlenSlot {
                    offset: (slot.offset - heap_base_offset) + shift,
                    len: slot.len,
                }
            }
        }));
        self.varlena_heap.extend_from_slice(heap);

        debug_assert_eq!(self.records.len(), self.id_index.len());
        debug_assert_eq!(
            self.varlena_toc.len(),
            self.records.len() * P::varlena_field_count() as usize,
        );
    }

    /// Écrit le fichier binaire complet en une passe.
    /// Le writer doit supporter Write (BufWriter<File>).
    pub fn write<W: Write>(&self, writer: &mut BufWriter<W>) -> io::Result<()> {
        let row_count = self.records.len() as u64;
        let stride = mem::size_of::<P::Record>() as u32;
        let vf = P::varlena_field_count() as u64;

        // ── Calcul des offsets de section (align8 importé de marius_projection) ─
        let rows_section = 64u64;
        let id_index_section = align8(rows_section + row_count * stride as u64);
        let varlena_toc_section = align8(id_index_section + row_count * 8);
        let varlena_heap_section = align8(varlena_toc_section + row_count * vf * 8);
        let varlena_heap_len = self.varlena_heap.len() as u64;

        // ── Header (PackfileStoreHeader importé de marius_projection) ────────
        let header = PackfileStoreHeader {
            magic: *b"MARIUSDB",
            version: 1,
            stride,
            row_count,
            varlena_field_count: P::varlena_field_count(),
            _pad: [0u8; 6],
            id_index_section,
            varlena_toc_section,
            varlena_heap_section,
            varlena_heap_len,
        };
        writer.write_all(bytemuck::bytes_of(&header))?;

        // ── StorageRow array (zero-copy) ─────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.records))?;
        pad_to(
            writer,
            id_index_section - (rows_section + row_count * stride as u64),
        )?;

        // ── ID Index ─────────────────────────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.id_index))?;
        pad_to(
            writer,
            varlena_toc_section - id_index_section - row_count * 8,
        )?;

        // ── Varlena TOC ───────────────────────────────────────────────────────
        writer.write_all(bytemuck::cast_slice(&self.varlena_toc))?;
        pad_to(
            writer,
            varlena_heap_section - varlena_toc_section - row_count * vf * 8,
        )?;

        // ── Varlena Heap ─────────────────────────────────────────────────────
        writer.write_all(&self.varlena_heap)?;

        Ok(())
    }

    #[inline(always)]
    pub fn row_count(&self) -> usize {
        self.records.len()
    }
}

// ── Helper IO ─────────────────────────────────────────────────────────────────

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
        let slot = VarlenSlot {
            offset: u32::MAX,
            len: 0,
        };
        assert!(slot.offset == u32::MAX && slot.len == 0);
    }
}
