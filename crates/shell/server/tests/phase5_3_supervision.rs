// =============================================================================
// crates/shell/server/tests/phase5_3_supervision.rs
//
// Phase 5.3 — Supervision & Résilience. Deux tests, deux modèles distincts,
// volontairement non mélangés (cf. handoff Phase 5.3, arbitrage point 3) :
//
//   1. fail_fast_panic_in_dispatcher_terminates_process (#[test], synchrone)
//      Sous-processus réel : binaire marius complet, observation externe via
//      try_wait() — pas par lecture du code. Prouve le câblage fail-fast de
//      bout en bout (panic → JoinError → process::exit(1)).
//
//   2. startup_order_does_not_lose_pending_signal (#[tokio::test])
//      In-process, concentré exclusivement sur Notify et Collector — aucun
//      Dispatcher réel, aucun PgPool, aucun socket. Prouve les deux contrats
//      du §8 de la spec indépendamment de tout consommateur.
// =============================================================================

use std::process::Command;
use std::time::{Duration, Instant};

// -----------------------------------------------------------------------------
// Test 1 — fail-fast sur panic, observé depuis un processus séparé
// -----------------------------------------------------------------------------

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

// -----------------------------------------------------------------------------
// Test 2 — contrat d'ordre Notify / Collector, isolé de tout consommateur
// -----------------------------------------------------------------------------

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
