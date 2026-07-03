// =============================================================================
// marius-db-forge · validate.rs
//
// Validation AOT : layout calculé depuis pg_attribute vs intent_density_bytes
// enregistré dans meta.containment_intent.
//
// Phase 0 : présent, non appelé (intent_density = 0 dans la liste hardcodée).
// Phase 2 : appelé dans build.rs après fetch_component_list().
// =============================================================================

use crate::mapping::{Column, map_type};

/// Vérifie que le layout `#[repr(C)]` calculé depuis `pg_attribute`
/// correspond à `intent_density_bytes` enregistré dans `meta.containment_intent`.
///
/// ─── Formule header PostgreSQL (Pivot 2) ─────────────────────────────────────
///
///   Le null bitmap du heap tuple contient un bit par colonne **totale**
///   (fixed-length + varlena), pas seulement par colonne fixed.
///   Utiliser n_fixed sous-évalue le header et produit un faux positif bloquant.
///
///   header_bytes = MAXALIGN(8)(23 + ceil(n_total / 8))
///               = ((23 + (n_total + 7) / 8) + 7) / 8 * 8
///
///   Source : src/include/access/htup_details.h (HeapTupleHeaderData).
///   Cohérent avec meta.f_generate_dod_template qui utilise n_total.
///
/// ─── Formule payload ─────────────────────────────────────────────────────────
///
///   Somme des size_bytes des colonnes fixed-length, padded au multiple de
///   max_align — identique au calcul de write_store_struct().
///
/// ─── Comparaison ─────────────────────────────────────────────────────────────
///
///   computed_total = header_bytes + padded_payload
///   si computed_total != intent_density → Err (→ cargo:error dans build.rs)
///
/// # Arguments
///
/// * `columns`        — toutes les colonnes de la table (fixed + varlena), triées par attnum.
/// * `intent_density` — meta.containment_intent.intent_density_bytes.
pub fn validate_layout(columns: &[Column], intent_density: i16) -> Result<(), String> {
    // n_total : toutes les colonnes du heap tuple (fixed + varlena, hors systèmes).
    let n_total = columns.len();

    // Header PostgreSQL : MAXALIGN(8)(23 + ceil(n_total / 8)).
    let header_bytes = (23 + n_total.div_ceil(8)).div_ceil(8) * 8;

    // Payload : somme des colonnes fixed-length, paddée à max_align.
    let mut payload_bytes = 0usize;
    let mut max_align = 1usize;
    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            payload_bytes += m.size_bytes;
            max_align = max_align.max(m.alignment);
        }
    }
    let padded_payload = payload_bytes.div_ceil(max_align.max(1)) * max_align.max(1);
    let computed_total = header_bytes + padded_payload;

    if computed_total != intent_density as usize {
        return Err(format!(
            "layout diverge du registre. \
             Calculé={computed_total}B (header={header_bytes}B + payload={padded_payload}B), \
             Enregistré={intent_density}B. \
             Relancer meta.f_generate_dod_template et mettre à jour meta.containment_intent.",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::Column;

    fn col(name: &str, sql_type: &str, is_notnull: bool) -> Column {
        Column {
            attnum: 1,
            name: name.to_string(),
            sql_type: sql_type.to_string(),
            is_notnull,
            sentinel: None,
        }
    }

    /// Cas nominal : layout cohérent avec intent_density.
    /// content.core-like : 3×i64 + 2×i32 + i16 + 3×bool = 24B + 8B header (3 cols/8 → 1B, MAXALIGN→8B)
    /// Ici test minimal : 1 colonne i64 NOT NULL.
    /// Header = MAXALIGN(23 + 1) = MAXALIGN(24) = 24B.
    /// Payload = 8B. Total = 32B.
    #[test]
    fn validates_correct_layout() {
        let columns = vec![col("id", "int8", true)];
        // header = ((23 + 1) + 7) / 8 * 8 = 24
        // payload = 8B, max_align = 8 → padded = 8
        // total = 32
        assert!(validate_layout(&columns, 32).is_ok());
    }

    /// Cas divergence : intent_density incorrect → Err.
    #[test]
    fn rejects_divergent_layout() {
        let columns = vec![col("id", "int8", true)];
        let result = validate_layout(&columns, 99);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("diverge du registre"));
    }

    /// Pivot 2 : vérifier que les colonnes varlena contribuent au header.
    /// 1×i64 + 1×text = 2 colonnes totales.
    /// Header = MAXALIGN(23 + 1) = 24B (ceil(2/8)=1, 23+1=24, MAXALIGN→24).
    /// Payload fixed = 8B. Total = 32B.
    #[test]
    fn header_includes_varlena_in_null_bitmap() {
        let columns = vec![
            col("id", "int8", true),
            col("label", "text", false), // varlena — contribue au null bitmap
        ];
        // n_total = 2, header = ((23 + 1) + 7) / 8 * 8 = 24
        // payload = 8, total = 32
        assert!(validate_layout(&columns, 32).is_ok());
    }

    /// Cas table sans aucune colonne fixed-length (uniquement varlena).
    /// payload = 0B, max_align = 1 → padded = 0B.
    /// n_total = 1 → header = MAXALIGN(23 + 1) = 24B. Total = 24B.
    #[test]
    fn zero_fixed_columns_all_varlena() {
        let columns = vec![col("slug", "text", false)];
        assert!(validate_layout(&columns, 24).is_ok());
    }

    /// Cas table vide (zéro colonnes).
    /// n_total = 0 → header = MAXALIGN(ceil(23/8) × 8) = MAXALIGN(24) = 24B.
    /// payload = 0B. Total = 24B.
    #[test]
    fn zero_columns_layout() {
        let columns: Vec<Column> = vec![];
        assert!(validate_layout(&columns, 24).is_ok());
    }

    /// Message d'erreur contient les valeurs calculé/enregistré (débogage).
    #[test]
    fn error_message_contains_computed_and_registered() {
        let columns = vec![col("id", "int8", true)];
        let err = validate_layout(&columns, 99).unwrap_err();
        assert!(
            err.contains("32"),
            "message doit mentionner la valeur calculée (32B)"
        );
        assert!(
            err.contains("99"),
            "message doit mentionner la valeur enregistrée (99B)"
        );
        assert!(
            err.contains("diverge"),
            "message doit mentionner la divergence"
        );
    }
}
