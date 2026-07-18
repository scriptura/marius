// crates/assets/src/manifest.rs
//
// Structures et utilitaires partagés par TOUS les pipelines : le manifeste
// (entrée/sortie), le registre d'URLs d'assets, le hachage de contenu, la
// table MIME et les utilitaires de chemin (slashes forcés). Rien ici n'est
// spécifique à un pipeline — c'est précisément le critère d'appartenance à
// ce fichier plutôt qu'à `styles.rs`/`scripts.rs`/etc.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

// =============================================================================
// manifest.toml — sérialisation de sortie
//
// Forme figée en session avec build.rs (crates/core/schema/build.rs) :
// dictionnaire `[assets."clé"]`, pas un tableau `[[asset]]` — lookup O(1)
// côté lecteur. Les noms de champs ci-dessous doivent rester
// caractère-pour-caractère identiques à la struct AssetEntry de build.rs :
// url, path, mime, size, hash, version. Toute divergence casse le lecteur
// sans erreur de compilation (désérialisation TOML silencieusement
// incomplète) — à ne jamais renommer d'un seul côté sans l'autre.
// =============================================================================

#[derive(Serialize)]
pub(crate) struct AssetManifest {
    pub(crate) assets: HashMap<String, AssetEntry>,
}

#[derive(Serialize)]
pub(crate) struct AssetEntry {
    /// URL publique versionnée, servie telle quelle par le Shell et gravée
    /// telle quelle par `generate_aot_snippet` (fragment-forge). Toujours
    /// préfixée `/`, toujours en slashes avant (jamais de séparateur OS).
    pub(crate) url: String,
    /// Chemin physique du fichier produit, relatif à la racine du workspace
    /// (même convention que les autres artéfacts Marius — cf.
    /// guide-cycle-de-vie-runtime.md §4, résolution par CWD).
    pub(crate) path: String,
    pub(crate) mime: String,
    pub(crate) size: u64,
    /// Empreinte BLAKE3 complète (64 caractères hex) — intégrité, distincte
    /// du suffixe court (5 caractères) utilisé dans le nom de fichier.
    pub(crate) hash: String,
    pub(crate) version: String,
}

// =============================================================================
// Registre des URLs d'assets — spec §10.1 + Roadmap §1.8 (désormais
// tranchée : tout `url()` du CSS est résolu, pas seulement `@font-face`).
// Deux exigences liées :
//  1. Le build CSS doit échouer si une ressource référencée par un `url()`
//     (que ce soit `@font-face`, `background-image`, ou autre) est absente
//     du registre effectivement copié par le pipeline verbatim.
//  2. Ce même registre sert de résolveur d'URL : le `url(...)` littéral
//     écrit par le développeur doit être réécrit vers l'URL publique
//     versionnée avant écriture du CSS final.
//
// Conséquence d'ordonnancement (spec, même §) : le pipeline verbatim doit
// avoir résolu ce registre AVANT que le pipeline styles ne s'exécute — d'où
// le passage explicite par valeur de retour, pas une variable globale ni un
// champ mutable partagé.
//
// Portée désormais élargie à TOUT [static.verbatim] (Phase 5 — c'était
// auparavant limité aux polices woff2/woff/ttf, cf. Handoff Phase 2 : un
// favicon n'a alors aucune raison d'être référencé par un `url()` CSS,
// mais une image de fond en a une, exactement le cas signalé en session).
//
// Clé = nom de fichier seul (pas le chemin complet), hérité tel quel de la
// conception Fonts d'origine — une collision entre deux fichiers homonymes
// dans des sous-dossiers différents de [static.verbatim] n'est pas
// détectée (dernière écriture gagne, silencieusement). Limitation
// préexistante, pas introduite par cette généralisation ; la corriger
// demanderait de résoudre par chemin relatif complet plutôt que par nom de
// fichier seul — portée plus large que ce qui a été demandé ici, à
// reprendre explicitement si une vraie collision se présente (même
// remarque que Roadmap §1.6 pour les SVG).
// =============================================================================
pub(crate) type AssetUrlRegistry = HashMap<String, String>;

// =============================================================================
// Table MIME — correspondance plate, pas de crate de detection générique
// (sympathie mécanique : un match statique suffit, l'ensemble des
// extensions gérées par la spec v1 est fermé et connu à l'avance).
// =============================================================================

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
        // W3C Web App Manifest — spec : "application/manifest+json", pas
        // "application/json" générique (Phase 6, [webmanifest]).
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

// =============================================================================
// Hachage — BLAKE3, 5 premiers caractères hex pour le suffixe de nom de
// fichier (convention déjà en production — cf. main.a81f9.css observé).
// Le hash complet (64 caractères) est conservé dans le manifeste (§7).
// =============================================================================

pub(crate) fn hash_content(bytes: &[u8]) -> (String, String) {
    let full_hex = blake3::hash(bytes).to_hex().to_string();
    let short = full_hex[..5].to_string();
    (full_hex, short)
}

// =============================================================================
// Utilitaires de chemin — slashes forcés, jamais de séparateur OS. Les URLs
// et les chemins écrits dans le manifeste doivent être stables quelle que
// soit la plateforme de build.
// =============================================================================

pub(crate) fn path_to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn join_slash(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}
