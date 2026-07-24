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
    /// Mappe les paires clé/valeur imbriquées sous `[scripts.components]`.
    #[serde(default)]
    pub(crate) scripts: ScriptsConfig,

    /// Configuration du Service Worker PWA (`[service_worker]`).
    ///
    /// Section optionnelle. Si absente, le Service Worker est supposé se trouver
    /// à la racine du thème (`serviceWorker.js`).
    #[serde(default)]
    pub(crate) service_worker: Option<ServiceWorkerConfig>,
}

#[derive(Deserialize)]
pub(crate) struct ServiceWorkerConfig {
    pub(crate) entry: String,
}

#[derive(Deserialize, Default)]
pub(crate) struct ScriptsConfig {
    #[serde(default)]
    pub(crate) components: HashMap<String, String>,
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
