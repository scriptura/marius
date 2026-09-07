// crates/core/schema/build/template/static_page.rs

//! Matérialisation des pages sans donnée dynamique (`STATIC_PAGES`) —
//! résolution du pipeline Mode Page puis rendu direct en HTML sur disque,
//! jamais en `render()` compilé (aucune table SQL, aucun `record`).
//!
//! Réutilise les briques de découverte de chaîne (`{% extends %}`) et
//! d'expansion de fragments (`{% import %}`) de `resolve_page_template`
//! (`crate::template::page`) — même pipeline, mêmes bornes
//! (`MAX_EXTENDS_DEPTH`, `MAX_IMPORT_DEPTH`), même détection de cycle.
//! Ancienne divergence corrigée : cette fonction avait sa propre copie
//! figée du pipeline (garde single-level `extends`, aucune notion
//! d'`import`), jamais mise à jour lors de la généralisation de la chaîne
//! ni lors de l'introduction de `{% import %}` — source du bug « `Import`
//! non développé atteint `lower_leaf_token` » observé sur `offline.offline`,
//! qui partage `base.marius` avec les pages pilotées par
//! `fetch_component_list` sans jamais passer par leur pipeline corrigé.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use marius_fragment_forge::{
    AssetLookup, FlatPageToken, NamedBlockRange, PageArena, PageLinkError, ParsedPageTemplate,
    SchemaIndex, TemplateId, collect_blocks, collect_static_refs, detect_extends,
    extract_static_marker_facts, hoist_and_dedupe_scripts, link_chain, lower, parse_page_tokens,
    relative_path_for_include_str, resolve_and_measure, scan, splice_hoisted_scripts, validate_ast,
};

use crate::asset_lookup::resolve_asset_lookup;
use crate::capabilities::CapabilityInfo;
use crate::manifest::AssetEntry;
use crate::modules_lowering::{lower_modules_for_template, render_modules_as_static_html};
use crate::template::common::{read_template_file, split_static_at_marker};
use crate::template::page::{
    MAX_EXTENDS_DEPTH, discover_imports, render_chain, splice_all_imports,
};
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
/// `resolve_page_template` (`crate::template::page`), dont cette fonction
/// réutilise directement les briques de résolution de chaîne (`discover_
/// imports`, `splice_all_imports`, `render_chain`, `MAX_EXTENDS_DEPTH`,
/// `MAX_IMPORT_DEPTH`) plutôt que d'en garder une copie séparée — c'est
/// cette copie séparée, jamais mise à jour lors des deux généralisations
/// successives de `resolve_page_template` (chaîne `extends` N-aire, puis
/// `{% import %}`), qui a produit la régression corrigée par cette
/// réécriture. Trois différences restent délibérées vis-à-vis de
/// `resolve_page_template` :
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
/// explicite plutôt qu'un comportement deviné.
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
        .expect("detect_extends garantit extends.is_some() après parse réussi")
        .to_string();

    // ── Phase 1 — Découverte de la chaîne extends ───────────────────────
    //
    // `visited_paths[0]` est le chemin réel du template de la table (connu
    // ici, contrairement à `resolve_page_template` qui ne reçoit que le
    // contenu — cf. sa propre doc pour le label synthétique qu'elle utilise
    // à défaut).
    let mut visited_paths: Vec<PathBuf> = vec![template_path.clone()];
    let mut sources: Vec<String> = vec![src];
    let mut current_extends = child_extends;

    loop {
        if sources.len() >= MAX_EXTENDS_DEPTH {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : chaîne extends trop profonde \
                 (max {MAX_EXTENDS_DEPTH} fichiers) : {} -> (arrêté avant lecture de `{current_extends}`)",
                render_chain(&visited_paths)
            );
            return Err(());
        }

        let path = PathBuf::from(relative_path_for_include_str(
            manifest_dir,
            &current_extends,
        ));

        if visited_paths.contains(&path) {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : cycle détecté dans la chaîne \
                 extends : {} -> {} (déjà présent plus haut dans la chaîne)",
                render_chain(&visited_paths),
                path.display()
            );
            return Err(());
        }

        if !path.exists() {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : extends introuvable — {} déclare \
                 `{current_extends}`, mais {} n'existe pas",
                render_chain(&visited_paths),
                path.display()
            );
            return Err(());
        }

        println!("cargo:rerun-if-changed={}", path.display());
        let ancestor_src = read_template_file(&path)?;

        let peek_ast = parse_page_tokens(scan(&ancestor_src)).map_err(|e| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : maillon extends invalide ({}) : {e:?}",
                path.display()
            );
        })?;
        let next_extends = peek_ast.extends.map(str::to_string);

        visited_paths.push(path);
        sources.push(ancestor_src);

        match next_extends {
            Some(next) => current_extends = next,
            None => break, // ce maillon est le Root
        }
    }

    println!(
        "cargo:warning=DB-Forge [{schema}.{table}] : chaîne extends : {}",
        render_chain(&visited_paths)
    );

    // ── Phase 1.5 — Découverte des imports de chaque maillon ────────────
    let mut import_sources: Vec<String> = Vec::new();
    let mut import_trees = Vec::with_capacity(sources.len());

    for (index, ancestor_src) in sources.iter().enumerate() {
        let mut ancestry: Vec<PathBuf> = vec![visited_paths[index].clone()];
        let tree = discover_imports(
            schema,
            table,
            manifest_dir,
            ancestor_src,
            &visited_paths[index],
            0,
            &mut ancestry,
            &mut import_sources,
        )?;
        import_trees.push(tree);
    }

    // ── Phase 2 — Parsing réel + expansion des imports + admission ──────
    let mut arena = PageArena::default();
    let mut chain_ids: Vec<TemplateId> = Vec::with_capacity(sources.len());

    for (index, ancestor_src) in sources.iter().enumerate() {
        let ast = parse_page_tokens(scan(ancestor_src)).map_err(|e| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : maillon Mode Page invalide \
                 (ré-analyse pour admission en arène, {}) : {e:?}",
                visited_paths[index].display()
            );
        })?;

        let extends = ast.extends;
        let tokens = splice_all_imports(
            ast.tokens,
            &import_trees[index],
            &import_sources,
            schema,
            table,
        )?;

        chain_ids.push(arena.admit(ParsedPageTemplate { extends, tokens }));
    }

    let root_id = *chain_ids
        .last()
        .expect("chain_ids non vide : au moins le Root, garanti par la boucle Phase 1");
    let root_path = visited_paths
        .last()
        .expect("visited_paths non vide : au moins le Root, garanti par la boucle Phase 1");

    let path_for_template = |id: TemplateId| -> &Path {
        let idx = chain_ids
            .iter()
            .position(|&x| x == id)
            .expect("template admis dans cette chaîne — invariant garanti par la boucle ci-dessus");
        &visited_paths[idx]
    };

    let mut chain_blocks_owned: Vec<Vec<NamedBlockRange<'_>>> = Vec::with_capacity(chain_ids.len());
    for &id in &chain_ids {
        let blocks = collect_blocks(id, &arena.get(id).tokens).map_err(|errors| {
            println!(
                "cargo:error=DB-Forge [{schema}.{table}] : blocs mal formés ({}) : {errors:?}",
                path_for_template(id).display()
            );
        })?;
        chain_blocks_owned.push(blocks);
    }
    let chain_blocks: Vec<&[NamedBlockRange<'_>]> =
        chain_blocks_owned.iter().map(Vec::as_slice).collect();

    let mut static_refs = Vec::new();
    for &id in &chain_ids {
        static_refs.extend(collect_static_refs(&arena.get(id).tokens));
    }

    let file_exists = |path: &str| -> bool {
        Path::new(&relative_path_for_include_str(manifest_dir, path)).exists()
    };

    let plan = link_chain(&chain_blocks, &static_refs, file_exists).map_err(|errors| {
        for &error in &errors {
            match error {
                PageLinkError::OrphanBlock { name, template } => {
                    println!(
                        "cargo:error=DB-Forge [{schema}.{table}] : bloc `{name}` déclaré dans \
                         {} ne correspond à aucun slot du Root ({}) — bloc mort, à supprimer \
                         ou renommer",
                        path_for_template(template).display(),
                        root_path.display()
                    );
                }
                other => println!(
                    "cargo:error=DB-Forge [{schema}.{table}] : linking Mode Page échoué : {other:?}"
                ),
            }
        }
    })?;

    let tokens = lower(&arena.get(root_id).tokens, &plan, &arena);

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
                    root_path.display()
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
                root_path.display()
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
