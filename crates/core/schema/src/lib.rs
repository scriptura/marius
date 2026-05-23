// marius-schema
// Crate de structs générées par DB-Forge.
// Aucun code manuel — tout vient de build.rs → $OUT_DIR/generated_schema.rs.

// Réexporte le trait Projection depuis marius-projection (frontière Core/Shell).
// Le code généré utilise `crate::projection::Projection`.
pub mod projection {
    pub use marius_projection::Projection;
}

// Réexporte le Collector pour les statics générés.
pub mod collector {
    pub use marius_collector::Collector;
}

// Point d'entrée de la génération.
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::Projection;

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

        let mut buf = String::new();
        ContentCoreProjection::render(&records[0], &mut buf);
        println!("ContentCore[0] HTML : {buf}");
        assert!(buf.contains("content-core"));
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

        let mut buf = String::new();
        CommerceProductCoreProjection::render(&records[0], &mut buf);
        println!("ProductCore[0] HTML : {buf}");
    }
}
