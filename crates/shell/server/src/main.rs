// marius-server — point d'entrée du moteur réactif.
//
// Architecture Phase 1 :
//   PgListener → collector.insert() → InsertResult → notify (si ThresholdReached)
//   Dispatcher (dans marius-render) → flush → fetch_batch → render → artefact
//
// Variables d'environnement :
//   DATABASE_URL          : connexion PostgreSQL (obligatoire)
//   MARIUS_ARTIFACTS_DIR  : répertoire de sortie des artefacts (défaut : ./artifacts)
 
use std::sync::Arc;
use tokio::sync::Notify;
 
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
 
    std::fs::create_dir_all(format!("{artifacts_dir}/content/core"))?;
    std::fs::create_dir_all(format!("{artifacts_dir}/commerce/product_core"))?;
 
    let pool = sqlx::PgPool::connect(&database_url).await?;
    println!("[marius] pool connecté");
 
    let config = DispatcherConfig::default();
 
    let notify_content = Arc::new(Notify::new());
    let notify_product = Arc::new(Notify::new());
 
    let dispatcher_content = Dispatcher::<
        ContentCoreProjection,
        MAX_CONTENT_CORE_ID,
        CONTENT_CORE_WORDS,
    >::new(
        &CONTENT_CORE_COLLECTOR,
        notify_content.clone(),
        pool.clone(),
        DispatcherConfig::default(),
    );
 
    let dispatcher_product = Dispatcher::<
        CommerceProductCoreProjection,
        MAX_COMMERCE_PRODUCT_CORE_ID,
        COMMERCE_PRODUCT_CORE_WORDS,
    >::new(
        &COMMERCE_PRODUCT_CORE_COLLECTOR,
        notify_product.clone(),
        pool.clone(),
        DispatcherConfig::default(),
    );
 
    tokio::spawn(dispatcher_content.run());
    tokio::spawn(dispatcher_product.run());
    println!("[marius] dispatchers démarrés");
 
    let mut listener = sqlx::postgres::PgListener::connect(&database_url).await?;
    listener.listen("content_core_updates").await?;
    listener.listen("product_core_updates").await?;
    println!("[marius] LISTEN actif sur : content_core_updates, product_core_updates");
 
    loop {
        let notification = listener.recv().await?;
        let channel = notification.channel();
        let payload = notification.payload();
 
        match payload.parse::<i64>() {
            Ok(id) => {
                match channel {
                    "content_core_updates" => {
                        // insert() retourne InsertResult — zéro Tokio dans le Collector.
                        if CONTENT_CORE_COLLECTOR.insert(id, config.threshold_flush)
                            == InsertResult::ThresholdReached
                        {
                            notify_content.notify_one();
                        }
                    }
                    "product_core_updates" => {
                        if COMMERCE_PRODUCT_CORE_COLLECTOR.insert(id, config.threshold_flush)
                            == InsertResult::ThresholdReached
                        {
                            notify_product.notify_one();
                        }
                    }
                    other => eprintln!("[marius] canal inconnu : {other}"),
                }
            }
            Err(_) => eprintln!("[marius] payload non parsable : {payload:?} sur {channel}"),
        }
    }
}
