// crates/core/schema/build/template/mod.rs

//! Pipeline Voie B — lecture d'un `.marius`, résolution, génération du
//! corps de `render()`. Point d'entrée : [`dynamic::resolve_template`]
//! (composants pilotés par `fetch_component_list`) et
//! [`static_page::resolve_static_page`] (`STATIC_PAGES`, sans SQL).

pub(crate) mod common;
pub(crate) mod dynamic;
pub(crate) mod page;
pub(crate) mod static_page;
