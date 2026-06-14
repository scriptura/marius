// =============================================================================
// marius-db-forge · registry.rs
//
// Types représentant une entrée du registre meta.containment_intent enrichie
// des métadonnées de JOIN varlena.
//
// Phase 0 : types uniquement — list hardcodée dans build.rs.
// Phase 1 : fetch_component_list() lit meta.containment_intent +
//           meta.component_varlena_join (Pivot 1 — table dédiée, join_slot_idx
//           garantit un ordre déterministe multi-JOIN).
// =============================================================================

/// Configuration complète d'un composant ECS pour la génération.
///
/// Produit par le registre (`meta.containment_intent`) et enrichi
/// de la liaison varlena (`meta.component_varlena_join`).
#[derive(Debug)]
pub struct ComponentConfig {
    pub schema:            String,
    pub table:             String,
    /// meta.containment_intent.intent_density_bytes.
    /// Phase 0 : 0 (non validé). Phase 2 : comparé au layout calculé.
    pub intent_density:    i16,
    /// meta.containment_intent.rls_guard_bitmask.
    pub rls_guard_bitmask: Option<i32>,
    /// Table jointe portant les colonnes varlena.
    /// Phase 0 : hardcodé. Phase 1 : lu depuis meta.component_varlena_join.
    pub varlena_join:      Option<VarlenJoin>,
}

/// Description d'un JOIN varlena pour un composant.
///
/// Correspond à une ligne de meta.component_varlena_join (Pivot 1).
/// `join_slot_idx` garantit l'ordre de lecture en cas de multi-JOIN.
#[derive(Debug)]
pub struct VarlenJoin {
    pub schema: String,
    pub table:  String,
    pub fk_col: String,
}

// Phase 1 : fetch_component_list() sera implémenté ici.
// Il interrogera meta.containment_intent + meta.component_varlena_join
// et retournera Vec<ComponentConfig> trié par component_id.
