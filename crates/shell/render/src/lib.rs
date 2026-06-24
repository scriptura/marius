// =============================================================================
// crates/shell/render/src/lib.rs
//
// Façade du crate marius-render.
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

// LiveRegistry, RouteEntry, IdSource, packfile_path_for : même convention que
// BatchRenderer ci-dessus (type principal d'un module, ré-exporté à plat).
// RouteEntry/IdSource/packfile_path_for ajoutés en Phase 3 — nécessaires dès
// que la frontière réseau (marius-server) doit construire sa ROUTE_TABLE et
// résoudre les chemins de packfiles avec les mêmes types que cold_start().
pub use registry::{packfile_path_for, IdSource, LiveRegistry, RouteEntry};

// PackHtmlIndex — non ré-exporté avant cette session ("pas étendu, hors
// périmètre de la Phase 2"). Phase 3 le requiert : LiveRegistry::load()
// retourne Arc<PackHtmlIndex> à handlers.rs (marius-server), qui doit nommer
// ce type pour typer deliver(). Première fois que la frontière de crate
// matérialise ce besoin — pas une extension anticipée par confort.
pub use pack_html_index::PackHtmlIndex;
