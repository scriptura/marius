// =============================================================================
// marius-db-forge · codegen/from_impl.rs
// Génération de From<{Name}Row> for {Name}StorageRow.
// =============================================================================

use std::fmt::Write as _;

use crate::mapping::{Column, map_type};
use crate::naming::to_pascal;

/// Génère From<{Name}Row> for {Name}StorageRow.
///
/// Transfère uniquement les champs fixed-length.
/// NOT NULL  → valeur directe (timestamp_micros() pour les types chrono).
/// NULLABLE  → map_type().from_expr : unwrap_or(sentinel) ou map(…).unwrap_or(0).
///
/// Note : si la table a des champs varlena JOIN, From<Row> N'EST PAS appelé
/// dans fetch_batch — la déstructuration complète est émise inline dans
/// write_projection_stub pour éviter les partial moves (E0382).
pub fn write_from_impl(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out, "impl From<{name}Row> for {name}StorageRow {{").unwrap();
    writeln!(out, "    fn from(r: {name}Row) -> Self {{").unwrap();
    writeln!(out, "        Self {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align    = 1usize;

    for col in columns {
        let m = map_type(&col.sql_type);
        if !m.is_fixed { continue; }

        // Suivi du layout pour le calcul ultérieur du tail padding
        layout_bytes += m.size_bytes;
        max_align     = max_align.max(m.alignment);

        let mut expr = if col.is_notnull {
            match m.row_type {
                "chrono::DateTime<chrono::Utc>" => {
                    format!("r.{}.timestamp_micros()", col.name)
                }
                "chrono::NaiveDateTime" => {
                    format!("r.{}.and_utc().timestamp_micros()", col.name)
                }
                "chrono::NaiveDate" => {
                    format!("r.{}.num_days_from_ce()", col.name)
                }
                _ => format!("r.{}", col.name),
            }
        } else {
            m.from_expr.replace("{field}", &format!("r.{}", col.name))
        };

        // INTEGRATION PHASE 1.4 : Cast de l'expression vers u8 si le type d'origine SQL est un booléen
        if m.row_type == "bool" {
            expr = format!("({expr}) as u8");
        }

        writeln!(out, "            {}: {},", col.name, expr).unwrap();
    }

    // INTEGRATION PHASE 1.4 : Initialisation obligatoire du tail padding à zéro
    let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);
    let tail_pad = padded_size - layout_bytes;
    if tail_pad > 0 {
        writeln!(out, "            _pad: [0u8; {tail_pad}],").unwrap();
    }

    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}
