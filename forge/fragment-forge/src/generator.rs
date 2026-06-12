use std::collections::HashMap;

use crate::body::{FieldSpec, VarlenField, generate_sequential_body};
use crate::prologue::generate_render_prologue;
use crate::FlatPageToken;

// ── Épilogue ──────────────────────────────────────────────────────────────────

/// Émet l'accolade fermante de `render_page`.
/// Périmètre strict : un seul token textuel, aucune logique.
#[inline]
fn generate_render_epilogue() -> &'static str {
    "}\n"
}

// ── Point d'entrée du pipeline ────────────────────────────────────────────────

/// Assemble le fichier Rust complet de `render_page` :
///   Prologue (signature + reserve) · Corps séquentiel · Épilogue (accolade).
///
/// Allocation unique : `String::with_capacity` dimensionné sur les trois
/// segments. Aucune allocation intermédiaire — `push_str` étend en place.
pub fn generate_aot_snippet(
    record_type: &str,
    varlena_type: &str,
    total_cap_ident: &str,
    flat: &[FlatPageToken<'_>],
    fields: &[FieldSpec<'_>],
    varlena: &[VarlenField<'_>],
    static_idents: &HashMap<String, String>,
) -> String {
    let prologue = generate_render_prologue(record_type, varlena_type, total_cap_ident);
    let body = generate_sequential_body(flat, fields, varlena, static_idents);
    let epilogue = generate_render_epilogue();

    let mut code = String::with_capacity(
        prologue.len() + body.len() + epilogue.len()
    );
    code.push_str(&prologue);
    code.push_str(&body);
    code.push_str(epilogue);
    code
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assembly_pipeline() {
        // ── Fixtures — AST minimal : un seul Static ───────────────────────────
        let flat: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<html></html>"),
        ];

        let expected_prologue = concat!(
            "fn render_page(record: &PageRow, varlena: &PageVarlen, buf: &mut String) {\n",
            "    buf.reserve(PAGE_TOTAL_CAP);\n",
        );
        let expected_epilogue = "}\n";

        // ── Exécution ─────────────────────────────────────────────────────────
        let output = generate_aot_snippet(
            "PageRow",
            "PageVarlen",
            "PAGE_TOTAL_CAP",
            flat,
            &[],
            &[],
            &HashMap::new(),
        );

        // ── Assertions structurelles ──────────────────────────────────────────
        assert!(
            output.starts_with(expected_prologue),
            "prologue absent ou incorrect:\n{output}"
        );
        assert!(
            output.ends_with(expected_epilogue),
            "épilogue absent ou incorrect:\n{output}"
        );

        // Assertion d'ordre : le corps se trouve entre prologue et épilogue.
        // On vérifie que l'instruction Static est bien présente et intercalée.
        let body_start = expected_prologue.len();
        let body_end = output.len() - expected_epilogue.len();
        let body = &output[body_start..body_end];
        assert!(
            body.contains("buf.push_str(\"<html></html>\");"),
            "corps absent ou mal positionné:\n{body}"
        );
    }
}
