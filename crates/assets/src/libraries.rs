// crates/assets/src/libraries.rs

//! Découverte `[libraries.*]` — SPEC-canonical-asset-identity.md §6.
//!
//! Mécanisme d'AUTORISATION, jamais un second pipeline d'assets : ce
//! module ne fait rien d'autre que walker un sous-arbre du thème et
//! retourner une liste de chemins theme-relatifs. Cette liste est ensuite
//! consommée par `run_verbatim_pipeline` (`verbatim.rs`) exactement comme
//! `[static.verbatim].files` — aucune bifurcation de traitement après ce
//! point, aucun type d'identité ni namespace propre aux bibliothèques
//! (§9).

use std::fs;
use std::path::Path;

/// Découvre récursivement tous les fichiers sous `theme_dir/root`.
///
/// Déterministe : même arborescence physique ⇒ même ensemble de chemins,
/// dans le même ordre, à chaque appel — trié explicitement, jamais
/// dépendant de l'ordre de retour du système de fichiers (`read_dir` ne
/// garantit aucun ordre).
///
/// Retourne des chemins THEME-RELATIFS (`libraries/<nom>/...`), dans la
/// même forme que `[static.verbatim].files` — directement concaténables à
/// cette liste avant l'appel à `run_verbatim_pipeline`, sans transformation
/// supplémentaire.
pub(crate) fn discover_library_files(
    theme_dir: &Path,
    root: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let root_abs = theme_dir.join(root);
    if !root_abs.is_dir() {
        return Err(format!(
            "libraries : root '{root}' introuvable ou n'est pas un répertoire ({})",
            root_abs.display()
        )
        .into());
    }

    let mut found = Vec::new();
    walk_recursive(theme_dir, &root_abs, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk_recursive(
    theme_dir: &Path,
    dir: &Path,
    found: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("libraries : lecture de {} impossible : {e}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            walk_recursive(theme_dir, &path, found)?;
        } else if file_type.is_file() {
            let rel = path.strip_prefix(theme_dir).map_err(|_| {
                format!(
                    "libraries : {} n'est pas sous la racine du thème {}",
                    path.display(),
                    theme_dir.display()
                )
            })?;
            found.push(crate::manifest::path_to_slash(rel));
        }
        // Symlinks, sockets, etc. : ignorés silencieusement — jamais
        // rencontrés en pratique pour une bibliothèque vendoriée, pas la
        // peine d'ajouter une branche d'erreur pour un cas qui n'arrive
        // jamais.
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::manifest::{AssetEntry, CanonicalAssetId};
    use crate::verbatim::run_verbatim_pipeline;

    fn sandbox(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "marius-assets-test-libraries-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn discovers_all_files_recursively_sorted() {
        let base = sandbox("discover");
        let theme_dir = base.join("theme");
        fs::create_dir_all(theme_dir.join("libraries/deck-gl/images")).unwrap();
        fs::write(theme_dir.join("libraries/deck-gl/deck.js"), b"js").unwrap();
        fs::write(theme_dir.join("libraries/deck-gl/deck.css"), b"css").unwrap();
        fs::write(theme_dir.join("libraries/deck-gl/images/icon.png"), b"png").unwrap();

        let mut found = discover_library_files(&theme_dir, "libraries/deck-gl").unwrap();
        found.sort();

        assert_eq!(
            found,
            vec![
                "libraries/deck-gl/deck.css".to_string(),
                "libraries/deck-gl/deck.js".to_string(),
                "libraries/deck-gl/images/icon.png".to_string(),
            ]
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_root_is_a_hard_error() {
        let base = sandbox("missing-root");
        let theme_dir = base.join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let result = discover_library_files(&theme_dir, "libraries/does-not-exist");
        assert!(result.is_err());

        let _ = fs::remove_dir_all(&base);
    }

    /// SPEC §6 bout en bout — la découverte alimente directement le
    /// pipeline verbatim existant, aucun traitement distinct : chaque
    /// fichier reçoit un CanonicalAssetId correct, aucun fichier hors
    /// `root` déclaré n'est jamais inclus.
    #[test]
    fn discovered_files_flow_through_existing_verbatim_pipeline_unmodified() {
        let base = sandbox("e2e");
        let theme_dir = base.join("theme");
        let build_root = base.join("build");
        fs::create_dir_all(theme_dir.join("libraries/deck-gl")).unwrap();
        fs::create_dir_all(theme_dir.join("other-untouched-dir")).unwrap();
        fs::create_dir_all(&build_root).unwrap();
        fs::write(theme_dir.join("libraries/deck-gl/deck.js"), b"content").unwrap();
        // Fichier HORS root déclaré — ne doit jamais entrer dans le build.
        fs::write(theme_dir.join("other-untouched-dir/secret.js"), b"nope").unwrap();

        let discovered = discover_library_files(&theme_dir, "libraries/deck-gl").unwrap();

        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let registry = run_verbatim_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &discovered,
            &mut manifest,
        )
        .unwrap();

        let key =
            CanonicalAssetId::from_theme_relative_path(Path::new("libraries/deck-gl/deck.js"));
        assert!(registry.contains_key(&key));
        assert!(manifest.contains_key("libraries/deck-gl/deck.js"));
        assert!(
            !manifest.contains_key("other-untouched-dir/secret.js"),
            "un fichier hors du root déclaré ne doit jamais entrer dans le build"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
