// crates/forge/db-forge/src/codegen/row.rs

//! # marius-db-forge - row
//!
//! Génération de {Name}Row : struct de désérialisation sqlx.

use std::fmt::Write as _;

use crate::mapping::{Column, map_type};
use crate::naming::to_pascal;
use marius_fragment_forge::VarlenField;

/// Génère {Name}Row : struct sqlx::FromRow, non-repr(C).
///
/// Types natifs Rust + Option<T> pour nullable.
/// Champs varlena de la table principale : String ou Option<String>.
/// Champs varlena de la table jointe (LEFT JOIN) : toujours Option<String>.
pub fn write_row_struct(
    out: &mut String,
    schema: &str,
    table: &str,
    columns: &[Column],
    varlena: &[VarlenField],
) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(
        out,
        "/// Struct de désérialisation sqlx pour {schema}.{table}.\n\
         /// Types natifs Rust + Option<T> pour nullable. NON repr(C).\n\
         /// Varlena JOIN : Option<String> (allocation sqlx, durée éphémère).\n\
         /// Transformer en StorageRow (From impl) + VarlenOwned avant usage."
    )
    .unwrap();
    writeln!(out, "#[derive(Debug, sqlx::FromRow)]").unwrap();
    writeln!(out, "pub struct {name}Row {{").unwrap();

    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            if col.is_notnull {
                writeln!(out, "    pub {}: {},", col.name, m.row_type).unwrap();
            } else {
                writeln!(
                    out,
                    "    pub {}: Option<{}>,  // NULLABLE → sentinel dans StorageRow",
                    col.name, m.row_type
                )
                .unwrap();
            }
        } else if m.row_type.starts_with("/*") {
            // Phase 2 ou type inconnu : commentaire uniquement.
            writeln!(
                out,
                "    // EXCLU Phase 1 : {} ({}) — {}",
                col.name, col.sql_type, m.row_type
            )
            .unwrap();
        } else if col.is_notnull {
            writeln!(
                out,
                "    pub {}: {},  // varlena table principale",
                col.name, m.row_type
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "    pub {}: Option<{}>,  // varlena NULLABLE table principale",
                col.name, m.row_type
            )
            .unwrap();
        }
    }

    // Champs varlena de la table jointe : toujours Option<String> (LEFT JOIN possible NULL).
    for v in varlena {
        // max_len: Option<usize> depuis ADR-007 — None affiché explicitement
        // (TEXT non borné) plutôt que masqué. Cohérent avec varlen.rs.
        let bound_descr = match v.max_len {
            Some(n) => format!("VARCHAR({n})"),
            None => "TEXT non borné".to_string(),
        };
        writeln!(
            out,
            "    pub {}: Option<String>,  // varlena JOIN {} — → as_deref()",
            v.name, bound_descr
        )
        .unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
