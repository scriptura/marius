// crates/assets/src/main.rs

//! # Marius Assets — Compilateur AOT d'Assets Statiques
//!
//! Outil de build hôte exclusivement (aucune trace runtime dans le Shell ni le Core `no_std`).
//! Références : `marius-assets-specification.md` et `marius-assets-HANDOFF.md`.
//!
//! ## Invariants d'Exécution & Modèle Mémoire
//!
//! - **Profil Hôte vs Chemin Chaud :** Exécuté une seule fois à la compilation sur la machine hôte.
//!   Les allocations dynamiques (`String`, `Vec`, `HashMap`) y sont acceptées sans restriction
//!   (contrairement au chemin chaud du Shell / Core).
//! - **Discipline Data-Oriented :** Traitement séquentiel, passage unique par fichier,
//!   aucune structure de données hiérarchique, zéro indirection dynamique (`dyn Trait`).
//! - **Purge & Idempotence :** Le répertoire `build_root` est intégralement vidé au démarrage
//!   dans `main()` avant l'exécution des pipelines.
//!
//! ## Cartographie des Modules (`theme.toml`)
//!
//! | Module | Section `theme.toml` | Responsabilité |
//! | :--- | :--- | :--- |
//! | `config.rs` | Racine | Désérialisation du fichier de configuration `theme.toml`. |
//! | `manifest.rs` | Partagé | Gestion du manifeste (E/S), `AssetUrlRegistry`, hachage, MIME et utilitaires de chemin. |
//! | `resolve.rs` | Partagé | Résolution d'URL partagée (CSS, JS, SW, Webmanifest). |
//! | `verbatim.rs` | `[static.verbatim]` | Copie et transfert brut des actifs statiques. |
//! | `webmanifest.rs` | `[webmanifest]` | Pipeline de génération du Web App Manifest. |
//! | `sprites.rs` | `[sprites]` | Assemblage AOT des sprites. |
//! | `styles.rs` | `[styles]` | Transformation CSS (résolution de `$variables`, boucles `@for`, `url()` et LightningCSS). |
//! | `scripts.rs` | `[scripts.components]` | Lexer JS + arène ESM (Data-Oriented). |
//! | `js_minify.rs` | Partagé | Passe finale de minification `oxc` (partagée entre scripts et Service Worker). |
//! | `service_worker.rs` | `[service_worker]` | Assemblage du SW (réutilise le lexer de `scripts.rs`). |
//!
//! ## Encapsulation & Visibilité des Tests
//!
//! Les suites de tests (`#[cfg(test)] mod tests`) sont systématiquement co-localisées dans
//! chaque fichier de module. Cela évite d'exposer la visibilité en `pub(crate)` sur des
//! fonctions qui doivent rester privées à leur sous-système.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

mod config;
mod js_minify;
mod manifest;
mod resolve;
mod scripts;
mod service_worker;
mod sprites;
mod styles;
mod verbatim;
mod webmanifest;

use config::ThemeConfig;
use manifest::{AssetEntry, AssetManifest, AssetUrlRegistry, join_slash};
use scripts::run_scripts_pipeline;
use service_worker::run_service_worker_pipeline;
use sprites::run_sprites_pipeline;
use styles::run_styles_pipeline;
use verbatim::run_verbatim_pipeline;
use webmanifest::run_webmanifest_pipeline;

// =============================================================================
// main
// =============================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let theme_dir_arg = args
        .next()
        .ok_or("usage : marius-assets <chemin-du-dossier-de-theme> (ex: ./assets/default)")?;

    let theme_dir = PathBuf::from(&theme_dir_arg);
    if !theme_dir.is_dir() {
        return Err(format!(
            "dossier de thème introuvable ou invalide : {}",
            theme_dir.display()
        )
        .into());
    }

    let theme_toml_path = theme_dir.join("theme.toml");
    let raw_theme = fs::read_to_string(&theme_toml_path)
        .map_err(|e| format!("theme.toml introuvable dans {} : {e}", theme_dir.display()))?;
    let theme: ThemeConfig = toml::from_str(&raw_theme)
        .map_err(|e| format!("theme.toml malformé ({}) : {e}", theme_toml_path.display()))?;

    // Convention CWD-relative — même discipline que marius-dump/marius-verify
    // (guide-cycle-de-vie-runtime.md §4) : "build/" est résolu par rapport
    // au répertoire courant du processus au lancement, jamais via un
    // chemin absolu recalculé. Lancer ce binaire hors de la racine du
    // workspace produit un "build/" local, silencieusement — même piège,
    // même remède : toujours invoquer depuis la racine.
    let build_root_rel = join_slash("build", &theme.theme.name);
    let build_root = PathBuf::from(&build_root_rel);

    // Purge intégrale avant tout pipeline — Phase 5, en réponse à
    // l'accumulation de fichiers hachés observée en session
    // (`main.0e03e.css`, `main.42e0b.css`, ... jamais nettoyés d'un build
    // à l'autre). Arbitrage : nettoyage global ICI plutôt que trois
    // nettoyages partiels distincts (un par pipeline — styles/, sprites/,
    // chaque sous-dossier verbatim). `build_root` est un répertoire
    // ENTIÈREMENT généré (rien n'y est jamais écrit à la main — même
    // convention que l'en-tête `GÉNÉRÉ... NE PAS MODIFIER MANUELLEMENT`
    // déjà en usage ailleurs dans ce workspace) : le vider puis le
    // reconstruire de zéro à chaque invocation est donc strictement sûr,
    // et élimine toute la classe de bugs "un pipeline oublie de nettoyer
    // son propre sous-dossier" plutôt que de la déplacer vers trois
    // implémentations à maintenir en synchronisation. Pas besoin
    // d'atomicité (suppression puis recréation, pas de bascule
    // symlink) : ce binaire est séquentiel, personne ne lit `build_root`
    // pendant son exécution.
    if build_root.exists() {
        fs::remove_dir_all(&build_root)
            .map_err(|e| format!("nettoyage impossible de {} : {e}", build_root.display()))?;
    }
    fs::create_dir_all(&build_root)?;

    let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

    // Ordonnancement obligatoire (spec §10.1) : verbatim (résout le
    // registre d'URLs) AVANT styles (le consomme) — jamais l'inverse.
    let asset_url_registry = run_verbatim_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.static_.verbatim.files,
        &mut manifest,
    )?;

    // [webmanifest] (Phase 6) — dépend uniquement du registre d'URLs
    // (icons[].src pointe vers des favicons déjà hachés par verbatim ci-
    // dessus), aucune dépendance avec sprites/styles. `Option` : un thème
    // sans PWA (pas de section [webmanifest] dans theme.toml) est valide,
    // on saute silencieusement ce pipeline plutôt que de forcer sa
    // présence.
    if let Some(webmanifest_config) = &theme.webmanifest {
        run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            &build_root_rel,
            webmanifest_config,
            &asset_url_registry,
            &mut manifest,
        )?;
    }

    // [sprites] (Phase 4) — aucune dépendance avec verbatim/styles, ordre
    // libre. Placé ici à votre demande explicite, juste après verbatim.
    run_sprites_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.sprites,
        &mut manifest,
    )?;

    run_styles_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.styles.entries,
        &asset_url_registry,
        &mut manifest,
    )?;

    // [scripts.components] (Phase 7) — dépend uniquement du registre
    // d'URLs (les imports non-relatifs comme `/libs/leaflet.js` doivent
    // déjà être hachés par verbatim), aucune dépendance avec
    // sprites/styles/webmanifest. Placé en dernier des pipelines de
    // contenu par simple cohérence de lecture (ordre d'apparition dans
    // theme.toml), pas par nécessité d'ordonnancement.
    run_scripts_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.scripts.components,
        &asset_url_registry,
        &mut manifest,
    )?;

    // La version vient de [theme].version, identique pour toutes les
    // entrées de ce build — renseignée ici plutôt que dans chaque pipeline
    // pour ne l'écrire qu'à un seul endroit.
    for entry in manifest.values_mut() {
        entry.version = theme.theme.version.clone();
    }

    // [service_worker] (Handoff §3) — SEUL pipeline de ce binaire à
    // dépendre du MANIFESTE COMPLET (styles + sprites + scripts +
    // webmanifest + verbatim), pas seulement d'`AssetUrlRegistry` (résolu
    // en tout début par [static.verbatim], structurellement plus étroit :
    // il ne contient jamais les URLs des styles/sprites/scripts/
    // webmanifest). D'où son câblage ici, tout dernier, APRÈS la boucle de
    // version ci-dessus (§3.4) — sa propre `AssetEntry` reçoit donc la
    // version explicitement, juste après l'appel, plutôt que de réordonner
    // cette boucle globale pour ce seul cas.
    //
    // `Option` : un thème sans Service Worker (pas de section
    // `[service_worker]` dans `theme.toml`) reste valide, ce pipeline est
    // sauté silencieusement — même politique que `[webmanifest]`.
    if let Some(sw_config) = &theme.service_worker {
        // Vue dérivée, jetable : reconstruite à partir du manifeste final
        // pour réutiliser `resolve_asset_reference` (qui exige
        // `&AssetUrlRegistry`) sans le modifier — `AssetUrlRegistry` et
        // `manifest` sont deux structures distinctes du code réel (l'une
        // ne contient QUE le verbatim, l'autre TOUTES les clés).
        let manifest_url_registry: AssetUrlRegistry = manifest
            .iter()
            .map(|(k, v)| (k.clone(), v.url.clone()))
            .collect();

        run_service_worker_pipeline(
            &theme_dir,
            &build_root,
            &build_root_rel,
            sw_config,
            &manifest_url_registry,
            &mut manifest,
        )?;

        if let Some(entry) = manifest.get_mut("serviceWorker.js") {
            entry.version = theme.theme.version.clone();
        }
    }

    let output = AssetManifest { assets: manifest };
    let serialized = toml::to_string_pretty(&output)?;
    let manifest_path = build_root.join("manifest.toml");
    fs::write(&manifest_path, serialized)?;

    println!(
        "[marius-assets] manifeste écrit : {} ({} entrées)",
        manifest_path.display(),
        output.assets.len()
    );

    Ok(())
}
