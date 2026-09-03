// crates/core/schema/build/template/page.rs

//! Sous-orchestration Mode Page (`{% extends %}`) — pipeline complet :
//! E/S parent, garde single-level, admission en arène, `LinkPlan`,
//! Lowering, jonction avec le pipeline gelé de `fragment-forge`.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use marius_fragment_forge::{
    AssetLookup, FlatPageToken, PageArena, SchemaIndex, TemplateMetrics, VarlenField,
    collect_blocks, collect_static_refs, extract_static_marker_facts, generate_aot_snippet,
    generate_segmented_snippet, hoist_and_dedupe_scripts, link, lower, parse_page_tokens,
    relative_path_for_include_str, resolve_and_measure, scan, splice_hoisted_scripts, validate_ast,
};

use crate::asset_lookup::resolve_asset_lookup;
use crate::capabilities::CapabilityInfo;
use crate::manifest::AssetEntry;
use crate::modules_lowering::{lower_modules_for_template, render_modules_as_rust};
use crate::template::common::{read_template_file, split_static_at_marker};
use crate::{MODULES_PLACEHOLDER, SCRIPTS_PLACEHOLDER};

/// Sous-orchestration Mode Page — pipeline complet : E/S parent, garde
/// single-level, admission en arène, `LinkPlan`, Lowering, jonction avec le
/// pipeline gelé (`validate_ast` → `resolve_and_measure` →
/// `generate_aot_snippet`). Dernière phase de la roadmap (6.6).
///
/// Signature gelée à sa forme finale (Document 3 §4), désormais entièrement
/// consommée : `fixed`/`varlena` alimentent le `SchemaIndex` du point de
/// jonction, au même titre que dans le chemin Mode Fragment de
/// `resolve_template`. Plus aucun paramètre `_`-préfixé.
///
/// Point de convergence (Document 3 §2) : à partir de `lower`, cette
/// fonction appelle exactement les trois fonctions gelées, sans
/// modification de signature ni de corps, dans le même ordre que le chemin
/// Mode Fragment — aucun branchement de mode ne survit à ce point.
///
/// Retourne :
///   `Err(())` : parent illisible (E/S), parent syntaxiquement invalide,
///               parent déclarant lui-même `extends` (garde single-level),
///               enfant syntaxiquement invalide à la ré-analyse
///               d'admission, blocs enfant ou parent syntaxiquement mal
///               formés, linking échoué (bloc orphelin, fichier `static`
///               introuvable), template Mode Page sémantiquement invalide
///               (`validate_ast`), ou résolution de capacité échouée
///               (`resolve_and_measure` — fichier `include`/`static`
///               illisible). Neuf messages `cargo:error` distincts.
///   `Ok((body, metrics))` : template Mode Page résolu avec succès —
///               structurellement indiscernable, à ce point, d'un résultat
///               Mode Fragment (Document 2 §5, postcondition finale).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_page_template<'src>(
    manifest_dir: &str,
    assets: &HashMap<String, AssetEntry>,
    schema: &str,
    table: &str,
    fixed: &[marius_fragment_forge::FieldSpec],
    varlena: &[VarlenField],
    child_src: &'src str,
    child_extends: &'src str,
    capabilities: &[(String, CapabilityInfo)],
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

    let plan =
        link(&parent_blocks, &child_blocks, &static_refs, file_exists).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : linking Mode Page échoué : {errors:?}"
            );
        })?;

    // Lowering (Document 2 §5, dernière étape du domaine composition) :
    // fusion irréversible parent+enfant selon `plan`, projection vers
    // `FlatPageToken` — Block/TemplateId/Static(StaticPartialRef) cessent
    // d'exister à partir d'ici. Fonction totale (pas de `Result`) : par
    // construction, `plan` provient d'un `link` réussi, donc toute
    // référence de composition est déjà résolue.
    let tokens = lower(&arena.get(parent_id).tokens, &plan, &arena);

    // Point de jonction unique (Document 3 §2) : à partir d'ici, identique
    // au chemin Mode Fragment de `resolve_template` — mêmes fonctions,
    // mêmes signatures, aucun paramètre de mode.
    validate_ast(&tokens).map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : Mode Page sémantiquement invalide : {errors:?}"
        );
    })?;

    // Hoisting + déduplication des <script> (session dédiée, révisée :
    // capture de bloc {% script %}/{% endscript %}, plus une simple clé
    // AssetRef) — exécuté APRÈS validate_ast : la passe de hoisting
    // suppose un flux IfBool/EndIf ET ScriptStart/ScriptEnd bien formés,
    // une garantie que seule validate_ast établit. Après validate_ast,
    // jamais avant.
    let (tokens, hoisted_blocks) = hoist_and_dedupe_scripts(tokens).map_err(|e| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : hoisting des scripts échoué : {e}");
    })?;

    let tokens = if hoisted_blocks.is_empty() {
        // Rien à hisser — le marqueur, présent ou non dans le layout,
        // n'exige aucun traitement : s'il est là sans jamais être utilisé,
        // il reste un commentaire HTML inoffensif dans la sortie.
        tokens
    } else {
        match split_static_at_marker(tokens, SCRIPTS_PLACEHOLDER) {
            Some((tokens, splice_index)) => {
                splice_hoisted_scripts(tokens, &hoisted_blocks, splice_index)
            }
            None => {
                println!(
                    "cargo:error=DB-Forge [{schema}.{table}] : {} bloc(s) {{% script %}} à \
                     hisser mais aucun marqueur {SCRIPTS_PLACEHOLDER} trouvé dans le layout {}",
                    hoisted_blocks.len(),
                    parent_path.display()
                );
                return Err(());
            }
        }
    };

    // MARIUS_MODULES — lowering PAR TEMPLATE (addendum « MARIUS_MODULES
    // agrège deux sources » — scan statique des Static tokens de CE flux
    // déjà fusionné parent+enfant, croisé avec record.js_deps). Jamais
    // conditionnel au nombre de capacités actives (contrairement au
    // hoisting de scripts ci-dessus) : `base.marius` porte ce marqueur en
    // permanence, son absence signale une corruption du layout, pas une
    // simple absence de contenu à hisser. Un snippet vide (0 capacité
    // concerne ce template) est un cas normal qui se traduit par un token
    // présent, mais lowerisant vers zéro octet — jamais une raison de
    // sauter le splice.
    let static_facts = extract_static_marker_facts(&tokens);
    let emissions = lower_modules_for_template(capabilities, &static_facts, true);
    let modules_lowering = render_modules_as_rust(&emissions);

    let mut tokens = match split_static_at_marker(tokens, MODULES_PLACEHOLDER) {
        Some((mut tokens, splice_index)) => {
            tokens.insert(splice_index, FlatPageToken::ModulesPlaceholder);
            tokens
        }
        None => {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : marqueur {MODULES_PLACEHOLDER} \
                 introuvable dans le layout {} — base.marius doit le porter en permanence \
                 (avant la fermeture de </head>)",
                parent_path.display()
            );
            return Err(());
        }
    };

    let schema_index = SchemaIndex { fixed, varlena };

    let manifest_dir_owned = manifest_dir.to_string();
    let get_file_size = move |rel_path: &str| -> Result<usize, String> {
        std::fs::metadata(Path::new(&manifest_dir_owned).join(rel_path))
            .map(|m| m.len() as usize)
            .map_err(|e| e.to_string())
    };

    // Résolution des {% asset key %} — manifeste réel (Roadmap marius-assets
    // §1.4, close). `resolve_asset_len` sert `resolve_and_measure` (longueur
    // de l'URL publique, jamais celle du fichier source) ; `resolve_asset_url`
    // sert `generate_aot_snippet` (émission littérale). Clé absente :
    // `AssetLookup::NotFound { suggestion }` / panic respectivement —
    // `AssetNotFound` est déjà remonté comme `ResolverError` par
    // `resolve_and_measure`, capturé par le `map_err` ci-dessous ; un panic
    // dans `resolve_asset_url` signalerait uniquement un appel hors ordre
    // (bug interne), jamais atteint si `resolve_and_measure` a réussi en
    // premier. `resolve_asset_lookup` calcule la suggestion diagnostique
    // ici — `fragment-forge` ne la recalcule jamais, il n'a pas les clés.
    let resolve_asset_len = |key: &str| -> AssetLookup { resolve_asset_lookup(assets, key) };
    let resolve_asset_url = |key: &str| -> &str {
        assets.get(key).map(|a| a.url.as_str()).unwrap_or_else(|| {
            panic!("AssetNotFound '{key}' non intercepté par resolve_and_measure")
        })
    };

    let metrics = resolve_and_measure(
        &mut tokens,
        &schema_index,
        get_file_size,
        resolve_asset_len,
        modules_lowering.static_bytes,
    )
    .map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : résolution du template Mode Page échouée : {errors:?}"
        );
    })?;

    // CONTRAT-implementation-projection-segmentee.md, Étape 5 : un champ
    // is_segment (tag SQL marius:large_content) déclenche generate_segmented_snippet
    // au lieu de generate_aot_snippet — jamais les deux pour le même composant.
    // write_projection_stub (codegen/projection.rs) recalcule ce même booléen
    // à partir du même varlena pour décider d'émettre render_segments() —
    // aucune valeur à faire transiter par le tuple de retour de cette fonction.
    let has_segment = varlena.iter().any(|v| v.is_segment);
    let body = if has_segment {
        generate_segmented_snippet(
            &tokens,
            &schema_index,
            resolve_asset_url,
            &modules_lowering.snippet,
        )
    } else {
        generate_aot_snippet(
            &tokens,
            &schema_index,
            resolve_asset_url,
            &modules_lowering.snippet,
        )
    };

    Ok((body, metrics))
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

// =============================================================================
// Tests — Phase 6.6
// =============================================================================
//
// Même réserve qu'en Phases 6.4/6.5 (cf. NB plus haut) : module non exécuté
// automatiquement par `cargo test` (build.rs n'est pas une cible de test
// Cargo standard) — écrit pour vérification manuelle et migration future
// vers `tests/`.
#[cfg(test)]
mod tests_phase_6_6_full_pipeline {
    use super::read_template_file;
    use marius_fragment_forge::{
        PageArena, SchemaIndex, collect_blocks, generate_aot_snippet, link, lower,
        parse_page_tokens, resolve_and_measure, scan, validate_ast,
    };
    use std::path::PathBuf;

    fn write_fixtures(dir_suffix: &str, parent_src: &[u8], child_src: &[u8]) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "marius-phase-6-6-pipeline-{dir_suffix}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("création du répertoire de fixture");

        let parent_path = dir.join("parent.marius");
        let child_path = dir.join("child.marius");
        std::fs::write(&parent_path, parent_src).expect("écriture de la fixture parent");
        std::fs::write(&child_path, child_src).expect("écriture de la fixture enfant");

        (parent_path, child_path)
    }

    /// Jalon Vert (roadmap §6.6, cas « override ») — reproduit intégralement
    /// la séquence de `resolve_page_template` sur fixtures disque réelles,
    /// jusqu'au point de jonction : lecture, parse ×2, admission en arène,
    /// `collect_blocks` ×2, `link`, `lower`, `validate_ast`,
    /// `resolve_and_measure`, `generate_aot_snippet`. Aucun `{{ champ }}`
    /// dans ces fixtures : `SchemaIndex` vide (`fixed`/`varlena` à `&[]`)
    /// suffit, `get_file_size` n'est jamais appelé (aucun `{% static %}`).
    #[test]
    fn override_case_pipeline_produces_expected_body_and_metrics() {
        let (parent_path, child_path) = write_fixtures(
            "override",
            b"<html>{% block title %}ParentTitle{% endblock %}</html>",
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

        let mut tokens = lower(&arena.get(parent_id).tokens, &plan, &arena);
        validate_ast(&tokens).expect("aucun Field/If dans ces fixtures : validation triviale");

        let schema_index = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let get_file_size =
            |_: &str| -> Result<usize, String> { Err("aucun static attendu dans ce test".into()) };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema_index,
            get_file_size,
            |_| unreachable!("aucun AssetRef dans ce test"),
            0,
        )
        .expect("aucun StaticInclude/Field dans ces fixtures : résolution triviale");

        // "<html>" (6) + "ChildTitle" (10) + "</html>" (7) = 23 — le contenu
        // parent substitué ("ParentTitle") ne doit apparaître nulle part.
        assert_eq!(metrics.total_static_bytes, 23);
        assert_eq!(metrics.total_dynamic_bytes, 0);
        assert_eq!(metrics.include_count, 0);

        let body = generate_aot_snippet(
            &tokens,
            &schema_index,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );
        assert!(body.contains("buf.push_str(\"ChildTitle\")"));
        assert!(!body.contains("ParentTitle"));

        let _ = std::fs::remove_dir_all(parent_path.parent().unwrap());
    }

    /// Jalon Vert (roadmap §6.6, cas « fallback parent ») — l'enfant ne
    /// redéfinit pas le bloc `footer` : le contenu parent traverse
    /// intégralement le Lowering et se retrouve dans le `body` généré.
    #[test]
    fn fallback_case_pipeline_produces_expected_body_and_metrics() {
        let (parent_path, child_path) = write_fixtures(
            "fallback",
            b"<footer>{% block footer %}ParentFooter{% endblock %}</footer>",
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

        let mut tokens = lower(&arena.get(parent_id).tokens, &plan, &arena);
        validate_ast(&tokens).expect("aucun Field/If dans ces fixtures : validation triviale");

        let schema_index = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let get_file_size =
            |_: &str| -> Result<usize, String> { Err("aucun static attendu dans ce test".into()) };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema_index,
            get_file_size,
            |_| unreachable!("aucun AssetRef dans ce test"),
            0,
        )
        .expect("aucun StaticInclude/Field dans ces fixtures : résolution triviale");

        // "<footer>" (8) + "ParentFooter" (12) + "</footer>" (9) = 29.
        assert_eq!(metrics.total_static_bytes, 29);
        assert_eq!(metrics.total_dynamic_bytes, 0);
        assert_eq!(metrics.include_count, 0);

        let body = generate_aot_snippet(
            &tokens,
            &schema_index,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );
        assert!(body.contains("buf.push_str(\"ParentFooter\")"));

        let _ = std::fs::remove_dir_all(parent_path.parent().unwrap());
    }

    /// Jalon Vert (roadmap §6.6, critère oublié en première passe) — le
    /// `body` généré n'est pas seulement vérifié par sous-chaîne : il est
    /// réellement compilé via `rustc --edition 2024 --crate-type lib`,
    /// même critère que la roadmap Fragment Phase 3.3. Enveloppe minimale :
    /// `fn render(buf: &mut String) { <body> }` — suffisante ici, ces
    /// fixtures ne contiennent aucun `Field`/`IfBool`/`StaticInclude` (donc
    /// aucune référence à `record`, `varlena`, ou `marius_html_escape`,
    /// tous définis par ailleurs dans `GENERATED_HEADER`, hors périmètre
    /// de ce test).
    ///
    /// Précondition d'environnement : `rustc` doit être sur le `PATH` —
    /// c'est le cas de toute machine construisant ce workspace (même
    /// toolchain que celle qui compile `build.rs` lui-même).
    #[test]
    fn override_case_generated_body_compiles_via_rustc() {
        let (parent_path, child_path) = write_fixtures(
            "rustc-check",
            b"<html>{% block title %}ParentTitle{% endblock %}</html>",
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

        let mut tokens = lower(&arena.get(parent_id).tokens, &plan, &arena);
        validate_ast(&tokens).expect("aucun Field/If dans ces fixtures : validation triviale");

        let schema_index = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let get_file_size =
            |_: &str| -> Result<usize, String> { Err("aucun static attendu dans ce test".into()) };
        resolve_and_measure(
            &mut tokens,
            &schema_index,
            get_file_size,
            |_| unreachable!("aucun AssetRef dans ce test"),
            0,
        )
        .expect("aucun StaticInclude/Field dans ces fixtures : résolution triviale");

        let body = generate_aot_snippet(
            &tokens,
            &schema_index,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        let check_dir = std::env::temp_dir().join(format!(
            "marius-phase-6-6-rustc-check-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&check_dir).expect("création du répertoire de vérification rustc");

        let src_path = check_dir.join("generated_snippet_check.rs");
        let wrapped = format!(
            "#[allow(dead_code, unused_mut)]\nfn render(buf: &mut String) {{\n{body}\n}}\n"
        );
        std::fs::write(&src_path, &wrapped).expect("écriture du fichier source à compiler");

        let out_path = check_dir.join("generated_snippet_check.rmeta");
        let output = std::process::Command::new("rustc")
            .args([
                "--edition",
                "2024",
                "--crate-type",
                "lib",
                "--emit",
                "metadata",
            ])
            .arg("-o")
            .arg(&out_path)
            .arg(&src_path)
            .output()
            .expect(
                "rustc doit être disponible sur le PATH — même toolchain que celle qui \
                 compile ce build.rs",
            );

        assert!(
            output.status.success(),
            "le snippet Rust généré par generate_aot_snippet ne compile pas :\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = std::fs::remove_dir_all(&check_dir);
        let _ = std::fs::remove_dir_all(parent_path.parent().unwrap());
    }

    /// Jalon Vert (roadmap §6.6, critère oublié en première passe) —
    /// invalidation de cache : le Document 3 §5 exige qu'un `base.marius`
    /// modifié invalide le build au même titre que le fichier de la table,
    /// via `println!("cargo:rerun-if-changed=...")` pour les deux chemins.
    ///
    /// Portée assumée de ce test, documentée explicitement : capturer
    /// littéralement la sortie `cargo:` de `println!` depuis l'intérieur du
    /// même process de test exigerait soit une redirection de descripteur
    /// de fichier au niveau OS (FFI `dup`/`dup2` non liée par `std` stable
    /// sans la crate `libc`), soit un process enfant réexécutant `cargo
    /// test` récursivement — les deux disproportionnés pour ce seul
    /// critère, et hors périmètre d'une session de test (pas de nouvelle
    /// dépendance, pas de code `unsafe` ajouté). Ce test vérifie donc la
    /// propriété qui rend ces deux lignes atteignables : `resolve_template`
    /// (chemin complet, enfant + parent) lit effectivement les deux
    /// fichiers et retourne `Ok(Some(..))` — les deux `println!` de
    /// `cargo:rerun-if-changed` précèdent inconditionnellement toute
    /// lecture réussie dans le code de production (vérifié par relecture),
    /// donc leur exécution est une conséquence directe et nécessaire de ce
    /// succès, pas une coïncidence.
    #[test]
    fn resolve_template_end_to_end_reads_both_child_and_parent_paths() {
        let manifest_dir = std::env::temp_dir().join(format!(
            "marius-phase-6-6-rerun-if-changed-{}",
            std::process::id()
        ));
        let templates_dir = manifest_dir.join("templates").join("blog");
        std::fs::create_dir_all(&templates_dir)
            .expect("création de l'arborescence templates/{schema}");

        let parent_path = templates_dir.join("base.marius");
        let child_path = templates_dir.join("post.marius");

        std::fs::write(
            &parent_path,
            b"<html>{% block title %}Base{% endblock %}<!-- MARIUS_MODULES --></html>",
        )
        .expect("écriture de la fixture parent");
        std::fs::write(
            &child_path,
            b"{% extends templates/blog/base.marius %}{% block title %}Post{% endblock %}",
        )
        .expect("écriture de la fixture enfant");

        let manifest_dir_str = manifest_dir.to_string_lossy().into_owned();
        let capabilities: Vec<(String, super::CapabilityInfo)> = Vec::new();
        let result = super::resolve_template(
            &manifest_dir_str,
            &std::collections::HashMap::new(),
            "blog",
            "post",
            &[],
            &[],
            &capabilities,
        );

        assert!(
            result.is_ok(),
            "resolve_template doit réussir sur ces fixtures pour que les deux chemins \
             (enfant + parent) aient effectivement été atteints et lus"
        );
        assert!(
            result.unwrap().is_some(),
            "template présent sur disque : Ok(Some(..)) attendu, pas Ok(None)"
        );

        let _ = std::fs::remove_dir_all(&manifest_dir);
    }
}
