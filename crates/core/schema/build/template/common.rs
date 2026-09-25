// crates/core/schema/build/template/common.rs

//! Primitives partagées du pipeline Voie B (`.marius` → `render()`) :
//! en-tête du fichier généré, lecture brute d'un template, et repérage
//! du point d'injection d'un marqueur HTML dans un flux de tokens.

use std::path::Path;

use marius_fragment_forge::FlatPageToken;

// En-tête statique du fichier généré — pas de couplage sur fragment-forge pour
// ce seul token textuel (décision architecturale Phase 0).
pub(crate) const GENERATED_HEADER: &str = "// GÉNÉRÉ PAR LA FORGE MARIUS — NE PAS MODIFIER MANUELLEMENT\n\
// Régénérer via : cargo build\n\n\
#[allow(unused_imports)]\n\
use crate::projection::Projection as _;\n\n\
#[allow(unused_imports)]\n\
use chrono::Datelike as _;\n\n\
/// Échappe les caractères HTML dangereux dans `s` et pousse le résultat dans `buf`.\n\
/// Zéro allocation : opère directement sur buf (déjà réservé par render()).\n\
#[inline(always)]\n\
#[allow(dead_code)]\n\
fn marius_html_escape(s: &str, buf: &mut String) {\n\
    for ch in s.chars() {\n\
        match ch {\n\
            '&'  => buf.push_str(\"&amp;\"),\n\
            '<'  => buf.push_str(\"&lt;\"),\n\
            '>'  => buf.push_str(\"&gt;\"),\n\
            '\"' => buf.push_str(\"&quot;\"),\n\
            '\\'' => buf.push_str(\"&#39;\"),\n\
            _    => buf.push(ch),\n\
        }\n\
    }\n\
}\n\n\
/// Pousse un VarlenSlot dans la TOC et concatène la valeur dans le heap (Phase 1.4).\n\
#[inline(always)]\n\
#[allow(dead_code)]\n\
fn push_varlen_slot(field: &Option<String>, heap: &mut Vec<u8>, toc: &mut Vec<crate::projection::VarlenSlot>) {\n\
    match field {\n\
        None    => toc.push(crate::projection::VarlenSlot { offset: u32::MAX, len: 0 }),\n\
        Some(s) => {\n\
            let offset = heap.len() as u32;\n\
            heap.extend_from_slice(s.as_bytes());\n\
            toc.push(crate::projection::VarlenSlot { offset, len: s.len() as u32 });\n\
        }\n\
    }\n\
}\n\n";

/// Lecture brute d'un fichier `.marius`. Extraction pure du bloc de lecture
/// déjà présent dans `resolve_template` — aucun changement de comportement
/// sur le chemin de succès. Isolée pour être réutilisable telle quelle par
/// un futur appelant traitant un second fichier (portée hors Phase 6.1 :
/// aucun second appelant n'est câblé ici).
///
/// Retourne :
///   `Ok(src)` : contenu du fichier.
///   `Err(())` : lecture échouée — cargo:error déjà émis par cette fonction.
pub(crate) fn read_template_file(path: &Path) -> Result<String, ()> {
    std::fs::read_to_string(path).map_err(|e| {
        println!(
            "cargo:error=DB-Forge : lecture du template échouée ({}) : {e}",
            path.display()
        );
    })
}

/// Cherche `marker` comme SOUS-CHAÎNE d'un `FlatPageToken::Static` du flux
/// — pas une correspondance de token entier : en pratique, le marqueur est
/// noyé dans un bloc HTML statique plus large (`<head>...<!--MARIUS_
/// SCRIPTS-->...</head>` forme un seul `Static` tant qu'aucune directive
/// `{% %}`/`{{ }}` ne le coupe). Si trouvé, scinde ce token en (avant,
/// après) — en omettant la moitié vide s'il y en a une (le marqueur en
/// tout début ou toute fin de bloc ne doit pas produire un `Static("")`
/// inutile — pas une simplification cosmétique : `generate_aot_snippet`
/// émettrait un `buf.push_str("")` mort dans le code généré, un `Static`
/// vide n'a aucune raison structurelle d'exister dans ce flux.
///
/// Retourne `(flux_modifié, indice_où_insérer_le_bloc_de_scripts)` — cet
/// indice tombe exactement entre les deux moitiés, prêt pour
/// `splice_hoisted_scripts`. `None` si le marqueur n'apparaît dans aucun
/// `Static` du flux.
pub(crate) fn split_static_at_marker<'src>(
    mut tokens: Vec<FlatPageToken<'src>>,
    marker: &str,
) -> Option<(Vec<FlatPageToken<'src>>, usize)> {
    let (index, pos) = tokens.iter().enumerate().find_map(|(i, t)| match t {
        FlatPageToken::Static(s) => s.find(marker).map(|pos| (i, pos)),
        _ => None,
    })?;

    let mut tail = tokens.split_off(index + 1);
    let marked = tokens
        .pop()
        .expect("index provient de tokens.iter(), non vide ici");
    let full = match marked {
        FlatPageToken::Static(s) => s,
        _ => unreachable!("le filtre ci-dessus ne retient que des Static"),
    };
    let before = &full[..pos];
    let after = &full[pos + marker.len()..];

    if !before.is_empty() {
        tokens.push(FlatPageToken::Static(before));
    }
    let splice_index = tokens.len();
    if !after.is_empty() {
        tokens.push(FlatPageToken::Static(after));
    }
    tokens.append(&mut tail);

    Some((tokens, splice_index))
}

/// Erreurs nommées de [`split_static_at_region`] — convention `Result`
/// (jamais `Option`) pour s'insérer dans le style déjà en vigueur de
/// `resolve_page_template` (`Result`/`map_err`/`cargo:error`), à la
/// différence de `split_static_at_marker` ci-dessus (un seul marqueur, un
/// seul cas d'échec possible — `None` suffisait).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// intentionally unused until V2c wires the split into template publication —
// câblage réel (publication.toml → artefacts → RouteDescriptor K=3) prévu
// pour V2c, pas cet incrément (V2b : mécanique de séparation isolée et
// testée, aucun appelant de production encore). Retirer cet allow au
// premier appel réel introduit par V2c — jamais avant.
#[allow(dead_code)]
pub(crate) enum SplitRegionError {
    /// `begin_marker` n'apparaît dans aucun `FlatPageToken::Static` du flux.
    BeginMarkerNotFound,
    /// `begin_marker` trouvé, mais `end_marker` n'apparaît dans aucun
    /// `Static` à partir de cette position (y compris le cas où
    /// `end_marker` existerait avant `begin_marker` — non traité comme un
    /// cas séparé, reporté ici : aucune fin valide pour ce début).
    EndMarkerNotFound,
    /// `begin_marker` apparaît plus d'une fois dans le flux — ambiguïté
    /// jamais résolue silencieusement (ni « la première », ni « la
    /// dernière » occurrence n'est un choix par défaut acceptable).
    DuplicateBeginMarker,
    /// `end_marker` apparaît plus d'une fois à partir de la position de
    /// `begin_marker` — même discipline que `DuplicateBeginMarker`.
    DuplicateEndMarker,
    /// Un token qui n'est pas `FlatPageToken::Static` (`Field`, `IfBool`,
    /// `IfEq`, `IfNeq`, `Else`, `EndIf`, `AssetRef`, `ScriptStart`,
    /// `ScriptEnd`, `StaticInclude`, `ModulesPlaceholder`) a été rencontré
    /// strictement entre `begin_marker` et `end_marker`. La région doit
    /// être du HTML statique inconditionnel de bout en bout — généralise
    /// à tout token non-`Static` l'exigence « profondeur if == 0 » (une
    /// région conditionnelle n'est qu'un cas particulier de ce que ce
    /// variant rejette).
    NonStaticTokenInRegion,
}

/// Sépare `tokens` en deux flux (`head`, `tail`) autour d'une région
/// délimitée par deux marqueurs textuels, **retirée des deux flux** —
/// jamais réinjectée. Distinct de [`split_static_at_marker`] ci-dessus (un
/// seul marqueur, un point d'INJECTION) : ici deux marqueurs bornent une
/// région à EXCISER, produite ailleurs, au runtime (contrat Volatile).
///
/// # Ne suppose jamais la région contenue dans un seul `Static`
///
/// Un commentaire `{# ... #}` bien formé scinde déjà silencieusement un run
/// HTML en deux `FlatPageToken::Static` consécutifs (`lexer.rs`,
/// `Mode::Literal` : le contenu du commentaire n'émet aucun span, mais la
/// recherche du `Literal` suivant repart à zéro après `#}`) — sans qu'aucun
/// bloc de contrôle n'intervienne. Cette fonction parcourt donc
/// explicitement plusieurs `Static` consécutifs pour trouver `begin_marker`
/// et `end_marker`, plutôt que de chercher dans un seul token comme
/// `split_static_at_marker`.
///
/// # Erreurs
///
/// Voir [`SplitRegionError`]. Jamais de panic, jamais de troncature ou de
/// résolution silencieuse d'une ambiguïté (marqueur dupliqué, région
/// traversant un token non-`Static`).
// intentionally unused until V2c wires the split into template publication —
// même justification que SplitRegionError ci-dessus. Retirer cet allow au
// premier appel réel introduit par V2c — jamais avant.
#[allow(dead_code)]
pub(crate) fn split_static_at_region<'src>(
    tokens: Vec<FlatPageToken<'src>>,
    begin_marker: &str,
    end_marker: &str,
) -> Result<(Vec<FlatPageToken<'src>>, Vec<FlatPageToken<'src>>), SplitRegionError> {
    // ── 1. Localiser begin_marker — toutes occurrences, dans tout le flux ──
    let mut begin_hits: Vec<(usize, usize)> = Vec::new(); // (index token, offset octet)
    for (idx, token) in tokens.iter().enumerate() {
        if let FlatPageToken::Static(s) = token {
            let mut start = 0;
            while let Some(rel) = s[start..].find(begin_marker) {
                begin_hits.push((idx, start + rel));
                start += rel + begin_marker.len();
                if start > s.len() {
                    break;
                }
            }
        }
    }
    let (begin_idx, begin_off) = match begin_hits.as_slice() {
        [] => return Err(SplitRegionError::BeginMarkerNotFound),
        [one] => *one,
        _ => return Err(SplitRegionError::DuplicateBeginMarker),
    };

    // ── 2. Localiser end_marker — occurrences à partir de la position de
    //    begin_marker (même Static, après begin_off, ou Static suivants) ──
    let mut end_hits: Vec<(usize, usize)> = Vec::new();
    for (idx, token) in tokens.iter().enumerate().skip(begin_idx) {
        if let FlatPageToken::Static(s) = token {
            let search_from = if idx == begin_idx {
                begin_off + begin_marker.len()
            } else {
                0
            };
            if search_from > s.len() {
                continue;
            }
            let mut start = search_from;
            while let Some(rel) = s[start..].find(end_marker) {
                end_hits.push((idx, start + rel));
                start += rel + end_marker.len();
                if start > s.len() {
                    break;
                }
            }
        }
    }
    let (end_idx, end_off) = match end_hits.as_slice() {
        [] => return Err(SplitRegionError::EndMarkerNotFound),
        [one] => *one,
        _ => return Err(SplitRegionError::DuplicateEndMarker),
    };

    // ── 3. La région (strictement entre begin_marker et end_marker) ne
    //    doit contenir que du Static — tout autre token est une erreur ──
    for token in tokens.iter().take(end_idx + 1).skip(begin_idx) {
        if !matches!(token, FlatPageToken::Static(_)) {
            return Err(SplitRegionError::NonStaticTokenInRegion);
        }
    }

    // ── 4. Construction de head/tail — jamais de Static("") mort ────────
    let mut head: Vec<FlatPageToken<'src>> = tokens[..begin_idx].to_vec();
    if let FlatPageToken::Static(s) = &tokens[begin_idx] {
        let before = &s[..begin_off];
        if !before.is_empty() {
            head.push(FlatPageToken::Static(before));
        }
    }

    let mut tail: Vec<FlatPageToken<'src>> = Vec::new();
    if let FlatPageToken::Static(s) = &tokens[end_idx] {
        let after = &s[end_off + end_marker.len()..];
        if !after.is_empty() {
            tail.push(FlatPageToken::Static(after));
        }
    }
    tail.extend_from_slice(&tokens[end_idx + 1..]);

    Ok((head, tail))
}

#[cfg(test)]
mod tests_split_static_at_region {
    use super::{FlatPageToken, SplitRegionError, split_static_at_region};

    const BEGIN: &str = "<!-- MARIUS_VOLATILE_BEGIN nav_profile -->";
    const END: &str = "<!-- MARIUS_VOLATILE_END -->";

    /// F.1 — cas nominal, région sur un seul `Static` : head/tail corrects,
    /// région retirée des deux.
    #[test]
    fn well_formed_region_on_a_single_static() {
        let tokens = vec![FlatPageToken::Static(
            "<header>H</header><!-- MARIUS_VOLATILE_BEGIN nav_profile --><li>volatile</li><!-- MARIUS_VOLATILE_END --><footer>F</footer>",
        )];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        assert_eq!(head, vec![FlatPageToken::Static("<header>H</header>")]);
        assert_eq!(tail, vec![FlatPageToken::Static("<footer>F</footer>")]);
    }

    /// F.6 — région à cheval sur PLUSIEURS `Static` (cas réel démontré en
    /// §1 du rapport : un `{# ... #}` intercalé scinde un run HTML sans
    /// qu'aucun autre token ne s'interpose). Simulé ici directement par la
    /// construction de deux `Static` consécutifs — même effet observable
    /// que la scission opérée par le lexer, sans dépendre de celui-ci.
    #[test]
    fn well_formed_region_spanning_multiple_consecutive_statics() {
        let tokens = vec![
            FlatPageToken::Static("<header>H</header><!-- MARIUS_VOLATILE_BEGIN nav_profile -->"),
            FlatPageToken::Static("<li>volatile</li><!-- MARIUS_VOLATILE_END --><footer>F</footer>"),
        ];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        assert_eq!(head, vec![FlatPageToken::Static("<header>H</header>")]);
        assert_eq!(tail, vec![FlatPageToken::Static("<footer>F</footer>")]);
    }

    /// F.5 — région vide (BEGIN immédiatement suivi de END) : head/tail
    /// corrects, aucun token vide introduit.
    #[test]
    fn empty_region_between_adjacent_markers() {
        let tokens = vec![FlatPageToken::Static(
            "<a></a><!-- MARIUS_VOLATILE_BEGIN nav_profile --><!-- MARIUS_VOLATILE_END --><b></b>",
        )];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        assert_eq!(head, vec![FlatPageToken::Static("<a></a>")]);
        assert_eq!(tail, vec![FlatPageToken::Static("<b></b>")]);
    }

    /// F.2 — BEGIN absent : erreur nommée, jamais un split partiel.
    #[test]
    fn missing_begin_marker_is_a_named_error() {
        let tokens = vec![FlatPageToken::Static("<a></a><!-- MARIUS_VOLATILE_END --><b></b>")];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::BeginMarkerNotFound)
        );
    }

    /// F.3 — BEGIN sans END : erreur nommée distincte de BeginMarkerNotFound.
    #[test]
    fn begin_without_end_is_a_named_error() {
        let tokens = vec![FlatPageToken::Static(
            "<a></a><!-- MARIUS_VOLATILE_BEGIN nav_profile --><li>orphan</li>",
        )];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::EndMarkerNotFound)
        );
    }

    /// F.4 — END sans BEGIN : rapporté comme BeginMarkerNotFound (§1 de
    /// l'algorithme — begin_marker cherché en premier, absent ici).
    #[test]
    fn end_without_begin_is_a_named_error() {
        let tokens = vec![FlatPageToken::Static("<a></a><!-- MARIUS_VOLATILE_END --><b></b>")];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::BeginMarkerNotFound)
        );
    }

    /// F.8 (BEGIN) — deux BEGIN dans le flux : ambiguïté jamais résolue
    /// silencieusement (ni « le premier », ni « le dernier »).
    #[test]
    fn duplicate_begin_marker_is_a_named_error() {
        let tokens = vec![FlatPageToken::Static(
            "<!-- MARIUS_VOLATILE_BEGIN nav_profile --><!-- MARIUS_VOLATILE_BEGIN nav_profile --><!-- MARIUS_VOLATILE_END -->",
        )];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::DuplicateBeginMarker)
        );
    }

    /// F.8 (END) — deux END après le BEGIN : même discipline.
    #[test]
    fn duplicate_end_marker_is_a_named_error() {
        let tokens = vec![FlatPageToken::Static(
            "<!-- MARIUS_VOLATILE_BEGIN nav_profile --><!-- MARIUS_VOLATILE_END --><!-- MARIUS_VOLATILE_END -->",
        )];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::DuplicateEndMarker)
        );
    }

    /// F.7 — région traversant un token non-`Static` (`Field` ici) :
    /// erreur nommée, jamais un split qui ignorerait silencieusement le
    /// champ dynamique.
    #[test]
    fn region_spanning_a_non_static_token_is_a_named_error() {
        let tokens = vec![
            FlatPageToken::Static("<!-- MARIUS_VOLATILE_BEGIN nav_profile --><li>"),
            FlatPageToken::Field {
                entity: "record",
                field: "username",
            },
            FlatPageToken::Static("</li><!-- MARIUS_VOLATILE_END -->"),
        ];
        assert_eq!(
            split_static_at_region(tokens, BEGIN, END),
            Err(SplitRegionError::NonStaticTokenInRegion)
        );
    }

    /// F.9/F.10 — conservation exacte : head et tail concaténés au flux
    /// original moins (BEGIN + région + END) reconstituent exactement le
    /// texte statique attendu, aucun octet perdu ni ajouté ailleurs.
    #[test]
    fn head_and_tail_preserve_surrounding_text_exactly() {
        let tokens = vec![FlatPageToken::Static(
            "PREFIX-TEXT<!-- MARIUS_VOLATILE_BEGIN nav_profile -->REMOVED<!-- MARIUS_VOLATILE_END -->SUFFIX-TEXT",
        )];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        assert_eq!(head, vec![FlatPageToken::Static("PREFIX-TEXT")]);
        assert_eq!(tail, vec![FlatPageToken::Static("SUFFIX-TEXT")]);
    }

    /// F.11 — le contenu de la région retirée n'apparaît dans NI head, NI
    /// tail (la sous-chaîne "REMOVED" ne doit survivre nulle part).
    #[test]
    fn removed_region_content_is_never_reinjected_in_either_stream() {
        let tokens = vec![FlatPageToken::Static(
            "before<!-- MARIUS_VOLATILE_BEGIN nav_profile -->REMOVED-CONTENT<!-- MARIUS_VOLATILE_END -->after",
        )];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        for token in head.iter().chain(tail.iter()) {
            if let FlatPageToken::Static(s) = token {
                assert!(!s.contains("REMOVED-CONTENT"));
            }
        }
    }

    /// Tokens avant `begin_idx` et après `end_idx` sont conservés tels
    /// quels, y compris s'ils ne sont pas `Static` (un `Field` avant la
    /// région, par exemple, doit survivre intact dans `head`).
    #[test]
    fn non_static_tokens_outside_the_region_are_preserved() {
        let tokens = vec![
            FlatPageToken::Field {
                entity: "record",
                field: "title",
            },
            FlatPageToken::Static("<!-- MARIUS_VOLATILE_BEGIN nav_profile -->x<!-- MARIUS_VOLATILE_END -->"),
            FlatPageToken::AssetRef("main.css"),
        ];
        let (head, tail) = split_static_at_region(tokens, BEGIN, END).expect("doit réussir");
        assert_eq!(
            head,
            vec![FlatPageToken::Field {
                entity: "record",
                field: "title"
            }]
        );
        assert_eq!(tail, vec![FlatPageToken::AssetRef("main.css")]);
    }
}

#[cfg(test)]
mod tests_split_static_at_marker {
    use super::split_static_at_marker;
    use marius_fragment_forge::FlatPageToken;

    #[test]
    fn marker_embedded_in_larger_static_splits_around_it() {
        let tokens = vec![FlatPageToken::Static(
            "<head><title>x</title><!-- MARIUS_SCRIPTS --></head>",
        )];

        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head><title>x</title>"),
                FlatPageToken::Static("</head>"),
            ]
        );
        assert_eq!(splice_index, 1); // entre les deux moitiés
    }

    /// Pas de `Static("")` mort dans le flux quand le marqueur est en
    /// tout début ou toute fin d'un bloc — voir doc de la fonction.
    #[test]
    fn marker_at_start_omits_empty_before_half() {
        let tokens = vec![FlatPageToken::Static("<!-- MARIUS_SCRIPTS --></head>")];
        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();
        assert_eq!(result, vec![FlatPageToken::Static("</head>")]);
        assert_eq!(splice_index, 0);
    }

    #[test]
    fn marker_at_end_omits_empty_after_half() {
        let tokens = vec![FlatPageToken::Static("<head><!-- MARIUS_SCRIPTS -->")];
        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();
        assert_eq!(result, vec![FlatPageToken::Static("<head>")]);
        assert_eq!(splice_index, 1);
    }

    #[test]
    fn preserves_tokens_before_and_after_the_marked_one() {
        let tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("<title>x</title><!-- MARIUS_SCRIPTS -->"),
            FlatPageToken::Static("</head><body>"),
        ];

        let (result, splice_index) =
            split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").unwrap();

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::Static("<title>x</title>"),
                FlatPageToken::Static("</head><body>"),
            ]
        );
        assert_eq!(splice_index, 2);
    }

    #[test]
    fn marker_absent_returns_none() {
        let tokens = vec![FlatPageToken::Static("<head></head>")];
        assert!(split_static_at_marker(tokens, "<!-- MARIUS_SCRIPTS -->").is_none());
    }
}
