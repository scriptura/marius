// crates/assets/src/resolve.rs

//! Résolution unifiée des chemins et URLs.
//!
//! Logique partagée entre :
//! - `[styles]` (directives `url()` CSS)
//! - `[scripts.components]` (imports ESM)
//! - `[service_worker]` (littéraux de chaînes)
//! - `[webmanifest]` (champs `icons[].src` JSON)
//!
//! Constitue le point de vérité unique évitant toute divergence de comportement
//! entre les pipelines sur les cas limites (fragments `#`, URLs absolues `http://`, URLs *data*).

use std::path::{Path, PathBuf};

use crate::manifest::{AssetUrlRegistry, CanonicalAssetId};

/// Origine d'une référence — SPEC-canonical-asset-identity.md §2.
///
/// Deux variantes, jamais davantage sans revenir sur la spécification.
/// `canonicalize_reference` ne devine JAMAIS l'origine depuis la forme du
/// specifier : c'est toujours l'appelant qui la fixe, selon la sémantique
/// propre à son propre format (CSS `url()`/import JS relatif → toujours
/// `RelativeToFile` ; déclaration `theme.toml`, convention `/...` déjà en
/// usage en webmanifest → `RelativeToThemeRoot`). Un specifier « nu » (ni
/// `./`, `../`, ni `/`) n'est jamais ambigu pour `canonicalize_reference`
/// elle-même : il l'est seulement si l'appelant ne sait pas quelle origine
/// choisir pour son propre contexte — ce choix ne regarde jamais cette
/// fonction.
pub(crate) enum ReferenceOrigin<'a> {
    /// Relative au fichier CONTENANT la référence — `id` = l'identité
    /// canonique de ce fichier (jamais du point d'entrée du bundle : pour
    /// un CSS, c'est le fichier réel où le `url()` est textuellement
    /// écrit, retrouvé via `source_index`, cf. styles.rs).
    RelativeToFile(&'a CanonicalAssetId),
    /// Relative à la racine du thème.
    RelativeToThemeRoot,
}

/// Erreur de canonicalisation — SPEC-canonical-asset-identity.md §10.
/// Toujours une erreur dure, jamais un repli silencieux.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CanonicalizeError {
    /// La référence sort de la racine du thème (`../` en excès).
    EscapesThemeRoot(String),
}

impl std::fmt::Display for CanonicalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalizeError::EscapesThemeRoot(s) => {
                write!(f, "la référence '{s}' sort de la racine du thème")
            }
        }
    }
}

/// Issue de la canonicalisation — deux cas, jamais fusionnés.
#[derive(Debug)]
pub(crate) enum CanonicalReference {
    /// Référence locale canonicalisée avec succès.
    Id(CanonicalAssetId),
    /// Hors périmètre de canonicalisation (URL externe, `data:`, fragment
    /// pur) — jamais une erreur, jamais un `CanonicalAssetId` : cette
    /// famille ne relève jamais du registre d'assets.
    External,
}

/// Canonicalise une référence de spécifier BRUTE, telle qu'écrite dans un
/// artefact source, vers un `CanonicalAssetId` — SPEC-canonical-asset-
/// identity.md §2/§8.
///
/// Fonction PURE : aucune E/S, aucun accès à un registre, aucune
/// connaissance de bibliothèque ni de manifeste. Ne rejette jamais un
/// artefact source pour la forme de ses références (`../foo`, `foo`,
/// `/foo` restent tous des specifiers natifs valides de leur langage
/// d'origine) — seule la sortie de la racine du thème est une erreur ici.
pub(crate) fn canonicalize_reference(
    specifier: &str,
    origin: ReferenceOrigin,
) -> Result<CanonicalReference, CanonicalizeError> {
    if is_external_url(specifier) {
        return Ok(CanonicalReference::External);
    }

    let base_dir: PathBuf = match origin {
        ReferenceOrigin::RelativeToFile(id) => Path::new(id.as_str())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        ReferenceOrigin::RelativeToThemeRoot => PathBuf::new(),
    };

    // Convention `/...` (RelativeToThemeRoot) : le slash de tête est un
    // marqueur de mode côté auteur, jamais un segment de chemin —
    // équivalent à une racine déjà explicite, pas une remontée.
    let spec = specifier.strip_prefix('/').unwrap_or(specifier);

    let joined = base_dir.join(spec);
    let normalized = normalize_path_segments(&joined)?;

    Ok(CanonicalReference::Id(
        CanonicalAssetId::from_canonical_string(path_to_slash_owned(&normalized)),
    ))
}

/// Résout `.`/`..` d'un chemin logique (jamais touché au système de
/// fichiers réel — ce chemin peut ne pas exister encore au moment de
/// l'appel, ex. avant la découverte d'une bibliothèque). `..` au-delà de
/// la racine (pile vide) → erreur, jamais une remontée silencieuse hors
/// du thème.
fn normalize_path_segments(p: &Path) -> Result<PathBuf, CanonicalizeError> {
    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(CanonicalizeError::EscapesThemeRoot(path_to_slash_owned(p)));
                }
            }
            std::path::Component::CurDir | std::path::Component::RootDir => {}
            std::path::Component::Normal(seg) => stack.push(seg.to_os_string()),
            std::path::Component::Prefix(_) => {}
        }
    }
    Ok(stack.into_iter().collect())
}

/// Identique à `manifest::path_to_slash`, dupliquée en version qui prend
/// possession plutôt qu'emprunte — évite d'importer `path_to_slash` pour
/// un seul appel dans un module qui n'a par ailleurs aucune raison de
/// dépendre du reste de l'API de `manifest.rs` au-delà de
/// `AssetUrlRegistry`/`CanonicalAssetId`.
fn path_to_slash_owned(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Sépare un chemin ou une URL de son fragment (ex: `#icon`).
///
/// Fonction pure, indépendante du contexte d'appel (AST CSS, parsing JSON, etc.).
///
/// ## Exemples
/// - `"sprites/utils.svg#icon"` $\rightarrow$ `("sprites/utils.svg", "#icon")`
/// - `"sprites/utils.svg"` $\rightarrow$ `("sprites/utils.svg", "")`
pub(crate) fn split_url_fragment(source: &str) -> (&str, &str) {
    match source.find('#') {
        Some(idx) => (&source[..idx], &source[idx..]),
        None => (source, ""),
    }
}

/// Résout une référence (chemin ou URL) contre l'`AssetUrlRegistry`, via
/// `canonicalize_reference` — SPEC-canonical-asset-identity.md §3
/// (« lookup registre », distinct de la canonicalisation pure elle-même).
///
/// ## Comportement de Retour
///
/// - `Ok(Some(url))` : Résolution réussie en URL publique.
/// - `Ok(None)` : La cible est externe (`canonicalize_reference` a renvoyé
///   `External`) — pas une erreur, l'appelant laisse la cible intacte.
/// - `Err(message)` : Cible locale canonicalisée mais introuvable dans le
///   registre (asset non autorisé), ou référence sortant de la racine du
///   thème. Jamais un repli vers une résolution par nom de fichier seul.
pub(crate) fn resolve_asset_reference(
    source: &str,
    origin: ReferenceOrigin,
    registry: &AssetUrlRegistry,
) -> Result<Option<String>, String> {
    // Sépare un éventuel fragment (`sprites/utils.svg#icon` — un `url()`
    // pointant vers UN symbole précis d'un sprite fusionné, cf. Phase 4) :
    // seul le chemin AVANT `#` participe à la canonicalisation. Le
    // fragment n'est jamais interprété, seulement réattaché tel quel à
    // l'URL résolue.
    let (path_part, fragment) = split_url_fragment(source);

    match canonicalize_reference(path_part, origin) {
        Ok(CanonicalReference::External) => Ok(None),
        Ok(CanonicalReference::Id(id)) => match registry.get(&id) {
            Some(resolved) => Ok(Some(format!("{resolved}{fragment}"))),
            None => Err(id.into_string()),
        },
        Err(e) => Err(e.to_string()),
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
pub(crate) fn is_external_url(url: &str) -> bool {
    url.starts_with('#') || url.starts_with("//") || url.starts_with("data:") || url.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── canonicalize_reference (SPEC-canonical-asset-identity.md §2) ────────

    fn cid(s: &str) -> CanonicalAssetId {
        CanonicalAssetId::from_canonical_string(s.to_string())
    }

    #[test]
    fn canonicalize_relative_to_file_joins_and_normalizes() {
        // styles/main.css + "../images/logo.svg" → images/logo.svg
        let origin_file = cid("styles/main.css");
        let result = canonicalize_reference(
            "../images/logo.svg",
            ReferenceOrigin::RelativeToFile(&origin_file),
        );
        match result {
            Ok(CanonicalReference::Id(id)) => assert_eq!(id.as_str(), "images/logo.svg"),
            other => panic!("attendu Id(images/logo.svg), obtenu : {other:?}"),
        }
    }

    /// Cas nommé explicitement dans la SPEC (§ exemple GPT) : une
    /// bibliothèque important une image relative à SON PROPRE fichier CSS,
    /// pas au point d'entrée du bundle.
    #[test]
    fn canonicalize_relative_to_file_from_nested_library_source() {
        let origin_file = cid("libraries/deck-gl/deck.css");
        let result = canonicalize_reference(
            "./images/marker.png",
            ReferenceOrigin::RelativeToFile(&origin_file),
        );
        match result {
            Ok(CanonicalReference::Id(id)) => {
                assert_eq!(id.as_str(), "libraries/deck-gl/images/marker.png")
            }
            other => panic!("attendu Id(libraries/deck-gl/images/marker.png), obtenu : {other:?}"),
        }
    }

    #[test]
    fn canonicalize_relative_to_theme_root_strips_leading_slash() {
        let result =
            canonicalize_reference("/favicons/logo.svg", ReferenceOrigin::RelativeToThemeRoot);
        match result {
            Ok(CanonicalReference::Id(id)) => assert_eq!(id.as_str(), "favicons/logo.svg"),
            other => panic!("attendu Id(favicons/logo.svg), obtenu : {other:?}"),
        }
    }

    #[test]
    fn canonicalize_bare_specifier_relative_to_theme_root() {
        // theme.toml : main = "scripts/development/main.js" — nu, relatif
        // à la racine par convention de CE contexte précis.
        let result = canonicalize_reference(
            "scripts/development/main.js",
            ReferenceOrigin::RelativeToThemeRoot,
        );
        match result {
            Ok(CanonicalReference::Id(id)) => {
                assert_eq!(id.as_str(), "scripts/development/main.js")
            }
            other => panic!("attendu Id inchangé, obtenu : {other:?}"),
        }
    }

    #[test]
    fn canonicalize_external_returns_external_not_error() {
        let result = canonicalize_reference(
            "https://cdn.example.com/x.js",
            ReferenceOrigin::RelativeToThemeRoot,
        );
        assert!(matches!(result, Ok(CanonicalReference::External)));
    }

    /// SPEC §10 — sortie de racine : erreur dure, jamais une remontée
    /// silencieuse hors du thème.
    #[test]
    fn canonicalize_rejects_escape_beyond_theme_root() {
        let origin_file = cid("logo.svg"); // fichier À LA RACINE du thème
        let result = canonicalize_reference(
            "../../etc/passwd",
            ReferenceOrigin::RelativeToFile(&origin_file),
        );
        assert!(matches!(
            result,
            Err(CanonicalizeError::EscapesThemeRoot(_))
        ));
    }

    #[test]
    fn canonicalize_dot_segments_are_normalized_away() {
        let origin_file = cid("a/b/c.css");
        let result =
            canonicalize_reference("./././x.png", ReferenceOrigin::RelativeToFile(&origin_file));
        match result {
            Ok(CanonicalReference::Id(id)) => assert_eq!(id.as_str(), "a/b/x.png"),
            other => panic!("attendu Id(a/b/x.png), obtenu : {other:?}"),
        }
    }

    // ── resolve_asset_reference (partagé CSS + webmanifest + scripts) ───────

    #[test]
    fn resolve_asset_reference_found_returns_resolved_url() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert(cid("images/logo.svg"), "/images/logo.12452.svg".to_string());
        let origin_file = cid("styles/main.css");
        assert_eq!(
            resolve_asset_reference(
                "../images/logo.svg",
                ReferenceOrigin::RelativeToFile(&origin_file),
                &registry
            ),
            Ok(Some("/images/logo.12452.svg".to_string()))
        );
    }

    #[test]
    fn resolve_asset_reference_external_returns_ok_none() {
        let registry = AssetUrlRegistry::new();
        assert_eq!(
            resolve_asset_reference(
                "https://cdn.example.com/icon.png",
                ReferenceOrigin::RelativeToThemeRoot,
                &registry
            ),
            Ok(None)
        );
    }

    /// Le second bug de session, vu depuis l'API partagée : le fragment
    /// doit survivre à la résolution, pas seulement au niveau CSS.
    #[test]
    fn resolve_asset_reference_preserves_fragment_on_resolved_url() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            cid("sprites/utils.svg"),
            "/sprites/utils.4c4e9.svg".to_string(),
        );
        assert_eq!(
            resolve_asset_reference(
                "sprites/utils.svg#icon",
                ReferenceOrigin::RelativeToThemeRoot,
                &registry
            ),
            Ok(Some("/sprites/utils.4c4e9.svg#icon".to_string()))
        );
    }

    #[test]
    fn resolve_asset_reference_missing_key_returns_err_with_canonical_id() {
        let registry = AssetUrlRegistry::new();
        assert_eq!(
            resolve_asset_reference(
                "favicons/logo.svg",
                ReferenceOrigin::RelativeToThemeRoot,
                &registry
            ),
            Err("favicons/logo.svg".to_string())
        );
    }

    /// SPEC §11 — critère d'acceptation central de tout ce chantier :
    /// deux fichiers de même NOM, sous des racines distinctes, coexistent
    /// dans le registre sans écrasement. Aurait échoué sous l'ancien
    /// mécanisme `file_name()` (les deux auraient partagé la clé "index.js").
    #[test]
    fn no_collision_between_same_basename_under_different_roots() {
        let mut registry = AssetUrlRegistry::new();
        registry.insert(
            cid("libraries/foo/index.js"),
            "/libraries/foo/index.a1b2c.js".to_string(),
        );
        registry.insert(
            cid("libraries/bar/index.js"),
            "/libraries/bar/index.d4e5f.js".to_string(),
        );

        assert_eq!(
            resolve_asset_reference(
                "libraries/foo/index.js",
                ReferenceOrigin::RelativeToThemeRoot,
                &registry
            ),
            Ok(Some("/libraries/foo/index.a1b2c.js".to_string()))
        );
        assert_eq!(
            resolve_asset_reference(
                "libraries/bar/index.js",
                ReferenceOrigin::RelativeToThemeRoot,
                &registry
            ),
            Ok(Some("/libraries/bar/index.d4e5f.js".to_string()))
        );
    }
}
