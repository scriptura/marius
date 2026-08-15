// crates/forge/db-forge/src/codegen/from_impl.rs
//
//! # marius-db-forge - from_impl
//! Génération de From<{Name}Row> for {Name}StorageRow.

use std::fmt::Write as _;

use crate::mapping::{Column, map_type};
use crate::naming::to_pascal;

// ── Helper sentinel ───────────────────────────────────────────────────────────

/// Construit l'expression de conversion pour une colonne NULLABLE.
///
/// Substitue dans `from_expr` :
///   {field}    → `{prefix}{col.name}`
///   {sentinel} → `col.sentinel` si annoté, sinon `m.default_sentinel`
///
/// L'appelant applique ensuite le cast u8 si `m.row_type == "bool"`.
pub(crate) fn build_nullable_expr(col: &Column, prefix: &str) -> String {
    let m = map_type(&col.sql_type);
    let field = format!("{prefix}{}", col.name);
    let sentinel = col.sentinel.as_deref().unwrap_or(m.default_sentinel);

    m.from_expr
        .replace("{field}", &field)
        .replace("{sentinel}", sentinel)
}

// ── Génération ────────────────────────────────────────────────────────────────

/// Génère From<{Name}Row> for {Name}StorageRow.
///
/// Transfère uniquement les champs fixed-length.
/// NOT NULL  → valeur directe (timestamp_micros() pour les types chrono).
/// NULLABLE  → build_nullable_expr() : unwrap_or(sentinel) résolu par Phase 3.
///
/// Note : si la table a des champs varlena JOIN, From<Row> N'EST PAS appelé
/// dans fetch_batch Phase 2 (lecture mmap). Il reste généré pour cohérence et
/// sera actif quand marius-dump sera découplé de fetch_batch (dette Phase 2).
pub fn write_from_impl(out: &mut String, schema: &str, table: &str, columns: &[Column]) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out, "impl From<{name}Row> for {name}StorageRow {{").unwrap();
    writeln!(out, "    fn from(r: {name}Row) -> Self {{").unwrap();
    writeln!(out, "        Self {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align = 1usize;

    for col in columns {
        let m = map_type(&col.sql_type);
        if !m.is_fixed {
            continue;
        }

        layout_bytes += m.size_bytes;
        max_align = max_align.max(m.alignment);

        let mut expr = if col.is_notnull {
            // NOT NULL : expression directe, pas de sentinel.
            //
            // pg_lsn traité AVANT le match sur m.row_type : depuis le
            // correctif Phase 2 walsn (sqlx n'ayant aucun Decode natif pour
            // pg_lsn, cf. mapping.rs), row_type de pg_lsn vaut "i64" — au
            // même titre que bigint/int8. Impossible de les distinguer sur
            // row_type seul ; col.sql_type reste la seule clé fiable.
            // StorageRow attend u64 (store_type), Row porte i64 (déjà casté
            // en SQL par select_cast) — cast Rust explicite requis, jamais
            // un simple passthrough comme pour un bigint ordinaire.
            if col.sql_type == "pg_lsn" {
                format!("r.{} as u64", col.name)
            } else {
                match m.row_type {
                    "chrono::DateTime<chrono::Utc>" => {
                        format!("r.{}.timestamp_micros()", col.name)
                    }
                    "chrono::NaiveDateTime" => {
                        format!("r.{}.and_utc().timestamp_micros()", col.name)
                    }
                    "chrono::NaiveDate" => format!("r.{}.num_days_from_ce()", col.name),
                    _ => format!("r.{}", col.name),
                }
            }
        } else {
            // NULLABLE : sentinel résolu depuis col.sentinel ou default_sentinel.
            build_nullable_expr(col, "r.")
        };

        // bool → u8 : cast explicite (StorageRow est repr(C), bool n'est pas Pod).
        if m.row_type == "bool" {
            expr = format!("({expr}) as u8");
        }

        if col.name == expr {
            writeln!(out, "            {},", col.name).unwrap();
        } else {
            writeln!(out, "            {}: {},", col.name, expr).unwrap();
        }
    }

    // Tail padding calculé statiquement pour satisfaire bytemuck::Pod.
    let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);
    let tail_pad = padded_size - layout_bytes;
    if tail_pad > 0 {
        writeln!(out, "            _pad: [0u8; {tail_pad}],").unwrap();
    }

    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}
