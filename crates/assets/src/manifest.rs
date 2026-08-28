// crates/assets/src/manifest.rs

//! Utilitaires et structures partagés du compilateur d'assets.
//!
//! Centralise les composants transversaux non-spécifiques à un pipeline :
//! manifeste (E/S), registre d'URLs, hachage (BLAKE3), table MIME et normalisation de chemins.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

/// Identité canonique d'un asset — SPEC-canonical-asset-identity.md §1.
///
/// Chemin relatif à la racine du thème, slashes forcés, sans segment `.`
/// ni `..` résiduel, jamais de slash de tête. Remplace `file_name()` comme
/// mécanisme d'identité/résolution (§4) : deux fichiers physiques
/// distincts ne peuvent jamais produire le même `CanonicalAssetId`.
///
/// Type distinct (`newtype`) plutôt qu'un `String` nu — empêche qu'une
/// référence brute, non canonicalisée, soit passée par erreur là où une
/// identité déjà résolue est attendue (confusion de niveau que le compilateur
/// rejette maintenant, jamais un contrat purement documentaire).
///
/// Ne porte aucune notion de provenance (thème/bibliothèque, §9) : un
/// asset de bibliothèque et un asset du thème partagent exactement ce
/// type, leur seule différence est la manière dont ils ont été autorisés
/// à entrer dans le build (`[static.verbatim].files` énumère,
/// `[libraries.*].root` découvre) — jamais une propriété de l'identité
/// elle-même.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct CanonicalAssetId(String);

impl CanonicalAssetId {
    /// Construit un `CanonicalAssetId` à partir d'un chemin DÉJÀ canonique
    /// — segments réels du système de fichiers, relatifs à la racine du
    /// thème. N'effectue AUCUNE résolution de specifier utilisateur (pas
    /// de jonction avec une origine, pas de normalisation `.`/`..` au-delà
    /// de ce que `path_to_slash` fait déjà sur un chemin propre) : c'est
    /// `canonicalize_reference` (resolve.rs) qui porte cette responsabilité
    /// pour une référence BRUTE. Ce constructeur sert les appelants qui
    /// connaissent déjà un chemin réel (verbatim, découverte de
    /// bibliothèque, module JS de l'arène).
    pub(crate) fn from_theme_relative_path(p: &Path) -> Self {
        Self(path_to_slash(p))
    }

    /// Construit directement depuis une chaîne déjà canonique — utilisé
    /// par `canonicalize_reference` une fois la normalisation effectuée.
    /// `pub(crate)` comme le reste : jamais construit hors de ce crate
    /// sans passer par l'une des deux voies de canonicalisation connues.
    pub(crate) fn from_canonical_string(s: String) -> Self {
        Self(s)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for CanonicalAssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

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
    /// Mode de chargement natif de l'asset, pertinent uniquement pour du
    /// JS consommé via `[scripts.capabilities.*].deps` : `true` (défaut,
    /// Marius est ESM-first) → `<script type="module">` ; `false` →
    /// `<script defer>` classique, seule concession explicite pour une
    /// bibliothèque vendorée qui reste distribuée en UMD (`[libraries.*].
    /// module = false`). Descripteur de l'ASSET tel que produit — au même
    /// titre que `mime`, jamais une option de rendu recalculée ailleurs —
    /// pas une donnée de configuration : sa valeur est déjà entièrement
    /// déterminée à l'écriture de cette entrée, jamais relue depuis
    /// `theme.toml` en aval (`build.rs` ne connaît que ce manifeste).
    /// Inerte (toujours `true`) pour toute entrée non consommée par
    /// `deps` — CSS, sprites, webmanifest, composants/capacités JS
    /// propres au thème (systématiquement modules par construction).
    pub(crate) module: bool,
}

/// Registre de résolution des URLs publiques (`url()`).
///
/// ## Ordonnancement & Rôle
///
/// Construit par le pipeline `[static.verbatim]` (et, depuis
/// SPEC-canonical-asset-identity.md, par la découverte `[libraries.*]` —
/// même pipeline, mêmes invariants, aucune bifurcation de traitement)
/// **avant** l'exécution des pipelines `[styles]`/`[scripts]`. Sert de
/// source de vérité pour résoudre toute référence canonicalisée
/// (`CanonicalAssetId`) vers son URL publique hachée.
///
/// ## Invariant d'identité (SPEC-canonical-asset-identity.md §5)
///
/// La clé est un `CanonicalAssetId` — chemin canonique complet relatif à
/// la racine du thème, **jamais** un nom de fichier seul. Deux fichiers de
/// même nom sous des répertoires distincts (deux bibliothèques, ou une
/// bibliothèque et le thème) coexistent sans collision : c'est précisément
/// la limite de l'ancien modèle (`file_name()` seul) que cette identité
/// élimine, pas un héritage à documenter comme acceptable.
pub(crate) type AssetUrlRegistry = HashMap<CanonicalAssetId, String>;

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
