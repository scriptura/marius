// crates/forge/db-forge/src/registry.rs

//! # marius-db-forge - registry
//!
//! `fetch_component_list()` lit `meta.containment_intent` + `meta.component_varlena_join`
//! (Pivot 1). Ordre : `component_id ASC, join_slot_idx ASC` (déterminisme **INV-8**).
//!
//! ## Révision (`CONTRAT-implementation-multi-slot-varlena.md`, Étape 1)
//!
//! La limite Phase 1 (« `join_slot_idx = 0` : seul le premier slot est chargé ») est retirée.  
//! `ComponentConfig.varlena_join` passe de `Option<VarlenJoin>` à `Vec<VarlenJoin>` —
//! un composant peut désormais porter 0..N joins varlena, dans l'ordre
//! `join_slot_idx` croissant.
//!
//! Ce changement casse la compilation de `build.rs`
//! (`match &comp.varlena_join { Some(j) => ... }`, lignes 1767/1817) —
//! attendu, non corrigé ici : c'est la portée de l'Étape 4 du
//! même Contrat. Cette étape est testée en isolation, dans ce crate seul.

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
    /// 0..N joins varlena, triés par join_slot_idx ASC (INV-8). Vide = aucun join.
    pub varlena_join: Vec<VarlenJoin>,
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

// ── API publique ──────────────────────────────────────────────────────────────

/// Lit `meta.containment_intent` + `meta.component_varlena_join` et retourne
/// la liste des composants triée par `component_id ASC`, chaque composant
/// portant ses joins varlena triés par `join_slot_idx ASC`.
///
/// Contraintes respectées :
/// - Aucune limitation de cardinalité : un composant porte 0..N joins varlena
///   (le `LEFT JOIN` produit 0 ou 1 ligne par slot déclaré dans le registre —
///   0 join → exactement une ligne avec cvj.* NULL, N joins → N lignes).
/// - Tri `component_id ASC, join_slot_idx ASC` : ordre déterministe garanti
///   côté SQL, pas côté Rust — le regroupement par composant ci-dessous
///   suppose des lignes déjà contiguës et déjà triées par slot.
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
        ORDER BY ci.component_id ASC, cvj.join_slot_idx ASC   -- déterminisme INV-8
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut configs: Vec<ComponentConfig> = Vec::new();
    let mut current_component_id: Option<String> = None;

    for row in &rows {
        let component_id: String = row.try_get("component_id")?;
        let intent_density: i16 = row.try_get("intent_density_bytes")?;
        let rls_guard_bitmask: Option<i32> = row.try_get("rls_guard_bitmask")?;
        let ref_schema: Option<String> = row.try_get("ref_schema")?;
        let ref_table: Option<String> = row.try_get("ref_table")?;
        let fk_column: Option<String> = row.try_get("fk_column")?;

        // Les trois colonnes LEFT JOIN sont soit toutes présentes, soit toutes NULL.
        // Tout autre état indique une incohérence de registre — propagé comme decode error.
        let join = match (ref_schema, ref_table, fk_column) {
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

        let same_component_as_previous =
            current_component_id.as_deref() == Some(component_id.as_str());

        if same_component_as_previous {
            // Ligne supplémentaire du même composant (slot suivant) — le tri SQL
            // garantit sa contiguïté avec les lignes précédentes de ce composant.
            let last = configs
                .last_mut()
                .expect("current_component_id posé implique au moins une entrée dans configs");
            if let Some(j) = join {
                last.varlena_join.push(j);
            }
            // join == None ici ne devrait jamais arriver pour une ligne de
            // continuation (un composant avec 0 join produit une unique ligne,
            // jamais suivie d'une seconde) — mais si `meta.component_varlena_join`
            // contenait un doublon exact (component_id, join_slot_idx), le
            // JOIN SQL ne le produirait qu'une fois de toute façon (PK sur
            // (component_id, join_slot_idx)) : aucun état incohérent possible ici.
        } else {
            let (schema, table) = parse_component_id(&component_id)?;
            let mut cfg = ComponentConfig {
                schema,
                table,
                intent_density,
                rls_guard_bitmask,
                varlena_join: Vec::new(),
            };
            if let Some(j) = join {
                cfg.varlena_join.push(j);
            }
            configs.push(cfg);
            current_component_id = Some(component_id);
        }
    }

    Ok(configs)
}

#[cfg(test)]
mod integration {
    use super::*;

    async fn connect() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL requis pour les tests d'intégration");
        sqlx::PgPool::connect(&url)
            .await
            .expect("Connexion PgPool échouée")
    }

    /// content.core porte désormais 2 slots varlena (migration
    /// 03_content_body_varlena_join_registration.sql, session du 22/07/2026) :
    /// slot 0 → content.identity, slot 1 → content.body. Régression directe du
    /// retrait du filtre `join_slot_idx = 0` (Étape 1 du Contrat multi-slot) —
    /// avant cette révision, seul le slot 0 aurait été retourné.
    #[tokio::test]
    #[ignore]
    async fn content_core_has_two_varlena_joins_in_slot_order() {
        let pool = connect().await;
        let comps = fetch_component_list(&pool)
            .await
            .expect("fetch_component_list échoué");

        let core = comps
            .iter()
            .find(|c| c.schema == "content" && c.table == "core")
            .expect("content.core absent de meta.containment_intent");

        assert_eq!(
            core.varlena_join.len(),
            2,
            "content.core devrait porter 2 joins varlena (slots 0 et 1), trouvé : {:?}",
            core.varlena_join
        );

        assert_eq!(core.varlena_join[0].schema, "content");
        assert_eq!(core.varlena_join[0].table, "identity");
        assert_eq!(core.varlena_join[0].fk_col, "document_id");

        assert_eq!(core.varlena_join[1].schema, "content");
        assert_eq!(core.varlena_join[1].table, "body");
        assert_eq!(core.varlena_join[1].fk_col, "document_id");
    }

    /// content.document est déclaré dans meta.containment_intent (seed
    /// 10_meta_seed/01_manifest.sql) mais ne porte aucune ligne dans
    /// meta.component_varlena_join — Vec vide attendu, pas None : vérifie que
    /// le changement de signature (Option → Vec) ne réintroduit pas d'ambiguïté
    /// sur le cas « zéro join ».
    #[tokio::test]
    #[ignore]
    async fn content_document_has_no_varlena_join() {
        let pool = connect().await;
        let comps = fetch_component_list(&pool)
            .await
            .expect("fetch_component_list échoué");

        let document = comps
            .iter()
            .find(|c| c.schema == "content" && c.table == "document")
            .expect("content.document absent de meta.containment_intent");

        assert!(
            document.varlena_join.is_empty(),
            "content.document ne devrait porter aucun join varlena, trouvé : {:?}",
            document.varlena_join
        );
    }

    /// Non-régression du déterminisme O(1) INV-8 : la liste reste triée
    /// component_id ASC malgré le passage d'une à N lignes possibles par
    /// composant (le regroupement Rust ne doit pas réordonner).
    #[tokio::test]
    #[ignore]
    async fn component_list_sorted_by_component_id() {
        let pool = connect().await;
        let comps = fetch_component_list(&pool)
            .await
            .expect("fetch_component_list échoué");

        for w in comps.windows(2) {
            let id_a = format!("{}.{}", w[0].schema, w[0].table);
            let id_b = format!("{}.{}", w[1].schema, w[1].table);
            assert!(
                id_a < id_b,
                "Ordre component_id ASC violé : {id_a} >= {id_b}"
            );
        }
    }

    // Cas non testé ici, délibérément : incohérence ref_schema/ref_table/
    // fk_column partiellement NULL. Structurellement inatteignable via le
    // schéma réel — les trois colonnes de meta.component_varlena_join sont
    // NOT NULL (01_meta/01_tables.sql), donc un LEFT JOIN ne peut produire
    // que « toutes NULL » (pas de ligne correspondante) ou « toutes non-NULL »
    // (ligne présente), jamais un état intermédiaire. La branche `_ => Err(...)`
    // reste une défense en profondeur contre une future modification du
    // registre (colonnes NULLABLE ajoutées), pas un cas exerçable aujourd'hui —
    // non simulé par un test artificiel plutôt que d'inventer un état que le
    // schéma actuel interdit structurellement.
}
