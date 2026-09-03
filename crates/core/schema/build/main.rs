// crates/core/schema/build/main.rs

//! # Orchestrateur build-time.
//!
//! ## Responsabilités
//!
//! 1. **Registry driver (Phase 1)** : `fetch_component_list()`
//! 2. **Validation layout (Phase 2)** : `validate_layout()`
//! 3. **Pipeline template Voie B** : Lecture `.marius`, parse, résolution,
//!    génération du corps `render()` — toute l'I/O disque vit ici.  
//!    `db-forge` (`write_projection_stub`) reste un générateur pur : il reçoit
//!    le résultat déjà calculé (`Option<(&str, &TemplateMetrics)>`).
//!
//! ## Prérequis
//!
//! `DATABASE_URL` pointe vers `marius` avec le rôle `marius_admin`.
//!
//! ## Organisation
//!
//! Ce fichier reste l'orchestrateur mince (`main()`) ; le détail par
//! responsabilité vit à côté de lui, dans `build/` :
//! - [`manifest`] : chargement du manifeste d'assets, chemins du thème.
//! - [`markers`] : grammaire des sélecteurs de marqueurs.
//! - [`capabilities`] : résolution AOT de la table des capacités JS.
//! - [`modules_lowering`] : lowering des modules JS par template.
//! - [`asset_lookup`] : résolution diagnostique `{% asset %}`.
//! - [`template`] : pipeline Voie B (`.marius` → `render()`).

mod asset_lookup;
mod capabilities;
mod manifest;
mod markers;
mod modules_lowering;
mod template;

use std::path::PathBuf;

use marius_db_forge::{
    PrimaryKey, build_field_specs, check_no_name_collision, fetch_columns, fetch_component_list,
    fetch_max_id, fetch_pk_column, fetch_varlena_cols, validate_layout, write_collector,
    write_from_impl, write_projection_stub, write_row_struct, write_section_header,
    write_store_struct, write_varlen_owned_struct,
};
use marius_fragment_forge::VarlenField;

use crate::capabilities::validate_capabilities;
use crate::manifest::{build_dir, load_asset_manifest};
use crate::template::common::GENERATED_HEADER;
use crate::template::dynamic::resolve_template;
use crate::template::static_page::{STATIC_PAGES, resolve_static_page};

/// Marqueur textuel du point d'injection des `<script>` hissés — décision
/// actée en session : pas de nouveau token dans l'AST gelé de
/// `fragment-forge`, une simple constante de chaîne recherchée comme
/// SOUS-CHAÎNE parmi les `FlatPageToken::Static` du layout PARENT (Mode
/// Page), après `lower`. Un commentaire HTML, jamais interprété par ce
/// moteur de template (`{% %}`/`{{ }}` sont les seules syntaxes actives) :
/// à écrire tel quel dans le `<head>` du layout de base, où les scripts
/// doivent apparaître.
pub(crate) const SCRIPTS_PLACEHOLDER: &str = "<!-- MARIUS_SCRIPTS -->";

/// Marqueur textuel du point d'injection des modules conditionnels pilotés
/// par `content.core.js_deps` — HANDOFF-js-deps-capacites-frontend-v2.md.
///
/// Même mécanisme de principe que `SCRIPTS_PLACEHOLDER` (sous-chaîne d'un
/// `FlatPageToken::Static`, jamais interprétée par le moteur de template),
/// mais lowering DIFFÉRENT : `SCRIPTS_PLACEHOLDER` reste un simple
/// commentaire HTML inoffensif s'il n'y a rien à hisser (aucun nouveau
/// token) ; `MODULES_PLACEHOLDER` se lowerise systématiquement en
/// `FlatPageToken::ModulesPlaceholder` — ajout délibéré à l'AST gelé,
/// nécessaire parce que l'émission dépend d'un bitset RUNTIME
/// (`record.js_deps`), jamais connaissable au moment de la composition
/// Page/Fragment (contrairement au contenu statique d'un `{% script %}`).
pub(crate) const MODULES_PLACEHOLDER: &str = "<!-- MARIUS_MODULES -->";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Invalider le cache Cargo si DATABASE_URL ou ce fichier changent.
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("DB-Forge : CARGO_MANIFEST_DIR non définie (toujours fournie par Cargo).");

    // Lecture unique du manifeste d'assets — une seule fois pour tout le
    // build, pas par table (évite un re-parsing TOML redondant et une
    // duplication d'émissions rerun-if-changed).
    let loaded = load_asset_manifest(&manifest_dir).unwrap_or_else(|()| {
        // cargo:error déjà émis par load_asset_manifest — arrêt immédiat,
        // même discipline que resolve_template plus bas.
        std::process::exit(1);
    });

    // Validation unique de la table canonique des capacités — les capacités
    // sont globales (theme.toml + scripts_registry.lock), jamais recalculées
    // par composant. Avant la boucle STATIC_PAGES, pour la même raison que
    // load_asset_manifest ci-dessus : fait échouer le build sur ce point
    // précis avant toute tentative de connexion Postgres. Le lowering PAR
    // TEMPLATE (scan statique + croisement avec record.js_deps) est fait
    // plus loin, une fois par composant/page — jamais ici.
    let capabilities =
        validate_capabilities(&manifest_dir, &loaded.assets, &loaded.classic_scripts)
            .unwrap_or_else(|()| {
                std::process::exit(1);
            });

    // `classic_scripts` n'est plus nécessaire au-delà de la résolution
    // ci-dessus (déjà consommée dans `capabilities[*].deps`) — `assets`
    // redevient le nom utilisé par tout le reste de cette fonction,
    // inchangé depuis avant l'introduction de `LoadedAssets`.
    let assets = loaded.assets;

    // ── Pages sans donnée dynamique (STATIC_PAGES) ─────────────────────────
    //
    // Volontairement AVANT l'ouverture du pool Postgres ci-dessous : aucune
    // des pages de cette liste n'a besoin d'une connexion SQL, les traiter
    // ici évite une dépendance artificielle à la base pour un chemin qui
    // structurellement n'en a aucun besoin — et fait échouer le build sur
    // ce point précis avant même la tentative de connexion, si jamais il y
    // avait un problème ici.
    let theme_build_dir = build_dir(&manifest_dir);
    for (schema, table) in STATIC_PAGES {
        let html = resolve_static_page(&manifest_dir, &assets, schema, table, &capabilities)
            .unwrap_or_else(|()| {
                // cargo:error déjà émis par resolve_static_page — arrêt
                // immédiat, même discipline que pour les templates pilotés par
                // fetch_component_list plus bas.
                std::process::exit(1);
            });

        let output_path = theme_build_dir.join(format!("{table}.html"));
        std::fs::write(&output_path, &html).unwrap_or_else(|e| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : écriture de la page statique \
                 échouée ({}) : {e}",
                output_path.display()
            );
            std::process::exit(1);
        });

        // Jamais d'entrée dans manifest.toml pour ces pages : ce fichier a
        // un producteur unique (marius-assets, qui le réécrit intégralement
        // à chaque exécution) — y ajouter une entrée depuis ce build.rs
        // serait effacé sans avertissement au prochain build de
        // marius-assets. L'URL publique (`/offline.html`, non hachée —
        // décision de session, page de routage, pas une sous-ressource)
        // reste un littéral écrit à la main là où elle est référencée
        // (`OFFLINE_URL` dans `serviceWorker.js`), jamais résolue via ce
        // manifeste.
        println!(
            "cargo:warning=DB-Forge [{schema}.{table}] : page statique -> {}",
            output_path.display()
        );
    }

    let database_url = std::env::var("DATABASE_URL").expect("DB-Forge : DATABASE_URL non définie.");
    let pool = sqlx::PgPool::connect(&database_url).await?;

    // ── Phase 1 : registry driver ─────────────────────────────────────────────
    let components = fetch_component_list(&pool).await?;

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let out_path = out_dir.join("generated_schema.rs");

    let mut output = String::from(GENERATED_HEADER);

    for comp in &components {
        let columns = fetch_columns(&pool, &comp.schema, &comp.table).await?;
        let pk = fetch_pk_column(&pool, &comp.schema, &comp.table).await?;

        let max_id: Option<usize> = match &pk {
            PrimaryKey::Single(col) => {
                Some(fetch_max_id(&pool, &comp.schema, &comp.table, col).await?)
            }
            PrimaryKey::Composite => None,
        };

        // Assemblage multi-slot (CONTRAT-implementation-multi-slot-varlena.md,
        // Étape 4) : comp.varlena_join est désormais Vec<VarlenJoin> (registry.rs,
        // Étape 1) — un appel à fetch_varlena_cols par slot, concaténés dans
        // l'ordre join_slot_idx croissant déjà garanti par registry.rs.
        let mut varlena: Vec<VarlenField> = Vec::new();
        for join in &comp.varlena_join {
            varlena.extend(fetch_varlena_cols(&pool, &join.schema, &join.table).await?);
        }

        // ── Collision de nom (Étape 3) ─────────────────────────────────────────
        // Échec de build explicite si un champ varlena entre en collision avec
        // un autre slot ou avec une colonne propre du composant — politique
        // DDL-driven arbitrée le 22/07/2026, aucune désambiguïsation automatique.
        {
            let component_id = format!("{}.{}", comp.schema, comp.table);
            if let Err(msg) = check_no_name_collision(&component_id, &columns, &varlena) {
                println!("cargo:error=DB-Forge [{component_id}] : {msg}");
                std::process::exit(1);
            }
        }

        // ── Phase 2 : validation layout ───────────────────────────────────────
        if comp.intent_density != 0
            && let Err(msg) = validate_layout(&columns, comp.intent_density)
        {
            println!(
                "cargo:error=DB-Forge [{}.{}] : {}",
                comp.schema, comp.table, msg
            );
            std::process::exit(1);
        }

        // ── Voie B : pipeline template .marius ────────────────────────────────
        // Toute l'I/O disque (lecture du template) vit ici. db-forge ne touche
        // jamais le système de fichiers — il reçoit le résultat déjà calculé.
        let field_specs = build_field_specs(&columns);
        let render = resolve_template(
            &manifest_dir,
            &assets,
            &comp.schema,
            &comp.table,
            &field_specs,
            &varlena,
            &capabilities,
        )
        .unwrap_or_else(|()| {
            // cargo:error déjà émis par resolve_template — arrêt immédiat.
            std::process::exit(1);
        });

        write_section_header(&mut output, &comp.schema, &comp.table, &pk);
        write_row_struct(&mut output, &comp.schema, &comp.table, &columns, &varlena);
        write_store_struct(&mut output, &comp.schema, &comp.table, &columns);
        write_varlen_owned_struct(&mut output, &comp.schema, &comp.table, &varlena);
        write_from_impl(&mut output, &comp.schema, &comp.table, &columns);

        if let (PrimaryKey::Single(col), Some(max)) = (&pk, max_id) {
            write_collector(&mut output, &comp.schema, &comp.table, col, max);
        }

        let varlena_join_tuples: Vec<(&str, &str, &str)> = comp
            .varlena_join
            .iter()
            .map(|j| (j.schema.as_str(), j.table.as_str(), j.fk_col.as_str()))
            .collect();

        write_projection_stub(
            &mut output,
            &comp.schema,
            &comp.table,
            &columns,
            &pk,
            &varlena,
            &varlena_join_tuples,
            render
                .as_ref()
                .map(|(body, metrics)| (body.as_str(), metrics)),
        );
    }

    std::fs::write(&out_path, &output)?;
    eprintln!("DB-Forge : généré → {}", out_path.display());
    Ok(())
}
