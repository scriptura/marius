// crates/core/schema/build/template/dynamic.rs

//! Point d'entrée du pipeline Voie B pour un composant piloté par
//! `fetch_component_list` — dispatche vers Mode Page
//! (`crate::template::page::resolve_page_template`, si `{% extends %}`
//! détecté) ou traite le flux en Mode Fragment directement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use marius_fragment_forge::{
    AssetLookup, SchemaIndex, TemplateMetrics, VarlenField, detect_extends, generate_aot_snippet,
    generate_segmented_snippet, parse_page_tokens, parse_tokens, resolve_and_measure, scan,
    validate_ast,
};

use crate::asset_lookup::resolve_asset_lookup;
use crate::capabilities::CapabilityInfo;
use crate::manifest::AssetEntry;
use crate::template::common::read_template_file;
use crate::template::page::resolve_page_template;

/// Tente de résoudre le template `.marius` d'une table via le pipeline
/// Fragment-Forge complet : scan → (parse_tokens | Mode Page : parse_page_tokens
/// → arène → link → lower) → validate_ast → resolve_and_measure →
/// generate_aot_snippet — convergence sur `Vec<FlatPageToken<'src>>` avant le
/// point de jonction (Document 3 §2).
///
/// Chemin attendu : `{manifest_dir}/templates/{schema}/{table}.marius`.
///
/// Invariant d'incrémentalité (Phase 6.7, post-mortem Dispatcher/PoC figée
/// en cache) : `cargo:rerun-if-changed` pour `template_path` et son
/// répertoire parent sont émis de façon INCONDITIONNELLE, avant tout test
/// d'existence. Cargo fige la liste des chemins surveillés au dernier build
/// réussi du script — un `rerun-if-changed` émis seulement dans la branche
/// "fichier trouvé" ne crée jamais l'arête de dépendance tant que le fichier
/// n'existe pas, et une création ultérieure du fichier reste alors invisible
/// à l'incrémentalité (le stub `Ok(None)` reste self-consistant à jamais).
/// Le rerun-if-changed sur le répertoire parent est le filet de sécurité :
/// son mtime change dès qu'un fichier y est créé, ce qui couvre précisément
/// ce cas de bord.
///
/// Retourne :
///   `Ok(None)`        : fichier absent — cargo:warning émis, fallback stub.
///   `Ok(Some((body, metrics)))` : template résolu avec succès, Mode
///                       Fragment ou Mode Page indifféremment — même
///                       structure de retour, aucun marqueur de mode.
///   `Err(())`         : toute erreur de parsing/validation/résolution
///                       (Mode Fragment), ou tout échec de
///                       `resolve_page_template` (Mode Page — Phase 6.2 :
///                       `detect_extends` est le point de décision de mode
///                       unique de ce fichier ; Phase 6.3 : E/S parent,
///                       garde single-level ; Phase 6.4 : admission en
///                       arène ; Phase 6.5 : collecte de blocs, extraction
///                       `static`, calcul du `LinkPlan` ; Phase 6.6 :
///                       Lowering, jonction avec le pipeline gelé).
///                       cargo:error déjà émis ; l'appelant doit exit(1).
pub(crate) fn resolve_template(
    manifest_dir: &str,
    assets: &HashMap<String, AssetEntry>,
    schema: &str,
    table: &str,
    fixed: &[marius_fragment_forge::FieldSpec],
    varlena: &[VarlenField],
    capabilities: &[(String, CapabilityInfo)],
) -> Result<Option<(String, TemplateMetrics)>, ()> {
    let template_path: PathBuf = Path::new(manifest_dir)
        .join("templates")
        .join(schema)
        .join(format!("{table}.marius"));

    // Émission inconditionnelle — avant le test d'existence. Voir invariant
    // d'incrémentalité ci-dessus.
    println!("cargo:rerun-if-changed={}", template_path.display());
    if let Some(parent_dir) = template_path.parent() {
        println!("cargo:rerun-if-changed={}", parent_dir.display());
    }

    println!("cargo:warning=DB-Forge [{schema}.{table}]"); // TODO : retirer ce log de débogage une fois la Phase 6.6 stabilisée.
    println!("cargo:warning=template={}", template_path.display()); // TODO : retirer ce log de débogage une fois la Phase 6.6 stabilisée.

    if !template_path.exists() {
        println!(
            "cargo:warning=DB-Forge [{schema}.{table}] : aucun template trouvé \
             ({}) — render() vide (capacité 0).",
            template_path.display(),
        );
        return Ok(None);
    }

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
            assets,
            schema,
            table,
            fixed,
            varlena,
            &src,
            child_extends,
            capabilities,
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

    // Résolution des {% asset key %} — manifeste réel, même câblage que
    // resolve_page_template ci-dessus (suggestion calculée par
    // resolve_asset_lookup, jamais par fragment-forge).
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
        // Fragment isolé, jamais de <head>, jamais de marqueur
        // MODULES_PLACEHOLDER dans ce flux — 0 en dur, `capabilities` n'a
        // ici aucune raison d'être consulté (ce paramètre de fonction
        // n'existe que pour le forward vers `resolve_page_template` dans la
        // branche `extends` ci-dessus).
        0,
    )
    .map_err(|errors| {
        println!(
            "cargo:error=DB-Forge [{schema}.{table}] : résolution du template échouée : {errors:?}"
        );
    })?;

    // CONTRAT-implementation-projection-segmentee.md, Étape 5 — même
    // branchement que resolve_page_template ci-dessus.
    let has_segment = varlena.iter().any(|v| v.is_segment);
    let body = if has_segment {
        generate_segmented_snippet(&tokens, &schema_index, resolve_asset_url, "")
    } else {
        generate_aot_snippet(&tokens, &schema_index, resolve_asset_url, "")
    };

    Ok(Some((body, metrics)))
}
