// =============================================================================
// crates/shell/server/src/main.rs
//
// Bootstrap — frontière réseau Axum/Tokio + ressources/Dispatchers réactifs.
// specification-phase5-orchestration-main.md, Phase 5.1.
//
// Historique : Phase 3 a posé la frontière de lecture pure (ROUTE_TABLE,
// cold_start, build_router/axum::serve) — composée ici à l'identique (spec
// §7), aucune modification du Read Path.
//
// Phase 5.1 (cette session) ajoute :
//   - Ressources globales : PgPool, Arc<Semaphore> (I/O), Arc<LiveRegistry>
//     partagé (spec §4).
//   - Table de configuration unifiée par shard, non générique : SHARDS,
//     ShardMetadata, DEFAULT_DISPATCHER_CONFIG (spec §3).
//   - Construction explicite des deux Dispatcher (content_core,
//     commerce_product_core — bloc par shard, pas de boucle générique :
//     monomorphisation), lancés en tokio::spawn nu.
//
// Phase 5.2 a ajouté le PgListener réactif (LISTEN/NOTIFY → Collector →
// Notify), toujours en tokio::spawn nu à ce stade.
//
// Phase 5.3 (cette session) remplace les trois tokio::spawn nus (deux
// Dispatcher + PgListener) par un JoinSet supervisé : tokio::select! entre
// axum::serve et tasks.join_next(), fail-fast (process::exit(1)) sur toute
// terminaison anormale d'une tâche supervisée — cf. fin de main().

//
// ROUTE_TABLE reste le STUB écrit à la main des phases précédentes (le
// compilateur de templates FragmentRef n'existe pas encore, hors périmètre).
// =============================================================================

mod handlers;

use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::{Extension, Router};
use tokio::sync::{Notify, Semaphore};

// Point de vérification à la compilation (en plus du Vecteur C ci-dessous) :
// Dispatcher/DispatcherConfig sont assumés réexportés à la racine de
// marius_render, par analogie avec IdSource/LiveRegistry/RouteEntry (déjà
// confirmés à cette racine par le main.rs Phase 3). Si marius_render les
// expose plutôt via un sous-module (`marius_render::dispatcher::Dispatcher`),
// cargo build échoue ici, explicitement, sur cet import — à corriger en
// remplaçant ce chemin par le chemin réel, rien d'autre n'en dépend.
use marius_render::{Dispatcher, DispatcherConfig, IdSource, LiveRegistry, RouteEntry};

// Vecteur C (handoff) : noms du shard `commerce_product_core` assumés par
// convention isomorphique stricte avec `content_core` (seule paire confirmée
// littéralement, cf. spec §3) — jamais vus écrits dans marius_schema généré.
// Si la forge a produit un nom différent, cargo build échoue explicitement
// sur cet import, pas un bug silencieux à l'exécution.
use marius_schema::{
    CommerceProductCoreProjection, ContentCoreProjection, COMMERCE_PRODUCT_CORE_COLLECTOR,
    COMMERCE_PRODUCT_CORE_TOTAL_CAP, CONTENT_CORE_COLLECTOR, CONTENT_CORE_TOTAL_CAP,
};

// Phase 5.2 : seul InsertResult est nommé ici. Collector<MAX, WORDS> reste
// non importé — chaque branche de run_pg_listener référence directement
// CONTENT_CORE_COLLECTOR / COMMERCE_PRODUCT_CORE_COLLECTOR (déjà statiques,
// déjà importés ci-dessus), jamais le type générique lui-même : aucun site
// n'a besoin de nommer Collector<_, _> pour appeler .insert().
use marius_collector::InsertResult;

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

/// Configuration par défaut des Dispatcher — littéral explicite, pas
/// `DispatcherConfig::default()` : `Default::default()` n'est pas garanti
/// const-évaluable, alors que `Duration::from_millis`/`from_secs` le sont
/// (handoff Vecteur B, spec §3). `const`, pas `static` : un `const` est
/// réinjecté par valeur à chaque site d'usage (`SHARDS` ci-dessous), aucune
/// indirection, aucune allocation.
const DEFAULT_DISPATCHER_CONFIG: DispatcherConfig = DispatcherConfig {
    tick_default:    Duration::from_millis(500),
    tick_min:        Duration::from_millis(100),
    tick_max:        Duration::from_secs(2),
    threshold_flush: 128,
    threshold_low:   10,
    threshold_high:  100,
    render_budget:   Duration::from_millis(200),
};

/// Faits non génériques par shard, rassemblés en un seul endroit pour qu'ils
/// ne dérivent jamais l'un de l'autre (spec §3). Le type concret
/// (`Projection`, `Collector<MAX,WORDS>`) et `total_cap` restent référencés
/// par leur nom généré à chaque site d'usage ci-dessous : ils ne sont pas des
/// valeurs portables dans une structure non générique — `Dispatcher` est
/// monomorphisé, pas itéré dynamiquement (spec §3, pas de boucle ici).
struct ShardMetadata {
    packfile_key: &'static str,
    /// Nom de canal LISTEN réel. Lu par `run_pg_listener` (Phase 5.2,
    /// cette session) : source unique pour `listen_all` et pour le
    /// routage par canal dans la boucle de réception.
    channel: &'static str,
    config: DispatcherConfig,
}

static SHARDS: &[ShardMetadata] = &[
    ShardMetadata {
        packfile_key: "content_core",
        channel:      "content_core_updates",
        config:       DEFAULT_DISPATCHER_CONFIG,
    },
    ShardMetadata {
        packfile_key: "commerce_product_core",
        // Arbitrage architecte (cette session) : trigger DB déjà migré vers
        // la convention symétrique {packfile_key}_updates (spec §10, acté).
        // L'ancien nom "product_core_updates" (spec §2) n'est plus valide.
        channel:      "commerce_product_core_updates",
        config:       DEFAULT_DISPATCHER_CONFIG,
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

/// PgListener réactif (Phase 5.2). Boucle de reconnexion externe (invariant
/// de disponibilité, pas une optimisation) + boucle de réception interne.
/// Hors périmètre cette session : supervision JoinSet/select!/process::exit
/// (Phase 5.3) — tokio::spawn nu, comme les deux Dispatcher depuis 5.1.
///
/// Pas de match unifié sur les deux branches de canal (arbitrage acté) :
/// CONTENT_CORE_COLLECTOR et COMMERCE_PRODUCT_CORE_COLLECTOR sont deux types
/// Collector<MAX, WORDS> distincts (monomorphisation) — un retour de branche
/// commun échouerait à la compilation (E0308) sans dyn, exclu explicitement.
/// Chaque branche reste donc autonome : routage, insert(), notify_one()
/// inline, parsing du payload dupliqué (4 lignes/branche, accepté — même
/// discipline que la construction des Dispatcher en 5.1).
async fn run_pg_listener(
    database_url: String,
    content_core_notify: Arc<Notify>,
    commerce_product_core_notify: Arc<Notify>,
) {
    let mut backoff = Duration::from_millis(500);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    loop {
        let mut listener = match sqlx::postgres::PgListener::connect(&database_url).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[pg_listener] connexion échouée: {e} — retry dans {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };

        let channels: Vec<&str> = SHARDS.iter().map(|s| s.channel).collect();
        if let Err(e) = listener.listen_all(channels).await {
            eprintln!("[pg_listener] listen_all échoué: {e} — retry dans {backoff:?}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
            continue;
        }
        backoff = Duration::from_millis(500); // reset après succès
        eprintln!("[pg_listener] abonné — {} canal(aux)", SHARDS.len());

        loop {
            match listener.recv().await {
                Ok(notification) => match notification.channel() {
                    c if c == SHARDS[0].channel => {
                        let Ok(id) = notification.payload().parse::<i64>() else {
                            eprintln!(
                                "[pg_listener] payload non numérique sur {}: {:?}",
                                notification.channel(),
                                notification.payload()
                            );
                            continue;
                        };
                        if CONTENT_CORE_COLLECTOR.insert(id, SHARDS[0].config.threshold_flush)
                            == InsertResult::ThresholdReached
                        {
                            content_core_notify.notify_one();
                        }
                    }
                    c if c == SHARDS[1].channel => {
                        let Ok(id) = notification.payload().parse::<i64>() else {
                            eprintln!(
                                "[pg_listener] payload non numérique sur {}: {:?}",
                                notification.channel(),
                                notification.payload()
                            );
                            continue;
                        };
                        if COMMERCE_PRODUCT_CORE_COLLECTOR
                            .insert(id, SHARDS[1].config.threshold_flush)
                            == InsertResult::ThresholdReached
                        {
                            commerce_product_core_notify.notify_one();
                        }
                    }
                    other => {
                        eprintln!("[pg_listener] canal inattendu: {other}");
                    }
                },
                Err(e) => {
                    eprintln!("[pg_listener] connexion perdue: {e} — reconstruction");
                    break; // ressort vers la boucle externe : reconnexion + ré-abonnement complets
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Ressources globales (spec §4) ───────────────────────────────────────
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = sqlx::PgPool::connect(&database_url).await?;
    eprintln!("[marius-server] PgPool connecté"); // jamais l'URL en clair (identifiants)

    let io_permits: usize = std::env::var("MARIUS_IO_PERMITS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4);
    let io_semaphore = Arc::new(Semaphore::new(io_permits));
    eprintln!("[marius-server] Arc<Semaphore> initialisé — {io_permits} permis I/O");

    // ── Provisioning de l'espace de projection (spec-provisioning-projection)
    // Un environnement vierge (aucun fichier sous artifacts/) n'est pas une
    // erreur : c'est l'état initial légitime d'un espace de projection pas
    // encore matérialisé (spec-provisioning §1). Tout le reste — packfile
    // présent mais corrompu — reste fatal via cold_start ci-dessous,
    // inchangé : ensure_provisioned() ne fait que garantir qu'un fichier
    // existe sous une forme au moins valide-vide, jamais qu'il est correct.
    for route in ROUTE_TABLE {
        match marius_render::ensure_provisioned(route.packfile_key).await? {
            marius_render::ProvisionOutcome::Provisioned => eprintln!(
                "[marius-server] espace de projection provisionné (vierge) — shard \"{}\"",
                route.packfile_key
            ),
            marius_render::ProvisionOutcome::AlreadyPresent => {}
        }
    }

    // Cold start : mmap eager de chaque index connu, fd ouverts — tout le
    // coût d'initialisation payé une fois, avant d'accepter la première
    // connexion (spec §5/Phase 3). Échec fatal si un packfile référencé par
    // ROUTE_TABLE est introuvable — pas de dégradation silencieuse.
    // Arc construit une seule fois ici, jamais reconstruit au point d'appel
    // (handoff : registre partagé entre build_router et les Dispatcher).
    let registry = Arc::new(LiveRegistry::cold_start(ROUTE_TABLE)?);
    eprintln!(
        "[marius-server] cold_start réussi — {} route(s) enregistrée(s)",
        ROUTE_TABLE.len()
    );

    // ── Dispatcher — shard content_core ─────────────────────────────────────
    // Un seul Arc<Notify> par shard, jamais reconstruit. Phase 5.2 lui donne
    // un second consommateur (run_pg_listener, même Arc) : .clone() ici,
    // binding d'origine déplacé plus bas dans le spawn du listener (dernier
    // usage).
    let content_core_notify: Arc<Notify> = Arc::new(Notify::new());

    // Turbofish explicite sur P (`ContentCoreProjection`) : aucun paramètre
    // de Dispatcher::new ne porte ce type (PhantomData<P> est interne, pas
    // un argument), donc rien ne permet à l'inférence de le déduire sans
    // annotation — sans ça, "type annotations needed" (E0282). MAX/WORDS
    // restent en `_` : déduits sans ambiguïté depuis le type concret de
    // `&CONTENT_CORE_COLLECTOR`.
    let content_core_dispatcher = Dispatcher::<ContentCoreProjection, _, _>::new(
        &CONTENT_CORE_COLLECTOR,
        content_core_notify.clone(),
        pool.clone(),
        SHARDS[0].config,
        CONTENT_CORE_TOTAL_CAP,
        registry.clone(),
        SHARDS[0].packfile_key,
        io_semaphore.clone(),
    );
    // ── Supervision fail-fast (spec §6, Phase 5.3) ──────────────────────────
    // Les trois tâches ci-dessous (deux Dispatcher + PgListener) ne sont
    // jamais censées se terminer normalement — cf. tokio::select! en fin de
    // main() pour le traitement de toute terminaison comme un bug.
    let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    tasks.spawn(content_core_dispatcher.run());
    eprintln!(
        "[marius-server] Dispatcher démarré — shard \"{}\"",
        SHARDS[0].packfile_key
    );

    // ── Dispatcher — shard commerce_product_core ────────────────────────────
    // Dernier usage de `pool` et de `io_semaphore` dans cette fonction :
    // déplacés (pas de .clone()) — un seul clone de chaque ressource
    // partagée suffit pour deux consommateurs (move sur le second).
    // `commerce_product_core_notify` suit la même règle que content_core_notify
    // ci-dessus : .clone() ici (second consommateur = run_pg_listener),
    // binding d'origine déplacé plus bas.
    let commerce_product_core_notify: Arc<Notify> = Arc::new(Notify::new());

    let commerce_product_core_dispatcher = Dispatcher::<CommerceProductCoreProjection, _, _>::new(
        &COMMERCE_PRODUCT_CORE_COLLECTOR,
        commerce_product_core_notify.clone(),
        pool,
        SHARDS[1].config,
        COMMERCE_PRODUCT_CORE_TOTAL_CAP,
        registry.clone(),
        SHARDS[1].packfile_key,
        io_semaphore,
    );
    tasks.spawn(commerce_product_core_dispatcher.run());
    eprintln!(
        "[marius-server] Dispatcher démarré — shard \"{}\"",
        SHARDS[1].packfile_key
    );

    // ── PgListener (spec §5, Phase 5.2) ─────────────────────────────────────
    // database_url et les deux Arc<Notify> d'origine : derniers usages,
    // déplacés ici sans .clone() (Correctif 1 ci-dessus a déjà couvert le
    // second consommateur côté Dispatcher). tasks.spawn (Phase 5.3) au lieu
    // d'un tokio::spawn nu — même supervision fail-fast que les deux
    // Dispatcher ci-dessus.
    tasks.spawn(run_pg_listener(
        database_url,
        content_core_notify,
        commerce_product_core_notify,
    ));
    eprintln!("[marius-server] PgListener démarré — backoff 500ms→30s");

    // ── Read Path (spec §7, inchangé) ───────────────────────────────────────
    // Dernier usage de `registry` : déplacé, pas cloné.
    let app = build_router(ROUTE_TABLE, registry);

    let bind_addr = std::env::var("MARIUS_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    eprintln!("[marius-server] HTTP sur http://{bind_addr}");

    // ── Supervision fail-fast (spec §6, actée) ───────────────────────────────
    // Aucune des trois tâches n'est censée se terminer normalement : run()
    // boucle indéfiniment, run_pg_listener aussi (sa boucle de reconnexion
    // est interne). Une terminaison, quelle qu'elle soit, est un bug — le
    // processus entier s'arrête bruyamment plutôt que de continuer à servir
    // des lectures avec un shard figé sans signal. Pas de redémarrage
    // silencieux (cf. Arbitrage / Hors scope, handoff Phase 5.3).
    tokio::select! {
        result = axum::serve(listener, app) => {
            result?;
        }
        Some(finished) = tasks.join_next() => {
            match finished {
                Ok(()) => eprintln!(
                    "[supervisor] une tâche supervisée s'est arrêtée normalement \
                     — ne devrait jamais arriver"
                ),
                Err(join_err) => eprintln!(
                    "[supervisor] une tâche supervisée a paniqué: {join_err}"
                ),
            }
            std::process::exit(1);
        }
    }
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
//
// Inchangés par Phase 5.1 — n'exercent que build_router/cold_start, jamais
// les Dispatcher (aucune dépendance Postgres dans ce module de test).
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
