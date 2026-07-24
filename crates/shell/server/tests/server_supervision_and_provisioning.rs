//! crates/shell/server/tests/server_supervision_and_provisioning.rs
//!
//! Validation des invariants structurels du pipeline déterministe (ECS/DOD/AOT).
//! Trois contrats distincts, isolés mécaniquement :
//!
//!   1. fail_fast_panic_in_dispatcher_terminates_process : Supervision OS-level (synchrone).
//!      Prouve l'absence d'état zombie (zombie state). Le superviseur relaie l'erreur (JoinError)
//!      et délègue le redémarrage à l'orchestrateur externe via process::exit(1).
//!
//!   2. startup_order_does_not_lose_pending_signal : Déterminisme du Signal (asynchrone).
//!      Isole la synchronisation (Notify/Collector) des aléas du scheduler Tokio.
//!      Prouve que l'ordre d'exécution n'altère pas la cohérence de l'état (bit-vector).
//!
//!   3. provisioning_on_empty_environment_starts_cleanly_and_serves_404 : Projection AOT.
//!      Démarrage sur environnement vierge (cold start). Prouve qu'une absence
//!      de données précalculées ne génère pas d'exception d'infrastructure (Runtime Error 500)
//!      mais un Zero-State valide (404), garantissant le Data-Oriented Design (DOD).

use std::process::Command;
use std::time::{Duration, Instant};

// Test 1 — fail-fast sur panic, observé depuis un processus séparé

/// Démarre le binaire `marius` réel avec `MARIUS_DEBUG_PANIC_SHARD` positionné
/// sur le shard `content_core`, et observe depuis l'extérieur (try_wait(),
/// pas de lecture de code) que le processus entier se termine avec un code de
/// sortie non nul — preuve que le superviseur (JoinSet + tokio::select! +
/// process::exit(1), Correctif 2) réagit bien à la panique d'une tâche.
///
/// Synchrone (`#[test]`, pas `#[tokio::test]`) : le harnais lui-même n'a pas
/// besoin de runtime Tokio — seul le binaire enfant en démarre un.
#[test]
fn fail_fast_panic_in_dispatcher_terminates_process() {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "DATABASE_URL absent de l'environnement de test — prérequis \
             bloquant (identique à 5.2, non couvert par ce test lui-même) : \
             instance Postgres accessible, migrations + \
             triggers_notify_dml.sql appliqués, packfiles déjà présents sur \
             disque pour les trois entrées de ROUTE_TABLE (cold_start() est \
             fatal sinon, et le binaire doit démarrer jusqu'au bout pour que \
             ce test ait un sens)."
        )
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_marius"))
        .env("DATABASE_URL", &database_url)
        // Port éphémère, jamais interrogé par ce test : seul le code de
        // sortie du processus compte ici, pas le Read Path.
        .env("MARIUS_BIND", "127.0.0.1:0")
        .env("MARIUS_DEBUG_PANIC_SHARD", "content_core")
        .spawn()
        .expect("échec du spawn du binaire marius (CARGO_BIN_EXE_marius)");

    // tick_default du Dispatcher est 500ms (DEFAULT_DISPATCHER_CONFIG,
    // main.rs) : le panic injecté survient au tout premier tick, donc bien
    // avant le plafond de polling ci-dessous.
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const POLL_TIMEOUT: Duration = Duration::from_secs(5);

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait() a échoué") {
            break status;
        }
        if start.elapsed() >= POLL_TIMEOUT {
            // Pas de timeout silencieux : on tue le processus orphelin puis
            // on échoue explicitement, avec le diagnostic exact.
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "le processus n'a pas terminé dans la fenêtre de {POLL_TIMEOUT:?} \
                 — panic injecté non observé : fail-fast cassé, supervision \
                 absente, ou MARIUS_DEBUG_PANIC_SHARD non lu côté dispatcher.rs"
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    assert!(
        !status.success(),
        "le processus s'est terminé avec un code de sortie 0 — attendu : \
         non nul (panic dans Dispatcher::run() → JoinError → \
         std::process::exit(1) côté superviseur, Correctif 2)"
    );
}

// Test 2 — contrat d'ordre Notify / Collector, isolé de tout consommateur

/// §8 de la spec, deux contrats indépendants de tout Dispatcher réel :
///
///   - `Collector` : bit-vector statique — `insert()` puis `flush()` doivent
///     être corrects quel que soit l'instant relatif des deux appels (ici :
///     écriture avant lecture).
///   - `Notify` : `notify_one()` pose un permis qui n'est jamais perdu, qu'il
///     soit posé avant ("Ordre A") ou après ("Ordre B") l'attente sur
///     `notified()`.
///
/// `CONTENT_CORE_COLLECTOR` (static de production, marius_schema) est exclu
/// délibérément : partagée avec le reste du binaire, non isolée entre tests
/// exécutés en parallèle. Un `Collector` local, à capacité arbitraire petite,
/// suffit à isoler la propriété — aucune dépendance d'infrastructure.
#[tokio::test]
async fn startup_order_does_not_lose_pending_signal() {
    use marius_collector::Collector;
    use tokio::sync::Notify;

    // MAX = 64, WORDS = 1 : un seul mot de bit-vector (capacité arbitraire,
    // largement suffisante pour l'id de test ci-dessous).
    const MAX: usize = 64;
    const WORDS: usize = 1;
    const TEST_ID: i64 = 42;
    const ARBITRARY_THRESHOLD: usize = 1;

    // ── Ordre A : "signal avant attente" ────────────────────────────────────
    {
        // -- Collector : insert() puis flush(), écriture avant lecture. --
        let collector = Collector::<MAX, WORDS>::new_zeroed();
        let _ = collector.insert(TEST_ID, ARBITRARY_THRESHOLD);

        let ids = collector.flush();
        assert_eq!(ids.len(), 1, "flush() doit retourner exactement un id");
        assert_eq!(ids[0], TEST_ID, "flush() doit retourner l'id inséré");

        // flush() draine : un second appel immédiat doit être vide — sémantique
        // sur laquelle dispatcher.rs s'appuie (`if ids.is_empty() { continue; }`).
        let ids_after = collector.flush();
        assert!(
            ids_after.is_empty(),
            "flush() doit être idempotent après extraction de l'id"
        );

        // -- Notify : notify_one() posé avant tout notified(). --
        let notify = Notify::new();
        notify.notify_one(); // signal posé AVANT toute attente

        tokio::time::timeout(Duration::from_millis(200), notify.notified())
            .await
            .expect(
                "notify_one() appelé avant notified() doit débloquer le \
                 prochain notified().await immédiatement — permis perdu ?",
            );
    }

    // ── Ordre B : "attente avant signal" ────────────────────────────────────
    {
        let notify = std::sync::Arc::new(Notify::new());
        let waiter_notify = notify.clone();

        // Le consommateur s'enregistre sur notified() avant tout signal.
        let waiter = tokio::spawn(async move {
            waiter_notify.notified().await;
        });

        // Marge pour garantir que le waiter est bien enregistré sur
        // notified() avant l'appel à notify_one() ci-dessous — sans cette
        // marge, le test ne prouverait rien sur l'ordre B spécifiquement.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        notify.notify_one();

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("la tâche en attente n'a pas été rejointe dans le délai")
            .expect("la tâche en attente a paniqué");
    }
}

// Test 3 — provisioning idempotent : démarrage de bout en bout sur un
// environnement vierge (specification-provisioning-projection.md §8,
// handoff-provisioning-projection.md, mission point 4, second niveau).

/// Envoie une requête HTTP/1.1 minimale sur `addr` et retourne le code de
/// statut numérique de la ligne de réponse. Implémentation volontairement
/// nue (`std::net::TcpStream`, pas `reqwest`) : ce test tourne en
/// sous-processus synchrone (`#[test]`, pas de runtime Tokio dans le
/// harnais), et la disponibilité de la feature `blocking` de `reqwest` dans
/// les dev-dependencies du crate n'est pas confirmée — aucune raison
/// d'introduire une dépendance incertaine pour trois octets de ligne de
/// statut.
fn http_get_status_code(addr: &str, path: &str) -> std::io::Result<u16> {
    use std::io::{Read, Write};

    let mut stream = std::net::TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);

    let status_line = text
        .lines()
        .next()
        .ok_or_else(|| std::io::Error::other("réponse HTTP vide — aucune ligne de statut"))?;

    // "HTTP/1.1 404 Not Found" → second champ, séparé par des espaces.
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "ligne de statut HTTP non parseable: {status_line:?}"
            ))
        })
}

/// Démarre le binaire `marius` réel avec `MARIUS_ARTIFACTS_DIR` pointant
/// vers un répertoire temporaire vide — aucun packfile ne préexiste pour
/// aucune des trois entrées de ROUTE_TABLE. Preuve de bout en bout que :
///
///   1. le processus démarre jusqu'au bout sans l'erreur fatale
///      `cold_start: échec ouverture packfile ... No such file or
///      directory` qui motive cette spécification — observé par sondage
///      HTTP actif, pas par lecture de code (même discipline que le test 1
///      : observation externe du processus enfant) ;
///   2. les trois entrées de ROUTE_TABLE ont été provisionnées sur disque ;
///   3. une requête sur une route provisionnée mais vide ("/",
///      pages_homepage, entry_count == 0) répond 404 — pas 500 — cf.
///      spec-provisioning §8 : conséquence directe du format valide produit
///      par ensure_provisioned, branche NOT_FOUND déjà existante de
///      serve_route, zéro modification de handlers.rs.
///
/// Répertoire temporaire construit à la main (`std::env::temp_dir()` +
/// suffixe pid/horloge), pas via `tempfile::tempdir()` : disponibilité de
/// `tempfile` dans les dev-dependencies du crate non confirmée par lecture
/// directe d'un `Cargo.toml` non fourni à cette session — même réserve que
/// pour `reqwest::blocking` ci-dessus, résolue par une primitive standard
/// déjà utilisée ailleurs dans ce système pour le même besoin
/// (`unique_path`, regenerate.rs).
///
/// Port choisi par le harnais (bind-puis-drop d'un `TcpListener` éphémère),
/// pas par le sous-processus : `MARIUS_BIND` impose un port exact connu
/// d'avance, ce qui évite d'avoir à parser le port réel depuis la sortie
/// standard du binaire enfant — `eprintln!("[marius-server] HTTP sur
/// http://{bind_addr}")` n'imprime que la valeur fournie à `MARIUS_BIND`,
/// jamais un port résolu dynamiquement si on lui passait ":0". Fenêtre de
/// réutilisation de port théoriquement non nulle entre le drop de la sonde
/// et le bind du sous-processus — acceptable en environnement de test
/// isolé, compromis déjà accepté ailleurs dans ce système (`main.rs`,
/// `spawn_test_server`, qui contourne le problème autrement en restant
/// in-process).
#[test]
fn provisioning_on_empty_environment_starts_cleanly_and_serves_404() {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        panic!(
            "DATABASE_URL absent de l'environnement de test — prérequis bloquant \
             (identique aux deux tests précédents) : instance Postgres accessible, \
             migrations appliquées. Ce test n'exige en revanche AUCUN packfile \
             préexistant sous artifacts/ — c'est précisément ce qu'il vérifie."
        )
    });

    // Répertoire temporaire vide, dédié à ce test — isolation totale
    // vis-à-vis des fixtures réelles sous artifacts/, garantie par
    // construction via MARIUS_ARTIFACTS_DIR (handoff étape 1), sans
    // déplacement ni sauvegarde de fichiers existants.
    let unique_suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("horloge système valide")
        .as_nanos();
    let artifacts_dir = std::env::temp_dir().join(format!(
        "marius_provisioning_e2e_{}_{unique_suffix}",
        std::process::id()
    ));
    std::fs::create_dir_all(&artifacts_dir)
        .expect("création du répertoire artifacts/ temporaire pour le test");
    assert_eq!(
        std::fs::read_dir(&artifacts_dir)
            .expect("lecture du répertoire temporaire fraîchement créé")
            .count(),
        0,
        "précondition : le répertoire temporaire doit être vide avant le démarrage du processus"
    );

    // Port choisi par le harnais — voir doc ci-dessus.
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind d'un port éphémère pour sonder un port libre");
        probe.local_addr().expect("local_addr du port sondé").port()
    };
    let bind_addr = format!("127.0.0.1:{port}");

    let mut child = Command::new(env!("CARGO_BIN_EXE_marius"))
        .env("DATABASE_URL", &database_url)
        .env("MARIUS_BIND", &bind_addr)
        .env("MARIUS_ARTIFACTS_DIR", &artifacts_dir)
        .spawn()
        .expect("échec du spawn du binaire marius (CARGO_BIN_EXE_marius)");

    // Polling sur la disponibilité HTTP : le serveur n'émet aucun signal de
    // readiness consommable autrement que par sondage actif — même
    // discipline que le polling try_wait() du test 1, transposé à un socket
    // plutôt qu'à un code de sortie. À chaque itération, vérifie aussi que
    // le processus n'a pas terminé prématurément : un try_wait() positif
    // avant la première réponse HTTP est un échec du provisioning à
    // diagnostiquer immédiatement, pas un cas à confondre avec "pas encore
    // prêt".
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const POLL_TIMEOUT: Duration = Duration::from_secs(10);
    let url_path = "/";

    let start = Instant::now();
    let status_code = loop {
        if let Some(exit_status) = child.try_wait().expect("try_wait() a échoué") {
            panic!(
                "le processus marius s'est terminé prématurément ({exit_status:?}) avant \
                 de répondre sur {bind_addr}{url_path} — provisioning probablement en \
                 échec : vérifier que MARIUS_ARTIFACTS_DIR est bien lu par \
                 packfile_path_for() et que ensure_provisioned() est bien câblé avant \
                 cold_start() dans main.rs"
            );
        }

        match http_get_status_code(&bind_addr, url_path) {
            Ok(code) => break code,
            // Connexion refusée : le serveur n'écoute pas encore — normal au
            // tout début de la fenêtre de polling, pas une erreur en soi.
            Err(_) => {}
        }

        if start.elapsed() >= POLL_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "aucune réponse HTTP sur {bind_addr}{url_path} dans la fenêtre de \
                 {POLL_TIMEOUT:?} — le serveur n'a pas démarré jusqu'au bout \
                 (provisioning bloqué, ou cold_start toujours fatal sur environnement \
                 vierge)"
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    assert_eq!(
        status_code, 404,
        "GET / sur un packfile provisionné vide (pages_homepage, entry_count == 0) \
         doit répondre 404 — pas 500, pas 200 : conséquence directe du format valide \
         produit par ensure_provisioned, branche NOT_FOUND déjà existante de \
         serve_route (spec-provisioning-projection.md §8)"
    );

    let _ = child.kill();
    let _ = child.wait();

    // Les trois entrées de ROUTE_TABLE doivent avoir été provisionnées —
    // preuve directe sur le système de fichiers, pas seulement déduite du
    // succès du démarrage.
    for key in ["commerce_product_core", "content_core", "pages_homepage"] {
        let provisioned_path = artifacts_dir.join(format!("{key}.bin"));
        assert!(
            provisioned_path.exists(),
            "le packfile \"{key}\" doit avoir été provisionné sous {} — absent \
             après un démarrage réussi sur environnement vierge",
            artifacts_dir.display()
        );
    }

    let _ = std::fs::remove_dir_all(&artifacts_dir);
}
