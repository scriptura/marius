//! # marius-db-forge · `crates/forge/db-forge/src/lib.rs`
//!
//! API publique du crate.  
//! Exposé comme build-dependency de `crates/core/schema/build.rs`.
//!
//! ## Séparation des responsabilités
//!
//! - `db-forge` : Introspection `pg_catalog` + génération des types Rust.
//! - `fragment-forge` : Génération des corps `render()` + constantes capacité HTML.
//! - `build.rs` : Orchestrateur mince, lecture `DATABASE_URL`, écriture disque.
//!
//! ## Dépendance unidirectionnelle (INV-4)
//!
//! - `db-forge` → `fragment-forge` (`VarlenField`, `FieldSpec`, `FieldKind`, `generate_render`).
//! - `fragment-forge` n'importe pas `db-forge`.

pub mod codegen;
pub mod introspect;
pub mod mapping;
pub mod naming;
pub mod registry;
pub mod validate;

// ── Mapping de types ──────────────────────────────────────────────────────────
pub use mapping::{Column, PrimaryKey, TypeMapping, map_type};

// ── Nommage ───────────────────────────────────────────────────────────────────
pub use naming::{to_pascal, to_screaming};

// ── Introspection pg_catalog ──────────────────────────────────────────────────
pub use introspect::{fetch_columns, fetch_max_id, fetch_pk_column, fetch_varlena_cols};

// ── Registre (Phase 1 : + fetch_component_list) ───────────────────────────────
pub use crate::registry::{ComponentConfig, VarlenJoin, fetch_component_list};

// ── Validation layout (Phase 2) + collision de nom (multi-slot, Étape 3) ─────
pub use validate::{check_no_name_collision, validate_layout};

// ── Génération des artefacts ──────────────────────────────────────────────────
pub use codegen::{
    write_collector, write_from_impl, write_projection_stub, write_row_struct,
    write_section_header, write_store_struct, write_varlen_owned_struct,
};

// ── Résolution de schéma partagée (Voie B — templates .marius) ───────────────
//
// Utilisée par write_projection_stub (résolution de pk_field pour record_id())
// ET par build.rs (construction du SchemaIndex passé à resolve_and_measure /
// generate_aot_snippet). Définie une seule fois ici pour éviter la divergence
// entre les deux call sites — la même logique de mapping doit produire le
// même résultat partout.
use marius_fragment_forge::{FieldKind, FieldSpec};

/// Construit la liste des `FieldSpec` (champs fixed-length avec leur `FieldKind`)
/// depuis les colonnes introspectées.
///
/// Filtre : uniquement les colonnes `is_fixed` (exclut varlena et types Phase 2).
/// Filtre : uniquement les types reconnus par `FieldKind::from_sql_type` —
/// un type fixed sans FieldKind correspondant (cas théorique, tous les types
/// fixed de mapping.rs ont un FieldKind) serait silencieusement exclu.
pub fn build_field_specs(columns: &[Column]) -> Vec<FieldSpec> {
    columns
        .iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .filter_map(|c| {
            FieldKind::from_sql_type(&c.sql_type).map(|kind| FieldSpec {
                name: c.name.clone(),
                kind,
                attnum: c.attnum,
            })
        })
        .collect()
}
