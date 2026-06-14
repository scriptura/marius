// =============================================================================
// marius-db-forge · codegen/varlen.rs
// Génération de {Name}VarlenOwned.
// =============================================================================

use std::fmt::Write as _;

use crate::naming::to_pascal;
use marius_fragment_forge::VarlenField;

/// Génère {Name}VarlenOwned : struct possédée portant les données varlena.
///
/// Send + 'static : traverse tokio::spawn et rayon::par_iter sans contrainte.
/// render() reconstruit les &str localement via as_deref() — zéro copie.
///
/// Si aucun varlena : commentaire uniquement (type VarlenOwned = () dans le trait).
pub fn write_varlen_owned_struct(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    varlena: &[VarlenField],
) {
    if varlena.is_empty() {
        writeln!(out,
            "// {schema}.{table} : aucun champ varlena — type VarlenOwned = () dans le trait.\n"
        ).unwrap();
        return;
    }

    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(out,
        "/// Données varlena possédées pour {schema}.{table}.\n\
         /// Send + 'static : traversée tokio::spawn et rayon::par_iter.\n\
         /// render() reconstruit les &str localement via as_deref() — zéro copie."
    ).unwrap();
    writeln!(out, "#[derive(Debug, Default)]").unwrap();
    writeln!(out, "pub struct {name}VarlenOwned {{").unwrap();

    for v in varlena {
        writeln!(out,
            "    /// VARCHAR({}) — {} × {}.",
            v.max_len,
            if v.is_pre_escaped { "pré-échappé, facteur" } else { "escape HTML, facteur" },
            if v.is_pre_escaped { 1 } else { VarlenField::HTML_ESCAPE_FACTOR },
        ).unwrap();
        writeln!(out, "    pub {}: Option<String>,", v.name).unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
