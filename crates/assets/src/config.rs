// crates/assets/src/config.rs

//! Désérialisation et modèle de configuration de theme.toml.
//!
//! ## Stratégie de Transmission
//!
//! Le point d'entrée unique ThemeConfig est consommé exclusivement à l'initialisation par main().
//! Les champs sont immédiatement déstructurés en types simples (primitives, &[String], &HashMap<String, String>)
//! et passés directement aux pipelines d'actifs respectifs.
//!
//! Cette approche évite de faire circuler une structure de configuration monolithique à travers
//! les sous-systèmes.
//!
//! Exception : WebManifestConfig et ServiceWorkerConfig font référence à des configurations PWA
//! autonomes et sont transmis en tant que blocs isolés.

use std::collections::HashMap;

use serde::Deserialize;

/// Configuration racine désérialisée depuis theme.toml.
#[derive(Deserialize)]
pub(crate) struct ThemeConfig {
    /// Métadonnées générales du thème (`[theme]`).
    pub(crate) theme: ThemeInfo,
    /// Configuration des feuilles de style et pipelines CSS (`[styles]`).
    #[serde(default)]
    pub(crate) styles: StylesConfig,
    /// Configuration des ressources statiques recopiées à l'identique (`[static]`).
    ///
    /// *Note : Le champ est nommé `static_` pour éviter le mot-clé réservé Rust `static`.*
    #[serde(rename = "static", default)]
    pub(crate) static_: StaticConfig,

    /// Dictionnaire plat des planches de sprites (`[sprites]`).
    ///
    /// Associe un nom logique au dossier source des images (ex: `silos = "sprites/silos"`).
    #[serde(default)]
    pub(crate) sprites: HashMap<String, String>,

    /// Configuration PWA Web Manifest (`[webmanifest]`).
    ///
    /// Section optionnelle : un thème sans PWA reste valide. Un seul manifeste
    /// est autorisé par thème (spécification W3C).
    #[serde(default)]
    pub(crate) webmanifest: Option<WebManifestConfig>,

    /// Points d'entrée des scripts et composants client (`[scripts]`).
    ///
    /// Mappe les paires clé/valeur imbriquées sous `[scripts.components]`
    /// et `[scripts.capabilities]`.
    #[serde(default)]
    pub(crate) scripts: ScriptsConfig,

    /// Configuration du Service Worker PWA (`[service_worker]`).
    ///
    /// Section optionnelle. Si absente, le Service Worker est supposé se trouver
    /// à la racine du thème (`service-worker.js`).
    #[serde(default)]
    pub(crate) service_worker: Option<ServiceWorkerConfig>,
}

#[derive(Deserialize)]
pub(crate) struct ServiceWorkerConfig {
    pub(crate) entry: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct ScriptsConfig {
    /// Scripts inconditionnels — chargés sur toute page qui les référence,
    /// sans dépendance à une donnée persistée (`[scripts.components]`).
    #[serde(default)]
    pub(crate) components: HashMap<String, String>,

    /// Capacités frontend conditionnelles (`[scripts.capabilities.*]`).
    ///
    /// Amélioration progressive pilotée à l'exécution par
    /// `content.core.js_deps` (bitset) — jamais chargées inconditionnellement
    /// comme `components` ci-dessus. Clé de la `HashMap` = nom de capacité =
    /// future clé d'attribution de bit dans `scripts_registry.lock` (source
    /// de vérité de l'identité `capacité → bit`, distincte de ce fichier et
    /// non lue par ce binaire — voir HANDOFF-js-deps-capacites-frontend-v2.md,
    /// § Déterminisme de l'attribution des bits).
    #[serde(default)]
    pub(crate) capabilities: HashMap<String, CapabilityConfig>,
}

/// Une entrée de `[scripts.capabilities.<nom>]`.
///
/// `markers`/`activation` ne transitent jamais au-delà de la désérialisation
/// de ce binaire — ni vers `AssetManifest`, ni vers aucune autre structure
/// partagée. Seul `entry` alimente `run_scripts_pipeline` (résolution d'URL
/// hachée), exactement au même titre qu'une entrée de `components`. Ces deux
/// champs sont lus en aval, hors de ce crate, par `schema/build.rs`
/// (lecture de `theme.toml` en tant que source d'intention humaine, pas par
/// `marius-assets`).
#[derive(Deserialize)]
pub(crate) struct CapabilityConfig {
    pub(crate) entry: String,
    /// Jamais lu par ce binaire (`marius-assets`) — seul `entry` alimente
    /// `run_scripts_pipeline`. Consommé exclusivement par
    /// `crates/core/schema/build.rs` (validation non-vide, lecture SQL en
    /// aval), via sa propre désérialisation de `theme.toml`, distincte de
    /// celle-ci (aucun type partagé entre les deux crates, Roadmap
    /// `marius-assets` §2.1).
    #[allow(dead_code)]
    pub(crate) markers: Vec<String>,
    /// Idem `markers` ci-dessus — consommé par `build.rs` pour le lowering
    /// AOT de `js_deps` (import ESM nommé), jamais par ce binaire.
    #[allow(dead_code)]
    pub(crate) activation: String,
}

#[derive(Deserialize)]
pub(crate) struct WebManifestConfig {
    pub(crate) entry: String,
}

#[derive(Deserialize)]
pub(crate) struct ThemeInfo {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct StylesConfig {
    #[serde(default)]
    pub(crate) entries: Vec<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct StaticConfig {
    #[serde(default)]
    pub(crate) verbatim: VerbatimConfig,
}

#[derive(Deserialize, Default)]
pub(crate) struct VerbatimConfig {
    #[serde(default)]
    pub(crate) files: Vec<String>,
}
