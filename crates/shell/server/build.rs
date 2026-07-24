// crates/shell/server/build.rs
//!
//! Maintien de l'isolation topologique (marius-assets-HANDOFF.md §1.10) :
//! le routage HTTP est une responsabilité Shell, disjointe de `crates/core/schema/`.
//! La double lecture du `manifest.toml` (déjà lu par `core/schema/build.rs`
//! pour `{% asset %}`) respecte le pattern "Single Producer" (spec §8) :
//! l'invariant de producteur unique contraint l'émission des données, pas leur
//! consommation par de multiples terminaux de build en aval.
//!
//! Pipeline AOT (Ahead-Of-Time) & Data Layout :
//! Génération de `ASSET_ROUTES`, table de routage indexée par URL publique
//! (projection inverse du manifeste, au lieu d'une indexation par ID logique).
//! Calculée à la compilation via une Perfect Hash Function (PHF) et figée
//! directement dans la section `.rodata` du binaire Shell.
//!
//! Caractéristiques physiques au runtime (DOD) :
//! - Lookup structuré en O(1) strict.
//! - Zéro allocation dynamique (absence d'indirection heap).
//! - Zéro cycle CPU d'initialisation (pas de construction de HashMap au démarrage,
//!   les données étant directement mappées en mémoire).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Deserialize;

/// Décision actée en session : un seul thème possible en v1 — voir la même
/// constante et la même justification dans crates/core/schema/build.rs.
const THEME_NAME: &str = "default";

#[derive(Deserialize)]
struct AssetManifest {
    assets: HashMap<String, AssetManifestEntry>,
}

#[derive(Deserialize)]
struct AssetManifestEntry {
    url: String,
    path: String,
    mime: String,
    size: u64,
    hash: String,
    #[allow(dead_code)]
    version: String,
}

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("shell/server : CARGO_MANIFEST_DIR non définie (toujours fournie par Cargo).");

    // Même profondeur que crates/core/schema/ (trois niveaux sous la racine
    // du workspace) : même nombre de remontées, même chemin final.
    let manifest_path = Path::new(&manifest_dir)
        .join("../../../build")
        .join(THEME_NAME)
        .join("manifest.toml");

    // Émission inconditionnelle — avant tout test d'existence. Piège déjà
    // documenté (guide-cycle-de-vie-runtime.md §2) : une émission
    // conditionnelle ne rattrape jamais un fichier qui apparaît après le
    // premier build.
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    if let Some(parent) = manifest_path.parent() {
        println!("cargo:rerun-if-changed={}", parent.display());
    }

    let raw = fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        println!(
            "cargo:error=shell/server : manifeste d'assets introuvable ({}) : {e}",
            manifest_path.display()
        );
        std::process::exit(1);
    });

    let parsed: AssetManifest = toml::from_str(&raw).unwrap_or_else(|e| {
        println!(
            "cargo:error=shell/server : manifeste d'assets malformé ({}) : {e}",
            manifest_path.display()
        );
        std::process::exit(1);
    });

    // Deux URLs ne peuvent jamais entrer en collision (le hash de contenu
    // les distingue par construction) — aucune détection de doublon requise
    // ici, à la différence de la clé "id logique" côté core/schema.
    let mut literals: Vec<(String, String)> = Vec::with_capacity(parsed.assets.len());
    for entry in parsed.assets.values() {
        let short_hash = &entry.hash[..entry.hash.len().min(5)];
        // Forme HTTP prête à l'emploi, guillemets inclus — cuite ici plutôt
        // qu'au moment de la requête : zéro formatage sur le chemin chaud.
        let etag_value = format!("\"{short_hash}\"");
        let literal = format!(
            "AssetRoute {{ path: {:?}, mime: {:?}, size: {}, etag: {:?} }}",
            entry.path, entry.mime, entry.size, etag_value
        );
        literals.push((entry.url.clone(), literal));
    }

    let mut builder: phf_codegen::Map<&str> = phf_codegen::Map::new();
    for (url, literal) in &literals {
        builder.entry(url.as_str(), literal.as_str());
    }

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR toujours fournie par Cargo.");
    let dest_path = Path::new(&out_dir).join("asset_routes.rs");
    let mut out = fs::File::create(&dest_path)
        .unwrap_or_else(|e| panic!("écriture de {} : {e}", dest_path.display()));

    writeln!(
        out,
        "pub static ASSET_ROUTES: phf::Map<&'static str, AssetRoute> = {};",
        builder.build()
    )
    .expect("écriture de asset_routes.rs");
}
