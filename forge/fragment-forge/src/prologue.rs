/// Émet la signature de `render_page` et l'instruction `reserve` initiale.
///
/// Contrat : les identifiants sont passés résolus (aucune transformation de casse).
/// L'accolade ouvrante est incluse. L'accolade fermante est hors périmètre.
pub fn generate_render_prologue(
    record_type: &str,
    varlena_type: &str,
    total_cap_ident: &str,
) -> String {
    format!(
        "fn render_page(record: &{record_type}, varlena: &{varlena_type}, buf: &mut String) {{\n    buf.reserve({total_cap_ident});\n"
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_render_prologue() {
        let output = generate_render_prologue(
            "ArticleRow",
            "ArticleVarlen",
            "ARTICLE_TOTAL_CAP",
        );

        let expected = concat!(
            "fn render_page(record: &ArticleRow, varlena: &ArticleVarlen, buf: &mut String) {\n",
            "    buf.reserve(ARTICLE_TOTAL_CAP);\n",
        );

        assert_eq!(output, expected);
    }
}
