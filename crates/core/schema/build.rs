// =============================================================================
// DB-Forge — crates/core/schema/build.rs
//
// Orchestrateur build-time. Responsabilités :
//   1. Registry driver (Phase 1)      : fetch_component_list()
//   2. Validation layout (Phase 2)    : validate_layout()
//   3. Pipeline template Voie B       : lecture .marius, parse, résolution,
//      génération du corps render() — TOUTE l'I/O disque vit ici.
//      db-forge (write_projection_stub) reste un générateur pur : il reçoit
//      le résultat déjà calculé (Option<(&str, &TemplateMetrics)>).
//
// Prérequis : DATABASE_URL pointe vers marius avec rôle marius_admin.
// =============================================================================

use std::path::{Path, PathBuf};

use marius_db_forge::{
    PrimaryKey, build_field_specs, fetch_columns, fetch_component_list, fetch_max_id,
    fetch_pk_column, fetch_varlena_cols, validate_layout, write_collector, write_from_impl,
    write_projection_stub, write_row_struct, write_section_header, write_store_struct,
    write_varlen_owned_struct,
};

use marius_fragment_forge::{
    SchemaIndex, TemplateMetrics, VarlenField, detect_extends, generate_aot_snippet,
    parse_page_tokens, parse_tokens, relative_path_for_include_str, resolve_and_measure, scan,
    validate_ast,
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

/// Lecture brute d'un fichier `.marius`. Extraction pure du bloc de lecture
/// déjà présent dans `resolve_template` — aucun changement de comportement
/// sur le chemin de succès. Isolée pour être réutilisable telle quelle par
/// un futur appelant traitant un second fichier (portée hors Phase 6.1 :
/// aucun second appelant n'est câblé ici).
///
/// Retourne :
///   `Ok(src)` : contenu du fichier.
///   `Err(())` : lecture échouée — cargo:error déjà émis par cette fonction.
fn read_template_file(path: &Path) -> Result<String, ()> {
    std::fs::read_to_string(path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : lecture du template échouée ({}) : {e}",
            path.display()
        );
    })
}

/// Sous-orchestration Mode Page — E/S parent + garde single-level.
///
/// Signature gelée à sa forme finale (Document 3 §4) : les paramètres
/// `_fixed`/`_varlena`/`_child_src` ne sont pas encore consommés par cette
/// phase (câblage arène/collecte de blocs/linker/lowering : Phases 6.4 à
/// 6.6), d'où le préfixe `_`. Aucune logique de ces phases n'est anticipée
/// ici — toute garde passée retourne un refus explicite, pas un résultat
/// construit.
///
/// Retourne :
///   `Err(())` : parent illisible (E/S), parent syntaxiquement invalide, ou
///               parent déclarant lui-même `extends` (garde single-level).
///               Trois messages `cargo:error` distincts.
///   `Err(())` (garde passée) : câblage aval non encore implémenté — ce
///               n'est pas une erreur du template, seulement l'état actuel
///               du pipeline. Distingué des trois précédents par son propre
///               message.
fn resolve_page_template<'src>(
    manifest_dir: &str,
    schema: &str,
    table: &str,
    _fixed: &[marius_fragment_forge::FieldSpec],
    _varlena: &[VarlenField],
    _child_src: &'src str,
    child_extends: &'src str,
) -> Result<(String, TemplateMetrics), ()> {
    let parent_path = PathBuf::from(relative_path_for_include_str(manifest_dir, child_extends));

    // Invalidation de cache : un parent modifié doit invalider le build, au
    // même titre que l'enfant (déjà couvert par resolve_template).
    println!("cargo:rerun-if-changed={}", parent_path.display());

    let parent_src = read_template_file(&parent_path)?;

    let parent_ast = parse_page_tokens(scan(&parent_src)).map_err(|e| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : parent Mode Page invalide ({}) : {e:?}",
            parent_path.display()
        );
    })?;

    // Garde single-level (Document 2 §6.1, tranchée par Document 3 §5) :
    // l'héritage multi-niveaux n'est pas couvert par ce contrat.
    if parent_ast.extends.is_some() {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : héritage multi-niveaux non supporté \
             ({} déclare lui-même extends)",
            parent_path.display()
        );
        return Err(());
    }

    // Garde passée : câblage aval (arène, collecte de blocs, linker,
    // lowering) hors périmètre de la Phase 6.3.
    println!(
        "cargo:error=DB-Forge [{schema}.{table}] : Mode Page — garde single-level passée, \
         câblage aval non encore implémenté (Phase 6.3)"
    );
    Err(())
}

/// Tente de résoudre le template `.marius` d'une table via le pipeline complet
/// Fragment-Forge : scan → parse_tokens → validate_ast → resolve_and_measure →
/// generate_aot_snippet.
///
/// Chemin attendu : `{manifest_dir}/templates/{schema}/{table}.marius`.
///
/// Retourne :
///   `Ok(None)`        : fichier absent — cargo:warning émis, fallback stub.
///   `Ok(Some((body, metrics)))` : template résolu avec succès (Mode
///                       Fragment, ou Mode Page une fois le pipeline complet
///                       câblé — hors portée avant Phase 6.6).
///   `Err(())`         : toute erreur de parsing/validation/résolution
///                       (Mode Fragment), ou tout échec de
///                       `resolve_page_template` (Mode Page — Phase 6.2 :
///                       `detect_extends` est le point de décision de mode
///                       unique de ce fichier ; Phase 6.3 : E/S parent,
///                       garde single-level, refus explicite au-delà).
///                       cargo:error déjà émis ; l'appelant doit exit(1).
fn resolve_template(
    manifest_dir: &str,
    schema: &str,
    table: &str,
    fixed: &[marius_fragment_forge::FieldSpec],
    varlena: &[VarlenField],
) -> Result<Option<(String, TemplateMetrics)>, ()> {
    let template_path: PathBuf = Path::new(manifest_dir)
        .join("templates")
        .join(schema)
        .join(format!("{table}.marius"));

    if !template_path.exists() {
        println!(
            "cargo:warning=DB-Forge [{schema}.{table}] : aucun template trouvé \
             ({}) — render() vide (capacité 0).",
            template_path.display(),
        );
        return Ok(None);
    }

    // Invalidation du cache build si le template change.
    println!("cargo:rerun-if-changed={}", template_path.display());

    let src = read_template_file(&template_path)?;

    // Point de branchement de mode — unique dans tout le fichier (Document 3
    // §1). Le parse enfant ci-dessous est le « appel minimal à
    // parse_page_tokens » : il ne sert qu'à extraire le chemin déclaré par
    // extends, aucun appel à collect_blocks/link/lower en aval (hors
    // périmètre Phase 6.3).
    if detect_extends(&src) {
        let child_ast = parse_page_tokens(scan(&src)).map_err(|e| {
            println!("cargo:error=DB-Forge [{schema}.{table}] : enfant Mode Page invalide : {e:?}");
        })?;

        // Invariant : detect_extends == true garantit qu'un parse réussi
        // porte extends.is_some() — la déclaration de tête est ce que
        // detect_extends vient de constater (Document 1 §2.2).
        let child_extends = child_ast
            .extends
            .expect("detect_extends garantit extends.is_some() après parse réussi");

        let (body, metrics) = resolve_page_template(
            manifest_dir,
            schema,
            table,
            fixed,
            varlena,
            &src,
            child_extends,
        )?;
        return Ok(Some((body, metrics)));
    }

    let spans = scan(&src);
    let mut tokens = parse_tokens(spans).map_err(|e| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : erreur de parsing template : {e:?}");
    })?;

    validate_ast(&tokens).map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : template sémantiquement invalide : {errors:?}"
        );
    })?;

    let schema_index = SchemaIndex { fixed, varlena };

    // Résout les inclusions {% include path %} relativement au manifeste.
    // Aucun {% include %} dans les templates actuels — closure prête pour usage futur.
    let manifest_dir_owned = manifest_dir.to_string();
    let get_file_size = move |rel_path: &str| -> Result<usize, String> {
        std::fs::metadata(Path::new(&manifest_dir_owned).join(rel_path))
            .map(|m| m.len() as usize)
            .map_err(|e| e.to_string())
    };

    let metrics = resolve_and_measure(&mut tokens, &schema_index, get_file_size).map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : résolution du template échouée : {errors:?}"
        );
    })?;

    let body = generate_aot_snippet(&tokens, &schema_index);

    Ok(Some((body, metrics)))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Invalider le cache Cargo si DATABASE_URL ou ce fichier changent.
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("DB-Forge : CARGO_MANIFEST_DIR non définie (toujours fournie par Cargo).");

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

        let varlena = match &comp.varlena_join {
            Some(j) => fetch_varlena_cols(&pool, &j.schema, &j.table).await?,
            None => vec![],
        };

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
            &comp.schema,
            &comp.table,
            &field_specs,
            &varlena,
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

        write_projection_stub(
            &mut output,
            &comp.schema,
            &comp.table,
            &columns,
            &pk,
            &varlena,
            comp.varlena_join
                .as_ref()
                .map(|j| (j.schema.as_str(), j.table.as_str(), j.fk_col.as_str())),
            render
                .as_ref()
                .map(|(body, metrics)| (body.as_str(), metrics)),
        );
    }

    std::fs::write(&out_path, &output)?;
    eprintln!("DB-Forge : généré → {}", out_path.display());
    Ok(())
}
