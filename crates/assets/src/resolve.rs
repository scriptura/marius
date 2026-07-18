// crates/assets/src/resolve.rs
//
// Résolution d'une référence de chemin contre `AssetUrlRegistry` — logique
// PARTAGÉE entre [styles] (url() CSS), [scripts.components] (import ESM),
// [service_worker] (littéraux de chaîne) et run_webmanifest_pipeline
// (icons[].src JSON). Un seul point de vérité, jamais une réimplémentation
// par pipeline qui pourrait diverger silencieusement sur un cas limite
// (fragment, URL externe...).

use std::path::Path;

use crate::manifest::AssetUrlRegistry;

/// Sépare un `url()`/`src` en (chemin, fragment) — `"sprites/utils.svg#icon"`
/// → `("sprites/utils.svg", "#icon")`, `"sprites/utils.svg"` → (inchangé,
/// `""`). Fonction pure, testable indépendamment de tout AST CSS ou JSON.
pub(crate) fn split_url_fragment(source: &str) -> (&str, &str) {
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
pub(crate) fn resolve_asset_reference(
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

}
