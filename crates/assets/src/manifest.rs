// crates/assets/src/manifest.rs

//! Utilitaires et structures partagés du compilateur d'assets.
//!
//! Centralise les composants transversaux non-spécifiques à un pipeline :
//! manifeste (E/S), registre d'URLs, hachage (BLAKE3), table MIME et normalisation de chemins.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

/// Manifeste d'assets sérialisé en `manifest.toml`.
///
/// ## Contrat de Sérialisation (Phase de Build)
///
/// Forme figée en accord avec `crates/core/schema/build.rs`.
/// La structure sérialisée produit un dictionnaire `[assets."clé"]` pour garantir
/// un accès $O(1)$ côté lecteur, par opposition à une liste de tables `[[asset]]`.
#[derive(Serialize)]
pub(crate) struct AssetManifest {
    pub(crate) assets: HashMap<String, AssetEntry>,
}

/// Entrée individuelle d'un asset dans le manifeste.
///
/// **Invariant de rupture :** Les noms de champs doivent rester strictement identiques
/// (caractère par caractère) à la struct `AssetEntry` lue par `build.rs`.
/// Toute divergence cassera la désérialisation TOML silencieusement (sans erreur de compilation).
#[derive(Serialize)]
pub(crate) struct AssetEntry {
    /// URL publique versionnée (ex: `/static/image.a81f9.png`).
    /// Servie telle quelle par le Shell et gravée par `fragment-forge`.
    /// **Toujours préfixée `/`, toujours en slashes avant.**
    pub(crate) url: String,
    /// Chemin physique du fichier produit, relatif à la racine du workspace.
    /// (Résolution par CWD au runtime, cf. `guide-cycle-de-vie-runtime.md` §4).
    pub(crate) path: String,
    pub(crate) mime: String,
    pub(crate) size: u64,
    /// Empreinte BLAKE3 complète (64 caractères hex) pour validation d'intégrité stricte.
    pub(crate) hash: String,
    pub(crate) version: String,
}

/// Registre de résolution des URLs publiques (`url()`).
///
/// ## Ordonnancement & Rôle
///
/// Construit par le pipeline `[static.verbatim]` **avant** l'exécution du pipeline `[styles]`.
/// Sert de source de vérité pour valider et réécrire toutes les directives `url()`
/// du CSS (polices, images de fond). Si une ressource est absente, le build CSS échoue.
///
/// ## Limite Architecturale Connue (Collisions)
///
/// La clé de résolution est le **nom de fichier seul** (héritage de la conception originelle
/// des polices), pas le chemin relatif complet. Deux fichiers homonymes dans des sous-dossiers
/// distincts provoqueront un écrasement silencieux (la dernière écriture gagne).
pub(crate) type AssetUrlRegistry = HashMap<String, String>;

/// Résolution MIME statique ($O(1)$).
///
/// Applique le principe de *Mechanical Sympathy* : aucune crate de détection générique n'est
/// nécessaire. L'ensemble des extensions gérées par la spécification v1 est fermé et connu AOT.
pub(crate) fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "css" => "text/css",
        "js" => "application/javascript",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "ico" => "image/vnd.microsoft.icon",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        // Requis par le W3C (spécifique au pipeline [webmanifest]), pas "application/json".
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Hachage cryptographique du contenu (BLAKE3).
///
/// Retourne un tuple `(hash_complet, suffixe_court)` :
/// - `hash_complet` (64 chars) : Destiné au manifeste pour l'intégrité.
/// - `suffixe_court` (5 chars) : Destiné au cache-busting dans les noms de fichiers (ex: `main.a81f9.css`).
pub(crate) fn hash_content(bytes: &[u8]) -> (String, String) {
    let full_hex = blake3::hash(bytes).to_hex().to_string();
    let short = full_hex[..5].to_string();
    (full_hex, short)
}

/// Convertit un `Path` en chaîne de caractères avec slashes forcés (`/`).
///
/// **Invariant de stabilité :** Garantit que les URLs et les chemins inscrits dans le manifeste
/// restent strictement identiques indépendamment du système d'exploitation de l'hôte exécutant le build.
pub(crate) fn path_to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Concatène deux segments en forçant un séparateur slash (`/`).
pub(crate) fn join_slash(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}
