// crates/assets/src/webmanifest.rs

//! Pipeline `[webmanifest]` (Phase 6).
//!
//! Mutation ciblée d'un arbre JSON générique. Seul le nœud `icons[].src`
//! est altéré (réécriture d'URL pour le cache-busting). L'intégralité du reste
//! du document W3C est projetée de l'entrée vers la sortie de manière invariante.
//!
//! ## Ordonnancement & Dépendances AOT
//!
//! - **Couplage Minimal :** Ce pipeline s'appuie exclusivement sur l'`AssetUrlRegistry`
//!   (déjà résolu par `[static.verbatim]`).
//! - **Parallélisme Logique :** Son graphe d'exécution est strictement orthogonal aux
//!   pipelines `[sprites]` et `[styles]`, permettant un ordonnancement libre vis-à-vis d'eux.
//!
//! ## Invariants & Décision Architecturale : Arbre Générique vs Mapping Strict
//!
//! L'implémentation utilise `serde_json::Value` et rejette délibérément l'usage
//! d'une structure fortement typée avec `#[serde(flatten)]`.
//!
//! - **Schéma Ouvert :** Le standard Web App Manifest est une grammaire ouverte et
//!   évolutive (clés propriétaires, spécifications futures comme `share_target` ou `protocol_handlers`).
//! - **Zéro Destruction :** Un typage strict d'un format ouvert risque la perte ou le
//!   réordonnancement silencieux des champs non mappés. `Value` opère comme un conteneur
//!   agnostique : il ne fait aucune hypothèse sur le format au-delà du nœud explicitement muté.
//! - **Contraste de Domaine :** Cette approche est asymétrique avec le traitement
//!   de `theme.toml` dans la base de code, dont la grammaire est fermée et strictement
//!   définie par notre domaine, justifiant alors un layout de données typé (*Struct of Arrays* ou équivalent).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::config::WebManifestConfig;
use crate::manifest::{AssetEntry, AssetUrlRegistry, hash_content, join_slash, mime_for_extension};
use crate::resolve::{ReferenceOrigin, resolve_asset_reference};

/// Erreur survenant lors de la lecture, la mutation ou la sérialisation du manifeste W3C.
///
/// Remonte un échec dur si la résolution d'une icône locale échoue contre le registre AOT.
#[derive(Debug)]
pub(crate) struct WebManifestError(String);

impl fmt::Display for WebManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetNotFound (webmanifest icons[].src) : {}", self.0)
    }
}

impl std::error::Error for WebManifestError {}

pub(crate) fn run_webmanifest_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    config: &WebManifestConfig,
    asset_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_path = theme_dir.join(&config.entry);
    let source_text = fs::read_to_string(&source_path).map_err(|e| {
        format!(
            "webmanifest : lecture impossible de {} : {e}",
            source_path.display()
        )
    })?;

    let mut document: Value = serde_json::from_str(&source_text).map_err(|e| {
        format!(
            "webmanifest : JSON invalide dans {} : {e}",
            source_path.display()
        )
    })?;

    // Mutation ciblée : SEUL `icons[].src` est touché. `document["icons"]`
    // absent ou de forme inattendue n'est pas une erreur — un manifeste
    // sans `icons` (ou avec une forme que ce pipeline ne reconnaît pas)
    // traverse simplement intact, aucune icône à résoudre.
    if let Some(icons) = document.get_mut("icons").and_then(Value::as_array_mut) {
        for icon in icons.iter_mut() {
            let Some(src) = icon.get("src").and_then(Value::as_str) else {
                continue;
            };

            match resolve_asset_reference(
                src,
                ReferenceOrigin::RelativeToThemeRoot,
                asset_url_registry,
            ) {
                Ok(Some(resolved)) => {
                    icon["src"] = Value::String(resolved);
                }
                Ok(None) => {
                    // Externe ou fragment pur — rare pour une icône PWA,
                    // mais le W3C ne l'interdit pas (ex. icône hébergée
                    // sur un CDN dédié). Laissé strictement inchangé.
                }
                Err(filename) => return Err(Box::new(WebManifestError(filename))),
            }
        }
    }

    // Re-sérialisation : la mise en forme (indentation, ordre des clés)
    // n'est PAS préservée à l'octet près — `serde_json` réémet sa propre
    // forme canonique. Sans conséquence : c'est un document JSON, pas un
    // CSS où l'ordre des règles a une sémantique de cascade. Compact
    // (`to_string`, pas `to_string_pretty`) : ce fichier est servi au
    // runtime, jamais lu par un humain une fois buildé.
    let serialized = serde_json::to_string(&document).map_err(|e| {
        format!(
            "webmanifest : sérialisation échouée pour {} : {e}",
            source_path.display()
        )
    })?;
    let bytes = serialized.as_bytes();
    let (full_hash, short_hash) = hash_content(bytes);

    let extension = Path::new(&config.entry)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("webmanifest");
    let hashed_filename = format!("manifest.{short_hash}.{extension}");
    let output_abs = build_root.join(&hashed_filename);
    fs::write(&output_abs, bytes)?;

    // Clé logique fixe `manifest.webmanifest`, indépendante du nom de
    // fichier source réel (`config.entry`) — c'est cette clé stable que
    // `<link rel="manifest" href="{% asset manifest.webmanifest %}">`
    // référencera côté template, jamais le nom de fichier source, qui
    // peut changer sans casser les templates.
    manifest.insert(
        "manifest.webmanifest".to_string(),
        AssetEntry {
            url: format!("/{hashed_filename}"),
            path: join_slash(build_root_rel, &hashed_filename),
            mime: mime_for_extension(extension).to_string(),
            size: bytes.len() as u64,
            hash: full_hash,
            version: String::new(), // rempli par l'appelant (theme.version)
            // JSON — jamais consommé via `deps`, champ inerte.
            module: true,
        },
    );

    println!(
        "[marius-assets] webmanifest {} -> /{hashed_filename}",
        config.entry
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CanonicalAssetId;

    // ── run_webmanifest_pipeline (Phase 6) ───────────────────────────────────
    //
    // Seuls tests de ce fichier à toucher le disque — nécessaire ici : la
    // propriété centrale à prouver (non-destruction du reste du document,
    // spec §3 de la mission) ne peut pas se vérifier sur une fonction pure,
    // `run_webmanifest_pipeline` lit et écrit réellement des fichiers.
    // Chaque test utilise un sous-répertoire de nom unique sous le
    // répertoire temporaire du système, pour rester sûr en cas
    // d'exécution parallèle des tests (le harnais `cargo test` parallélise
    // par défaut).

    #[test]
    fn run_webmanifest_pipeline_rewrites_icons_and_preserves_rest_of_document() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-webmanifest-ok");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("manifest.webmanifest"),
            r##"{
                "name": "Marius",
                "theme_color": "#ff6347",
                "icons": [
                    { "src": "/favicons/logoAny.svg", "sizes": "any", "type": "image/svg+xml" },
                    { "src": "/favicons/logo192.png", "sizes": "192x192", "type": "image/png" }
                ]
            }"##,
        )
        .unwrap();

        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            CanonicalAssetId::from_theme_relative_path(Path::new("favicons/logoAny.svg")),
            "/favicons/logoAny.12452.svg".to_string(),
        );
        registry.insert(
            CanonicalAssetId::from_theme_relative_path(Path::new("favicons/logo192.png")),
            "/favicons/logo192.53aea.png".to_string(),
        );

        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = WebManifestConfig {
            entry: "manifest.webmanifest".to_string(),
        };

        run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        )
        .unwrap();

        let entry = manifest
            .get("manifest.webmanifest")
            .expect("la clé logique manifest.webmanifest doit être enregistrée");

        let written_filename = Path::new(&entry.url).file_name().unwrap();
        let written = fs::read_to_string(build_root.join(written_filename)).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();

        // Non-destruction (spec §3 de la mission) : les clés hors icons[]
        // traversent intactes.
        assert_eq!(parsed["name"], "Marius");
        assert_eq!(parsed["theme_color"], "#ff6347");
        // Mutation ciblée : seul src est réécrit.
        assert_eq!(parsed["icons"][0]["src"], "/favicons/logoAny.12452.svg");
        assert_eq!(parsed["icons"][0]["sizes"], "any");
        assert_eq!(parsed["icons"][1]["src"], "/favicons/logo192.53aea.png");

        let _ = fs::remove_dir_all(&sandbox);
    }

    /// Fail-hard (spec §2 de la mission) : une icône absente du registre
    /// doit faire échouer tout le pipeline, jamais produire un manifeste
    /// avec une URL non versionnée ou une ressource orpheline.
    #[test]
    fn run_webmanifest_pipeline_fails_hard_on_missing_icon_asset() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-webmanifest-missing");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("manifest.webmanifest"),
            r#"{"icons": [{"src": "/favicons/ghost.png"}]}"#,
        )
        .unwrap();

        let registry = AssetUrlRegistry::new(); // vide : rien ne peut être trouvé
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = WebManifestConfig {
            entry: "manifest.webmanifest".to_string(),
        };

        let result = run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        );
        assert!(result.is_err());
        assert!(
            manifest.get("manifest.webmanifest").is_none(),
            "aucune entrée ne doit être enregistrée si la résolution échoue"
        );

        let _ = fs::remove_dir_all(&sandbox);
    }
}
