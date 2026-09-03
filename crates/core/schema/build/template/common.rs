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
