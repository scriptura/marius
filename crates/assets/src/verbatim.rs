// crates/assets/src/verbatim.rs

//! Pipeline `[static.verbatim]`.
//!
//! Pipeline *pass-through* strict : copie brute, génération d'empreinte AOT (hachage)
//! et enregistrement au manifeste.
//!
//! ## Invariants & Data Layout
//!
//! - **Zéro Transformation :** L'empreinte mémoire de la donnée source est projetée 1:1
//!   vers la cible. L'intégrité du flux d'octets est absolue ; seule la métadonnée
//!   (le nom de fichier) est mutée pour y injecter le hash BLAKE3.
//! - **Identité canonique (SPEC-canonical-asset-identity.md) :** la clé
//!   d'accès dans le registre (`AssetUrlRegistry`) et dans `manifest.toml`
//!   est le `CanonicalAssetId` du fichier — son chemin RELATIF COMPLET à
//!   la racine du thème, jamais son seul nom de fichier. Deux fichiers
//!   homonymes sous des répertoires distincts (deux bibliothèques
//!   `[libraries.*]`, ou une bibliothèque et le thème) coexistent sans
//!   collision — c'est précisément l'ancienne limite (« adressage plat »,
//!   nom de fichier seul) que cette identité élimine. Le chemin de SORTIE
//!   physique, lui, reste inchangé : mêmes segments de répertoire que la
//!   source, seul le nom de fichier gagne le hash — l'identité logique
//!   d'entrée et l'URL de sortie restent deux préoccupations distinctes.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::manifest::{
    AssetEntry, AssetUrlRegistry, CanonicalAssetId, hash_content, join_slash, mime_for_extension,
    path_to_slash,
};

/// Exécute le pipeline verbatim sur un répertoire source statique.
///
/// Parcourt la structure source de manière déterministe, calcule les empreintes,
/// duplique les buffers vers le point de montage de build, et retourne un registre
/// plat garantissant un accès direct aux URLs publiques.
pub(crate) fn run_verbatim_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    files: &[String],
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<AssetUrlRegistry, Box<dyn std::error::Error>> {
    let mut asset_url_registry = AssetUrlRegistry::new();

    for rel_path in files {
        let source_path = theme_dir.join(rel_path);
        let bytes = fs::read(&source_path).map_err(|e| {
            format!(
                "static.verbatim : lecture impossible de {} : {e}",
                source_path.display()
            )
        })?;

        let (full_hash, short_hash) = hash_content(&bytes);

        let rel = Path::new(rel_path);
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let stem = rel
            .file_stem()
            .ok_or_else(|| format!("static.verbatim : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy();
        let ext: String = rel
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();

        let hashed_filename = if ext.is_empty() {
            format!("{stem}.{short_hash}")
        } else {
            format!("{stem}.{short_hash}.{ext}")
        };

        // Sous-chemin de sortie relatif à build_root — mêmes segments de
        // répertoire que la source (favicons/, fonts/...), slashes forcés.
        let output_rel = join_slash(&path_to_slash(parent), &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, &bytes)?;

        let logical_key = CanonicalAssetId::from_theme_relative_path(rel);

        let url = format!("/{output_rel}");

        // Alimentation du registre d'URLs — désormais TOUT [static.verbatim]
        // (Phase 5) et toute découverte [libraries.*] (SPEC-canonical-
        // asset-identity.md §6, même pipeline, aucune bifurcation) : n'importe
        // quel fichier copié verbatim est potentiellement référencé par un
        // `url()` CSS, un import JS non-relatif, ou une icône webmanifest.
        asset_url_registry.insert(logical_key.clone(), url.clone());

        manifest.insert(
            logical_key.into_string(),
            AssetEntry {
                url,
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension(&ext).to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(), // rempli par l'appelant (theme.version)
            },
        );

        println!("[marius-assets] verbatim  {rel_path} -> /{output_rel}");
    }

    Ok(asset_url_registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "marius-assets-test-verbatim-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    /// SPEC-canonical-asset-identity.md §5 — la clé est désormais le
    /// chemin canonique complet, jamais file_name() seul.
    #[test]
    fn logical_key_is_full_canonical_path_not_basename() {
        let base = sandbox("canonical-key");
        let theme_dir = base.join("theme");
        let build_root = base.join("build");
        fs::create_dir_all(theme_dir.join("fonts")).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        fs::write(theme_dir.join("fonts/noto.woff2"), b"fake-font").unwrap();

        let mut manifest = HashMap::new();
        let registry = run_verbatim_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &["fonts/noto.woff2".to_string()],
            &mut manifest,
        )
        .unwrap();

        let key = CanonicalAssetId::from_theme_relative_path(Path::new("fonts/noto.woff2"));
        assert!(
            registry.contains_key(&key),
            "la clé doit être le chemin canonique complet 'fonts/noto.woff2', jamais 'noto.woff2' seul"
        );
        assert!(manifest.contains_key("fonts/noto.woff2"));

        let _ = fs::remove_dir_all(&base);
    }

    /// SPEC-canonical-asset-identity.md §11 — critère d'acceptation
    /// central : deux fichiers homonymes sous des répertoires distincts ne
    /// s'écrasent jamais. Aurait échoué sous l'ancien mécanisme
    /// file_name() (écrasement silencieux, dernière écriture gagne).
    #[test]
    fn no_silent_overwrite_between_same_basename_in_different_directories() {
        let base = sandbox("no-collision");
        let theme_dir = base.join("theme");
        let build_root = base.join("build");
        fs::create_dir_all(theme_dir.join("libraries/foo")).unwrap();
        fs::create_dir_all(theme_dir.join("libraries/bar")).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        fs::write(theme_dir.join("libraries/foo/index.js"), b"foo content").unwrap();
        fs::write(theme_dir.join("libraries/bar/index.js"), b"bar content").unwrap();

        let mut manifest = HashMap::new();
        let registry = run_verbatim_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &[
                "libraries/foo/index.js".to_string(),
                "libraries/bar/index.js".to_string(),
            ],
            &mut manifest,
        )
        .unwrap();

        let key_foo =
            CanonicalAssetId::from_theme_relative_path(Path::new("libraries/foo/index.js"));
        let key_bar =
            CanonicalAssetId::from_theme_relative_path(Path::new("libraries/bar/index.js"));

        assert_eq!(
            registry.len(),
            2,
            "les deux entrées doivent coexister, aucune écrasée"
        );
        assert_ne!(
            registry.get(&key_foo),
            registry.get(&key_bar),
            "les deux fichiers homonymes doivent produire des URLs hachées distinctes"
        );
        assert_eq!(manifest.len(), 2);

        let _ = fs::remove_dir_all(&base);
    }
}
