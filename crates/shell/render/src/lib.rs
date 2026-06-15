// =============================================================================
// crates/shell/render/src/batch_renderer.rs
// =============================================================================

pub mod dispatcher;
pub mod batch_renderer;
pub mod packfile_builder;
pub mod dumper;

// Ré-export pour la façade
pub use dispatcher::{Dispatcher, DispatcherConfig};
pub use batch_renderer::{BatchRenderer, PackfileEntry};
pub use packfile_builder::PackfileBuilder;
