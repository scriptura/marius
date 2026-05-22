// marius-schema
// Ce crate ne contient pas de code écrit à la main.
// Toutes les structs, statics et stubs Projection sont générés par DB-Forge
// via build.rs → $OUT_DIR/generated_schema.rs.

// Réexporte le trait Projection pour que les implémentations générées
// puissent y accéder via `crate::projection::Projection`.
pub mod projection {
    pub use marius_collector::Projection;
}

// Réexporte le Collector pour les statics générés.
pub mod collector {
    pub use marius_collector::Collector;
}

// Point d'entrée de la génération.
// Le compilateur voit toutes les structs et implémentations comme
// si elles étaient écrites ici.
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use marius_collector::Projection; // trait en scope pour appeler fetch_batch

    // cargo test -p marius-schema -- --ignored --nocapture
    // Requiert DATABASE_URL dans l'environnement ou .cargo/config.toml.
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
        println!(
            "ContentCore[0] : document_id={}, status={}, created_at={}",
            records[0].document_id,
            records[0].status,
            records[0].created_at,
        );
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

        assert!(!records.is_empty(), "Aucun enregistrement — DML appliqué ?");
        println!(
            "ProductCore[0] : id={}, price_cents={}, stock={}, is_available={}",
            records[0].id,
            records[0].price_cents, // -1 si NULL en DB (sentinel)
            records[0].stock,
            records[0].is_available,
        );
    }
}
