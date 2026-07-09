// =============================================================================
// marius-db-forge · codegen/collector.rs
// Génération du Collector<MAX, WORDS> statique.
// =============================================================================

use std::fmt::Write as _;

use crate::naming::to_screaming;

/// Génère le Collector dimensionné pour la table.
///
/// MAX_ENTITY_ID : borne du domaine IDs (calculée par fetch_max_id).
/// WORDS         : nombre de mots u64 = MAX_ENTITY_ID / 64.
///
/// La relation WORDS = MAX / 64 est imposée par la Forge car
/// `generic_const_exprs` est instable en Rust stable.
pub fn write_collector(
    out: &mut String,
    schema: &str,
    table: &str,
    pk_col: &str,
    max_entity_id: usize,
) {
    let screaming = to_screaming(&format!("{schema}_{table}"));
    let words = max_entity_id.div_ceil(64);

    writeln!(out, "// Collector dimensionné pour {schema}.{table}").unwrap();
    writeln!(out, "// PK = {pk_col} | MAX_ID+20% arrondi power-of-two").unwrap();
    writeln!(
        out,
        "pub const MAX_{screaming}_ID: usize = {max_entity_id};"
    )
    .unwrap();
    writeln!(out, "pub const {screaming}_WORDS: usize = {words};").unwrap();
    writeln!(out,
        "pub static {screaming}_COLLECTOR: crate::collector::Collector<MAX_{screaming}_ID, {screaming}_WORDS> ="
    ).unwrap();
    writeln!(out, "    crate::collector::Collector::new_zeroed();\n").unwrap();
}
