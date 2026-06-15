// Exécuté manuellement au déploiement : cargo run --bin marius-dump
// Jamais par cargo build, jamais par le Dispatcher.

use marius_schema::ContentCoreProjection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = sqlx::PgPool::connect(&database_url).await?;

    // Récupération des ids — SELECT id FROM table ORDER BY id ASC
    // À remplacer par la fonction d'introspection adéquate.
    let all_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT document_id::BIGINT FROM content.core ORDER BY document_id ASC" // <-- Forçage INT8 avec ::BIGINT pour éviter le cast implicite en i32
    )
    .fetch_all(&pool)
    .await?;

    marius_render::dumper::dump_table::<ContentCoreProjection>(
        &pool,
        &all_ids,
        all_ids.len() + all_ids.len() / 5, // +20% marge
    )
    .await?;

    Ok(())
}
