// crates/core/schema/build/publication.rs

//! Manifeste AOT de publication/exposition (`publication.toml`) : lecture,
//! validation croisée avec le registre de composants, génération du code
//! consommé par les autres crates.
//!
//! ## Rôle
//!
//! `publication.toml` est la SEULE définition de deux relations distinctes
//! (voir l'en-tête de ce fichier et `marius_projection::publication`) :
//!
//! ```text
//! [[artifact]]  artefact → composant producteur     (publication)
//! [[route]]     route    → artefact + sélection     (exposition)
//! ```
//!
//! Ce module produit, dans `generated_schema.rs` :
//!
//! - `ARTIFACTS`, `<KEY>_ARTIFACT`, `<KEY>_SOURCE_KEY` — catalogue d'artefacts
//!   et handles `SourceKey` (position dans le catalogue, non persistante) ;
//! - `<NAME>_ROUTE`, `ROUTES` — les `RouteSpec` neutres, avec la colonne PK
//!   résolue par la Forge ;
//! - `ROUTE_DESCRIPTORS` — un `RouteDescriptor` T2A (K=1) par route, aligné
//!   index à index sur `ROUTES`.
//!
//! Les représentations serveur (`RouteEntry`) se dérivent en aval
//! (`marius-render`) : ce module ne nomme aucun type de render/server.
//!
//! ## Pureté
//!
//! Tout est fonction pure (texte → structures → texte) sauf `load_publication`,
//! seule à toucher le disque. `parse_publication`,
//! `validate_against_components` et `generate_publication_code` sont donc
//! testables sans PostgreSQL ni Cargo.
//!
//! Le parsing passe par `toml::Value` (aucune dérivation serde) : rejet
//! explicite de toute clé inconnue, messages d'erreur nominatifs.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

/// Nom du manifeste, relatif à `CARGO_MANIFEST_DIR` de `marius-schema`.
pub(crate) const MANIFEST_FILE: &str = "publication.toml";

/// Nombre maximal d'artefacts : un `SourceKey` est un `u16`.
const MAX_ARTIFACTS: usize = u16::MAX as usize + 1;

// ─── Structures déclaratives ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactDecl {
    pub key: String,
    pub component: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteDecl {
    pub name: String,
    pub pattern: String,
    pub artifact: String,
    pub parameter: String,
    pub selection: String,
}

/// `[[volatile_region]]` (V2c) — jonction entre un `component` et la
/// partition de son template en (head, volatile, tail). Distincte de
/// `[[artifact]]` (identité) : cette déclaration ne PORTE aucune identité
/// d'artefact elle-même, elle en RÉFÉRENCE deux déjà déclarées.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolatileRegionDecl {
    pub component: String,
    pub marker: String,
    pub head_artifact: String,
    pub tail_artifact: String,
    pub capacity: u32,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PublicationManifest {
    pub artifacts: Vec<ArtifactDecl>,
    pub routes: Vec<RouteDecl>,
    pub volatile_regions: Vec<VolatileRegionDecl>,
}

/// Faits sur un composant, collectés par la boucle Forge de `main()`.
///
/// `pk_column` vaut `None` pour une PK composite : la sélection
/// `primary_key` n'a alors pas de colonne unique à laquelle se rattacher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComponentFacts {
    /// `"schema.table"`.
    pub component_id: String,
    pub pk_column: Option<String>,
}

// ─── Utilitaires de syntaxe ─────────────────────────────────────────────

/// `[a-z][a-z0-9_]*` — utilisable tel quel comme fragment d'identifiant Rust
/// (une fois mis en majuscules) et comme nom de fichier d'artefact.
fn is_snake_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Paramètres `{nom}` d'un motif d'URL.
///
/// Restrictions volontaires (déterministes, vérifiables au build) : le motif
/// commence par `/`, n'emploie que `[A-Za-z0-9/_.-{}]`, et chaque paramètre
/// occupe un segment entier (`/content/{id}`, jamais `/content-{id}`).
fn pattern_parameters(pattern: &str) -> Result<Vec<String>, String> {
    if !pattern.starts_with('/') {
        return Err(format!("motif «{pattern}» : doit commencer par «/»"));
    }
    if let Some(bad) = pattern
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-' | '{' | '}')))
    {
        return Err(format!(
            "motif «{pattern}» : caractère «{bad}» non admis (attendu [A-Za-z0-9/_.-{{}}])"
        ));
    }

    let mut params = Vec::new();
    for segment in pattern.split('/') {
        let opens = segment.matches('{').count();
        let closes = segment.matches('}').count();
        if opens == 0 && closes == 0 {
            continue;
        }
        let whole_param = segment.len() > 2
            && segment.starts_with('{')
            && segment.ends_with('}')
            && opens == 1
            && closes == 1;
        if !whole_param {
            return Err(format!(
                "motif «{pattern}» : segment «{segment}» invalide — un paramètre \
                 doit occuper un segment entier, sous la forme «{{nom}}»"
            ));
        }
        let name = &segment[1..segment.len() - 1];
        if !is_snake_ident(name) {
            return Err(format!(
                "motif «{pattern}» : nom de paramètre «{name}» invalide (attendu [a-z][a-z0-9_]*)"
            ));
        }
        params.push(name.to_string());
    }
    Ok(params)
}

// ─── Lecture des tables TOML ────────────────────────────────────────────

fn check_known_keys(table: &toml::Table, allowed: &[&str], ctx: &str, errors: &mut Vec<String>) {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(format!(
                "{ctx} : clé inconnue «{key}» (admises : {})",
                allowed.join(", ")
            ));
        }
    }
}

fn read_string(
    table: &toml::Table,
    key: &str,
    ctx: &str,
    required: bool,
    errors: &mut Vec<String>,
) -> Option<String> {
    match table.get(key) {
        None => {
            if required {
                errors.push(format!("{ctx} : clé obligatoire «{key}» absente"));
            }
            None
        }
        Some(value) => match value.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                errors.push(format!("{ctx} : «{key}» doit être une chaîne"));
                None
            }
        },
    }
}

/// Pendant de [`read_string`] pour un entier TOML — seul champ numérique du
/// manifeste à ce jour (`[[volatile_region]].capacity`, V2c). Même
/// discipline de messages nominatifs, même signature à `required`.
fn read_u32(
    table: &toml::Table,
    key: &str,
    ctx: &str,
    required: bool,
    errors: &mut Vec<String>,
) -> Option<u32> {
    match table.get(key) {
        None => {
            if required {
                errors.push(format!("{ctx} : clé obligatoire «{key}» absente"));
            }
            None
        }
        Some(value) => match value.as_integer() {
            Some(n) => match u32::try_from(n) {
                Ok(v) => Some(v),
                Err(_) => {
                    errors.push(format!(
                        "{ctx} : «{key}» doit tenir dans un u32 (valeur lue : {n})"
                    ));
                    None
                }
            },
            None => {
                errors.push(format!("{ctx} : «{key}» doit être un entier"));
                None
            }
        },
    }
}

fn read_array_of_tables<'a>(
    root: &'a toml::Table,
    key: &str,
    errors: &mut Vec<String>,
) -> Vec<&'a toml::Table> {
    let Some(value) = root.get(key) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        errors.push(format!(
            "«{key}» doit être un tableau de tables ([[{key}]])"
        ));
        return Vec::new();
    };
    let mut tables = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match item.as_table() {
            Some(t) => tables.push(t),
            None => errors.push(format!("{key}[{i}] : doit être une table")),
        }
    }
    tables
}

// ─── Parsing + validation structurelle ──────────────────────────────────

/// Parse le manifeste et vérifie tout ce qui ne dépend pas du registre de
/// composants : syntaxe, unicité, références internes, cohérence du motif
/// d'URL avec `parameter`, mode de sélection connu.
///
/// Accumule toutes les erreurs (fail-slow), comme les passes Fragment-Forge.
pub(crate) fn parse_publication(src: &str) -> Result<PublicationManifest, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    // `toml::Table` (et non `toml::Value`) : `Value::from_str` ne parse, selon
    // la version de la crate, qu'une VALEUR TOML — jamais un document complet.
    let root: toml::Table = match src.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => return Err(vec![format!("TOML invalide : {e}")]),
    };
    let root = &root;
    check_known_keys(root, &["artifact", "route", "volatile_region"], "manifeste", &mut errors);

    let mut manifest = PublicationManifest::default();

    for (i, table) in read_array_of_tables(root, "artifact", &mut errors)
        .into_iter()
        .enumerate()
    {
        let ctx = format!("artifact[{i}]");
        check_known_keys(table, &["key", "component"], &ctx, &mut errors);
        let key = read_string(table, "key", &ctx, true, &mut errors);
        let component = read_string(table, "component", &ctx, false, &mut errors);
        if let Some(key) = key {
            manifest.artifacts.push(ArtifactDecl { key, component });
        }
    }

    for (i, table) in read_array_of_tables(root, "route", &mut errors)
        .into_iter()
        .enumerate()
    {
        let ctx = format!("route[{i}]");
        check_known_keys(
            table,
            &["name", "pattern", "artifact", "parameter", "selection"],
            &ctx,
            &mut errors,
        );
        let name = read_string(table, "name", &ctx, true, &mut errors);
        let pattern = read_string(table, "pattern", &ctx, true, &mut errors);
        let artifact = read_string(table, "artifact", &ctx, true, &mut errors);
        let parameter = read_string(table, "parameter", &ctx, true, &mut errors);
        let selection = read_string(table, "selection", &ctx, true, &mut errors);
        if let (Some(name), Some(pattern), Some(artifact), Some(parameter), Some(selection)) =
            (name, pattern, artifact, parameter, selection)
        {
            manifest.routes.push(RouteDecl {
                name,
                pattern,
                artifact,
                parameter,
                selection,
            });
        }
    }

    for (i, table) in read_array_of_tables(root, "volatile_region", &mut errors)
        .into_iter()
        .enumerate()
    {
        let ctx = format!("volatile_region[{i}]");
        check_known_keys(
            table,
            &["component", "marker", "head_artifact", "tail_artifact", "capacity"],
            &ctx,
            &mut errors,
        );
        let component = read_string(table, "component", &ctx, true, &mut errors);
        let marker = read_string(table, "marker", &ctx, true, &mut errors);
        let head_artifact = read_string(table, "head_artifact", &ctx, true, &mut errors);
        let tail_artifact = read_string(table, "tail_artifact", &ctx, true, &mut errors);
        let capacity = read_u32(table, "capacity", &ctx, true, &mut errors);
        if let (Some(component), Some(marker), Some(head_artifact), Some(tail_artifact), Some(capacity)) =
            (component, marker, head_artifact, tail_artifact, capacity)
        {
            manifest.volatile_regions.push(VolatileRegionDecl {
                component,
                marker,
                head_artifact,
                tail_artifact,
                capacity,
            });
        }
    }

    // ── Artefacts ───────────────────────────────────────────────────────
    if manifest.artifacts.len() > MAX_ARTIFACTS {
        errors.push(format!(
            "{} artefacts déclarés : SourceKey est un u16 (maximum {MAX_ARTIFACTS})",
            manifest.artifacts.len()
        ));
    }
    let mut seen_keys: HashSet<&str> = HashSet::new();
    for artifact in &manifest.artifacts {
        if !is_snake_ident(&artifact.key) {
            errors.push(format!(
                "artefact «{}» : clé invalide (attendu [a-z][a-z0-9_]*)",
                artifact.key
            ));
        }
        if !seen_keys.insert(artifact.key.as_str()) {
            errors.push(format!(
                "artefact «{}» : clé déclarée deux fois",
                artifact.key
            ));
        }
    }

    // ── Routes ──────────────────────────────────────────────────────────
    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut seen_patterns: HashSet<&str> = HashSet::new();
    for route in &manifest.routes {
        let ctx = format!("route «{}»", route.name);
        if !is_snake_ident(&route.name) {
            errors.push(format!("{ctx} : nom invalide (attendu [a-z][a-z0-9_]*)"));
        }
        if !seen_names.insert(route.name.as_str()) {
            errors.push(format!("{ctx} : nom déclaré deux fois"));
        }
        if !seen_patterns.insert(route.pattern.as_str()) {
            errors.push(format!(
                "{ctx} : motif «{}» déclaré deux fois",
                route.pattern
            ));
        }
        if !seen_keys.contains(route.artifact.as_str()) {
            errors.push(format!(
                "{ctx} : artefact «{}» non déclaré ([[artifact]] absent)",
                route.artifact
            ));
        }
        match pattern_parameters(&route.pattern) {
            Err(e) => errors.push(format!("{ctx} : {e}")),
            Ok(params) => {
                if params.len() != 1 || params[0] != route.parameter {
                    errors.push(format!(
                        "{ctx} : `parameter = \"{}\"` doit être l'unique paramètre du \
                         motif «{}» (paramètres trouvés : {params:?})",
                        route.parameter, route.pattern
                    ));
                }
            }
        }
        if route.selection != "primary_key" {
            errors.push(format!(
                "{ctx} : selection «{}» inconnue (admise : \"primary_key\")",
                route.selection
            ));
        }
    }

    // ── Régions volatiles (V2c) ────────────────────────────────────────
    let mut seen_region_components: HashSet<&str> = HashSet::new();
    for region in &manifest.volatile_regions {
        let ctx = format!("région volatile «{}»", region.marker);
        if !seen_keys.contains(region.head_artifact.as_str()) {
            errors.push(format!(
                "{ctx} : head_artifact «{}» non déclaré ([[artifact]] absent)",
                region.head_artifact
            ));
        }
        if !seen_keys.contains(region.tail_artifact.as_str()) {
            errors.push(format!(
                "{ctx} : tail_artifact «{}» non déclaré ([[artifact]] absent)",
                region.tail_artifact
            ));
        }
        if region.head_artifact == region.tail_artifact {
            errors.push(format!(
                "{ctx} : head_artifact et tail_artifact doivent être distincts \
                 (les deux valent «{}»)",
                region.head_artifact
            ));
        }
        if !seen_region_components.insert(region.component.as_str()) {
            errors.push(format!(
                "{ctx} : le composant «{}» est déjà couvert par une autre \
                 [[volatile_region]] — une seule région par composant, \
                 jamais d'ambiguïté sur la région applicable",
                region.component
            ));
        }
    }

    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}

// ─── Validation croisée avec le registre de composants ──────────────────

/// Vérifie ce qui dépend du registre : existence des composants référencés,
/// et compatibilité de la sélection `primary_key` avec le composant de
/// l'artefact ciblé (composant présent, PK simple).
pub(crate) fn validate_against_components(
    manifest: &PublicationManifest,
    components: &[ComponentFacts],
) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    for artifact in &manifest.artifacts {
        if let Some(component) = &artifact.component
            && !components.iter().any(|c| &c.component_id == component)
        {
            errors.push(format!(
                "artefact «{}» : composant «{component}» absent de meta.containment_intent",
                artifact.key
            ));
        }
    }

    for route in &manifest.routes {
        if route.selection != "primary_key" {
            continue; // déjà signalé par parse_publication
        }
        let Some(artifact) = manifest.artifacts.iter().find(|a| a.key == route.artifact) else {
            continue; // déjà signalé par parse_publication
        };
        let Some(component) = &artifact.component else {
            errors.push(format!(
                "route «{}» : selection = \"primary_key\" exige un artefact avec composant \
                 (artefact «{}» sans composant)",
                route.name, artifact.key
            ));
            continue;
        };
        let Some(facts) = components.iter().find(|c| &c.component_id == component) else {
            continue; // composant absent : déjà signalé ci-dessus
        };
        if facts.pk_column.is_none() {
            errors.push(format!(
                "route «{}» : selection = \"primary_key\" exige une PK simple, or le composant \
                 «{component}» a une PK composite",
                route.name
            ));
        }
    }

    for region in &manifest.volatile_regions {
        let ctx = format!("région volatile «{}»", region.marker);
        if !components.iter().any(|c| c.component_id == region.component) {
            errors.push(format!(
                "{ctx} : composant «{}» absent de meta.containment_intent",
                region.component
            ));
        }
        for (role, key) in [
            ("head_artifact", &region.head_artifact),
            ("tail_artifact", &region.tail_artifact),
        ] {
            let Some(artifact) = manifest.artifacts.iter().find(|a| &a.key == key) else {
                continue; // absence déjà signalée par parse_publication
            };
            match &artifact.component {
                Some(c) if c == &region.component => {}
                Some(c) => errors.push(format!(
                    "{ctx} : {role} «{key}» porte le composant «{c}», \
                     attendu «{}» (celui de la région)",
                    region.component
                )),
                None => errors.push(format!(
                    "{ctx} : {role} «{key}» n'a aucun composant, \
                     attendu «{}» (celui de la région)",
                    region.component
                )),
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ─── Génération ─────────────────────────────────────────────────────────

/// Code Rust ajouté à `generated_schema.rs`. Valide d'abord contre les
/// composants — jamais de code généré à partir d'un manifeste incohérent.
pub(crate) fn generate_publication_code(
    manifest: &PublicationManifest,
    components: &[ComponentFacts],
) -> Result<String, Vec<String>> {
    validate_against_components(manifest, components)?;

    let mut out = String::new();
    let p = "::marius_projection";

    writeln!(out).unwrap();
    writeln!(
        out,
        "// ── Publication AOT — {MANIFEST_FILE} (généré, ne pas éditer) ─────────────────"
    )
    .unwrap();
    writeln!(out, "//").unwrap();
    writeln!(
        out,
        "// SourceKey(n) = position n dans ARTIFACTS. Numéro propre à CE build :"
    )
    .unwrap();
    writeln!(
        out,
        "// non persistant, jamais à stocker ni à comparer entre builds."
    )
    .unwrap();
    writeln!(out).unwrap();

    // Catalogue.
    writeln!(
        out,
        "/// Catalogue des artefacts publiables (`SourceKey(n)` = entrée `n`)."
    )
    .unwrap();
    writeln!(out, "pub const ARTIFACTS: &[{p}::ArtifactSpec] = &[").unwrap();
    for artifact in &manifest.artifacts {
        let component = match &artifact.component {
            Some(c) => format!("::core::option::Option::Some({c:?})"),
            None => "::core::option::Option::None".to_string(),
        };
        writeln!(
            out,
            "    {p}::ArtifactSpec {{ key: {p}::ArtifactKey::new({:?}), component: {component} }},",
            artifact.key
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Constantes par artefact.
    for (index, artifact) in manifest.artifacts.iter().enumerate() {
        let upper = artifact.key.to_ascii_uppercase();
        writeln!(
            out,
            "/// Clé de l'artefact `{}`.\npub const {upper}_ARTIFACT: {p}::ArtifactKey = {p}::ArtifactKey::new({:?});",
            artifact.key, artifact.key
        )
        .unwrap();
        writeln!(
            out,
            "/// `SourceKey` de l'artefact `{}` dans ce build.\npub const {upper}_SOURCE_KEY: {p}::SourceKey = {p}::SourceKey({index});",
            artifact.key
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    // RouteSpec neutres.
    for route in &manifest.routes {
        let upper = route.name.to_ascii_uppercase();
        let component_id = manifest
            .artifacts
            .iter()
            .find(|a| a.key == route.artifact)
            .and_then(|a| a.component.as_ref())
            .expect("route primary_key validée : artefact avec composant");
        let pk_column = components
            .iter()
            .find(|c| &c.component_id == component_id)
            .and_then(|c| c.pk_column.as_ref())
            .expect("route primary_key validée : PK simple");
        writeln!(
            out,
            "/// Route `{}` — `{}` → artefact `{}` (PK `{pk_column}` du composant `{component_id}`).",
            route.name, route.pattern, route.artifact
        )
        .unwrap();
        writeln!(
            out,
            "pub const {upper}_ROUTE: {p}::RouteSpec = {p}::RouteSpec {{"
        )
        .unwrap();
        writeln!(out, "    name: {:?},", route.name).unwrap();
        writeln!(out, "    pattern: {:?},", route.pattern).unwrap();
        writeln!(
            out,
            "    artifact: {p}::ArtifactKey::new({:?}),",
            route.artifact
        )
        .unwrap();
        writeln!(out, "    parameter: {:?},", route.parameter).unwrap();
        writeln!(
            out,
            "    selection: {p}::RouteSelection::PrimaryKey {{ column: {pk_column:?} }},"
        )
        .unwrap();
        writeln!(out, "}};").unwrap();
    }
    writeln!(out).unwrap();

    writeln!(
        out,
        "/// Toutes les routes déclarées, dans l'ordre du manifeste."
    )
    .unwrap();
    writeln!(out, "pub const ROUTES: &[{p}::RouteSpec] = &[").unwrap();
    for route in &manifest.routes {
        writeln!(out, "    {}_ROUTE,", route.name.to_ascii_uppercase()).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // RouteDescriptor T2A — K=1 par défaut, K=3 pour la route dont
    // l'artefact appartient à un `component` couvert par [[volatile_region]]
    // (V2c). Aligné index à index sur ROUTES dans les deux cas.
    writeln!(
        out,
        "/// `RouteDescriptor` T2A de chaque route de `ROUTES` (même index).\n\
         ///\n\
         /// K=1 (par défaut) : un segment, une source, sélection `RequestSlot(0)` —\n\
         /// le slot 0 est rempli, côté serveur, par l'unique paramètre HTTP de la\n\
         /// route. K=3 (route dont l'artefact appartient à un `component` couvert\n\
         /// par [[volatile_region]]) : StaticArtifact(head) → VolatileSlot →\n\
         /// StaticArtifact(tail), head et tail partageant le même `RequestSlot(0)`\n\
         /// (même sélection, deux artefacts distincts — contrat Volatile P7).\n\
         /// `backend_kind` : champ hérité d'un modèle antérieur, non consommé par\n\
         /// T2A — valeur neutre, sans signification architecturale."
    )
    .unwrap();
    writeln!(
        out,
        "pub static ROUTE_DESCRIPTORS: &[{p}::RouteDescriptor] = &["
    )
    .unwrap();
    for route in &manifest.routes {
        let route_artifact = manifest
            .artifacts
            .iter()
            .find(|a| a.key == route.artifact)
            .expect("route validée : artefact déclaré");
        let source_index = manifest
            .artifacts
            .iter()
            .position(|a| a.key == route.artifact)
            .expect("route validée : artefact déclaré");

        // Une région volatile s'applique à cette route si le `component` de
        // SON artefact (pas l'artefact lui-même) est couvert — jamais
        // l'inverse : `route.artifact` reste "content_core" (monolithique,
        // RouteSpec/RouteEntry inchangés), seule cette représentation T2A
        // bascule.
        let volatile_region = route_artifact.component.as_ref().and_then(|component| {
            manifest
                .volatile_regions
                .iter()
                .find(|r| &r.component == component)
        });

        writeln!(out, "    {p}::RouteDescriptor {{").unwrap();
        match volatile_region {
            None => {
                // K=1 — texte identique à avant cet incrément, pour toute
                // route non couverte par [[volatile_region]].
                writeln!(out, "        segments: &[{p}::SegmentDescriptor {{").unwrap();
                writeln!(out, "            source: {p}::SourceId(0),").unwrap();
                writeln!(
                    out,
                    "            selection: {p}::SegmentSelection::RequestSlot({p}::RequestValueId(0)),"
                )
                .unwrap();
                writeln!(out, "            flags: {p}::SegmentFlags::NONE,").unwrap();
                writeln!(out, "        }}],").unwrap();
                writeln!(
                    out,
                    "        sources: &[{p}::SourceSpec::StaticArtifact {{ key: {p}::SourceKey({source_index}) }}],"
                )
                .unwrap();
                writeln!(
                    out,
                    "        backend_kind: {p}::EmissionBackendKind::Scatter,"
                )
                .unwrap();
                writeln!(out, "        volatile_capacity: 0,").unwrap();
            }
            Some(region) => {
                // K=3 — vertical slice Volatile (V2c). `head`/`tail` déjà
                // validés (existence, même `component`, distincts) par
                // `validate_against_components`/`parse_publication` : les
                // deux `.position()` ci-dessous ne peuvent pas échouer ici.
                let head_index = manifest
                    .artifacts
                    .iter()
                    .position(|a| a.key == region.head_artifact)
                    .expect("région volatile validée : head_artifact déclaré");
                let tail_index = manifest
                    .artifacts
                    .iter()
                    .position(|a| a.key == region.tail_artifact)
                    .expect("région volatile validée : tail_artifact déclaré");
                writeln!(
                    out,
                    "        // Région volatile «{}» — [[volatile_region]], V2c.",
                    region.marker
                )
                .unwrap();
                writeln!(out, "        segments: &[").unwrap();
                writeln!(out, "            {p}::SegmentDescriptor {{").unwrap();
                writeln!(out, "                source: {p}::SourceId(0),").unwrap();
                writeln!(
                    out,
                    "                selection: {p}::SegmentSelection::RequestSlot({p}::RequestValueId(0)),"
                )
                .unwrap();
                writeln!(out, "                flags: {p}::SegmentFlags::NONE,").unwrap();
                writeln!(out, "            }},").unwrap();
                writeln!(out, "            {p}::SegmentDescriptor {{").unwrap();
                writeln!(out, "                source: {p}::SourceId(1),").unwrap();
                writeln!(
                    out,
                    "                selection: {p}::SegmentSelection::NotApplicable,"
                )
                .unwrap();
                writeln!(out, "                flags: {p}::SegmentFlags::VOLATILE,").unwrap();
                writeln!(out, "            }},").unwrap();
                writeln!(out, "            {p}::SegmentDescriptor {{").unwrap();
                writeln!(out, "                source: {p}::SourceId(2),").unwrap();
                writeln!(
                    out,
                    "                selection: {p}::SegmentSelection::RequestSlot({p}::RequestValueId(0)),"
                )
                .unwrap();
                writeln!(out, "                flags: {p}::SegmentFlags::NONE,").unwrap();
                writeln!(out, "            }},").unwrap();
                writeln!(out, "        ],").unwrap();
                writeln!(out, "        sources: &[").unwrap();
                writeln!(
                    out,
                    "            {p}::SourceSpec::StaticArtifact {{ key: {p}::SourceKey({head_index}) }},"
                )
                .unwrap();
                writeln!(
                    out,
                    "            {p}::SourceSpec::VolatileSlot {{ capacity: {}, producer: {p}::ProducerKey(0) }},",
                    region.capacity
                )
                .unwrap();
                writeln!(
                    out,
                    "            {p}::SourceSpec::StaticArtifact {{ key: {p}::SourceKey({tail_index}) }},"
                )
                .unwrap();
                writeln!(out, "        ],").unwrap();
                writeln!(
                    out,
                    "        backend_kind: {p}::EmissionBackendKind::Scatter,"
                )
                .unwrap();
                writeln!(out, "        volatile_capacity: {},", region.capacity).unwrap();
            }
        }
        writeln!(out, "    }},").unwrap();
    }
    writeln!(out, "];").unwrap();

    Ok(out)
}

// ─── I/O ────────────────────────────────────────────────────────────────

/// Lit et parse `publication.toml`. Seule fonction de ce module à toucher le
/// disque ni à écrire sur stdout (`cargo:` directives).
///
/// `rerun-if-changed` est émis inconditionnellement, avant la lecture : même
/// discipline d'incrémentalité que `resolve_template` (un fichier créé ou
/// modifié après un build en échec doit relancer le script).
pub(crate) fn load_publication(manifest_dir: &str) -> Result<PublicationManifest, ()> {
    let path = Path::new(manifest_dir).join(MANIFEST_FILE);
    println!("cargo:rerun-if-changed={}", path.display());

    let src = std::fs::read_to_string(&path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge [publication] : lecture de {} échouée : {e}",
            path.display()
        );
    })?;

    parse_publication(&src).map_err(|errors| {
        for e in errors {
            println!("cargo:error=DB-Forge [publication] : {e}");
        }
    })
}

// =============================================================================
// Tests
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    const REAL_MANIFEST: &str = include_str!("../publication.toml");

    fn content_core_facts() -> Vec<ComponentFacts> {
        vec![
            ComponentFacts {
                component_id: "commerce.product_core".into(),
                pk_column: Some("product_id".into()),
            },
            ComponentFacts {
                component_id: "content.core".into(),
                pk_column: Some("document_id".into()),
            },
        ]
    }

    fn assert_err_contains(result: Result<PublicationManifest, Vec<String>>, needle: &str) {
        let errors = result.expect_err("une erreur était attendue");
        assert!(
            errors.iter().any(|e| e.contains(needle)),
            "aucune erreur ne contient «{needle}» : {errors:?}"
        );
    }

    // ── Manifeste réel ──────────────────────────────────────────────────

    #[test]
    fn real_manifest_declares_content_route_over_content_core() {
        let m = parse_publication(REAL_MANIFEST).expect("le manifeste réel doit être valide");
        assert_eq!(
            m.artifacts,
            vec![
                ArtifactDecl {
                    key: "content_core".into(),
                    component: Some("content.core".into()),
                },
                ArtifactDecl {
                    key: "content_core_head".into(),
                    component: Some("content.core".into()),
                },
                ArtifactDecl {
                    key: "content_core_tail".into(),
                    component: Some("content.core".into()),
                },
            ]
        );
        assert_eq!(
            m.routes,
            vec![RouteDecl {
                name: "content_document".into(),
                pattern: "/content/{id}".into(),
                artifact: "content_core".into(),
                parameter: "id".into(),
                selection: "primary_key".into(),
            }]
        );
        assert_eq!(
            m.volatile_regions,
            vec![VolatileRegionDecl {
                component: "content.core".into(),
                marker: "nav_profile".into(),
                head_artifact: "content_core_head".into(),
                tail_artifact: "content_core_tail".into(),
                capacity: 512,
            }]
        );
    }

    #[test]
    fn real_manifest_validates_against_content_core_and_resolves_document_id() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        validate_against_components(&m, &content_core_facts())
            .expect("content.core existe et a une PK simple");

        let code = generate_publication_code(&m, &content_core_facts()).unwrap();
        // Le paramètre HTTP est `id`, la colonne SQL `document_id` : deux
        // identités distinctes, jamais renommées pour coïncider.
        assert!(code.contains("parameter: \"id\","), "{code}");
        assert!(
            code.contains("RouteSelection::PrimaryKey { column: \"document_id\" }"),
            "{code}"
        );
        assert!(code.contains("pub const CONTENT_CORE_SOURCE_KEY"), "{code}");
        assert!(code.contains("::marius_projection::SourceKey(0)"), "{code}");
        assert!(code.contains("pub const CONTENT_DOCUMENT_ROUTE"), "{code}");
        assert!(code.contains("pub static ROUTE_DESCRIPTORS"), "{code}");
    }

    // ── Région volatile (V2c) — câblage réel, pas théorique ─────────────

    #[test]
    fn real_manifest_generates_a_k3_route_descriptor_for_content_document() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        let code = generate_publication_code(&m, &content_core_facts()).unwrap();
        // head = SourceKey(1), tail = SourceKey(2) — ordre du manifeste :
        // content_core(0), content_core_head(1), content_core_tail(2).
        assert!(
            code.contains(
                "SourceSpec::StaticArtifact { key: ::marius_projection::SourceKey(1) }"
            ),
            "{code}"
        );
        assert!(
            code.contains(
                "SourceSpec::VolatileSlot { capacity: 512, producer: ::marius_projection::ProducerKey(0) }"
            ),
            "{code}"
        );
        assert!(
            code.contains(
                "SourceSpec::StaticArtifact { key: ::marius_projection::SourceKey(2) }"
            ),
            "{code}"
        );
        assert!(
            code.contains("SegmentSelection::NotApplicable"),
            "{code}"
        );
        assert!(code.contains("SegmentFlags::VOLATILE"), "{code}");
        assert!(code.contains("volatile_capacity: 512,"), "{code}");
        // Le K=1 monolithique (SourceKey(0), RouteEntry/RouteSpec) reste
        // généré tel quel — CONTENT_CORE_SOURCE_KEY toujours présent
        // (vérifié par le test précédent) : cette route n'a PAS disparu,
        // seule sa représentation T2A (ROUTE_DESCRIPTORS) a changé de forme.
    }

    #[test]
    fn route_without_volatile_region_still_generates_k1() {
        // Un manifeste sans [[volatile_region]] du tout doit produire
        // EXACTEMENT le texte K=1 précédent — non-régression explicite.
        let src = "[[artifact]]\nkey=\"a\"\ncomponent=\"s.t\"\n\
                   [[route]]\nname=\"r\"\npattern=\"/x/{id}\"\nartifact=\"a\"\n\
                   parameter=\"id\"\nselection=\"primary_key\"\n";
        let m = parse_publication(src).unwrap();
        let facts = vec![ComponentFacts {
            component_id: "s.t".into(),
            pk_column: Some("id".into()),
        }];
        let code = generate_publication_code(&m, &facts).unwrap();
        assert!(
            code.contains(
                "sources: &[::marius_projection::SourceSpec::StaticArtifact { key: ::marius_projection::SourceKey(0) }],"
            ),
            "{code}"
        );
        assert!(
            !code.contains("SourceSpec::VolatileSlot"),
            "aucune route de ce manifeste n'a de région volatile : {code}"
        );
        assert!(!code.contains("SegmentSelection::NotApplicable"), "{code}");
    }

    #[test]
    fn volatile_region_head_artifact_must_be_declared() {
        let src = "[[artifact]]\nkey=\"t\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m\"\n\
                   head_artifact=\"missing_head\"\ntail_artifact=\"t\"\ncapacity=64\n";
        assert_err_contains(parse_publication(src), "head_artifact «missing_head» non déclaré");
    }

    #[test]
    fn volatile_region_tail_artifact_must_be_declared() {
        let src = "[[artifact]]\nkey=\"h\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m\"\n\
                   head_artifact=\"h\"\ntail_artifact=\"missing_tail\"\ncapacity=64\n";
        assert_err_contains(parse_publication(src), "tail_artifact «missing_tail» non déclaré");
    }

    #[test]
    fn volatile_region_head_and_tail_must_be_distinct() {
        let src = "[[artifact]]\nkey=\"same\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m\"\n\
                   head_artifact=\"same\"\ntail_artifact=\"same\"\ncapacity=64\n";
        assert_err_contains(parse_publication(src), "doivent être distincts");
    }

    #[test]
    fn two_volatile_regions_on_the_same_component_are_ambiguous() {
        let src = "[[artifact]]\nkey=\"h1\"\n[[artifact]]\nkey=\"t1\"\n\
                   [[artifact]]\nkey=\"h2\"\n[[artifact]]\nkey=\"t2\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m1\"\n\
                   head_artifact=\"h1\"\ntail_artifact=\"t1\"\ncapacity=64\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m2\"\n\
                   head_artifact=\"h2\"\ntail_artifact=\"t2\"\ncapacity=64\n";
        assert_err_contains(parse_publication(src), "déjà couvert par une autre");
    }

    #[test]
    fn volatile_region_component_must_exist_in_registry() {
        let src = "[[artifact]]\nkey=\"h\"\ncomponent=\"s.t\"\n\
                   [[artifact]]\nkey=\"t\"\ncomponent=\"s.t\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m\"\n\
                   head_artifact=\"h\"\ntail_artifact=\"t\"\ncapacity=64\n";
        let m = parse_publication(src).unwrap();
        let errors = validate_against_components(&m, &[]).expect_err("composant absent");
        assert!(errors.iter().any(|e| e.contains("s.t")), "{errors:?}");
    }

    #[test]
    fn volatile_region_head_and_tail_must_share_the_regions_component() {
        let src = "[[artifact]]\nkey=\"h\"\ncomponent=\"s.other\"\n\
                   [[artifact]]\nkey=\"t\"\ncomponent=\"s.t\"\n\
                   [[volatile_region]]\ncomponent=\"s.t\"\nmarker=\"m\"\n\
                   head_artifact=\"h\"\ntail_artifact=\"t\"\ncapacity=64\n";
        let m = parse_publication(src).unwrap();
        let facts = vec![
            ComponentFacts {
                component_id: "s.t".into(),
                pk_column: None,
            },
            ComponentFacts {
                component_id: "s.other".into(),
                pk_column: None,
            },
        ];
        let errors =
            validate_against_components(&m, &facts).expect_err("head porte un autre composant");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("head_artifact") && e.contains("s.other")),
            "{errors:?}"
        );
    }

    #[test]

    fn real_manifest_is_rejected_when_component_is_missing_from_registry() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        let errors = validate_against_components(&m, &[]).expect_err("composant absent");
        assert!(
            errors.iter().any(|e| e.contains("content.core")),
            "{errors:?}"
        );
    }

    #[test]
    fn real_manifest_is_rejected_when_pk_is_composite() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        let facts = vec![ComponentFacts {
            component_id: "content.core".into(),
            pk_column: None,
        }];
        let errors = validate_against_components(&m, &facts).expect_err("PK composite");
        assert!(errors.iter().any(|e| e.contains("PK simple")), "{errors:?}");
    }

    // ── Validation structurelle ─────────────────────────────────────────

    #[test]
    fn unknown_keys_are_rejected() {
        assert_err_contains(
            parse_publication("[[artifact]]\nkey = \"a\"\nextra = 1\n"),
            "clé inconnue «extra»",
        );
        assert_err_contains(parse_publication("foo = 1\n"), "clé inconnue «foo»");
    }

    #[test]
    fn missing_required_key_is_rejected() {
        assert_err_contains(parse_publication("[[artifact]]\n"), "«key» absente");
    }

    #[test]
    fn route_must_reference_a_declared_artifact() {
        let src = "[[route]]\nname=\"r\"\npattern=\"/x/{id}\"\nartifact=\"nope\"\n\
                   parameter=\"id\"\nselection=\"primary_key\"\n";
        assert_err_contains(parse_publication(src), "artefact «nope» non déclaré");
    }

    #[test]
    fn parameter_must_be_the_pattern_placeholder() {
        let src = "[[artifact]]\nkey=\"a\"\ncomponent=\"s.t\"\n\
                   [[route]]\nname=\"r\"\npattern=\"/x/{id}\"\nartifact=\"a\"\n\
                   parameter=\"document_id\"\nselection=\"primary_key\"\n";
        assert_err_contains(parse_publication(src), "unique paramètre");
    }

    #[test]
    fn pattern_without_or_with_several_parameters_is_rejected() {
        for pattern in ["/x", "/x/{a}/{b}"] {
            let src = format!(
                "[[artifact]]\nkey=\"a\"\ncomponent=\"s.t\"\n\
                 [[route]]\nname=\"r\"\npattern=\"{pattern}\"\nartifact=\"a\"\n\
                 parameter=\"a\"\nselection=\"primary_key\"\n"
            );
            assert_err_contains(parse_publication(&src), "unique paramètre");
        }
    }

    #[test]
    fn malformed_patterns_are_rejected() {
        assert!(pattern_parameters("x/{id}").is_err(), "sans / initial");
        assert!(pattern_parameters("/x-{id}").is_err(), "param partiel");
        assert!(pattern_parameters("/x/{id").is_err(), "accolade ouverte");
        assert!(pattern_parameters("/x/{}").is_err(), "nom vide");
        assert!(pattern_parameters("/x/{Id}").is_err(), "majuscule");
        assert!(pattern_parameters("/x/{*rest}").is_err(), "wildcard");
        assert!(pattern_parameters("/x/é").is_err(), "non ASCII");
        assert_eq!(
            pattern_parameters("/a/{id}").unwrap(),
            vec!["id".to_string()]
        );
        assert_eq!(pattern_parameters("/").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unknown_selection_is_rejected() {
        let src = "[[artifact]]\nkey=\"a\"\ncomponent=\"s.t\"\n\
                   [[route]]\nname=\"r\"\npattern=\"/x/{id}\"\nartifact=\"a\"\n\
                   parameter=\"id\"\nselection=\"slug\"\n";
        assert_err_contains(parse_publication(src), "selection «slug» inconnue");
    }

    #[test]
    fn duplicates_are_rejected() {
        assert_err_contains(
            parse_publication("[[artifact]]\nkey=\"a\"\n[[artifact]]\nkey=\"a\"\n"),
            "déclarée deux fois",
        );
        let two_routes = "[[artifact]]\nkey=\"a\"\ncomponent=\"s.t\"\n\
            [[route]]\nname=\"r\"\npattern=\"/x/{id}\"\nartifact=\"a\"\nparameter=\"id\"\nselection=\"primary_key\"\n\
            [[route]]\nname=\"r\"\npattern=\"/y/{id}\"\nartifact=\"a\"\nparameter=\"id\"\nselection=\"primary_key\"\n";
        assert_err_contains(parse_publication(two_routes), "nom déclaré deux fois");
        let same_pattern = two_routes.replace("\"/y/{id}\"", "\"/x/{id}\"").replace(
            "name=\"r\"\npattern=\"/x/{id}\"\nartifact=\"a\"\nparameter=\"id\"\nselection=\"primary_key\"\n[[route]]\nname=\"r\"",
            "name=\"r1\"\npattern=\"/x/{id}\"\nartifact=\"a\"\nparameter=\"id\"\nselection=\"primary_key\"\n[[route]]\nname=\"r2\"",
        );
        assert_err_contains(
            parse_publication(&same_pattern),
            "motif «/x/{id}» déclaré deux fois",
        );
    }

    #[test]
    fn invalid_identifiers_are_rejected() {
        assert_err_contains(
            parse_publication("[[artifact]]\nkey=\"Content-Core\"\n"),
            "clé invalide",
        );
    }

    // ── Artefact sans composant (cas pages_homepage) ────────────────────

    #[test]
    fn artifact_without_component_is_valid_without_primary_key_route() {
        let m = parse_publication("[[artifact]]\nkey = \"pages_homepage\"\n").unwrap();
        assert_eq!(m.artifacts[0].component, None);
        validate_against_components(&m, &[]).expect("aucun composant requis");
        let code = generate_publication_code(&m, &[]).unwrap();
        assert!(code.contains("::core::option::Option::None"), "{code}");
        assert!(code.contains("PAGES_HOMEPAGE_ARTIFACT"), "{code}");
    }

    #[test]
    fn primary_key_route_over_artifact_without_component_is_rejected() {
        let src = "[[artifact]]\nkey=\"pages_homepage\"\n\
                   [[route]]\nname=\"home\"\npattern=\"/h/{id}\"\nartifact=\"pages_homepage\"\n\
                   parameter=\"id\"\nselection=\"primary_key\"\n";
        let m = parse_publication(src).unwrap();
        let errors = validate_against_components(&m, &[]).expect_err("pas de composant");
        assert!(
            errors.iter().any(|e| e.contains("sans composant")),
            "{errors:?}"
        );
    }

    // ── Génération ──────────────────────────────────────────────────────

    #[test]
    fn source_keys_follow_manifest_order() {
        let src = "[[artifact]]\nkey=\"alpha\"\n[[artifact]]\nkey=\"beta\"\ncomponent=\"content.core\"\n\
                   [[route]]\nname=\"r\"\npattern=\"/b/{id}\"\nartifact=\"beta\"\n\
                   parameter=\"id\"\nselection=\"primary_key\"\n";
        let m = parse_publication(src).unwrap();
        let code = generate_publication_code(&m, &content_core_facts()).unwrap();
        assert!(
            code.contains("pub const ALPHA_SOURCE_KEY: ::marius_projection::SourceKey = ::marius_projection::SourceKey(0);"),
            "{code}"
        );
        assert!(
            code.contains("pub const BETA_SOURCE_KEY: ::marius_projection::SourceKey = ::marius_projection::SourceKey(1);"),
            "{code}"
        );
        // Le descripteur de la route sur `beta` référence la source 1.
        assert!(
            code.contains("StaticArtifact { key: ::marius_projection::SourceKey(1) }"),
            "{code}"
        );
    }

    #[test]
    fn generation_refuses_an_inconsistent_manifest() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        assert!(generate_publication_code(&m, &[]).is_err());
    }

    #[test]
    fn generated_code_never_names_render_server_or_transport_types() {
        let m = parse_publication(REAL_MANIFEST).unwrap();
        let code = generate_publication_code(&m, &content_core_facts()).unwrap();
        for forbidden in [
            "RouteEntry",
            "IdSource",
            "marius_render",
            "axum",
            "hyper",
            "EmissionPlan",
            "IoSlice",
        ] {
            assert!(
                !code.contains(forbidden),
                "«{forbidden}» ne doit pas apparaître"
            );
        }
    }
}
