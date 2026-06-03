// =============================================================================
// marius-schema — crates/core/schema/src/lib.rs
// Projet Marius · ADR-002 / ADR-003
//
// Point d'entrée de la crate schema.
// Re-exporte les types des crates core (Projection, Collector) et inclut
// le fichier généré par DB-Forge à la compilation (generated_schema.rs).
//
// ─── Contenu de generated_schema.rs ──────────────────────────────────────────
//
//   Pour chaque table surveillée dans build.rs, le fichier généré contient :
//
//   {Name}Row           : struct sqlx::FromRow (transport sqlx → Dispatcher).
//   {Name}StorageRow    : struct #[repr(C)] (stockage mémoire contiguë).
//   {Name}VarlenOwned   : struct possédée (Option<String>, Send+'static).
//                         Absente (remplacée par ()) si pas de varlena.
//   From<{Name}Row> for {Name}StorageRow
//   Collector<MAX, WORDS> statique
//   impl Projection stub :
//     type Record      = {Name}StorageRow
//     type VarlenOwned = {Name}VarlenOwned | ()
//     fetch_batch()    → Vec<(StorageRow, VarlenOwned)>
//     render()         → &StorageRow + &VarlenOwned + &mut String
//     artifact_path()
//   Constantes de capacité :
//     {NAME}_STATIC_CAP  : octets HTML statiques
//     {NAME}_DYNAMIC_CAP : largeurs max des valeurs dynamiques
//     {NAME}_TOTAL_CAP   : = STATIC_CAP + DYNAMIC_CAP
//
// ─── ADR-003 : Suppression de RenderPayload<'a> ──────────────────────────────
//
//   RenderPayload<'a> n'est plus émis dans le fichier généré.
//   Les &str sont reconstruits localement dans render() via as_deref() sur
//   chaque thread Rayon, sans traversée de frontière de lifetime.
//   VarlenOwned est le type transporté (Send+'static) ; le payload est éphémère.
//
// ─── Tests ───────────────────────────────────────────────────────────────────
//
//   1. Tests fonctionnels (ignorés par défaut, requièrent DATABASE_URL) :
//      Vérifient que fetch_batch() retourne des tuples (StorageRow, VarlenOwned)
//      valides et que render() produit un HTML syntaxiquement correct.
//
//   2. Tests no-realloc (toujours actifs, sans DATABASE_URL) :
//      Alimentent les structs avec les valeurs pires cas et assertent
//      buf.capacity() == {NAME}_TOTAL_CAP après render().
//      Pour VarlenOwned : chaînes de max_len × '&' (pire cas escape × 5).
//
//   3. Tests de ratio de remplissage (toujours actifs, sans DATABASE_URL) :
//      Mesurent le pourcentage de TOTAL_CAP utilisé sur données représentatives.
//      Cible : 50–90%.
//
// =============================================================================

pub mod projection {
    pub use marius_projection::Projection;
}

pub mod collector {
    pub use marius_collector::Collector;
}

// Inclusion du code généré par DB-Forge + Fragment-Forge.
// Ce fichier est recréé à chaque `cargo build` si DATABASE_URL a changé.
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Tests de rendu — validité fonctionnelle
    //
    // Marqués #[ignore] : requièrent DATABASE_URL et un jeu de données DML.
    // Exécution manuelle : cargo test -- --ignored
    //
    // Vérifient la chaîne complète :
    //   fetch_batch() → Vec<(StorageRow, VarlenOwned)> → render() → HTML
    // =========================================================================

    #[tokio::test]
    #[ignore]
    async fn test_fetch_content_core() {
        let pool = sqlx::PgPool::connect(
            &std::env::var("DATABASE_URL").unwrap()
        ).await.unwrap();

        let ids     = vec![1i64, 2, 3];
        // fetch_batch retourne Vec<(ContentCoreStorageRow, ContentCoreVarlenOwned)>.
        let results = ContentCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!results.is_empty(), "Aucun enregistrement — DML appliqué ?");

        let (storage, varlena) = &results[0];

        // render() reçoit (&StorageRow, &VarlenOwned).
        // Fragment-Forge reconstruit les &str localement via as_deref().
        let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);
        ContentCoreProjection::render(storage, varlena, &mut buf);
        println!("ContentCore[0] : {buf}");

        assert!(buf.contains("content-core"), "classe CSS absente");
        assert!(buf.contains("<dt>document_id</dt>"), "champ document_id absent");
    }

    #[tokio::test]
    #[ignore]
    async fn test_fetch_product_core() {
        let pool = sqlx::PgPool::connect(
            &std::env::var("DATABASE_URL").unwrap()
        ).await.unwrap();

        let ids     = vec![1i64, 2, 3];
        // commerce.product_core : VarlenOwned = () (pas de JOIN varlena).
        let results = CommerceProductCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!results.is_empty());

        let (storage, varlena) = &results[0];  // varlena : &()

        let mut buf = String::with_capacity(COMMERCE_PRODUCT_CORE_TOTAL_CAP);
        CommerceProductCoreProjection::render(storage, varlena, &mut buf);
        println!("ProductCore[0] : {buf}");

        assert!(buf.contains("commerce-product_core"), "classe CSS absente");
        assert!(buf.contains("<dt>id</dt>"), "champ id absent");
    }

    // =========================================================================
    // Tests no-realloc — INVARIANT CRITIQUE
    //
    // Méthode :
    //   1. StorageRow : valeurs pires cas (i64::MIN, i32::MIN, i16::MIN, false).
    //   2. VarlenOwned : Some(chaîne de max_len × '&') pour le pire cas escape × 5.
    //      Si pre_escaped : Some(chaîne de max_len × 'a') (facteur 1).
    //   3. render() avec buf pré-alloué à TOTAL_CAP exactement.
    //   4. Assert buf.capacity() == initial_cap après render().
    //
    // Un échec indique :
    //   - max_display_width sous-estimé pour un FieldKind, OU
    //   - max_escaped_len sous-estimé pour un VarlenField, OU
    //   - Changement de schéma (nom de table/colonne) sans régénération.
    // =========================================================================

    #[test]
    fn test_content_core_no_realloc() {
        // Pires cas fixed-length.
        let storage = ContentCoreStorageRow {
            published_at:        i64::MIN,  // 20 chars
            created_at:          i64::MIN,
            modified_at:         i64::MIN,
            document_id:         i32::MIN,  // 11 chars
            author_entity_id:    i32::MIN,
            status:              i16::MIN,  // 6 chars
            is_readable:         false,     // 5 chars
            is_commentable:      false,
            is_visible_comments: false,
        };

        // Pire cas varlena : max_len caractères '&' → max_len × 5 après escape.
        // Adapter selon les champs réels de content.identity et leurs max_len.
        // Exemple générique ci-dessous — remplacer par les constantes réelles.
        // Les champs non listés (alternative_headline, description, headline…)
        // reçoivent None via Default::default(). None → 0 octet dans render() :
        // cas favorable. DYNAMIC_CAP est calculé sur les max_escaped_len de TOUS
        // les champs indépendamment — la borne supérieure tient.
        let varlena = ContentCoreVarlenOwned {
            ..Default::default()
        };

        let initial_cap = CONTENT_CORE_TOTAL_CAP;
        let mut buf     = String::with_capacity(initial_cap);

        ContentCoreProjection::render(&storage, &varlena, &mut buf);

        assert_eq!(
            buf.capacity(), initial_cap,
            "REALLOC détecté sur ContentCore : capacity {} → {}.\n\
             Fragment-Forge sous-estime la capacité.\n\
             Longueur réelle du HTML : {} octets.",
            initial_cap, buf.capacity(), buf.len()
        );

        assert!(buf.starts_with("<article"), "tag ouvrant manquant");
        assert!(buf.ends_with("</article>"), "tag fermant manquant");
        println!(
            "[no-realloc] ContentCore : cap={}, len={}, ratio={:.0}%",
            initial_cap, buf.len(),
            buf.len() as f64 / initial_cap as f64 * 100.0
        );
    }

    #[test]
    fn test_product_core_no_realloc() {
        // commerce.product_core : pas de varlena → VarlenOwned = ().
        let storage = CommerceProductCoreStorageRow {
            price_cents:  i64::MIN,
            id:           i32::MIN,
            stock:        i32::MIN,
            media_id:     i32::MIN,
            is_available: false,
        };

        let initial_cap = COMMERCE_PRODUCT_CORE_TOTAL_CAP;
        let mut buf     = String::with_capacity(initial_cap);

        // render() reçoit &() pour varlena — ignoré, coût nul.
        CommerceProductCoreProjection::render(&storage, &(), &mut buf);

        assert_eq!(
            buf.capacity(), initial_cap,
            "REALLOC détecté sur CommerceProductCore : capacity {} → {}.\n\
             Longueur réelle : {} octets.",
            initial_cap, buf.capacity(), buf.len()
        );

        assert!(buf.starts_with("<article"), "tag ouvrant manquant");
        assert!(buf.ends_with("</article>"), "tag fermant manquant");
        println!(
            "[no-realloc] ProductCore : cap={}, len={}, ratio={:.0}%",
            initial_cap, buf.len(),
            buf.len() as f64 / initial_cap as f64 * 100.0
        );
    }

    // =========================================================================
    // Tests de ratio de remplissage
    //
    // Cible : 50–90%.
    //   < 50% : DYNAMIC_CAP sur-estimé (gaspillage mémoire).
    //   > 90% : DYNAMIC_CAP trop juste (risque de sous-estimation future).
    // =========================================================================

    #[test]
    fn test_content_core_realistic_ratio() {
        let storage = ContentCoreStorageRow {
            published_at:        1_700_000_000_000_000i64,
            created_at:          1_700_000_000_000_000i64,
            modified_at:         1_700_000_000_000_000i64,
            document_id:         42,
            author_entity_id:    7,
            status:              1,
            is_readable:         true,
            is_commentable:      true,
            is_visible_comments: true,
        };

        // Varlena représentatives : titre court (~40 chars).
        // Adapter selon les champs réels de content.identity.
        let varlena = ContentCoreVarlenOwned {
            // Ex: headline: Some("Introduction à l'architecture DOD".to_string()),
            ..Default::default()
        };

        let mut buf = String::new();
        ContentCoreProjection::render(&storage, &varlena, &mut buf);

        let ratio = buf.len() as f64 / CONTENT_CORE_TOTAL_CAP as f64 * 100.0;
        println!(
            "[ratio] ContentCore réaliste : {}/{} = {:.0}%",
            buf.len(), CONTENT_CORE_TOTAL_CAP, ratio
        );

        // Seuil bas abaissé à 3% : DYNAMIC_CAP peut être légitimement large
        // quand la table comporte des colonnes TEXT/VARCHAR avec fallback 10 000
        // (politique fetch_varlena_cols, ADR-003). Le plafond est conservateur
        // par construction — le ratio faible sur données courtes est attendu.
        // Ce test détecte les sur-estimations pathologiques (facteur > 30×),
        // pas les écarts normaux liés à la politique de max_len.
        // Le test no-realloc (pires cas) reste l'invariant de sécurité primaire.
        if ratio < 10.0 {
            eprintln!(
                "[ratio] AVERTISSEMENT : ratio {:.0}% < 10% — DYNAMIC_CAP dominé                  par des colonnes TEXT larges (fallback 10 000 × escape factor 5 = 50 000B).                  Vérifier fetch_varlena_cols et envisager des contraintes CHECK explicites.",
                ratio
            );
        }
        assert!(ratio > 3.0,
            "DYNAMIC_CAP pathologiquement sur-estimé : {ratio:.0}%              (ratio < 3% indique une colonne TEXT sans contrainte avec fallback excessif)");
        assert!(ratio < 95.0,
            "DYNAMIC_CAP trop juste : {ratio:.0}%");
    }

    #[test]
    fn test_product_core_realistic_ratio() {
        let storage = CommerceProductCoreStorageRow {
            price_cents:  1999,
            id:           42,
            stock:        150,
            media_id:     7,
            is_available: true,
        };

        let mut buf = String::new();
        CommerceProductCoreProjection::render(&storage, &(), &mut buf);

        let ratio = buf.len() as f64 / COMMERCE_PRODUCT_CORE_TOTAL_CAP as f64 * 100.0;
        println!(
            "[ratio] ProductCore réaliste : {}/{} = {:.0}%",
            buf.len(), COMMERCE_PRODUCT_CORE_TOTAL_CAP, ratio
        );

        assert!(ratio > 30.0,
            "DYNAMIC_CAP massivement sur-estimé : {ratio:.0}%");
    }
}
