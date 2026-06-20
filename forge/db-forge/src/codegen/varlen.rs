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
        // max_len est Option<usize> depuis ADR-007 (frontière Hot/Cold) :
        // None signifie "pas de borne connue dans le schéma PostgreSQL"
        // (TEXT sans VARCHAR(N) ni CHECK reconnu). Affiché explicitement
        // dans le commentaire généré plutôt que masqué — un mainteneur
        // lisant generated_schema.rs doit voir immédiatement qu'un champ
        // non borné existe, avant même qu'il ne déclenche éventuellement
        // un ResolverError::UnboundedField s'il est référencé par un template.
        let bound_descr = match v.max_len {
            Some(n) => format!("VARCHAR({n})"),
            None    => "TEXT (non borné — Cold sauf si référencé)".to_string(),
        };
        writeln!(out,
            "    /// {} — {} × {}.",
            bound_descr,
            if v.pre_escaped { "pré-échappé, facteur" } else { "escape HTML, facteur" },
            if v.pre_escaped { 1 } else { VarlenField::HTML_ESCAPE_FACTOR },
        ).unwrap();
        writeln!(out, "    pub {}: Option<String>,", v.name).unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
