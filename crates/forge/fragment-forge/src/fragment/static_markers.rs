// crates/forge/fragment-forge/src/fragment/static_markers.rs

//! Scan statique post-lowering des marqueurs `class`/`id`/`data-*`/élément
//! sur le HTML **statique** d'un flux `FlatPageToken` déjà abaissé.
//! Contrat lexical partagé avec `content.compute_js_deps` côté SQL — deux
//! implémentations indépendantes de la même définition, aucune ne dérive
//! de l'autre.

use crate::fragment::token::FlatPageToken;

// =============================================================================
// Scan statique des marqueurs `class` — HANDOFF-js-deps-capacites-frontend-v2.md,
// addendum « MARIUS_MODULES agrège deux sources ».
// =============================================================================

/// Extrait l'ensemble des tokens `class` présents dans le HTML **statique**
/// d'un flux déjà abaissé (post-`lower()` — parent+enfant fusionnés, avant
/// splice de `ModulesPlaceholder`).
///
/// Scanne EXCLUSIVEMENT `FlatPageToken::Static` — jamais l'intérieur d'un
/// `{{ champ }}`/`{% if %}` : une classe qui dépend d'une donnée runtime
/// (`class="{{ some_class }}"`) n'est structurellement pas détectable ici,
/// et ne doit jamais l'être — c'est précisément la frontière entre ce que
/// `fragment-forge` peut savoir à la compilation et ce que seul
/// `content.compute_js_deps` (SQL, à l'écriture) peut savoir.
///
/// Contrat lexical partagé avec `content.compute_js_deps`
/// (`db/05_content/02_systems.sql`) — même DÉFINITION du marqueur (token
/// exact d'un attribut `class`, délimiteur `'` ou `"`, tokenisation sur les
/// espaces), deux implémentations INDÉPENDANTES, aucune ne dérive de
/// l'autre. Jamais une sous-chaîne, jamais un attribut `data-*`.
pub fn extract_static_class_tokens<'src>(
    tokens: &[FlatPageToken<'src>],
) -> std::collections::HashSet<String> {
    use std::sync::OnceLock;

    // Ancrage de frontière sur le nom d'attribut ((?:^|[\s<])class=) — même
    // principe que la regex PL/pgSQL, transposé : évite de matcher un
    // attribut dont le nom se TERMINE par "class" (ex: "data-class=").
    static CLASS_ATTR_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = CLASS_ATTR_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:^|[\s<])class=(?:"([^"]*)"|'([^']*)')"#)
            .expect("regex statique — motif fixe, jamais construit depuis une entrée externe")
    });

    let mut out = std::collections::HashSet::new();
    for token in tokens {
        if let FlatPageToken::Static(s) = token {
            for caps in re.captures_iter(s) {
                let value = caps
                    .get(1)
                    .or_else(|| caps.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                for tok in value.split_whitespace() {
                    if !tok.is_empty() {
                        out.insert(tok.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Extrait les identifiants `id="..."` littéraux d'un flux déjà résolu —
/// même discipline qu'`extract_static_class_tokens` juste au-dessus
/// (uniquement `FlatPageToken::Static`, jamais `{{ }}`, même ancrage de
/// frontière pour éviter de matcher un attribut se terminant par "id=",
/// ex. `data-id=`).
///
/// Contrairement à `class`, un `id` HTML n'est jamais une liste de tokens
/// espacés — une seule valeur retenue par occurrence, jamais splittée sur
/// les espaces.
pub fn extract_static_id_tokens<'src>(
    tokens: &[FlatPageToken<'src>],
) -> std::collections::HashSet<String> {
    use std::sync::OnceLock;

    static ID_ATTR_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = ID_ATTR_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:^|[\s<])id=(?:"([^"]*)"|'([^']*)')"#)
            .expect("regex statique — motif fixe, jamais construit depuis une entrée externe")
    });

    let mut out = std::collections::HashSet::new();
    for token in tokens {
        if let FlatPageToken::Static(s) = token {
            for caps in re.captures_iter(s) {
                let value = caps
                    .get(1)
                    .or_else(|| caps.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                if !value.is_empty() {
                    out.insert(value.to_string());
                }
            }
        }
    }
    out
}

/// Extrait la PRÉSENCE d'attributs `data-*` littéraux — jamais leur valeur
/// (décision de session : un marker `[data-*]` teste uniquement la
/// présence, jamais la valeur). La clé insérée est le nom complet de
/// l'attribut (`data-component`), jamais une forme abrégée ou sans le
/// préfixe `data-`.
pub fn extract_static_data_attribute_tokens<'src>(
    tokens: &[FlatPageToken<'src>],
) -> std::collections::HashSet<String> {
    use std::sync::OnceLock;

    // Le nom est capturé puis borné par '=', un espace, '>' ou '/' — couvre
    // à la fois la forme avec valeur (`data-x="..."`) et la forme booléenne
    // sans valeur (`data-x` seul, `data-x>`, `data-x/>`).
    static DATA_ATTR_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = DATA_ATTR_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:^|[\s<])(data-[a-zA-Z0-9_-]+)(?:=|[\s>/])"#)
            .expect("regex statique — motif fixe, jamais construit depuis une entrée externe")
    });

    let mut out = std::collections::HashSet::new();
    for token in tokens {
        if let FlatPageToken::Static(s) = token {
            for caps in re.captures_iter(s) {
                if let Some(m) = caps.get(1) {
                    out.insert(m.as_str().to_string());
                }
            }
        }
    }
    out
}

/// Extrait les noms de balises OUVRANTES littérales (`<main`, `<nav`, un
/// custom element `<my-widget`...) — jamais une balise produite par
/// composition de fragments non encore résolue à ce point du pipeline
/// (appelée après `lower`, exactement comme `extract_static_class_tokens`).
///
/// Aucune whitelist HTML : tout nom syntaxiquement valide est retenu,
/// l'existence réelle de l'élément (standard ou custom element) n'est
/// jamais vérifiée ici — décision de session, la responsabilité du choix
/// d'un nom pertinent appartient à l'intégrateur.
pub fn extract_static_element_tokens<'src>(
    tokens: &[FlatPageToken<'src>],
) -> std::collections::HashSet<String> {
    use std::sync::OnceLock;

    static ELEMENT_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = ELEMENT_RE.get_or_init(|| {
        regex::Regex::new(r#"<([a-zA-Z][a-zA-Z0-9-]*)"#)
            .expect("regex statique — motif fixe, jamais construit depuis une entrée externe")
    });

    let mut out = std::collections::HashSet::new();
    for token in tokens {
        if let FlatPageToken::Static(s) = token {
            for caps in re.captures_iter(s) {
                if let Some(m) = caps.get(1) {
                    out.insert(m.as_str().to_string());
                }
            }
        }
    }
    out
}

/// Regroupe les quatre catégories de faits statiques nécessaires au
/// matching de `MarkerPredicate` (`crates/core/schema/build.rs`) — un
/// ensemble par forme, JAMAIS fusionnés : une classe et un élément de même
/// nom littéral ne doivent jamais se confondre au matching (absence
/// d'ambiguïté actée en session).
pub struct StaticMarkerFacts {
    pub classes: std::collections::HashSet<String>,
    pub ids: std::collections::HashSet<String>,
    pub data_attributes: std::collections::HashSet<String>,
    pub elements: std::collections::HashSet<String>,
}

/// Point d'entrée unique pour `build.rs` — évite quatre appels séparés à
/// chaque site de composition (Mode Page / STATIC_PAGES). Calculé une
/// seule fois par template, jamais recalculé par capacité — même
/// discipline qu'`extract_static_class_tokens` auparavant. Chacun des
/// quatre extracteurs individuels reste `pub` et testable isolément.
pub fn extract_static_marker_facts<'src>(tokens: &[FlatPageToken<'src>]) -> StaticMarkerFacts {
    StaticMarkerFacts {
        classes: extract_static_class_tokens(tokens),
        ids: extract_static_id_tokens(tokens),
        data_attributes: extract_static_data_attribute_tokens(tokens),
        elements: extract_static_element_tokens(tokens),
    }
}

#[cfg(test)]
mod tests_extract_static_id_tokens {
    use super::FlatPageToken;
    use super::extract_static_id_tokens;

    #[test]
    fn finds_double_quoted_id() {
        let tokens = vec![FlatPageToken::Static(r#"<nav id="menu">"#)];
        let found = extract_static_id_tokens(&tokens);
        assert!(found.contains("menu"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn finds_single_quoted_id() {
        let tokens = vec![FlatPageToken::Static("<div id='hero'>")];
        let found = extract_static_id_tokens(&tokens);
        assert!(found.contains("hero"));
    }

    #[test]
    fn does_not_split_on_whitespace() {
        // Un id ne contient jamais plusieurs tokens espacés, contrairement
        // à class — la valeur entière est retenue telle quelle.
        let tokens = vec![FlatPageToken::Static(r#"<div id="not a valid id">"#)];
        let found = extract_static_id_tokens(&tokens);
        assert_eq!(found.len(), 1);
        assert!(found.contains("not a valid id"));
    }

    #[test]
    fn never_matches_attribute_ending_in_id() {
        let tokens = vec![FlatPageToken::Static(r#"<div data-id="not-a-marker">"#)];
        let found = extract_static_id_tokens(&tokens);
        assert!(found.is_empty());
    }
}

#[cfg(test)]
mod tests_extract_static_data_attribute_tokens {
    use super::FlatPageToken;
    use super::extract_static_data_attribute_tokens;

    #[test]
    fn finds_data_attribute_with_value() {
        let tokens = vec![FlatPageToken::Static(r#"<div data-component="gallery">"#)];
        let found = extract_static_data_attribute_tokens(&tokens);
        assert!(found.contains("data-component"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn finds_boolean_data_attribute_without_value() {
        let tokens = vec![FlatPageToken::Static(r#"<div data-component>"#)];
        let found = extract_static_data_attribute_tokens(&tokens);
        assert!(found.contains("data-component"));
    }

    #[test]
    fn value_is_never_captured() {
        // Présence uniquement — la valeur ne doit jamais apparaître dans
        // l'ensemble retourné.
        let tokens = vec![FlatPageToken::Static(r#"<div data-component="gallery">"#)];
        let found = extract_static_data_attribute_tokens(&tokens);
        assert!(!found.contains("gallery"));
    }

    #[test]
    fn ignores_non_data_attributes() {
        let tokens = vec![FlatPageToken::Static(r#"<div class="map" id="x">"#)];
        let found = extract_static_data_attribute_tokens(&tokens);
        assert!(found.is_empty());
    }
}

#[cfg(test)]
mod tests_extract_static_element_tokens {
    use super::FlatPageToken;
    use super::extract_static_element_tokens;

    #[test]
    fn finds_standard_element() {
        let tokens = vec![FlatPageToken::Static("<main class=\"content\">")];
        let found = extract_static_element_tokens(&tokens);
        assert!(found.contains("main"));
    }

    #[test]
    fn finds_custom_element_without_whitelist() {
        // Aucune whitelist HTML : un custom element est accepté à égalité
        // avec un élément standard.
        let tokens = vec![FlatPageToken::Static("<my-widget></my-widget>")];
        let found = extract_static_element_tokens(&tokens);
        assert!(found.contains("my-widget"));
    }

    #[test]
    fn ignores_closing_tags_as_separate_entries_but_still_finds_name() {
        // Une balise fermante partage le même nom — pas une régression,
        // juste une conséquence attendue d'une regex sur `<nom`.
        let tokens = vec![FlatPageToken::Static("<main></main>")];
        let found = extract_static_element_tokens(&tokens);
        assert_eq!(found.len(), 1);
        assert!(found.contains("main"));
    }
}

#[cfg(test)]
mod tests_extract_static_marker_facts {
    use super::FlatPageToken;
    use super::extract_static_marker_facts;

    #[test]
    fn bundles_all_four_categories_independently() {
        let tokens = vec![FlatPageToken::Static(
            r#"<main id="hero" class="tabs" data-component>"#,
        )];
        let facts = extract_static_marker_facts(&tokens);
        assert!(facts.classes.contains("tabs"));
        assert!(facts.ids.contains("hero"));
        assert!(facts.data_attributes.contains("data-component"));
        assert!(facts.elements.contains("main"));
    }
}

#[cfg(test)]
mod tests_extract_static_class_tokens {
    use super::FlatPageToken;
    use super::extract_static_class_tokens;

    #[test]
    fn finds_double_quoted_class() {
        let tokens = vec![FlatPageToken::Static(r#"<pre class="add-line-marks">"#)];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("add-line-marks"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn finds_single_quoted_class() {
        let tokens = vec![FlatPageToken::Static("<div class='map'>")];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("map"));
    }

    #[test]
    fn splits_multiple_tokens_in_one_class_attr() {
        let tokens = vec![FlatPageToken::Static(
            r#"<div class="range range-multithumb extra">"#,
        )];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("range"));
        assert!(found.contains("range-multithumb"));
        assert!(found.contains("extra"));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn never_matches_attribute_ending_in_class() {
        // Ancrage de frontière : "data-class=" ne doit jamais être confondu
        // avec "class=" — même piège que la regex SQL doit éviter.
        let tokens = vec![FlatPageToken::Static(r#"<div data-class="not-a-marker">"#)];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.is_empty());
    }

    #[test]
    fn ignores_non_static_tokens() {
        // Field/IfBool/etc. ne sont jamais scannés — seul le HTML
        // véritablement statique participe à cette détection.
        let tokens = vec![
            FlatPageToken::Field {
                entity: "record",
                field: "class",
            },
            FlatPageToken::Static(r#"<div class="map">"#),
        ];
        let found = extract_static_class_tokens(&tokens);
        assert_eq!(found.len(), 1);
        assert!(found.contains("map"));
    }

    #[test]
    fn empty_when_no_static_class_present() {
        let tokens = vec![FlatPageToken::Static("<div>sans classe ici</div>")];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.is_empty());
    }

    #[test]
    fn scans_across_multiple_static_tokens() {
        let tokens = vec![
            FlatPageToken::Static(r#"<div class="map">"#),
            FlatPageToken::Static(r#"<pre class="add-line-marks">"#),
        ];
        let found = extract_static_class_tokens(&tokens);
        assert_eq!(found.len(), 2);
        assert!(found.contains("map"));
        assert!(found.contains("add-line-marks"));
    }
}
