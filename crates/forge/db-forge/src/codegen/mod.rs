//! marius-db-forge · codegen/mod.rs
//! Génération des artefacts Rust par table.

pub mod collector;
pub mod from_impl;
pub mod projection;
pub mod row;
pub mod storage;
pub mod varlen;

pub use collector::write_collector;
pub use from_impl::write_from_impl;
pub use projection::write_projection_stub;
pub use row::write_row_struct;
pub use storage::write_store_struct;
pub use varlen::write_varlen_owned_struct;

use crate::mapping::PrimaryKey;
use std::fmt::Write as _;

/// En-tête de section dans `generated_schema.rs`.
pub fn write_section_header(out: &mut String, schema: &str, table: &str, pk: &PrimaryKey) {
    let pk_info = match pk {
        PrimaryKey::Single(col) => format!("PK={col}"),
        PrimaryKey::Composite => "PK composite — Collector N/A".to_string(),
    };
    writeln!(out, "// {}", "=".repeat(60)).unwrap();
    writeln!(out, "// {schema}.{table} · {pk_info}").unwrap();
    writeln!(out, "// {}\n", "=".repeat(60)).unwrap();
}
