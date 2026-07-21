// crates/assets/src/js_minify.rs
//
// Minification JS (oxc), partagée entre [scripts.components] et
// [service_worker]. Un seul point d'entrée, `minify_javascript`, plutôt
// que deux implémentations parallèles.

use std::fmt;
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, CommentOptions};
use oxc_minifier::{CompressOptions, MangleOptions, Minifier, MinifierOptions};
use oxc_parser::Parser as OxcParser;
use oxc_span::SourceType;

// =============================================================================
// Chantier 4 — minification JS (oxc), partagée entre [scripts.components]
// et [service_worker]. Un seul point d'entrée, `minify_javascript`, plutôt
// que deux implémentations parallèles : la minification n'a besoin
// d'aucune connaissance de QUEL pipeline l'appelle, seulement du texte
// déjà résolu (imports/chemins substitués en amont, en texte brut — cf.
// Handoff/session) et d'un `Path` indicatif pour détecter Script vs
// Module.
//
// AST instancié UNIQUEMENT ici, pour cette seule passe finale — jamais
// pour la résolution de chemins (`lex_imports`/`scan_and_resolve_service_
// worker` restent des scanners plats sur `&[u8]`, aucune régression vers
// un AST pour cette tâche-là : elle n'en a jamais eu besoin, un AST y
// serait une dépense pure sans bénéfice).
// =============================================================================

#[derive(Debug)]
pub(crate) enum MinifyError {
    /// Erreur de parsing — le buffer déjà résolu par ce pipeline (imports/
    /// chemins substitués) doit rester du JavaScript syntaxiquement
    /// valide ; si `oxc_parser` le rejette, c'est le signe d'un bug en
    /// amont (substitution ayant cassé la syntaxe), jamais une tolérance
    /// à absorber silencieusement.
    Parse(PathBuf, String),
}

impl fmt::Display for MinifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MinifyError::Parse(path, msg) => {
                write!(f, "minification : {} : {msg}", path.display())
            }
        }
    }
}

impl std::error::Error for MinifyError {}

/// Parse -> minifie (compresse + mangle) -> imprime. `path_hint` sert
/// UNIQUEMENT à détecter Script vs Module (extension `.js`, détection
/// `Unambiguous` par contenu — vérifié dans le code source d'`oxc_span`
/// le jour de cette session, pas supposé : `import`/`export` détectés ⟹
/// Module, sinon Script), jamais à relire le fichier depuis le disque —
/// le buffer déjà en mémoire (`source`) est la seule source de vérité ici.
///
/// `MangleOptions::default()` laisse `top_level` à `None`, qui se résout
/// alors selon LE TYPE DE SOURCE détecté : `true` pour un Module, `false`
/// pour un Script classique (vérifié dans `oxc_mangler`, pas supposé) —
/// c'est exactement le comportement voulu SANS aucune branche
/// pipeline-spécifique : les modules `[scripts.components]` (ESM natif,
/// import/export réels) gardent leurs noms EXPORTÉS intacts (résolution
/// cross-fichier à l'exécution, chaque fichier miniifié indépendamment
/// des autres) tandis que leurs bindings locaux sont mangled ; le
/// Service Worker (aucun `import`/`export`, un script isolé, jamais
/// référencé par nom depuis un autre fichier) n'a par défaut que ses
/// bindings de fonction mangled, pas ses `const` de premier niveau — un
/// choix conservateur délibéré, pas une limite technique : rien n'empêche
/// de forcer `top_level: Some(true)` pour ce seul fichier si l'octet
/// supplémentaire économisé importe un jour.
pub(crate) fn minify_javascript(source: &str, path_hint: &Path) -> Result<String, MinifyError> {
    let source_type = SourceType::from_path(path_hint).expect(
        "tous les points d'entrée de ce binaire sont suffixés .js par construction du thème",
    );

    let allocator = Allocator::default();
    let parser_ret = OxcParser::new(&allocator, source, source_type).parse();

    if parser_ret.diagnostics.has_errors() {
        let messages: Vec<String> = parser_ret
            .diagnostics
            .errors()
            .map(std::string::ToString::to_string)
            .collect();
        return Err(MinifyError::Parse(
            path_hint.to_path_buf(),
            messages.join("; "),
        ));
    }

    let mut program = parser_ret.program;

    // `CompressOptions::smallest()` : preset officiel du crate, `drop_
    // console: false` par défaut — les `console.error(...)` de diagnostic
    // du Service Worker (et de tout module applicatif) survivent
    // volontairement, utiles en inspection navigateur sur un incident réel.
    let options = MinifierOptions {
        mangle: Some(MangleOptions::default()),
        compress: Some(CompressOptions::smallest()),
    };
    let minifier_ret = Minifier::new(options).minify(&allocator, &mut program);

    let codegen_ret = Codegen::new()
        .with_options(CodegenOptions {
            minify: true,
            comments: CommentOptions::disabled(),
            ..CodegenOptions::default()
        })
        .with_scoping(minifier_ret.scoping)
        .build(&program);

    Ok(codegen_ret.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── minify_javascript (Chantier 4, oxc) ─────────────────────────────

    /// Suppression des commentaires et des espaces superflus — le
    /// comportement de base attendu d'une minification.
    #[test]
    fn minify_javascript_strips_comments_and_whitespace() {
        let src = "// commentaire\nconst   x   =   1 + 1;\nconsole.log(x);\n";
        let out = minify_javascript(src, Path::new("test.js")).unwrap();
        assert!(!out.contains("commentaire"));
        assert!(!out.contains('\n'));
    }

    /// Le contenu d'un littéral de chaîne (une URL hachée, typiquement)
    /// traverse la minification intact — seuls les commentaires/espaces/
    /// noms de bindings locaux sont affectés, jamais le contenu d'une
    /// chaîne.
    #[test]
    fn minify_javascript_preserves_string_literal_content() {
        let src = "const u = '/scripts/main.a1b2c3.js';\nconsole.log(u);";
        let out = minify_javascript(src, Path::new("test.js")).unwrap();
        assert!(out.contains("/scripts/main.a1b2c3.js"));
    }

    /// Un nom EXPORTÉ (Module ESM réel, `import`/`export` présents) doit
    /// survivre à la passe de mangling — c'est la garantie centrale sans
    /// laquelle [scripts.components] casserait la résolution ESM
    /// inter-fichiers (cf. commentaire de `minify_javascript`).
    #[test]
    fn minify_javascript_keeps_exported_name_in_module_source() {
        let src = "export const initNavigation = () => { console.log('Nav'); };";
        let out = minify_javascript(src, Path::new("navigation.js")).unwrap();
        assert!(out.contains("initNavigation"));
    }

    /// Fail-hard : un buffer syntaxiquement invalide en amont (bug de
    /// substitution, pas une tolérance à absorber) doit faire échouer la
    /// minification, pas produire un artefact tronqué silencieusement.
    #[test]
    fn minify_javascript_fails_hard_on_invalid_syntax() {
        let src = "const x = ;";
        let result = minify_javascript(src, Path::new("broken.js"));
        assert!(result.is_err());
    }

    /// Une source sans `import`/`export` (cas du Service Worker réel,
    /// script isolé) doit être détectée comme Script, pas Module, et se
    /// minifier sans erreur.
    #[test]
    fn minify_javascript_handles_plain_script_source() {
        let src =
            "const CACHE_NAME = 'MARIUS_CACHE_HASH';\nself.addEventListener('install', () => {});";
        let out = minify_javascript(src, Path::new("serviceWorker.js")).unwrap();
        assert!(out.contains("MARIUS_CACHE_HASH"));
    }
}
