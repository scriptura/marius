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

    for col in columns {
        let m = map_type(&col.sql_type);
        if !m.is_fixed { continue; }

        let expr = if col.is_notnull {
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

        writeln!(out, "            {}: {},", col.name, expr).unwrap();
    }

    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}
