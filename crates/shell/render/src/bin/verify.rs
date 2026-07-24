// marius-render · crates/shell/render/src/bin/marius_verify.rs

//! Outil de validation de cohérence binaire AOT (`marius-verify`).
//!
//! Exécuté hors ligne après la passe d'extraction (`marius-dump`). Valide la lisibilité,
//! la topologie mémoire et l'intégrité structurelle du fichier `store.bin`.
//!
//! ## Invariants & Topologie Physique
//!
//! - **Indépendance I/O :** Aucune connexion base de données (`DATABASE_URL`) requise. Lit exclusivement
//!   la projection binaire stockée sur disque.
//! - **Validation Stricte des Formats (`INV-R1` à `INV-R5`) :**
//!   - `INV-R1` : Signature magique (`b"MARIUSDB"`) et version de schéma (`1`).
//!   - `INV-R2` : Intégrité de la taille totale du fichier ($\text{offset\_heap} + \text{taille\_heap}$).
//!   - `INV-R3` : Alignement fixe du pas (*stride*) ($=\text{sizeof}(\text{ContentCoreStorageRow})$).
//!   - `INV-R4` : Monotonie stricte de l'index des identifiants (`id_index` trié `ASC`).
//!   - `INV-R5` : Étanchéité de la heap dynamique : chaque `VarlenSlot` est soit *sentinel*,
//!     soit contenu dans les bornes strictes de la section varlen.

use std::fs;
use std::mem;

use marius_projection::{PackfileStoreHeader, Projection, VarlenSlot};
use marius_schema::{ContentCoreProjection, ContentCoreStorageRow};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store_path = ContentCoreProjection::store_path();

    eprintln!("[verify] lecture de {}", store_path.display());

    let bytes = fs::read(&store_path)?;

    // ── INV-R1 : Header ───────────────────────────────────────────────────────
    let header_size = mem::size_of::<PackfileStoreHeader>();
    if bytes.len() < header_size {
        return Err(format!(
            "fichier trop court : {}B < {}B (header)",
            bytes.len(),
            header_size
        )
        .into());
    }

    let header: &PackfileStoreHeader = bytemuck::from_bytes(&bytes[..header_size]);

    if &header.magic != b"MARIUSDB" {
        return Err(format!("magic invalide : {:?}", header.magic).into());
    }
    if header.version != 1 {
        return Err(format!("version inattendue : {}", header.version).into());
    }

    eprintln!(
        "[verify] header OK — version={} stride={}B row_count={} varlena_fields={}",
        header.version, header.stride, header.row_count, header.varlena_field_count
    );

    // ── INV-R3 : stride cohérent avec le type généré ─────────────────────────
    let expected_stride = mem::size_of::<ContentCoreStorageRow>() as u32;
    if header.stride != expected_stride {
        return Err(format!(
            "stride incohérent : header={} != sizeof(ContentCoreStorageRow)={}",
            header.stride, expected_stride
        )
        .into());
    }
    eprintln!(
        "[verify] stride={} — conforme à sizeof(ContentCoreStorageRow)",
        expected_stride
    );

    // ── INV-R2 : taille fichier ───────────────────────────────────────────────
    let expected_len = (header.varlena_heap_section + header.varlena_heap_len) as usize;
    if bytes.len() != expected_len {
        return Err(format!(
            "taille fichier incohérente : {}B sur disque, {}B attendus d'après le header",
            bytes.len(),
            expected_len
        )
        .into());
    }
    eprintln!("[verify] taille fichier OK — {}B", bytes.len());

    // ── Extraction des sections ───────────────────────────────────────────────
    let row_count = header.row_count as usize;
    let stride = header.stride as usize;
    let vf = header.varlena_field_count as usize;

    let rows_section = header_size;
    let id_index_section = header.id_index_section as usize;
    let varlena_toc_section = header.varlena_toc_section as usize;
    let varlena_heap_section = header.varlena_heap_section as usize;
    let varlena_heap_len = header.varlena_heap_len as usize;

    // Slice StorageRow — cast_slice sans unsafe grâce à bytemuck::Pod
    let rows_bytes = &bytes[rows_section..rows_section + row_count * stride];
    let rows: &[ContentCoreStorageRow] = bytemuck::cast_slice(rows_bytes);

    // Slice ID Index
    let id_bytes = &bytes[id_index_section..id_index_section + row_count * 8];
    let ids: &[i64] = bytemuck::cast_slice(id_bytes);

    // Slice Varlena TOC
    let toc_entry_size = mem::size_of::<VarlenSlot>(); // 8B
    let toc_bytes =
        &bytes[varlena_toc_section..varlena_toc_section + row_count * vf * toc_entry_size];
    let toc: &[VarlenSlot] = bytemuck::cast_slice(toc_bytes);

    // Slice Varlena Heap
    let heap = &bytes[varlena_heap_section..varlena_heap_section + varlena_heap_len];

    eprintln!(
        "[verify] sections — rows@{} id_index@{} varlena_toc@{} varlena_heap@{} ({}B)",
        rows_section, id_index_section, varlena_toc_section, varlena_heap_section, varlena_heap_len
    );

    // ── INV-R4 : ID Index trié ASC ────────────────────────────────────────────
    for i in 1..ids.len() {
        if ids[i] <= ids[i - 1] {
            return Err(format!(
                "id_index non trié à l'indice {}: ids[{}]={} >= ids[{}]={}",
                i,
                i - 1,
                ids[i - 1],
                i,
                ids[i]
            )
            .into());
        }
    }
    eprintln!("[verify] id_index trié ASC — {} entrées", ids.len());

    // ── INV-R5 : VarlenSlots dans les bornes ─────────────────────────────────
    let mut null_slots = 0usize;
    let mut valid_slots = 0usize;

    for (slot_idx, slot) in toc.iter().enumerate() {
        if slot.offset == u32::MAX && slot.len == 0 {
            // Sentinel NULL — valide par convention
            null_slots += 1;
            continue;
        }
        let start = slot.offset as usize;
        let end = start + slot.len as usize;
        if end > heap.len() {
            let record_idx = slot_idx / vf.max(1);
            let field_idx = slot_idx % vf.max(1);
            return Err(format!(
                "VarlenSlot hors bornes : record={} field={} offset={}+len={} > heap_len={}",
                record_idx,
                field_idx,
                slot.offset,
                slot.len,
                heap.len()
            )
            .into());
        }
        valid_slots += 1;
    }
    eprintln!(
        "[verify] varlena_toc OK — {} slots valides, {} nulls (sentinel)",
        valid_slots, null_slots
    );

    // ── Affichage des N premiers enregistrements ──────────────────────────────
    let display_count = row_count.min(5);
    eprintln!(
        "[verify] premiers enregistrements ({}/{}) :",
        display_count, row_count
    );

    for i in 0..display_count {
        let row = &rows[i];
        let pk = ids[i];

        // Reconstruction varlena depuis TOC + Heap pour vérification end-to-end
        let toc_base = i * vf;
        let varlena_strings: Vec<Option<&str>> = (0..vf)
            .map(|f| {
                let slot = &toc[toc_base + f];
                if slot.offset == u32::MAX {
                    None
                } else {
                    let s = slot.offset as usize;
                    let e = s + slot.len as usize;
                    std::str::from_utf8(&heap[s..e]).ok()
                }
            })
            .collect();

        eprintln!(
            "  [{}] pk={} document_id={} status={} varlena={:?}",
            i, pk, row.document_id, row.status, varlena_strings
        );
    }

    eprintln!(
        "[verify] ✓ store.bin valide — {} enregistrements, {} champs varlena/enreg.",
        row_count, vf
    );

    Ok(())
}
