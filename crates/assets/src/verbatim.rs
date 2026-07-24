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
//! - **Adressage Logique Plat $O(1)$ :** La clé d'accès dans le registre (`AssetUrlRegistry`)
//!   correspond exclusivement au nom du fichier physique (ex: `notoSans-Regular.woff2`),
//!   ignorant toute l'arborescence relative. Cette topologie plate élimine les cycles CPU
//!   dédiés à la résolution de chemins au runtime et empêche structurellement le couplage
//!   des assets à une hiérarchie de dossiers arbitraire.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::manifest::{
    AssetEntry, AssetUrlRegistry, hash_content, join_slash, mime_for_extension, path_to_slash,
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

        let logical_key = rel
            .file_name()
            .ok_or_else(|| format!("static.verbatim : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy()
            .into_owned();

        let url = format!("/{output_rel}");

        // Alimentation du registre d'URLs — désormais TOUT [static.verbatim]
        // (Phase 5), pas seulement les extensions de police : n'importe quel
        // fichier copié verbatim est potentiellement référencé par un
        // `url()` CSS (background-image, favicon en CSS custom, etc.), pas
        // seulement les polices via `@font-face`.
        asset_url_registry.insert(logical_key.clone(), url.clone());

        manifest.insert(
            logical_key,
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
