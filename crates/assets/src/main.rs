// =============================================================================
// crates/assets/src/main.rs
//
// marius-assets — compilateur AOT d'assets statiques du thème Marius.
// Outil de build hôte exclusivement (aucune trace runtime dans le Shell ni
// le Core no_std) — voir marius-assets-specification.md et
// marius-assets-HANDOFF.md pour le contexte complet.
//
// Étape 1 de la roadmap d'implémentation : pipelines [static.verbatim] et
// [styles] uniquement. [scripts.components] et [sprites] apparaissent dans
// theme.toml mais ne sont pas encore traités ici (Phase 2) — serde les
// ignore silencieusement, aucun champ ne les capture dans ThemeConfig.
//
// Invariant DOD respecté : traitement séquentiel, un seul passage par
// fichier, aucune structure de données hiérarchique, aucun trait dynamique.
// Ce n'est PAS le chemin chaud du Shell (§9 de la spec) : les allocations
// (String, Vec, HashMap) sont acceptées ici sans restriction — ce
// programme s'exécute une fois, sur la machine hôte, jamais par requête.
// =============================================================================

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use lightningcss::bundler::{Bundler, FileProvider};
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions};
use lightningcss::values::url::Url;
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

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
    // [scripts.components] et [sprites] existent dans theme.toml (Phase 2,
    // non traités) : absents de cette struct, serde les ignore sans erreur
    // tant qu'aucun #[serde(deny_unknown_fields)] n'est posé ici — délibéré.
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
/// Pré-passe lexicale des variables `$` : non implémentée, confirmé absent
/// du CSS de test par l'auteur du projet — `lightningcss` échouera sur un
/// token `$variable` si un tel fichier apparaît avant qu'un lexer dédié ne
/// soit écrit (hors périmètre de cette session).
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
    let provider = FileProvider::new();
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
