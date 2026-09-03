// crates/core/schema/build/manifest.rs

//! Chargement du manifeste d'assets AOT (`assets/build/manifest.toml`)
//! et résolution des chemins du thème actif.
//!
//! Toute l'I/O de découverte de chemins (`CARGO_MANIFEST_DIR` → racine du
//! thème / du build) vit ici — jamais recalculée ailleurs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

// =============================================================================
// Manifeste d'assets — marius-assets-specification.md §7, Roadmap §1.4 (clos).
//
// Structure miroir du TOML `[assets."clé"]` (dictionnaire, pas [[asset]]) —
// désérialisation directe en HashMap, lookup O(1) pour chaque {% asset %}
// rencontré. Décision actée : voir échange de session, format dictionnaire
// retenu explicitement au détriment du tableau pour cette raison.
//
// serde + toml : build-dependencies uniquement (Cargo.toml) — jamais liées
// au binaire du Shell ni du Core (no_std). Coût de parsing entièrement
// confiné à la machine hôte, phase AOT.
// =============================================================================

#[derive(Deserialize)]
struct AssetManifest {
    assets: HashMap<String, AssetEntry>,
    /// Miroir de `crates/assets/src/manifest.rs::AssetManifest.classic_scripts`
    /// — métadonnée du mécanisme de chargement de `deps`, jamais une
    /// propriété d'`AssetEntry` (voir doc-comment côté producteur pour la
    /// justification complète de cette séparation). Sparse : ne contient
    /// que les clés canoniques explicitement classiques/UMD.
    #[serde(default)]
    classic_scripts: Vec<String>,
}

/// Une entrée du manifeste — champs de la spec §7. Seuls `url` (et sa
/// longueur) sont consommés par ce build.rs ; `path`/`mime`/`size`/`hash`/
/// `version` sont ceux que le Shell consomme au runtime (`handlers.rs`),
/// partagés depuis le même fichier — producteur unique, spec §8.
///
/// Reste un pur descripteur d'artefact — jamais de champ propre à un
/// mécanisme de consommation particulier (voir `AssetManifest::
/// classic_scripts` pour où vit cette nuance pour `deps`).
#[derive(Deserialize)]
pub(crate) struct AssetEntry {
    pub(crate) url: String,
    #[allow(dead_code)]
    pub(crate) path: String,
    #[allow(dead_code)]
    pub(crate) mime: String,
    #[allow(dead_code)]
    pub(crate) size: u64,
    #[allow(dead_code)]
    pub(crate) hash: String,
    #[allow(dead_code)]
    pub(crate) version: String,
}

/// Nom du thème actif. Décision actée en session : un seul thème possible
/// pour cette v1 — pas de mécanisme de sélection (env var, section
/// Cargo.toml, configuration multi-thème) nécessaire tant que cet invariant
/// tient. Si un jour plusieurs thèmes coexistent, ce point redevient ouvert
/// et cette constante devra être remplacée par un paramètre réel — mais ce
/// n'est plus une inconnue pour la v1.
const THEME_NAME: &str = "default";

/// Répertoire de build du thème actif : `{workspace_root}/build/{theme}`,
/// où `workspace_root` = `CARGO_MANIFEST_DIR` (= `crates/core/schema`) +
/// trois remontées (`schema → core → crates → racine`) — PAS deux, piège
/// déjà documenté pour `manifest.toml` ci-dessous, désormais factorisé ici
/// pour que la page statique (§ plus bas) ne puisse pas le redupliquer
/// avec un nombre de remontées différent par accident de copier-coller.
pub(crate) fn build_dir(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .join("../../../build")
        .join(THEME_NAME)
}

/// Vue exploitable du manifeste, après désérialisation — sépare, dès la
/// sortie de ce module, les deux natures de données que
/// `manifest.toml` transporte : les artefacts eux-mêmes (`assets`) et la
/// métadonnée de chargement de scripts (`classic_scripts`), jamais
/// mélangées dans une même structure en aval non plus.
pub(crate) struct LoadedAssets {
    pub(crate) assets: HashMap<String, AssetEntry>,
    /// Converti en `HashSet` ici (une seule fois) : `validate_capabilities`
    /// fait un test d'appartenance par dépendance résolue, pas un parcours.
    pub(crate) classic_scripts: HashSet<String>,
}

/// Résout le chemin du manifeste d'assets et l'enregistre auprès de Cargo.
///
/// `cargo:rerun-if-changed` émis de façon INCONDITIONNELLE, avant tout test
/// d'existence, y compris sur le répertoire parent — piège documenté dans
/// `guide-cycle-de-vie-runtime.md` §2 : une émission conditionnelle ne
/// rattrape jamais un fichier qui apparaît après le premier build. Même
/// discipline que `resolve_template` (ligne ~316) pour les templates.
pub(crate) fn load_asset_manifest(manifest_dir: &str) -> Result<LoadedAssets, ()> {
    let manifest_path = build_dir(manifest_dir).join("manifest.toml");

    // Émission inconditionnelle — avant le test d'existence.
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    if let Some(parent_dir) = manifest_path.parent() {
        println!("cargo:rerun-if-changed={}", parent_dir.display());
    }

    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : manifeste d'assets introuvable ({}) : {e}",
            manifest_path.display()
        );
    })?;

    let parsed: AssetManifest = toml::from_str(&raw).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : manifeste d'assets malformé ({}) : {e}",
            manifest_path.display()
        );
    })?;

    Ok(LoadedAssets {
        assets: parsed.assets,
        classic_scripts: parsed.classic_scripts.into_iter().collect(),
    })
}

/// Répertoire source du thème (contient `theme.toml`) — symétrique de
/// `build_dir` ci-dessus (trois remontées identiques depuis
/// `CARGO_MANIFEST_DIR` = `crates/core/schema`), confirmé par le message
/// d'usage littéral de `marius-assets`
/// (`marius-assets <chemin-du-dossier-de-theme> (ex: ./assets/default)`) :
/// `workspace_root/assets/{THEME_NAME}`, jamais `workspace_root/build/...`
/// (qui est un répertoire ENTIÈREMENT généré, jamais une source).
pub(crate) fn theme_source_dir(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .join("../../../assets")
        .join(THEME_NAME)
}

/// Emplacement du registre de bits `scripts_registry.lock` — CONFIRMÉ en
/// session : `assets/{THEME_NAME}/scripts_registry.lock`, sibling de
/// `theme.toml` (même répertoire source, même statut de fichier manuel
/// versionné).
pub(crate) fn scripts_registry_path(manifest_dir: &str) -> PathBuf {
    theme_source_dir(manifest_dir).join("scripts_registry.lock")
}
