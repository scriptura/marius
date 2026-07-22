// crates/shell/render/src/bin/dump.rs
//
// Exécuté manuellement au déploiement : cargo run --bin marius-dump
// Jamais par cargo build, jamais par le Dispatcher.

use std::sync::Arc;

use marius_render::{IdSource, LiveRegistry, RouteEntry, regenerate_and_swap};
use marius_schema::{CONTENT_CORE_TOTAL_CAP, ContentCoreProjection};

/// Topologie minimale locale à ce binaire — un seul packfile_key. Ne pas
/// réutiliser ROUTE_TABLE de marius-server : couplage inverse crate render
/// → server proscrit (Document 3 §7, séparation Shell/Forge déjà actée
/// pour build.rs, même principe ici pour les binaires).
static DUMP_ROUTE_TABLE: &[RouteEntry] = &[RouteEntry {
    pattern: "/content/:id",
    packfile_key: "content_core",
    id_source: IdSource::PathParam("id"),
    content_type: "text/html; charset=utf-8",
}];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = sqlx::PgPool::connect(&database_url).await?;

    let all_ids: Vec<i64> =
        sqlx::query_scalar("SELECT document_id::BIGINT FROM content.core ORDER BY document_id ASC")
            .fetch_all(&pool)
            .await?;

    // Store brut #[repr(C)] — conservé : consommé par marius-verify,
    // indépendant du pack HTML.
    marius_render::dumper::dump_table::<ContentCoreProjection>(
        &pool,
        &all_ids,
        all_ids.len() + all_ids.len() / 5,
    )
    .await?;

    // StoreRegistry — provisionnement à froid, local à ce binaire (Phase 1,
    // réactivité CoW). Doit suivre dump_table (qui vient d'écrire
    // artifacts/content_core_store.bin) et précéder tout appel à
    // regenerate_and_swap ci-dessous : celui-ci lit store.bin via
    // fetch_batch/StoreRegistry, jamais via une requête SQL directe (cf.
    // DFS-phase1-reactivite-cow.md §3-4) — sans ce cold_start, fetch_batch
    // panique (StoreRegistry non provisionné), exactement ce que ce binaire
    // a déclenché avant ce correctif : dump_table réussit, écrit le fichier,
    // puis regenerate_and_swap panique en tentant de le relire via un
    // registre jamais monté dans ce process.
    ContentCoreProjection::cold_start_store()?;

    // Pack HTML — invariant manquant jusqu'ici. Provisioning + cold_start
    // locaux à ce process, jetables : ce binaire ne sert aucune requête,
    // il n'a besoin ni du Router Axum, ni des Dispatcher réactifs.
    for route in DUMP_ROUTE_TABLE {
        marius_render::ensure_provisioned(route.packfile_key).await?;
    }
    let registry = Arc::new(LiveRegistry::cold_start(DUMP_ROUTE_TABLE)?);

    // Semaphore à 1 permis : appel unique, séquentiel, aucun shard concurrent
    // dans ce binaire — pas de partage inter-Dispatcher à réguler ici.
    let io_semaphore = Arc::new(tokio::sync::Semaphore::new(1));

    regenerate_and_swap::<ContentCoreProjection>(
        &pool,
        &all_ids,
        CONTENT_CORE_TOTAL_CAP,
        "content_core",
        &registry,
        &io_semaphore,
    )
    .await?;

    println!("[dump] store + pack régénérés — {} enreg.", all_ids.len());

    Ok(())
}
