// marius-schema
// Crate de structs générées par DB-Forge.

pub mod projection {
    pub use marius_projection::Projection;
}

pub mod collector {
    pub use marius_collector::Collector;
}

include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::Projection;

    // =========================================================================
    // Tests de rendu — validité fonctionnelle
    // =========================================================================

    #[tokio::test]
    #[ignore]
    async fn test_fetch_content_core() {
        let pool = sqlx::PgPool::connect(
            &std::env::var("DATABASE_URL").unwrap()
        ).await.unwrap();

        let ids     = vec![1i64, 2, 3];
        let records = ContentCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!records.is_empty(), "Aucun enregistrement — DML appliqué ?");

        let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);
        ContentCoreProjection::render(&records[0], &mut buf);
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
        let records = CommerceProductCoreProjection::fetch_batch(&pool, &ids)
            .await
            .unwrap();

        assert!(!records.is_empty());

        let mut buf = String::with_capacity(COMMERCE_PRODUCT_CORE_TOTAL_CAP);
        CommerceProductCoreProjection::render(&records[0], &mut buf);
        println!("ProductCore[0] : {buf}");

        assert!(buf.contains("commerce-product_core"));
        assert!(buf.contains("<dt>id</dt>"));
    }

    // =========================================================================
    // Tests no-realloc — INVARIANT CRITIQUE
    //
    // Vérifient que Fragment-Forge calcule correctement STATIC_CAP + DYNAMIC_CAP.
    // Un échec signifie que le render() déclenche un realloc sur le tas (heap) :
    //   - Fragment-Forge sous-estime la taille d'un champ dynamique, OU
    //   - Un champ statique a changé sans que les constantes soient régénérées.
    //
    // Méthode : alimenter le record avec les PIRES CAS de chaque type
    //   (valeur la plus longue à afficher) puis vérifier que capacity est intact.
    // =========================================================================

    #[test]
    fn test_content_core_no_realloc() {
        // Pires cas : valeurs maximales en termes de largeur d'affichage.
        let record = ContentCore {
            published_at:        i64::MIN,   // "-9223372036854775808" = 20 chars
            created_at:          i64::MIN,
            modified_at:         i64::MIN,
            document_id:         i32::MIN,   // "-2147483648" = 11 chars
            author_entity_id:    i32::MIN,
            status:              i16::MIN,   // "-32768" = 6 chars
            is_readable:         false,      // "false" = 5 chars (> "true")
            is_commentable:      false,
            is_visible_comments: false,
        };

        let initial_cap = CONTENT_CORE_TOTAL_CAP;
        let mut buf     = String::with_capacity(initial_cap);

        ContentCoreProjection::render(&record, &mut buf);

        assert_eq!(
            buf.capacity(), initial_cap,
            "REALLOC détecté sur ContentCore : capacity {} → {}.\n\
             Fragment-Forge sous-estime la capacité dynamique.\n\
             Longueur réelle du HTML : {} octets.",
            initial_cap, buf.capacity(), buf.len()
        );

        // Sanity check : le HTML produit est valide (bornes présentes).
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
        let record = CommerceProductCore {
            price_cents:  i64::MIN,
            id:           i32::MIN,
            stock:        i32::MIN,
            media_id:     i32::MIN,
            is_available: false,
        };

        let initial_cap = COMMERCE_PRODUCT_CORE_TOTAL_CAP;
        let mut buf     = String::with_capacity(initial_cap);

        CommerceProductCoreProjection::render(&record, &mut buf);

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
    // Test de mesure du ratio de remplissage
    //
    // Imprime le pourcentage de la capacité réellement utilisée sur des données
    // réalistes (vs pires cas). Un ratio > 90% signale un DYNAMIC_CAP trop juste.
    // Un ratio < 50% signale un DYNAMIC_CAP sur-estimé (gaspillage mémoire).
    // La cible est 70-85%.
    // =========================================================================

    #[test]
    fn test_content_core_realistic_ratio() {
        // Valeurs représentatives d'un document réel
        let record = ContentCore {
            published_at:        1_700_000_000_000_000i64, // ~2023
            created_at:          1_700_000_000_000_000i64,
            modified_at:         1_700_000_000_000_000i64,
            document_id:         42,
            author_entity_id:    7,
            status:              1,
            is_readable:         true,
            is_commentable:      true,
            is_visible_comments: true,
        };

        let mut buf = String::new();
        ContentCoreProjection::render(&record, &mut buf);

        let ratio = buf.len() as f64 / CONTENT_CORE_TOTAL_CAP as f64 * 100.0;
        println!(
            "[ratio] ContentCore réaliste : {}/{} = {:.0}%",
            buf.len(), CONTENT_CORE_TOTAL_CAP, ratio
        );

        // Cible : 50-90%. En dehors → revoir DYNAMIC_CAP dans Fragment-Forge.
        assert!(ratio > 30.0, "DYNAMIC_CAP massivment sur-estimé : {ratio:.0}%");
    }
}
