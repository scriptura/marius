// crates/core/schema/build.rs

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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use marius_db_forge::{
    PrimaryKey, build_field_specs, check_no_name_collision, fetch_columns, fetch_component_list,
    fetch_max_id, fetch_pk_column, fetch_varlena_cols, validate_layout, write_collector,
    write_from_impl, write_projection_stub, write_row_struct, write_section_header,
    write_store_struct, write_varlen_owned_struct,
};

use marius_fragment_forge::{
    AssetLookup, FlatPageToken, PageArena, SchemaIndex, TemplateMetrics, VarlenField,
    collect_blocks, collect_static_refs, detect_extends, extract_static_class_tokens,
    generate_aot_snippet, generate_segmented_snippet, hoist_and_dedupe_scripts, link, lower,
    parse_page_tokens, parse_tokens, relative_path_for_include_str, resolve_and_measure, scan,
    splice_hoisted_scripts, validate_ast,
};

/// Marqueur textuel du point d'injection des `<script>` hissés — décision
/// actée en session : pas de nouveau token dans l'AST gelé de
/// `fragment-forge`, une simple constante de chaîne recherchée comme
/// SOUS-CHAÎNE parmi les `FlatPageToken::Static` du layout PARENT (Mode
/// Page), après `lower`. Un commentaire HTML, jamais interprété par ce
/// moteur de template (`{% %}`/`{{ }}` sont les seules syntaxes actives) :
/// à écrire tel quel dans le `<head>` du layout de base, où les scripts
/// doivent apparaître.
const SCRIPTS_PLACEHOLDER: &str = "<!-- MARIUS_SCRIPTS -->";

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
const MODULES_PLACEHOLDER: &str = "<!-- MARIUS_MODULES -->";

/// Une entrée de `theme.toml` → `[scripts.capabilities.<nom>]`.
///
/// Désérialisation MINIMALE, propre à `build.rs` — délibérément dupliquée
/// depuis `crates/marius-assets/src/config.rs` plutôt que partagée : même
/// interdiction de couplage de types Rust entre `marius-assets` et les
/// crates de la Forge que pour `suggest_asset_key`/`levenshtein` ci-dessus
/// (Roadmap `marius-assets` §2.1). `markers` est déserialisé pour validation
/// (non-vide) mais n'entre dans aucun codegen ici — sa consommation est
/// exclusivement SQL (`compute_js_deps`, chantier séparé).
#[derive(Deserialize)]
struct CapabilityConfig {
    entry: String,
    markers: Vec<String>,
    activation: String,
}

#[derive(Deserialize, Default)]
struct ScriptsSection {
    #[serde(default)]
    capabilities: HashMap<String, CapabilityConfig>,
}

/// Vue partielle de `theme.toml` — seule la section `[scripts.capabilities]`
/// intéresse ce lowering ; `[theme]`, `[styles]`, `[sprites]`, etc. sont
/// ignorées ici (déjà day-to-day consommées par `marius-assets`, jamais par
/// `db-forge`).
#[derive(Deserialize)]
struct ThemeTomlScriptsOnly {
    #[serde(default)]
    scripts: ScriptsSection,
}

/// Une capacité entièrement validée et résolue — bit, activation, URL,
/// marqueurs — prête pour le lowering par template. Calculée UNE SEULE FOIS
/// pour tout le build (`validate_capabilities`), jamais recalculée par
/// template : c'est `lower_modules_for_template` qui la croise, à chaque
/// appel, avec le HTML statique du template en cours.
struct CapabilityInfo {
    bit: i64,
    activation: String,
    url: String,
    /// Marqueurs de classe déclenchant cette capacité — nécessaires ici
    /// pour le scan statique (comparés à `extract_static_class_tokens`),
    /// pas seulement pour la validation de non-vacuité.
    markers: Vec<String>,
}

/// Une émission calculée pour UN template donné, par `lower_modules_for_template`.
///
/// `bit: None` — émission INCONDITIONNELLE : soit le marqueur a été détecté
/// statiquement dans le HTML du template (constant folding AOT — Forge sait
/// déjà, à la compilation, que ce template en a besoin, quel que soit le
/// record), soit l'appelant n'a structurellement aucun `record`
/// (`STATIC_PAGES`) et l'émission ne peut alors provenir QUE du cas
/// statique.
///
/// `bit: Some(BIT)` — émission CONDITIONNELLE : marqueur absent
/// statiquement, dépend de `record.js_deps` — n'existe jamais pour un
/// appelant sans `record`.
struct ModuleEmission {
    html: String,
    bit: Option<i64>,
}

/// Résultat du lowering AOT de `ModulesPlaceholder`, assemblé en CODE RUST
/// (Mode Page, `resolve_page_template`) — jamais utilisé par
/// `resolve_static_page`, qui assemble du HTML littéral directement
/// (`render_modules_as_static_html`, pas de code Rust généré pour
/// `STATIC_PAGES`).
struct ModulesLowering {
    /// Code Rust déjà assemblé pour CE template — une ligne par capacité
    /// concernée : `buf.push_str(...)` inconditionnel (marqueur détecté
    /// statiquement) ou `if record.js_deps & BIT != 0 { buf.push_str(...); }`
    /// (marqueur absent statiquement, dépend du record). Chaîne vide si
    /// aucune capacité ne concerne ce template.
    snippet: String,
    /// Somme des octets des snippets HTML RÉELLEMENT concernés par ce
    /// template (inconditionnels + pire cas des tests dynamiques) — jamais
    /// les capacités qui ne concernent pas du tout ce template (exclues en
    /// amont par `lower_modules_for_template`, jamais comptées ici).
    static_bytes: usize,
}

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
const STATIC_PAGES: &[(&str, &str)] = &[("offline", "offline")];

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

// =============================================================================
// Manifeste d'assets — marius-assets-specification.md §7, Roadmap §1.4 (clos).
//
// Structure miroir du TOML `[assets."clé"]` (dictionnaire, pas [[asset]]) —
// désérialisation directe en HashMap, lookup O(1) pour chaque {% asset %}
// rencontré. Décision actée : voir échange de session, format dictionnaire
// retenu explicitement au détriment du tableau pour cette raison.
//
// serde + toml : build-dependencies uniquement (Cargo.toml) — jamais liées
// au binaire du Shell ni du Core (no_std). Coût de parsing entièrement
// confiné à la machine hôte, phase AOT.
// =============================================================================

#[derive(Deserialize)]
struct AssetManifest {
    assets: HashMap<String, AssetEntry>,
}

/// Une entrée du manifeste — champs de la spec §7. Seuls `url` (et sa
/// longueur) sont consommés par ce build.rs ; `path`/`mime`/`size`/`hash`/
/// `version` sont ceux que le Shell consomme au runtime (`handlers.rs`),
/// partagés depuis le même fichier — producteur unique, spec §8.
#[derive(Deserialize)]
struct AssetEntry {
    url: String,
    #[allow(dead_code)]
    path: String,
    #[allow(dead_code)]
    mime: String,
    #[allow(dead_code)]
    size: u64,
    #[allow(dead_code)]
    hash: String,
    #[allow(dead_code)]
    version: String,
}

/// Nom du thème actif. Décision actée en session : un seul thème possible
/// pour cette v1 — pas de mécanisme de sélection (env var, section
/// Cargo.toml, configuration multi-thème) nécessaire tant que cet invariant
/// tient. Si un jour plusieurs thèmes coexistent, ce point redevient ouvert
/// et cette constante devra être remplacée par un paramètre réel — mais ce
/// n'est plus une inconnue pour la v1.
const THEME_NAME: &str = "default";

/// Répertoire de build du thème actif : `{workspace_root}/build/{theme}`,
/// où `workspace_root` = `CARGO_MANIFEST_DIR` (= `crates/core/schema`) +
/// trois remontées (`schema → core → crates → racine`) — PAS deux, piège
/// déjà documenté pour `manifest.toml` ci-dessous, désormais factorisé ici
/// pour que la page statique (§ plus bas) ne puisse pas le redupliquer
/// avec un nombre de remontées différent par accident de copier-coller.
fn build_dir(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .join("../../../build")
        .join(THEME_NAME)
}

/// Résout le chemin du manifeste d'assets et l'enregistre auprès de Cargo.
///
/// `cargo:rerun-if-changed` émis de façon INCONDITIONNELLE, avant tout test
/// d'existence, y compris sur le répertoire parent — piège documenté dans
/// `guide-cycle-de-vie-runtime.md` §2 : une émission conditionnelle ne
/// rattrape jamais un fichier qui apparaît après le premier build. Même
/// discipline que `resolve_template` (ligne ~316) pour les templates.
fn load_asset_manifest(manifest_dir: &str) -> Result<HashMap<String, AssetEntry>, ()> {
    let manifest_path = build_dir(manifest_dir).join("manifest.toml");

    // Émission inconditionnelle — avant le test d'existence.
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    if let Some(parent_dir) = manifest_path.parent() {
        println!("cargo:rerun-if-changed={}", parent_dir.display());
    }

    let raw = std::fs::read_to_string(&manifest_path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : manifeste d'assets introuvable ({}) : {e}",
            manifest_path.display()
        );
    })?;

    let parsed: AssetManifest = toml::from_str(&raw).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : manifeste d'assets malformé ({}) : {e}",
            manifest_path.display()
        );
    })?;

    Ok(parsed.assets)
}

/// Répertoire source du thème (contient `theme.toml`) — symétrique de
/// `build_dir` ci-dessus (trois remontées identiques depuis
/// `CARGO_MANIFEST_DIR` = `crates/core/schema`), confirmé par le message
/// d'usage littéral de `marius-assets`
/// (`marius-assets <chemin-du-dossier-de-theme> (ex: ./assets/default)`) :
/// `workspace_root/assets/{THEME_NAME}`, jamais `workspace_root/build/...`
/// (qui est un répertoire ENTIÈREMENT généré, jamais une source).
fn theme_source_dir(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .join("../../../assets")
        .join(THEME_NAME)
}

/// Emplacement du registre de bits `scripts_registry.lock` — CONFIRMÉ en
/// session : `assets/{THEME_NAME}/scripts_registry.lock`, sibling de
/// `theme.toml` (même répertoire source, même statut de fichier manuel
/// versionné).
fn scripts_registry_path(manifest_dir: &str) -> PathBuf {
    theme_source_dir(manifest_dir).join("scripts_registry.lock")
}

/// Identifiant Rust/JS valide — `activation` (theme.toml) est injecté
/// VERBATIM, comme identifiant nu (pas comme littéral de chaîne échappé),
/// dans le code Rust généré (`import{{{activation} as _n}}...`) : un texte
/// libre non validé romprait la génération de code, voire l'injecterait.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Valide et résout GLOBALEMENT (une seule fois pour tout le build) la
/// table canonique des capacités `js_deps` — bijection `theme.toml` ↔
/// `scripts_registry.lock`, validité/unicité des bits, `activation` valide,
/// résolution de chaque `entry` contre le manifeste d'assets. Ne connaît
/// aucun template spécifique : `lower_modules_for_template` croise cette
/// table, à chaque appel, avec le HTML statique du template en cours.
///
/// Retourne les capacités triées par nom (ordre déterministe, reproductible
/// d'un build à l'autre — jamais l'ordre d'itération d'une `HashMap`).
fn validate_capabilities(
    manifest_dir: &str,
    assets: &HashMap<String, AssetEntry>,
) -> Result<Vec<(String, CapabilityInfo)>, ()> {
    let theme_toml_path = theme_source_dir(manifest_dir).join("theme.toml");
    println!("cargo:rerun-if-changed={}", theme_toml_path.display());

    let raw_theme = std::fs::read_to_string(&theme_toml_path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : theme.toml introuvable ({}) : {e}",
            theme_toml_path.display()
        );
    })?;
    let theme: ThemeTomlScriptsOnly = toml::from_str(&raw_theme).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : theme.toml malformé ({}) : {e}",
            theme_toml_path.display()
        );
    })?;

    let capabilities = &theme.scripts.capabilities;

    let registry_path = scripts_registry_path(manifest_dir);
    println!("cargo:rerun-if-changed={}", registry_path.display());

    if capabilities.is_empty() {
        // Rien à valider — scripts_registry.lock peut même ne pas exister
        // tant qu'aucune capacité ne l'exige.
        return Ok(Vec::new());
    }

    let raw_registry = std::fs::read_to_string(&registry_path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : {} capacité(s) déclarée(s) dans [scripts.capabilities] \
             mais scripts_registry.lock introuvable ({}) : {e}",
            capabilities.len(),
            registry_path.display()
        );
    })?;
    let registry: HashMap<String, i64> = toml::from_str(&raw_registry).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : scripts_registry.lock malformé ({}) : {e}",
            registry_path.display()
        );
    })?;

    let mut errors: Vec<String> = Vec::new();

    // Entrées actives = clé ne commençant pas par "_retired_" — un nom
    // retiré n'est jamais réattribuable, mais reste dans le fichier comme
    // mémoire d'identité (le bit ne doit jamais être réutilisé par un
    // futur nom différent).
    let active_registry: HashMap<&str, i64> = registry
        .iter()
        .filter(|(k, _)| !k.starts_with("_retired_"))
        .map(|(k, v)| (k.as_str(), *v))
        .collect();

    // ── Bijection theme.toml capacités ↔ registre actif ─────────────────
    for name in capabilities.keys() {
        if !active_registry.contains_key(name.as_str()) {
            errors.push(format!(
                "capacité '{name}' déclarée dans [scripts.capabilities] mais absente de \
                 scripts_registry.lock — attribution de bit manquante"
            ));
        }
    }
    for name in active_registry.keys() {
        if !capabilities.contains_key(*name) {
            errors.push(format!(
                "bit '{name}' présent dans scripts_registry.lock (actif) mais aucune capacité \
                 correspondante dans [scripts.capabilities] de theme.toml — capacité retirée \
                 sans préfixe '_retired_', ou registre en avance sur la configuration"
            ));
        }
    }

    // ── Validité et unicité des bits ─────────────────────────────────────
    let mut seen_bits: HashMap<i64, &str> = HashMap::new();
    for (name, bit) in &active_registry {
        if *bit <= 0 || (*bit & (*bit - 1)) != 0 {
            errors.push(format!(
                "bit invalide pour '{name}' : {bit} n'est pas une puissance de deux \
                 strictement positive"
            ));
            continue;
        }
        if let Some(other) = seen_bits.insert(*bit, name) {
            errors.push(format!(
                "collision de bit {bit} entre '{other}' et '{name}' — deux capacités ne \
                 peuvent jamais partager le même bit"
            ));
        }
    }

    if !errors.is_empty() {
        for e in &errors {
            println!("cargo:error=DB-Forge [scripts_registry.lock] : {e}");
        }
        return Err(());
    }

    // ── Résolution — ordre déterministe (tri par nom), jamais l'ordre
    // d'itération d'une HashMap, qui varie d'un build à l'autre et
    // romprait la reproductibilité du fichier généré. ───────────────────
    let mut names: Vec<&String> = capabilities.keys().collect();
    names.sort();

    let mut result: Vec<(String, CapabilityInfo)> = Vec::new();

    for name in names {
        let cap = &capabilities[name];
        let bit = active_registry[name.as_str()];

        if !is_valid_identifier(&cap.activation) {
            errors.push(format!(
                "[scripts.capabilities.{name}].activation = {:?} n'est pas un identifiant \
                 Rust/JS valide — jamais injecté tel quel dans le code généré",
                cap.activation
            ));
            continue;
        }

        if cap.markers.is_empty() {
            errors.push(format!(
                "[scripts.capabilities.{name}].markers est vide — cette capacité ne pourrait \
                 jamais être déclenchée, ni par aucun contenu éditorial (content.compute_js_deps), \
                 ni par aucun template (scan statique)"
            ));
            continue;
        }

        let manifest_key = format!("{name}.js");
        let url = match assets.get(&manifest_key) {
            Some(entry) => entry.url.clone(),
            None => {
                errors.push(format!(
                    "'{name}' : clé '{manifest_key}' absente du manifeste d'assets — \
                     [scripts.capabilities.{name}].entry ({:?}) n'a produit aucune entrée via \
                     run_scripts_pipeline",
                    cap.entry
                ));
                continue;
            }
        };

        result.push((
            name.clone(),
            CapabilityInfo {
                bit,
                activation: cap.activation.clone(),
                url,
                markers: cap.markers.clone(),
            },
        ));
    }

    if !errors.is_empty() {
        for e in &errors {
            println!("cargo:error=DB-Forge [theme.toml scripts.capabilities] : {e}");
        }
        return Err(());
    }

    Ok(result)
}

/// Lowering `ModulesPlaceholder` POUR UN TEMPLATE DONNÉ — jamais un calcul
/// global (contrairement à `validate_capabilities`). Croise la table
/// canonique des capacités avec les classes détectées statiquement dans CE
/// template (`extract_static_class_tokens`, fragment-forge, appliquée sur
/// le flux déjà fusionné parent+enfant) et la présence ou non d'un
/// `record` dans le générateur appelant.
///
/// INVARIANT GARANTI PAR CONSTRUCTION (addendum HANDOFF-js-deps-capacites-
/// frontend-v2.md, « MARIUS_MODULES agrège deux sources ») : pour chaque
/// capacité et chaque template, cette fonction produit AU PLUS UNE émission
/// — jamais un test `if record.js_deps & BIT != 0 { ... }` en plus d'une
/// émission déjà inconditionnelle. La présence statique domine
/// systématiquement le besoin dynamique :
///   - marqueur présent dans le HTML statique du template → émission
///     inconditionnelle (`bit: None`, constant folding AOT — Forge sait
///     déjà, à la compilation, que ce template en a besoin, quel que soit
///     le record) ;
///   - marqueur absent statiquement, `has_record = true` → émission
///     conditionnelle (`bit: Some(BIT)`) — le besoin dépend du contenu
///     éditorial de CE record précis, décidé à l'écriture par
///     `content.compute_js_deps` ;
///   - marqueur absent statiquement, `has_record = false` (`STATIC_PAGES`)
///     → rien : ni test possible (pas de record), ni besoin structurel
///     (sinon il serait présent statiquement dans le template).
fn lower_modules_for_template(
    capabilities: &[(String, CapabilityInfo)],
    static_classes: &std::collections::HashSet<String>,
    has_record: bool,
) -> Vec<ModuleEmission> {
    let mut out = Vec::new();

    for (_, cap) in capabilities {
        let static_hit = cap.markers.iter().any(|m| static_classes.contains(m));

        // Un seul <script type="module"> par capacité active : import ESM
        // nommé, aliasé (chaque `if`/bloc est un scope Rust distinct, `_n`
        // n'entre jamais en conflit entre capacités), appelé immédiatement.
        let html = format!(
            r#"<script type="module">import{{{} as _n}}from{:?};_n();</script>"#,
            cap.activation, cap.url
        );

        if static_hit {
            out.push(ModuleEmission { html, bit: None });
        } else if has_record {
            out.push(ModuleEmission {
                html,
                bit: Some(cap.bit),
            });
        }
        // else : STATIC_PAGES, marqueur absent statiquement — rien à
        // émettre pour cette capacité sur ce template (doc ci-dessus).
    }

    out
}

/// Assemble les émissions en CODE RUST — Mode Page (`resolve_page_template`),
/// pour insertion verbatim par `generate_aot_snippet`/`generate_segmented_snippet`.
fn render_modules_as_rust(emissions: &[ModuleEmission]) -> ModulesLowering {
    use std::fmt::Write as _;

    let mut snippet = String::new();
    let mut static_bytes = 0usize;

    for e in emissions {
        static_bytes += e.html.len();
        match e.bit {
            None => writeln!(snippet, "buf.push_str({:?});", e.html).unwrap(),
            Some(bit) => writeln!(
                snippet,
                "if record.js_deps & {bit} != 0 {{ buf.push_str({:?}); }}",
                e.html
            )
            .unwrap(),
        }
    }

    ModulesLowering {
        snippet,
        static_bytes,
    }
}

/// Assemble les émissions en HTML LITTÉRAL — `STATIC_PAGES`
/// (`resolve_static_page`), jamais de code Rust généré pour ce pipeline.
///
/// `has_record = false` dans l'appel à `lower_modules_for_template` qui
/// produit `emissions` garantit STRUCTURELLEMENT qu'aucune émission ici ne
/// porte `bit: Some(_)` — le `assert!` ci-dessous défend cet invariant
/// plutôt que de le supposer silencieusement : un `Some` rencontré ici
/// serait un bug de câblage (mauvais `has_record` passé en amont), jamais
/// un cas runtime légitime à absorber.
fn render_modules_as_static_html(emissions: &[ModuleEmission]) -> String {
    let mut html = String::new();
    for e in emissions {
        assert!(
            e.bit.is_none(),
            "DB-Forge : bit conditionnel rencontré pour une page STATIC_PAGES — bug de \
             câblage (has_record aurait dû valoir false dans lower_modules_for_template)"
        );
        html.push_str(&e.html);
    }
    html
}

// =============================================================================
// Résolution d'asset avec diagnostic — remplace les anciennes closures
// `|key| assets.get(key).map(|a| a.url.len())` (Option<usize>) câblées
// directement à `resolve_and_measure`. `fragment-forge` ne possède pas les
// clés du manifeste (aucun I/O dans ce crate — invariant inchangé) : le
// calcul de la suggestion diagnostique doit donc vivre ici, seul endroit
// où les clés candidates existent réellement en mémoire.
// =============================================================================

/// Résout un `{% asset key %}` contre le manifeste chargé — clé absente :
/// suggestion calculée ici, jamais dans `fragment-forge` (voir doc
/// d'`AssetLookup`, marius-fragment-forge/src/lib.rs).
fn resolve_asset_lookup(assets: &HashMap<String, AssetEntry>, key: &str) -> AssetLookup {
    match assets.get(key) {
        Some(entry) => AssetLookup::Found(entry.url.len()),
        None => AssetLookup::NotFound {
            suggestion: suggest_asset_key(key, assets),
        },
    }
}

/// Duplication délibérée de `suggest_variable`/`levenshtein` (`marius-assets`,
/// pipeline `[styles]` Phase 3) — même algorithme, aucune dépendance
/// partagée : la Roadmap `marius-assets` (§2.1) interdit explicitement tout
/// couplage de types Rust entre `marius-assets` et les crates de la Forge ;
/// `build.rs` n'a aucune raison d'y déroger pour emprunter dix lignes de
/// calcul de distance d'édition.
///
/// Même hiérarchie de confiance que côté `marius-assets`, jamais mélangée
/// dans un seul message : casse différente (quasi certaine) avant distance
/// de Levenshtein (une piste, pas une certitude), bornée à 2 pour éviter
/// une suggestion trompeuse sur une clé sans rapport réel.
fn suggest_asset_key(key: &str, assets: &HashMap<String, AssetEntry>) -> Option<String> {
    if let Some(exact_ci) = assets.keys().find(|k| k.eq_ignore_ascii_case(key)) {
        return Some(format!(
            "la casse ne correspond pas : le manifeste contient '{exact_ci}', pas '{key}'"
        ));
    }

    assets
        .keys()
        .map(|k| (k, levenshtein(key, k)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(k, _)| format!("vouliez-vous dire '{k}' ?"))
}

/// Distance de Levenshtein — deux lignes de tableau roulées (`prev`/`curr`),
/// pas de matrice complète : un manifeste de thème compte au plus quelques
/// centaines de clés, seule l'empreinte mémoire par comparaison justifie ce
/// choix, pas la complexité (O(n·m) par paire est hors de propos ici).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// =============================================================================
// Tests — résolution d'asset avec diagnostic.
//
// Trois responsabilités testées séparément, sans se chevaucher :
//   - `levenshtein` : la fonction de distance elle-même, cas classiques.
//   - `suggest_asset_key` : la hiérarchie de confiance (casse avant
//     Levenshtein, aucune suggestion trompeuse au-delà du seuil).
//   - `resolve_asset_lookup` : le câblage — clé trouvée → `Found`, clé
//     absente → `NotFound` portant exactement la suggestion calculée.
// =============================================================================

#[cfg(test)]
mod tests_asset_lookup {
    use super::{AssetEntry, AssetLookup, levenshtein, resolve_asset_lookup, suggest_asset_key};
    use std::collections::HashMap;

    /// Entrée de manifeste minimale — seul `url` (et sa longueur) est
    /// consommé par `resolve_asset_lookup`, le reste n'a besoin d'être
    /// que syntaxiquement présent (voir `#[allow(dead_code)]` sur
    /// `AssetEntry` : ces champs sont pour le Shell au runtime, pas pour
    /// ce build.rs — même remarque que la doc de la struct).
    fn make_entry(url: &str) -> AssetEntry {
        AssetEntry {
            url: url.to_string(),
            path: String::new(),
            mime: String::new(),
            size: 0,
            hash: String::new(),
            version: String::new(),
        }
    }

    fn sample_manifest() -> HashMap<String, AssetEntry> {
        let mut m = HashMap::new();
        m.insert(
            "utils.svg".to_string(),
            make_entry("/sprites/utils.4c4e9.svg"),
        );
        m.insert(
            "players.svg".to_string(),
            make_entry("/sprites/players.76165.svg"),
        );
        m
    }

    // ── levenshtein ──────────────────────────────────────────────────────────

    #[test]
    fn levenshtein_identical_strings_is_zero() {
        assert_eq!(levenshtein("utils.svg", "utils.svg"), 0);
    }

    #[test]
    fn levenshtein_classic_kitten_sitting_is_three() {
        // Exemple canonique de la littérature — sert de garde-fou contre
        // une régression silencieuse de l'algorithme (ex. coût de
        // substitution mal posé, tableau non roulé correctement).
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn levenshtein_against_empty_string_is_the_other_length() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn levenshtein_single_typo_is_one() {
        // Le cas réel qui a motivé cette fonctionnalité : "util.svg" saisi
        // pour "utils.svg" — une lettre manquante, distance 1.
        assert_eq!(levenshtein("util.svg", "utils.svg"), 1);
    }

    // ── suggest_asset_key ────────────────────────────────────────────────────

    /// Priorité 1 : une correspondance insensible à la casse doit produire
    /// le message "la casse ne correspond pas", jamais un "vouliez-vous
    /// dire" générique — même clé, seule la casse diffère, la confiance
    /// est maximale et le message doit le refléter.
    #[test]
    fn suggest_asset_key_case_mismatch_takes_priority() {
        let manifest = sample_manifest();
        let suggestion = suggest_asset_key("UTILS.SVG", &manifest)
            .expect("une entrée ne différant que par la casse doit produire une suggestion");
        assert!(
            suggestion.contains("la casse ne correspond pas"),
            "message inattendu : {suggestion:?}"
        );
        assert!(
            suggestion.contains("utils.svg"),
            "message inattendu : {suggestion:?}"
        );
    }

    /// Priorité 2 : à défaut de correspondance de casse, une clé à
    /// distance ≤ 2 doit être proposée comme "vouliez-vous dire".
    /// C'est le cas exact rencontré en session : "util.svg" pour
    /// "utils.svg" (distance 1).
    #[test]
    fn suggest_asset_key_close_typo_suggests_nearest_key() {
        let manifest = sample_manifest();
        let suggestion = suggest_asset_key("util.svg", &manifest)
            .expect("distance 1 : une suggestion est attendue");
        assert_eq!(suggestion, "vouliez-vous dire 'utils.svg' ?");
    }

    /// Au-delà du seuil (distance > 2) et sans correspondance de casse :
    /// aucune suggestion — mieux vaut se taire qu'orienter vers une clé
    /// sans rapport réel. Cas réel : "silos/195v.svg", dont la présence
    /// d'un `/` seul suffit à l'éloigner de toute clé plate du manifeste.
    #[test]
    fn suggest_asset_key_no_close_match_returns_none() {
        let manifest = sample_manifest();
        assert_eq!(suggest_asset_key("silos/195v.svg", &manifest), None);
    }

    /// Manifeste vide : aucune candidate à proposer, `None` — pas de panique
    /// sur un registre sans entrée (`.min_by_key` sur un itérateur vide).
    #[test]
    fn suggest_asset_key_empty_manifest_returns_none() {
        let manifest: HashMap<String, AssetEntry> = HashMap::new();
        assert_eq!(suggest_asset_key("anything.svg", &manifest), None);
    }

    // ── resolve_asset_lookup ─────────────────────────────────────────────────

    #[test]
    fn resolve_asset_lookup_found_returns_url_length() {
        let manifest = sample_manifest();
        let result = resolve_asset_lookup(&manifest, "utils.svg");
        assert_eq!(result, AssetLookup::Found("/sprites/utils.4c4e9.svg".len()));
    }

    #[test]
    fn resolve_asset_lookup_missing_key_carries_the_computed_suggestion() {
        let manifest = sample_manifest();
        match resolve_asset_lookup(&manifest, "util.svg") {
            AssetLookup::NotFound { suggestion } => {
                assert_eq!(
                    suggestion,
                    Some("vouliez-vous dire 'utils.svg' ?".to_string())
                );
            }
            other => panic!("NotFound attendu, obtenu : {other:?}"),
        }
    }

    #[test]
    fn resolve_asset_lookup_missing_key_without_candidate_carries_none() {
        let manifest = sample_manifest();
        match resolve_asset_lookup(&manifest, "silos/195v.svg") {
            AssetLookup::NotFound { suggestion } => {
                assert_eq!(suggestion, None);
            }
            other => panic!("NotFound attendu, obtenu : {other:?}"),
        }
    }
}

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

/// Cherche `marker` comme SOUS-CHAÎNE d'un `FlatPageToken::Static` du flux
/// — pas une correspondance de token entier : en pratique, le marqueur est
/// noyé dans un bloc HTML statique plus large (`<head>...<!--MARIUS_
/// SCRIPTS-->...</head>` forme un seul `Static` tant qu'aucune directive
/// `{% %}`/`{{ }}` ne le coupe). Si trouvé, scinde ce token en (avant,
/// après) — en omettant la moitié vide s'il y en a une (le marqueur en
/// tout début ou toute fin de bloc ne doit pas produire un `Static("")`
/// inutile — pas une simplification cosmétique : `generate_aot_snippet`
/// émettrait un `buf.push_str("")` mort dans le code généré, un `Static`
/// vide n'a aucune raison structurelle d'exister dans ce flux.
///
/// Retourne `(flux_modifié, indice_où_insérer_le_bloc_de_scripts)` — cet
/// indice tombe exactement entre les deux moitiés, prêt pour
/// `splice_hoisted_scripts`. `None` si le marqueur n'apparaît dans aucun
/// `Static` du flux.
fn split_static_at_marker<'src>(
    mut tokens: Vec<FlatPageToken<'src>>,
    marker: &str,
) -> Option<(Vec<FlatPageToken<'src>>, usize)> {
    let (index, pos) = tokens.iter().enumerate().find_map(|(i, t)| match t {
        FlatPageToken::Static(s) => s.find(marker).map(|pos| (i, pos)),
        _ => None,
    })?;

    let mut tail = tokens.split_off(index + 1);
    let marked = tokens
        .pop()
        .expect("index provient de tokens.iter(), non vide ici");
    let full = match marked {
        FlatPageToken::Static(s) => s,
        _ => unreachable!("le filtre ci-dessus ne retient que des Static"),
    };
    let before = &full[..pos];
    let after = &full[pos + marker.len()..];

    if !before.is_empty() {
        tokens.push(FlatPageToken::Static(before));
    }
    let splice_index = tokens.len();
    if !after.is_empty() {
        tokens.push(FlatPageToken::Static(after));
    }
    tokens.append(&mut tail);

    Some((tokens, splice_index))
}

#[cfg(test)]
mod tests_split_static_at_marker {
    use super::split_static_at_marker;
    use marius_fragment_forge::FlatPageToken;

    #[test]
    fn marker_embedded_in_larger_static_splits_around_it() {
        let tokens = vec![FlatPageToken::Static(
            "<head><title>x</title><!-- MARIUS_SCRIPTS --></head>",
        )];

        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head><title>x</title>"),
                FlatPageToken::Static("</head>"),
            ]
        );
        assert_eq!(splice_index, 1); // entre les deux moitiés
    }

    /// Pas de `Static("")` mort dans le flux quand le marqueur est en
    /// tout début ou toute fin d'un bloc — voir doc de la fonction.
    #[test]
    fn marker_at_start_omits_empty_before_half() {
        let tokens = vec![FlatPageToken::Static("<!-- MARIUS_SCRIPTS --></head>")];
        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();
        assert_eq!(result, vec![FlatPageToken::Static("</head>")]);
        assert_eq!(splice_index, 0);
    }

    #[test]
    fn marker_at_end_omits_empty_after_half() {
        let tokens = vec![FlatPageToken::Static("<head><!-- MARIUS_SCRIPTS -->")];
        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();
        assert_eq!(result, vec![FlatPageToken::Static("<head>")]);
        assert_eq!(splice_index, 1);
    }

    #[test]
    fn preserves_tokens_before_and_after_the_marked_one() {
        let tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("<title>x</title><!-- MARIUS_SCRIPTS -->"),
            FlatPageToken::Static("</head><body>"),
        ];

        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::Static("<title>x</title>"),
                FlatPageToken::Static("</head><body>"),
            ]
        );
        assert_eq!(splice_index, 2);
    }

    #[test]
    fn marker_absent_returns_none() {
        let tokens = vec![FlatPageToken::Static("<head></head>")];
        assert!(split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").is_none());
    }
}

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
fn resolve_page_template<'src>(
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
    let static_classes = extract_static_class_tokens(&tokens);
    let emissions = lower_modules_for_template(capabilities, &static_classes, true);
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
fn resolve_static_page(
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
    let static_classes = extract_static_class_tokens(&tokens);
    let emissions = lower_modules_for_template(capabilities, &static_classes, false);
    let static_html_modules = render_modules_as_static_html(&emissions);
    let modules_static_bytes: usize = emissions.iter().map(|e| e.html.len()).sum();

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
fn resolve_template(
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
    let assets = load_asset_manifest(&manifest_dir).unwrap_or_else(|()| {
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
    let capabilities = validate_capabilities(&manifest_dir, &assets).unwrap_or_else(|()| {
        std::process::exit(1);
    });

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
