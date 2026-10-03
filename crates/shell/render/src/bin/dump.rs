// crates/shell/render/src/bin/dump.rs

//! # marius-dump
//! Exécuté manuellement au déploiement : cargo run --bin marius-dump
//! Jamais par cargo build, jamais par le Dispatcher.
//!
//! V3b — population initiale complète : le `Dispatcher` réactif ne
//! régénère que les ids que `Collector::flush()` lui signale (notifications
//! Postgres survenues APRÈS son propre démarrage, jamais un rattrapage des
//! lignes déjà présentes). `marius-dump` reste donc le seul mécanisme de
//! première population — désormais étendu à `content_core_head`/
//! `content_core_tail`, pas seulement à l'artefact monolithique : sans ce
//! correctif, `/content/{id}` répond 404 pour tout id tant qu'aucune
//! écriture n'a eu lieu en base après le démarrage du serveur (head/tail ne
//! contiennent, à l'origine, que le packfile vide produit par
//! `ensure_provisioned`).

use std::sync::Arc;

use marius_render::{
    LiveRegistry, RouteEntry, SplitRenderTarget, regenerate_and_swap_with_volatile_split,
    route_entry_from_spec,
};
use marius_schema::{
    CONTENT_CORE_ARTIFACT, CONTENT_CORE_HEAD_ARTIFACT, CONTENT_CORE_HEAD_TOTAL_CAP,
    CONTENT_CORE_TAIL_ARTIFACT, CONTENT_CORE_TAIL_TOTAL_CAP, CONTENT_CORE_TOTAL_CAP,
    CONTENT_DOCUMENT_ROUTE, ContentCoreProjection,
};

/// Topologie minimale locale à ce binaire — la seule route de contenu, DÉRIVÉE
/// de la déclaration générée par le build de marius-schema (publication.toml),
/// jamais redéclarée ici. Ne pas réutiliser ROUTE_TABLE de marius-server :
/// couplage inverse crate render → server proscrit (Document 3 §7, séparation
/// Shell/Forge déjà actée pour build.rs, même principe ici pour les
/// binaires). La déclaration, elle, vit en amont des deux crates
/// (marius-schema), donc reste accessible sans ce couplage.
static DUMP_ROUTE_TABLE: &[RouteEntry] = &[route_entry_from_spec(&CONTENT_DOCUMENT_ROUTE)];

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
    // regenerate_and_swap_with_volatile_split ci-dessous : celui-ci lit
    // store.bin via fetch_batch/StoreRegistry, jamais via une requête SQL
    // directe (cf. DFS-phase1-reactivite-cow.md §3-4) — sans ce cold_start,
    // fetch_batch panique (StoreRegistry non provisionné).
    ContentCoreProjection::cold_start_store()?;

    // Pack HTML — monolithique (DUMP_ROUTE_TABLE, "content_core") ET,
    // depuis V3b, head/tail (clés hors route, jamais montées en route HTTP
    // par ce binaire — même discipline que marius-server). Provisioning +
    // cold_start locaux à ce process, jetables : ce binaire ne sert aucune
    // requête, il n'a besoin ni du Router Axum, ni des Dispatcher réactifs.
    let content_core_head_key: &'static str = CONTENT_CORE_HEAD_ARTIFACT.as_str();
    let content_core_tail_key: &'static str = CONTENT_CORE_TAIL_ARTIFACT.as_str();
    for route in DUMP_ROUTE_TABLE {
        marius_render::ensure_provisioned(route.packfile_key).await?;
    }
    for key in [content_core_head_key, content_core_tail_key] {
        marius_render::ensure_provisioned(key).await?;
    }
    let registry = Arc::new(LiveRegistry::cold_start_with_extra_keys(
        DUMP_ROUTE_TABLE,
        &[content_core_head_key, content_core_tail_key],
    )?);

    // Semaphore à 1 permis : appel unique, séquentiel, aucun shard concurrent
    // dans ce binaire — pas de partage inter-Dispatcher à réguler ici.
    let io_semaphore = Arc::new(tokio::sync::Semaphore::new(1));

    // Une seule ingestion (fetch_batch, à l'intérieur de la fonction
    // ci-dessous) pour les trois artefacts — même contrat que le Dispatcher
    // réactif (V2d/V3b, main.rs), reproduit ici pour la première
    // population plutôt que réinventé.
    regenerate_and_swap_with_volatile_split::<ContentCoreProjection>(
        &pool,
        &all_ids,
        CONTENT_CORE_TOTAL_CAP,
        CONTENT_CORE_ARTIFACT.as_str(),
        Some(&(
            SplitRenderTarget {
                packfile_key: content_core_head_key,
                total_cap: CONTENT_CORE_HEAD_TOTAL_CAP,
                render: ContentCoreProjection::render_head,
            },
            SplitRenderTarget {
                packfile_key: content_core_tail_key,
                total_cap: CONTENT_CORE_TAIL_TOTAL_CAP,
                render: ContentCoreProjection::render_tail,
            },
        )),
        &registry,
        &io_semaphore,
    )
    .await?;

    println!(
        "[dump] store + pack (monolithique + head + tail) régénérés — {} enreg.",
        all_ids.len()
    );

    Ok(())
}
