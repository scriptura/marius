//! # Marius — Crate Façade (crates/forge/fragment-forge/src/lib.rs)
//!
//! Réexporte les types publics fondamentaux depuis leurs crates canoniques.
//!
//! ## Topologie Phase 1
//!
//! - `marius-collector` : `Collector<MAX, WORDS>` + `InsertResult` *(Core pur, zéro Tokio)*
//! - `marius-projection` : trait `Projection` *(frontière Core/Shell)*
//! - `marius-render` : `Dispatcher` + `DispatcherConfig` *(Shell, Tokio)*

pub use marius_collector::{Collector, InsertResult};
pub use marius_projection::Projection;
pub use marius_render::{Dispatcher, DispatcherConfig};
