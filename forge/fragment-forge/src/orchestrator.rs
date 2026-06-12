use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::{FlatPageToken, TemplateMetrics};
use crate::generate_aot_snippet;

// ── Erreur ───────────────────────────────────────────────────────────────────

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

// ── Point d'entrée ───────────────────────────────────────────────────────────

/// Génère `{out_dir}/{table_name}_render.rs`.
///
/// Contrat vis-à-vis de Phase 2.2 :
/// `generate_aot_snippet` ne doit PAS émettre la ligne `buf.reserve(...)`.
/// L'orchestrateur l'émet lui-même en référençant `PAGE_STATIC_CAP`
/// pour que le fichier généré soit auto-cohérent.
/// Phase I/O unique autorisée (INV-4).
pub fn orchestrate_generation(
    table_name: &str,
    tokens: &[FlatPageToken<'_>],
    metrics: &TemplateMetrics,
    out_dir: &str,
) -> Result<(), OrchestratorError> {
    let path = Path::new(out_dir).join(format!("{}_render.rs", table_name));
    let file = File::create(&path)?;
    let mut w = BufWriter::new(file);

    // ── En-tête ──────────────────────────────────────────────────────────────
    writeln!(
        w,
        "// Code généré automatiquement par Marius Fragment-Forge. Ne pas modifier."
    )?;
    writeln!(w)?;

    // ── Constante statique ───────────────────────────────────────────────────
    writeln!(
        w,
        "pub const PAGE_STATIC_CAP: usize = {};",
        metrics.total_static_bytes
    )?;
    writeln!(w)?;

    // ── Signature de fonction ────────────────────────────────────────────────
    writeln!(w, "/// Rendu de la page.")?;
    writeln!(
        w,
        "/// Hypothèse : `record` fournit les champs nécessaires. `buf` est pré-alloué."
    )?;
    writeln!(w, "pub fn render_page(record: &Context, buf: &mut String) {{")?;

    // ── Reserve initial — référence la constante du même fichier ─────────────
    writeln!(w, "    buf.reserve(PAGE_STATIC_CAP);")?;

    // ── Snippet Phase 2.2 (corps sans la ligne reserve) ──────────────────────
    // generate_aot_snippet produit des lignes à indentation 0 (hors if)
    // ou 4 espaces (corps du if). On ajoute 4 espaces pour le niveau fonction.
    let snippet = generate_aot_snippet(tokens, metrics);
    for line in snippet.lines() {
        if line.is_empty() {
            writeln!(w)?;
        } else {
            writeln!(w, "    {}", line)?;
        }
    }

    writeln!(w, "}}")?;

    // Flush explicite : propagation d'erreur avant drop du BufWriter.
    w.flush()?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Tokens minimaux : 1 statique + 1 champ dynamique.
    /// Suffisant pour valider la structure du fichier généré
    /// sans dépendre du comportement complet de Phase 2.2.
    #[test]
    fn test_orchestrator_output() {
        // ── Fixtures ─────────────────────────────────────────────────────────
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("Hello, "),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
        ];
        let metrics = TemplateMetrics {
            total_static_bytes: 7, // "Hello, "
            include_count: 0,
        };

        // ── Répertoire de sortie : std::env::temp_dir() ──────────────────────
        // Pas de dépendance externe (tempfile crate non autorisé sur ce projet).
        let tmp = std::env::temp_dir();
        let out_dir = tmp.to_str().expect("temp_dir invalide (non-UTF8)");

        // ── Exécution ────────────────────────────────────────────────────────
        orchestrate_generation("user", tokens, &metrics, out_dir)
            .expect("orchestrate_generation a échoué");

        // ── Lecture et assertions structurelles ──────────────────────────────
        let out_path = tmp.join("user_render.rs");
        let content = fs::read_to_string(&out_path)
            .expect("fichier généré introuvable");

        // Constante présente et valide
        assert!(
            content.contains("pub const PAGE_STATIC_CAP: usize = 7;"),
            "PAGE_STATIC_CAP manquante ou incorrecte:\n{content}"
        );

        // Signature de fonction présente
        assert!(
            content.contains("pub fn render_page(record: &Context, buf: &mut String)"),
            "signature render_page manquante:\n{content}"
        );

        // Reserve initial référençant la constante
        assert!(
            content.contains("buf.reserve(PAGE_STATIC_CAP);"),
            "buf.reserve(PAGE_STATIC_CAP) absent:\n{content}"
        );

        // Accolade fermante de fonction présente (validité syntaxique minimale)
        assert!(
            content.ends_with("}\n"),
            "accolade fermante manquante:\n{content}"
        );

        // ── Nettoyage ────────────────────────────────────────────────────────
        let _ = fs::remove_file(&out_path);
    }
}