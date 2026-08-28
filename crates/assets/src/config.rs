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
    /// à la racine du thème (`serviceWorker.js`).
    #[serde(default)]
    pub(crate) service_worker: Option<ServiceWorkerConfig>,

    /// Bibliothèques frontend tierces vendoriées (`[libraries.*]`) —
    /// SPEC-canonical-asset-identity.md §6.
    ///
    /// Autorisation de découverte récursive, jamais un second pipeline
    /// d'assets : chaque `root` déclaré est walké au build, chaque fichier
    /// trouvé rejoint la même liste que `[static.verbatim].files` avant
    /// d'entrer dans `run_verbatim_pipeline` — traitement strictement
    /// identique une fois découvert, aucune bifurcation. Clé de la
    /// `HashMap` = nom logique de la bibliothèque, jamais utilisé comme
    /// namespace d'identité (§9 : un asset de bibliothèque reçoit son
    /// `CanonicalAssetId` = son chemin réel sous le thème, pas un préfixe
    /// dérivé de cette clé).
    #[serde(default)]
    pub(crate) libraries: HashMap<String, LibraryConfig>,
}

/// Une entrée de `[libraries.<nom>]`.
#[derive(Deserialize)]
pub(crate) struct LibraryConfig {
    /// Répertoire racine de la bibliothèque, relatif au thème — tout son
    /// sous-arbre est autorisé à entrer dans le build (JS, CSS, images,
    /// fonts, `.map`, sans distinction — SPEC §6).
    pub(crate) root: String,

    /// Marius est ESM-first : par défaut, tout fichier `.js` découvert
    /// sous `root` est considéré consommable comme module ES (`<script
    /// type="module">` s'il est chargé via `deps`, §HANDOFF `deps`).
    /// `module = false` est une concession explicite pour une
    /// bibliothèque vendorée qui reste distribuée en UMD/classique et
    /// expose son API via `window` — jamais une supposition implicite.
    /// Propriété de l'ASSET, pas de la déclaration `deps` qui le consomme
    /// (une même bibliothèque se charge toujours de la même façon, quelle
    /// que soit la capacité qui la référence).
    #[serde(default = "default_module_true")]
    pub(crate) module: bool,
}

fn default_module_true() -> bool {
    true
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
    /// Scripts dont le CHARGEMENT doit précéder l'activation de cette
    /// capacité — jamais un `import` ESM à injecter dans `entry` (`map.js`
    /// reste intégralement ignorant de ce mécanisme). Jamais nommé
    /// `js_deps` : ceci est une dépendance de chargement de script au sens
    /// de `[scripts.capabilities.*]`, pas un détail d'implémentation du
    /// bitset `content.core.js_deps`, qui reste un mécanisme totalement
    /// distinct.
    ///
    /// **Jamais lu par ce binaire**, exactement comme `markers`/
    /// `activation` ci-dessus — déclaré ici uniquement pour documenter la
    /// forme complète d'une capacité. La résolution AOT
    /// (`canonicalize_reference` → `resolve_asset_reference` →
    /// `AssetUrlRegistry` → échec dur si absent) est entièrement à la
    /// charge de `crates/core/schema/build.rs`, via sa propre
    /// désérialisation indépendante de `theme.toml` (aucun type partagé
    /// entre les deux crates). `marius-assets` n'a besoin de rien savoir
    /// de `deps` : il produit `manifest.toml` (via `[libraries.*]`), c'est
    /// tout ce dont `build.rs` a besoin pour résoudre `deps` lui-même.
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) deps: Vec<String>,
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
