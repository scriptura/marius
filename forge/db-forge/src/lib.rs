// =============================================================================
// marius-db-forge · lib.rs
//
// API publique du crate.
// Exposé comme build-dependency de crates/core/schema/build.rs.
//
// ─── Séparation des responsabilités ──────────────────────────────────────────
//
//   db-forge   : introspection pg_catalog + génération des types Rust.
//   fragment-forge : génération des corps render() + constantes capacité HTML.
//   build.rs   : orchestrateur mince, lecture DATABASE_URL, écriture disque.
//
// ─── Dépendance unidirectionnelle (INV-4) ────────────────────────────────────
//
//   db-forge → fragment-forge (VarlenField, FieldSpec, FieldKind, generate_render).
//   fragment-forge N'importe PAS db-forge.
//
// =============================================================================

pub mod codegen;
pub mod introspect;
pub mod mapping;
pub mod naming;
pub mod registry;
pub mod validate;

// ── Mapping de types ──────────────────────────────────────────────────────────
pub use mapping::{map_type, Column, PrimaryKey, TypeMapping};

// ── Nommage ───────────────────────────────────────────────────────────────────
pub use naming::{to_pascal, to_screaming};

// ── Introspection pg_catalog ──────────────────────────────────────────────────
pub use introspect::{fetch_columns, fetch_max_id, fetch_pk_column, fetch_varlena_cols};

// ── Registre (Phase 1 : + fetch_component_list) ───────────────────────────────
pub use crate::registry::{ComponentConfig, VarlenJoin, fetch_component_list};

// ── Validation layout (Phase 2) ───────────────────────────────────────────────
pub use validate::validate_layout;

// ── Génération des artefacts ──────────────────────────────────────────────────
pub use codegen::{
    write_collector,
    write_from_impl,
    write_projection_stub,
    write_row_struct,
    write_section_header,
    write_store_struct,
    write_varlen_owned_struct,
};
