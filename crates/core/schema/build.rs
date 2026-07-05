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
    PageArena, SchemaIndex, TemplateMetrics, VarlenField, collect_blocks, collect_static_refs,
    detect_extends, generate_aot_snippet, link, parse_page_tokens, parse_tokens,
    relative_path_for_include_str, resolve_and_measure, scan, validate_ast,
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

/// Sous-orchestration Mode Page — E/S parent, garde single-level, admission
/// en arène, calcul du `LinkPlan` (Phase 6.5).
///
/// Signature gelée à sa forme finale (Document 3 §4) : les paramètres
/// `_fixed`/`_varlena` restent non consommés (câblage du Lowering et
/// jonction avec le pipeline gelé : Phase 6.6), d'où le préfixe `_`.
/// `child_src` reste consommé pour la ré-analyse d'admission (Phase 6.4,
/// double parse accepté, inchangé ici). Aucune logique de la Phase 6.6
/// n'est anticipée ici — `LinkPlan` obtenu, la fonction retourne un refus
/// explicite, pas un résultat construit ni un appel à `lower`.
///
/// Retourne :
///   `Err(())` : parent illisible (E/S), parent syntaxiquement invalide,
///               parent déclarant lui-même `extends` (garde single-level),
///               enfant syntaxiquement invalide à la ré-analyse
///               d'admission, blocs enfant ou parent syntaxiquement mal
///               formés (`collect_blocks` — imbrication, `for`, mot-clé
///               relationnel), ou linking échoué (`link` — bloc orphelin,
///               fichier `static` introuvable). Sept messages `cargo:error`
///               distincts au total.
///   `Err(())` (`LinkPlan` obtenu) : câblage aval (Lowering, jonction avec
///               `validate_ast`/`resolve_and_measure`/`generate_aot_snippet`)
///               non encore implémenté — ce n'est pas une erreur du
///               template, seulement l'état actuel du pipeline. Distingué
///               des précédents par son propre message.
fn resolve_page_template<'src>(
    manifest_dir: &str,
    schema: &str,
    table: &str,
    _fixed: &[marius_fragment_forge::FieldSpec],
    _varlena: &[VarlenField],
    child_src: &'src str,
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

    // Admission en arène (Phase 6.4, Documents 1+2 §2) : enfant et parent
    // obtiennent chacun un TemplateId distinct dans le contexte réel du
    // build. Ré-analyse de l'enfant : `parse_page_tokens` a déjà validé ce
    // même contenu dans `resolve_template` (extraction de `child_extends`)
    // ; un échec ici serait donc un bug du pipeline, pas un cas de template
    // réellement invalide — message distinct pour le distinguer à la lecture
    // des logs `cargo:error`.
    let child_ast = parse_page_tokens(scan(child_src)).map_err(|e| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : enfant Mode Page invalide \
             (ré-analyse pour admission en arène) : {e:?}"
        );
    })?;

    let mut arena = PageArena::default();
    let child_id = arena.admit(child_ast);
    let parent_id = arena.admit(parent_ast);

    // Collecte de blocs — schéma-libre (Document 2 §3, Phase 5.2-5.4),
    // câblée ici sur les AST réels de l'arène. Erreurs distinctes
    // enfant/parent : une plage mal formée du mauvais fichier ne doit pas
    // se confondre dans un message unique.
    let child_blocks = collect_blocks(child_id, &arena.get(child_id).tokens).map_err(|errors| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : blocs enfant mal formés : {errors:?}");
    })?;
    let parent_blocks =
        collect_blocks(parent_id, &arena.get(parent_id).tokens).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : blocs parent mal formés : {errors:?}"
            );
        })?;

    // Extraction des références `{% static %}` des deux fichiers (Document 2
    // §5.7, déjà scaffoldée dans fragment-forge) — aucune déduplication à ce
    // stade (Document 2 §6.2, hors périmètre).
    let mut static_refs = collect_static_refs(&arena.get(child_id).tokens);
    static_refs.extend(collect_static_refs(&arena.get(parent_id).tokens));

    // Vérification d'existence des fichiers `static`, injectée au Linker
    // sous forme de closure pure modulo E/S (Document 2 §4) — résolution
    // relative au manifeste, même fonction que pour le chemin `extends`.
    let file_exists = |path: &str| -> bool {
        Path::new(&relative_path_for_include_str(manifest_dir, path)).exists()
    };

    let _plan =
        link(&parent_blocks, &child_blocks, &static_refs, file_exists).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : linking Mode Page échoué : {errors:?}"
            );
        })?;

    // LinkPlan obtenu : câblage aval (Lowering, jonction avec le pipeline
    // gelé) hors périmètre de la Phase 6.5.
    println!(
        "cargo:error=DB-Forge [{schema}.{table}] : Mode Page — LinkPlan calculé, \
         câblage aval (lowering) non encore implémenté (Phase 6.5)"
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
///                       garde single-level ; Phase 6.4 : admission en
///                       arène ; Phase 6.5 : collecte de blocs, extraction
///                       `static`, calcul du `LinkPlan`, refus explicite
///                       au-delà).
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

// =============================================================================
// Tests — Phase 6.4
// =============================================================================
//
// NB (transparence de cadrage, pas un invariant du produit) : un build script
// (`build.rs`) n'est pas un cible testée par `cargo test` dans Cargo standard
// — il n'existe pas de harness qui exécute ce module automatiquement. Il est
// néanmoins écrit ici, contre les fonctions réelles de ce fichier et de
// `marius-fragment-forge`, pour vérification manuelle (`rustc --edition 2024
// --test build.rs` une fois les crates de dépendance résolues) et pour
// documenter précisément le jalon vert attendu par la roadmap. Migrer ce test
// vers une cible `tests/` intégrée à `crates/core/schema` — ce qui rendrait
// son exécution automatique sous `cargo test` — est un choix d'outillage
// hors périmètre de la Phase 6.4 (aucune restructuration de crate n'est
// prévue par cette phase).
#[cfg(test)]
mod tests_phase_6_4_arena_admission {
    use super::read_template_file;
    use marius_fragment_forge::{PageArena, parse_page_tokens, scan};
    use std::io::Write;
    use std::path::PathBuf;

    /// Jalon Vert (roadmap §6.4) — fixtures réelles sur disque (pas de
    /// construction en mémoire comme en Phase 5.1) : lecture, ré-analyse,
    /// puis admission en arène de l'enfant et du parent. Vérifie que
    /// `arena.get(child_id).tokens.len()` et
    /// `arena.get(parent_id).tokens.len()` correspondent exactement au
    /// contenu attendu de chaque fixture — pas seulement que l'admission ne
    /// panique pas.
    ///
    /// Portée du test : reproduit la séquence I/O + parse + admission de
    /// `resolve_page_template` (lecture, `parse_page_tokens(scan(..))`,
    /// `PageArena::admit` ×2) sans passer par le pipeline complet de
    /// `build.rs` (connexion PostgreSQL, `main()`) — `resolve_page_template`
    /// n'est pas `pub`, ce test exerce donc les mêmes briques constitutives
    /// qu'elle, dans le même ordre, exactement comme le prescrit la
    /// roadmap (« test d'intégration avec fixtures réelles sur disque »).
    #[test]
    fn admitting_child_and_parent_fixtures_yields_expected_token_counts() {
        let dir = std::env::temp_dir().join(format!(
            "marius-phase-6-4-arena-admission-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("création du répertoire de fixture");

        let parent_path: PathBuf = dir.join("parent.marius");
        let child_path: PathBuf = dir.join("child.marius");

        // Parent : un unique bloc top-level → 3 tokens (BlockOpen, Static,
        // BlockEnd), extends absent.
        std::fs::File::create(&parent_path)
            .and_then(|mut f| f.write_all(b"{% block header %}Default{% endblock %}"))
            .expect("écriture de la fixture parent");

        // Enfant : extends capturé hors de `tokens` + un bloc top-level → 3
        // tokens (BlockOpen, Static, BlockEnd).
        std::fs::File::create(&child_path)
            .and_then(|mut f| {
                f.write_all(b"{% extends parent.marius %}{% block header %}Child{% endblock %}")
            })
            .expect("écriture de la fixture enfant");

        let parent_src = read_template_file(&parent_path).expect("lecture du parent");
        let child_src = read_template_file(&child_path).expect("lecture de l'enfant");

        let parent_ast =
            parse_page_tokens(scan(&parent_src)).expect("parent syntaxiquement valide");
        let child_ast = parse_page_tokens(scan(&child_src)).expect("enfant syntaxiquement valide");

        assert_eq!(parent_ast.extends, None);
        assert_eq!(child_ast.extends, Some("parent.marius"));

        let mut arena = PageArena::default();
        let child_id = arena.admit(child_ast);
        let parent_id = arena.admit(parent_ast);

        assert_eq!(arena.get(child_id).tokens.len(), 3);
        assert_eq!(arena.get(parent_id).tokens.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// =============================================================================
// Tests — Phase 6.5
// =============================================================================
//
// Même réserve qu'en Phase 6.4 (cf. NB ci-dessus) : ce module n'est pas
// exécuté automatiquement par `cargo test` puisque `build.rs` n'est pas une
// cible de test Cargo standard — écrit pour vérification manuelle et
// migration future vers `tests/`.
#[cfg(test)]
mod tests_phase_6_5_collect_and_link {
    use super::read_template_file;
    use marius_fragment_forge::{PageArena, collect_blocks, link, parse_page_tokens, scan};
    use std::path::PathBuf;

    /// Écrit `parent.marius`/`child.marius` sur disque dans un répertoire de
    /// fixture dédié (nommé par `dir_suffix`, pour éviter toute collision
    /// entre les deux tests de ce module) et retourne leurs chemins.
    fn write_fixtures(dir_suffix: &str, parent_src: &[u8], child_src: &[u8]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "marius-phase-6-5-link-{dir_suffix}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("création du répertoire de fixture");

        let parent_path = dir.join("parent.marius");
        let child_path = dir.join("child.marius");
        std::fs::write(&parent_path, parent_src).expect("écriture de la fixture parent");
        std::fs::write(&child_path, child_src).expect("écriture de la fixture enfant");

        (parent_path, child_path)
    }

    /// Jalon Vert (roadmap §6.5, cas « override ») — l'enfant redéfinit le
    /// bloc `title` du parent : le `LinkPlan` retient la plage de l'enfant
    /// (`source.template == child_id`), pas celle du parent, vérifié par
    /// introspection directe du plan (pas d'exécution de `lower`, hors
    /// périmètre de cette phase). Séquence identique à
    /// `resolve_page_template` : lecture disque, ré-analyse, admission en
    /// arène, `collect_blocks` ×2, `link`.
    #[test]
    fn override_case_plan_retains_child_block() {
        let (parent_path, child_path) = write_fixtures(
            "override",
            b"{% block title %}ParentTitle{% endblock %}",
            b"{% extends parent.marius %}{% block title %}ChildTitle{% endblock %}",
        );

        let parent_src = read_template_file(&parent_path).expect("lecture du parent");
        let child_src = read_template_file(&child_path).expect("lecture de l'enfant");

        let parent_ast =
            parse_page_tokens(scan(&parent_src)).expect("parent syntaxiquement valide");
        let child_ast = parse_page_tokens(scan(&child_src)).expect("enfant syntaxiquement valide");

        let mut arena = PageArena::default();
        let child_id = arena.admit(child_ast);
        let parent_id = arena.admit(parent_ast);

        let child_blocks = collect_blocks(child_id, &arena.get(child_id).tokens)
            .expect("blocs enfant bien formés");
        let parent_blocks = collect_blocks(parent_id, &arena.get(parent_id).tokens)
            .expect("blocs parent bien formés");

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true)
            .expect("linking doit réussir : bloc correspondant, aucune référence static");

        assert_eq!(plan.substitutions.len(), 1);
        assert_eq!(plan.substitutions[0].name, "title");
        assert_eq!(plan.substitutions[0].source.template, child_id);

        let _ = std::fs::remove_dir_all(parent_path.parent().unwrap());
    }

    /// Jalon Vert (roadmap §6.5, cas « fallback parent ») — l'enfant ne
    /// redéfinit pas le bloc `footer` du parent : le `LinkPlan` retient la
    /// plage du parent lui-même (`source.template == parent_id`),
    /// comportement par défaut acté au Document 2 §4. Même séquence que le
    /// cas « override » ci-dessus.
    #[test]
    fn fallback_case_plan_retains_parent_block_when_not_overridden() {
        let (parent_path, child_path) = write_fixtures(
            "fallback",
            b"{% block footer %}ParentFooter{% endblock %}",
            b"{% extends parent.marius %}",
        );

        let parent_src = read_template_file(&parent_path).expect("lecture du parent");
        let child_src = read_template_file(&child_path).expect("lecture de l'enfant");

        let parent_ast =
            parse_page_tokens(scan(&parent_src)).expect("parent syntaxiquement valide");
        let child_ast = parse_page_tokens(scan(&child_src)).expect("enfant syntaxiquement valide");

        let mut arena = PageArena::default();
        let child_id = arena.admit(child_ast);
        let parent_id = arena.admit(parent_ast);

        let child_blocks = collect_blocks(child_id, &arena.get(child_id).tokens)
            .expect("blocs enfant bien formés (aucun bloc)");
        let parent_blocks = collect_blocks(parent_id, &arena.get(parent_id).tokens)
            .expect("blocs parent bien formés");

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true)
            .expect("linking doit réussir : aucun bloc enfant, aucune référence static");

        assert_eq!(plan.substitutions.len(), 1);
        assert_eq!(plan.substitutions[0].name, "footer");
        assert_eq!(plan.substitutions[0].source.template, parent_id);

        let _ = std::fs::remove_dir_all(parent_path.parent().unwrap());
    }
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
