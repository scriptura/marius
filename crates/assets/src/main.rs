// =============================================================================
// crates/assets/src/main.rs
//
// marius-assets — compilateur AOT d'assets statiques du thème Marius.
// Outil de build hôte exclusivement (aucune trace runtime dans le Shell ni
// le Core no_std) — voir marius-assets-specification.md et
// marius-assets-HANDOFF.md pour le contexte complet.
//
// Étape 1 de la roadmap d'implémentation : pipelines [static.verbatim],
// [styles] (variables `$`, boucles `@for`, url() généralisée — Phase 5,
// Roadmap §1.8 tranchée), [sprites] (Phase 4), [webmanifest] (Phase 6) et
// [scripts.components] (Phase 7, ES Modules natifs, arène DOD).
//
// Le contenu de `build_root` est intégralement régénéré à chaque
// invocation (voir `main`, purge avant tout pipeline) : aucun fichier de
// build n'a de raison de survivre à un build dont il n'est plus issu.
//
// Invariant DOD respecté : traitement séquentiel, un seul passage par
// fichier, aucune structure de données hiérarchique, aucun trait dynamique.
// Ce n'est PAS le chemin chaud du Shell (§9 de la spec) : les allocations
// (String, Vec, HashMap) sont acceptées ici sans restriction — ce
// programme s'exécute une fois, sur la machine hôte, jamais par requête.
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use lightningcss::bundler::{Bundler, ResolveResult, SourceProvider};
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions};
use lightningcss::values::url::Url;
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

// Pipeline [webmanifest] (Phase 6) — mutation ciblée d'un arbre JSON
// générique : seul icons[].src est muté, tout le reste du document W3C
// (présent ou futur, connu ou non) traverse intact.
use serde_json::Value;

// Pipeline [sprites] (Phase 4) — parseur pull, aucun DOM construit : un
// seul passage par fichier SVG, mémoire proportionnelle au buffer de
// sortie, pas à la taille de l'arbre. `Event<'a>` emprunte directement le
// texte source (&str), aucune copie avant la sérialisation ciblée.
use quick_xml::Reader;
use quick_xml::events::Event;

// =============================================================================
// theme.toml — désérialisation d'entrée
// =============================================================================

#[derive(Deserialize)]
struct ThemeConfig {
    theme: ThemeInfo,
    #[serde(default)]
    styles: StylesConfig,
    // `static` est un mot-clé Rust — renommage obligatoire côté champ, pas
    // côté TOML (la clé `[static.verbatim]` reste inchangée dans le fichier).
    #[serde(rename = "static", default)]
    static_: StaticConfig,
    // [sprites] — Phase 4, cette session : dictionnaire plat nom logique
    // -> dossier source (`silos = "sprites/silos"`). Pas de struct
    // intermédiaire nécessaire, contrairement à `styles`/`static_` : une
    // seule paire clé/valeur par entrée, `HashMap<String, String>` suffit
    // à la représenter fidèlement sans couche superflue.
    #[serde(default)]
    sprites: HashMap<String, String>,
    // [webmanifest] — Phase 6, cette session : un seul point d'entrée (pas
    // une liste comme [styles].entries — un site n'a qu'un seul manifeste
    // PWA par construction W3C). `Option`, pas un champ requis : un thème
    // sans PWA reste un thème valide, ne pas forcer une section vide.
    #[serde(default)]
    webmanifest: Option<WebManifestConfig>,
    // [scripts.components] — Phase 7, cette session : table imbriquée
    // (contrairement à [sprites], à plat) parce que la clé TOML porte deux
    // niveaux (`scripts.components`), pas parce que la donnée elle-même
    // est plus riche — `ScriptsConfig` n'existe que pour porter ce niveau
    // d'imbrication, `components` reste un dictionnaire plat nom logique
    // -> point d'entrée, exactement comme [sprites].
    #[serde(default)]
    scripts: ScriptsConfig,
}

#[derive(Deserialize, Default)]
struct ScriptsConfig {
    #[serde(default)]
    components: HashMap<String, String>,
}

#[derive(Deserialize)]
struct WebManifestConfig {
    entry: String,
}

#[derive(Deserialize)]
struct ThemeInfo {
    name: String,
    version: String,
}

#[derive(Deserialize, Default)]
struct StylesConfig {
    #[serde(default)]
    entries: Vec<String>,
}

#[derive(Deserialize, Default)]
struct StaticConfig {
    #[serde(default)]
    verbatim: VerbatimConfig,
}

#[derive(Deserialize, Default)]
struct VerbatimConfig {
    #[serde(default)]
    files: Vec<String>,
}

// =============================================================================
// manifest.toml — sérialisation de sortie
//
// Forme figée en session avec build.rs (crates/core/schema/build.rs) :
// dictionnaire `[assets."clé"]`, pas un tableau `[[asset]]` — lookup O(1)
// côté lecteur. Les noms de champs ci-dessous doivent rester
// caractère-pour-caractère identiques à la struct AssetEntry de build.rs :
// url, path, mime, size, hash, version. Toute divergence casse le lecteur
// sans erreur de compilation (désérialisation TOML silencieusement
// incomplète) — à ne jamais renommer d'un seul côté sans l'autre.
// =============================================================================

#[derive(Serialize)]
struct AssetManifest {
    assets: HashMap<String, AssetEntry>,
}

#[derive(Serialize)]
struct AssetEntry {
    /// URL publique versionnée, servie telle quelle par le Shell et gravée
    /// telle quelle par `generate_aot_snippet` (fragment-forge). Toujours
    /// préfixée `/`, toujours en slashes avant (jamais de séparateur OS).
    url: String,
    /// Chemin physique du fichier produit, relatif à la racine du workspace
    /// (même convention que les autres artéfacts Marius — cf.
    /// guide-cycle-de-vie-runtime.md §4, résolution par CWD).
    path: String,
    mime: String,
    size: u64,
    /// Empreinte BLAKE3 complète (64 caractères hex) — intégrité, distincte
    /// du suffixe court (5 caractères) utilisé dans le nom de fichier.
    hash: String,
    version: String,
}

// =============================================================================
// Registre des URLs d'assets — spec §10.1 + Roadmap §1.8 (désormais
// tranchée : tout `url()` du CSS est résolu, pas seulement `@font-face`).
// Deux exigences liées :
//  1. Le build CSS doit échouer si une ressource référencée par un `url()`
//     (que ce soit `@font-face`, `background-image`, ou autre) est absente
//     du registre effectivement copié par le pipeline verbatim.
//  2. Ce même registre sert de résolveur d'URL : le `url(...)` littéral
//     écrit par le développeur doit être réécrit vers l'URL publique
//     versionnée avant écriture du CSS final.
//
// Conséquence d'ordonnancement (spec, même §) : le pipeline verbatim doit
// avoir résolu ce registre AVANT que le pipeline styles ne s'exécute — d'où
// le passage explicite par valeur de retour, pas une variable globale ni un
// champ mutable partagé.
//
// Portée désormais élargie à TOUT [static.verbatim] (Phase 5 — c'était
// auparavant limité aux polices woff2/woff/ttf, cf. Handoff Phase 2 : un
// favicon n'a alors aucune raison d'être référencé par un `url()` CSS,
// mais une image de fond en a une, exactement le cas signalé en session).
//
// Clé = nom de fichier seul (pas le chemin complet), hérité tel quel de la
// conception Fonts d'origine — une collision entre deux fichiers homonymes
// dans des sous-dossiers différents de [static.verbatim] n'est pas
// détectée (dernière écriture gagne, silencieusement). Limitation
// préexistante, pas introduite par cette généralisation ; la corriger
// demanderait de résoudre par chemin relatif complet plutôt que par nom de
// fichier seul — portée plus large que ce qui a été demandé ici, à
// reprendre explicitement si une vraie collision se présente (même
// remarque que Roadmap §1.6 pour les SVG).
// =============================================================================
type AssetUrlRegistry = HashMap<String, String>;

// =============================================================================
// Table MIME — correspondance plate, pas de crate de detection générique
// (sympathie mécanique : un match statique suffit, l'ensemble des
// extensions gérées par la spec v1 est fermé et connu à l'avance).
// =============================================================================

fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "css" => "text/css",
        "js" => "application/javascript",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "ico" => "image/vnd.microsoft.icon",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        // W3C Web App Manifest — spec : "application/manifest+json", pas
        // "application/json" générique (Phase 6, [webmanifest]).
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

// =============================================================================
// Hachage — BLAKE3, 5 premiers caractères hex pour le suffixe de nom de
// fichier (convention déjà en production — cf. main.a81f9.css observé).
// Le hash complet (64 caractères) est conservé dans le manifeste (§7).
// =============================================================================

fn hash_content(bytes: &[u8]) -> (String, String) {
    let full_hex = blake3::hash(bytes).to_hex().to_string();
    let short = full_hex[..5].to_string();
    (full_hex, short)
}

// =============================================================================
// Pipeline [static.verbatim] — copie brute, hachage, entrée de manifeste.
//
// Aucune transformation de contenu : le fichier source EST le fichier
// servi, au hash près dans le nom. Clé logique = nom de fichier seul (pas
// le chemin relatif complet) — convention déjà exercée par
// {% asset notoSans-Regular.woff2 %} dans les templates réels.
// =============================================================================

fn run_verbatim_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    files: &[String],
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<AssetUrlRegistry, Box<dyn std::error::Error>> {
    let mut asset_url_registry = AssetUrlRegistry::new();

    for rel_path in files {
        let source_path = theme_dir.join(rel_path);
        let bytes = fs::read(&source_path).map_err(|e| {
            format!(
                "static.verbatim : lecture impossible de {} : {e}",
                source_path.display()
            )
        })?;

        let (full_hash, short_hash) = hash_content(&bytes);

        let rel = Path::new(rel_path);
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        let stem = rel
            .file_stem()
            .ok_or_else(|| format!("static.verbatim : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy();
        let ext: String = rel
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();

        let hashed_filename = if ext.is_empty() {
            format!("{stem}.{short_hash}")
        } else {
            format!("{stem}.{short_hash}.{ext}")
        };

        // Sous-chemin de sortie relatif à build_root — mêmes segments de
        // répertoire que la source (favicons/, fonts/...), slashes forcés.
        let output_rel = join_slash(&path_to_slash(parent), &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, &bytes)?;

        let logical_key = rel
            .file_name()
            .ok_or_else(|| format!("static.verbatim : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy()
            .into_owned();

        let url = format!("/{output_rel}");

        // Alimentation du registre d'URLs — désormais TOUT [static.verbatim]
        // (Phase 5), pas seulement les extensions de police : n'importe quel
        // fichier copié verbatim est potentiellement référencé par un
        // `url()` CSS (background-image, favicon en CSS custom, etc.), pas
        // seulement les polices via `@font-face`.
        asset_url_registry.insert(logical_key.clone(), url.clone());

        manifest.insert(
            logical_key,
            AssetEntry {
                url,
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension(&ext).to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(), // rempli par l'appelant (theme.version)
            },
        );

        println!("[marius-assets] verbatim  {rel_path} -> /{output_rel}");
    }

    Ok(asset_url_registry)
}

// =============================================================================
// Pipeline [webmanifest] — Phase 6. Dépend UNIQUEMENT de `AssetUrlRegistry`
// (résolu par [static.verbatim], donc placé juste après lui — aucune
// dépendance avec [sprites]/[styles], ordre libre vis-à-vis d'eux).
//
// Écart assumé par rapport au prompt suggestif reçu en session : il
// proposait soit `serde_json::Value` soit une struct typée avec
// `#[serde(flatten)]`. J'ai tranché pour `Value` sans hésitation — un Web
// App Manifest W3C a des dizaines de clés optionnelles possibles (`name`,
// `screenshots`, `shortcuts`, `share_target`, `protocol_handlers`,
// extensions spécifiques aux navigateurs...), dont certaines pas encore
// stables ou pas encore nées au moment de l'écriture. Une struct avec
// `#[serde(flatten)]` demanderait quand même de lister explicitement tout
// ce qu'on veut préserver de façon typée ; oublier une seule clé future la
// ferait passer dans le fourre-tout `flatten` avec un risque de
// réordonnancement ou de perte de nuance de type. `Value` ne fait
// AUCUNE hypothèse sur la forme du document au-delà de ce qu'on mute
// explicitement (`icons[].src`) — c'est la seule garantie honnête de
// non-destruction pour un format dont le sur-ensemble de clés n'est pas
// fermé, contrairement à `theme.toml` (grammaire interne, fermée, que
// NOUS contrôlons) qui justifie au contraire des structs typées ailleurs
// dans ce fichier.
// =============================================================================

#[derive(Debug)]
struct WebManifestError(String);

impl fmt::Display for WebManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetNotFound (webmanifest icons[].src) : {}", self.0)
    }
}

impl std::error::Error for WebManifestError {}

fn run_webmanifest_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    config: &WebManifestConfig,
    asset_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_path = theme_dir.join(&config.entry);
    let source_text = fs::read_to_string(&source_path).map_err(|e| {
        format!(
            "webmanifest : lecture impossible de {} : {e}",
            source_path.display()
        )
    })?;

    let mut document: Value = serde_json::from_str(&source_text).map_err(|e| {
        format!(
            "webmanifest : JSON invalide dans {} : {e}",
            source_path.display()
        )
    })?;

    // Mutation ciblée : SEUL `icons[].src` est touché. `document["icons"]`
    // absent ou de forme inattendue n'est pas une erreur — un manifeste
    // sans `icons` (ou avec une forme que ce pipeline ne reconnaît pas)
    // traverse simplement intact, aucune icône à résoudre.
    if let Some(icons) = document.get_mut("icons").and_then(Value::as_array_mut) {
        for icon in icons.iter_mut() {
            let Some(src) = icon.get("src").and_then(Value::as_str) else {
                continue;
            };

            match resolve_asset_reference(src, asset_url_registry) {
                Ok(Some(resolved)) => {
                    icon["src"] = Value::String(resolved);
                }
                Ok(None) => {
                    // Externe ou fragment pur — rare pour une icône PWA,
                    // mais le W3C ne l'interdit pas (ex. icône hébergée
                    // sur un CDN dédié). Laissé strictement inchangé.
                }
                Err(filename) => return Err(Box::new(WebManifestError(filename))),
            }
        }
    }

    // Re-sérialisation : la mise en forme (indentation, ordre des clés)
    // n'est PAS préservée à l'octet près — `serde_json` réémet sa propre
    // forme canonique. Sans conséquence : c'est un document JSON, pas un
    // CSS où l'ordre des règles a une sémantique de cascade. Compact
    // (`to_string`, pas `to_string_pretty`) : ce fichier est servi au
    // runtime, jamais lu par un humain une fois buildé.
    let serialized = serde_json::to_string(&document).map_err(|e| {
        format!(
            "webmanifest : sérialisation échouée pour {} : {e}",
            source_path.display()
        )
    })?;
    let bytes = serialized.as_bytes();
    let (full_hash, short_hash) = hash_content(bytes);

    let extension = Path::new(&config.entry)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("webmanifest");
    let hashed_filename = format!("manifest.{short_hash}.{extension}");
    let output_abs = build_root.join(&hashed_filename);
    fs::write(&output_abs, bytes)?;

    // Clé logique fixe `manifest.webmanifest`, indépendante du nom de
    // fichier source réel (`config.entry`) — c'est cette clé stable que
    // `<link rel="manifest" href="{% asset manifest.webmanifest %}">`
    // référencera côté template, jamais le nom de fichier source, qui
    // peut changer sans casser les templates.
    manifest.insert(
        "manifest.webmanifest".to_string(),
        AssetEntry {
            url: format!("/{hashed_filename}"),
            path: join_slash(build_root_rel, &hashed_filename),
            mime: mime_for_extension(extension).to_string(),
            size: bytes.len() as u64,
            hash: full_hash,
            version: String::new(), // rempli par l'appelant (theme.version)
        },
    );

    println!(
        "[marius-assets] webmanifest {} -> /{hashed_filename}",
        config.entry
    );

    Ok(())
}

// =============================================================================
// Pipeline [sprites] — Phase 4. Un dossier source = une cible : tous les
// `.svg` du dossier sont fusionnés en un unique `<symbol>` par fichier,
// assemblés dans un seul sprite maître `<svg>` (masqué, `display:none`),
// haché comme tout autre artefact transformé (le hash reflète le sprite
// assemblé, jamais les fichiers sources pris isolément).
//
// Sympathie mécanique : `quick_xml::Reader` est un parseur PULL sur `&str`
// — aucun DOM construit, mémoire proportionnelle à la sortie produite, pas
// à la taille de l'arbre XML. Un seul passage par fichier, la profondeur
// d'imbrication du contenu utile (tout ce qui est À L'INTÉRIEUR de la
// balise racine `<svg>`) est suivie par un simple compteur entier — même
// discipline que `find_matching_brace` pour les boucles `@for` : pas de
// pile explicite, la structure XML étant garantie bien formée par le
// parseur lui-même (toute erreur de nesting remonte comme `Err` avant
// d'atteindre notre logique de comptage).
// =============================================================================

#[derive(Debug)]
struct SpriteError(String);

impl fmt::Display for SpriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sprites : {}", self.0)
    }
}

impl std::error::Error for SpriteError {}

/// Sérialise une balise ouvrante (`Start` ou `Empty`) en purgeant `fill`/
/// `stroke` codés en dur — sauf `currentColor`/`none`, qui doivent
/// survivre tels quels (comportement déjà correct, rien à réécrire :
/// `none` signifie explicitement "pas de remplissage", `currentColor` est
/// déjà la manipulabilité CSS visée, pas un codage en dur à éliminer).
/// Toute autre valeur (`#ff0000`, `rgb(...)`, un nom de couleur littéral)
/// fige la couleur au niveau du fichier source — exactement ce que la
/// mission demande de retirer pour laisser le CSS du thème piloter la
/// couleur au runtime.
fn serialize_start(
    e: &quick_xml::events::BytesStart,
    self_closing: bool,
) -> Result<String, SpriteError> {
    let mut out = String::new();
    out.push('<');
    out.push_str(&String::from_utf8_lossy(e.name().as_ref()));

    for attr in e.attributes() {
        let attr = attr.map_err(|err| SpriteError(format!("attribut XML invalide : {err}")))?;
        let key = attr.key.as_ref();
        if matches!(key, b"fill" | b"stroke") {
            let value = attr.value.as_ref();
            if value != b"currentColor" && value != b"none" {
                continue; // purgé — codé en dur, ni currentColor ni none.
            }
        }
        out.push(' ');
        out.push_str(&String::from_utf8_lossy(key));
        out.push_str("=\"");
        out.push_str(&String::from_utf8_lossy(attr.value.as_ref()));
        out.push('"');
    }

    out.push_str(if self_closing { " />" } else { ">" });
    Ok(out)
}

/// Transforme un fichier SVG source en `<symbol id="...">...</symbol>` —
/// entêtes `<?xml...?>`/`<!DOCTYPE...>` ignorés, seul le contenu À
/// L'INTÉRIEUR de la balise racine `<svg>` est conservé.
///
/// Suivi de profondeur : `depth` compte les éléments ouverts DEPUIS la
/// racine (elle-même exclue — jamais incrémentée pour son propre `Start`).
/// La balise fermante rencontrée avec `depth == 0` est donc nécessairement
/// celle de la racine elle-même : fin du contenu utile, sans qu'il soit
/// besoin de mémoriser son nom pour la reconnaître.
fn svg_file_to_symbol(id: &str, source: &str) -> Result<String, SpriteError> {
    let mut reader = Reader::from_str(source);
    let mut out = String::new();
    out.push_str("<symbol id=\"");
    out.push_str(id);
    out.push_str("\">");

    let mut root_found = false;
    let mut depth: u32 = 0;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| SpriteError(format!("{id} : XML invalide : {e}")))?;

        match event {
            // Entêtes explicitement ignorés — spec §3 de la mission.
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) | Event::Comment(_) => {}

            Event::Start(e) => {
                if !root_found {
                    if e.local_name().as_ref() == b"svg" {
                        root_found = true;
                    } else {
                        return Err(SpriteError(format!(
                            "{id} : balise racine <svg> attendue, trouvé <{}>",
                            String::from_utf8_lossy(e.name().as_ref())
                        )));
                    }
                } else {
                    depth += 1;
                    out.push_str(&serialize_start(&e, false)?);
                }
            }

            Event::Empty(e) => {
                if !root_found {
                    if e.local_name().as_ref() == b"svg" {
                        // <svg ... /> — racine explicitement vide.
                        break;
                    }
                    return Err(SpriteError(format!(
                        "{id} : balise racine <svg> attendue, trouvé <{}>",
                        String::from_utf8_lossy(e.name().as_ref())
                    )));
                }
                out.push_str(&serialize_start(&e, true)?);
            }

            Event::End(e) => {
                if !root_found {
                    return Err(SpriteError(format!(
                        "{id} : balise fermante inattendue avant la racine <svg>"
                    )));
                }
                if depth == 0 {
                    // Fermeture de la racine elle-même : fin du contenu utile.
                    break;
                }
                depth -= 1;
                out.push_str("</");
                out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                out.push('>');
            }

            Event::Text(e) => {
                if root_found {
                    out.push_str(&String::from_utf8_lossy(&e));
                }
            }

            Event::CData(e) => {
                if root_found {
                    out.push_str("<![CDATA[");
                    out.push_str(&String::from_utf8_lossy(&e));
                    out.push_str("]]>");
                }
            }

            // Référence générale (`&entity;`) — grammaire non rencontrée
            // dans les SVG réels du thème à ce stade, ignorée sans erreur
            // plutôt que de bloquer tout le pipeline pour un cas marginal.
            Event::GeneralRef(_) => {}

            Event::Eof => {
                if !root_found {
                    return Err(SpriteError(format!("{id} : aucune balise <svg> trouvée")));
                }
                break;
            }
        }
    }

    out.push_str("</symbol>");
    Ok(out)
}

fn run_sprites_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    sprites: &HashMap<String, String>,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `theme.toml` désérialise `[sprites]` en HashMap — ordre d'itération
    // non spécifié par le langage. Trier les clés n'est pas une question
    // de style : le manifeste (et son hash indirect via le contenu émis)
    // doit être reproductible d'un build à l'autre sur la même source.
    let mut sprite_names: Vec<&String> = sprites.keys().collect();
    sprite_names.sort();

    for sprite_name in sprite_names {
        let source_dir_rel = &sprites[sprite_name];
        let source_dir = theme_dir.join(source_dir_rel);

        let mut svg_files: Vec<PathBuf> = fs::read_dir(&source_dir)
            .map_err(|e| {
                format!(
                    "sprites : dossier introuvable {} : {e}",
                    source_dir.display()
                )
            })?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("svg"))
            .collect();
        // Même raison que le tri des clés ci-dessus : `read_dir` ne
        // garantit lui non plus aucun ordre — la reproductibilité du
        // sprite maître (donc de son contenu et de son hash) en dépend.
        svg_files.sort();

        let mut symbols = String::new();
        for svg_path in &svg_files {
            let id = svg_path.file_stem().ok_or_else(|| {
                format!("sprites : nom de fichier invalide : {}", svg_path.display())
            })?;
            let id = id.to_string_lossy();

            let source = fs::read_to_string(svg_path).map_err(|e| {
                format!(
                    "sprites : lecture impossible de {} : {e}",
                    svg_path.display()
                )
            })?;

            let symbol = svg_file_to_symbol(&id, &source)
                .map_err(|e| format!("sprites : {} : {e}", svg_path.display()))?;

            symbols.push_str(&symbol);
        }

        let sprite_svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" style="display:none;">{symbols}</svg>"#
        );
        let bytes = sprite_svg.as_bytes();
        let (full_hash, short_hash) = hash_content(bytes);

        let hashed_filename = format!("{sprite_name}.{short_hash}.svg");
        let output_rel = join_slash("sprites", &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, bytes)?;

        let logical_key = format!("{sprite_name}.svg");
        manifest.insert(
            logical_key,
            AssetEntry {
                url: format!("/{output_rel}"),
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension("svg").to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(), // rempli par l'appelant (theme.version)
            },
        );

        println!(
            "[marius-assets] sprites   {source_dir_rel} -> /{output_rel} ({} icônes)",
            svg_files.len()
        );
    }

    Ok(())
}

// =============================================================================
// Pipeline [styles] — bundling + validation Fonts + minification (voir
// transform_css ci-dessous), hachage du résultat
// transformé (pas de la source : le hash doit refléter ce qui est
// effectivement servi), écriture aplatie sous build_root/styles/.
//
// Le sous-dossier de staging (`development/` dans l'exemple) est
// délibérément absorbé : la sortie ne connaît qu'un seul niveau `styles/`.
// =============================================================================

/// Erreur de résolution d'URL CSS (spec §10.1, Roadmap §1.8) — ressource
/// référencée par un `url()` absente du registre verbatim. Échec dur
/// volontaire : pas de valeur par défaut, pas de passthrough silencieux
/// vers une URL non versionnée.
#[derive(Debug)]
struct CssUrlResolutionError(String);

impl fmt::Display for CssUrlResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AssetNotFound (CSS url(), spec §10.1 / Roadmap §1.8) : {}",
            self.0
        )
    }
}

impl std::error::Error for CssUrlResolutionError {}

/// Sépare un `url()`/`src` en (chemin, fragment) — `"sprites/utils.svg#icon"`
/// → `("sprites/utils.svg", "#icon")`, `"sprites/utils.svg"` → (inchangé,
/// `""`). Fonction pure, testable indépendamment de tout AST CSS ou JSON.
fn split_url_fragment(source: &str) -> (&str, &str) {
    match source.find('#') {
        Some(idx) => (&source[..idx], &source[idx..]),
        None => (source, ""),
    }
}

/// Résolution d'une référence de chemin contre `AssetUrlRegistry` — logique
/// PARTAGÉE entre le pipeline `[styles]` (`url()` CSS) et
/// `run_webmanifest_pipeline` (`icons[].src` JSON, Phase 6) : même notion
/// d'URL externe/fragment à ignorer, même extraction de nom de fichier,
/// même échec dur si absent. Un seul point de vérité pour ce
/// comportement — pas deux implémentations qui pourraient un jour diverger
/// silencieusement sur un cas limite (fragment, URL externe...).
///
/// `Ok(None)` : `source` est externe ou un fragment pur, rien à résoudre,
/// ce n'est PAS une erreur. `Err(nom_de_fichier)` : référence locale
/// absente du registre — c'est à l'appelant de l'envelopper dans son
/// propre type d'erreur (`CssUrlResolutionError`, `WebManifestError`...),
/// cette fonction reste agnostique du contexte appelant.
fn resolve_asset_reference(
    source: &str,
    registry: &AssetUrlRegistry,
) -> Result<Option<String>, String> {
    if is_external_url(source) {
        return Ok(None);
    }

    // Sépare un éventuel fragment (`sprites/utils.svg#icon` — un `url()`
    // pointant vers UN symbole précis d'un sprite fusionné, cf. Phase 4) :
    // seul le chemin AVANT `#` est un vrai nom de fichier à chercher dans
    // le registre. Le fragment n'est ni cherché ni interprété ici,
    // seulement réattaché tel quel à l'URL résolue — bug réel rencontré en
    // session : `Path::file_name()` seul traite `#icon` comme faisant
    // partie du nom de fichier, ce qui ne correspond jamais à une clé de
    // registre.
    let (path_part, fragment) = split_url_fragment(source);

    let filename = Path::new(path_part)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_part.to_string());

    match registry.get(&filename) {
        Some(resolved) => Ok(Some(format!("{resolved}{fragment}"))),
        None => Err(filename),
    }
}

/// URL jamais résolue contre `AssetUrlRegistry`, jamais une erreur si
/// absente — deux familles bien distinctes, toutes deux hors du périmètre
/// de ce pipeline :
///  - ressource véritablement externe (schéma explicite, protocole-relatif
///    `//`, ou `data:` — un URI de données n'a pas de "nom de fichier" à
///    chercher dans le registre) ;
///  - référence de FRAGMENT PUR (`url(#mask-id)`) — motif très courant en
///    CSS (`mask`, `clip-path`, `filter`, `fill` référençant un élément
///    `<defs>` SVG inline dans le même document). Il n'y a alors aucun
///    fichier à résoudre, seulement un identifiant d'élément. Bug réel
///    rencontré en session : sans cette exclusion, la généralisation de
///    `url()` (Phase 5) faisait échouer le build sur ce pattern pourtant
///    parfaitement légitime.
///
/// Détection volontairement simple (préfixe/sous-chaîne) — suffisant pour
/// distinguer un chemin de thème relatif (`../images/logo.svg`) de ces
/// deux familles, pas une validation d'URI complète (hors périmètre ici).
fn is_external_url(url: &str) -> bool {
    url.starts_with('#') || url.starts_with("//") || url.starts_with("data:") || url.contains("://")
}

/// Visiteur AST — résout TOUT `url()` du document contre
/// `AssetUrlRegistry` (Phase 5 : Roadmap §1.8 tranchée — `background-image`,
/// `mask`, `cursor`, etc., pas seulement `@font-face`). La validation dure
/// (échec si absent du registre) s'applique désormais uniformément, pas
/// seulement aux polices.
struct CssUrlVisitor<'a> {
    registry: &'a AssetUrlRegistry,
}

impl<'i> Visitor<'i> for CssUrlVisitor<'_> {
    type Error = CssUrlResolutionError;

    fn visit_types(&self) -> VisitTypes {
        visit_types!(URLS)
    }

    fn visit_url(&mut self, url: &mut Url<'i>) -> Result<(), Self::Error> {
        match resolve_asset_reference(url.url.as_ref(), self.registry) {
            Ok(Some(resolved)) => {
                url.url = resolved.into();
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(filename) => Err(CssUrlResolutionError(filename)),
        }
    }
}

// =============================================================================
// Phase 3 — Résolution AOT des `$variables` (dialecte Sass-like du thème).
//
// Piège identifié : `lightningcss` est un parseur W3C strict, il ne peut
// jamais voir un token `$nom`. Toute pré-passe doit donc s'exécuter
// AVANT que quoi que ce soit ne soit tendu à `Bundler`/`StyleSheet::parse`.
//
// Écart écarté : substituer à l'intérieur d'un `SourceProvider::read()`
// pris isolément, fichier par fichier. Ordre réel d'appel du `Bundler` :
// il lit d'abord le texte BRUT du fichier d'entrée en entier, PUIS le
// parse, et ne découvre (donc ne lit) un `@import` qu'à ce moment-là —
// après coup. Si `$brand-primary` est déclaré dans un partial importé
// mais utilisé dans le fichier d'entrée, le registre serait encore vide
// au moment de traiter l'entrée : substitution manquée, pas d'erreur
// franche, corruption silencieuse du CSS émis. C'est exactement le piège
// signalé par l'auteur du projet.
//
// Conséquence architecturale : deux passes strictement séparées, jamais
// fusionnées dans un seul appel — même discipline que la séparation
// extraction-d'usage / substitution actée en Roadmap §2.1 pour le futur
// tree-shaking (éviter un faux cycle en confondant deux passes qui n'ont
// pas la même dépendance de données) :
//
//   Passe A — walk_variable_graph  (lecture seule, texte brut)
//     Parcourt le graphe `@import` par un scan textuel minimal — PAS par
//     `lightningcss` (qui crasherait). Construit le VariableRegistry
//     complet pour TOUT le graphe avant que quiconque ne songe à
//     substituer quoi que ce soit. Ne connaît aucune sémantique CSS
//     (`layer(...)`, media, supports) — seul le chemin importé l'intéresse.
//
//   Passe B — MvarProvider (SourceProvider custom, remplace FileProvider)
//     Une fois le registre figé, `Bundler` s'exécute normalement pour la
//     sémantique réelle (`@import`, `layer(...)`, media/supports — cf.
//     Handoff §1, non ré-implémentée ici). Le seul point d'interception
//     est `read()` : chaque fichier, qu'il soit l'entrée ou n'importe
//     quel import résolu par `Bundler`, passe par la substitution avant
//     que son texte n'atteigne le parseur. Un seul point d'ancrage
//     garantit la couverture totale du graphe sans dupliquer la logique
//     de résolution d'imports de `Bundler` lui-même.
// =============================================================================

/// Registre des variables `$nom -> valeur`, construit par la Passe A et
/// figé (lecture seule) pendant toute la Passe B. Pas de `RefCell`/mutation
/// partagée : le cycle de vie séquentiel (registre complet AVANT premher
/// `read()`) rend toute synchronisation runtime inutile.
type VariableRegistry = HashMap<String, String>;

#[derive(Debug)]
enum MvarError {
    Io(std::io::Error),
    /// `$nom` rencontré à la substitution mais absent du registre — échec
    /// dur, même politique que `CssUrlResolutionError` : pas de passthrough
    /// silencieux d'un token non résolu vers le CSS final.
    ///
    /// `suggestion` est calculée UNE SEULE FOIS, au point de construction
    /// de l'erreur (`substitute_line`, qui a déjà `&VariableRegistry` sous
    /// la main) — pas au moment de l'affichage. Ce n'est pas un détail
    /// cosmétique : l'erreur transporte déjà tout ce dont `Display` a
    /// besoin, sans lui redonner accès au registre.
    UndefinedVariable {
        name: String,
        file: PathBuf,
        suggestion: Option<String>,
    },
    /// Grammaire `@for` malformée (borne manquante, accolade non fermée,
    /// pas nul, etc.) — voir `ForLoopError` plus bas dans ce fichier.
    ForLoop(ForLoopError),
    /// Chaîne ou commentaire CSS non fermé — voir `CssCommentError`,
    /// `strip_css_comments` plus bas. Détecté avant même la recherche de
    /// `$variables`/`@for`, donc toujours la première erreur possible sur
    /// un fichier donné.
    Comment(CssCommentError),
}

impl fmt::Display for MvarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MvarError::Io(e) => write!(f, "styles (variables) : lecture impossible : {e}"),
            MvarError::UndefinedVariable {
                name,
                file,
                suggestion,
            } => {
                write!(
                    f,
                    "styles (variables) : ${name} utilisée mais jamais déclarée (fichier {})",
                    file.display()
                )?;
                match suggestion {
                    Some(hint) => write!(f, " — {hint}"),
                    None => write!(
                        f,
                        " — aucune variable proche dans le registre ; vérifiez l'orthographe \
                         et la présence de la déclaration `${name}: valeur;`."
                    ),
                }
            }
            MvarError::ForLoop(e) => write!(f, "{e}"),
            MvarError::Comment(e) => write!(f, "{e}"),
        }
    }
}

/// Suggestion pour un `$nom` non résolu — deux niveaux de confiance,
/// jamais mélangés dans le même message (une correspondance insensible à
/// la casse est quasi certaine, une correspondance approchée par distance
/// d'édition ne l'est pas, le message ne doit pas prétendre le contraire).
///
/// Priorité 1 — casse différente : le cas le plus probable en pratique
/// (l'auteur du projet a confirmé ce comportement lors de la session
/// précédente : `${name}` sensible à la casse est un choix assumé, pas un
/// bug — mais une faute de casse reste l'erreur la plus fréquente pour
/// autant, elle mérite un message qui la nomme explicitement plutôt qu'un
/// "vouliez-vous dire" générique.
///
/// Priorité 2 — faute de frappe : plus proche voisin par distance de
/// Levenshtein, borné à 2 pour éviter une suggestion trompeuse sur un nom
/// sans rapport réel (mieux vaut aucune suggestion qu'une mauvaise piste).
fn suggest_variable(name: &str, registry: &VariableRegistry) -> Option<String> {
    if let Some(exact_ci) = registry.keys().find(|k| k.eq_ignore_ascii_case(name)) {
        return Some(format!(
            "la casse ne correspond pas : le registre contient ${exact_ci}, pas ${name} \
             (la casse est sensible, comportement assumé)"
        ));
    }

    registry
        .keys()
        .map(|k| (k, levenshtein(name, k)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(k, _)| format!("vouliez-vous dire ${k} ?"))
}

/// Distance de Levenshtein — classique, deux lignes de tableau roulées
/// (`prev`/`curr`) plutôt qu'une matrice complète : le registre de
/// $variables d'un thème compte au plus quelques dizaines d'entrées, la
/// complexité O(n·m) par comparaison est hors de propos ici, seule
/// l'empreinte mémoire (une matrice complète serait un gaspillage sans
/// contrepartie) justifie ce choix.
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

impl std::error::Error for MvarError {}

impl From<std::io::Error> for MvarError {
    fn from(e: std::io::Error) -> Self {
        MvarError::Io(e)
    }
}

// =============================================================================
// Phase 3 (préambule) — Purge des commentaires CSS AVANT tout le reste.
//
// Bug signalé en session : une `$variable` indéfinie ou mal formatée à
// l'intérieur d'un commentaire (`/* $old-var: 10; */`) faisait échouer le
// build alors que ce texte est de la donnée morte — jamais vue par
// `lightningcss`, elle ne devrait jamais être vue par nos pré-passes non
// plus. Principe DOD direct : éliminer la donnée morte le plus tôt
// possible dans le pipeline, avant que quoi que ce soit d'autre n'ait la
// moindre chance de trébucher dessus.
//
// Piège écarté explicitement (celui qui rend une regex ou un
// `.replace("/*", ...)` naïf incorrects) : `/*` et `*/` sont des
// caractères de contenu parfaitement légaux à l'intérieur d'une chaîne
// CSS — `content: "/*";` ne doit jamais être tronqué. Un automate à trois
// états (Normal / DansChaîne / DansCommentaire) est nécessaire, pas une
// recherche de sous-chaîne.
//
// Branchement : appelé dans LES DEUX passes, pas une seule —
//   - Passe A (`walk_variable_graph`) : sans ça, un `$nom: valeur;` ou un
//     `@import "...";` écrit à l'intérieur d'un commentaire multi-lignes
//     serait toujours capturé par `extract_declarations`/
//     `extract_import_targets` (ni l'un ni l'autre ne connaît la notion
//     de commentaire) — un import commenté ferait planter tout le build
//     sur un fichier "manquant" qui n'a jamais eu vocation à exister.
//   - Passe B (`MvarProvider::read`) : corrige directement le bug
//     rapporté (usage de `$var` dans un commentaire).
// Même cause racine dans les deux cas (scan textuel naïf, aveugle aux
// commentaires) : un seul correctif, appliqué aux deux points d'entrée du
// texte source, pas un correctif ponctuel sur le seul symptôme observé.
// =============================================================================

#[derive(Debug)]
struct CssCommentError(String);

impl fmt::Display for CssCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "styles (commentaires) : {}", self.0)
    }
}

impl std::error::Error for CssCommentError {}

/// Purge les commentaires CSS `/* ... */` d'un texte source — automate à
/// trois états, un seul passage O(N) sur les octets, aucune regex.
///
/// États : `Normal` (copie tout), `InString(quote)` (une chaîne CSS est en
/// cours — `/*`/`*/` y sont des caractères ordinaires, jamais des
/// délimiteurs), `InComment` (tout est ignoré jusqu'à `*/`, y compris tout
/// ce qui ressemblerait à une chaîne — CSS n'a pas de commentaires
/// imbriqués, le premier `*/` rencontré ferme, point final).
///
/// Copie par segments (`segment_start..i`), jamais octet par octet : les
/// limites de coupe ne tombent QUE sur des octets ASCII à un seul octet
/// (`/`, `*`, `"`, `'`, `\`), donc toujours des frontières de caractère
/// UTF-8 valides — un contenu non-ASCII (accents dans un commentaire ou
/// une chaîne) traverse la fonction sans risque de corruption, puisqu'il
/// n'est jamais reconstruit octet par octet.
///
/// Échec dur sur chaîne ou commentaire non fermé en fin de fichier — un
/// tel fichier est de toute façon invalide, mieux vaut le signaler ici
/// qu'obtenir un message d'erreur incompréhensible plus loin dans le
/// pipeline.
fn strip_css_comments(input: &str) -> Result<String, CssCommentError> {
    enum State {
        Normal,
        InString(u8),
        InComment,
    }

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut state = State::Normal;
    let mut i = 0usize;
    let mut segment_start = 0usize;

    while i < bytes.len() {
        match state {
            State::Normal => {
                if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    out.push_str(&input[segment_start..i]);
                    state = State::InComment;
                    i += 2;
                } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                    state = State::InString(bytes[i]);
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::InString(quote) => {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    // Paire échappée (ex. `\"`) : avancée ensemble, jamais
                    // interprétée séparément — un guillemet échappé ne
                    // ferme jamais la chaîne prématurément.
                    i += 2;
                } else if bytes[i] == quote {
                    state = State::Normal;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::InComment => {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    i += 2;
                    state = State::Normal;
                    segment_start = i; // reprise de la copie après le commentaire
                } else {
                    i += 1;
                }
            }
        }
    }

    match state {
        State::Normal => {
            out.push_str(&input[segment_start..]);
            Ok(out)
        }
        State::InString(_) => Err(CssCommentError(
            "chaîne non fermée (guillemet manquant avant la fin du fichier)".to_string(),
        )),
        State::InComment => Err(CssCommentError(
            "commentaire non fermé ('*/' manquant avant la fin du fichier)".to_string(),
        )),
    }
}

/// Passe A — walk textuel minimal du graphe `@import`, lecture seule.
///
/// Ne passe jamais par `lightningcss` : un scan ligne-à-ligne suffit, la
/// seule information recherchée est (a) les déclarations `$nom: valeur;`
/// et (b) les cibles `@import "chemin";` à suivre récursivement. Aucune
/// sémantique `layer(...)`/media n'est interprétée ici — seul le chemin
/// importé compte, la sémantique réelle reste intégralement déléguée à
/// `Bundler` en Passe B.
///
/// Hypothèse de grammaire posée explicitement (non vérifiée par l'auteur
/// à ce stade, à confirmer) : une déclaration `$nom: valeur;` tient sur
/// une seule ligne. Aucun `.mcss` réel ne contredit cette hypothèse au
/// moment de l'écriture (cf. Handoff §1 : aucun fichier de test n'utilise
/// encore de variables).
fn build_variable_registry(entry: &Path) -> Result<VariableRegistry, Box<dyn std::error::Error>> {
    let mut registry = VariableRegistry::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    walk_variable_graph(entry, &mut registry, &mut visited)?;
    Ok(registry)
}

fn walk_variable_graph(
    path: &Path,
    registry: &mut VariableRegistry,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Clé de dédoublonnage canonique — un même partial importé deux fois
    // (diamant d'imports) ne doit être ni relu, ni source d'une boucle
    // infinie sur un cycle d'imports.
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(key) {
        return Ok(());
    }

    let text = fs::read_to_string(path).map_err(|e| {
        format!(
            "styles (variables) : lecture impossible de {} : {e}",
            path.display()
        )
    })?;

    // Purge des commentaires AVANT toute recherche de déclaration ou
    // d'import — voir bloc de commentaires "Phase 3 (préambule)" plus
    // haut : sans ça, un `@import` commenté ferait planter le build sur
    // un fichier qui n'a jamais eu vocation à être lu.
    let text = strip_css_comments(&text)
        .map_err(|e| format!("styles (variables) : {} : {e}", path.display()))?;

    extract_declarations(&text, registry);

    for import_rel in extract_import_targets(&text) {
        // Même règle de résolution que `FileProvider::resolve` (spec :
        // chemin relatif au fichier important, jamais à la racine du
        // thème) — dupliquée ici volontairement : la Passe A n'a pas
        // accès à `Bundler`, mais doit rester cohérente avec sa
        // convention de résolution de chemin.
        let import_path = path.with_file_name(&import_rel);
        walk_variable_graph(&import_path, registry, visited)?;
    }

    Ok(())
}

/// Extrait les déclarations `$nom: valeur;` d'un texte source, une par
/// ligne. Purement additif sur `registry` — dernière déclaration lue
/// l'emporte en cas de redéfinition inter-fichiers (portée globale,
/// cohérent avec l'absence de toute notion de scope/import qualifié dans
/// la spec actuelle du dialecte `$variable`).
fn extract_declarations(text: &str, registry: &mut VariableRegistry) {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix('$') else {
            continue;
        };
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        let Some(value) = rest[colon + 1..].trim().strip_suffix(';') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        registry.insert(name.to_string(), value.trim().to_string());
    }
}

/// Extrait les cibles `@import "chemin";` (ou `@import url("chemin");`)
/// d'un texte source. Ne traite que ce dont la Passe A a besoin : le
/// chemin. Les qualificatifs (`layer(...)`, media, supports) sont ignorés
/// ici sans risque — ils ne changent jamais QUEL fichier est importé,
/// seulement comment `Bundler` l'enveloppera en Passe B.
fn extract_import_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("@import") {
            continue;
        }
        let rest = &trimmed["@import".len()..];
        let Some(start) = rest.find(['"', '\'']) else {
            continue;
        };
        let quote = rest.as_bytes()[start] as char;
        let Some(end_rel) = rest[start + 1..].find(quote) else {
            continue;
        };
        targets.push(rest[start + 1..start + 1 + end_rel].to_string());
    }
    targets
}

/// Substitue chaque `$nom` par sa valeur résolue et purge les lignes de
/// déclaration (grammaire CSS fermée, §10.3 de la spec — un token `$nom`
/// non substitué ferait échouer `lightningcss` de toute façon ; le purger
/// en amont est la seule option, pas un choix parmi d'autres).
fn substitute_and_purge(
    text: &str,
    registry: &VariableRegistry,
    file: &Path,
) -> Result<String, MvarError> {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim();
        // Ligne de déclaration : déjà capturée en Passe A, purgée ici
        // pour ne jamais atteindre le parseur (grammaire non reconnue).
        if trimmed.starts_with('$') && trimmed.contains(':') {
            continue;
        }
        out.push_str(&substitute_line(line, registry, file)?);
        out.push('\n');
    }
    Ok(out)
}

/// Substitution caractère-à-caractère d'une seule ligne — un seul passage,
/// aucune allocation intermédiaire hors la chaîne de sortie elle-même.
/// Opère sur `char_indices` (pas `bytes[i] as char`) : une valeur UTF-8
/// multioctet dans un `$nom` de variable romprait un découpage par octet.
fn substitute_line(
    line: &str,
    registry: &VariableRegistry,
    file: &Path,
) -> Result<String, MvarError> {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();

    while let Some((idx, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }

        let start = idx + c.len_utf8();
        let mut end = start;
        while let Some(&(j, ch)) = chars.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                end = j + ch.len_utf8();
                chars.next();
            } else {
                break;
            }
        }

        if end == start {
            // '$' isolé, sans nom derrière : pas une variable, recopié tel quel.
            out.push(c);
            continue;
        }

        let name = &line[start..end];
        match registry.get(name) {
            Some(value) => out.push_str(value),
            None => {
                return Err(MvarError::UndefinedVariable {
                    name: name.to_string(),
                    file: file.to_path_buf(),
                    suggestion: suggest_variable(name, registry),
                });
            }
        }
    }

    Ok(out)
}

// =============================================================================
// Phase 3 (suite) — Déroulage AOT des boucles `@for` (dialecte Sass-like).
//
// Différence structurelle avec le registre de $variables plates ci-dessus :
// une boucle `@for` est ENTIÈREMENT locale à son fichier — borne, pas et
// corps sont dans le même texte. Pas de piège d'ordre inter-fichiers ici,
// donc pas de Passe A dédiée : le déroulage tient dans le point
// d'interception déjà en place (`MvarProvider::read`).
//
// Ordre à l'intérieur de `read()` (voir plus bas) :
//   1. `expand_for_loops`     — élimine tout `@for`, ne substitue QUE la
//                                variable de boucle courante ($i / $(i)),
//                                laisse tout autre `$nom` strictement
//                                intact.
//   2. `substitute_and_purge` — résout les `$nom` globaux restants via le
//                                VariableRegistry de la Passe A.
// Ne jamais inverser : `substitute_and_purge` échoue dur sur tout `$nom`
// non déclaré — inversé, elle verrait encore `$i` et le traiterait comme
// une variable globale absente. C'est exactement l'erreur observée
// (`UndefinedVariable("i", …)`) avant ce correctif.
// =============================================================================

#[derive(Debug)]
struct ForLoopError(String);

impl fmt::Display for ForLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "styles (@for) : {}", self.0)
    }
}

impl std::error::Error for ForLoopError {}

/// Déroule tous les `@for $var from A to B [by S] { ... }` d'un texte.
/// Récursif : le corps isolé (comptage d'accolades, pas de regex) est
/// entièrement déplié AVANT d'être dupliqué par la boucle englobante — une
/// boucle imbriquée est donc traitée une seule fois, pas de second passage
/// nécessaire. Limite assumée : deux boucles imbriquées partageant le même
/// nom de variable ($i dans les deux) ne sont pas gardées contre une
/// collision — cas non rencontré dans les fichiers actuels, à traiter si
/// besoin réel se présente.
///
/// Hypothèse de grammaire à confirmer explicitement : `to` est ici traité
/// comme EXCLUSIF de la borne haute (convention Sass standard — `through`
/// serait inclusif, mais n'apparaît pas dans la grammaire cible donnée).
/// Conséquence concrète sur votre second exemple : `@for $i from 90 to
/// 180 by 90` ne produit qu'UNE itération (i = 90, 180 exclu) avec cette
/// hypothèse. Si vous attendiez `rotate90` ET `rotate180`, c'est `to`
/// inclusif qu'il faut — je ne tranche pas ce point à votre place, à
/// confirmer avant de considérer cette Phase 3 close.
fn expand_for_loops(text: &str) -> Result<String, ForLoopError> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;

    while let Some(rel) = text[cursor..].find("@for") {
        let for_start = cursor + rel;
        out.push_str(&text[cursor..for_start]);

        let mut i = for_start + "@for".len();
        i = skip_ws(bytes, i);

        i = expect_byte(bytes, i, b'$')
            .ok_or_else(|| ForLoopError(format!("'$' attendu après @for (position {i})")))?;
        let (var_name, next) = parse_ident(text, i).ok_or_else(|| {
            ForLoopError(format!("nom de variable attendu après '$' (position {i})"))
        })?;
        i = skip_ws(bytes, next);

        i = expect_literal(text, i, "from")
            .ok_or_else(|| ForLoopError(format!("mot-clé 'from' attendu (position {i})")))?;
        i = skip_ws(bytes, i);
        let (start, next) = parse_int(text, i)
            .ok_or_else(|| ForLoopError(format!("borne basse entière attendue (position {i})")))?;
        i = skip_ws(bytes, next);

        i = expect_literal(text, i, "to")
            .ok_or_else(|| ForLoopError(format!("mot-clé 'to' attendu (position {i})")))?;
        i = skip_ws(bytes, i);
        let (end, next) = parse_int(text, i)
            .ok_or_else(|| ForLoopError(format!("borne haute entière attendue (position {i})")))?;
        i = skip_ws(bytes, next);

        let mut step: i64 = 1;
        if let Some(after_by) = expect_literal(text, i, "by") {
            let after_ws = skip_ws(bytes, after_by);
            let (s, next) = parse_int(text, after_ws).ok_or_else(|| {
                ForLoopError(format!(
                    "pas entier attendu après 'by' (position {after_ws})"
                ))
            })?;
            if s == 0 {
                return Err(ForLoopError("pas ('by') ne peut pas être 0".to_string()));
            }
            step = s;
            i = skip_ws(bytes, next);
        }

        i = expect_byte(bytes, i, b'{').ok_or_else(|| {
            ForLoopError(format!(
                "'{{' attendu pour ouvrir le corps de boucle (position {i})"
            ))
        })?;

        let body_start = i;
        let body_end = find_matching_brace(bytes, body_start)
            .ok_or_else(|| ForLoopError("accolade fermante manquante pour @for".to_string()))?;
        let raw_body = &text[body_start..body_end];

        // Récursion AVANT duplication : toute boucle imbriquée dans le
        // corps est entièrement dépliée une seule fois ici.
        let expanded_body = expand_for_loops(raw_body)?;

        let mut i_iter = start;
        loop {
            let done = if step > 0 {
                i_iter >= end
            } else {
                i_iter <= end
            };
            if done {
                break;
            }
            out.push_str(&substitute_loop_variable(&expanded_body, &var_name, i_iter));
            i_iter += step;
        }

        cursor = body_end + 1; // juste après la '}' fermante du @for
    }

    out.push_str(&text[cursor..]);
    Ok(out)
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

fn expect_byte(bytes: &[u8], i: usize, b: u8) -> Option<usize> {
    if i < bytes.len() && bytes[i] == b {
        Some(i + 1)
    } else {
        None
    }
}

fn expect_literal(text: &str, i: usize, lit: &str) -> Option<usize> {
    text.get(i..)?.strip_prefix(lit).map(|_| i + lit.len())
}

fn parse_ident(text: &str, i: usize) -> Option<(String, usize)> {
    let rest = text.get(i..)?;
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some((rest[..end].to_string(), i + end))
    }
}

fn parse_int(text: &str, i: usize) -> Option<(i64, usize)> {
    let rest = text.get(i..)?;
    let bytes = rest.as_bytes();
    let mut end = 0;
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    rest[..end].parse::<i64>().ok().map(|v| (v, i + end))
}

/// Comptage d'accolades — pas de regex, la grammaire n'est pas régulière
/// (le corps contient ses propres règles CSS imbriquées avec `{`/`}`).
/// `open_pos` pointe sur la '{' d'ouverture ; retourne l'indice de la '}'
/// fermante correspondante (profondeur 0).
fn find_matching_brace(bytes: &[u8], open_pos: usize) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut i = open_pos + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Remplace UNIQUEMENT `$(nom)` et `$nom` (frontière de mot stricte) par
/// la valeur de l'itération courante — tout autre `$token` (variable
/// globale pas encore résolue, ex. `$demoColorDeviation`) traverse
/// strictement inchangé. Volontairement non-erronant sur un `$autre`
/// rencontré : ce n'est pas son rôle, `substitute_and_purge` s'en charge
/// en aval, une fois le registre global connu.
fn substitute_loop_variable(body: &str, var_name: &str, value: i64) -> String {
    let value_str = value.to_string();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;

    while i < body.len() {
        if body.as_bytes()[i] != b'$' {
            let next_dollar = body[i..].find('$').map(|r| i + r).unwrap_or(body.len());
            out.push_str(&body[i..next_dollar]);
            i = next_dollar;
            continue;
        }

        // Forme interpolée : $(nom)
        if body
            .get(i + 1..)
            .map(|s| s.starts_with('('))
            .unwrap_or(false)
        {
            let name_start = i + 2;
            if let Some(close_rel) = body[name_start..].find(')') {
                let name_end = name_start + close_rel;
                let name = &body[name_start..name_end];
                if name == var_name {
                    out.push_str(&value_str);
                } else {
                    // $(autre_nom) : pas notre variable, recopié tel quel.
                    out.push_str(&body[i..name_end + 1]);
                }
                i = name_end + 1;
                continue;
            }
            // '(' sans ')' fermante : pas une interpolation valide, '$'
            // recopié seul, le reste suit son cours normalement.
            out.push('$');
            i += 1;
            continue;
        }

        // Forme nue : $nom, frontière de mot stricte.
        let name_start = i + 1;
        let name_end = body[name_start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .map(|r| name_start + r)
            .unwrap_or(body.len());

        if name_end > name_start {
            let name = &body[name_start..name_end];
            if name == var_name {
                out.push_str(&value_str);
            } else {
                out.push_str(&body[i..name_end]);
            }
            i = name_end;
        } else {
            out.push('$');
            i += 1;
        }
    }

    out
}

/// Passe B — `SourceProvider` custom, remplace `FileProvider` en entrée de
/// `Bundler`. Seul point d'interception : chaque fichier du graphe,
/// entrée comme import résolu par `Bundler` lui-même, transite par
/// `read()` avant tout parsing — la substitution y est donc appliquée de
/// façon globale et transparente sans dupliquer la logique d'import de
/// `Bundler`.
///
/// Contrainte de signature à respecter strictement :
/// `read<'a>(&'a self, file: &Path) -> Result<&'a str, Self::Error>` — la
/// référence retournée est liée à la durée de vie de `&self`, pas de
/// l'appel. Un `String` local ne peut donc pas être retourné par `&str`
/// sans que son adresse reste stable après la fin de `read()`. Solution
/// identique à celle déjà employée par `FileProvider` lui-même (lu dans
/// le code source du crate, §"Point de vigilance" du Handoff toujours
/// valable) : `Box::into_raw` + accumulation des pointeurs, libérés au
/// `Drop` de `MvarProvider`, jamais retirés du `Vec` entre-temps.
struct MvarProvider {
    registry: VariableRegistry,
    outputs: Mutex<Vec<*mut String>>,
}

impl MvarProvider {
    fn new(registry: VariableRegistry) -> Self {
        MvarProvider {
            registry,
            outputs: Mutex::new(Vec::new()),
        }
    }
}

// SAFETY : même justification que `FileProvider` dans lightningcss —
// aucun état mutable partagé n'est exposé sans passer par le `Mutex`, et
// les pointeurs accumulés ne sont jamais déréférencés en dehors de ce
// fichier ni retirés avant le `Drop`.
unsafe impl Sync for MvarProvider {}
unsafe impl Send for MvarProvider {}

impl SourceProvider for MvarProvider {
    type Error = MvarError;

    fn read<'a>(&'a self, file: &Path) -> Result<&'a str, Self::Error> {
        let raw = fs::read_to_string(file)?;
        // Ordre impératif, trois étapes : purge des commentaires D'ABORD
        // (donnée morte éliminée avant que quoi que ce soit d'autre ne la
        // voie — voir "Phase 3 (préambule)" plus haut), déroulage des @for
        // ENSUITE (élimine $i / $(i) sans toucher aux $vars globales),
        // résolution du registre global EN DERNIER (voir "Phase 3
        // (suite)" plus haut).
        let stripped = strip_css_comments(&raw).map_err(MvarError::Comment)?;
        let unrolled = expand_for_loops(&stripped).map_err(MvarError::ForLoop)?;
        let transformed = substitute_and_purge(&unrolled, &self.registry, file)?;
        let ptr = Box::into_raw(Box::new(transformed));
        self.outputs.lock().unwrap().push(ptr);
        // SAFETY : le pointeur ne meurt qu'au `Drop` de `MvarProvider`, et
        // n'est jamais retiré du `Vec` avant — la référence rendue reste
        // valide aussi longtemps que `&'a self`.
        Ok(unsafe { &*ptr })
    }

    fn resolve(
        &self,
        specifier: &str,
        originating_file: &Path,
    ) -> Result<ResolveResult, Self::Error> {
        // Résolution de chemin identique à `FileProvider::resolve` — la
        // Phase 3 ne change pas la convention de résolution des imports,
        // seulement le contenu texte renvoyé pour chaque fichier.
        Ok(originating_file.with_file_name(specifier).into())
    }
}

impl Drop for MvarProvider {
    fn drop(&mut self) {
        for ptr in self.outputs.lock().unwrap().iter() {
            drop(unsafe { Box::from_raw(*ptr) });
        }
    }
}

/// Pipeline `[styles]` réel — spec §10.1, §10.3, Roadmap §1.8 (tranchée).
///
/// 1. Bundling (`Bundler` + `MvarProvider`) : résout et inline les
///    `@import`, y compris ceux qualifiés `layer(...)`. **Écart assumé par
///    rapport à la demande initiale** : je n'ai pas implémenté de logique
///    séparée pour "préserver" les imports en couche sans les inliner — un
///    `@import` non résolu resterait une requête réseau non hachée, non
///    présente dans le manifeste, ce qui romprait l'invariant de
///    versionnement de tout ce pipeline. `layer(...)` qualifie la couche
///    cible du contenu importé, pas une exemption d'inlining : un bundler
///    conforme à la spec CSS Cascade Layers inline le contenu et
///    l'enveloppe dans le `@layer` nommé, il ne le laisse pas non résolu.
///    Le `Bundler` standard fait déjà cela — aucun traitement spécial requis.
/// 2. Visiteur AST (`CssUrlVisitor` ci-dessus) : validation dure +
///    réécriture d'URL pour TOUT `url()` du document — plus seulement
///    `@font-face` (Roadmap §1.8, désormais tranchée par la demande de
///    session : `background-image` doit être réécrit exactement comme
///    `@font-face` l'était déjà).
/// 3. Minification, puis émission du CSS final.
///
/// **Pourquoi la réécriture d'URL se fait ICI, avant minification/hash, et
/// pas "en toute fin de build"** (question explicitement posée en
/// session) : le hash du fichier CSS produit (`run_styles_pipeline`, juste
/// après l'appel à cette fonction) doit refléter EXACTEMENT ce qui est
/// servi — invariant déjà en place pour `@font-face` avant cette session,
/// pas une nouveauté. Réécrire après coup (sur le fichier déjà écrit et
/// haché) obligerait soit à re-hacher après coup (passe supplémentaire,
/// aucun bénéfice sur la première option), soit à accepter un hash
/// obsolète (romprait l'invariant). Faire la réécriture ICI, avant
/// `minify`/`to_css`, ne coûte rien de plus qu'un second passage du même
/// visiteur déjà en place — la seule différence est la portée
/// (`in_font_face` retiré), pas le moment.
///
/// Pré-passe lexicale des commentaires, des variables `$` et des boucles
/// `@for` (Phase 3) : dans `MvarProvider::read`, `strip_css_comments`
/// d'abord (donnée morte éliminée avant tout le reste — un `$var`
/// indéfinie ou un `@for` malformé À L'INTÉRIEUR d'un commentaire ne doit
/// jamais faire échouer le build), puis `expand_for_loops` (élimine
/// `@for`, local à chaque fichier, pas de piège d'ordre), puis résolution
/// des `$nom` globaux via le `VariableRegistry` construit en amont par
/// `build_variable_registry` (walk textuel du graphe `@import`, AVANT que
/// `Bundler` ne lise quoi que ce soit — piège d'ordre inter-fichiers,
/// celui-là bien réel, évité par cette séparation ; cette Passe A
/// applique elle aussi `strip_css_comments` en premier, même raison). Voir
/// les blocs de commentaires "Phase 3" plus haut dans ce fichier pour le
/// raisonnement complet.
///
/// Note de version — confirmé par compilation réelle (retour de session,
/// `lightningcss = "=1.0.0-alpha.71"`) : `ParserOptions` se passe à
/// `Bundler::new()` (3 arguments), pas à `.bundle()` (1 seul argument, le
/// chemin). L'ancienne version de ce commentaire supposait l'inverse par
/// prudence documentaire, faute de pouvoir compiler dans cet
/// environnement — l'ambiguïté est levée, plus un avertissement.
fn transform_css(
    entry_path: &Path,
    asset_url_registry: &AssetUrlRegistry,
) -> Result<String, Box<dyn std::error::Error>> {
    // Passe A — walk textuel complet du graphe AVANT toute chose : le
    // registre doit être figé pour tout le graphe avant que `Bundler` ne
    // lise ne serait-ce que le fichier d'entrée (cf. commentaire Phase 3
    // ci-dessus — piège d'ordre si cette étape était fusionnée avec la
    // lecture individuelle de chaque fichier).
    let var_registry = build_variable_registry(entry_path)?;

    // Passe B — `Bundler` s'exécute normalement, mais chaque lecture de
    // fichier passe par `MvarProvider` : substitution + purge transparentes,
    // `Bundler` ne voit jamais un seul token `$`.
    let provider = MvarProvider::new(var_registry);
    let parser_options = ParserOptions::default();
    let mut bundler = Bundler::new(&provider, None, parser_options);
    let mut stylesheet = bundler.bundle(entry_path).map_err(|e| {
        format!(
            "styles : bundling échoué pour {} : {e:?}",
            entry_path.display()
        )
    })?;

    let mut visitor = CssUrlVisitor {
        registry: asset_url_registry,
    };
    stylesheet
        .visit(&mut visitor)
        .map_err(|e| format!("styles : {e}"))?;

    stylesheet.minify(MinifyOptions::default()).map_err(|e| {
        format!(
            "styles : minification échouée pour {} : {e:?}",
            entry_path.display()
        )
    })?;

    let result = stylesheet
        .to_css(PrinterOptions {
            minify: true,
            ..Default::default()
        })
        .map_err(|e| {
            format!(
                "styles : émission échouée pour {} : {e:?}",
                entry_path.display()
            )
        })?;

    Ok(result.code)
}

fn run_styles_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    entries: &[String],
    asset_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    for rel_path in entries {
        let source_path = theme_dir.join(rel_path);
        if !source_path.is_file() {
            return Err(format!("styles : fichier introuvable : {}", source_path.display()).into());
        }

        let transformed = transform_css(&source_path, asset_url_registry)?;
        let bytes = transformed.as_bytes();
        let (full_hash, short_hash) = hash_content(bytes);

        let rel = Path::new(rel_path);
        let stem = rel
            .file_stem()
            .ok_or_else(|| format!("styles : nom de fichier invalide : {rel_path}"))?
            .to_string_lossy();

        let logical_key = format!("{stem}.css");
        let hashed_filename = format!("{stem}.{short_hash}.css");
        let output_rel = join_slash("styles", &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, bytes)?;

        manifest.insert(
            logical_key,
            AssetEntry {
                url: format!("/{output_rel}"),
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension("css").to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(),
            },
        );

        println!("[marius-assets] styles    {rel_path} -> /{output_rel}");
    }

    Ok(())
}

// =============================================================================
// Utilitaires de chemin — slashes forcés, jamais de séparateur OS. Les URLs
// et les chemins écrits dans le manifeste doivent être stables quelle que
// soit la plateforme de build.
// =============================================================================

fn path_to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn join_slash(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

// =============================================================================
// Pipeline [scripts.components] — Phase 7. ES Modules natifs, pas de
// bundling par concaténation : chaque module source devient un fichier
// `.js` haché indépendant, les `import ... from '...'` sont réécrits pour
// pointer vers les URLs publiques des autres modules déjà hachés — le
// navigateur assemble le graphe lui-même via ESM natif au runtime, ce
// pipeline ne fait QUE renommer les chemins et garantir l'ordre de hachage
// (une dépendance doit être hachée avant le module qui l'importe, puisque
// son URL finale doit apparaître dans le texte patché de ce dernier).
//
// Aucune nouvelle dépendance Cargo : lexer fait main (octets), arène plate
// (Vec + indices), BLAKE3 déjà présent pour le hash — rien à ajouter au
// Cargo.toml pour cette Phase.
//
// Trois passes séparées, dans cet ordre strict, jamais fusionnées :
//   1. `build_module_arena`          — exploration (I/O + lex), construit
//                                       le graphe.
//   2. `topological_order_leaves_first` — tri topologique pur (aucune I/O,
//                                       aucune allocation de contenu),
//                                       détecte les cycles.
//   3. `patch_and_hash_modules`      — écrit sur disque, dans l'ordre
//                                       feuilles → racines imposé par (2).
// Même discipline que la séparation Passe A / Passe B déjà en place pour
// `[styles]` (Phase 3) : chaque passe a une seule responsabilité, aucune
// ne mélange "lire le graphe" et "l'ordonner" et "le matérialiser".
// =============================================================================

/// Position (octet, longueur) d'un chemin d'import littéral dans son texte
/// source — guillemets exclus. Alias nommé pour la lisibilité et pour
/// satisfaire `clippy::type_complexity` sur les signatures qui l'imbriquent
/// (`Option<(ImportSpan, usize)>`) ; la structure reste un simple tuple,
/// aucune sémantique supplémentaire par rapport à `(usize, usize)`.
type ImportSpan = (usize, usize);

#[derive(Debug)]
enum JsPipelineError {
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

fn skip_line_comment(source: &[u8], i: usize) -> usize {
    // S'arrête AU newline sans le consommer — le newline reste un
    // caractère significatif pour l'appelant (fin de déclaration `import`
    // sans `from`, cf. `lex_import_statement`).
    source[i..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|rel| i + rel)
        .unwrap_or(source.len())
}

fn skip_block_comment(source: &[u8], i: usize, ctx: &Path) -> Result<usize, JsPipelineError> {
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
fn find_unescaped_quote(
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
fn skip_string_like(
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
/// mot-clé — cherche `from '<chemin>'`/`from "<chemin>"`, bornée par `;`,
/// un saut de ligne, ou l'EOF. Retourne `((offset, len), position_après)`
/// du contenu du chemin (guillemets exclus), ou `None` si :
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

    while i < source.len() {
        match source[i] {
            b';' | b'\n' => return Ok(None),
            b'/' if source.get(i + 1) == Some(&b'/') => i = skip_line_comment(source, i),
            b'/' if source.get(i + 1) == Some(&b'*') => i = skip_block_comment(source, i, ctx)?,
            // Bug réel identifié à la relecture : un import SANS `from`
            // (`import './from-server.js';`) présente son chemin AVANT
            // toute occurrence légitime de 'from'. Sans cette ligne, le
            // mot "from" à l'intérieur même de ce chemin littéral serait
            // pris pour le vrai mot-clé, et la recherche de guillemet
            // qui suit repartirait d'un point arbitraire à l'intérieur de
            // la chaîne — extraction silencieusement fausse, pas une
            // erreur franche. Aucune grammaire JS valide ne place une
            // chaîne AVANT un `from` réel : sauter toute chaîne rencontrée
            // ici est donc sûr pour le cas légitime, et corrige le cas
            // illégitime.
            b'\'' | b'"' | b'`' => i = skip_string_like(source, i, source[i], ctx)?,
            _ if starts_with_word(source, i, b"from") => {
                let quote_pos = skip_ws_and_comments(source, i + "from".len(), ctx)?;
                let quote = match source.get(quote_pos) {
                    Some(&q @ (b'\'' | b'"')) => q,
                    _ => return Ok(None), // 'from' pas suivi d'une chaîne littérale
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
                match resolve_asset_reference(specifier, asset_url_registry) {
                    Ok(Some(resolved)) => ImportTarget::ExternalAsset(resolved),
                    Ok(None) => ImportTarget::ExternalAsset(specifier.to_string()),
                    Err(filename) => {
                        return Err(JsPipelineError::AssetNotFound {
                            specifier: specifier.to_string(),
                            filename,
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

        let bytes = patched.into_bytes();
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

fn run_scripts_pipeline(
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

    Ok(())
}

// =============================================================================
// main
// =============================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let theme_dir_arg = args
        .next()
        .ok_or("usage : marius-assets <chemin-du-dossier-de-theme> (ex: ./assets/default)")?;

    let theme_dir = PathBuf::from(&theme_dir_arg);
    if !theme_dir.is_dir() {
        return Err(format!(
            "dossier de thème introuvable ou invalide : {}",
            theme_dir.display()
        )
        .into());
    }

    let theme_toml_path = theme_dir.join("theme.toml");
    let raw_theme = fs::read_to_string(&theme_toml_path)
        .map_err(|e| format!("theme.toml introuvable dans {} : {e}", theme_dir.display()))?;
    let theme: ThemeConfig = toml::from_str(&raw_theme)
        .map_err(|e| format!("theme.toml malformé ({}) : {e}", theme_toml_path.display()))?;

    // Convention CWD-relative — même discipline que marius-dump/marius-verify
    // (guide-cycle-de-vie-runtime.md §4) : "build/" est résolu par rapport
    // au répertoire courant du processus au lancement, jamais via un
    // chemin absolu recalculé. Lancer ce binaire hors de la racine du
    // workspace produit un "build/" local, silencieusement — même piège,
    // même remède : toujours invoquer depuis la racine.
    let build_root_rel = join_slash("build", &theme.theme.name);
    let build_root = PathBuf::from(&build_root_rel);

    // Purge intégrale avant tout pipeline — Phase 5, en réponse à
    // l'accumulation de fichiers hachés observée en session
    // (`main.0e03e.css`, `main.42e0b.css`, ... jamais nettoyés d'un build
    // à l'autre). Arbitrage : nettoyage global ICI plutôt que trois
    // nettoyages partiels distincts (un par pipeline — styles/, sprites/,
    // chaque sous-dossier verbatim). `build_root` est un répertoire
    // ENTIÈREMENT généré (rien n'y est jamais écrit à la main — même
    // convention que l'en-tête `GÉNÉRÉ... NE PAS MODIFIER MANUELLEMENT`
    // déjà en usage ailleurs dans ce workspace) : le vider puis le
    // reconstruire de zéro à chaque invocation est donc strictement sûr,
    // et élimine toute la classe de bugs "un pipeline oublie de nettoyer
    // son propre sous-dossier" plutôt que de la déplacer vers trois
    // implémentations à maintenir en synchronisation. Pas besoin
    // d'atomicité (suppression puis recréation, pas de bascule
    // symlink) : ce binaire est séquentiel, personne ne lit `build_root`
    // pendant son exécution.
    if build_root.exists() {
        fs::remove_dir_all(&build_root)
            .map_err(|e| format!("nettoyage impossible de {} : {e}", build_root.display()))?;
    }
    fs::create_dir_all(&build_root)?;

    let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

    // Ordonnancement obligatoire (spec §10.1) : verbatim (résout le
    // registre d'URLs) AVANT styles (le consomme) — jamais l'inverse.
    let asset_url_registry = run_verbatim_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.static_.verbatim.files,
        &mut manifest,
    )?;

    // [webmanifest] (Phase 6) — dépend uniquement du registre d'URLs
    // (icons[].src pointe vers des favicons déjà hachés par verbatim ci-
    // dessus), aucune dépendance avec sprites/styles. `Option` : un thème
    // sans PWA (pas de section [webmanifest] dans theme.toml) est valide,
    // on saute silencieusement ce pipeline plutôt que de forcer sa
    // présence.
    if let Some(webmanifest_config) = &theme.webmanifest {
        run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            &build_root_rel,
            webmanifest_config,
            &asset_url_registry,
            &mut manifest,
        )?;
    }

    // [sprites] (Phase 4) — aucune dépendance avec verbatim/styles, ordre
    // libre. Placé ici à votre demande explicite, juste après verbatim.
    run_sprites_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.sprites,
        &mut manifest,
    )?;

    run_styles_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.styles.entries,
        &asset_url_registry,
        &mut manifest,
    )?;

    // [scripts.components] (Phase 7) — dépend uniquement du registre
    // d'URLs (les imports non-relatifs comme `/libs/leaflet.js` doivent
    // déjà être hachés par verbatim), aucune dépendance avec
    // sprites/styles/webmanifest. Placé en dernier des pipelines de
    // contenu par simple cohérence de lecture (ordre d'apparition dans
    // theme.toml), pas par nécessité d'ordonnancement.
    run_scripts_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.scripts.components,
        &asset_url_registry,
        &mut manifest,
    )?;

    // La version vient de [theme].version, identique pour toutes les
    // entrées de ce build — renseignée ici plutôt que dans chaque pipeline
    // pour ne l'écrire qu'à un seul endroit.
    for entry in manifest.values_mut() {
        entry.version = theme.theme.version.clone();
    }

    let output = AssetManifest { assets: manifest };
    let serialized = toml::to_string_pretty(&output)?;
    let manifest_path = build_root.join("manifest.toml");
    fs::write(&manifest_path, serialized)?;

    println!(
        "[marius-assets] manifeste écrit : {} ({} entrées)",
        manifest_path.display(),
        output.assets.len()
    );

    Ok(())
}

// =============================================================================
// Tests — documentent les intentions autant qu'ils protègent des
// régressions (voir échange de session : les trois familles de tests
// ci-dessous correspondent exactement aux trois erreurs vécues en
// intégration — variable en majuscule, boucle @for, purge fill/stroke —
// pas une couverture générique décidée après coup.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_css_comments (Phase 3, préambule) ─────────────────────────────

    #[test]
    fn strip_css_comments_removes_simple_comment() {
        let input = ".a { color: red; /* comment */ }";
        assert_eq!(strip_css_comments(input).unwrap(), ".a { color: red;  }");
    }

    #[test]
    fn strip_css_comments_multiline_comment_is_removed_entirely() {
        let input = "before\n/*\nmulti\nline\n*/\nafter";
        assert_eq!(strip_css_comments(input).unwrap(), "before\n\nafter");
    }

    /// Exemple exact de la mission : `/*` à l'intérieur d'une chaîne CSS
    /// n'est pas un délimiteur de commentaire, cette propriété ne doit
    /// jamais être altérée.
    #[test]
    fn strip_css_comments_preserves_slash_star_inside_double_quoted_string() {
        let input = r#".icon::before { content: "/*"; }"#;
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    #[test]
    fn strip_css_comments_preserves_slash_star_inside_single_quoted_string() {
        let input = ".icon::before { content: '/*'; }";
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    /// Un guillemet échappé à l'intérieur de la chaîne ne doit jamais être
    /// vu comme sa fermeture — sinon le `/*` qui suit serait interprété
    /// hors chaîne et supprimerait à tort le reste du fichier.
    #[test]
    fn strip_css_comments_escaped_quote_does_not_close_string_early() {
        let input = "content: \"a\\\" /* b\"; ";
        assert_eq!(strip_css_comments(input).unwrap(), input);
    }

    /// Le bug exact rapporté en session : une `$variable` indéfinie à
    /// l'intérieur d'un commentaire ne doit plus jamais atteindre
    /// `substitute_line` — la preuve la plus directe est qu'elle a
    /// entièrement disparu du texte après cette passe.
    #[test]
    fn strip_css_comments_hides_undefined_variable_usage_inside_comment() {
        let input = ".a { color: red; /* $old-var: 10; */ }";
        let stripped = strip_css_comments(input).unwrap();
        assert!(!stripped.contains('$'));
    }

    #[test]
    fn strip_css_comments_unterminated_comment_is_an_error() {
        assert!(strip_css_comments(".a { /* never closed").is_err());
    }

    #[test]
    fn strip_css_comments_unterminated_string_is_an_error() {
        assert!(strip_css_comments("content: \"never closed").is_err());
    }

    // ── suggest_variable / levenshtein (Phase 3, $variables) ────────────────

    fn registry_with(pairs: &[(&str, &str)]) -> VariableRegistry {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn levenshtein_identical_strings_is_zero() {
        assert_eq!(levenshtein("demoColorDeg", "demoColorDeg"), 0);
    }

    #[test]
    fn levenshtein_classic_kitten_sitting_is_three() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    /// Le cas réel qui a motivé cette fonctionnalité : une variable saisie
    /// avec une casse différente de sa déclaration.
    #[test]
    fn suggest_variable_case_mismatch_takes_priority_over_levenshtein() {
        let registry = registry_with(&[("demoColorDeg", "15")]);
        let suggestion = suggest_variable("democolordeg", &registry)
            .expect("une entrée ne différant que par la casse doit produire une suggestion");
        assert!(
            suggestion.contains("la casse est sensible"),
            "message inattendu : {suggestion:?}"
        );
        assert!(
            suggestion.contains("demoColorDeg"),
            "message inattendu : {suggestion:?}"
        );
    }

    #[test]
    fn suggest_variable_close_typo_suggests_nearest_key() {
        let registry = registry_with(&[("brandPrimary", "#ff0000")]);
        let suggestion = suggest_variable("brandPrimarry", &registry)
            .expect("distance 1 : une suggestion est attendue");
        assert_eq!(suggestion, "vouliez-vous dire $brandPrimary ?");
    }

    #[test]
    fn suggest_variable_no_close_match_returns_none() {
        let registry = registry_with(&[("brandPrimary", "#ff0000")]);
        assert_eq!(suggest_variable("totallyUnrelatedName", &registry), None);
    }

    #[test]
    fn suggest_variable_empty_registry_returns_none() {
        let registry = VariableRegistry::new();
        assert_eq!(suggest_variable("anything", &registry), None);
    }

    // ── expand_for_loops / substitute_loop_variable (Phase 3, @for) ─────────

    #[test]
    fn substitute_loop_variable_replaces_interpolated_form() {
        assert_eq!(substitute_loop_variable("<a>$(i)</a>", "i", 5), "<a>5</a>");
    }

    #[test]
    fn substitute_loop_variable_replaces_bare_form_at_word_boundary() {
        assert_eq!(substitute_loop_variable("v$i", "i", 5), "v5");
    }

    /// Propriété de non-préfixe : `$i` ne doit jamais matcher à l'intérieur
    /// de `$image` — sans cette frontière de mot stricte, toute variable
    /// dont le nom est un préfixe d'une autre serait corrompue.
    #[test]
    fn substitute_loop_variable_does_not_match_variable_name_as_prefix() {
        assert_eq!(substitute_loop_variable("$image", "i", 5), "$image");
    }

    #[test]
    fn substitute_loop_variable_leaves_other_variables_untouched() {
        assert_eq!(
            substitute_loop_variable("$(other) stays, $other too", "i", 5),
            "$(other) stays, $other too"
        );
    }

    #[test]
    fn substitute_loop_variable_lone_dollar_at_end_is_kept_as_is() {
        assert_eq!(substitute_loop_variable("trailing $", "i", 5), "trailing $");
    }

    #[test]
    fn expand_for_loops_default_step_unrolls_each_integer_exclusive_of_end() {
        // `to` exclusif (convention Sass) : from 1 to 3 → i = 1, 2 seulement.
        let out = expand_for_loops("@for $i from 1 to 3 {<a>$(i)</a>}").unwrap();
        assert_eq!(out, "<a>1</a><a>2</a>");
    }

    #[test]
    fn expand_for_loops_explicit_step_by_is_respected() {
        let out = expand_for_loops("@for $i from 10 to 40 by 10 {<r>$(i)</r>}").unwrap();
        assert_eq!(out, "<r>10</r><r>20</r><r>30</r>");
    }

    #[test]
    fn expand_for_loops_bare_form_inside_calc_is_substituted() {
        let out = expand_for_loops("@for $i from 1 to 3 {v$i}").unwrap();
        assert_eq!(out, "v1v2");
    }

    /// Le bug exact observé en session : un `$nom` global (pas la variable
    /// de boucle) présent dans le corps doit traverser le déroulage
    /// intact — sa résolution est la responsabilité de
    /// `substitute_and_purge`, pas d'`expand_for_loops`.
    #[test]
    fn expand_for_loops_leaves_global_variables_untouched_for_later_pass() {
        let out = expand_for_loops("@for $i from 1 to 2 {a$i b$other c}").unwrap();
        assert_eq!(out, "a1 b$other c");
    }

    /// Boucles imbriquées : l'intérieure doit être entièrement dépliée
    /// avant que l'extérieure ne duplique son corps — sans quoi le texte
    /// dupliqué contiendrait encore un `@for` littéral, jamais réexaminé.
    #[test]
    fn expand_for_loops_nested_loop_is_expanded_before_outer_duplication() {
        let out = expand_for_loops("@for $i from 1 to 2 {@for $j from 1 to 3 {<b>$(i)-$(j)</b>}}")
            .unwrap();
        assert_eq!(out, "<b>1-1</b><b>1-2</b>");
    }

    #[test]
    fn expand_for_loops_missing_to_keyword_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 through 3 {x}").is_err());
    }

    #[test]
    fn expand_for_loops_zero_step_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 to 10 by 0 {x}").is_err());
    }

    #[test]
    fn expand_for_loops_unclosed_brace_is_an_error() {
        assert!(expand_for_loops("@for $i from 1 to 3 {x").is_err());
    }

    #[test]
    fn expand_for_loops_text_without_any_loop_passes_through_unchanged() {
        assert_eq!(
            expand_for_loops(".foo { color: red; }").unwrap(),
            ".foo { color: red; }"
        );
    }

    // ── svg_file_to_symbol / serialize_start (Phase 4, [sprites]) ───────────

    /// Le comportement central de la mission : `fill`/`stroke` codés en dur
    /// sont purgés, la racine `<svg>` disparaît au profit de `<symbol>`.
    #[test]
    fn svg_file_to_symbol_purges_hardcoded_fill_and_wraps_in_symbol() {
        let src =
            r##"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0" fill="#ff0000"/></svg>"##;
        let out = svg_file_to_symbol("icon", src).unwrap();
        assert_eq!(out, r#"<symbol id="icon"><path d="M0 0" /></symbol>"#);
    }

    /// Exception explicite de la mission : `currentColor`/`none` doivent
    /// survivre intacts, ce ne sont pas des couleurs codées en dur.
    #[test]
    fn svg_file_to_symbol_keeps_current_color_and_none() {
        let src = r#"<svg><path fill="currentColor" stroke="none" d="M1 1"/></svg>"#;
        let out = svg_file_to_symbol("icon", src).unwrap();
        assert_eq!(
            out,
            r#"<symbol id="icon"><path fill="currentColor" stroke="none" d="M1 1" /></symbol>"#
        );
    }

    #[test]
    fn svg_file_to_symbol_handles_nested_non_empty_elements() {
        let src = r#"<svg><g><path d="M0 0"/></g></svg>"#;
        let out = svg_file_to_symbol("g1", src).unwrap();
        assert_eq!(out, r#"<symbol id="g1"><g><path d="M0 0" /></g></symbol>"#);
    }

    #[test]
    fn svg_file_to_symbol_ignores_decl_doctype_and_comments() {
        let src =
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg>\n<!-- c --><svg><rect width=\"1\"/></svg>";
        let out = svg_file_to_symbol("r", src).unwrap();
        assert_eq!(out, r#"<symbol id="r"><rect width="1" /></symbol>"#);
    }

    #[test]
    fn svg_file_to_symbol_empty_self_closing_root_yields_empty_symbol() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let out = svg_file_to_symbol("empty", src).unwrap();
        assert_eq!(out, r#"<symbol id="empty"></symbol>"#);
    }

    /// Fail-hard : un fichier sans balise racine `<svg>` doit échouer, pas
    /// produire un `<symbol>` vide silencieusement.
    #[test]
    fn svg_file_to_symbol_missing_svg_root_is_an_error() {
        let src = "<g><path/></g>";
        assert!(svg_file_to_symbol("icon", src).is_err());
    }

    // ── is_external_url (Phase 5, url() généralisée) ────────────────────────

    #[test]
    fn is_external_url_detects_absolute_schemes() {
        assert!(is_external_url("https://cdn.example.com/logo.svg"));
        assert!(is_external_url("http://cdn.example.com/logo.svg"));
    }

    #[test]
    fn is_external_url_detects_protocol_relative() {
        assert!(is_external_url("//cdn.example.com/logo.svg"));
    }

    #[test]
    fn is_external_url_detects_data_uri() {
        assert!(is_external_url("data:image/png;base64,AAAA"));
    }

    /// Le cas réel de la mission : un chemin relatif de thème n'est jamais
    /// externe, il doit passer par la résolution du registre.
    #[test]
    fn is_external_url_relative_theme_path_is_not_external() {
        assert!(!is_external_url("../images/logo.svg"));
        assert!(!is_external_url("logo.svg"));
    }

    /// Bug réel rencontré en session : `url(#mask-id)` (référence pure à un
    /// élément `<defs>` SVG inline, motif courant pour `mask`/`clip-path`/
    /// `filter`/`fill`) n'a aucun fichier à résoudre — sans cette exclusion,
    /// la généralisation de `url()` (Phase 5) faisait échouer le build.
    #[test]
    fn is_external_url_detects_pure_fragment_reference() {
        assert!(is_external_url("#mask-id"));
    }

    // ── split_url_fragment (Phase 5, url() avec fragment) ───────────────────

    #[test]
    fn split_url_fragment_no_fragment_returns_path_unchanged() {
        assert_eq!(
            split_url_fragment("sprites/utils.svg"),
            ("sprites/utils.svg", "")
        );
    }

    /// Le second bug réel rencontré en session : `url("sprites/utils.svg#icon")`
    /// référence UN symbole précis d'un sprite fusionné (Phase 4) — seul le
    /// chemin avant `#` doit être cherché dans le registre, le fragment doit
    /// être préservé tel quel pour être réattaché à l'URL résolue.
    #[test]
    fn split_url_fragment_splits_path_and_fragment() {
        assert_eq!(
            split_url_fragment("sprites/utils.svg#icon"),
            ("sprites/utils.svg", "#icon")
        );
    }

    #[test]
    fn split_url_fragment_empty_path_before_fragment() {
        assert_eq!(split_url_fragment("#icon"), ("", "#icon"));
    }

    // ── resolve_asset_reference (Phase 5/6, partagé CSS + webmanifest) ──────

    #[test]
    fn resolve_asset_reference_found_returns_resolved_url() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert("logo.svg".to_string(), "/images/logo.12452.svg".to_string());
        assert_eq!(
            resolve_asset_reference("../images/logo.svg", &registry),
            Ok(Some("/images/logo.12452.svg".to_string()))
        );
    }

    #[test]
    fn resolve_asset_reference_external_returns_ok_none() {
        let registry = AssetUrlRegistry::new();
        assert_eq!(
            resolve_asset_reference("https://cdn.example.com/icon.png", &registry),
            Ok(None)
        );
    }

    /// Le second bug de session, vu depuis l'API partagée : le fragment
    /// doit survivre à la résolution, pas seulement au niveau CSS.
    #[test]
    fn resolve_asset_reference_preserves_fragment_on_resolved_url() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            "utils.svg".to_string(),
            "/sprites/utils.4c4e9.svg".to_string(),
        );
        assert_eq!(
            resolve_asset_reference("sprites/utils.svg#icon", &registry),
            Ok(Some("/sprites/utils.4c4e9.svg#icon".to_string()))
        );
    }

    #[test]
    fn resolve_asset_reference_missing_key_returns_err_with_filename() {
        let registry = AssetUrlRegistry::new();
        assert_eq!(
            resolve_asset_reference("favicons/logo.svg", &registry),
            Err("logo.svg".to_string())
        );
    }

    // ── run_webmanifest_pipeline (Phase 6) ───────────────────────────────────
    //
    // Seuls tests de ce fichier à toucher le disque — nécessaire ici : la
    // propriété centrale à prouver (non-destruction du reste du document,
    // spec §3 de la mission) ne peut pas se vérifier sur une fonction pure,
    // `run_webmanifest_pipeline` lit et écrit réellement des fichiers.
    // Chaque test utilise un sous-répertoire de nom unique sous le
    // répertoire temporaire du système, pour rester sûr en cas
    // d'exécution parallèle des tests (le harnais `cargo test` parallélise
    // par défaut).

    #[test]
    fn run_webmanifest_pipeline_rewrites_icons_and_preserves_rest_of_document() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-webmanifest-ok");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("manifest.webmanifest"),
            r##"{
                "name": "Marius",
                "theme_color": "#ff6347",
                "icons": [
                    { "src": "/favicons/logoAny.svg", "sizes": "any", "type": "image/svg+xml" },
                    { "src": "/favicons/logo192.png", "sizes": "192x192", "type": "image/png" }
                ]
            }"##,
        )
        .unwrap();

        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            "logoAny.svg".to_string(),
            "/favicons/logoAny.12452.svg".to_string(),
        );
        registry.insert(
            "logo192.png".to_string(),
            "/favicons/logo192.53aea.png".to_string(),
        );

        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = WebManifestConfig {
            entry: "manifest.webmanifest".to_string(),
        };

        run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        )
        .unwrap();

        let entry = manifest
            .get("manifest.webmanifest")
            .expect("la clé logique manifest.webmanifest doit être enregistrée");

        let written_filename = Path::new(&entry.url).file_name().unwrap();
        let written = fs::read_to_string(build_root.join(written_filename)).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();

        // Non-destruction (spec §3 de la mission) : les clés hors icons[]
        // traversent intactes.
        assert_eq!(parsed["name"], "Marius");
        assert_eq!(parsed["theme_color"], "#ff6347");
        // Mutation ciblée : seul src est réécrit.
        assert_eq!(parsed["icons"][0]["src"], "/favicons/logoAny.12452.svg");
        assert_eq!(parsed["icons"][0]["sizes"], "any");
        assert_eq!(parsed["icons"][1]["src"], "/favicons/logo192.53aea.png");

        let _ = fs::remove_dir_all(&sandbox);
    }

    /// Fail-hard (spec §2 de la mission) : une icône absente du registre
    /// doit faire échouer tout le pipeline, jamais produire un manifeste
    /// avec une URL non versionnée ou une ressource orpheline.
    #[test]
    fn run_webmanifest_pipeline_fails_hard_on_missing_icon_asset() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-webmanifest-missing");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("manifest.webmanifest"),
            r#"{"icons": [{"src": "/favicons/ghost.png"}]}"#,
        )
        .unwrap();

        let registry = AssetUrlRegistry::new(); // vide : rien ne peut être trouvé
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = WebManifestConfig {
            entry: "manifest.webmanifest".to_string(),
        };

        let result = run_webmanifest_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        );
        assert!(result.is_err());
        assert!(
            manifest.get("manifest.webmanifest").is_none(),
            "aucune entrée ne doit être enregistrée si la résolution échoue"
        );

        let _ = fs::remove_dir_all(&sandbox);
    }

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
        let main_dir = theme_dir.join("scripts/development/main");
        let more_dir = theme_dir.join("scripts/development/more");
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
            "leaflet.js".to_string(),
            "/libs/leaflet.9f8e7.js".to_string(),
        );

        let mut components = HashMap::new();
        components.insert(
            "main".to_string(),
            "scripts/development/main/index.js".to_string(),
        );
        components.insert(
            "more".to_string(),
            "scripts/development/more/index.js".to_string(),
        );

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

        // Seules les cibles logiques entrent dans le manifeste — jamais
        // navigation.js, artefact de build intermédiaire.
        assert!(manifest.contains_key("main.js"));
        assert!(manifest.contains_key("more.js"));
        assert!(!manifest.contains_key("navigation.js"));

        let main_url = &manifest["main.js"].url;
        let main_filename = Path::new(main_url).file_name().unwrap();
        let main_written =
            fs::read_to_string(build_root.join("scripts").join(main_filename)).unwrap();

        // L'import relatif doit pointer vers l'URL hachée RÉELLE de
        // navigation.js — pas vers './navigation.js' ni vers un
        // placeholder : on retrouve son hash effectif en resolvant depuis
        // le même dossier scripts/ produit par CE run.
        assert!(!main_written.contains("./navigation.js"));
        assert!(main_written.contains("initNavigation();"));

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
        let dir = theme_dir.join("scripts/development/main");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(dir.join("index.js"), "import L from '/libs/leaflet.js';").unwrap();

        let registry = AssetUrlRegistry::new(); // vide : rien à trouver
        let mut components = HashMap::new();
        components.insert(
            "main".to_string(),
            "scripts/development/main/index.js".to_string(),
        );
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
}
