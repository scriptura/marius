// marius-server — point d'entrée du moteur réactif
//
// Pipeline :
//   PostgreSQL LISTEN/NOTIFY
//     → Collector<MAX, WORDS> (bit-vector lock-free)
//     → Dispatcher (tick adaptatif + seuil volumétrique)
//     → render() + artifact_path()
//     → fichier HTML sur disque
//
// Variables d'environnement :
//   DATABASE_URL          : connexion PostgreSQL (obligatoire)
//   MARIUS_ARTIFACTS_DIR  : répertoire de sortie des artefacts (défaut : ./artifacts)

use std::sync::Arc;
use tokio::sync::Notify;

use marius_collector::{Dispatcher, DispatcherConfig};
use marius_schema::{
    CONTENT_CORE_COLLECTOR,          MAX_CONTENT_CORE_ID,          CONTENT_CORE_WORDS,
    COMMERCE_PRODUCT_CORE_COLLECTOR, MAX_COMMERCE_PRODUCT_CORE_ID, COMMERCE_PRODUCT_CORE_WORDS,
    ContentCoreProjection,
    CommerceProductCoreProjection,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL non définie");

    let artifacts_dir = std::env::var("MARIUS_ARTIFACTS_DIR")
        .unwrap_or_else(|_| "artifacts".to_string());

    // Créer les répertoires d'artefacts au démarrage.
    std::fs::create_dir_all(format!("{artifacts_dir}/content/core"))?;
    std::fs::create_dir_all(format!("{artifacts_dir}/commerce/product_core"))?;

    // Pool applicatif — utilisé par les Dispatchers pour fetch_batch.
    // En production : utiliser marius_user (RLS actif).
    let pool = sqlx::PgPool::connect(&database_url).await?;
    println!("[marius] pool connecté");

    // Notifiers volumétriques — réveillent le Dispatcher avant le tick.
    let notify_content = Arc::new(Notify::new());
    let notify_product = Arc::new(Notify::new());

    // Dispatcher content.core
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

    // Dispatcher commerce.product_core
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

    // Lancer les Dispatchers en arrière-plan.
    tokio::spawn(dispatcher_content.run());
    tokio::spawn(dispatcher_product.run());
    println!("[marius] dispatchers démarrés");

    // Connexion dédiée au LISTEN/NOTIFY.
    // PgListener maintient sa propre connexion longue durée,
    // séparée du pool applicatif.
    let mut listener = sqlx::postgres::PgListener::connect(&database_url).await?;
    listener.listen("content_core_updates").await?;
    listener.listen("product_core_updates").await?;
    println!("[marius] LISTEN actif sur : content_core_updates, product_core_updates");

    // Boucle principale — réception des notifications PostgreSQL.
    loop {
        let notification = listener.recv().await?;
        let channel = notification.channel();
        let payload = notification.payload();

        match payload.parse::<i64>() {
            Ok(id) => {
                match channel {
                    "content_core_updates" => {
                        CONTENT_CORE_COLLECTOR.insert(
                            id,
                            DispatcherConfig::default().threshold_flush,
                            &notify_content,
                        );
                    }
                    "product_core_updates" => {
                        COMMERCE_PRODUCT_CORE_COLLECTOR.insert(
                            id,
                            DispatcherConfig::default().threshold_flush,
                            &notify_product,
                        );
                    }
                    other => {
                        eprintln!("[marius] canal inconnu : {other}");
                    }
                }
            }
            Err(_) => {
                eprintln!("[marius] payload non parsable : {payload:?} sur {channel}");
            }
        }
    }
}
