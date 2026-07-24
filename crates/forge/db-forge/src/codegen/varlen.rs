//! marius-db-forge · crates/forge/db-forge/src/codegen/varlen.rs
//! Génération de {Name}VarlenOwned.

use std::fmt::Write as _;

use crate::naming::to_pascal;
use marius_fragment_forge::{EscapePolicy, VarlenField};

/// Génère {Name}VarlenOwned : struct possédée portant les données varlena.
///
/// Send + 'static : traverse tokio::spawn et rayon::par_iter sans contrainte.
/// render() reconstruit les &str localement via as_deref() — zéro copie.
///
/// Si aucun varlena : commentaire uniquement (type VarlenOwned = () dans le trait).
pub fn write_varlen_owned_struct(
    out: &mut String,
    schema: &str,
    table: &str,
    varlena: &[VarlenField],
) {
    if varlena.is_empty() {
        writeln!(
            out,
            "// {schema}.{table} : aucun champ varlena — type VarlenOwned = () dans le trait.\n"
        )
        .unwrap();
        return;
    }

    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(
        out,
        "/// Données varlena possédées pour {schema}.{table}.\n\
         /// Send + 'static : traversée tokio::spawn et rayon::par_iter.\n\
         /// render() reconstruit les &str localement via as_deref() — zéro copie."
    )
    .unwrap();
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
            None => "TEXT (non borné — Cold sauf si référencé)".to_string(),
        };
        // CONTRAT-implementation-varlena-raw.md, Étape 5 : match exhaustif —
        // Raw distinct de PreEscaped dans le commentaire généré (même facteur
        // de capacité 1, mais Raw n'est JAMAIS échappé au runtime, alors que
        // PreEscaped l'est quand même par défense en profondeur).
        let (escape_descr, escape_factor) = match v.escape_policy {
            EscapePolicy::Escaped => ("escape HTML, facteur", VarlenField::HTML_ESCAPE_FACTOR),
            EscapePolicy::PreEscaped => ("pré-échappé (échappé quand même), facteur", 1),
            EscapePolicy::Raw => ("brut — HTML pré-rendu, jamais échappé, facteur", 1),
        };
        writeln!(
            out,
            "    /// {} — {} × {}.",
            bound_descr, escape_descr, escape_factor,
        )
        .unwrap();
        // CONTRAT-implementation-projection-segmentee.md, Étape 5 : mention
        // explicite quand ce champ ne sera jamais concaténé dans le buffer
        // partagé — un mainteneur lisant generated_schema.rs doit voir
        // immédiatement pourquoi ce champ, potentiellement volumineux,
        // n'apparaît dans aucun calcul de DYNAMIC_CAP.
        if v.is_segment {
            writeln!(
                out,
                "    /// Segment autonome (marius:large_content) — jamais concaténé \
                 dans buf, ne dimensionne jamais la capacité totale du composant."
            )
            .unwrap();
        }
        writeln!(out, "    pub {}: Option<String>,", v.name).unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
