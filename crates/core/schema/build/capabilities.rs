// crates/core/schema/build/capabilities.rs

//! Résolution AOT de la table des capacités JS déclarées dans
//! `theme.toml` (`[scripts.capabilities.*]`), croisées avec
//! `scripts_registry.lock` pour les capacités `content_driven`.
//!
//! `validate_capabilities` est calculée UNE SEULE FOIS pour tout le build —
//! jamais recalculée par template (voir `crate::modules_lowering` pour le
//! croisement par template).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::manifest::{AssetEntry, scripts_registry_path, theme_source_dir};
use crate::markers::{MarkerPredicate, parse_marker};

/// Une entrée de `theme.toml` → `[scripts.capabilities.<nom>]`.
///
/// Désérialisation MINIMALE, propre à `build.rs` — délibérément dupliquée
/// depuis `crates/marius-assets/src/config.rs` plutôt que partagée : même
/// interdiction de couplage de types Rust entre `marius-assets` et les
/// crates de la Forge que pour `suggest_asset_key`/`levenshtein` ci-dessus
/// (Roadmap `marius-assets` §2.1). `markers` est désérialisé brut ici
/// (`Vec<String>`) puis parsé en `MarkerPredicate` par `parse_marker` dans
/// `validate_capabilities` — seule sa consommation DYNAMIQUE (bitset
/// `content.core.js_deps`) reste exclusivement SQL (`compute_js_deps`,
/// chantier séparé, non touché ici).
#[derive(Deserialize)]
struct CapabilityConfig {
    entry: String,
    markers: Vec<String>,
    activation: String,
    /// Miroir de `crates/assets/src/config.rs::CapabilityConfig.deps` —
    /// dépendances de CHARGEMENT (jamais un import ESM à injecter dans
    /// `entry`), résolues ici contre le manifeste exactement comme
    /// `entry` lui-même. Nom délibérément distinct de `js_deps`
    /// (`content.core`) : deux mécanismes sans rapport, l'un statique
    /// (chargement de script), l'autre un bitset runtime.
    #[serde(default)]
    deps: Vec<String>,
    /// Déclare si cette capacité peut être activée dynamiquement à partir
    /// de `record.js_deps` (bitset produit par `content.compute_js_deps`,
    /// SQL) — jamais une propriété intrinsèque de toute capacité. `true`
    /// exige une entrée active dans `scripts_registry.lock` ; `false`
    /// (défaut) interdit toute entrée pour cette capacité, quelle qu'elle
    /// soit (`validate_capabilities`, chantier « découplage registry »).
    ///
    /// Orthogonal à `markers`/`activation`/`deps` : ne change ni le
    /// comportement du scan statique (`extract_static_marker_facts`), ni
    /// celui de `deps` — seulement l'obligation (ou l'interdiction) d'un
    /// bit `js_deps`. Défaut `false` délibéré : la paire d'invariants
    /// symétrique de `validate_capabilities` (bit manquant si `true` sans
    /// registre ; entrée orpheline ou mal flaguée si `false` avec
    /// registre) fait échouer le build bruyamment dans les deux sens d'un
    /// oubli — aucun chemin silencieux ne masque une incohérence
    /// architecturale, contrairement à un défaut qui aurait été `true`.
    #[serde(default)]
    content_driven: bool,
}

#[derive(Deserialize, Default)]
struct ScriptsSection {
    #[serde(default)]
    capabilities: HashMap<String, CapabilityConfig>,
}

/// Vue partielle de `theme.toml` — seule la section `[scripts.capabilities]`
/// intéresse ce lowering ; `[theme]`, `[styles]`, `[sprites]`, etc. sont
/// ignorées ici (déjà day-to-day consommées par `marius-assets`, jamais par
/// `db-forge`).
#[derive(Deserialize)]
struct ThemeTomlScriptsOnly {
    #[serde(default)]
    scripts: ScriptsSection,
}

/// Une capacité entièrement validée et résolue — bit, activation, URL,
/// marqueurs — prête pour le lowering par template. Calculée UNE SEULE FOIS
/// pour tout le build (`validate_capabilities`), jamais recalculée par
/// template : c'est `lower_modules_for_template` qui la croise, à chaque
/// appel, avec les faits statiques du template en cours.
pub(crate) struct CapabilityInfo {
    /// `Some(bit)` uniquement si `content_driven = true` (bit résolu
    /// contre `scripts_registry.lock`) — `None` pour toute capacité
    /// `content_driven = false` (défaut). Jamais une propriété
    /// intrinsèque de toute capacité : `lower_modules_for_template` ne le
    /// consulte que dans sa branche dynamique, jamais dans la branche
    /// statique (`bit: None` de `ModuleEmission`, indépendant de ce
    /// champ).
    pub(crate) bit: Option<i64>,
    pub(crate) activation: String,
    pub(crate) url: String,
    /// Marqueurs déjà PARSÉS (`parse_marker`, une seule fois ici) —
    /// nécessaires pour le scan statique (comparés aux quatre ensembles de
    /// `StaticMarkerFacts`), pas seulement pour la validation de
    /// non-vacuité. Jamais une chaîne brute au-delà de cette structure.
    pub(crate) markers: Vec<MarkerPredicate>,
    /// Dépendances de chargement déjà résolues (clé canonique, URL hachée,
    /// mode de chargement) — résolution faite UNE SEULE FOIS ici, jamais
    /// recalculée par template (même discipline que `url` ci-dessus).
    pub(crate) deps: Vec<ResolvedDep>,
}

/// Une dépendance de `deps` entièrement résolue contre le manifeste —
/// jamais construite avant la résolution AOT, jamais une chaîne brute
/// transportée telle quelle au-delà de `validate_capabilities`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedDep {
    /// Identité canonique — clé de dédoublonnage entre capacités
    /// partageant la même dépendance (SPEC `deps` §3) : jamais l'URL
    /// hachée finale, qui varie avec le contenu et masquerait deux
    /// déclarations identiques comme distinctes à la moindre
    /// recompilation.
    pub(crate) canonical_key: String,
    pub(crate) url: String,
    /// `true` (défaut ESM-first) → `<script type="module">` ; `false` →
    /// `<script defer>` classique (bibliothèque `[libraries.*].module =
    /// false`). Propriété de l'ASSET référencé, jamais de la déclaration
    /// `deps` qui le consomme.
    pub(crate) module: bool,
}

/// Identifiant Rust/JS valide — `activation` (theme.toml) est injecté
/// VERBATIM, comme identifiant nu (pas comme littéral de chaîne échappé),
/// dans le code Rust généré (`import{{{activation} as _n}}...`) : un texte
/// libre non validé romprait la génération de code, voire l'injecterait.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Valide et résout GLOBALEMENT (une seule fois pour tout le build) la
/// table canonique des capacités `js_deps` — bijection `theme.toml` ↔
/// `scripts_registry.lock`, validité/unicité des bits, `activation` valide,
/// résolution de chaque `entry` contre le manifeste d'assets. Ne connaît
/// aucun template spécifique : `lower_modules_for_template` croise cette
/// table, à chaque appel, avec le HTML statique du template en cours.
///
/// Retourne les capacités triées par nom (ordre déterministe, reproductible
/// d'un build à l'autre — jamais l'ordre d'itération d'une `HashMap`).
pub(crate) fn validate_capabilities(
    manifest_dir: &str,
    assets: &HashMap<String, AssetEntry>,
    classic_scripts: &HashSet<String>,
) -> Result<Vec<(String, CapabilityInfo)>, ()> {
    let theme_toml_path = theme_source_dir(manifest_dir).join("theme.toml");
    println!("cargo:rerun-if-changed={}", theme_toml_path.display());

    let raw_theme = std::fs::read_to_string(&theme_toml_path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : theme.toml introuvable ({}) : {e}",
            theme_toml_path.display()
        );
    })?;
    let theme: ThemeTomlScriptsOnly = toml::from_str(&raw_theme).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : theme.toml malformé ({}) : {e}",
            theme_toml_path.display()
        );
    })?;

    let capabilities = &theme.scripts.capabilities;

    if capabilities.is_empty() {
        // Rien à valider — scripts_registry.lock peut même ne pas exister
        // tant qu'aucune capacité ne l'exige.
        return Ok(Vec::new());
    }

    // Le registre n'est requis QUE si au moins une capacité est
    // `content_driven = true` — une capacité `content_driven = false`
    // (défaut) n'a structurellement aucun besoin de bit, donc aucun besoin
    // que ce fichier existe même. `scripts_registry.lock` n'est donc plus
    // une dépendance systématique de [scripts.capabilities], contrairement
    // à avant le découplage.
    let needs_registry = capabilities.values().any(|c| c.content_driven);

    let registry_path = scripts_registry_path(manifest_dir);
    println!("cargo:rerun-if-changed={}", registry_path.display());

    let registry: HashMap<String, i64> = match std::fs::read_to_string(&registry_path) {
        Ok(raw) => toml::from_str(&raw).map_err(|e| {
            println!(
                "cargo:error=DB-Forge : scripts_registry.lock malformé ({}) : {e}",
                registry_path.display()
            );
        })?,
        Err(e) => {
            if needs_registry {
                println!(
                    "cargo:error=DB-Forge : {} capacité(s) content_driven = true déclarée(s) \
                     dans [scripts.capabilities] mais scripts_registry.lock introuvable ({}) : \
                     {e}",
                    capabilities.values().filter(|c| c.content_driven).count(),
                    registry_path.display()
                );
                return Err(());
            }
            // Aucune capacité content-driven — l'absence du fichier n'est
            // pas une erreur, un registre vide est équivalent.
            HashMap::new()
        }
    };

    let mut errors: Vec<String> = Vec::new();

    // Entrées actives = clé ne commençant pas par "_retired_" — un nom
    // retiré n'est jamais réattribuable, mais reste dans le fichier comme
    // mémoire d'identité (le bit ne doit jamais être réutilisé par un
    // futur nom différent).
    let active_registry: HashMap<&str, i64> = registry
        .iter()
        .filter(|(k, _)| !k.starts_with("_retired_"))
        .map(|(k, v)| (k.as_str(), *v))
        .collect();

    // ── Bijection : capacités content_driven = true ↔ registre actif ────
    // Scope volontairement restreint au sous-ensemble content-driven —
    // une capacité `content_driven = false` n'entre JAMAIS dans cette
    // bijection, dans aucun des deux sens.
    for (name, cap) in capabilities {
        if cap.content_driven && !active_registry.contains_key(name.as_str()) {
            errors.push(format!(
                "capacité '{name}' déclarée content_driven = true dans [scripts.capabilities] \
                 mais absente de scripts_registry.lock — attribution de bit manquante"
            ));
        }
    }
    for name in active_registry.keys() {
        match capabilities.get(*name) {
            None => {
                errors.push(format!(
                    "bit '{name}' présent dans scripts_registry.lock (actif) mais aucune \
                     capacité correspondante dans [scripts.capabilities] de theme.toml — \
                     capacité retirée sans préfixe '_retired_', ou registre en avance sur la \
                     configuration"
                ));
            }
            Some(cap) if !cap.content_driven => {
                errors.push(format!(
                    "bit '{name}' présent dans scripts_registry.lock (actif) mais \
                     [scripts.capabilities.{name}].content_driven n'est pas 'true' — une \
                     capacité non content-driven ne doit jamais posséder d'entrée registry \
                     (retirez l'entrée, ou déclarez explicitement content_driven = true si \
                     cette capacité doit réellement être pilotée par record.js_deps)"
                ));
            }
            Some(_) => {}
        }
    }

    // ── Validité et unicité des bits ─────────────────────────────────────
    let mut seen_bits: HashMap<i64, &str> = HashMap::new();
    for (name, bit) in &active_registry {
        if *bit <= 0 || (*bit & (*bit - 1)) != 0 {
            errors.push(format!(
                "bit invalide pour '{name}' : {bit} n'est pas une puissance de deux \
                 strictement positive"
            ));
            continue;
        }
        if let Some(other) = seen_bits.insert(*bit, name) {
            errors.push(format!(
                "collision de bit {bit} entre '{other}' et '{name}' — deux capacités ne \
                 peuvent jamais partager le même bit"
            ));
        }
    }

    if !errors.is_empty() {
        for e in &errors {
            println!("cargo:error=DB-Forge [scripts_registry.lock] : {e}");
        }
        return Err(());
    }

    // ── Résolution — ordre déterministe (tri par nom), jamais l'ordre
    // d'itération d'une HashMap, qui varie d'un build à l'autre et
    // romprait la reproductibilité du fichier généré. ───────────────────
    let mut names: Vec<&String> = capabilities.keys().collect();
    names.sort();

    let mut result: Vec<(String, CapabilityInfo)> = Vec::new();

    for name in names {
        let cap = &capabilities[name];
        // Sûr : si `cap.content_driven`, la bijection ci-dessus garantit
        // déjà une entrée (sinon `errors` serait non vide et la fonction
        // aurait déjà retourné `Err(())` avant d'atteindre cette boucle).
        let bit = if cap.content_driven {
            Some(active_registry[name.as_str()])
        } else {
            None
        };

        if !is_valid_identifier(&cap.activation) {
            errors.push(format!(
                "[scripts.capabilities.{name}].activation = {:?} n'est pas un identifiant \
                 Rust/JS valide — jamais injecté tel quel dans le code généré",
                cap.activation
            ));
            continue;
        }

        if cap.markers.is_empty() {
            errors.push(format!(
                "[scripts.capabilities.{name}].markers est vide — cette capacité ne pourrait \
                 jamais être déclenchée, ni par aucun contenu éditorial (content.compute_js_deps), \
                 ni par aucun template (scan statique)"
            ));
            continue;
        }

        // Parsing AOT de chaque marker brut vers sa forme typée
        // (`MarkerPredicate`) — échec dur et explicite par marker malformé,
        // jamais un repli implicite. Tous les markers d'une capacité sont
        // validés avant de continuer, pour remonter d'un coup toutes les
        // erreurs de syntaxe plutôt que de s'arrêter à la première.
        let mut parsed_markers: Vec<MarkerPredicate> = Vec::with_capacity(cap.markers.len());
        let mut marker_error = false;
        for raw_marker in &cap.markers {
            match parse_marker(raw_marker) {
                Ok(predicate) => parsed_markers.push(predicate),
                Err(message) => {
                    marker_error = true;
                    errors.push(format!("[scripts.capabilities.{name}].markers : {message}"));
                }
            }
        }
        if marker_error {
            continue;
        }

        // Identité canonique (SPEC-canonical-asset-identity.md) : la clé
        // manifeste d'un point d'entrée est son chemin THÈME-RELATIF réel
        // (`cap.entry`, ex. "scripts/map.js"), jamais un nom symbolique
        // suffixé `.js` dérivé de `name` — l'identité publique d'un asset
        // ne dépend jamais du pipeline/de la clé `theme.toml` qui l'a
        // produit (même invariant que `deps` juste en dessous, et que
        // `[libraries.*]`/`[static.verbatim]`/CSS/sprites partout ailleurs
        // dans ce projet). Slash de tête toléré, même convention que
        // `deps` : jamais une résolution différente selon que
        // l'intégrateur l'a écrit ou non.
        let manifest_key = cap.entry.strip_prefix('/').unwrap_or(&cap.entry);
        let url = match assets.get(manifest_key) {
            Some(entry) => entry.url.clone(),
            None => {
                errors.push(format!(
                    "'{name}' : clé '{manifest_key}' absente du manifeste d'assets — \
                     [scripts.capabilities.{name}].entry ({:?}) n'a produit aucune entrée via \
                     run_scripts_pipeline",
                    cap.entry
                ));
                continue;
            }
        };

        // Résolution de `deps` — MÊME contrat et MÊME convention d'identité
        // que `manifest_key` ci-dessus (chemin canonique thème-relatif,
        // jamais un nom symbolique) : échec dur, jamais un repli silencieux.
        // Slash de tête optionnel toléré (même convention que `scripts.rs`
        // §3.2 côté JS) — jamais une résolution différente selon que
        // l'intégrateur l'a écrit ou non.
        let mut deps: Vec<ResolvedDep> = Vec::new();
        let mut dep_error = false;
        for raw_dep in &cap.deps {
            let dep_key = raw_dep.strip_prefix('/').unwrap_or(raw_dep);
            match assets.get(dep_key) {
                Some(entry) => deps.push(ResolvedDep {
                    canonical_key: dep_key.to_string(),
                    url: entry.url.clone(),
                    // ESM-first : classique/UMD uniquement si explicitement
                    // listé — jamais une propriété de `entry` lui-même
                    // (`AssetEntry` reste un pur descripteur d'artefact).
                    module: !classic_scripts.contains(dep_key),
                }),
                None => {
                    dep_error = true;
                    errors.push(format!(
                        "'{name}' : deps '{raw_dep}' absente du manifeste d'assets — aucune \
                         entrée '{dep_key}' (bibliothèque non déclarée dans [libraries.*], \
                         chemin incorrect, ou build de marius-assets non relancé)"
                    ));
                }
            }
        }
        if dep_error {
            continue;
        }

        result.push((
            name.clone(),
            CapabilityInfo {
                bit,
                activation: cap.activation.clone(),
                url,
                markers: parsed_markers,
                deps,
            },
        ));
    }

    if !errors.is_empty() {
        for e in &errors {
            println!("cargo:error=DB-Forge [theme.toml scripts.capabilities] : {e}");
        }
        return Err(());
    }

    Ok(result)
}

/// Tests de `validate_capabilities` — résolution AOT de `deps` (SPEC
/// `deps`). Sandbox filesystem, même discipline que `verbatim.rs`/
/// `libraries.rs`/`webmanifest.rs` (`marius-assets`) : nécessaire ici
/// aussi, `validate_capabilities` lit réellement `theme.toml` et
/// `scripts_registry.lock` depuis `manifest_dir` (via `theme_source_dir`,
/// remontée fixe de 3 niveaux + `assets/{THEME_NAME}`).
#[cfg(test)]
mod tests_validate_capabilities_deps {
    use super::*;
    use std::fs;

    /// Construit `{base}/a/b/c` (le `manifest_dir` fictif passé à
    /// `validate_capabilities`) et `{base}/assets/default` (où
    /// `theme_source_dir` va effectivement chercher `theme.toml` après
    /// résolution des trois `..`) — les deux DOIVENT exister physiquement
    /// pour que la traversée de chemin réussisse au niveau de l'OS.
    fn sandbox(name: &str) -> (std::path::PathBuf, String) {
        let base = std::env::temp_dir().join(format!(
            "marius-core-schema-test-deps-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let manifest_dir = base.join("a/b/c");
        fs::create_dir_all(&manifest_dir).unwrap();
        fs::create_dir_all(base.join("assets/default")).unwrap();
        (base, manifest_dir.to_string_lossy().into_owned())
    }

    fn write_theme_toml(base: &std::path::Path, capabilities_toml: &str) {
        fs::write(base.join("assets/default/theme.toml"), capabilities_toml).unwrap();
    }

    fn write_registry(base: &std::path::Path, entries: &str) {
        fs::write(base.join("assets/default/scripts_registry.lock"), entries).unwrap();
    }

    /// `AssetEntry` reste un pur descripteur d'artefact — plus aucun
    /// paramètre `module` ici, volontairement : voir `classic_scripts` en
    /// argument direct de `validate_capabilities` dans chaque test.
    fn asset(url: &str) -> AssetEntry {
        AssetEntry {
            url: url.to_string(),
            path: String::new(),
            mime: String::new(),
            size: 0,
            hash: String::new(),
            version: String::new(),
        }
    }

    fn classic(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deps_resolved_with_module_flag_from_classic_scripts() {
        let (base, manifest_dir) = sandbox("resolved");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = [".map"]
activation = "bootstrap"
content_driven = true
deps = ["libraries/deckgl/deckgl.js"]
"#,
        );
        write_registry(&base, "map = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));
        assets.insert(
            "libraries/deckgl/deckgl.js".to_string(),
            asset("/libraries/deckgl.HASH.js"),
        );
        let classic_scripts = classic(&["libraries/deckgl/deckgl.js"]);

        let result = validate_capabilities(&manifest_dir, &assets, &classic_scripts).unwrap();
        assert_eq!(result.len(), 1);
        let (name, info) = &result[0];
        assert_eq!(name, "map");
        assert_eq!(info.deps.len(), 1);
        assert_eq!(info.deps[0].canonical_key, "libraries/deckgl/deckgl.js");
        assert_eq!(info.deps[0].url, "/libraries/deckgl.HASH.js");
        assert!(
            !info.deps[0].module,
            "deckgl listée dans classic_scripts doit rester classique (module: false) ici"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// ESM-first : une dépendance résolue mais ABSENTE de `classic_scripts`
    /// doit rester `module: true` — comportement par défaut, jamais une
    /// supposition dépendant de l'extension ou du contenu du fichier.
    #[test]
    fn dep_not_in_classic_scripts_defaults_to_module_true() {
        let (base, manifest_dir) = sandbox("esm-default");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.foo]
entry = "scripts/foo.js"
markers = [".foo"]
activation = "boot"
content_driven = true
deps = ["libraries/bar/bar.js"]
"#,
        );
        write_registry(&base, "foo = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/foo.js".to_string(), asset("/scripts/foo.HASH.js"));
        assets.insert(
            "libraries/bar/bar.js".to_string(),
            asset("/libraries/bar.HASH.js"),
        );
        let classic_scripts = HashSet::new(); // aucune bibliothèque classique déclarée

        let result = validate_capabilities(&manifest_dir, &assets, &classic_scripts).unwrap();
        assert!(result[0].1.deps[0].module);

        let _ = fs::remove_dir_all(&base);
    }

    /// Décision d'architecture de cette session : l'identité manifeste
    /// d'un point d'entrée est son chemin thème-relatif réel (`entry`),
    /// jamais le nom symbolique `theme.toml` de la capacité suffixé
    /// `.js`. Nom de capacité délibérément SANS RAPPORT avec le nom de
    /// fichier pour rendre le test sans ambiguïté possible : si le code
    /// utilisait encore `format!("{name}.js")`, il chercherait
    /// "the-map-feature.js" et échouerait, jamais "scripts/map.js".
    #[test]
    fn capability_manifest_key_is_entry_path_not_capability_name() {
        let (base, manifest_dir) = sandbox("entry-path-key");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.the-map-feature]
entry = "scripts/map.js"
markers = [".map"]
activation = "bootstrap"
content_driven = true
"#,
        );
        write_registry(&base, "the-map-feature = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].1.url, "/scripts/map.HASH.js",
            "la résolution doit trouver l'entrée sous 'scripts/map.js', jamais \
             'the-map-feature.js'"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// `deps` absente : comportement STRICTEMENT identique à avant son
    /// introduction — `#[serde(default)]` garantit une désérialisation
    /// réussie, et `CapabilityInfo.deps` reste simplement vide.
    #[test]
    fn deps_absent_is_backward_compatible() {
        let (base, manifest_dir) = sandbox("absent");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = [".map"]
activation = "bootstrap"
content_driven = true
"#,
        );
        write_registry(&base, "map = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].1.deps.is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    /// Contrat central de la résolution AOT : une `deps` absente du
    /// manifeste est un échec DUR, jamais un repli silencieux vers une
    /// capacité sans dépendance.
    #[test]
    fn deps_missing_from_manifest_is_a_hard_error_never_silent() {
        let (base, manifest_dir) = sandbox("missing");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = [".map"]
activation = "bootstrap"
content_driven = true
deps = ["libraries/deckgl/deckgl.js"]
"#,
        );
        write_registry(&base, "map = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));
        // deckgl.js volontairement absent du manifeste.

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new());
        assert!(
            result.is_err(),
            "une deps absente du manifeste doit faire échouer le build, jamais un repli silencieux"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Même convention de slash de tête optionnel que côté JS
    /// (`scripts.rs` §3.2, `ExternalAsset`) — jamais une résolution
    /// différente selon que l'intégrateur l'a écrit ou non. Vérifie aussi
    /// que `classic_scripts` est consultée avec la même clé NORMALISÉE
    /// (sans slash de tête), jamais la forme brute écrite par
    /// l'intégrateur.
    #[test]
    fn deps_leading_slash_is_tolerated_including_for_classic_scripts_lookup() {
        let (base, manifest_dir) = sandbox("leading-slash");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = [".map"]
activation = "bootstrap"
content_driven = true
deps = ["/libraries/deckgl/deckgl.js"]
"#,
        );
        write_registry(&base, "map = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));
        assets.insert(
            "libraries/deckgl/deckgl.js".to_string(),
            asset("/libraries/deckgl.HASH.js"),
        );
        // classic_scripts stocke la forme SANS slash de tête (celle produite
        // par CanonicalAssetId côté marius-assets) — jamais celle,
        // éventuellement préfixée, écrite par l'intégrateur dans `deps`.
        let classic_scripts = classic(&["libraries/deckgl/deckgl.js"]);

        let result = validate_capabilities(&manifest_dir, &assets, &classic_scripts).unwrap();
        assert_eq!(
            result[0].1.deps[0].canonical_key,
            "libraries/deckgl/deckgl.js"
        );
        assert!(!result[0].1.deps[0].module);

        let _ = fs::remove_dir_all(&base);
    }

    /// Breaking change de session : un marker bare (`"map"`, sans préfixe)
    /// n'est plus une classe implicite — il DOIT parser comme `Element` et
    /// valider avec succès end-to-end, jamais échouer et jamais retomber
    /// sur `Class`. Non-régression directe de la décision « aucune
    /// rétrocompatibilité de la syntaxe historique ».
    #[test]
    fn bare_marker_validates_as_element_never_as_implicit_class() {
        let (base, manifest_dir) = sandbox("bare-marker-element");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = ["map"]
activation = "bootstrap"
content_driven = true
"#,
        );
        write_registry(&base, "map = 2\n");

        let mut assets = HashMap::new();
        assets.insert("scripts/map.js".to_string(), asset("/scripts/map.HASH.js"));

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].1.markers,
            vec![MarkerPredicate::Element("map".to_string())],
            "un marker bare doit toujours devenir Element, jamais Class par défaut implicite"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Les quatre formes reconnues doivent toutes parser correctement
    /// lorsqu'elles proviennent réellement d'un `theme.toml` désérialisé
    /// (pas seulement via `parse_marker` appelé directement en unitaire) —
    /// verrouille l'intégration TOML → validate_capabilities →
    /// MarkerPredicate de bout en bout.
    #[test]
    fn all_four_marker_forms_parse_end_to_end() {
        let (base, manifest_dir) = sandbox("four-forms");
        write_theme_toml(
            &base,
            r##"
[scripts.capabilities.widget]
entry = "scripts/widget.js"
markers = [".tabs", "#menu", "[data-component]", "main"]
activation = "boot"
content_driven = true
"##,
        );
        write_registry(&base, "widget = 1\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/widget.js".to_string(),
            asset("/scripts/widget.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(
            result[0].1.markers,
            vec![
                MarkerPredicate::Class("tabs".to_string()),
                MarkerPredicate::Id("menu".to_string()),
                MarkerPredicate::Attribute("data-component".to_string()),
                MarkerPredicate::Element("main".to_string()),
            ]
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Un marker malformé (valeur d'attribut, hors périmètre) doit faire
    /// échouer `validate_capabilities` de bout en bout — jamais un repli
    /// silencieux vers une capacité sans ce marker, ni une interprétation
    /// partielle.
    #[test]
    fn attribute_marker_with_value_is_a_hard_error_end_to_end() {
        let (base, manifest_dir) = sandbox("attr-value-rejected");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.gallery]
entry = "scripts/gallery.js"
markers = ["[data-component=\"gallery\"]"]
activation = "boot"
content_driven = true
"#,
        );
        write_registry(&base, "gallery = 1\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/gallery.js".to_string(),
            asset("/scripts/gallery.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new());
        assert!(
            result.is_err(),
            "un marker [data-*=\"valeur\"] doit faire échouer le build — présence uniquement, \
             jamais de comparaison de valeur"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// `content_driven` absent du TOML → défaut `false` (`#[serde(default)]`)
    /// — aucune entrée registry requise, `scripts_registry.lock` peut même
    /// être absent du disque. Cas `navigation`/`scroll-to-top` du chantier
    /// de découplage.
    #[test]
    fn content_driven_defaults_to_false_and_needs_no_registry_file() {
        let (base, manifest_dir) = sandbox("no-registry-needed");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.navigation]
entry = "scripts/navigation.js"
markers = [".cmd-nav"]
activation = "initNavigation"
"#,
        );
        // Aucun write_registry() ici — le fichier n'existe pas du tout sur
        // le disque, et ça ne doit PAS être une erreur.

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/navigation.js".to_string(),
            asset("/scripts/navigation.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].1.bit, None,
            "content_driven = false (défaut) → bit toujours None, jamais résolu"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// `content_driven = true` sans entrée registry correspondante → erreur
    /// dure, quel que soit le contenu par ailleurs valide de la capacité.
    #[test]
    fn content_driven_true_without_registry_entry_is_a_hard_error() {
        let (base, manifest_dir) = sandbox("content-driven-missing-bit");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.image-focus]
entry = "scripts/image-focus.js"
markers = [".figure-image-focus"]
activation = "initImageFocus"
content_driven = true
"#,
        );
        // Registre présent mais sans l'entrée attendue.
        write_registry(&base, "unrelated-capability = 1\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/image-focus.js".to_string(),
            asset("/scripts/image-focus.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new());
        assert!(
            result.is_err(),
            "content_driven = true sans bit attribué doit échouer — attribution manquante"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// `content_driven = false` (ou omis) AVEC une entrée registry existante
    /// pour cette même capacité → erreur dure — invariant symétrique
    /// nouvellement introduit par le découplage : une capacité non
    /// content-driven ne doit jamais posséder de bit.
    #[test]
    fn content_driven_false_with_existing_registry_entry_is_a_hard_error() {
        let (base, manifest_dir) = sandbox("false-with-orphan-bit");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.navigation]
entry = "scripts/navigation.js"
markers = [".cmd-nav"]
activation = "initNavigation"
"#,
        );
        // Entrée présente pour "navigation" alors que la capacité est
        // content_driven = false (par défaut, omis ici).
        write_registry(&base, "navigation = 1\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/navigation.js".to_string(),
            asset("/scripts/navigation.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new());
        assert!(
            result.is_err(),
            "une capacité content_driven = false ne doit jamais posséder d'entrée registry, \
             même si le bit qu'elle porterait serait par ailleurs valide"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Une entrée registry sans AUCUNE capacité correspondante dans
    /// `theme.toml` reste une erreur — comportement inchangé par le
    /// découplage (invariant 2, cas "aucune capacité de ce nom").
    #[test]
    fn registry_entry_without_any_matching_capability_is_a_hard_error() {
        let (base, manifest_dir) = sandbox("orphan-registry-entry");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.image-focus]
entry = "scripts/image-focus.js"
markers = [".figure-image-focus"]
activation = "initImageFocus"
content_driven = true
"#,
        );
        write_registry(&base, "image-focus = 1\nghost-capability = 2\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/image-focus.js".to_string(),
            asset("/scripts/image-focus.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new());
        assert!(
            result.is_err(),
            "un bit actif sans aucune capacité déclarée du même nom doit échouer"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Deux capacités coexistent : l'une `content_driven = true` (bit
    /// résolu), l'autre `content_driven = false` (bit `None`) — vérifie
    /// que la bijection scope correctement chacune sans interférence
    /// croisée, et que `scripts_registry.lock` peut être partiellement
    /// peuplé (une seule des deux capacités y figure).
    #[test]
    fn mixed_content_driven_and_static_only_capabilities_coexist() {
        let (base, manifest_dir) = sandbox("mixed-capabilities");
        write_theme_toml(
            &base,
            r#"
[scripts.capabilities.navigation]
entry = "scripts/navigation.js"
markers = [".cmd-nav"]
activation = "initNavigation"

[scripts.capabilities.image-focus]
entry = "scripts/image-focus.js"
markers = [".figure-image-focus"]
activation = "initImageFocus"
content_driven = true
"#,
        );
        // Uniquement "image-focus" — "navigation" n'y figure jamais.
        write_registry(&base, "image-focus = 1\n");

        let mut assets = HashMap::new();
        assets.insert(
            "scripts/navigation.js".to_string(),
            asset("/scripts/navigation.HASH.js"),
        );
        assets.insert(
            "scripts/image-focus.js".to_string(),
            asset("/scripts/image-focus.HASH.js"),
        );

        let result = validate_capabilities(&manifest_dir, &assets, &HashSet::new()).unwrap();
        assert_eq!(result.len(), 2);
        let by_name: HashMap<&str, &CapabilityInfo> =
            result.iter().map(|(n, c)| (n.as_str(), c)).collect();
        assert_eq!(by_name["navigation"].bit, None);
        assert_eq!(by_name["image-focus"].bit, Some(1));

        let _ = fs::remove_dir_all(&base);
    }
}
