// crates/forge/db-forge/src/codegen/storage.rs

//! # marius-db-forge - storage
//!
//! Génération de {Name}StorageRow : struct #[repr(C)] + static_assertions.

use std::fmt::Write as _;

use crate::mapping::{Column, map_type};
use crate::naming::to_pascal;

/// Génère {Name}StorageRow : struct de stockage en mémoire contiguë.
///
/// ─── Invariant repr(C) ───────────────────────────────────────────────────────
///
///   Chaque champ est placé à l'offset dicté par son alignement naturel.
///   L'ordre des champs suit attnum ASC (invariant de Symétrie Mécanique).
///
/// ─── Varlena exclues ─────────────────────────────────────────────────────────
///
///   String / &str = fat pointer 16B = brise la symétrie binaire avec le
///   heap tuple PostgreSQL. Portées par VarlenOwned.
///
/// ─── static_assertions ───────────────────────────────────────────────────────
///
///   size_of et align_of vérifiés à la compilation. Un ALTER TABLE non suivi
///   d'une reconstruction déclenche une erreur compilateur, pas une corruption.
pub fn write_store_struct(out: &mut String, schema: &str, table: &str, columns: &[Column]) {
    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(
        out,
        "/// Struct de stockage en mémoire contiguë pour {schema}.{table}.\n\
        /// #[repr(C)] : layout C déterministe des champs fixed-length.\n\
        ///\n\
        /// L'ordre des champs suit attnum ASC, conformément à l'ordre des attributs\n\
        /// PostgreSQL. Le layout de cette struct n'inclut ni le header du heap tuple,\n\
        /// ni le null bitmap PostgreSQL : StorageRow représente uniquement les valeurs\n\
        /// fixed-length extraites du tuple.\n\
         /// Champs fixed-length uniquement. Nullable → sentinel (0 ou -1 selon type).\n\
         /// Varlena exclues : portées par VarlenOwned.\n\
         ///\n\
         /// AVERTISSEMENT NULLABLE : sentinel domain-specific (Phase 3 : pg_description)."
    )
    .unwrap();
    writeln!(out, "#[repr(C)]").unwrap();
    // INTEGRATION PHASE 1.4 : Dérivation sécurisée via bytemuck
    writeln!(
        out,
        "#[derive(Debug, Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]"
    )
    .unwrap();
    writeln!(out, "pub struct {name}StorageRow {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align = 1usize;

    for col in columns {
        let m = map_type(&col.sql_type);

        if m.is_fixed {
            let null_marker = if col.is_notnull { "" } else { " [sentinel]" };
            let pad = if col.name.len() < 20 {
                " ".repeat(20 - col.name.len())
            } else {
                String::new()
            };

            let emit_type = if m.store_type == "bool" {
                "u8"
            } else {
                m.store_type
            };

            writeln!(
                out,
                "    pub {}: {},{}  // attnum={}, {}B{}",
                col.name, emit_type, pad, col.attnum, m.size_bytes, null_marker,
            )
            .unwrap();

            max_align = max_align.max(m.alignment);

            // Simule le placement d'un champ selon #[repr(C)].
            layout_bytes = layout_bytes.div_ceil(m.alignment) * m.alignment;
            layout_bytes += m.size_bytes;
        } else {
            writeln!(
                out,
                "    // VARLENA exclu : {} ({}) → VarlenOwned",
                col.name, col.sql_type
            )
            .unwrap();
        }
    }

    let padded_size = layout_bytes.div_ceil(max_align) * max_align;
    let tail_pad = padded_size - layout_bytes;
    if tail_pad > 0 {
        writeln!(
            out,
            "    pub _pad: [u8; {tail_pad}], // Tail padding explicite pour alignement Pod"
        )
        .unwrap();
    }

    writeln!(out, "}}").unwrap();

    // Commentaire de layout : visible dans generated_schema.rs pour diagnostic
    writeln!(out,
        "// Layout fixed-length : {layout_bytes}B données → {padded_size}B padded (align={max_align}B)"
    ).unwrap();
    writeln!(
        out,
        "// + {}B header heap PostgreSQL (MAXALIGN(23 + ceil({}/8)))",
        {
            let n = columns.len();
            (23 + n.div_ceil(8)).div_ceil(8) * 8
        },
        columns.len()
    )
    .unwrap();
    writeln!(out).unwrap();

    // static_assertions : symétrie binaire à la compilation
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::size_of::<{name}StorageRow>() == {padded_size},\n    \
         \"DB-Forge [{schema}.{table}]: size_of diverge du DDL — reconstruire après ALTER TABLE\",\n\
         );"
    ).unwrap();
    writeln!(
        out,
        "const _: () = assert!(\n    \
         std::mem::align_of::<{name}StorageRow>() == {max_align},\n    \
         \"DB-Forge [{schema}.{table}]: align_of diverge du DDL — vérifier les types colonnes\",\n\
         );"
    )
    .unwrap();
    writeln!(out).unwrap();
}
