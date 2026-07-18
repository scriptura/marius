// crates/assets/src/service_worker.rs
//
// Pipeline [service_worker] — Handoff §3. `serviceWorker.js` est traité
// comme un gabarit textuel déterministe, pas un AST. Réutilise le lexer
// bas niveau de `crate::scripts` (`skip_line_comment`, `skip_block_comment`,
// `find_unescaped_quote`) sans dupliquer sa logique — cf. commentaire de
// `scripts.rs`.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ServiceWorkerConfig;
use crate::manifest::{AssetEntry, AssetUrlRegistry, hash_content, join_slash, mime_for_extension};
use crate::js_minify::minify_javascript;
use crate::resolve::resolve_asset_reference;
use crate::scripts::{JsPipelineError, find_unescaped_quote, skip_block_comment, skip_line_comment};

// =============================================================================
// Pipeline [service_worker] — Handoff §3. `serviceWorker.js` est traité
// comme un gabarit textuel déterministe, pas un AST : chaque littéral de
// chaîne du fichier est scanné, et réécrit en place s'il ressemble à un
// chemin d'asset local — exactement le modèle d'auteurship déjà en place
// pour `{% asset %}`/`url()`/`icons[].src` ("je liste mes ressources
// moi-même, l'outil réécrit chaque chemin"), jamais une régénération du
// tableau depuis le manifeste.
//
// Seul pipeline de ce binaire à dépendre du MANIFESTE COMPLET (toutes les
// clés, tous pipelines confondus), pas seulement d'`AssetUrlRegistry`
// (peuplé exclusivement par [static.verbatim], structurellement plus
// étroit) — d'où son câblage en tout dernier dans `main()` (§3.4). Une
// vue `AssetUrlRegistry` est dérivée du manifeste juste avant l'appel,
// pour réutiliser `resolve_asset_reference` sans le modifier.
// =============================================================================

#[derive(Debug)]
pub(crate) enum ServiceWorkerError {
    Io(PathBuf, std::io::Error),
    Lex(PathBuf, String),
    /// Même politique fail-hard que CSS/webmanifest/scripts : une chaîne
    /// qui commence par `/`, n'est ni `/` ni terminée par `.html`, mais
    /// absente du registre, est une erreur fatale — jamais une exception
    /// silencieuse propre à ce seul pipeline (Handoff §3.2, proposition de
    /// pass-through généralisé explicitement rejetée).
    AssetNotFound {
        specifier: String,
        filename: String,
        in_file: PathBuf,
    },
}

impl fmt::Display for ServiceWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceWorkerError::Io(path, e) => {
                write!(
                    f,
                    "service_worker : lecture impossible de {} : {e}",
                    path.display()
                )
            }
            ServiceWorkerError::Lex(path, msg) => {
                write!(f, "service_worker : {} : {msg}", path.display())
            }
            ServiceWorkerError::AssetNotFound {
                specifier,
                filename,
                in_file,
            } => write!(
                f,
                "service_worker : AssetNotFound '{specifier}' (fichier '{filename}' absent du \
                 registre) référencé dans {}",
                in_file.display()
            ),
        }
    }
}

impl std::error::Error for ServiceWorkerError {}

/// Conversion depuis `JsPipelineError` — seules `skip_block_comment` et
/// `find_unescaped_quote` (réutilisées ci-dessous, cf. `[scripts.components]`)
/// peuvent produire une erreur ici, et seulement la variante `Lex` (chaîne
/// ou commentaire bloc non fermé) : ni `AssetNotFound` ni `CyclicImport`
/// n'appartiennent à ce pipeline (résolution d'imports / tri topologique,
/// tous deux hors sujet pour un scan linéaire de littéraux). Le bras `other`
/// ne devrait donc jamais s'exécuter — message explicite plutôt qu'un
/// panic aveugle si cet invariant venait à changer un jour.
impl From<JsPipelineError> for ServiceWorkerError {
    fn from(e: JsPipelineError) -> Self {
        match e {
            JsPipelineError::Lex(path, msg) => ServiceWorkerError::Lex(path, msg),
            JsPipelineError::Io(path, io_err) => ServiceWorkerError::Io(path, io_err),
            other => ServiceWorkerError::Lex(
                PathBuf::new(),
                format!(
                    "erreur inattendue réutilisée depuis le lexer [scripts.components] : {other}"
                ),
            ),
        }
    }
}

/// Applique la règle de résolution à 3 niveaux (Handoff §3.2) à UN
/// littéral déjà isolé par le scanner (guillemets déjà exclus par
/// l'appelant).
///
/// Niveau 1 (filtrage a priori, zéro bruit) : toute chaîne ne commençant
/// pas par `/` n'est même pas candidate — `'style'`, `'GET'`, le jeton
/// `MARIUS_CACHE_HASH` lui-même, tous exclus ici, avant toute tentative.
/// Niveau 2 (liste blanche restreinte) : `/` seul (racine du document) et
/// tout ce qui se termine par `.html` — aucun pipeline de ce projet ne
/// hache de page HTML, cf. Handoff §3.2 (point de vigilance explicite pour
/// l'avenir, pas une objection actuelle).
/// Niveau 3 (résolution stricte) : tout le reste passe par
/// `resolve_asset_reference`, échec dur sans exception.
fn resolve_service_worker_literal(
    literal: &str,
    registry: &AssetUrlRegistry,
    ctx: &Path,
) -> Result<String, ServiceWorkerError> {
    if !literal.starts_with('/') {
        return Ok(literal.to_string());
    }
    if literal == "/" || literal.ends_with(".html") {
        return Ok(literal.to_string());
    }
    match resolve_asset_reference(literal, registry) {
        Ok(Some(resolved)) => Ok(resolved),
        // Structurellement improbable ici (Niveau 1 exige déjà un `/` en
        // tête, alors que `is_external_url` reconnaît `#`/`//`/`data:`/
        // `://`) — mais `resolve_asset_reference` reste appelé sans le
        // contourner, pour ne jamais diverger silencieusement du
        // comportement déjà éprouvé par [styles]/[scripts.components].
        Ok(None) => Ok(literal.to_string()),
        Err(filename) => Err(ServiceWorkerError::AssetNotFound {
            specifier: literal.to_string(),
            filename,
            in_file: ctx.to_path_buf(),
        }),
    }
}

/// Scan à plat du buffer entier — aucune notion de déclaration `import`
/// (contrairement à `lex_import_statement`), juste une alternance stricte
/// commentaire / littéral / reste, dans cet ordre de priorité À CHAQUE
/// position : c'est cet ordre qui protège les apostrophes des élisions
/// françaises à l'intérieur des blocs `/** JSDoc */` (nombreuses dans ce
/// fichier réel) contre une interprétation erronée comme guillemet
/// ouvrant — sans ce garde-fou, un scanner naïf désynchroniserait tout le
/// reste du fichier dès le premier commentaire un peu long.
///
/// Un littéral gabarit (`` ` `` ) est traité comme une région opaque, même
/// limite déjà documentée pour `skip_string_like` : une interpolation
/// `${...}` n'est jamais un chemin d'asset valide dans ce projet, la
/// laisser intacte est sûr, pas une approximation risquée.
fn scan_and_resolve_service_worker(
    source: &str,
    registry: &AssetUrlRegistry,
    ctx: &Path,
) -> Result<String, ServiceWorkerError> {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = skip_line_comment(bytes, i);
                out.push_str(&source[i..end]);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = skip_block_comment(bytes, i, ctx)?;
                out.push_str(&source[i..end]);
                i = end;
            }
            b'\'' | b'"' | b'`' => {
                let quote = bytes[i];
                let close = find_unescaped_quote(bytes, i + 1, quote, ctx)?;
                let literal = &source[i + 1..close];
                out.push(quote as char);
                out.push_str(&resolve_service_worker_literal(literal, registry, ctx)?);
                out.push(quote as char);
                i = close + 1;
            }
            _ => {
                // Avance d'un caractère UTF-8 complet, pas d'un octet — même
                // discipline que `substitute_line` : un caractère multioctet
                // (accents français, hors des zones déjà traitées ci-dessus)
                // ne doit jamais être coupé en deux.
                let ch_len = source[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
                out.push_str(&source[i..i + ch_len]);
                i += ch_len;
            }
        }
    }

    Ok(out)
}

/// Pipeline `[service_worker]` réel — Handoff §3, bootstrap du hash en 2
/// passes (§3.3), méthode actée sans réserve :
///
///  1. Résolution des chemins d'assets (Niveaux 1-3 ci-dessus) — le jeton
///     `MARIUS_CACHE_HASH` n'est JAMAIS touché à cette passe : il ne
///     commence pas par `/`, le Niveau 1 l'exclut déjà naturellement,
///     aucun traitement spécial requis. Hash de CE buffer intermédiaire :
///     cette valeur devient `CACHE_NAME` au runtime — elle change si et
///     seulement si un asset référencé a changé.
///  2. Substitution exacte du jeton sentinelle par ce hash — un
///     remplacement de chaîne ciblé, pas une nouvelle passe de résolution
///     d'assets (le hash n'est structurellement pas un nom de fichier du
///     registre). Hash COMPLET (64 caractères hex), pas le suffixe court :
///     `CACHE_NAME` est une clé `caches.open()` arbitraire au runtime,
///     sans contrainte de longueur de nom de fichier — le hash complet
///     porte plus d'entropie pour ce rôle d'identité de contenu, le
///     suffixe court reste réservé à son usage établi (nom de fichier).
///  3. Hash du buffer FINAL, réellement écrit sur disque — nomme le
///     fichier produit, même convention que tous les autres pipelines.
pub(crate) fn run_service_worker_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    config: &ServiceWorkerConfig,
    manifest_url_registry: &AssetUrlRegistry,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_path = theme_dir.join(&config.entry);
    let raw = fs::read_to_string(&source_path).map_err(|e| {
        format!(
            "service_worker : lecture impossible de {} : {e}",
            source_path.display()
        )
    })?;

    let pass1_resolved = scan_and_resolve_service_worker(&raw, manifest_url_registry, &source_path)?;

    // Chantier 4 — minification, APRÈS la résolution textuelle des
    // chemins mais AVANT le bootstrap du hash en 2 passes ci-dessous : le
    // hash de `CACHE_NAME` doit porter sur les octets réellement servis
    // (minifiés), pas sur un brouillon plus verbeux qui ne sera jamais
    // écrit sur disque — même principe que pour [scripts.components].
    // Le jeton `MARIUS_CACHE_HASH` traverse cette passe intact : c'est un
    // littéral de chaîne, jamais altéré par un minifieur correct (au pire
    // dupliqué si le `const CACHE_NAME` est inliné à ses points d'usage —
    // sans conséquence ici, la substitution globale ci-dessous reste
    // correcte quel que soit le nombre d'occurrences).
    let pass1 = minify_javascript(&pass1_resolved, &source_path)
        .map_err(|e| format!("service_worker : {e}"))?;

    let (cache_hash_full, _) = hash_content(pass1.as_bytes());
    let final_content = pass1.replace("MARIUS_CACHE_HASH", &cache_hash_full);

    let bytes = final_content.as_bytes();
    let (full_hash, short_hash) = hash_content(bytes);

    let hashed_filename = format!("serviceWorker.{short_hash}.js");
    let output_abs = build_root.join(&hashed_filename);
    fs::write(&output_abs, bytes)?;

    // Clé logique fixe, comme `manifest.webmanifest` — un seul Service
    // Worker par thème, indépendant du nom de fichier source réel.
    manifest.insert(
        "serviceWorker.js".to_string(),
        AssetEntry {
            url: format!("/{hashed_filename}"),
            path: join_slash(build_root_rel, &hashed_filename),
            mime: mime_for_extension("js").to_string(),
            size: bytes.len() as u64,
            hash: full_hash,
            // Ce pipeline tourne APRÈS la boucle de version dans `main()`
            // (§3.4) : la version doit lui être affectée explicitement par
            // l'appelant juste après cet appel, pas ici.
            version: String::new(),
        },
    );

    println!(
        "[marius-assets] service_worker {} -> /{hashed_filename}",
        config.entry
    );

    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_service_worker_literal / scan_and_resolve_service_worker
    // ── (Handoff §3, [service_worker]) ───────────────────────────────────

    /// Niveau 1 : ne commence pas par `/` — jamais candidat, quelle que
    /// soit sa forme (y compris le jeton sentinelle lui-même).
    #[test]
    fn resolve_service_worker_literal_ignores_non_slash_strings() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        assert_eq!(
            resolve_service_worker_literal("style", &registry, ctx).unwrap(),
            "style"
        );
        assert_eq!(
            resolve_service_worker_literal("MARIUS_CACHE_HASH", &registry, ctx).unwrap(),
            "MARIUS_CACHE_HASH"
        );
    }

    /// Niveau 2 : racine du document et routes `.html` — exemptées, jamais
    /// soumises à `resolve_asset_reference`, même avec un registre vide.
    #[test]
    fn resolve_service_worker_literal_exempts_root_and_html_routes() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        assert_eq!(resolve_service_worker_literal("/", &registry, ctx).unwrap(), "/");
        assert_eq!(
            resolve_service_worker_literal("/offline.html", &registry, ctx).unwrap(),
            "/offline.html"
        );
    }

    /// Niveau 3 : résolution stricte — trouvé, réécrit vers l'URL du
    /// registre.
    #[test]
    fn resolve_service_worker_literal_resolves_known_asset() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert("main.css".to_string(), "/styles/main.a1b2c.css".to_string());
        let ctx = Path::new("sw.js");
        assert_eq!(
            resolve_service_worker_literal("/styles/main.css", &registry, ctx).unwrap(),
            "/styles/main.a1b2c.css"
        );
    }

    /// Niveau 3 : échec dur, même politique que CSS/webmanifest/scripts —
    /// aucune tolérance propre à ce seul pipeline.
    #[test]
    fn resolve_service_worker_literal_fails_hard_on_unknown_asset() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        let err = resolve_service_worker_literal("/scripts/index.js", &registry, ctx).unwrap_err();
        match err {
            ServiceWorkerError::AssetNotFound { specifier, filename, .. } => {
                assert_eq!(specifier, "/scripts/index.js");
                assert_eq!(filename, "index.js");
            }
            other => panic!("attendu AssetNotFound, obtenu {other:?}"),
        }
    }

    /// Le bug exact que ce scanner doit éviter : une apostrophe française
    /// à l'intérieur d'un commentaire bloc ne doit jamais être vue comme un
    /// guillemet ouvrant — sinon tout le reste du fichier serait
    /// désynchronisé. Reproduction directe du motif réel de
    /// `serviceWorker.js` (élision dans un bloc `/** ... */`).
    #[test]
    fn scan_and_resolve_service_worker_apostrophe_inside_block_comment_is_not_a_quote() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        let src = "/** L'API Cache Storage n'a qu'un seul rôle */\nconst x = '/offline.html';";
        let out = scan_and_resolve_service_worker(src, &registry, ctx).unwrap();
        assert_eq!(out, src); // rien à résoudre ici, mais surtout : pas d'erreur de lex.
    }

    /// Même garde-fou pour un commentaire de ligne `//`.
    #[test]
    fn scan_and_resolve_service_worker_apostrophe_inside_line_comment_is_not_a_quote() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        let src = "// évite l'interception des fragments 206\nconst y = '/';";
        let out = scan_and_resolve_service_worker(src, &registry, ctx).unwrap();
        assert_eq!(out, src);
    }

    /// Un littéral gabarit (backtick) avec interpolation est traité comme
    /// une région opaque — jamais soumis à la résolution, contenu intact.
    #[test]
    fn scan_and_resolve_service_worker_template_literal_is_opaque() {
        let registry = AssetUrlRegistry::new();
        let ctx = Path::new("sw.js");
        let src = "const m = `media-${CACHE_NAME}`;";
        let out = scan_and_resolve_service_worker(src, &registry, ctx).unwrap();
        assert_eq!(out, src);
    }

    /// Un littéral d'asset réel, entouré de bruit non pertinent
    /// (mot-clé `'GET'`), doit être réécrit en place, le reste du fichier
    /// intact caractère pour caractère.
    #[test]
    fn scan_and_resolve_service_worker_rewrites_asset_path_in_place() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            "utils.svg".to_string(),
            "/sprites/utils.4c4e9.svg".to_string(),
        );
        let ctx = Path::new("sw.js");
        let src = "if (m !== 'GET') return '/sprites/utils.svg';";
        let out = scan_and_resolve_service_worker(src, &registry, ctx).unwrap();
        assert_eq!(
            out,
            "if (m !== 'GET') return '/sprites/utils.4c4e9.svg';"
        );
    }

    /// Trouve la première sous-chaîne de 64 caractères hexadécimaux dans
    /// un texte — la représentation littérale d'un hash BLAKE3 complet,
    /// quel que soit le guillemet qui l'entoure. S'assure de ne pas
    /// capturer un fragment d'un hexadécimal plus long en vérifiant que le
    /// caractère suivant n'en est pas un lui-même.
    fn find_hex64(text: &str) -> Option<&str> {
        let bytes = text.as_bytes();
        if bytes.len() < 64 {
            return None;
        }
        for start in 0..=bytes.len() - 64 {
            let end = start + 64;
            let candidate = &text[start..end];
            let next_is_hex = bytes.get(end).is_some_and(u8::is_ascii_hexdigit);
            if !next_is_hex && candidate.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(candidate);
            }
        }
        None
    }

    // ── run_service_worker_pipeline (intégration, Handoff §3.3/§3.4) ────────

    /// Bootstrap du hash en 2 passes de bout en bout : le jeton sentinelle
    /// disparaît totalement du fichier écrit sur disque, remplacé par un
    /// hash de 64 caractères hex ; le nom de fichier produit reflète un
    /// SECOND hash, du contenu final — les deux hashs doivent différer
    /// (l'un précède l'injection de l'autre dans le buffer).
    #[test]
    fn run_service_worker_pipeline_two_pass_hash_bootstrap() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-sw-bootstrap");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("serviceWorker.js"),
            "const CACHE_NAME = \"MARIUS_CACHE_HASH\";\nconst r = ['/styles/main.css'];",
        )
        .unwrap();

        let mut registry = AssetUrlRegistry::new();
        registry.insert("main.css".to_string(), "/styles/main.a1b2c.css".to_string());
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = ServiceWorkerConfig {
            entry: "serviceWorker.js".to_string(),
        };

        run_service_worker_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        )
        .unwrap();

        let entry = manifest.get("serviceWorker.js").expect("entrée attendue");
        assert!(!entry.url.contains("MARIUS_CACHE_HASH"));
        assert!(entry.url.starts_with("/serviceWorker."));
        assert!(entry.url.ends_with(".js"));

        let written =
            fs::read_to_string(build_root.join(entry.url.trim_start_matches('/'))).unwrap();

        assert!(!written.contains("MARIUS_CACHE_HASH"));
        assert!(written.contains("/styles/main.a1b2c.css"));

        // Le hash CACHE_NAME injecté dans le contenu est le hash COMPLET
        // (64 hex) du buffer intermédiaire (Passe 1) ; le hash du manifeste
        // (`entry.hash`, également complet) est celui du buffer FINAL
        // (Passe 2, après injection) — les deux hashent des contenus
        // différents, ils doivent donc différer.
        //
        // Extraction SANS dépendre du caractère de guillemet : en mode
        // `minify: true`, `oxc_codegen` choisit dynamiquement guillemet
        // simple/double selon la sortie la plus courte pour CHAQUE chaîne
        // (vérifié dans `oxc_codegen/src/str.rs` — le champ `single_quote`
        // n'est consulté que hors mode minifié). Un guillemet double fixe
        // n'est donc jamais une hypothèse sûre ici.
        let injected_hash =
            find_hex64(&written).expect("hash complet (64 hex) attendu dans le contenu écrit");
        assert_eq!(
            injected_hash.len(),
            64,
            "hash complet attendu, pas le suffixe court"
        );
        assert_ne!(
            injected_hash, entry.hash,
            "Passe 1 (avant injection) et Passe 2 (après) doivent produire des hashs distincts"
        );

        let _ = fs::remove_dir_all(&sandbox);
    }

    /// Fail-hard : un chemin ressemblant à un asset mais absent du
    /// registre dérivé du manifeste bloque tout le pipeline, aucune
    /// exception propre au Service Worker.
    #[test]
    fn run_service_worker_pipeline_fails_hard_on_missing_asset() {
        let sandbox = std::env::temp_dir().join("marius-assets-test-sw-missing");
        let theme_dir = sandbox.join("theme");
        let build_root = sandbox.join("build");
        fs::create_dir_all(&theme_dir).unwrap();
        fs::create_dir_all(&build_root).unwrap();

        fs::write(
            theme_dir.join("serviceWorker.js"),
            "const CACHE_NAME = \"MARIUS_CACHE_HASH\";\nconst r = ['/scripts/index.js'];",
        )
        .unwrap();

        let registry = AssetUrlRegistry::new(); // vide : rien à trouver
        let mut manifest: HashMap<String, AssetEntry> = HashMap::new();
        let config = ServiceWorkerConfig {
            entry: "serviceWorker.js".to_string(),
        };

        let result = run_service_worker_pipeline(
            &theme_dir,
            &build_root,
            "build/default",
            &config,
            &registry,
            &mut manifest,
        );
        assert!(result.is_err());
        assert!(manifest.is_empty());

        let _ = fs::remove_dir_all(&sandbox);
    }

}
