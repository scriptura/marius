//! # marius-collector · `crates/core/collector/src/collector.rs`
//!
//! Crate Core — attitude `no_std`.  
//! Contient uniquement : `Collector<MAX, WORDS>` + `InsertResult`.
//!
//! Ce crate n'a aucune dépendance Tokio, SQLx, ou Rayon.  
//! Le `Dispatcher` (Tokio) vit dans `marius-render` (Shell).  
//! Le trait `Projection` (SQLx) vit dans `marius-projection` (frontière Core/Shell).

pub mod collector;
pub use collector::{Collector, InsertResult};
