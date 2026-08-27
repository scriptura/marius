// crates/assets/src/scripts.rs

//! Pipeline `[scripts.components]` (Phase 7).
//!
//! Assemble et hache les composants en **ES Modules natifs**.
//! Aucune concaténation de *bundling* n'est effectuée : chaque module source devient un fichier
//! `.js` haché et indépendant. Les directives `import` sont réécrites à la volée pour pointer
//! vers les URLs publiques des dépendances. Le graphe ESM est résolu nativement par le navigateur au runtime.
//!
//! ## Invariants & Cost Discipline
//!
//! - **Zéro Dépendance Cargo Ajoutée :** L'implémentation repose entièrement sur un lexer d'octets
//!   `&[u8]` fait main, une arène plate de graphe (`Vec` + indices), et `blake3` (déjà présent).
//! - **Partage de Logique :** Le lexer bas niveau (`skip_line_comment`, `find_unescaped_quote`, `JsPipelineError`)
//!   est `pub(crate)` afin d'être réutilisé tel quel par `crate::service_worker` (cf. *Handoff §3*).
//!
//! ## Ordre Strict d'Éxecution (Discipline Data-Oriented)
//!
//! Pour garantir que l'URL d'une dépendance soit connue avant la substitution de son importateur,
//! le pipeline impose 3 passes séquentielles isolées :
//!
//! 1. `build_module_arena` : Exploration (I/O + Lexing) et construction du graphe.
//! 2. `topological_order_leaves_first` : Tri topologique pur (zéro I/O, zéro allocation textuelle). Détecte les cycles.
//! 3. `patch_and_hash_modules` : Patch textuel et écriture sur le disque (ordre garanti *feuilles $\rightarrow$ racines* par l'étape 2).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::js_minify::minify_javascript;
use crate::manifest::{
    AssetEntry, AssetUrlRegistry, CanonicalAssetId, hash_content, join_slash, mime_for_extension,
};
use crate::resolve::{ReferenceOrigin, resolve_asset_reference};

/// Position `(octet_depart, longueur)` d'un chemin d'import littéral dans son texte source.
///
/// ## Emprise
///
/// Les coordonnées ciblent exclusivement le chemin, **guillemets exclus**.
///
/// Cet alias sert purement à la lisibilité et à apaiser le lint `clippy::type_complexity`
/// lors d'imbrications (ex: `Option<(ImportSpan, usize)>`). Sémantiquement, cela reste
/// un simple tuple sans surcoût.
pub(crate) type ImportSpan = (usize, usize);

#[derive(Debug)]
pub(crate) enum JsPipelineError {
    Io(PathBuf, std::io::Error),
    Lex(PathBuf, String),
    /// Import non-relatif (`/libs/leaflet.js`, un nom de paquet nu, etc.)
    /// absent du registre d'assets verbatim — même politique fail-hard que
    /// `CssUrlResolutionError`/`WebManifestError` : ce n'est pas un module
    /// de CE pipeline (voir doc `ImportTarget::ExternalAsset`), mais son
    /// absence est quand même fatale, pas un `console.error` runtime.
    AssetNotFound {
        specifier: String,
        filename: String,
        in_file: PathBuf,
    },
    /// Cycle d'imports détecté pendant le tri topologique — mission §3 :
    /// erreur fatale immédiate, jamais une résolution partielle.
    CyclicImport(Vec<PathBuf>),
}

impl fmt::Display for JsPipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsPipelineError::Io(path, e) => {
                write!(
                    f,
                    "scripts : lecture impossible de {} : {e}",
                    path.display()
                )
            }
            JsPipelineError::Lex(path, msg) => {
                write!(f, "scripts : {} : {msg}", path.display())
            }
            JsPipelineError::AssetNotFound {
                specifier,
                filename,
                in_file,
            } => write!(
                f,
                "scripts : AssetNotFound '{specifier}' (fichier '{filename}' absent du registre) \
                 référencé dans {}",
                in_file.display()
            ),
            JsPipelineError::CyclicImport(paths) => {
                let list = paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "scripts : cycle d'imports détecté impliquant : {list}")
            }
        }
    }
}

impl std::error::Error for JsPipelineError {}

// ── Lexer — un seul passage sur &[u8], aucune regex, aucun AST ─────────────

/// Un octet appartient-il à un identifiant JS (partiel : suffisant pour la
/// détection de frontière de mot autour de `import`/`from`, pas une
/// validation complète des identifiants Unicode JS).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// `source[i..]` commence-t-il par `word`, à une frontière de mot stricte
/// des deux côtés (ni précédé ni suivi d'un octet d'identifiant) ?
fn starts_with_word(source: &[u8], i: usize, word: &[u8]) -> bool {
    if !source[i..].starts_with(word) {
        return false;
    }
    let before_ok = i == 0 || !is_ident_byte(source[i - 1]);
    let after_ok = source
        .get(i + word.len())
        .map(|&b| !is_ident_byte(b))
        .unwrap_or(true);
    before_ok && after_ok
}

pub(crate) fn skip_line_comment(source: &[u8], i: usize) -> usize {
    // S'arrête AU newline sans le consommer — le newline reste un
    // caractère significatif pour l'appelant (fin de déclaration `import`
    // sans `from`, cf. `lex_import_statement`).
    source[i..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|rel| i + rel)
        .unwrap_or(source.len())
}

pub(crate) fn skip_block_comment(
    source: &[u8],
    i: usize,
    ctx: &Path,
) -> Result<usize, JsPipelineError> {
    let mut j = i + 2;
    while j < source.len() {
        if source[j] == b'*' && source.get(j + 1) == Some(&b'/') {
            return Ok(j + 2);
        }
        j += 1;
    }
    Err(JsPipelineError::Lex(
        ctx.to_path_buf(),
        "commentaire bloc /* non fermé".to_string(),
    ))
}

/// Retourne l'indice du guillemet FERMANT non échappé — même discipline
/// d'échappement que `strip_css_comments`/`MvarProvider` : une paire
/// échappée (`\'`, `\"`, `` \` ``) avance de deux octets ensemble, jamais
/// interprétée séparément.
pub(crate) fn find_unescaped_quote(
    source: &[u8],
    mut i: usize,
    quote: u8,
    ctx: &Path,
) -> Result<usize, JsPipelineError> {
    while i < source.len() {
        if source[i] == b'\\' && i + 1 < source.len() {
            i += 2;
        } else if source[i] == quote {
            return Ok(i);
        } else {
            i += 1;
        }
    }
    Err(JsPipelineError::Lex(
        ctx.to_path_buf(),
        format!(
            "chaîne ou gabarit non fermé (guillemet '{}' manquant)",
            quote as char
        ),
    ))
}

/// Saute une chaîne (`'`/`"`) ou un littéral gabarit (`` ` ``) traité comme
/// une région opaque. Limite connue et documentée, pas un oubli : les
/// interpolations `${...}` d'un gabarit ne sont pas analysées — un import
/// écrit à l'intérieur d'une interpolation de gabarit (cas extrêmement
/// rare, non idiomatique) ne serait pas détecté. Hors grammaire fermée v1.
pub(crate) fn skip_string_like(
    source: &[u8],
    i: usize,
    quote: u8,
    ctx: &Path,
) -> Result<usize, JsPipelineError> {
    Ok(find_unescaped_quote(source, i + 1, quote, ctx)? + 1)
}

fn skip_ws_and_comments(source: &[u8], mut i: usize, ctx: &Path) -> Result<usize, JsPipelineError> {
    loop {
        while i < source.len() && source[i].is_ascii_whitespace() {
            i += 1;
        }
        if source.get(i) == Some(&b'/') && source.get(i + 1) == Some(&b'/') {
            i = skip_line_comment(source, i);
        } else if source.get(i) == Some(&b'/') && source.get(i + 1) == Some(&b'*') {
            i = skip_block_comment(source, i, ctx)?;
        } else {
            break;
        }
    }
    Ok(i)
}

/// Analyse le contenu d'UNE déclaration `import`, immédiatement après le
/// mot-clé — cherche `from '<chemin>'`/`from "<chemin>"`. Bornée par `;`
/// ou un saut de ligne **uniquement en dehors d'une clause `{ ... }`
/// ouverte** : un import multi-lignes (`import {\n  A,\n  B,\n} from
/// '...'`, imposé par certains formatters dès que la liste de symboles
/// nommés dépasse une longueur seuil) doit rester détecté — un saut de
/// ligne à l'intérieur de la clause n'est jamais la fin de la
/// déclaration. `{`/`}` n'apparaissent jamais ailleurs dans une clause
/// d'import valide (identifiants, `,`, `as`, commentaires, chaînes déjà
/// sautées) : un comptage plat de profondeur suffit, sans pile.
/// Retourne `((offset, len), position_après)` du contenu du chemin
/// (guillemets exclus), ou `None` si :
///  - c'est un `import(...)` dynamique (mission §4, ignoré délibérément) ;
///  - c'est un import sans `from` (`import './x.js';` — effet de bord pur,
///    hors grammaire v1, cf. doc de `lex_imports`) ;
///  - la déclaration est incomplète/malformée avant tout `from`.
fn lex_import_statement(
    source: &[u8],
    start: usize,
    ctx: &Path,
) -> Result<Option<(ImportSpan, usize)>, JsPipelineError> {
    let mut i = skip_ws_and_comments(source, start, ctx)?;

    if source.get(i) == Some(&b'(') {
        return Ok(None); // import(...) dynamique — grammaire fermée.
    }

    let mut brace_depth: i32 = 0;

    while i < source.len() {
        match source[i] {
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth -= 1;
                i += 1;
            }
            b';' | b'\n' if brace_depth == 0 => return Ok(None),
            b'/' if source.get(i + 1) == Some(&b'/') => i = skip_line_comment(source, i),
            b'/' if source.get(i + 1) == Some(&b'*') => i = skip_block_comment(source, i, ctx)?,
            b'\'' | b'"' | b'`' => i = skip_string_like(source, i, source[i], ctx)?,
            _ if starts_with_word(source, i, b"from") => {
                let quote_pos = skip_ws_and_comments(source, i + "from".len(), ctx)?;
                let quote = match source.get(quote_pos) {
                    Some(&q @ (b'\'' | b'"')) => q,
                    _ => return Ok(None),
                };
                let content_start = quote_pos + 1;
                let end = find_unescaped_quote(source, content_start, quote, ctx)?;
                return Ok(Some(((content_start, end - content_start), end + 1)));
            }
            _ => i += 1,
        }
    }

    Ok(None)
}

/// Lexer principal — un seul passage sur `source`, retourne les positions
/// (octet, longueur) du contenu de chaque chemin d'import statique de
/// premier niveau détecté (guillemets exclus, zéro allocation de `String`
/// intermédiaire : chaque span est une sous-tranche empruntée à `source`
/// au moment du patch, jamais copiée ici).
///
/// Le scan de plus haut niveau reste conscient des chaînes/gabarits/
/// commentaires (même nécessité que `strip_css_comments` pour le CSS) :
/// sans ça, le mot `import` pourrait être détecté à tort à l'intérieur
/// d'une chaîne ou d'un commentaire.
///
/// Limite connue, non résolue ici (documentée, pas silencieuse) :
/// aucune distinction division `/` vs littéral regex `/.../ `. Un
/// commentaire `//` à l'intérieur d'un littéral regex (`/foo\/\/bar/`)
/// serait à tort traité comme un début de commentaire de ligne. La
/// désambiguïsation complète division/regex est l'un des problèmes
/// classiques les plus coûteux du lexing JS (elle dépend du token
/// précédent) — hors périmètre de cette grammaire fermée v1.
fn lex_imports(source: &[u8], ctx: &Path) -> Result<Vec<ImportSpan>, JsPipelineError> {
    let mut spans = Vec::new();
    let mut i = 0usize;

    while i < source.len() {
        match source[i] {
            b'/' if source.get(i + 1) == Some(&b'/') => {
                i = skip_line_comment(source, i);
            }
            b'/' if source.get(i + 1) == Some(&b'*') => {
                i = skip_block_comment(source, i, ctx)?;
            }
            b'\'' | b'"' | b'`' => {
                i = skip_string_like(source, i, source[i], ctx)?;
            }
            _ if starts_with_word(source, i, b"import") => {
                let after_keyword = i + "import".len();
                match lex_import_statement(source, after_keyword, ctx)? {
                    Some((span, next)) => {
                        spans.push(span);
                        i = next;
                    }
                    None => i = after_keyword,
                }
            }
            _ => i += 1,
        }
    }

    Ok(spans)
}

// ── Arène DOD — Vec<JsModule> plat, arêtes = indices ────────────────────────

/// Cible d'un import détecté — deux familles disjointes, jamais confondues :
///  - `Module` : import relatif (`./`, `../`), un AUTRE nœud de CETTE
///    arène — arête réelle du DAG, soumise au tri topologique.
///  - `ExternalAsset` : tout le reste (`/libs/leaflet.js`, un nom de
///    paquet nu, une URL externe) — déjà résolu contre `AssetUrlRegistry`
///    au moment de l'EXPLORATION (Passe 1), jamais une arête du DAG : ce
///    pipeline ne possède pas ce fichier, ne le parse jamais, n'a aucune
///    contrainte d'ordre de hachage à son sujet — sa valeur finale est
///    déjà connue avant même que le tri topologique ne commence.
enum ImportTarget {
    Module(usize),
    ExternalAsset(String),
}

struct ImportEdge {
    /// Position (octet, longueur) du chemin littéral dans
    /// `JsModule::source` — guillemets exclus, réutilisée telle quelle
    /// par la passe de patch (Passe 3), jamais recalculée.
    span: ImportSpan,
    target: ImportTarget,
}

struct JsModule {
    /// Chemin absolu canonique — clé de dédoublonnage à l'exploration (un
    /// diamant d'imports ne doit produire qu'un seul nœud).
    path: PathBuf,
    /// Rempli au moment où ce nœud est dépilé du worklist d'exploration —
    /// vide (`String::new()`) entre sa réservation et son traitement,
    /// jamais lu avant (voir `build_module_arena`).
    source: String,
    imports: Vec<ImportEdge>,
}

/// Réserve un index d'arène pour `path` s'il n'en a pas déjà un — ne lit
/// JAMAIS le fichier ici (seule `build_module_arena` le fait, au moment où
/// l'index est dépilé du worklist). Idempotent : un diamant d'imports
/// (deux modules important le même troisième) obtient le même index sans
/// second passage ; un cycle ne boucle jamais à l'infini pour la même
/// raison — la détection du cycle lui-même est le travail de la Passe 2,
/// pas de cette fonction.
fn reserve_module_index(
    path: &Path,
    arena: &mut Vec<JsModule>,
    index_by_path: &mut HashMap<PathBuf, usize>,
    worklist: &mut VecDeque<usize>,
) -> usize {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(&idx) = index_by_path.get(&canonical) {
        return idx;
    }
    let idx = arena.len();
    arena.push(JsModule {
        path: canonical.clone(),
        source: String::new(),
        imports: Vec::new(),
    });
    index_by_path.insert(canonical, idx);
    worklist.push_back(idx);
    idx
}

/// Passe 1 — exploration. Worklist (BFS), pas de récursion : un appel
/// récursif aurait exigé un emprunt vivant sur `arena[idx].source`
/// pendant un appel qui pousse lui-même dans `arena` (réallocation
/// possible du `Vec`) — conflit d'emprunt structurel, pas contournable
/// sans `unsafe`. Le worklist élimine le problème par construction :
/// aucun emprunt ne traverse jamais un point de mutation du `Vec`.
///
/// Seule allocation de chemin en dehors du lexer lui-même : le
/// `path.clone()` en tête de boucle, nécessaire pour la même raison
/// (`fs::read_to_string` emprunte `path`, puis `arena[idx].source = ...`
/// emprunte `arena` en mutable — les deux emprunts ne peuvent pas
/// coexister si `path` est lui-même emprunté depuis `arena[idx]`).
fn build_module_arena(
    entry_paths: &[PathBuf],
    asset_url_registry: &AssetUrlRegistry,
) -> Result<(Vec<JsModule>, Vec<usize>), JsPipelineError> {
    let mut arena: Vec<JsModule> = Vec::new();
    let mut index_by_path: HashMap<PathBuf, usize> = HashMap::new();
    let mut worklist: VecDeque<usize> = VecDeque::new();

    let entry_indices: Vec<usize> = entry_paths
        .iter()
        .map(|p| reserve_module_index(p, &mut arena, &mut index_by_path, &mut worklist))
        .collect();

    while let Some(idx) = worklist.pop_front() {
        let path = arena[idx].path.clone();
        let source = fs::read_to_string(&path).map_err(|e| JsPipelineError::Io(path.clone(), e))?;
        let raw_spans = lex_imports(source.as_bytes(), &path)?;

        let mut imports = Vec::with_capacity(raw_spans.len());
        for (start, len) in raw_spans {
            let specifier = &source[start..start + len];
            let target = if specifier.starts_with('.') {
                let dep_path = path.with_file_name(specifier);
                let dep_idx =
                    reserve_module_index(&dep_path, &mut arena, &mut index_by_path, &mut worklist);
                ImportTarget::Module(dep_idx)
            } else {
                // Import non-relatif — SPEC-canonical-asset-identity.md §2 :
                // convention déjà en usage dans ce pipeline (`/libs/leaflet.js`,
                // cf. tests) = relative à la racine du thème, jamais au
                // fichier contenant l'import. Jamais `RelativeToFile` ici :
                // un specifier non-relatif désigne délibérément une
                // ressource externe au graphe de CE module, pas un sibling.
                match resolve_asset_reference(
                    specifier,
                    ReferenceOrigin::RelativeToThemeRoot,
                    asset_url_registry,
                ) {
                    Ok(Some(resolved)) => ImportTarget::ExternalAsset(resolved),
                    Ok(None) => ImportTarget::ExternalAsset(specifier.to_string()),
                    Err(message) => {
                        return Err(JsPipelineError::AssetNotFound {
                            specifier: specifier.to_string(),
                            filename: message,
                            in_file: path.clone(),
                        });
                    }
                }
            };
            imports.push(ImportEdge {
                span: (start, len),
                target,
            });
        }

        arena[idx].source = source;
        arena[idx].imports = imports;
    }

    Ok((arena, entry_indices))
}

// ── Tri topologique — Kahn, feuilles → racines, détection de cycle ────────

/// Ordonne les indices de l'arène pour que toute dépendance-MODULE d'un
/// nœud apparaisse AVANT lui — condition nécessaire et suffisante pour
/// que la Passe 3 (patch) connaisse toujours déjà l'URL finale de chaque
/// dépendance au moment de traiter un module. Algorithme de Kahn sur le
/// graphe des dépendances (arêtes `Module` uniquement — `ExternalAsset`
/// n'est jamais une arête, déjà résolu à l'exploration) : un nœud entre
/// dans la file dès que toutes ses dépendances en sont sorties.
///
/// Cycle détecté ⟺ au moins un nœud n'atteint jamais `out_degree == 0` :
/// erreur fatale immédiate (mission §3), aucune tentative de résolution
/// partielle.
fn topological_order_leaves_first(arena: &[JsModule]) -> Result<Vec<usize>, JsPipelineError> {
    let n = arena.len();
    let mut out_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, module) in arena.iter().enumerate() {
        for edge in &module.imports {
            if let ImportTarget::Module(dep_idx) = edge.target {
                out_degree[i] += 1;
                dependents[dep_idx].push(i);
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| out_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(i) = queue.pop_front() {
        order.push(i);
        for &dependent in &dependents[i] {
            out_degree[dependent] -= 1;
            if out_degree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    if order.len() != n {
        let stuck = (0..n)
            .filter(|&i| out_degree[i] > 0)
            .map(|i| arena[i].path.clone())
            .collect();
        return Err(JsPipelineError::CyclicImport(stuck));
    }

    Ok(order)
}

// ── Patch + hash — bottom-up, dans l'ordre de la Passe 2 ───────────────────

/// Métadonnées d'un module patché, retournées à l'appelant pour qu'il
/// décide lui-même lesquelles entrent dans le manifeste (seuls les points
/// d'entrée logiques de `[scripts.components]` y entrent — un module
/// intermédiaire comme `navigation.js` est un artefact de build, jamais
/// référencé directement par `{% asset %}` côté template).
struct PatchedModule {
    url: String,
    output_rel: String,
    full_hash: String,
    size: u64,
}

/// Passe 3 — pour chaque nœud, DANS L'ORDRE `order` (feuilles → racines) :
/// recopie `source` en substituant chaque span d'import par l'URL publique
/// finale de sa cible (déjà connue par construction — soit calculée à une
/// itération précédente de CETTE boucle pour une `Module`, soit déjà
/// résolue à l'exploration pour une `ExternalAsset`), hache le résultat,
/// écrit sur disque.
fn patch_and_hash_modules(
    arena: &[JsModule],
    order: &[usize],
    build_root: &Path,
) -> Result<Vec<Option<PatchedModule>>, Box<dyn std::error::Error>> {
    let mut resolved: Vec<Option<PatchedModule>> = (0..arena.len()).map(|_| None).collect();

    let scripts_dir = build_root.join("scripts");
    fs::create_dir_all(&scripts_dir)?;

    for &idx in order {
        let module = &arena[idx];
        let mut patched = String::with_capacity(module.source.len());
        let mut cursor = 0usize;

        for edge in &module.imports {
            let (start, len) = edge.span;
            patched.push_str(&module.source[cursor..start]);

            let replacement: &str = match &edge.target {
                ImportTarget::Module(dep_idx) => {
                    resolved[*dep_idx].as_ref().map(|p| p.url.as_str()).expect(
                        "dépendance déjà patchée par construction — garanti par l'ordre \
                         topologique de la Passe 2",
                    )
                }
                ImportTarget::ExternalAsset(url) => url.as_str(),
            };
            patched.push_str(replacement);

            cursor = start + len;
        }
        patched.push_str(&module.source[cursor..]);

        // Chantier 4 — minification, dernière étape avant le hachage :
        // le hash doit porter sur les octets RÉELLEMENT servis, pas sur
        // un brouillon intermédiaire plus volumineux qui ne sera jamais
        // écrit sur disque.
        let minified =
            minify_javascript(&patched, &module.path).map_err(|e| format!("scripts   : {e}"))?;
        let bytes = minified.into_bytes();
        let (full_hash, short_hash) = hash_content(&bytes);

        let stem = module
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("module-{idx}"));
        let hashed_filename = format!("{stem}.{short_hash}.js");
        let output_rel = join_slash("scripts", &hashed_filename);
        fs::write(scripts_dir.join(&hashed_filename), &bytes)?;

        resolved[idx] = Some(PatchedModule {
            url: format!("/{output_rel}"),
            output_rel,
            full_hash,
            size: bytes.len() as u64,
        });
    }

    Ok(resolved)
}

pub(crate) fn run_scripts_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    components: &HashMap<String, String>,
    asset_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Même raison que [sprites]/[webmanifest] : HashMap n'a pas d'ordre
    // d'itération garanti, le manifeste doit être reproductible.
    let mut target_names: Vec<&String> = components.keys().collect();
    target_names.sort();

    let entry_paths: Vec<PathBuf> = target_names
        .iter()
        .map(|name| theme_dir.join(&components[*name]))
        .collect();

    let (arena, entry_indices) = build_module_arena(&entry_paths, asset_url_registry)?;
    let order = topological_order_leaves_first(&arena)?;
    let resolved = patch_and_hash_modules(&arena, &order, build_root)?;

    for (target_name, &entry_idx) in target_names.iter().zip(entry_indices.iter()) {
        let patched = resolved[entry_idx]
            .as_ref()
            .expect("chaque point d'entrée est traité par la Passe 3");

        manifest.insert(
            format!("{target_name}.js"),
            AssetEntry {
                url: patched.url.clone(),
                path: join_slash(build_root_rel, &patched.output_rel),
                mime: mime_for_extension("js").to_string(),
                size: patched.size,
                hash: patched.full_hash.clone(),
                version: String::new(), // rempli par l'appelant (theme.version)
            },
        );

        println!(
            "[marius-assets] scripts   {} -> {}",
            components[*target_name], patched.url
        );
    }

    // Modules DÉPENDANCE (importés transitivement via ESM natif, ex.
    // `navigation.js` importé par `main`/`index.js`) — hachés et écrits sur
    // disque par `patch_and_hash_modules` (Passe 3, l'arène ENTIÈRE,
    // dépendances comprises), mais jusqu'ici jamais inscrits au manifeste
    // puisque la boucle ci-dessus ne parcourt QUE les points d'entrée
    // déclarés dans `theme.toml`. Un fichier physiquement présent mais
    // absent de tout registre logique est invisible à `{% asset %}`
    // (Forge) ET à la table de routage statique du Shell (`asset_routes.rs`,
    // elle-même dérivée de ce manifeste) — 404 côté navigateur malgré un
    // build apparemment réussi.
    //
    // Clé : CanonicalAssetId (chemin complet relatif à la racine du thème),
    // SPEC-canonical-asset-identity.md §4 — remplace l'ancienne clé par
    // `file_stem()` seul (`format!("{stem}.js")`), dont ce commentaire
    // documentait lui-même la collision (`foo/utils.js` vs `bar/utils.js`)
    // comme une limite connue et non résolue. Ce n'est plus le cas : deux
    // dépendances de même nom sous des répertoires distincts coexistent
    // désormais sans écrasement, comme partout ailleurs dans ce crate.
    let entry_idx_set: HashSet<usize> = entry_indices.iter().copied().collect();
    for (idx, slot) in resolved.iter().enumerate() {
        if entry_idx_set.contains(&idx) {
            continue; // déjà couvert ci-dessus, sous sa clé logique (nom de cible)
        }
        let Some(patched) = slot else {
            continue; // jamais atteint par la Passe 2/3 — nœud mort, hors graphe réel
        };

        let rel = arena[idx]
            .path
            .strip_prefix(theme_dir)
            .unwrap_or(&arena[idx].path);
        let canonical_key = CanonicalAssetId::from_theme_relative_path(rel).into_string();

        manifest.insert(
            canonical_key,
            AssetEntry {
                url: patched.url.clone(),
                path: join_slash(build_root_rel, &patched.output_rel),
                mime: mime_for_extension("js").to_string(),
                size: patched.size,
                hash: patched.full_hash.clone(),
                version: String::new(),
            },
        );

        println!(
            "[marius-assets] scripts   (dépendance) {} -> {}",
            arena[idx].path.display(),
            patched.url
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── lex_imports (Phase 7, scripts) ───────────────────────────────────────

    fn lex(source: &str) -> Vec<ImportSpan> {
        lex_imports(source.as_bytes(), Path::new("test.js")).unwrap()
    }

    fn lexed_specifiers(source: &str) -> Vec<&str> {
        lex(source)
            .into_iter()
            .map(|(start, len)| &source[start..start + len])
            .collect()
    }

    #[test]
    fn lex_imports_named_import() {
        let src = "import { initNavigation } from './navigation.js';\ninitNavigation();";
        assert_eq!(lexed_specifiers(src), vec!["./navigation.js"]);
    }

    #[test]
    fn lex_imports_default_import() {
        let src = "import L from '/libs/leaflet.js';";
        assert_eq!(lexed_specifiers(src), vec!["/libs/leaflet.js"]);
    }

    /// Mission §4 : grammaire fermée, ignoré délibérément (404 légitime au
    /// runtime si enfreint), pas une erreur de ce lexer.
    #[test]
    fn lex_imports_dynamic_import_is_ignored() {
        let src = "const mod = import('./lazy.js');";
        assert!(lex(src).is_empty());
    }

    /// Hors grammaire v1 (documenté dans `lex_imports`), pas un oubli
    /// silencieux : un import à but d'effet de bord seul, sans `from`.
    #[test]
    fn lex_imports_side_effect_import_without_from_is_ignored() {
        let src = "import './polyfill.js';";
        assert!(lex(src).is_empty());
    }

    /// Bug réel corrigé à la relecture : le chemin lui-même contient le
    /// mot "from" — sans le correctif (sauter les chaînes rencontrées
    /// avant tout `from` légitime), ce mot aurait été pris pour le
    /// mot-clé, avec une extraction de chemin silencieusement fausse à la
    /// clé.
    #[test]
    fn lex_imports_path_containing_the_word_from_is_still_ignored_without_real_from() {
        let src = "import './from-server.js';\nimport { y } from './real.js';";
        assert_eq!(lexed_specifiers(src), vec!["./real.js"]);
    }

    #[test]
    fn lex_imports_ignores_import_keyword_inside_line_comment() {
        let src = "// import { x } from './ghost.js';\nimport { y } from './real.js';";
        assert_eq!(lexed_specifiers(src), vec!["./real.js"]);
    }

    #[test]
    fn lex_imports_ignores_import_keyword_inside_block_comment() {
        let src = "/* import { x } from './ghost.js'; */\nimport { y } from './real.js';";
        assert_eq!(lexed_specifiers(src), vec!["./real.js"]);
    }

    #[test]
    fn lex_imports_ignores_import_keyword_inside_string() {
        let src = "const s = \"import { x } from './ghost.js';\";\nimport { y } from './real.js';";
        assert_eq!(lexed_specifiers(src), vec!["./real.js"]);
    }

    /// Limite connue et assumée (documentée sur `lex_imports`) : un
    /// gabarit est une région opaque, pas d'interpolation `${...}`
    /// analysée. Ce test prouve seulement que l'opacité fonctionne, pas
    /// qu'une interpolation serait gérée.
    #[test]
    fn lex_imports_skips_template_literal_as_opaque() {
        let src = "const s = `import fake from './ghost.js'`;\nimport { y } from './real.js';";
        assert_eq!(lexed_specifiers(src), vec!["./real.js"]);
    }

    #[test]
    fn lex_imports_multiple_statements_in_order() {
        let src = "import a from './a.js';\nimport b from './b.js';";
        assert_eq!(lexed_specifiers(src), vec!["./a.js", "./b.js"]);
    }

    #[test]
    fn lex_imports_unterminated_string_is_an_error() {
        assert!(lex_imports(b"import x from './a.js", Path::new("test.js")).is_err());
    }

    #[test]
    fn lex_imports_unterminated_block_comment_is_an_error() {
        assert!(lex_imports(b"/* never closed", Path::new("test.js")).is_err());
    }

    /// Bug réel de session : un import multi-lignes (imposé par certains
    /// formatters comme Biome dès que la clause `{ ... }` dépasse une
    /// longueur seuil) faisait abandonner la recherche de `from` au premier
    /// saut de ligne rencontré À L'INTÉRIEUR de la clause — le specifier
    /// n'était alors jamais présenté à `resolve_asset_reference`, traversant
    /// tout le pipeline non réécrit (404 au runtime, sans erreur de build).
    #[test]
    fn lex_imports_multiline_named_import_clause_is_still_detected() {
        let src = "import {\n\t//Deck,\n\tIconLayer,\n\tScatterplotLayer,\n\tTileLayer,\n} from \"/libraries/deckgl/deckgl.js\";";
        assert_eq!(lexed_specifiers(src), vec!["/libraries/deckgl/deckgl.js"]);
    }

    /// Même bug, sans commentaire interne — isole la seule variable en jeu
    /// (le saut de ligne dans la clause), pour ne pas dépendre du bon
    /// fonctionnement de `skip_line_comment` en plus.
    #[test]
    fn lex_imports_multiline_named_import_without_comment() {
        let src = "import {\n\tA,\n\tB,\n} from './multi.js';";
        assert_eq!(lexed_specifiers(src), vec!["./multi.js"]);
    }

    /// Variante default + named du même bug — clause `{ ... }` toujours
    /// détectée après le binding par défaut, peu importe où le saut de
    /// ligne structurant tombe dans la déclaration.
    #[test]
    fn lex_imports_multiline_default_and_named_import() {
        let src = "import Deck, {\n\tIconLayer,\n\tScatterplotLayer,\n} from \"/libraries/deckgl/deckgl.js\";";
        assert_eq!(lexed_specifiers(src), vec!["/libraries/deckgl/deckgl.js"]);
    }

    /// Bug réel de session, bout en bout : un import `ExternalAsset`
    /// multi-lignes doit non seulement être DÉTECTÉ par le lexer (couvert
    /// ci-dessus et dans `lex_imports_*`), mais effectivement RÉÉCRIT par
    /// `patch_and_hash_modules` dans le fichier produit — c'est cette
    /// dernière étape dont l'absence causait le 404 runtime malgré un build
    /// sans erreur.
    #[test]
    fn run_scripts_pipeline_rewrites_multiline_external_import_to_hashed_url() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-scripts-multiline-import");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        let map_dir = theme_dir.join("scripts");
        fs::create_dir_all(&map_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
        map_dir.join("map.js"),
        "import {\n\t//Deck,\n\tIconLayer,\n\tScatterplotLayer,\n\tTileLayer,\n} from \"/libraries/deckgl/deckgl.js\";\n\nconsole.log(IconLayer, ScatterplotLayer, TileLayer);",
    )
    .unwrap();

        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            CanonicalAssetId::from_theme_relative_path(Path::new("libraries/deckgl/deckgl.js")),
            "/libraries/deckgl.7c3a1.js".to_string(),
        );

        let mut components = HashMap::new();
        components.insert("map".to_string(), "scripts/map.js".to_string());
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

        run_scripts_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &components,
            &registry,
            &mut manifest,
        )
        .unwrap();

        let map_url = &manifest["map.js"].url;
        let map_filename = Path::new(map_url).file_name().unwrap();
        let map_written =
            fs::read_to_string(build_root.join("scripts").join(map_filename)).unwrap();

        // Le specifier canonique non haché ne doit plus jamais apparaître.
        assert!(!map_written.contains("/libraries/deckgl/deckgl.js"));
        // L'URL hachée exacte du registre doit être présente.
        assert!(map_written.contains("/libraries/deckgl.7c3a1.js"));

        let _ = fs::remove_dir_all(&sandbox);
    }

    // ── topological_order_leaves_first (Phase 7) ─────────────────────────────

    /// Construit une arène minimale à partir d'une liste d'arêtes
    /// `Module` (pas de vrai fichier, pas de vrai lexer) — suffisant pour
    /// tester le tri topologique isolément de l'exploration disque.
    fn arena_from_edges(edges: &[&[usize]]) -> Vec<JsModule> {
        edges
            .iter()
            .enumerate()
            .map(|(i, deps)| JsModule {
                path: PathBuf::from(format!("mod{i}.js")),
                source: String::new(),
                imports: deps
                    .iter()
                    .map(|&d| ImportEdge {
                        span: (0, 0),
                        target: ImportTarget::Module(d),
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn topological_order_linear_chain_leaves_first() {
        // 0 importe 1, 1 importe 2 : ordre attendu 2, 1, 0 (feuille d'abord).
        let arena = arena_from_edges(&[&[1], &[2], &[]]);
        assert_eq!(
            topological_order_leaves_first(&arena).unwrap(),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn topological_order_diamond_dependency_processes_shared_leaf_once() {
        // 0 importe 1 et 2 ; 1 et 2 importent tous deux 3 (diamant).
        let arena = arena_from_edges(&[&[1, 2], &[3], &[3], &[]]);
        let order = topological_order_leaves_first(&arena).unwrap();
        assert_eq!(order.len(), 4);
        // 3 doit précéder 1 et 2, qui doivent tous deux précéder 0.
        let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(3) < pos(1));
        assert!(pos(3) < pos(2));
        assert!(pos(1) < pos(0));
        assert!(pos(2) < pos(0));
    }

    /// Mission §3 : un cycle doit être une erreur fatale immédiate.
    #[test]
    fn topological_order_detects_cycle() {
        let arena = arena_from_edges(&[&[1], &[0]]); // 0 -> 1 -> 0
        assert!(topological_order_leaves_first(&arena).is_err());
    }

    #[test]
    fn topological_order_empty_arena_is_empty_order() {
        let arena: Vec<JsModule> = Vec::new();
        assert_eq!(
            topological_order_leaves_first(&arena).unwrap(),
            Vec::<usize>::new()
        );
    }

    // ── run_scripts_pipeline — intégration bout-en-bout (Phase 7) ────────────
    //
    // Reprend le scaffolding exact fourni en session : deux cibles
    // (`main`, `more`), un import relatif intra-thème (`navigation.js`) et
    // un import non-relatif vers une ressource verbatim déjà hachée
    // (`/libs/leaflet.js`).

    #[test]
    fn run_scripts_pipeline_resolves_relative_and_external_imports() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-scripts-ok");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        let main_dir = theme_dir.join("scripts/main");
        let more_dir = theme_dir.join("scripts/more");
        fs::create_dir_all(&main_dir).unwrap();
        fs::create_dir_all(&more_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            main_dir.join("navigation.js"),
            "export const initNavigation = () => { console.log(\"Nav\"); };",
        )
        .unwrap();
        fs::write(
            main_dir.join("index.js"),
            "import { initNavigation } from './navigation.js';\ninitNavigation();",
        )
        .unwrap();
        fs::write(
            more_dir.join("index.js"),
            "// /libs/leaflet.js est une ressource [static.verbatim] hachée en amont.\n\
             import L from '/libs/leaflet.js';",
        )
        .unwrap();

        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            CanonicalAssetId::from_theme_relative_path(Path::new("libs/leaflet.js")),
            "/libs/leaflet.9f8e7.js".to_string(),
        );

        let mut components = HashMap::new();
        components.insert("main".to_string(), "scripts/main/index.js".to_string());
        components.insert("more".to_string(), "scripts/more/index.js".to_string());

        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

        run_scripts_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &components,
            &registry,
            &mut manifest,
        )
        .unwrap();

        // Les cibles logiques ET les modules dépendance entrent au
        // manifeste — correctif de session : un module atteint seulement
        // par import ESM transitif (navigation.js, jamais déclaré comme
        // cible dans theme.toml) doit rester résolvable, sous peine d'être
        // introuvable par tout consommateur de manifest.toml malgré sa
        // présence réelle sur disque (bug constaté en usage réel : 404
        // côté navigateur).
        //
        // Clé de navigation.js : chemin canonique complet
        // (SPEC-canonical-asset-identity.md §4), plus "navigation.js" seul
        // — l'ancienne clé par `file_stem()` collisionnerait entre deux
        // dépendances homonymes sous des répertoires distincts, ce que ce
        // test ne peut pas exercer isolément (un seul fichier nommé
        // "navigation.js" ici) mais que `verbatim.rs`
        // (`no_silent_overwrite_between_same_basename_in_different_directories`)
        // couvre explicitement pour le même invariant.
        assert!(manifest.contains_key("main.js"));
        assert!(manifest.contains_key("more.js"));
        assert!(manifest.contains_key("scripts/main/navigation.js"));
        assert!(
            manifest["scripts/main/navigation.js"]
                .url
                .starts_with("/scripts/navigation.")
        );
        assert!(manifest["scripts/main/navigation.js"].url.ends_with(".js"));

        let main_url = &manifest["main.js"].url;
        let main_filename = Path::new(main_url).file_name().unwrap();
        let main_written =
            fs::read_to_string(build_root.join("scripts").join(main_filename)).unwrap();

        // L'import relatif doit pointer vers l'URL hachée RÉELLE de
        // navigation.js — pas vers './navigation.js' ni vers un
        // placeholder. On vérifie le PRÉFIXE de l'URL résolue, pas le nom
        // du binding local appelé : le mangling (Chantier 4) peut
        // légitimement renommer `initNavigation` en un identifiant court
        // à l'intérieur de main/index.js (binding purement local à ce
        // fichier, jamais son nom exporté — cf. commentaire de
        // `minify_javascript`), ce n'est pas une régression.
        assert!(!main_written.contains("./navigation.js"));
        assert!(main_written.contains("/scripts/navigation."));

        // Cohérence : l'URL substituée dans main_written doit être EXACTEMENT
        // celle du manifeste — pas seulement un préfixe qui matcherait par
        // coïncidence.
        assert!(main_written.contains(manifest["scripts/main/navigation.js"].url.as_str()));

        let more_url = &manifest["more.js"].url;
        let more_filename = Path::new(more_url).file_name().unwrap();
        let more_written =
            fs::read_to_string(build_root.join("scripts").join(more_filename)).unwrap();

        // L'import non-relatif est réécrit vers l'URL exacte du registre.
        assert!(more_written.contains("/libs/leaflet.9f8e7.js"));
        assert!(!more_written.contains("/libs/leaflet.js'"));

        let _ = fs::remove_dir_all(&sandbox);
    }

    /// Fail-hard (même politique que CSS/webmanifest) : un import
    /// non-relatif absent du registre doit faire échouer tout le
    /// pipeline.
    #[test]
    fn run_scripts_pipeline_fails_hard_on_missing_external_asset() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-scripts-missing");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        let dir = theme_dir.join("scripts/main");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(dir.join("index.js"), "import L from '/libs/leaflet.js';").unwrap();

        let registry = AssetUrlRegistry::new(); // vide : rien à trouver
        let mut components = HashMap::new();
        components.insert("main".to_string(), "scripts/main/index.js".to_string());
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

        let result = run_scripts_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &components,
            &registry,
            &mut manifest,
        );
        assert!(result.is_err());
        assert!(manifest.is_empty());

        let _ = fs::remove_dir_all(&sandbox);
    }

    /// SPEC-canonical-asset-identity.md §11 — critère d'acceptation
    /// central, appliqué ici au correctif spécifique de cette session :
    /// deux modules-dépendance de même nom, atteints par import relatif
    /// depuis deux cibles logiques distinctes, sous des répertoires
    /// différents. Aurait échoué sous l'ancienne clé par `file_stem()`
    /// (écrasement silencieux, un seul des deux "utils.js" aurait survécu
    /// au manifeste).
    #[test]
    fn dependency_modules_with_same_basename_under_different_targets_do_not_collide() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-scripts-no-collision");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        let foo_dir = theme_dir.join("scripts/foo");
        let bar_dir = theme_dir.join("scripts/bar");
        fs::create_dir_all(&foo_dir).unwrap();
        fs::create_dir_all(&bar_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(foo_dir.join("utils.js"), "export const tag = 'foo';").unwrap();
        fs::write(
            foo_dir.join("index.js"),
            "import { tag } from './utils.js';\nconsole.log(tag);",
        )
        .unwrap();
        fs::write(bar_dir.join("utils.js"), "export const tag = 'bar';").unwrap();
        fs::write(
            bar_dir.join("index.js"),
            "import { tag } from './utils.js';\nconsole.log(tag);",
        )
        .unwrap();

        let registry = AssetUrlRegistry::new();
        let mut components = HashMap::new();
        components.insert("foo".to_string(), "scripts/foo/index.js".to_string());
        components.insert("bar".to_string(), "scripts/bar/index.js".to_string());
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

        run_scripts_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &components,
            &registry,
            &mut manifest,
        )
        .unwrap();

        assert!(manifest.contains_key("scripts/foo/utils.js"));
        assert!(manifest.contains_key("scripts/bar/utils.js"));
        assert_ne!(
            manifest["scripts/foo/utils.js"].url, manifest["scripts/bar/utils.js"].url,
            "les deux modules homonymes doivent produire des URLs hachées distinctes \
             (contenus différents, hash différent) — aucune écrasée par l'autre"
        );
        assert_eq!(
            manifest.len(),
            4,
            "foo.js + bar.js (cibles) + scripts/foo/utils.js + scripts/bar/utils.js (dépendances)"
        );

        let _ = fs::remove_dir_all(&sandbox);
    }
}
