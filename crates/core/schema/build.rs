// =============================================================================
// DB-Forge — crates/core/schema/build.rs  (Phase 0 — orchestrateur mince)
//
// Responsabilité unique : orchestrer les appels à marius_db_forge et écrire
// generated_schema.rs dans $OUT_DIR.
//
// ─── Ce que ce fichier NE fait plus ──────────────────────────────────────────
//
//   Toute la logique d'introspection et de génération a été extraite vers
//   forge/db-forge/src/. Ce fichier ne contient plus que la séquence de build.
//
// ─── Phase 0 : liste hardcodée ───────────────────────────────────────────────
//
//   La liste des composants reste hardcodée (même comportement qu'avant).
//   Phase 1 : remplacée par fetch_component_list(&pool).await?
//             qui lit meta.containment_intent + meta.component_varlena_join.
//
// ─── Phase 2 : validation layout ─────────────────────────────────────────────
//
//   validate_layout() est disponible dans marius_db_forge mais non appelé ici.
//   intent_density = 0 dans la liste Phase 0. Phase 2 : lire depuis meta +
//   appeler validate_layout() avant write_section_header().
//
// Prérequis : DATABASE_URL pointe vers marius avec rôle marius_admin.
// =============================================================================

use std::path::PathBuf;

use marius_db_forge::{
    PrimaryKey,
    fetch_component_list,
    fetch_columns, fetch_max_id, fetch_pk_column, fetch_varlena_cols,
    validate_layout,
    write_collector, write_from_impl, write_projection_stub,
    write_row_struct, write_section_header, write_store_struct,
    write_varlen_owned_struct,
};

// En-tête statique du fichier généré — pas de couplage sur fragment-forge pour
// ce seul token textuel (décision architecturale Phase 0).
const GENERATED_HEADER: &str = "// GÉNÉRÉ PAR LA FORGE MARIUS — NE PAS MODIFIER MANUELLEMENT\n\
// Régénérer via : cargo build\n\n\
#[allow(unused_imports)]\n\
use crate::projection::Projection as _;\n\n\
#[allow(unused_imports)]\n\
use chrono::Datelike as _;\n\n\
/// Échappe les caractères HTML dangereux dans `s` et pousse le résultat dans `buf`.\n\
/// Zéro allocation : opère directement sur buf (déjà réservé par render()).\n\
#[inline(always)]\n\
#[allow(dead_code)]\n\
fn marius_html_escape(s: &str, buf: &mut String) {\n\
    for ch in s.chars() {\n\
        match ch {\n\
            '&'  => buf.push_str(\"&amp;\"),\n\
            '<'  => buf.push_str(\"&lt;\"),\n\
            '>'  => buf.push_str(\"&gt;\"),\n\
            '\"' => buf.push_str(\"&quot;\"),\n\
            '\\'' => buf.push_str(\"&#39;\"),\n\
            _    => buf.push(ch),\n\
        }\n\
    }\n\
}\n\n\
/// Pousse un VarlenSlot dans la TOC et concatène la valeur dans le heap (Phase 1.4).\n\
#[inline(always)]\n\
#[allow(dead_code)]\n\
fn push_varlen_slot(field: &Option<String>, heap: &mut Vec<u8>, toc: &mut Vec<crate::projection::VarlenSlot>) {\n\
    match field {\n\
        None    => toc.push(crate::projection::VarlenSlot { offset: u32::MAX, len: 0 }),\n\
        Some(s) => {\n\
            let offset = heap.len() as u32;\n\
            heap.extend_from_slice(s.as_bytes());\n\
            toc.push(crate::projection::VarlenSlot { offset, len: s.len() as u32 });\n\
        }\n\
    }\n\
}\n\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Invalider le cache Cargo si DATABASE_URL ou ce fichier changent.
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let database_url = std::env::var("DATABASE_URL")
        .expect("DB-Forge : DATABASE_URL non définie.");
    let pool = sqlx::PgPool::connect(&database_url).await?;

    // ── Phase 1 : registry driver ─────────────────────────────────────────────
    // Toute erreur (DATABASE_URL inaccessible, schéma absent, component_id malformé)
    // remonte via ? → Box<dyn Error> → cargo:error. Aucun panic.
    let components = fetch_component_list(&pool).await?;

    let out_dir  = PathBuf::from(std::env::var("OUT_DIR")?);
    let out_path = out_dir.join("generated_schema.rs");

    let mut output = String::from(GENERATED_HEADER);

    for comp in &components {
        let columns = fetch_columns(&pool, &comp.schema, &comp.table).await?;
        let pk      = fetch_pk_column(&pool, &comp.schema, &comp.table).await?;

        let max_id: Option<usize> = match &pk {
            PrimaryKey::Single(col) => {
                Some(fetch_max_id(&pool, &comp.schema, &comp.table, col).await?)
            }
            PrimaryKey::Composite => None,
        };

        let varlena = match &comp.varlena_join {
            Some(j) => fetch_varlena_cols(&pool, &j.schema, &j.table).await?,
            None    => vec![],
        };

        // ── Phase 2 : validation layout ───────────────────────────────────────
        // Garde : intent_density == 0 signifie que la densité n'est pas déclarée
        // dans le registre — skip silencieux (composant en cours de configuration).
        // Tout autre écart est une erreur de build fatale (cargo:error).
        if comp.intent_density != 0
            && let Err(msg) = validate_layout(&columns, comp.intent_density) {
                println!(
                    "cargo:error=DB-Forge [{}.{}] : {}",
                    comp.schema, comp.table, msg
                );
                std::process::exit(1);
            }

        write_section_header(&mut output, &comp.schema, &comp.table, &pk);
        write_row_struct(&mut output, &comp.schema, &comp.table, &columns, &varlena);
        write_store_struct(&mut output, &comp.schema, &comp.table, &columns);
        write_varlen_owned_struct(&mut output, &comp.schema, &comp.table, &varlena);
        write_from_impl(&mut output, &comp.schema, &comp.table, &columns);

        if let (PrimaryKey::Single(col), Some(max)) = (&pk, max_id) {
            write_collector(&mut output, &comp.schema, &comp.table, col, max);
        }

        write_projection_stub(
            &mut output,
            &comp.schema,
            &comp.table,
            &columns,
            &pk,
            &varlena,
            comp.varlena_join.as_ref().map(|j| {
                (j.schema.as_str(), j.table.as_str(), j.fk_col.as_str())
            }),
        );
    }

    std::fs::write(&out_path, &output)?;
    eprintln!("DB-Forge : généré → {}", out_path.display());
    Ok(())
}
