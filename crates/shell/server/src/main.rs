// =============================================================================
// crates/shell/server/src/main.rs
//
// Bootstrap — frontière réseau Axum/Tokio. specification-marius-render-shell.md
// §4/§5, roadmap-marius-render-shell.md Phase 3.
//
// Remplacement complet du prototype naïf précédent (ServeDir + Dispatcher +
// PgListener écrivant un fichier par entité) — structurellement obsolète vis-
// à-vis d'ADR-008 et des invariants posés en Phase 1/2 (un packfile est un
// blob unique, jamais un fichier par id). Arbitrage de session : la boucle
// d'écriture réactive (PgListener/Dispatcher/regenerate_and_swap) repasse en
// Phase 4 — hors périmètre ici. Ce fichier ne contient QUE la frontière de
// lecture : ROUTE_TABLE, cold_start(), enregistrement des routes Axum.
//
// ROUTE_TABLE ci-dessous est un STUB écrit à la main pour cette session — le
// compilateur de templates de pages (FragmentRef, ADR-008 §4.2-§4.5) qui la
// générerait n'existe pas et n'est pas improvisé ici (hors périmètre).
// =============================================================================

mod handlers;

use std::sync::Arc;

use axum::routing::get;
use axum::{Extension, Router};

use marius_render::{IdSource, LiveRegistry, RouteEntry};

/// Table de routage AOT — écrite à la main (cf. en-tête de fichier).
/// 3 entrées : suffisant pour le Jalon 3 ("2-3 packfiles synthétiques"),
/// couvre les deux IdSource (PathParam, Fixed).
static ROUTE_TABLE: &[RouteEntry] = &[
    RouteEntry {
        pattern: "/produit/:id",
        packfile_key: "commerce_product_core",
        id_source: IdSource::PathParam("id"),
    },
    RouteEntry {
        pattern: "/contenu/:id",
        packfile_key: "content_core",
        id_source: IdSource::PathParam("id"),
    },
    RouteEntry {
        pattern: "/",
        packfile_key: "pages_homepage",
        id_source: IdSource::Fixed(1),
    },
];

/// Construit le Router à partir d'une table de routage et d'un registre déjà
/// initialisé — factorisé pour être réutilisé tel quel par les tests
/// d'intégration ci-dessous (Jalon 3), sans dupliquer la logique de montage.
///
/// Une `Extension(entry)` par route (pas une closure par pattern) : chaque
/// `MethodRouter` reçoit son `&'static RouteEntry` via `.layer()`, injecté
/// dans le handler unique `handlers::serve_route` à l'extraction (spec §6.1).
fn build_router(route_table: &'static [RouteEntry], registry: Arc<LiveRegistry>) -> Router {
    let mut router = Router::new();
    for entry in route_table {
        router = router.route(
            entry.pattern,
            get(handlers::serve_route).layer(Extension(entry)),
        );
    }
    router.with_state(registry)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Cold start : mmap eager de chaque index connu, fd ouverts — tout le
    // coût d'initialisation payé une fois, avant d'accepter la première
    // connexion (spec §5). Échec fatal si un packfile référencé par
    // ROUTE_TABLE est introuvable — pas de dégradation silencieuse.
    let registry = LiveRegistry::cold_start(ROUTE_TABLE)?;
    println!(
        "[marius-server] cold_start réussi — {} route(s) enregistrée(s)",
        ROUTE_TABLE.len()
    );

    let app = build_router(ROUTE_TABLE, Arc::new(registry));

    let bind_addr = std::env::var("MARIUS_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("[marius-server] HTTP sur http://{bind_addr}");

    axum::serve(listener, app).await?;
    Ok(())
}

// =============================================================================
// Tests — Jalon 3
//
// Intégration réelle : vrai TcpListener (port éphémère, 127.0.0.1:0), vrai
// reqwest::Client, vrai cold_start() sur des packfiles synthétiques écrits
// au chemin résolu par packfile_path_for(). Chaque test utilise une clé de
// packfile unique (pid + compteur, leak 'static volontaire — usage de test
// uniquement) pour rester indépendant des autres tests de ce binaire et
// d'éventuelles exécutions parallèles (comportement par défaut de
// `cargo test`).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufWriter, Write};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_test_key(label: &str) -> &'static str {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Box::leak(format!("jalon3_{label}_{}_{n}", std::process::id()).into_boxed_str())
    }

    /// Écrit un packfile synthétique au chemin résolu par
    /// `marius_render::packfile_path_for` — même convention que
    /// `cold_start()`, pour que le test prouve quelque chose sur le
    /// bootstrap réel, pas sur un chemin arbitraire.
    fn write_fixture_packfile(packfile_key: &'static str, ids_and_fragments: &[(i64, &[u8])]) {
        let path = marius_render::packfile_path_for(packfile_key);
        std::fs::create_dir_all(
            path.parent()
                .expect("packfile_path_for retourne toujours un chemin avec parent"),
        )
        .expect("création du répertoire artifacts/ de test");

        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(ids_and_fragments.len());
        let mut offset = 0u64;
        for (id, frag) in ids_and_fragments {
            blob.extend_from_slice(frag);
            entries.push(marius_render::PackfileEntry {
                id: *id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        let file = std::fs::File::create(&path).expect("création packfile de test");
        let mut writer = BufWriter::new(file);
        writer.write_all(&blob).expect("écriture blob");
        marius_render::pack_html_format::write_packfile_footer(
            &mut writer,
            blob.len() as u64,
            &entries,
        )
        .expect("écriture footer+index");
        writer.flush().expect("flush");
    }

    /// Démarre un serveur réel sur un port éphémère, à partir d'une table de
    /// routage déjà fixée par le test (fixtures déjà écrites sur disque).
    /// Retourne l'adresse à interroger et l'`Arc<LiveRegistry>` du serveur
    /// (nécessaire au test de swap concurrent — point de vigilance n°4).
    async fn spawn_test_server(
        route_table: &'static [RouteEntry],
    ) -> (SocketAddr, Arc<LiveRegistry>) {
        let registry = Arc::new(
            LiveRegistry::cold_start(route_table)
                .expect("cold_start doit réussir — fixtures déjà écrites"),
        );
        let app = build_router(route_table, registry.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind port éphémère");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serveur de test");
        });

        (addr, registry)
    }

    #[tokio::test]
    async fn jalon3_serves_200_404_400_correctly() {
        let key = unique_test_key("basic");
        let route_table: &'static [RouteEntry] = Box::leak(
            vec![
                RouteEntry {
                    pattern: "/produit/:id",
                    packfile_key: key,
                    id_source: IdSource::PathParam("id"),
                },
                RouteEntry {
                    pattern: "/",
                    packfile_key: key,
                    id_source: IdSource::Fixed(1),
                },
            ]
            .into_boxed_slice(),
        );

        write_fixture_packfile(
            key,
            &[(1, b"<p>produit-un</p>"), (2, b"<p>produit-deux</p>")],
        );

        let (addr, _registry) = spawn_test_server(route_table).await;
        let client = reqwest::Client::new();

        // 200 — id existant, Content-Length exact, corps identique.
        let resp = client
            .get(format!("http://{addr}/produit/1"))
            .send()
            .await
            .expect("requête id existant");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let content_length = resp.content_length().expect("Content-Length présent");
        let body = resp.bytes().await.expect("corps");
        assert_eq!(content_length, body.len() as u64);
        assert_eq!(&body[..], b"<p>produit-un</p>");

        // 200 — route Fixed (pas de paramètre de chemin).
        let resp = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("requête racine");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert_eq!(resp.bytes().await.unwrap().as_ref(), b"<p>produit-un</p>");

        // 404 — id absent du packfile.
        let resp = client
            .get(format!("http://{addr}/produit/999"))
            .send()
            .await
            .expect("requête id absent");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

        // 400 — paramètre non numérique.
        let resp = client
            .get(format!("http://{addr}/produit/abc"))
            .send()
            .await
            .expect("requête paramètre invalide");
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn jalon3_handles_concurrent_load_without_starving_tokio_pool() {
        let key = unique_test_key("load");
        let route_table: &'static [RouteEntry] = Box::leak(
            vec![RouteEntry {
                pattern: "/produit/:id",
                packfile_key: key,
                id_source: IdSource::PathParam("id"),
            }]
            .into_boxed_slice(),
        );

        const FRAGMENT: &[u8] = b"<p>charge</p>";
        write_fixture_packfile(key, &[(1, FRAGMENT)]);

        let (addr, _registry) = spawn_test_server(route_table).await;
        let client = reqwest::Client::new();

        const NUM_REQUESTS: usize = 300;
        let mut handles = Vec::with_capacity(NUM_REQUESTS);
        for _ in 0..NUM_REQUESTS {
            let client = client.clone();
            let url = format!("http://{addr}/produit/1");
            handles.push(tokio::spawn(async move {
                let resp = client.get(url).send().await.expect("requête concurrente");
                assert_eq!(resp.status(), reqwest::StatusCode::OK);
                let body = resp.bytes().await.expect("corps");
                assert_eq!(&body[..], FRAGMENT);
            }));
        }

        for h in handles {
            h.await.expect(
                "une tâche de requête a paniqué — pool Tokio probablement étouffé \
                 ou réponse incorrecte",
            );
        }
    }

    /// Point de vigilance n°4 (roadmap) : ArcSwap × spawn_blocking sous un
    /// vrai exécuteur Tokio, sous charge réelle — la Phase 2 prouve la
    /// correction d'ArcSwap en isolation (std::thread, sans Tokio), pas son
    /// interaction avec spawn_blocking et un vrai pool Tokio en service
    /// simultané de requêtes HTTP réelles.
    #[tokio::test]
    async fn jalon3_concurrent_store_during_live_serving_never_serves_torn_fragment() {
        let key = unique_test_key("swap");
        let route_table: &'static [RouteEntry] = Box::leak(
            vec![RouteEntry {
                pattern: "/produit/:id",
                packfile_key: key,
                id_source: IdSource::PathParam("id"),
            }]
            .into_boxed_slice(),
        );

        const IDS_AND_FRAGMENTS: &[(i64, &[u8])] =
            &[(1, b"<p>fragment-un</p>"), (2, b"<p>fragment-deux</p>")];
        write_fixture_packfile(key, IDS_AND_FRAGMENTS);

        let (addr, registry) = spawn_test_server(route_table).await;
        let client = reqwest::Client::new();

        const NUM_GENERATIONS: usize = 50;
        const NUM_REQUESTS: usize = 200;

        // Tâche d'écriture : réécrit le même packfile (mêmes ids/fragments —
        // seule l'identité de l'instance PackHtmlIndex change, même
        // discipline que le test Jalon 2 en std::thread) et store() sur le
        // LiveRegistry RÉELLEMENT utilisé par le serveur en train de servir
        // des requêtes. rename() atomique délibérément non exercé ici : ce
        // test cible l'interaction ArcSwap × spawn_blocking, pas la
        // durabilité de l'écriture (regenerate_and_swap, Phase 4).
        let writer_registry = registry.clone();
        let writer = tokio::spawn(async move {
            for generation in 0..NUM_GENERATIONS {
                write_fixture_packfile(key, IDS_AND_FRAGMENTS);
                let path = marius_render::packfile_path_for(key);
                let new_index = marius_render::PackHtmlIndex::open(&path)
                    .unwrap_or_else(|e| panic!("réouverture génération {generation} : {e}"));
                writer_registry.store(key, Arc::new(new_index));
            }
        });

        let mut handles = Vec::with_capacity(NUM_REQUESTS);
        for i in 0..NUM_REQUESTS {
            let client = client.clone();
            let (id, expected) = IDS_AND_FRAGMENTS[i % IDS_AND_FRAGMENTS.len()];
            let url = format!("http://{addr}/produit/{id}");
            handles.push(tokio::spawn(async move {
                let resp = client
                    .get(url)
                    .send()
                    .await
                    .expect("requête concurrente au swap");
                assert_eq!(resp.status(), reqwest::StatusCode::OK);
                let body = resp.bytes().await.expect("corps");
                assert_eq!(
                    &body[..],
                    expected,
                    "lecture incohérente pendant un store() concurrent — id={id}"
                );
            }));
        }

        for h in handles {
            h.await.expect("une requête a paniqué pendant le swap concurrent");
        }
        writer.await.expect("la tâche d'écriture a paniqué");
    }
}
