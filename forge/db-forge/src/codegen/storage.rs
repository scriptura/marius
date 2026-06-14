// =============================================================================
// marius-db-forge · codegen/storage.rs
// Génération de {Name}StorageRow : struct #[repr(C)] + static_assertions.
// =============================================================================

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
pub fn write_store_struct(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
) {
    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(out,
        "/// Struct de stockage en mémoire contiguë pour {schema}.{table}.\n\
         /// #[repr(C)] : layout bit-à-bit aligné sur le heap tuple PostgreSQL.\n\
         /// Champs fixed-length uniquement. Nullable → sentinel (0 ou -1 selon type).\n\
         /// Varlena exclues : portées par VarlenOwned.\n\
         ///\n\
         /// AVERTISSEMENT NULLABLE : sentinel domain-specific (Phase 3 : pg_description)."
    ).unwrap();
    writeln!(out, "#[repr(C)]").unwrap();
    writeln!(out, "#[derive(Debug, Clone, Copy, Default)]").unwrap();
    writeln!(out, "pub struct {name}StorageRow {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align    = 1usize;

    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            let null_marker = if col.is_notnull { "" } else { " [sentinel]" };
            let pad = if col.name.len() < 20 {
                " ".repeat(20 - col.name.len())
            } else {
                String::new()
            };
            writeln!(out,
                "    pub {}: {},{}  // attnum={}, {}B{}",
                col.name, m.store_type, pad,
                col.attnum, m.size_bytes, null_marker,
            ).unwrap();
            layout_bytes += m.size_bytes;
            max_align     = max_align.max(m.alignment);
        } else {
            writeln!(out,
                "    // VARLENA exclu : {} ({}) → VarlenOwned",
                col.name, col.sql_type
            ).unwrap();
        }
    }
    writeln!(out, "}}").unwrap();

    // Taille padded repr(C) : arrondie au multiple supérieur de max_align.
    let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);

    // Commentaire de layout : visible dans generated_schema.rs pour diagnostic.
    writeln!(out,
        "// Layout fixed-length : {layout_bytes}B données → {padded_size}B padded (align={max_align}B)"
    ).unwrap();
    // Header PostgreSQL : MAXALIGN(23 + ceil(N_total/8)) — N_total = toutes colonnes.
    writeln!(out,
        "// + {}B header heap PostgreSQL (MAXALIGN(23 + ceil({}/8)))",
        { let n = columns.len(); (23 + n.div_ceil(8)).div_ceil(8) * 8 },
        columns.len()
    ).unwrap();
    writeln!(out).unwrap();

    // static_assertions : symétrie binaire à la compilation.
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::size_of::<{name}StorageRow>() == {padded_size},\n    \
         \"DB-Forge [{schema}.{table}]: size_of diverge du DDL — reconstruire après ALTER TABLE\",\n\
         );"
    ).unwrap();
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::align_of::<{name}StorageRow>() == {max_align},\n    \
         \"DB-Forge [{schema}.{table}]: align_of diverge du DDL — vérifier les types colonnes\",\n\
         );"
    ).unwrap();
    writeln!(out).unwrap();
}
