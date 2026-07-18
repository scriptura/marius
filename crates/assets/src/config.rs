// crates/assets/src/config.rs
//
// Désérialisation de theme.toml. Un seul point d'entrée, `ThemeConfig`,
// consommé exclusivement par `main()` — chaque champ est ensuite passé aux
// pipelines sous forme déjà destructurée (primitives, `&[String]`,
// `&HashMap<String,String>`), jamais le struct de config entier, sauf pour
// `WebManifestConfig`/`ServiceWorkerConfig` (un seul point d'entrée nommé,
// plus simple à passer tel quel).

use std::collections::HashMap;

use serde::Deserialize;

// =============================================================================
// theme.toml — désérialisation d'entrée
// =============================================================================

#[derive(Deserialize)]
pub(crate) struct ThemeConfig {
    pub(crate) theme: ThemeInfo,
    #[serde(default)]
    pub(crate) styles: StylesConfig,
    // `static` est un mot-clé Rust — renommage obligatoire côté champ, pas
    // côté TOML (la clé `[static.verbatim]` reste inchangée dans le fichier).
    #[serde(rename = "static", default)]
    pub(crate) static_: StaticConfig,
    // [sprites] — Phase 4, cette session : dictionnaire plat nom logique
    // -> dossier source (`silos = "sprites/silos"`). Pas de struct
    // intermédiaire nécessaire, contrairement à `styles`/`static_` : une
    // seule paire clé/valeur par entrée, `HashMap<String, String>` suffit
    // à la représenter fidèlement sans couche superflue.
    #[serde(default)]
    pub(crate) sprites: HashMap<String, String>,
    // [webmanifest] — Phase 6, cette session : un seul point d'entrée (pas
    // une liste comme [styles].entries — un site n'a qu'un seul manifeste
    // PWA par construction W3C). `Option`, pas un champ requis : un thème
    // sans PWA reste un thème valide, ne pas forcer une section vide.
    #[serde(default)]
    pub(crate) webmanifest: Option<WebManifestConfig>,
    // [scripts.components] — Phase 7, cette session : table imbriquée
    // (contrairement à [sprites], à plat) parce que la clé TOML porte deux
    // niveaux (`scripts.components`), pas parce que la donnée elle-même
    // est plus riche — `ScriptsConfig` n'existe que pour porter ce niveau
    // d'imbrication, `components` reste un dictionnaire plat nom logique
    // -> point d'entrée, exactement comme [sprites].
    #[serde(default)]
    pub(crate) scripts: ScriptsConfig,
    // [service_worker] — Handoff §3, cette session : même forme que
    // [webmanifest] (un seul point d'entrée, `Option` — un thème sans SW
    // reste valide). Section absente de `theme.toml` à ce jour : à
    // ajouter par l'auteur du projet, cf. réponse de session — supposé
    // par défaut à la racine du thème (`serviceWorker.js`), au même
    // niveau que `manifest.webmanifest`, à corriger si le fichier réel
    // vit ailleurs.
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
