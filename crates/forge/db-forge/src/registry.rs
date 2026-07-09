// =============================================================================
// marius-db-forge · registry.rs
//
// Phase 1 : fetch_component_list() lit meta.containment_intent +
//           meta.component_varlena_join (Pivot 1).
//           Ordre : component_id ASC (déterministe), join_slot_idx = 0 (Phase 1).
// =============================================================================

use sqlx::Row as _; // try_get sur PgRow

// ── Types publics ─────────────────────────────────────────────────────────────

/// Configuration complète d'un composant ECS pour la génération.
///
/// Produit par `meta.containment_intent`, enrichi de `meta.component_varlena_join`.
#[derive(Debug)]
pub struct ComponentConfig {
    pub schema: String,
    pub table: String,
    /// Phase 0/1 : 0 (non validé). Phase 2 : comparé au layout calculé par validate_layout().
    pub intent_density: i16,
    pub rls_guard_bitmask: Option<i32>,
    /// Phase 1 : au plus un JOIN varlena par composant (join_slot_idx = 0).
    pub varlena_join: Option<VarlenJoin>,
}

/// Liaison varlena d'un composant — correspond à une ligne de meta.component_varlena_join.
#[derive(Debug)]
pub struct VarlenJoin {
    pub schema: String,
    pub table: String,
    pub fk_col: String,
}

// ── Erreur locale ─────────────────────────────────────────────────────────────

/// Wrappeur d'erreur pour sqlx::Error::Decode — évite une dépendance externe.
#[derive(Debug)]
struct RegistryError(String);

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RegistryError {}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Décode «schema.table» → (schema, table).
/// Invariant : les deux segments doivent être non-vides.
fn parse_component_id(id: &str) -> Result<(String, String), sqlx::Error> {
    let mut it = id.splitn(2, '.');
    match (it.next(), it.next()) {
        (Some(s), Some(t)) if !s.is_empty() && !t.is_empty() => Ok((s.to_string(), t.to_string())),
        _ => Err(sqlx::Error::Decode(Box::new(RegistryError(format!(
            "component_id invalide : «{id}» — attendu «schema.table»"
        ))))),
    }
}

// ── API publique Phase 1 ──────────────────────────────────────────────────────

/// Lit `meta.containment_intent` + `meta.component_varlena_join` et retourne
/// la liste des composants triée par `component_id ASC`.
///
/// Contraintes respectées :
/// - `join_slot_idx = 0` : seul le premier slot varlena est chargé (Phase 1).
/// - Tri `component_id ASC` : ordre déterministe garanti côté SQL, pas côté Rust.
/// - Aucun panic : toutes les erreurs (réseau, décodage, format) sont propagées
///   via `sqlx::Error` pour être interceptées par le pipeline Cargo (INV-3).
pub async fn fetch_component_list(
    pool: &sqlx::PgPool,
) -> Result<Vec<ComponentConfig>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT
            ci.component_id,
            ci.intent_density_bytes,
            ci.rls_guard_bitmask,
            cvj.ref_schema,
            cvj.ref_table,
            cvj.fk_column
        FROM  meta.containment_intent          ci
        LEFT  JOIN meta.component_varlena_join cvj
               ON  cvj.component_id   = ci.component_id
              AND  cvj.join_slot_idx  = 0       -- Phase 1 : slot unique
        ORDER BY ci.component_id ASC            -- déterminisme O(1) INV-8
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut configs = Vec::with_capacity(rows.len());

    for row in &rows {
        let component_id: String = row.try_get("component_id")?;
        let intent_density: i16 = row.try_get("intent_density_bytes")?;
        let rls_guard_bitmask: Option<i32> = row.try_get("rls_guard_bitmask")?;
        let ref_schema: Option<String> = row.try_get("ref_schema")?;
        let ref_table: Option<String> = row.try_get("ref_table")?;
        let fk_column: Option<String> = row.try_get("fk_column")?;

        let (schema, table) = parse_component_id(&component_id)?;

        // Les trois colonnes LEFT JOIN sont soit toutes présentes, soit toutes NULL.
        // Tout autre état indique une incohérence de registre — propagé comme decode error.
        let varlena_join = match (ref_schema, ref_table, fk_column) {
            (Some(s), Some(t), Some(f)) => Some(VarlenJoin {
                schema: s,
                table: t,
                fk_col: f,
            }),
            (None, None, None) => None,
            _ => {
                return Err(sqlx::Error::Decode(Box::new(RegistryError(format!(
                    "meta.component_varlena_join incohérente pour «{component_id}» \
                     — ref_schema/ref_table/fk_column partiellement NULL"
                )))));
            }
        };

        configs.push(ComponentConfig {
            schema,
            table,
            intent_density,
            rls_guard_bitmask,
            varlena_join,
        });
    }

    Ok(configs)
}
