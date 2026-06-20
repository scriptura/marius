use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::{FlatPageToken, SchemaIndex, TemplateMetrics};
use crate::generate_aot_snippet;

// ── Erreur ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum OrchestratorError {
    Io(std::io::Error),
}

impl From<std::io::Error> for OrchestratorError {
    #[inline]
    fn from(e: std::io::Error) -> Self {
        OrchestratorError::Io(e)
    }
}

// ── Point d'entrée ────────────────────────────────────────────────────────────

/// Génère `{out_dir}/{table_name}_render.rs`.
///
/// Responsabilités :
///   1. Émettre les trois constantes de capacité (STATIC, DYNAMIC, TOTAL).
///   2. Émettre la signature de fonction + `buf.reserve(PAGE_TOTAL_CAP)`.
///   3. Déléguer le corps du snippet à `generate_aot_snippet`.
///
/// `buf.reserve` est émis ici (pas dans `generate_aot_snippet`) pour référencer
/// la constante symbolique `PAGE_TOTAL_CAP` — cela permet aux tests du fichier
/// généré de valider l'invariant no-realloc sans dépendance sur les valeurs
/// numériques absolues.
///
/// Phase I/O unique autorisée (INV-4).
pub fn orchestrate_generation(
    table_name: &str,
    tokens:     &[FlatPageToken<'_>],
    metrics:    &TemplateMetrics,
    schema:     &SchemaIndex<'_>,
    out_dir:    &str,
) -> Result<(), OrchestratorError> {
    let path = Path::new(out_dir).join(format!("{}_render.rs", table_name));
    let file = File::create(&path)?;
    let mut w = BufWriter::new(file);

    // ── En-tête ───────────────────────────────────────────────────────────────
    writeln!(w, "// Code généré automatiquement par Marius Fragment-Forge. Ne pas modifier.")?;
    writeln!(w)?;

    // ── Constantes de capacité ────────────────────────────────────────────────
    // Trois constantes séparées : STATIC et DYNAMIC sont utiles individuellement
    // (tests de ratio, diagnostics), TOTAL est le seul utilisé dans le hot path.
    writeln!(w, "pub const PAGE_STATIC_CAP:  usize = {};", metrics.total_static_bytes)?;
    writeln!(w, "pub const PAGE_DYNAMIC_CAP: usize = {};", metrics.total_dynamic_bytes)?;
    writeln!(w, "pub const PAGE_TOTAL_CAP:   usize = {};",
        metrics.total_static_bytes + metrics.total_dynamic_bytes)?;
    writeln!(w)?;

    // ── Signature de fonction ─────────────────────────────────────────────────
    writeln!(w, "/// Rendu de la page — zéro allocation si buf pré-alloué à PAGE_TOTAL_CAP.")?;
    writeln!(w, "pub fn render_page(record: &Context, varlena: &VarlenOwned, buf: &mut String) {{")?;

    // ── Reserve initial — cible PAGE_TOTAL_CAP ────────────────────────────────
    // Invariant no-realloc : buf.capacity() ne doit pas augmenter pendant render_page().
    // PAGE_TOTAL_CAP = PAGE_STATIC_CAP + PAGE_DYNAMIC_CAP, borne supérieure exacte.
    writeln!(w, "    buf.reserve(PAGE_TOTAL_CAP);")?;

    // ── Corps généré par generate_aot_snippet ─────────────────────────────────
    let snippet = generate_aot_snippet(tokens, schema);
    for line in snippet.lines() {
        if line.is_empty() {
            writeln!(w)?;
        } else {
            writeln!(w, "    {}", line)?;
        }
    }

    writeln!(w, "}}")?;

    w.flush()?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldSpec, FieldKind};
    use std::fs;

    #[test]
    fn test_orchestrator_output() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("Hello, "),
            FlatPageToken::Field { entity: "record", field: "title" },
        ];

        let fixed = vec![FieldSpec { name: "title".to_string(), kind: FieldKind::I32, attnum: 1 }];
        let schema = SchemaIndex { fixed: &fixed, varlena: &[] };

        let metrics = TemplateMetrics {
            total_static_bytes:  7,   // "Hello, "
            total_dynamic_bytes: 11,  // i32::MIN = 11 chars
            include_count:       0,
        };

        let tmp     = std::env::temp_dir();
        let out_dir = tmp.to_str().expect("temp_dir invalide");

        orchestrate_generation("user", tokens, &metrics, &schema, out_dir)
            .expect("orchestrate_generation a échoué");

        let content = fs::read_to_string(tmp.join("user_render.rs"))
            .expect("fichier généré introuvable");

        assert!(content.contains("pub const PAGE_STATIC_CAP:  usize = 7;"),
            "PAGE_STATIC_CAP incorrecte:\n{content}");
        assert!(content.contains("pub const PAGE_DYNAMIC_CAP: usize = 11;"),
            "PAGE_DYNAMIC_CAP incorrecte:\n{content}");
        assert!(content.contains("pub const PAGE_TOTAL_CAP:   usize = 18;"),
            "PAGE_TOTAL_CAP incorrecte:\n{content}");
        assert!(content.contains("buf.reserve(PAGE_TOTAL_CAP);"),
            "buf.reserve(PAGE_TOTAL_CAP) absent:\n{content}");
        assert!(content.contains("pub fn render_page("),
            "signature render_page absente:\n{content}");
        assert!(content.ends_with("}\n"),
            "accolade fermante manquante:\n{content}");

        let _ = fs::remove_file(tmp.join("user_render.rs"));
    }
}
