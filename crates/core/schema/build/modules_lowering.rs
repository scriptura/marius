// crates/core/schema/build/modules_lowering.rs

//! Lowering AOT du `ModulesPlaceholder`, par template : croisement de la
//! table canonique des capacités ([`crate::capabilities`]) avec les faits
//! statiques d'un template donné, puis assemblage en code Rust
//! (`render_modules_as_rust`, Mode Page) ou en HTML littéral
//! (`render_modules_as_static_html`, `STATIC_PAGES`).

use marius_fragment_forge::StaticMarkerFacts;

use crate::capabilities::{CapabilityInfo, ResolvedDep};
use crate::markers::MarkerPredicate;

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
///
/// Composants BRUTS (`activation`/`url`), jamais une balise `<script>`
/// pré-assemblée — c'est `render_modules_as_rust`/
/// `render_modules_as_static_html` qui décident du regroupement final
/// (un seul `<script type="module">` par page, HANDOFF-js-deps-capacites-
/// frontend-v2.md, addendum « regroupement des modules »). L'alias
/// d'import (`_0`, `_1`, ...) est assigné à l'assemblage, par position
/// dans la liste — jamais porté ici.
pub(crate) struct ModuleEmission {
    activation: String,
    url: String,
    bit: Option<i64>,
    /// Copie des dépendances déjà résolues de la capacité d'origine —
    /// agrégées et dédupliquées PAR TEMPLATE par `aggregate_deps`, jamais
    /// ici (une `ModuleEmission` reste une vue par capacité, pas la vue
    /// finale par template).
    deps: Vec<ResolvedDep>,
}

/// Résultat du lowering AOT de `ModulesPlaceholder`, assemblé en CODE RUST
/// (Mode Page, `resolve_page_template`) — jamais utilisé par
/// `resolve_static_page`, qui assemble du HTML littéral directement
/// (`render_modules_as_static_html`, pas de code Rust généré pour
/// `STATIC_PAGES`).
pub(crate) struct ModulesLowering {
    /// Code Rust déjà assemblé pour CE template — AU PLUS UN
    /// `<script type="module">` regroupant tous les imports (dans l'ordre
    /// des capacités), puis tous les appels d'activation (même ordre).
    /// Chaque import/appel individuel reste `buf.push_str(...)`
    /// inconditionnel (marqueur détecté statiquement) ou
    /// `if record.js_deps & BIT != 0 { buf.push_str(...); }` (marqueur
    /// absent statiquement, dépend du record) ; la balise `<script>`
    /// elle-même est soit garantie présente (au moins une capacité
    /// statique), soit enveloppée dans un test global sur le OU binaire de
    /// tous les bits concernés (aucune capacité statique — jamais de
    /// `<script></script>` vide). Chaîne vide si aucune capacité ne
    /// concerne ce template.
    pub(crate) snippet: String,
    /// Pire cas d'octets pour CE template — deux balises d'enveloppe
    /// (présentes une seule fois, jamais par capacité) + somme de tous les
    /// imports/appels RÉELLEMENT concernés par ce template (exclus en
    /// amont par `lower_modules_for_template`, jamais comptés ici).
    pub(crate) static_bytes: usize,
}

/// Lowering `ModulesPlaceholder` POUR UN TEMPLATE DONNÉ — jamais un calcul
/// global (contrairement à `validate_capabilities`). Croise la table
/// canonique des capacités avec les quatre catégories de faits statiques
/// extraites de CE template (`extract_static_marker_facts`, fragment-forge,
/// appliquée sur le flux déjà fusionné parent+enfant) et la présence ou non
/// d'un `record` dans le générateur appelant.
///
/// INVARIANT GARANTI PAR CONSTRUCTION (addendum HANDOFF-js-deps-capacites-
/// frontend-v2.md, « MARIUS_MODULES agrège deux sources ») : pour chaque
/// capacité et chaque template, cette fonction produit AU PLUS UNE émission
/// — jamais un test `if record.js_deps & BIT != 0 { ... }` en plus d'une
/// émission déjà inconditionnelle. La présence statique domine
/// systématiquement le besoin dynamique, quelle que soit la FORME du
/// marqueur qui a matché (`Class`/`Id`/`Attribute`/`Element` traités à
/// parfaite égalité ici) :
///   - au moins un marqueur de la capacité matche un fait statique du
///     template → émission inconditionnelle (`bit: None`, constant folding
///     AOT — Forge sait déjà, à la compilation, que ce template en a
///     besoin, quel que soit le record) ;
///   - aucun marqueur ne matche statiquement, `cap.bit` est `Some(_)`
///     (`content_driven = true`, résolu par `validate_capabilities`) et
///     `has_record = true` → émission conditionnelle (`bit: Some(BIT)`) —
///     le besoin dépend du contenu éditorial de CE record précis, décidé
///     à l'écriture par `content.compute_js_deps` ;
///   - aucun marqueur ne matche statiquement ET (`cap.bit` est `None`,
///     `content_driven = false`, OU `has_record = false`, `STATIC_PAGES`)
///     → rien : une capacité non content-driven ne peut structurellement
///     jamais dépendre de `record.js_deps`, quel que soit `has_record` ;
///     et un appelant sans `record` ne peut de toute façon jamais tester
///     un bit, quel que soit `cap.bit`.
pub(crate) fn lower_modules_for_template(
    capabilities: &[(String, CapabilityInfo)],
    facts: &StaticMarkerFacts,
    has_record: bool,
) -> Vec<ModuleEmission> {
    let mut out = Vec::new();

    for (_, cap) in capabilities {
        let static_hit = cap.markers.iter().any(|m| match m {
            MarkerPredicate::Class(name) => facts.classes.contains(name),
            MarkerPredicate::Id(name) => facts.ids.contains(name),
            MarkerPredicate::Attribute(name) => facts.data_attributes.contains(name),
            MarkerPredicate::Element(name) => facts.elements.contains(name),
        });

        if static_hit {
            out.push(ModuleEmission {
                activation: cap.activation.clone(),
                url: cap.url.clone(),
                bit: None,
                deps: cap.deps.clone(),
            });
        } else if let Some(bit) = cap.bit {
            // `cap.bit` n'est `Some(_)` que pour une capacité
            // `content_driven = true` (résolution dans
            // `validate_capabilities`) — une capacité `content_driven =
            // false` n'entre jamais dans cette branche, quel que soit
            // `has_record`.
            if has_record {
                out.push(ModuleEmission {
                    activation: cap.activation.clone(),
                    url: cap.url.clone(),
                    bit: Some(bit),
                    deps: cap.deps.clone(),
                });
            }
            // else : STATIC_PAGES — pas de record possible, rien à
            // émettre, même pour une capacité content-driven.
        }
        // else : cap.bit == None (content_driven = false) et marqueur
        // absent statiquement — rien à émettre pour cette capacité sur ce
        // template, quel que soit has_record (doc ci-dessus).
    }

    out
}

/// Une dépendance UNIQUE (par identité canonique), pour un template donné,
/// après agrégation de toutes les émissions qui la consomment.
///
/// `bit: None` — au moins une émission consommatrice est statique
/// (domination, même règle que pour le `<script type="module">` lui-même,
/// SPEC `deps` §3) : la dépendance devient inconditionnelle, quel que soit
/// le nombre d'autres capacités dynamiques qui la partagent aussi.
/// `bit: Some(mask)` — OU binaire des bits de TOUTES les émissions
/// dynamiques qui la consomment, aucune n'étant statique.
struct AggregatedDep {
    dep: ResolvedDep,
    bit: Option<i64>,
}

/// Agrège et déduplique les dépendances de TOUTES les émissions d'un
/// template — jamais recalculé par dépendance individuelle. Déduplication
/// par IDENTITÉ CANONIQUE (`ResolvedDep::canonical_key`), jamais par URL
/// finale (SPEC `deps` §3) : deux émissions référençant la même clé
/// produisent une seule entrée agrégée, quelle que soit l'URL (qui sera de
/// toute façon identique, `CanonicalAssetId` étant unique par construction
/// — SPEC-canonical-asset-identity.md §1 — mais la clé reste le contrat
/// explicite, jamais une coïncidence exploitée implicitement).
///
/// Comparaison en O(n²) sur le nombre de dépendances DISTINCTES déjà vues
/// — même justification que `hoist_and_dedupe_scripts` (fragment-forge) :
/// une poignée de dépendances par template en pratique, pas la peine
/// d'imposer `Hash` pour un gain invisible à cette échelle.
///
/// Ordre déterministe : première apparition, dans l'ordre déjà
/// déterministe des émissions (lui-même hérité du tri par nom de
/// `validate_capabilities`).
fn aggregate_deps(emissions: &[ModuleEmission]) -> Vec<AggregatedDep> {
    let mut out: Vec<AggregatedDep> = Vec::new();

    for emission in emissions {
        for dep in &emission.deps {
            match out
                .iter_mut()
                .find(|agg| agg.dep.canonical_key == dep.canonical_key)
            {
                Some(existing) => {
                    existing.bit = match (existing.bit, emission.bit) {
                        (None, _) | (_, None) => None, // domination statique
                        (Some(mask), Some(bit)) => Some(mask | bit),
                    };
                }
                None => out.push(AggregatedDep {
                    dep: dep.clone(),
                    bit: emission.bit,
                }),
            }
        }
    }

    out
}

/// Balise `<script>` littérale pour UNE dépendance déjà résolue — jamais
/// assemblée en groupe (contrairement aux imports/appels de capacités,
/// SPEC `deps` §4 : chaque dépendance reste sa propre balise `<script>`
/// distincte, jamais fusionnée dans le `<script type="module">` de la
/// capacité qui la consomme).
fn render_dep_tag(dep: &ResolvedDep) -> String {
    if dep.module {
        format!("<script type=\"module\" src={:?}></script>", dep.url)
    } else {
        format!("<script src={:?} defer></script>", dep.url)
    }
}

/// Assemble les émissions en CODE RUST — Mode Page (`resolve_page_template`),
/// pour insertion verbatim par `generate_aot_snippet`/`generate_segmented_snippet`.
///
/// REGROUPEMENT (HANDOFF-js-deps-capacites-frontend-v2.md, addendum
/// « regroupement des modules ») : un seul `<script type="module">` au
/// maximum par page — tous les `import` d'abord, tous les appels
/// d'activation ensuite (spec §3 : contrainte d'ordre à l'intérieur du
/// bloc). Alias `_0`, `_1`, ... séquentiels, assignés par POSITION dans
/// `emissions` (ordre déjà déterministe, hérité de `CapabilityInfo` —
/// jamais un parcours de `HashMap`), locaux à ce bloc, jamais stables
/// entre deux pages.
///
/// La présence même du `<script>` dépend de la composition de `emissions` :
///   - au moins une émission `bit: None` (capacité statique) → le tag est
///     GARANTI présent (constant folding — Forge sait, à la compilation,
///     qu'au moins un import sera toujours émis pour ce template) : pas de
///     test enveloppe, seuls les imports/appels individuellement
///     conditionnels (`bit: Some`) restent testés un par un ;
///   - aucune émission statique, uniquement des `bit: Some(_)` → la
///     présence du tag dépend du runtime : enveloppé dans
///     `if record.js_deps & MASK != 0 { ... }` où `MASK` est le OU binaire
///     de tous les bits concernés — jamais un `<script></script>` vide si
///     aucun bit de `MASK` n'est actif ;
///   - `emissions` vide → rien émis, `snippet` reste `""`.
pub(crate) fn render_modules_as_rust(emissions: &[ModuleEmission]) -> ModulesLowering {
    use std::fmt::Write as _;

    if emissions.is_empty() {
        return ModulesLowering {
            snippet: String::new(),
            static_bytes: 0,
        };
    }

    const OPEN: &str = "<script type=\"module\">";
    const CLOSE: &str = "</script>";

    // Une pièce (import + appel) par émission, alias assigné par position —
    // calculée une fois, réutilisée pour le corps ET pour le pire cas de
    // capacité (jamais recalculée deux fois).
    struct Piece {
        import: String,
        call: String,
        bit: Option<i64>,
    }

    let mut pieces: Vec<Piece> = Vec::with_capacity(emissions.len());
    let mut has_static = false;
    let mut dynamic_mask: i64 = 0;

    for (i, e) in emissions.iter().enumerate() {
        let import = format!("import{{{} as _{i}}}from{:?};", e.activation, e.url);
        let call = format!("_{i}();");
        match e.bit {
            None => has_static = true,
            Some(bit) => dynamic_mask |= bit,
        }
        pieces.push(Piece {
            import,
            call,
            bit: e.bit,
        });
    }

    // Dépendances de chargement (`deps`, SPEC `deps`) — agrégées et
    // dédupliquées UNE FOIS pour tout le template (§3), jamais par
    // émission individuelle. Balises `<script>` totalement indépendantes
    // du regroupement import/appel ci-dessus : chacune sa propre balise,
    // son propre bit, jamais fusionnée dans `OPEN`/`CLOSE`.
    let aggregated_deps = aggregate_deps(emissions);
    let dep_tags: Vec<(String, Option<i64>)> = aggregated_deps
        .iter()
        .map(|agg| (render_dep_tag(&agg.dep), agg.bit))
        .collect();

    // Pire cas de capacité : TOUTES les pièces émises simultanément (jamais
    // une hypothèse d'exclusivité mutuelle entre bits) + les deux balises
    // d'enveloppe, présentes une seule fois quel que soit le nombre de
    // capacités, + toutes les balises de dépendances déjà dédupliquées.
    let static_bytes = OPEN.len()
        + CLOSE.len()
        + pieces
            .iter()
            .map(|p| p.import.len() + p.call.len())
            .sum::<usize>()
        + dep_tags.iter().map(|(tag, _)| tag.len()).sum::<usize>();

    let mut snippet = String::new();

    // Dépendances D'ABORD, dans l'ordre requis chargement → activation
    // (deps → entry → activation) — jamais à l'intérieur de `emit_body`,
    // jamais soumises au regroupement `OPEN`/`CLOSE` du `<script
    // type="module">` de la capacité : chaque dépendance est sa propre
    // balise `<script>` de premier niveau, conditionnelle à son propre bit
    // agrégé (`None` = au moins un consommateur statique, domination).
    for (tag, bit) in &dep_tags {
        match bit {
            None => writeln!(snippet, "buf.push_str({tag:?});").unwrap(),
            Some(bit) => writeln!(
                snippet,
                "if record.js_deps & {bit} != 0 {{ buf.push_str({tag:?}); }}"
            )
            .unwrap(),
        }
    }

    // Corps commun aux deux cas (tag garanti vs tag conditionnel) — évite
    // de dupliquer la boucle d'assemblage des imports/appels.
    let emit_body = |snippet: &mut String| {
        writeln!(snippet, "buf.push_str({OPEN:?});").unwrap();
        for p in &pieces {
            match p.bit {
                None => writeln!(snippet, "buf.push_str({:?});", p.import).unwrap(),
                Some(bit) => writeln!(
                    snippet,
                    "if record.js_deps & {bit} != 0 {{ buf.push_str({:?}); }}",
                    p.import
                )
                .unwrap(),
            }
        }
        for p in &pieces {
            match p.bit {
                None => writeln!(snippet, "buf.push_str({:?});", p.call).unwrap(),
                Some(bit) => writeln!(
                    snippet,
                    "if record.js_deps & {bit} != 0 {{ buf.push_str({:?}); }}",
                    p.call
                )
                .unwrap(),
            }
        }
        writeln!(snippet, "buf.push_str({CLOSE:?});").unwrap();
    };

    if has_static {
        emit_body(&mut snippet);
    } else {
        writeln!(snippet, "if record.js_deps & {dynamic_mask} != 0 {{").unwrap();
        emit_body(&mut snippet);
        writeln!(snippet, "}}").unwrap();
    }

    ModulesLowering {
        snippet,
        static_bytes,
    }
}

/// Assemble les émissions en HTML LITTÉRAL — `STATIC_PAGES`
/// (`resolve_static_page`), jamais de code Rust généré pour ce pipeline.
///
/// Même regroupement que `render_modules_as_rust` (un seul `<script>`,
/// imports puis appels, alias séquentiels par position), mais TOUJOURS
/// inconditionnel ici : `has_record = false` dans l'appel à
/// `lower_modules_for_template` qui produit `emissions` garantit
/// STRUCTURELLEMENT qu'aucune émission ne porte `bit: Some(_)` — le
/// `assert!` ci-dessous défend cet invariant plutôt que de le supposer
/// silencieusement : un `Some` rencontré ici serait un bug de câblage
/// (mauvais `has_record` passé en amont), jamais un cas runtime légitime.
pub(crate) fn render_modules_as_static_html(emissions: &[ModuleEmission]) -> String {
    if emissions.is_empty() {
        return String::new();
    }

    for e in emissions {
        assert!(
            e.bit.is_none(),
            "DB-Forge : bit conditionnel rencontré pour une page STATIC_PAGES — bug de \
             câblage (has_record aurait dû valoir false dans lower_modules_for_template)"
        );
    }

    // Dépendances D'ABORD (deps → entry → activation) — mêmes garanties
    // d'agrégation/dédoublonnage que `render_modules_as_rust`. `agg.bit`
    // ne peut être ici que `None` : la domination statique (§ci-dessus)
    // s'applique transitivement, chaque émission contribuant à l'agrégat
    // ayant elle-même `bit: None` — défendu explicitement plutôt que
    // supposé, même discipline que l'`assert!` sur `emissions` ci-dessus.
    let mut html = String::new();
    for agg in aggregate_deps(emissions) {
        assert!(
            agg.bit.is_none(),
            "DB-Forge : dépendance conditionnelle rencontrée pour une page STATIC_PAGES — \
             ne peut survenir que si une émission source portait déjà bit: Some(_), déjà exclu \
             par l'assert! ci-dessus"
        );
        html.push_str(&render_dep_tag(&agg.dep));
    }

    html.push_str("<script type=\"module\">");
    for (i, e) in emissions.iter().enumerate() {
        html.push_str(&format!(
            "import{{{} as _{i}}}from{:?};",
            e.activation, e.url
        ));
    }
    for (i, _) in emissions.iter().enumerate() {
        html.push_str(&format!("_{i}();"));
    }
    html.push_str("</script>");
    html
}

#[cfg(test)]
mod tests_module_grouping {
    use super::*;

    /// Capacité `content_driven = true` — `bit` toujours résolu
    /// (`Some(bit)`), pour les tests exerçant la branche dynamique du
    /// lowering (comportement historique de ce helper, avant
    /// l'introduction de `content_driven`).
    fn cap(
        name: &str,
        bit: i64,
        activation: &str,
        url: &str,
        markers: &[&str],
    ) -> (String, CapabilityInfo) {
        cap_with_deps(name, bit, activation, url, markers, &[])
    }

    fn cap_with_deps(
        name: &str,
        bit: i64,
        activation: &str,
        url: &str,
        markers: &[&str],
        deps: &[ResolvedDep],
    ) -> (String, CapabilityInfo) {
        (
            name.to_string(),
            CapabilityInfo {
                bit: Some(bit),
                activation: activation.to_string(),
                url: url.to_string(),
                // `parse_marker` est la même fonction utilisée par
                // `validate_capabilities` — un marker de test invalide doit
                // planter la construction de la fixture, jamais être
                // silencieusement ignoré ou réinterprété.
                markers: markers
                    .iter()
                    .map(|s| {
                        parse_marker(s).unwrap_or_else(|e| panic!("fixture de test invalide : {e}"))
                    })
                    .collect(),
                deps: deps.to_vec(),
            },
        )
    }

    /// Capacité `content_driven = false` — `bit: None`, jamais résolu,
    /// quel que soit `has_record` au lowering. Vérifie le comportement
    /// introduit par le découplage `content_driven`/`scripts_registry.lock`.
    fn cap_static_only(
        name: &str,
        activation: &str,
        url: &str,
        markers: &[&str],
    ) -> (String, CapabilityInfo) {
        (
            name.to_string(),
            CapabilityInfo {
                bit: None,
                activation: activation.to_string(),
                url: url.to_string(),
                markers: markers
                    .iter()
                    .map(|s| {
                        parse_marker(s).unwrap_or_else(|e| panic!("fixture de test invalide : {e}"))
                    })
                    .collect(),
                deps: Vec::new(),
            },
        )
    }

    fn dep(canonical_key: &str, url: &str, module: bool) -> ResolvedDep {
        ResolvedDep {
            canonical_key: canonical_key.to_string(),
            url: url.to_string(),
            module,
        }
    }

    /// Faits statiques vides — aucun marqueur, quelle que soit sa forme, ne
    /// matche dans le template. Remplace l'ancien
    /// `std::collections::HashSet::new()` nu, désormais insuffisant : la
    /// signature de `lower_modules_for_template` porte les quatre
    /// catégories, jamais une seule.
    fn empty_facts() -> StaticMarkerFacts {
        StaticMarkerFacts {
            classes: std::collections::HashSet::new(),
            ids: std::collections::HashSet::new(),
            data_attributes: std::collections::HashSet::new(),
            elements: std::collections::HashSet::new(),
        }
    }

    /// Faits statiques ne renseignant QUE des classes — la forme la plus
    /// fréquente dans ce module de tests (héritée de l'ancien modèle
    /// `static_classes`). `ids`/`data_attributes`/`elements` restent vides,
    /// jamais peuplés par erreur avec des noms de classe.
    fn facts_with_classes(classes: &[&str]) -> StaticMarkerFacts {
        StaticMarkerFacts {
            classes: classes.iter().map(|s| s.to_string()).collect(),
            ids: std::collections::HashSet::new(),
            data_attributes: std::collections::HashSet::new(),
            elements: std::collections::HashSet::new(),
        }
    }

    // ── lower_modules_for_template + render_modules_as_rust (Mode Page) ────

    #[test]
    fn zero_capability_emits_no_script() {
        let capabilities: Vec<(String, CapabilityInfo)> = vec![];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        assert!(emissions.is_empty());
        assert_eq!(lowering.snippet, "");
        assert_eq!(lowering.static_bytes, 0);
    }

    #[test]
    fn single_dynamic_capability_wraps_single_script_conditionally() {
        let capabilities = vec![cap(
            "map",
            2,
            "initMapsSystem",
            "/scripts/map.HASH.js",
            &[".map"],
        )];
        let static_facts = empty_facts(); // aucun marqueur statique
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].bit, Some(2));

        let lowering = render_modules_as_rust(&emissions);
        assert_eq!(lowering.snippet.matches("<script").count(), 1);
        assert_eq!(lowering.snippet.matches("</script>").count(), 1);
        assert!(lowering.snippet.contains("if record.js_deps & 2 != 0 {"));
        assert!(lowering.snippet.contains("as _0"));
        assert!(lowering.snippet.contains("_0();"));
    }

    #[test]
    fn single_static_capability_emits_unconditional_script() {
        let capabilities = vec![cap(
            "line-mark",
            8,
            "boot",
            "/scripts/line-mark.HASH.js",
            &[".add-line-marks"],
        )];
        let static_facts = facts_with_classes(&["add-line-marks"]);

        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].bit, None);

        let lowering = render_modules_as_rust(&emissions);
        assert_eq!(lowering.snippet.matches("<script").count(), 1);
        // Émission inconditionnelle — jamais de test record.js_deps pour
        // cette capacité, détectée statiquement.
        assert!(!lowering.snippet.contains("record.js_deps"));
        assert!(lowering.snippet.contains("as _0"));
    }

    #[test]
    fn multiple_capabilities_produce_single_script_imports_then_calls() {
        let capabilities = vec![
            cap(
                "line-mark",
                8,
                "boot",
                "/scripts/line-mark.HASH.js",
                &[".add-line-marks"],
            ),
            cap(
                "map",
                2,
                "initMapsSystem",
                "/scripts/map.HASH.js",
                &[".map"],
            ),
        ];
        // Aucun marqueur statique — les deux restent dynamiques.
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        assert_eq!(emissions.len(), 2);

        let lowering = render_modules_as_rust(&emissions);
        // Un seul script au total — jamais un par capacité.
        assert_eq!(lowering.snippet.matches("<script").count(), 1);
        assert_eq!(lowering.snippet.matches("</script>").count(), 1);

        // Tous les imports doivent précéder tous les appels d'activation.
        let last_import_pos = lowering.snippet.rfind("import{").unwrap();
        let first_call_pos = lowering.snippet.find("_0();").unwrap();
        assert!(
            first_call_pos > last_import_pos,
            "les imports doivent précéder les appels : {}",
            lowering.snippet
        );
    }

    #[test]
    fn aliases_are_sequential_and_distinct() {
        let capabilities = vec![
            cap("a", 1, "initA", "/scripts/a.js", &[".a-marker"]),
            cap("b", 2, "initB", "/scripts/b.js", &[".b-marker"]),
            cap("c", 4, "initC", "/scripts/c.js", &[".c-marker"]),
        ];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        for i in 0..3 {
            assert!(lowering.snippet.contains(&format!("as _{i}")));
            assert!(lowering.snippet.contains(&format!("_{i}();")));
        }
    }

    #[test]
    fn order_is_deterministic_matches_capabilities_order() {
        let capabilities = vec![
            cap(
                "alpha",
                1,
                "initAlpha",
                "/scripts/alpha.js",
                &[".alpha-marker"],
            ),
            cap("beta", 2, "initBeta", "/scripts/beta.js", &[".beta-marker"]),
        ];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        let pos_alpha = lowering.snippet.find("initAlpha").unwrap();
        let pos_beta = lowering.snippet.find("initBeta").unwrap();
        assert!(
            pos_alpha < pos_beta,
            "l'ordre doit suivre celui de `capabilities` (déjà trié par nom en amont), \
             jamais un ordre inversé ou dépendant d'un parcours de HashMap"
        );
    }

    #[test]
    fn mix_static_and_dynamic_capability_single_script_no_outer_guard() {
        let capabilities = vec![
            cap(
                "line-mark",
                8,
                "boot",
                "/scripts/line-mark.js",
                &[".add-line-marks"],
            ),
            cap("map", 2, "initMapsSystem", "/scripts/map.js", &[".map"]),
        ];
        let static_facts = facts_with_classes(&["add-line-marks"]);
        // line-mark : détecté statiquement
        // "map" reste dynamique (absent de static_facts.classes).

        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        assert_eq!(emissions[0].bit, None); // line-mark
        assert_eq!(emissions[1].bit, Some(2)); // map

        let lowering = render_modules_as_rust(&emissions);
        // Une capacité statique garantit la présence du tag — pas
        // d'enveloppe if record.js_deps autour du <script> lui-même.
        assert_eq!(lowering.snippet.matches("<script").count(), 1);
        assert!(
            !lowering
                .snippet
                .trim_start()
                .starts_with("if record.js_deps")
        );
        // "map" reste individuellement conditionnel à l'intérieur du bloc.
        assert!(lowering.snippet.contains("if record.js_deps & 2 != 0 {"));
        assert!(lowering.snippet.contains("as _0")); // line-mark, position 0
        assert!(lowering.snippet.contains("as _1")); // map, position 1
    }

    // ── render_modules_as_static_html (STATIC_PAGES) ────────────────────────

    #[test]
    fn static_pages_zero_capability_emits_empty_string() {
        assert_eq!(render_modules_as_static_html(&[]), "");
    }

    #[test]
    fn static_pages_multiple_static_capabilities_single_script() {
        let capabilities = vec![
            cap(
                "line-mark",
                8,
                "boot",
                "/scripts/line-mark.js",
                &[".add-line-marks"],
            ),
            cap("map", 2, "initMapsSystem", "/scripts/map.js", &[".map"]),
        ];
        let static_facts = facts_with_classes(&["add-line-marks", "map"]);

        // has_record = false : comportement STATIC_PAGES réel.
        let emissions = lower_modules_for_template(&capabilities, &static_facts, false);
        assert_eq!(emissions.len(), 2);
        assert!(emissions.iter().all(|e| e.bit.is_none()));

        let html = render_modules_as_static_html(&emissions);
        assert_eq!(html.matches("<script").count(), 1);
        assert_eq!(html.matches("</script>").count(), 1);
        assert!(html.contains("as _0"));
        assert!(html.contains("as _1"));
        assert!(!html.contains("record.js_deps")); // jamais de test dynamique ici
    }

    #[test]
    #[should_panic(expected = "bit conditionnel rencontré")]
    fn static_html_panics_on_conditional_emission_bug() {
        // Défense en profondeur : un ModuleEmission bit: Some(_) ne devrait
        // structurellement jamais atteindre cette fonction (has_record
        // aurait dû valoir false dans lower_modules_for_template). Ce test
        // vérifie que le garde-fou est actif, pas un cas normal.
        let emissions = vec![ModuleEmission {
            activation: "boot".to_string(),
            url: "/scripts/x.js".to_string(),
            bit: Some(1),
            deps: Vec::new(),
        }];
        render_modules_as_static_html(&emissions);
    }

    // ── deps (SPEC `deps`) — agrégation, dédoublonnage, conditionnalité ────

    #[test]
    fn dep_classic_umd_emits_defer_script_before_module_tag() {
        let capabilities = vec![cap_with_deps(
            "map",
            2,
            "bootstrap",
            "/scripts/map.HASH.js",
            &[".map"],
            &[dep(
                "libraries/deckgl/deckgl.js",
                "/libraries/deckgl.HASH.js",
                false,
            )],
        )];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        // Ordre : dépendance AVANT le <script type="module"> de l'entry.
        let dep_pos = lowering
            .snippet
            .find("libraries/deckgl.HASH.js")
            .expect("la balise de dépendance doit être présente");
        let module_pos = lowering
            .snippet
            .find("<script type=\\\"module\\\">")
            .expect("le <script type=\"module\"> de l'entry doit être présent");
        assert!(
            dep_pos < module_pos,
            "la dépendance doit précéder le module dans le snippet généré"
        );

        assert!(
            lowering
                .snippet
                .contains("<script src=\\\"/libraries/deckgl.HASH.js\\\" defer></script>")
        );
        // Jamais un <script type=\"module\"> pour une dépendance classique.
        assert!(
            !lowering
                .snippet
                .contains("<script type=\\\"module\\\" src=\\\"/libraries/deckgl.HASH.js\\\">")
        );
        // Même bit que la capacité consommatrice — jamais inconditionnelle
        // ici, puisque `map` est dynamique.
        assert!(lowering.snippet.contains("if record.js_deps & 2 != 0"));
    }

    #[test]
    fn dep_esm_emits_module_script_tag() {
        let capabilities = vec![cap_with_deps(
            "foo",
            4,
            "boot",
            "/scripts/foo.HASH.js",
            &[".foo"],
            &[dep("libraries/bar/bar.js", "/libraries/bar.HASH.js", true)],
        )];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        assert!(
            lowering.snippet.contains(
                "<script type=\\\"module\\\" src=\\\"/libraries/bar.HASH.js\\\"></script>"
            )
        );

        // Ordre : dépendance AVANT le <script type="module"> de l'entry —
        // même exigence que pour une dépendance classique, jamais vérifiée
        // jusqu'ici pour le cas ESM spécifiquement.
        let dep_pos = lowering
            .snippet
            .find("libraries/bar.HASH.js")
            .expect("la balise de dépendance ESM doit être présente");
        let module_pos = lowering
            .snippet
            .find("<script type=\\\"module\\\">")
            .expect("le <script type=\"module\"> de l'entry doit être présent");
        assert!(
            dep_pos < module_pos,
            "la dépendance ESM doit précéder le module de l'entry dans le snippet généré"
        );
    }

    #[test]
    fn dep_shared_by_two_capabilities_emits_single_tag_with_or_of_bits() {
        let shared = dep(
            "libraries/deckgl/deckgl.js",
            "/libraries/deckgl.HASH.js",
            false,
        );
        let capabilities = vec![
            cap_with_deps(
                "map",
                2,
                "bootMap",
                "/scripts/map.HASH.js",
                &[".map"],
                &[shared.clone()],
            ),
            cap_with_deps(
                "terrain",
                4,
                "bootTerrain",
                "/scripts/terrain.HASH.js",
                &[".terrain"],
                &[shared],
            ),
        ];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        assert_eq!(emissions.len(), 2);

        let lowering = render_modules_as_rust(&emissions);

        // Une seule balise pour la dépendance partagée — jamais deux.
        assert_eq!(
            lowering.snippet.matches("libraries/deckgl.HASH.js").count(),
            1,
            "la dépendance partagée par deux capacités ne doit produire qu'une seule balise"
        );
        // OU binaire des deux bits (2 | 4 = 6).
        assert!(lowering.snippet.contains("if record.js_deps & 6 != 0"));
    }

    /// Scénario 5 du cahier de non-régression `deps` : plusieurs
    /// dépendances DISTINCTES (jamais partagées) sur une même capacité —
    /// chacune doit apparaître exactement une fois, dans l'ordre de
    /// déclaration (déterministe), jamais un ordre d'itération de
    /// `HashMap`.
    #[test]
    fn multiple_distinct_deps_each_appear_once_in_declaration_order() {
        let capabilities = vec![cap_with_deps(
            "map",
            2,
            "bootMap",
            "/scripts/map.HASH.js",
            &[".map"],
            &[
                dep("libraries/a/a.js", "/libraries/a.HASH.js", true),
                dep("libraries/b/b.js", "/libraries/b.HASH.js", false),
            ],
        )];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        assert_eq!(lowering.snippet.matches("libraries/a.HASH.js").count(), 1);
        assert_eq!(lowering.snippet.matches("libraries/b.HASH.js").count(), 1);

        let pos_a = lowering.snippet.find("libraries/a.HASH.js").unwrap();
        let pos_b = lowering.snippet.find("libraries/b.HASH.js").unwrap();
        assert!(
            pos_a < pos_b,
            "l'ordre de déclaration dans `deps` doit être préservé : a avant b"
        );
    }

    /// Scénario 6 du cahier de non-régression `deps`, variante non couverte
    /// par `dep_shared_by_two_capabilities_*` : une SEULE capacité qui
    /// déclare deux fois la MÊME dépendance dans son propre `deps`. Exerce
    /// le dédoublonnage INTRA-émission d'`aggregate_deps` (`out.iter_mut()
    /// .find(...)` sur les dépendances d'une seule et même émission),
    /// chemin distinct du dédoublonnage inter-capacités déjà testé.
    #[test]
    fn same_dep_declared_twice_on_one_capability_is_not_duplicated() {
        let same = dep(
            "libraries/deckgl/deckgl.js",
            "/libraries/deckgl.HASH.js",
            false,
        );
        let capabilities = vec![cap_with_deps(
            "map",
            2,
            "bootMap",
            "/scripts/map.HASH.js",
            &[".map"],
            &[same.clone(), same],
        )];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        assert_eq!(
            lowering.snippet.matches("libraries/deckgl.HASH.js").count(),
            1,
            "une dépendance déclarée deux fois par la même capacité ne doit produire qu'une balise"
        );
    }

    #[test]
    fn dep_shared_with_one_static_consumer_becomes_unconditional() {
        let shared = dep(
            "libraries/deckgl/deckgl.js",
            "/libraries/deckgl.HASH.js",
            false,
        );
        let capabilities = vec![
            cap_with_deps(
                "map",
                2,
                "bootMap",
                "/scripts/map.HASH.js",
                &[".map"],
                &[shared.clone()],
            ),
            cap_with_deps(
                "terrain-static",
                4,
                "bootTerrain",
                "/scripts/terrain.HASH.js",
                &[".terrain-static"],
                &[shared],
            ),
        ];
        let static_facts = facts_with_classes(&["terrain-static"]);
        // capacité statique

        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        // La dépendance partagée devient inconditionnelle — domination par
        // le consommateur statique (SPEC `deps` §3), même si `map` reste
        // dynamique par ailleurs.
        let dep_line = lowering
            .snippet
            .lines()
            .find(|l| l.contains("libraries/deckgl.HASH.js"))
            .expect("la balise de dépendance doit être présente");
        assert!(
            !dep_line.contains("if record.js_deps"),
            "attendu inconditionnel, trouvé : {dep_line}"
        );
    }

    #[test]
    fn no_deps_leaves_snippet_identical_to_before() {
        // Non-régression explicite : une capacité sans `deps` ne doit
        // produire aucune balise supplémentaire ni aucun changement de
        // structure par rapport au comportement d'avant `deps`.
        let capabilities = vec![cap(
            "map",
            2,
            "initMapsSystem",
            "/scripts/map.HASH.js",
            &[".map"],
        )];
        let static_facts = empty_facts();
        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);
        let lowering = render_modules_as_rust(&emissions);

        assert_eq!(lowering.snippet.matches("<script").count(), 1);
        assert!(!lowering.snippet.contains("defer"));
    }

    #[test]
    fn static_page_dep_is_unconditional_and_dedup_still_applies() {
        let shared = dep(
            "libraries/deckgl/deckgl.js",
            "/libraries/deckgl.HASH.js",
            false,
        );
        let capabilities = vec![
            cap_with_deps(
                "map",
                2,
                "bootMap",
                "/scripts/map.HASH.js",
                &[".map"],
                &[shared.clone()],
            ),
            cap_with_deps(
                "terrain",
                4,
                "bootTerrain",
                "/scripts/terrain.HASH.js",
                &[".terrain"],
                &[shared],
            ),
        ];
        let static_facts = facts_with_classes(&["map", "terrain"]);

        // STATIC_PAGES : has_record = false, comme les autres tests de ce
        // fichier pour ce pipeline.
        let emissions = lower_modules_for_template(&capabilities, &static_facts, false);
        let html = render_modules_as_static_html(&emissions);

        assert_eq!(
            html.matches("libraries/deckgl.HASH.js").count(),
            1,
            "dédoublonnage attendu même sur STATIC_PAGES"
        );
        assert!(!html.contains("record.js_deps"));
        // Ordre : dépendance avant le <script type="module">.
        assert!(
            html.find("libraries/deckgl.HASH.js").unwrap()
                < html.find("<script type=\"module\">").unwrap()
        );
    }

    /// Cas fondateur de `deps` (SPEC/session originelle) : `map.js`
    /// consommant Deck.gl, UMD/classique, via `deps`. Contrairement aux
    /// autres tests de ce module (qui inspectent un *snippet de codegen
    /// Rust*, un niveau d'indirection avant le HTML réellement servi),
    /// celui-ci vérifie le HTML FINAL, littéral, produit par
    /// `render_modules_as_static_html` — aucune indirection
    /// supplémentaire : c'est le texte exact qui atteindrait le
    /// navigateur pour ce template. Assertion sur la chaîne exacte, pas
    /// seulement des sous-chaînes, pour verrouiller l'ordre ET l'absence
    /// de tout caractère parasite entre les deux balises.
    #[test]
    fn founding_case_deckgl_umd_dep_produces_exact_final_html() {
        let capabilities = vec![cap_with_deps(
            "map",
            2,
            "bootstrap",
            "/scripts/map.HASH.js",
            &[".map"],
            &[dep(
                "libraries/deckgl/deckgl.js",
                "/libraries/deckgl.HASH.js",
                false, // UMD/classique — [libraries.deckgl].module = false
            )],
        )];
        let static_facts = facts_with_classes(&["map"]);

        let emissions = lower_modules_for_template(&capabilities, &static_facts, false);
        let html = render_modules_as_static_html(&emissions);

        assert_eq!(
            html,
            "<script src=\"/libraries/deckgl.HASH.js\" defer></script>\
             <script type=\"module\">import{bootstrap as _0}from\"/scripts/map.HASH.js\";_0();</script>"
        );
    }

    /// `content_driven = false` (`cap.bit == None`) : aucune émission ne
    /// doit jamais apparaître dans la branche dynamique, MÊME si
    /// `has_record = true` — le comportement introduit par le découplage
    /// `content_driven`/`scripts_registry.lock`. Avant ce découplage,
    /// cette capacité aurait été émise avec `bit: Some(cap.bit)`.
    #[test]
    fn content_driven_false_never_emits_dynamically_even_with_record() {
        let capabilities = vec![cap_static_only(
            "navigation",
            "initNavigation",
            "/scripts/navigation.HASH.js",
            &[".cmd-nav"],
        )];
        // Marqueur absent statiquement de CE template précis.
        let static_facts = empty_facts();

        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);

        assert!(
            emissions.is_empty(),
            "une capacité content_driven = false ne doit jamais dépendre de record.js_deps"
        );
    }

    /// Même capacité `content_driven = false`, mais son marqueur EST
    /// présent statiquement dans ce template — l'émission inconditionnelle
    /// (`bit: None`) reste intacte, `cap.bit == None` n'a jamais empêché
    /// la branche statique, qui ne le consulte jamais de toute façon.
    #[test]
    fn content_driven_false_still_emits_unconditionally_on_static_hit() {
        let capabilities = vec![cap_static_only(
            "navigation",
            "initNavigation",
            "/scripts/navigation.HASH.js",
            &[".cmd-nav"],
        )];
        let static_facts = facts_with_classes(&["cmd-nav"]);

        let emissions = lower_modules_for_template(&capabilities, &static_facts, true);

        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].bit, None);
    }

    /// Symétrique de `content_driven_false_never_emits_dynamically_even_with_record` :
    /// sur `STATIC_PAGES` (`has_record = false`), une capacité
    /// `content_driven = false` sans hit statique n'émet toujours rien —
    /// comportement inchangé par rapport à avant l'introduction de
    /// `content_driven` (déjà vrai pour toute capacité dans ce cas).
    #[test]
    fn content_driven_false_emits_nothing_on_static_pages_without_hit() {
        let capabilities = vec![cap_static_only(
            "navigation",
            "initNavigation",
            "/scripts/navigation.HASH.js",
            &[".cmd-nav"],
        )];
        let static_facts = empty_facts();

        let emissions = lower_modules_for_template(&capabilities, &static_facts, false);

        assert!(emissions.is_empty());
    }
}
