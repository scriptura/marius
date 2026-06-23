// =============================================================================
// crates/shell/render/src/batch_renderer.rs
// =============================================================================

pub mod dispatcher;
pub mod batch_renderer;
pub mod packfile_builder;
pub mod pack_html_format;
pub mod pack_html_index;
pub mod dumper;

// Ré-export pour la façade
pub use dispatcher::{Dispatcher, DispatcherConfig};
pub use batch_renderer::BatchRenderer;
pub use pack_html_format::PackfileEntry;
pub use packfile_builder::PackfileBuilder;
