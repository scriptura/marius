// =============================================================================
// crates/shell/render/src/lib.rs
//
// Correction d'en-tête : le commentaire pointait vers batch_renderer.rs par
// copier-coller — ce fichier est lib.rs, la façade du crate.
// =============================================================================

pub mod dispatcher;
pub mod batch_renderer;
pub mod packfile_builder;
pub mod pack_html_format;
pub mod pack_html_index;
pub mod registry;
pub mod dumper;

// Ré-export pour la façade
pub use dispatcher::{Dispatcher, DispatcherConfig};
pub use batch_renderer::BatchRenderer;
pub use pack_html_format::PackfileEntry;
pub use packfile_builder::PackfileBuilder;
// LiveRegistry suit la même convention que BatchRenderer ci-dessus : le type
// principal d'un module est ré-exporté à plat. PackHtmlIndex, lui, n'était
// déjà pas ré-exporté avant cette session (consommé uniquement via
// crate::pack_html_index::PackHtmlIndex en interne) — pas étendu ici, hors
// périmètre de la Phase 2.
pub use registry::LiveRegistry;
