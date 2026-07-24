//! crates/shell/render/src/lib.rs
//!
//! Façade du crate marius-render.

mod sweep;

pub mod batch_renderer;
pub mod dispatcher;
pub mod dumper;
pub mod ingest_and_swap;
pub mod merge_store;
pub mod pack_html_format;
pub mod pack_html_index;
pub mod packfile_builder;
pub mod regenerate;
pub mod registry;
pub mod store_provisioning;

// Ré-export pour la façade
pub use batch_renderer::BatchRenderer;
pub use dispatcher::{Dispatcher, DispatcherConfig};
pub use pack_html_format::PackfileEntry;
pub use packfile_builder::PackfileBuilder;

// regenerate_and_swap — Phase 4. Même convention que BatchRenderer ci-dessus
// (fonction principale d'un module, ré-exportée à plat). dispatcher.rs
// l'appelle via `crate::regenerate_and_swap`, pas
// `crate::regenerate::regenerate_and_swap` — cohérent avec son usage
// `crate::BatchRenderer` déjà en place pour batch_renderer.
pub use regenerate::regenerate_and_swap;

// ingest_and_swap — même convention que regenerate_and_swap/BatchRenderer
// ci-dessus (fonction principale d'un module, ré-exportée à plat). Nouveau
// cette session (Phase 1, réactivité CoW) : étage 1 du pipeline, appelé par
// Dispatcher::run via `crate::ingest_and_swap`, jamais
// `crate::ingest_and_swap::ingest_and_swap` — cohérent avec l'appel
// `crate::regenerate_and_swap` déjà en place juste après lui dans
// Dispatcher::run.
pub use ingest_and_swap::ingest_and_swap;

// ensure_provisioned, ProvisionOutcome — même convention que
// regenerate_and_swap ci-dessus (fonction/type principal d'un module,
// ré-exporté à plat). main.rs l'appelle via marius_render::ensure_provisioned,
// pas marius_render::regenerate::ensure_provisioned.
pub use regenerate::{ProvisionOutcome, ensure_provisioned};

// ensure_store_provisioned — même convention, module distinct
// (store_provisioning.rs, pas regenerate.rs) : symétrique à
// ensure_provisioned mais pour store.bin plutôt que pack.bin. Nouveau cette
// session (Phase 1, réactivité CoW), ajouté après coup — un premier essai de
// bootstrap sans cette fonction cassait le contrat de démarrage propre sur
// environnement vierge déjà vérifié par
// server_supervision_and_provisioning.rs. ProvisionOutcome n'est pas
// réexporté une seconde fois : store_provisioning.rs réutilise celui déjà
// déclaré par regenerate.rs (`pub use crate::regenerate::ProvisionOutcome;`
// dans ce module), pas de doublon de type à exposer ici.
pub use store_provisioning::ensure_store_provisioned;

// LiveRegistry, RouteEntry, IdSource, packfile_path_for : même convention que
// BatchRenderer ci-dessus (type principal d'un module, ré-exporté à plat).
// RouteEntry/IdSource/packfile_path_for ajoutés en Phase 3 — nécessaires dès
// que la frontière réseau (marius-server) doit construire sa ROUTE_TABLE et
// résoudre les chemins de packfiles avec les mêmes types que cold_start().
pub use registry::{IdSource, LiveRegistry, RouteEntry, packfile_path_for};

// PackHtmlIndex — non ré-exporté avant cette session ("pas étendu, hors
// périmètre de la Phase 2"). Phase 3 le requiert : LiveRegistry::load()
// retourne Arc<PackHtmlIndex> à handlers.rs (marius-server), qui doit nommer
// ce type pour typer deliver(). Première fois que la frontière de crate
// matérialise ce besoin — pas une extension anticipée par confort.
pub use pack_html_index::PackHtmlIndex;
