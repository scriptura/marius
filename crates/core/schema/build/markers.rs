// crates/core/schema/build/markers.rs

//! Grammaire et parsing des sélecteurs de marqueurs (`MarkerPredicate`).
//!
//! Sous-ensemble fermé inspiré des sélecteurs CSS — quatre formes atomiques
//! (`.classe`, `#id`, `[data-*]`, élément bare), aucun combinator, aucun
//! sélecteur composé. Consommé par `capabilities::validate_capabilities`
//! (parsing, une seule fois par capacité) et par
//! `modules_lowering::lower_modules_for_template` (test de présence
//! contre `StaticMarkerFacts`).

/// Représentation interne d'un marker après parsing AOT — sous-ensemble
/// fermé inspiré des sélecteurs CSS (session « généralisation markers »),
/// quatre formes atomiques, jamais de combinator ni de sélecteur composé.
/// Chaque variante est un test de présence isolé, jamais combinable entre
/// elles au sein d'un même marker.
///
/// Breaking change assumé : l'ancienne syntaxe bare implicite (`markers =
/// ["carousel-embed"]` interprété comme une classe) n'existe plus. Un bare
/// token est désormais TOUJOURS `Element` — aucune whitelist HTML, la
/// responsabilité du nom pertinent appartient à l'intégrateur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkerPredicate {
    /// `.nom` — présence de la classe `nom`.
    Class(String),
    /// `#nom` — présence de l'id `nom`.
    Id(String),
    /// `[data-nom]` — présence de l'attribut `data-nom`, valeur jamais
    /// considérée. Restreint aux attributs `data-*` : `class`/`id`
    /// possèdent déjà leurs formes dédiées, aucun besoin d'un attribut
    /// générique (`[href]`, `[role]`, ...) à ce stade.
    Attribute(String),
    /// Bare token — présence d'un élément `<nom>` littéral. Aucune
    /// whitelist : un custom element est accepté à égalité avec un élément
    /// HTML standard.
    Element(String),
}

/// Parse un marker brut (`theme.toml`) vers sa forme typée — échec dur,
/// jamais un repli implicite vers une forme par défaut. Appelé une seule
/// fois par marker, dans `validate_capabilities`, jamais recalculé plus
/// tard (même discipline que la résolution `entry`/`deps`).
///
/// Grammaire volontairement minimale et fermée — voir doc de
/// `MarkerPredicate` pour le périmètre exact. Combinators, sélecteurs
/// composés, pseudo-classes/pseudo-éléments et comparateurs de valeur
/// d'attribut sont HORS PÉRIMÈTRE et rejetés ici avec un message explicite,
/// jamais silencieusement ignorés ou réinterprétés.
pub(crate) fn parse_marker(raw: &str) -> Result<MarkerPredicate, String> {
    if raw.is_empty() {
        return Err("marker vide".to_string());
    }
    if raw.chars().any(|c| c.is_whitespace()) {
        return Err(format!(
            "marker {raw:?} : espace interne interdit — une forme atomique ne peut jamais en \
             contenir (rejette notamment tout sélecteur composé du type \"main .tabs\")"
        ));
    }

    if let Some(rest) = raw.strip_prefix('.') {
        return parse_marker_ident_body(raw, rest, "Class").map(MarkerPredicate::Class);
    }
    if let Some(rest) = raw.strip_prefix('#') {
        return parse_marker_ident_body(raw, rest, "Id").map(MarkerPredicate::Id);
    }
    if raw.starts_with('[') {
        let inner = raw
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .ok_or_else(|| {
                format!(
                    "marker {raw:?} : forme [data-*] malformée — crochet fermant absent, ou \
                     contenu après ']'"
                )
            })?;
        if inner.contains('=') {
            return Err(format!(
                "marker {raw:?} : valeur d'attribut non supportée — un marker [data-*] teste \
                 uniquement la présence de l'attribut, jamais sa valeur (comparateurs =, ^=, $=, \
                 *=, ~=, |= tous hors périmètre)"
            ));
        }
        if !inner.starts_with("data-") || inner.len() == "data-".len() {
            return Err(format!(
                "marker {raw:?} : seuls les attributs 'data-*' non vides sont supportés — \
                 'class'/'id' possèdent déjà leurs formes dédiées ('.'/'#'), aucun attribut \
                 générique n'est accepté ici"
            ));
        }
        let suffix = &inner["data-".len()..];
        if !suffix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "marker {raw:?} : nom d'attribut data-* invalide — lettres, chiffres, '-', '_' \
                 uniquement après 'data-'"
            ));
        }
        return Ok(MarkerPredicate::Attribute(inner.to_string()));
    }
    if raw.ends_with(']') {
        return Err(format!(
            "marker {raw:?} : ']' rencontré sans '[' correspondant en tête"
        ));
    }

    if !raw.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(format!(
            "marker {raw:?} : aucune forme reconnue — formes valides : '.classe', '#id', \
             '[data-*]', ou un nom d'élément commençant par une lettre (combinators, sélecteurs \
             composés et pseudo-classes/éléments hors périmètre)"
        ));
    }
    if raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        Ok(MarkerPredicate::Element(raw.to_string()))
    } else {
        Err(format!(
            "marker {raw:?} : nom d'élément invalide — caractères autorisés après la lettre \
             initiale : lettres, chiffres, '-'"
        ))
    }
}

/// Corps partagé du parsing pour `.classe` et `#id` — même charset, même
/// contrainte de non-vacuité, seul le nom du prédicat change dans les
/// messages d'erreur.
fn parse_marker_ident_body(raw: &str, rest: &str, kind: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Err(format!("marker {raw:?} : {kind} vide après le préfixe"));
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "marker {raw:?} : {kind} contient un caractère non autorisé — lettres, chiffres, \
             '-', '_' uniquement"
        ));
    }
    Ok(rest.to_string())
}

#[cfg(test)]
mod tests_parse_marker {
    use super::{MarkerPredicate, parse_marker};

    #[test]
    fn class_form() {
        assert_eq!(
            parse_marker(".tabs"),
            Ok(MarkerPredicate::Class("tabs".to_string()))
        );
    }

    #[test]
    fn id_form() {
        assert_eq!(
            parse_marker("#menu"),
            Ok(MarkerPredicate::Id("menu".to_string()))
        );
    }

    #[test]
    fn data_attribute_form() {
        assert_eq!(
            parse_marker("[data-component]"),
            Ok(MarkerPredicate::Attribute("data-component".to_string()))
        );
    }

    #[test]
    fn element_form_bare_token() {
        assert_eq!(
            parse_marker("main"),
            Ok(MarkerPredicate::Element("main".to_string()))
        );
    }

    #[test]
    fn custom_element_name_is_accepted_without_whitelist() {
        // Aucune whitelist HTML — un custom element est accepté à égalité
        // avec un élément standard, seule la forme est vérifiée.
        assert_eq!(
            parse_marker("my-widget"),
            Ok(MarkerPredicate::Element("my-widget".to_string()))
        );
    }

    #[test]
    fn breaking_change_bare_token_is_never_implicit_class() {
        // Décision de session : l'ancienne rétrocompatibilité implicite
        // (bare token = classe) n'existe plus.
        assert_ne!(
            parse_marker("tabs").unwrap(),
            MarkerPredicate::Class("tabs".to_string())
        );
        assert_eq!(
            parse_marker("tabs").unwrap(),
            MarkerPredicate::Element("tabs".to_string())
        );
    }

    #[test]
    fn empty_marker_is_rejected() {
        assert!(parse_marker("").is_err());
    }

    #[test]
    fn empty_class_is_rejected() {
        assert!(parse_marker(".").is_err());
    }

    #[test]
    fn empty_id_is_rejected() {
        assert!(parse_marker("#").is_err());
    }

    #[test]
    fn internal_whitespace_is_rejected() {
        assert!(parse_marker("main .tabs").is_err());
    }

    #[test]
    fn attribute_value_is_rejected() {
        assert!(parse_marker(r#"[data-component="gallery"]"#).is_err());
    }

    #[test]
    fn attribute_value_comparators_are_all_rejected() {
        for marker in [
            r#"[data-x="y"]"#,
            r#"[data-x^="y"]"#,
            r#"[data-x$="y"]"#,
            r#"[data-x*="y"]"#,
            r#"[data-x~="y"]"#,
            r#"[data-x|="y"]"#,
        ] {
            assert!(
                parse_marker(marker).is_err(),
                "attendu une erreur pour {marker:?}"
            );
        }
    }

    #[test]
    fn non_data_attribute_is_rejected() {
        // 'class'/'id' possèdent déjà leurs formes dédiées ; aucun attribut
        // générique n'est accepté ('[href]', '[role]', '[aria-hidden]', ...).
        for marker in ["[href]", "[role]", "[aria-hidden]", "[class]", "[id]"] {
            assert!(
                parse_marker(marker).is_err(),
                "attendu une erreur pour {marker:?}"
            );
        }
    }

    #[test]
    fn unbalanced_bracket_is_rejected() {
        assert!(parse_marker("[data-x").is_err());
        assert!(parse_marker("data-x]").is_err());
    }

    #[test]
    fn unknown_prefix_is_rejected() {
        for marker in [":not(x)", "*", ">tabs", "~tabs", "+tabs"] {
            assert!(
                parse_marker(marker).is_err(),
                "attendu une erreur pour {marker:?}"
            );
        }
    }

    #[test]
    fn compound_selector_forms_are_rejected() {
        for marker in ["main.tabs", ".tabs#active", "main>tabs"] {
            assert!(
                parse_marker(marker).is_err(),
                "attendu une erreur pour {marker:?}"
            );
        }
    }

    #[test]
    fn pseudo_class_forms_are_rejected() {
        for marker in [":first-child", ":not(.tabs)", "::before"] {
            assert!(
                parse_marker(marker).is_err(),
                "attendu une erreur pour {marker:?}"
            );
        }
    }

    #[test]
    fn element_starting_with_digit_or_hyphen_is_rejected() {
        assert!(parse_marker("1widget").is_err());
        assert!(parse_marker("-widget").is_err());
    }

    #[test]
    fn class_and_id_reject_dot_or_hash_inside_body() {
        assert!(parse_marker(".tabs.active").is_err());
        assert!(parse_marker("#menu#top").is_err());
    }
}
