// =============================================================================
// crates/assets/src/main.rs
//
// marius-assets — compilateur AOT d'assets statiques du thème Marius.
// Outil de build hôte exclusivement (aucune trace runtime dans le Shell ni
// le Core no_std) — voir marius-assets-specification.md et
// marius-assets-HANDOFF.md pour le contexte complet.
//
// Étape 1 de la roadmap d'implémentation : pipelines [static.verbatim],
// [styles] (variables `$`, boucles `@for`, Fonts↔CSS) et [sprites]
// (Phase 4, cette session). [scripts.components] apparaît dans
// theme.toml mais n'est pas encore traité ici — serde l'ignore
// silencieusement, aucun champ ne le capture dans ThemeConfig.
//
// Invariant DOD respecté : traitement séquentiel, un seul passage par
// fichier, aucune structure de données hiérarchique, aucun trait dynamique.
// Ce n'est PAS le chemin chaud du Shell (§9 de la spec) : les allocations
// (String, Vec, HashMap) sont acceptées ici sans restriction — ce
// programme s'exécute une fois, sur la machine hôte, jamais par requête.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use lightningcss::bundler::{Bundler, ResolveResult, SourceProvider};
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions};
use lightningcss::values::url::Url;
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

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
    // [scripts.components] existe dans theme.toml, non traité cette
    // session : absent de cette struct, serde l'ignore sans erreur tant
    // qu'aucun #[serde(deny_unknown_fields)] n'est posé ici — délibéré.
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
// Registre des polices — spec §10.1 : deux exigences liées.
//  1. Le build CSS doit échouer si une police référencée en `@font-face`
//     est absente du registre effectivement copié par le pipeline verbatim.
//  2. Ce même registre sert de résolveur d'URL : le `url(...)` littéral
//     écrit par le développeur dans `@font-face` doit être réécrit vers
//     l'URL publique versionnée avant écriture du CSS final.
//
// Conséquence d'ordonnancement (spec, même §) : le pipeline verbatim doit
// avoir résolu ce registre AVANT que le pipeline styles ne s'exécute — d'où
// le passage explicite par valeur de retour, pas une variable globale ni un
// champ mutable partagé.
//
// Portée volontairement limitée aux polices (woff2/woff/ttf), pas à tout
// [static.verbatim] : le favicon n'a aucune raison d'être résolu par un
// `url(...)` CSS, l'inclure gonflerait le registre sans usage réel.
// =============================================================================
type FontRegistry = HashMap<String, String>;

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
) -> Result<FontRegistry, Box<dyn std::error::Error>> {
    let mut font_registry = FontRegistry::new();

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

        // Alimentation du registre Fonts — seulement les extensions de
        // police, pas tout [static.verbatim] (voir doc du type ci-dessus).
        if matches!(ext.as_str(), "woff2" | "woff" | "ttf") {
            font_registry.insert(logical_key.clone(), url.clone());
        }

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

    Ok(font_registry)
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

/// Erreur de résolution Fonts↔CSS (spec §10.1) — police référencée en
/// `@font-face` absente du registre. Échec dur volontaire : pas de valeur
/// par défaut, pas de passthrough silencieux vers une URL non versionnée.
#[derive(Debug)]
struct FontResolutionError(String);

impl fmt::Display for FontResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetNotFound (Fonts↔CSS, spec §10.1) : {}", self.0)
    }
}

impl std::error::Error for FontResolutionError {}

/// Visiteur AST — spec §10.1, scope strictement limité aux `url(...)`
/// rencontrés à l'intérieur d'une règle `@font-face`. `in_font_face` est le
/// mécanisme de scoping : `visit_url` est déclenché pour TOUT `url()` du
/// document (background-image compris), mais ne touche et ne valide que
/// ceux visités pendant que ce drapeau est vrai — cf. Roadmap §1.8, encore
/// ouverte pour les `url()` hors `@font-face` : ce visiteur ne tranche pas
/// cette question, il l'évite en ignorant tout le reste.
struct FontFaceUrlVisitor<'a> {
    font_registry: &'a FontRegistry,
    in_font_face: bool,
}

impl<'i> Visitor<'i> for FontFaceUrlVisitor<'_> {
    type Error = FontResolutionError;

    fn visit_types(&self) -> VisitTypes {
        // RULES : nécessaire pour que visit_rule() soit appelé (scoping
        // @font-face). URLS : nécessaire pour que visit_url() le soit.
        visit_types!(RULES | URLS)
    }

    fn visit_rule(&mut self, rule: &mut CssRule<'i>) -> Result<(), Self::Error> {
        let was_in_font_face = self.in_font_face;
        if matches!(rule, CssRule::FontFace(_)) {
            self.in_font_face = true;
        }
        let result = rule.visit_children(self);
        self.in_font_face = was_in_font_face;
        result
    }

    fn visit_url(&mut self, url: &mut Url<'i>) -> Result<(), Self::Error> {
        if !self.in_font_face {
            // Hors @font-face : hors périmètre v1 (Roadmap §1.8), url
            // laissée strictement inchangée.
            return Ok(());
        }

        let source = url.url.as_ref();
        let filename = Path::new(source)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.to_string());

        match self.font_registry.get(&filename) {
            Some(resolved_url) => {
                url.url = resolved_url.clone().into();
                Ok(())
            }
            None => Err(FontResolutionError(filename)),
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
    /// dur, même politique que `FontResolutionError` : pas de passthrough
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
        // Ordre impératif : déroulage des @for D'ABORD (élimine $i / $(i)
        // sans toucher aux $vars globales), résolution du registre global
        // ENSUITE (voir commentaire "Phase 3 (suite)" plus haut).
        let unrolled = expand_for_loops(&raw).map_err(MvarError::ForLoop)?;
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

/// Pipeline `[styles]` réel — spec §10.1 et §10.3.
///
/// 1. Bundling (`Bundler` + `FileProvider`) : résout et inline les
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
/// 2. Visiteur AST scopé `@font-face` (`FontFaceUrlVisitor` ci-dessus) :
///    validation dure + réécriture d'URL, spec §10.1.
/// 3. Minification, puis émission du CSS final.
///
/// Pré-passe lexicale des variables `$` et des boucles `@for` (Phase 3,
/// cette session) : dans `MvarProvider::read`, `expand_for_loops` d'abord
/// (élimine `@for`, local à chaque fichier, pas de piège d'ordre), puis
/// résolution des `$nom` globaux via le `VariableRegistry` construit en
/// amont par `build_variable_registry` (walk textuel du graphe `@import`,
/// AVANT que `Bundler` ne lise quoi que ce soit — piège d'ordre inter-
/// fichiers, celui-là bien réel, évité par cette séparation). Voir les
/// deux blocs de commentaires "Phase 3" plus haut dans ce fichier pour le
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
    font_registry: &FontRegistry,
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

    let mut visitor = FontFaceUrlVisitor {
        font_registry,
        in_font_face: false,
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
    font_registry: &FontRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    for rel_path in entries {
        let source_path = theme_dir.join(rel_path);
        if !source_path.is_file() {
            return Err(format!("styles : fichier introuvable : {}", source_path.display()).into());
        }

        let transformed = transform_css(&source_path, font_registry)?;
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
    fs::create_dir_all(&build_root)?;

    let mut manifest: HashMap<String, AssetEntry> = HashMap::new();

    // Ordonnancement obligatoire (spec §10.1) : verbatim (résout le
    // registre Fonts) AVANT styles (le consomme) — jamais l'inverse.
    let font_registry = run_verbatim_pipeline(
        &theme_dir,
        &build_root,
        &build_root_rel,
        &theme.static_.verbatim.files,
        &mut manifest,
    )?;

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
        &font_registry,
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
}
