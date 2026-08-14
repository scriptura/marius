// crates/forge/db-forge/src/validate.rs
//
//! # marius-db-forge - validate
//!
//! Validation AOT de la densité mémoire (`intent_density_bytes`).
//!
//! Recoupe le layout `#[repr(C)]` calculé depuis `pg_attribute` avec
//! l'empreinte enregistrée dans `meta.containment_intent`.
//!
//! S'exécute dans `build.rs` immédiatement après `fetch_component_list()`.

use crate::mapping::{Column, map_type};
use marius_fragment_forge::VarlenField;

/// Vérifie la correspondance exacte entre le layout `#[repr(C)]` (`pg_attribute`)
/// et la densité ciblée (`meta.containment_intent.intent_density_bytes`).
///
/// ## Formule Header PostgreSQL (Pivot 2)
///
/// Le null bitmap du heap tuple contient un bit par colonne **totale**
/// ($n_{\text{total}} = \text{fixed} + \text{varlena}$), et non uniquement les colonnes fixes.
/// Utiliser $n_{\text{fixed}}$ sous-évaluerait le header et générerait un faux positif bloquant.
///
/// ```text
/// header_bytes = MAXALIGN(8)(23 + ceil(n_total / 8))
///              = ((23 + (n_total + 7) / 8) + 7) / 8 * 8
/// ```
///
/// *Source : `src/include/access/htup_details.h` (`HeapTupleHeaderData`),
/// aligné sur `meta.f_generate_dod_template`.*
///
/// ## Formule Payload
///
/// Somme des `size_bytes` des colonnes fixed-length, alignée au multiple de `max_align`
/// (identique au calcul de `write_store_struct()`).
///
/// ## Validation & Échec
///
/// `computed_total = header_bytes + padded_payload`
///
/// Si `computed_total != intent_density`, retourne une `Err` déclenchant `cargo:error` dans `build.rs`.
///
/// # Arguments
///
/// * `columns` — Colonnes de la table (fixed + varlena) triées par `attnum`.
/// * `intent_density` — Valeur attendue (`meta.containment_intent.intent_density_bytes`).
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

// =============================================================================
// Validation AOT : collision de nom — varlena multi-slot (CONTRAT-implementation-
// multi-slot-varlena.md, Étape 3).
//
// Politique DDL-driven (arbitrage du 22/07/2026) : échec de build explicite,
// jamais de désambiguïsation automatique côté généré. Toute collision de nom
// est une erreur de modélisation SQL à corriger dans le schéma.
//
// Deux vérifications distinctes :
//
//   1. Collision inter-slots : deux VarlenField de tables jointes différentes
//      (même composant) partageant le même nom de colonne.
//
//   2. Collision varlena / colonne propre du composant : un VarlenField dont
//      le nom coïncide avec une colonne de `own_columns` (colonnes propres du
//      composant — fixed-length OU varlena inline, sans distinction). Portée
//      volontairement plus large que « fixed » seul : row.rs génère un champ
//      Rust nommé d'après col.name pour TOUTE colonne du composant, fixed ou
//      varlena directe (non jointe) — une collision ici produirait le même
//      champ dupliqué qu'entre deux slots (cf. row.rs, branches
//      "varlena table principale" / "varlena NULLABLE table principale").
// =============================================================================

/// Vérifie l'absence de collision de nom pour un composant donné.
///
/// `own_columns` : TOUTES les colonnes propres du composant (fixed-length et
/// varlena inline confondues) — pas seulement le sous-ensemble fixed-length
/// filtré ailleurs (`fixed_cols` dans codegen/projection.rs) pour la
/// construction du SELECT. `varlena` : tous les VarlenField assemblés pour ce
/// composant, tous slots confondus (join_slot_idx croissant), avec provenance
/// (ref_schema/ref_table) déjà renseignée — cf. Étape 2.
///
/// Message d'erreur nommant explicitement : le composant, la colonne en
/// conflit, et l'origine des deux occurrences — même niveau d'exigence
/// diagnostique que le garde-fou PK composite déjà en place dans
/// codegen/projection.rs.
pub fn check_no_name_collision(
    component_id: &str,
    own_columns: &[Column],
    varlena: &[VarlenField],
) -> Result<(), String> {
    // Cas 2 : varlena vs colonne propre du composant.
    for v in varlena {
        if let Some(col) = own_columns.iter().find(|c| c.name == v.name) {
            return Err(format!(
                "collision de nom sur «{}» : colonne propre de {component_id} \
                 (type SQL «{}») en conflit avec le champ varlena joint depuis \
                 {}.{}. Renommer l'une des deux colonnes dans le schéma SQL — \
                 aucune désambiguïsation automatique n'est effectuée par le générateur.",
                v.name, col.sql_type, v.ref_schema, v.ref_table
            ));
        }
    }

    // Cas 1 : collision inter-slots (comparaison par paires, O(n²) — au plus
    // quelques champs varlena par composant en pratique, cf. registry.rs).
    for (i, a) in varlena.iter().enumerate() {
        for b in &varlena[i + 1..] {
            if a.name == b.name {
                return Err(format!(
                    "collision de nom sur «{}» dans {component_id} : présent à la fois \
                     dans le join {}.{} et dans le join {}.{}. Renommer l'une des deux \
                     colonnes dans le schéma SQL — aucune désambiguïsation automatique \
                     n'est effectuée par le générateur.",
                    a.name, a.ref_schema, a.ref_table, b.ref_schema, b.ref_table
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::Column;
    use marius_fragment_forge::EscapePolicy;

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

    // ── check_no_name_collision ─────────────────────────────────────────────

    fn varlen(name: &str, ref_schema: &str, ref_table: &str) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            ref_schema: ref_schema.to_string(),
            ref_table: ref_table.to_string(),
            max_len: Some(100),
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    /// Cas nominal (content.core réel, session du 22/07/2026) : aucune
    /// collision entre identity (slug/headline/alternative_headline/
    /// description), body (content) et les colonnes propres de content.core.
    #[test]
    fn accepts_no_collision() {
        let own = vec![
            col("document_id", "int4", true),
            col("author_entity_id", "int4", false),
            col("status", "int2", true),
        ];
        let varlena = vec![
            varlen("slug", "content", "identity"),
            varlen("headline", "content", "identity"),
            varlen("content", "content", "body"),
        ];
        assert!(check_no_name_collision("content.core", &own, &varlena).is_ok());
    }

    /// Cas 1 : collision inter-slots — deux tables jointes différentes
    /// partageant un nom de colonne (jamais rencontré dans le schéma réel à
    /// ce jour — cas synthétique dédié, cf. Contrat Étape 3).
    #[test]
    fn rejects_collision_between_two_slots() {
        let own = vec![col("document_id", "int4", true)];
        let varlena = vec![
            varlen("name", "content", "identity"),
            varlen("name", "content", "body"),
        ];
        let err = check_no_name_collision("content.core", &own, &varlena).unwrap_err();
        assert!(
            err.contains("name"),
            "message doit nommer la colonne : {err}"
        );
        assert!(
            err.contains("content.identity") && err.contains("content.body"),
            "message doit nommer les deux tables sources : {err}"
        );
    }

    /// Cas 2 : collision varlena vs colonne propre du composant (fixed).
    #[test]
    fn rejects_collision_with_own_fixed_column() {
        let own = vec![col("status", "int2", true)];
        let varlena = vec![varlen("status", "content", "identity")];
        let err = check_no_name_collision("content.core", &own, &varlena).unwrap_err();
        assert!(
            err.contains("status"),
            "message doit nommer la colonne : {err}"
        );
        assert!(
            err.contains("content.core") && err.contains("content.identity"),
            "message doit nommer le composant et la table source : {err}"
        );
    }

    /// Cas 2bis : collision varlena vs colonne propre varlena inline (non
    /// jointe) — portée étendue au-delà de fixed_cols (row.rs génère un champ
    /// pour toute colonne propre, fixed ou varlena directe).
    #[test]
    fn rejects_collision_with_own_inline_varlena_column() {
        let own = vec![col("summary", "text", false)];
        let varlena = vec![varlen("summary", "content", "body")];
        let err = check_no_name_collision("content.some_table", &own, &varlena).unwrap_err();
        assert!(
            err.contains("summary"),
            "message doit nommer la colonne : {err}"
        );
    }
}
