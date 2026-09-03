// crates/core/schema/build/template/static_page.rs

//! Matérialisation des pages sans donnée dynamique (`STATIC_PAGES`) —
//! résolution du pipeline Mode Page puis rendu direct en HTML sur disque,
//! jamais en `render()` compilé (aucune table SQL, aucun `record`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use marius_fragment_forge::{
    AssetLookup, FlatPageToken, PageArena, SchemaIndex, collect_blocks, collect_static_refs,
    detect_extends, extract_static_marker_facts, hoist_and_dedupe_scripts, link, lower,
    parse_page_tokens, relative_path_for_include_str, resolve_and_measure, scan,
    splice_hoisted_scripts, validate_ast,
};

use crate::asset_lookup::resolve_asset_lookup;
use crate::capabilities::CapabilityInfo;
use crate::manifest::AssetEntry;
use crate::modules_lowering::{lower_modules_for_template, render_modules_as_static_html};
use crate::template::common::{read_template_file, split_static_at_marker};
use crate::{MODULES_PLACEHOLDER, SCRIPTS_PLACEHOLDER};

/// Pages sans donnée dynamique : `(schema, table)`, résolues par
/// `resolve_static_page` et matérialisées directement en HTML sur disque
/// (`build/{theme}/{table}.html`) — jamais compilées en `render_page()`
/// dans le binaire, jamais pilotées par `fetch_component_list` (aucune
/// table SQL requise). Décision de session : une page de routage (offline
/// fallback) n'est pas une sous-ressource, elle ne justifie pas une table
/// SQL stub uniquement pour satisfaire la boucle Phase 1.
///
/// Liste explicite, volontairement séparée de `fetch_component_list` — pas
/// fusionnée avec elle : ajouter une page ici n'exige ni migration SQL, ni
/// modification de la boucle Phase 1, seulement un template `.marius`
/// sous `templates/{schema}/{table}.marius` avec zéro référence
/// `{{ record.* }}`/`{% if %}` (garde-fou : voir `resolve_static_page`,
/// `resolve_and_measure` échoue explicitement — `UnknownField` — si cette
/// condition est violée, plutôt que de produire un HTML figé et faux).
pub(crate) const STATIC_PAGES: &[(&str, &str)] = &[("offline", "offline")];

/// Matérialise un flux `FlatPageToken` déjà résolu (`validate_ast` +
/// `resolve_and_measure` passés) directement en `String` HTML — jamais en
/// code Rust généré (`generate_aot_snippet` reste le chemin des pages
/// pilotées par `fetch_component_list`, compilées dans le binaire ; cette
/// fonction sert exclusivement `STATIC_PAGES`, où aucune fonction
/// `render()` n'existe ni n'est nécessaire).
///
/// `Field`/`IfBool`/`EndIf` : n'apparaissent normalement jamais ici — un
/// `SchemaIndex` vide (`fixed: &[], varlena: &[]`) fait déjà échouer
/// `resolve_and_measure` avec `UnknownField` sur la moindre référence
/// `{{ record.* }}`/`{% if %}` avant que cette fonction ne soit atteinte,
/// et `validate_ast` garantit qu'un `EndIf` n'existe jamais sans `IfBool`
/// pour le précéder. Un message d'erreur explicite reste émis plutôt
/// qu'un `unreachable!()` aveugle : un futur changement de
/// `resolve_and_measure` qui laisserait passer ce cas ne doit jamais finir
/// en panic silencieux dans un script de build.
/// `ScriptStart`/`ScriptEnd` : retirés du flux par `hoist_and_dedupe_scripts`
/// (appelé par l'appelant avant celle-ci), jamais présents non plus.
fn emit_static_html<'r>(
    tokens: &[FlatPageToken<'_>],
    manifest_dir: &str,
    schema: &str,
    table: &str,
    resolve_asset_url: impl Fn(&str) -> &'r str,
    // HTML déjà assemblé par render_modules_as_static_html — capacités
    // détectées STATIQUEMENT dans ce template (jamais record.js_deps,
    // structurellement absent ici). Chaîne vide si aucune capacité ne
    // concerne ce template. Inséré verbatim, comme StaticInclude : c'est
    // déjà du HTML final, pas une valeur à échapper.
    static_html_modules: &str,
) -> Result<String, ()> {
    let mut html = String::new();

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => html.push_str(s),
            FlatPageToken::AssetRef(key) => html.push_str(resolve_asset_url(key)),
            FlatPageToken::StaticInclude {
                rel_from_manifest, ..
            } => {
                let path = Path::new(manifest_dir).join(rel_from_manifest);
                let content = std::fs::read_to_string(&path).map_err(|e| {
                    println!(
                        "cargo:error=DB-Forge [{schema}.{table}] : lecture de l'inclusion \
                         statique échouée ({}) : {e}",
                        path.display()
                    );
                })?;
                html.push_str(&content);
            }
            FlatPageToken::Field { .. } | FlatPageToken::IfBool { .. } | FlatPageToken::EndIf => {
                println!(
                    "cargo:error=DB-Forge [{schema}.{table}] : page statique référençant une \
                     donnée dynamique — ne devrait jamais atteindre ce point (SchemaIndex vide, \
                     resolve_and_measure aurait dû échouer en amont avec UnknownField)"
                );
                return Err(());
            }
            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {
                println!(
                    "cargo:error=DB-Forge [{schema}.{table}] : marqueur de script résiduel — \
                     ne devrait jamais atteindre ce point (hoist_and_dedupe_scripts aurait dû \
                     les retirer du flux en amont)"
                );
                return Err(());
            }
            // Émission LITTÉRALE du HTML déjà calculé par
            // render_modules_as_static_html — jamais un no-op (correction
            // apportée après l'addendum Option A d'origine : ce n'était
            // vrai que pour la partie dynamique, structurellement absente
            // ici ; la partie statique, elle, peut légitimement produire du
            // contenu réel — voir doc de resolve_static_page, point 4).
            FlatPageToken::ModulesPlaceholder => html.push_str(static_html_modules),
        }
    }

    Ok(html)
}

/// Pipeline complet pour une entrée de `STATIC_PAGES` — modélisé sur
/// `resolve_page_template` (mêmes fonctions gelées, même ordre :
/// `scan` → `parse_page_tokens` → arène → `collect_blocks`/
/// `collect_static_refs` → `link` → `lower` → `validate_ast` →
/// `hoist_and_dedupe_scripts` (+ splice) → `resolve_and_measure`), à trois
/// différences près, chacune délibérée :
///
///  1. Aucune connexion Postgres, aucun `fetch_component_list` — cette
///     fonction est appelable AVANT même l'ouverture du pool (voir
///     `main()`), puisqu'aucune des informations qu'elle consomme
///     (template, manifeste d'assets) ne vient de la base.
///  2. `SchemaIndex { fixed: &[], varlena: &[] }` — TOUJOURS vide, jamais
///     paramétrable par l'appelant. C'est le garde-fou contre le
///     dynamique, pas une limitation transitoire : `resolve_and_measure`
///     échoue avec `UnknownField` sur la moindre référence
///     `{{ record.* }}`/`{% if %}`, avant que cette fonction n'émette un
///     seul octet — un futur `.marius` de cette liste qui deviendrait
///     dynamique par erreur casse le build plutôt que de servir un HTML
///     figé et faux.
///  3. `emit_static_html` remplace `generate_aot_snippet` — sortie HTML
///     directe, aucun code Rust généré, aucune fonction `render()`
///     compilée dans le binaire pour ces pages.
///  4. `ModulesPlaceholder` (MARIUS_MODULES) est splicé ici comme dans
///     `resolve_page_template`, et PEUT désormais émettre un contenu réel —
///     précision apportée après coup à l'addendum Option A d'origine
///     (HANDOFF-js-deps-capacites-frontend-v2.md, « MARIUS_MODULES agrège
///     deux sources ») : Option A ne concernait que la partie DYNAMIQUE
///     (`record.js_deps`, par définition vide sans `record` — ça reste
///     vrai, `has_record = false` ci-dessous). La partie STATIQUE (scan des
///     marqueurs `class` en dur dans le HTML du layout/template lui-même)
///     est, elle, parfaitement calculable même sans `record` — si
///     `base.marius`/`offline.marius` porte un marqueur statiquement,
///     `emit_static_html` DOIT l'émettre. Ce n'était pas une exception au
///     point 2 ci-dessus à l'origine, ça ne l'est toujours pas : le
///     garde-fou `SchemaIndex` vide protège le DYNAMIQUE, jamais le
///     STATIQUE.
///
/// Mode Page exigé explicitement (`detect_extends` doit être vrai) : les
/// pages de `STATIC_PAGES` connues à ce jour héritent toutes d'un layout
/// commun (`base.marius`). Un Mode Fragment ici retourne une erreur
/// explicite plutôt qu'un comportement deviné — ce cas n'a pas de
/// précédent dans le pipeline Mode Page existant, mieux vaut un
/// `cargo:error` net qu'une hypothèse silencieuse sur une sémantique non
/// éprouvée.
pub(crate) fn resolve_static_page(
    manifest_dir: &str,
    assets: &HashMap<String, AssetEntry>,
    schema: &str,
    table: &str,
    capabilities: &[(String, CapabilityInfo)],
) -> Result<String, ()> {
    let template_path: PathBuf = Path::new(manifest_dir)
        .join("templates")
        .join(schema)
        .join(format!("{table}.marius"));

    // Même invariant d'incrémentalité que `resolve_template` : émission
    // inconditionnelle, avant tout test d'existence.
    println!("cargo:rerun-if-changed={}", template_path.display());
    if let Some(parent_dir) = template_path.parent() {
        println!("cargo:rerun-if-changed={}", parent_dir.display());
    }

    let src = read_template_file(&template_path)?;

    if !detect_extends(&src) {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : page statique en Mode Fragment non \
             supportée ({}) — STATIC_PAGES exige un {{% extends %}} vers un layout commun",
            template_path.display()
        );
        return Err(());
    }

    let child_ast = parse_page_tokens(scan(&src)).map_err(|e| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : enfant Mode Page invalide : {e:?}");
    })?;
    let child_extends = child_ast
        .extends
        .expect("detect_extends garantit extends.is_some() après parse réussi");

    let parent_path = PathBuf::from(relative_path_for_include_str(manifest_dir, child_extends));
    println!("cargo:rerun-if-changed={}", parent_path.display());

    let parent_src = read_template_file(&parent_path)?;
    let parent_ast = parse_page_tokens(scan(&parent_src)).map_err(|e| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : parent Mode Page invalide ({}) : {e:?}",
            parent_path.display()
        );
    })?;

    // Garde single-level — même règle que `resolve_page_template`.
    if parent_ast.extends.is_some() {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : héritage multi-niveaux non supporté \
             ({} déclare lui-même extends)",
            parent_path.display()
        );
        return Err(());
    }

    // Ré-analyse de l'enfant pour admission en arène — même choix que
    // `resolve_page_template` (message distinct d'un bug interne si cette
    // seconde analyse échouait alors que la première a réussi).
    let child_ast_for_arena = parse_page_tokens(scan(&src)).map_err(|e| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : enfant Mode Page invalide \
             (ré-analyse pour admission en arène) : {e:?}"
        );
    })?;

    let mut arena = PageArena::default();
    let child_id = arena.admit(child_ast_for_arena);
    let parent_id = arena.admit(parent_ast);

    let child_blocks = collect_blocks(child_id, &arena.get(child_id).tokens).map_err(|errors| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : blocs enfant mal formés : {errors:?}");
    })?;
    let parent_blocks =
        collect_blocks(parent_id, &arena.get(parent_id).tokens).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : blocs parent mal formés : {errors:?}"
            );
        })?;

    let mut static_refs = collect_static_refs(&arena.get(child_id).tokens);
    static_refs.extend(collect_static_refs(&arena.get(parent_id).tokens));

    let file_exists = |path: &str| -> bool {
        Path::new(&relative_path_for_include_str(manifest_dir, path)).exists()
    };

    let plan =
        link(&parent_blocks, &child_blocks, &static_refs, file_exists).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : linking Mode Page échoué : {errors:?}"
            );
        })?;

    let tokens = lower(&arena.get(parent_id).tokens, &plan, &arena);

    validate_ast(&tokens).map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : Mode Page sémantiquement invalide : {errors:?}"
        );
    })?;

    let (tokens, hoisted_blocks) = hoist_and_dedupe_scripts(tokens).map_err(|e| {
        println!("cargo:error=DB-Forge [{schema}.{table}] : hoisting des scripts échoué : {e}");
    })?;

    let tokens = if hoisted_blocks.is_empty() {
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

    // MARIUS_MODULES — même splice systématique que resolve_page_template
    // (base.marius le porte en permanence). PRÉCISION (addendum
    // « MARIUS_MODULES agrège deux sources », suite à Option A) : la partie
    // DYNAMIQUE reste par définition vide ici (aucun `record`, `has_record
    // = false` ci-dessous) — Option A n'a jamais concerné la partie
    // STATIQUE, elle, parfaitement calculable même sans `record`. Si
    // `base.marius`/`offline.marius` contient un marqueur en dur, il DOIT
    // s'émettre ici aussi.
    let static_facts = extract_static_marker_facts(&tokens);
    let emissions = lower_modules_for_template(capabilities, &static_facts, false);
    let static_html_modules = render_modules_as_static_html(&emissions);
    // Longueur du HTML déjà construit ci-dessus — jamais recalculée
    // séparément : un seul <script> regroupé désormais (pas une somme par
    // capacité), la mesure exacte est déjà entre les mains.
    let modules_static_bytes: usize = static_html_modules.len();

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

    // Garde-fou central de cette fonction (point 2 de la doc ci-dessus) :
    // SchemaIndex toujours vide, jamais un paramètre.
    let schema_index = SchemaIndex {
        fixed: &[],
        varlena: &[],
    };

    let manifest_dir_owned = manifest_dir.to_string();
    let get_file_size = move |rel_path: &str| -> Result<usize, String> {
        std::fs::metadata(Path::new(&manifest_dir_owned).join(rel_path))
            .map(|m| m.len() as usize)
            .map_err(|e| e.to_string())
    };

    let resolve_asset_len = |key: &str| -> AssetLookup { resolve_asset_lookup(assets, key) };
    let resolve_asset_url = |key: &str| -> &str {
        assets.get(key).map(|a| a.url.as_str()).unwrap_or_else(|| {
            panic!("AssetNotFound '{key}' non intercepté par resolve_and_measure")
        })
    };

    resolve_and_measure(
        &mut tokens,
        &schema_index,
        get_file_size,
        resolve_asset_len,
        modules_static_bytes,
    )
    .map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : résolution de la page statique \
             échouée : {errors:?}"
        );
    })?;

    emit_static_html(
        &tokens,
        manifest_dir,
        schema,
        table,
        resolve_asset_url,
        &static_html_modules,
    )
}
