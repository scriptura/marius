// marius-server — point d'entrée du moteur réactif.
//
// Architecture :
//   tâche spawned : PgListener → Collector.insert() → notify si ThresholdReached
//   tâche main    : Axum ServeDir → sert les artefacts HTML depuis le disque
//   tâches fond   : Dispatcher × 2 (un par table surveillée)
//
// Variables d'environnement :
//   DATABASE_URL         : connexion PostgreSQL (obligatoire)
//   MARIUS_ARTIFACTS_DIR : répertoire des artefacts (défaut : ./artifacts)
//   MARIUS_BIND          : adresse d'écoute HTTP (défaut : 0.0.0.0:3000)

use std::sync::Arc;
use tokio::sync::Notify;

use axum::Router;
use tower_http::services::ServeDir;

use marius_collector::InsertResult;
use marius_render::{Dispatcher, DispatcherConfig};
use marius_schema::{
    CONTENT_CORE_COLLECTOR,          MAX_CONTENT_CORE_ID,          CONTENT_CORE_WORDS,
    COMMERCE_PRODUCT_CORE_COLLECTOR, MAX_COMMERCE_PRODUCT_CORE_ID, COMMERCE_PRODUCT_CORE_WORDS,
    ContentCoreProjection,
    CommerceProductCoreProjection,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url  = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL non définie");
    let artifacts_dir = std::env::var("MARIUS_ARTIFACTS_DIR")
        .unwrap_or_else(|_| "artifacts".to_string());
    let bind_addr     = std::env::var("MARIUS_BIND")
        .unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    std::fs::create_dir_all(format!("{artifacts_dir}/content/core"))?;
    std::fs::create_dir_all(format!("{artifacts_dir}/commerce/product_core"))?;

    let pool   = sqlx::PgPool::connect(&database_url).await?;
    let config = DispatcherConfig::default();
    println!("[marius] pool connecté");

    let notify_content = Arc::new(Notify::new());
    let notify_product = Arc::new(Notify::new());

    // ── Dispatchers ───────────────────────────────────────────────────────────
    tokio::spawn(Dispatcher::<
        ContentCoreProjection,
        MAX_CONTENT_CORE_ID,
        CONTENT_CORE_WORDS,
    >::new(
        &CONTENT_CORE_COLLECTOR,
        notify_content.clone(),
        pool.clone(),
        DispatcherConfig::default(),
    ).run());

    tokio::spawn(Dispatcher::<
        CommerceProductCoreProjection,
        MAX_COMMERCE_PRODUCT_CORE_ID,
        COMMERCE_PRODUCT_CORE_WORDS,
    >::new(
        &COMMERCE_PRODUCT_CORE_COLLECTOR,
        notify_product.clone(),
        pool.clone(),
        DispatcherConfig::default(),
    ).run());

    println!("[marius] dispatchers démarrés");

    // ── LISTEN/NOTIFY (tâche spawned) ─────────────────────────────────────────
    // Spawné pour libérer le thread main pour Axum.
    let database_url_clone = database_url.clone();
    let threshold          = config.threshold_flush;
    tokio::spawn(async move {
        let mut listener = match sqlx::postgres::PgListener::connect(&database_url_clone).await {
            Ok(l)  => l,
            Err(e) => { eprintln!("[marius] PgListener connect: {e}"); return; }
        };
        if let Err(e) = listener.listen_all(["content_core_updates", "product_core_updates"]).await {
            eprintln!("[marius] LISTEN: {e}"); return;
        }
        println!("[marius] LISTEN actif sur : content_core_updates, product_core_updates");

        loop {
            match listener.recv().await {
                Err(e) => { eprintln!("[marius] recv error: {e}"); break; }
                Ok(notification) => {
                    let Ok(id) = notification.payload().parse::<i64>() else {
                        eprintln!("[marius] payload invalide : {:?}", notification.payload());
                        continue;
                    };
                    match notification.channel() {
                        "content_core_updates" => {
                            if CONTENT_CORE_COLLECTOR.insert(id, threshold)
                                == InsertResult::ThresholdReached
                            {
                                notify_content.notify_one();
                            }
                        }
                        "product_core_updates" => {
                            if COMMERCE_PRODUCT_CORE_COLLECTOR.insert(id, threshold)
                                == InsertResult::ThresholdReached
                            {
                                notify_product.notify_one();
                            }
                        }
                        other => eprintln!("[marius] canal inconnu : {other}"),
                    }
                }
            }
        }
    });

    // ── Axum ServeDir (tâche main) ────────────────────────────────────────────
    // Read path O(1) : sendfile(2) depuis le disque, zéro traitement Rust.
    // Les artefacts sont pré-calculés par le Dispatcher au moment de la mutation.
    let app = Router::new()
        .nest_service("/", ServeDir::new(&artifacts_dir));

    let tcp_listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("[marius] HTTP sur http://{bind_addr}");
    println!("[marius] exemples :");
    println!("  http://{bind_addr}/content/core/1.html");
    println!("  http://{bind_addr}/commerce/product_core/1.html");

    axum::serve(tcp_listener, app).await?;
    Ok(())
}
