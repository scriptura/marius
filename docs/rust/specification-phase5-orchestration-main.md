# Spécification — Phase 5 : Orchestration Globale (`main.rs`)

Statut : spécification, pas d'implémentation. Périmètre verrouillé après audit croisé de `dispatcher.rs`, `regenerate.rs`, `batch_renderer.rs`, `collector.rs`, `lib.rs` (marius_projection), `specification-db-forge.md`, `specification-marius-render-shell.md`, et `db/dml/triggers_notify_dml.sql`. Tout élément ci-dessous est tracé à un fichier réel lu, pas déduit par hypothèse. Suite à revue (audit croisé), la résilience du `PgListener` et la supervision des `Dispatcher` sont actées comme invariants de disponibilité, pas comme dette différée à l'implémentation — seule l'asymétrie de nommage des canaux (§2) reste une recommandation non actée, sans impact sur la correction du système.

---

## 0. Périmètre

**Dans le périmètre de cette phase :**
- Ressources globales : `PgPool`, `Arc<Semaphore>` (I/O), `LiveRegistry` (cold start).
- `PgListener` : écoute LISTEN/NOTIFY, routage vers le `Collector` du shard concerné.
- Construction et lancement (`tokio::spawn`) d'un `Dispatcher` par shard à `Collector`.
- Composition avec le serveur Axum (Read Path), déjà fonctionnel et inchangé.
- Nettoyage ciblé de la documentation périmée dans `dispatcher.rs`.

**Hors périmètre, explicitement :**
- Shards de composition de page (`pages_homepage` et assimilés) — pas de `Collector` généré par `db-forge` pour eux (PK non simple/entière ou absence de table 1:1), donc pas de `Dispatcher` en Phase 5. Leur mécanisme de régénération (cascade ADR-008 §5) reste différé.
- Logique interne de `merge_sweep`, `apply_merge_io_sync`, `regenerate_and_swap`, `BatchRenderer` — validées Phase 4, non retouchées ici.
- Logique interne des fonctions trigger SQL existantes (`content.notify_core_change`, `commerce.notify_product_change`) — validées, non retouchées, à l'exception d'une recommandation de cohérence de nommage documentée séparément (§10) et non actée.

**Shards couverts par cette phase (les deux seuls à disposer d'un trigger réel et d'un `Collector`) :**

| packfile_key | Table Postgres | Projection générée | Canal LISTEN réel |
|---|---|---|---|
| `content_core` | `content.core` | `ContentCoreProjection` | `content_core_updates` |
| `commerce_product_core` | `commerce.product_core` | *(nom exact à confirmer par analogie — voir §3)* | `product_core_updates` |

---

## 0bis. Alignement avec le Manifeste de la Projection Réactive

Le manifeste (`manifest-reactive-projection.md`, révisé 22 juin 2026) pose le pipeline conceptuel en 7 étapes (§6). Cette spécification l'implémente, mais s'organise par ordre de construction dans `main.rs`, pas par étape conceptuelle — la correspondance ci-dessous relie les deux lectures sans dupliquer la structure du document.

| Étape du manifeste | Réalisée par |
|---|---|
| 1-2. Mutation DB → Trigger → `pg_notify` | `db/dml/triggers_notify_dml.sql` (existant, documenté §1) |
| 3. Capture (écouteur async → table de présence) | `PgListener` (§5) → `Collector::insert` |
| 4. Dispatch (seuil/tick → IDs uniques) | `Collector::flush()` + `Dispatcher::run()` (§6) |
| 5. Extraction Data | `Projection::fetch_batch` (Phase 4) — **divergence**, voir note |
| 6. Projection AOT | `BatchRenderer::render_batch` (Phase 4) — **divergence**, voir note |
| 7. Persistance (remplacement atomique) | `apply_merge_io_sync` : `merge_sweep` + `align8` + footer + `rename` (Phase 4) |
| *(absente du manifeste)* | Servir : serveur Axum, Read Path (§7) |

**Deux divergences entre le manifeste et le système réellement implémenté, à corriger dans le manifeste plutôt qu'à reproduire ici :**
- Étape 5 : le manifeste décrit une extraction par `SELECT` Postgres. Le chemin réactif réel lit `store.bin` via `mmap` (`fetch_batch`, zéro-allocation) — `fetch_from_pg` (le vrai accès SQL) n'est utilisé que par `marius-dump`, hors cycle réactif.
- Étape 6 : le manifeste décrit un rendu distribué sur tous les cœurs via Rayon. `BatchRenderer::render_batch` est strictement séquentiel (buffer unique réutilisé, par construction anti-allocation) — pas de parallélisme intra-batch dans le chemin réel actuel.

---

## 1. Contrat SQL réel — ce que `triggers_notify_dml.sql` fait réellement

Ce trigger existe déjà et fonctionne ; cette section documente son contrat, elle ne le conçoit pas.

- **Deux fonctions dédiées**, pas une fonction générique paramétrée : `content.notify_core_change()` et `commerce.notify_product_change()`. Chacune connaît son canal et sa colonne PK en dur.
- **Un canal par shard**, pas un canal partagé : `content_core_updates`, `product_core_updates`.
- **Payload = l'ID brut en texte**, pas une chaîne composite : `pg_notify(canal, COALESCE(NEW.id, OLD.id)::text)`.
- **DELETE notifie aussi**, avec l'ID de la ligne supprimée (`OLD.id`) — cohérent avec le contrat déjà établi en Phase 4.2 : un id absent du résultat de `fetch_batch` est traité comme suppression.

Conséquence pour le `PgListener` (§5) : le routage shard se fait sur le **nom du canal** (`PgNotification::channel()`), jamais par parsing d'un payload composite. Plus simple et plus robuste que la proposition initiale à canal unique.

---

## 2. Décision à trancher — asymétrie de nommage des canaux

`content_core_updates` suit la convention `{packfile_key}_updates`. `product_core_updates` ne la suit pas (`packfile_key` réel : `commerce_product_core`) — le préfixe de schéma manque.

Cette spécification ne dépend pas de la résolution de ce point : la table de configuration unifiée (§3) référence les canaux par leur nom **littéral actuel**, asymétrie comprise. Recommandation séparée, non actée : renommer `product_core_updates` → `commerce_product_core_updates` dans `triggers_notify_dml.sql` pour une convention uniforme, à exécuter (ou non) indépendamment de cette phase.

---

## 3. Table de configuration unifiée par shard

**Ce n'est pas une structure de données homogène itérée dynamiquement.** `Dispatcher<P: Projection, const MAX, const WORDS>` est générique ; chaque shard produit un type concret différent (monomorphisation). Il n'existe pas de boucle Rust unique sur une liste runtime qui construirait les deux `Dispatcher` — c'est un bloc de code explicite par shard, suivant un patron identique, pas une abstraction d'exécution. La "table unifiée" est documentaire : la garantie qu'elle apporte, c'est qu'un seul endroit du code rassemble les sept faits suivants par shard, pour qu'ils ne dérivent jamais l'un de l'autre :

1. `packfile_key` (doit correspondre à `ROUTE_TABLE`, lecture).
2. Type concret `Projection` (`marius_schema::{Nom}Projection`).
3. `static` `Collector` généré par `db-forge` (`marius_schema::{SCREAMING}_COLLECTOR`).
4. Constante de capacité (`marius_schema::{SCREAMING}_TOTAL_CAP`).
5. `DispatcherConfig` (par défaut `DispatcherConfig::default()`, confirmé existant avec des valeurs concrètes — `tick_default: 500ms, tick_min: 100ms, tick_max: 2s, threshold_flush: 128, threshold_low: 10, threshold_high: 100, render_budget: 200ms` — sauf override explicite par shard si nécessaire).
6. `Arc<Notify>` — un par shard, partagé entre le `Dispatcher` et le `PgListener`.
7. Nom(s) de canal(aux) LISTEN réel(s) pour ce shard (§1).

**Esquisse pour le shard confirmé (`content_core`) :**

```rust
use marius_schema::{ContentCoreProjection, CONTENT_CORE_COLLECTOR, CONTENT_CORE_TOTAL_CAP};

let content_core_notify: Arc<Notify> = Arc::new(Notify::new());
// packfile_key: "content_core", canal: "content_core_updates"
```

**Pour `commerce_product_core` : nom exact non vérifié littéralement** — `CommerceProductCoreProjection`/`COMMERCE_PRODUCT_CORE_COLLECTOR`/`COMMERCE_PRODUCT_CORE_TOTAL_CAP` par application de la même convention que `content_core` (confirmée dans les tests de `dispatcher.rs`), mais je n'ai pas vu ce nom écrit littéralement dans un fichier généré. **À confirmer dans `marius_schema` avant la première compilation**, pas une supposition à figer dans le code sans vérification.

**Factorisation des faits non génériques (suite à revue).** Les éléments 1, 7, et la configuration (5) ne portent aucun paramètre de type — ils peuvent être centralisés dans une structure statique unique, sans toucher au caractère monomorphisé des `Dispatcher` eux-mêmes (2-4, 6 restent par construction des blocs explicites par shard) :

```rust
const DEFAULT_DISPATCHER_CONFIG: DispatcherConfig = DispatcherConfig {
    tick_default:    Duration::from_millis(500),
    tick_min:        Duration::from_millis(100),
    tick_max:        Duration::from_secs(2),
    threshold_flush: 128,
    threshold_low:   10,
    threshold_high:  100,
    render_budget:   Duration::from_millis(200),
}; // littéral explicite, pas DispatcherConfig::default() — Default::default()
   // n'est pas garanti const-évaluable ; Duration::from_millis/from_secs le sont.

struct ShardMetadata {
    packfile_key: &'static str,
    channel:      &'static str,
    config:       DispatcherConfig,
}

static SHARDS: &[ShardMetadata] = &[
    ShardMetadata {
        packfile_key: "content_core",
        channel:      "content_core_updates",
        config:       DEFAULT_DISPATCHER_CONFIG,
    },
    ShardMetadata {
        packfile_key: "commerce_product_core",
        channel:      "product_core_updates", // nom réel actuel, cf. §2
        config:       DEFAULT_DISPATCHER_CONFIG,
    },
];
```

`SHARDS` devient la source unique pour : la liste de canaux passée à `listen_all` (§5), le routage par canal dans la boucle de réception (§5), et le `packfile_key`/`config` passés à chaque `Dispatcher::new()` (§6). Une chaîne ne peut plus diverger entre ces trois points d'usage — elle n'existe plus qu'à un seul endroit. Le type concret (`Projection`, `Collector<MAX,WORDS>`) et `total_cap` restent référencés par leur nom généré à chaque site d'usage, puisqu'ils ne sont pas des valeurs portables dans une structure non générique.

---

## 4. Ressources globales

```rust
let database_url = std::env::var("DATABASE_URL")?;
let pool = sqlx::PgPool::connect(&database_url).await?;

let io_permits: usize = std::env::var("MARIUS_IO_PERMITS")
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(4); // défaut convenu — file NVMe saine, à ajuster par profiling
let io_semaphore = Arc::new(tokio::sync::Semaphore::new(io_permits));

let registry = Arc::new(LiveRegistry::cold_start(ROUTE_TABLE)?);
```

`cold_start` reste un échec fatal au boot (comportement actuel inchangé — aucune dégradation silencieuse si un packfile référencé par `ROUTE_TABLE` est introuvable).

---

## 5. `PgListener` — invariant de disponibilité

Décision actée suite à revue : **tant que le processus vit, un `PgListener` abonné aux canaux des shards doit exister.** Ce n'est pas une optimisation — un `PgListener` mort signifie un serveur qui répond toujours en lecture mais ne projette plus jamais aucune mutation, sans qu'aucune erreur visible ne le signale. Le mécanisme doit donc être auto-réparateur, et **ne dépend pas** du comportement interne réel (documenté ou non) de `sqlx::postgres::PgListener` face à une coupure — la défense est construite à la couche application, par-dessus, quel que soit ce comportement :

```rust
async fn run_pg_listener(database_url: String) {
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

        loop {
            match listener.recv().await {
                Ok(notification) => {
                    let routing = match notification.channel() {
                        c if c == SHARDS[0].channel =>
                            Some((&CONTENT_CORE_COLLECTOR as &'static Collector<_, _>,
                                  SHARDS[0].config.threshold_flush, &content_core_notify)),
                        c if c == SHARDS[1].channel =>
                            Some((&COMMERCE_PRODUCT_CORE_COLLECTOR as &'static Collector<_, _>,
                                  SHARDS[1].config.threshold_flush, &product_core_notify)),
                        other => { eprintln!("[pg_listener] canal inattendu: {other}"); None }
                    };
                    let Some((collector, threshold, notify)) = routing else { continue };

                    let Ok(id) = notification.payload().parse::<i64>() else {
                        eprintln!("[pg_listener] payload non numérique sur {}: {:?}",
                            notification.channel(), notification.payload());
                        continue;
                    };

                    if collector.insert(id, threshold) == InsertResult::ThresholdReached {
                        notify.notify_one();
                    }
                }
                Err(e) => {
                    eprintln!("[pg_listener] connexion perdue: {e} — reconstruction");
                    break; // ressort vers la boucle externe : reconnexion + ré-abonnement complets
                }
            }
        }
    }
}
```

**Conséquence sur le démarrage** : cette tâche est `tokio::spawn`-ée immédiatement, sans bloquer le boot sur sa première connexion réussie. Si Postgres est temporairement indisponible au démarrage, le serveur démarre quand même en lecture, et le pipeline réactif se rétablit seul en arrière-plan, de façon journalisée. Ça résout aussi l'ancien point ouvert du §8 ("fatal au boot ou dégradé ?") : ce n'est plus un choix binaire, c'est toujours nominal en façade, toujours auto-réparateur en interne.

**Ce qui reste une vraie défaillance, pas une coupure transitoire** : si cette tâche elle-même se termine ou panique (jamais censé arriver, la boucle externe est infinie), ce n'est plus une coupure réseau à absorber — c'est un bug. Traité par la supervision globale (§6), pas par cette boucle.

**Note de conception confirmée** : le payload étant un entier brut (§1), `notification.payload().parse::<i64>()` suffit — aucun parsing de chaîne composite.

---

## 6. Construction, lancement et supervision des `Dispatcher`

Un bloc explicite par shard (pas de boucle générique — cf. §3), mais plus de `tokio::spawn` isolé : tous les `JoinHandle` (Dispatchers et tâche `PgListener`) sont désormais possédés par un superviseur unique.

```rust
let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

let content_core_dispatcher = Dispatcher::new(
    &CONTENT_CORE_COLLECTOR,
    content_core_notify.clone(),
    pool.clone(),
    SHARDS[0].config,
    CONTENT_CORE_TOTAL_CAP,
    registry.clone(),
    SHARDS[0].packfile_key,
    io_semaphore.clone(),
);
tasks.spawn(content_core_dispatcher.run());

// Bloc structurellement identique pour commerce_product_core, type concret
// différent (monomorphisation) — cf. §3 pour le nom exact à confirmer.

tasks.spawn(run_pg_listener(database_url.clone()));
```

`pool.clone()` : `sqlx::PgPool` est interne à un `Arc`, clone bon marché. `registry.clone()` : `Arc<LiveRegistry>`, idem.

**Décision actée suite à revue : supervision fail-fast, pas de redémarrage silencieux en place.** Cohérent avec le reste du système (`cold_start` fatal, `regenerate_and_swap` qui panique sur clé non provisionnée) — aucune des tâches supervisées n'est censée se terminer normalement, `run()` boucle indéfiniment par construction. Si l'une d'elles se termine ou panique, ce n'est jamais une dégradation locale acceptable : le processus entier s'arrête bruyamment, plutôt que de continuer à servir des lectures avec un shard figé sans que rien ne le signale.

```rust
tokio::select! {
    result = axum::serve(listener, app) => {
        result?;
    }
    Some(finished) = tasks.join_next() => {
        match finished {
            Ok(()) => eprintln!("[supervisor] une tâche supervisée s'est arrêtée normalement — ne devrait jamais arriver"),
            Err(join_err) => eprintln!("[supervisor] une tâche supervisée a paniqué: {join_err}"),
        }
        std::process::exit(1);
    }
}
```

Reconstruction automatique en place (respawn) délibérément écartée : elle masquerait un bug réel sous un comportement qui ressemble à du fonctionnement normal. La restauration de service après un arrêt du processus relève de la supervision externe (systemd, orchestrateur de conteneurs) — hors périmètre de `main.rs`.

---

## 7. Serveur Axum (Read Path)

Inchangé : `build_router(ROUTE_TABLE, registry.clone())`, `axum::serve(...)`. Cette phase compose ce composant existant avec les nouveaux (§4-6) dans le même `main()`, sans le modifier.

---

## 8. Ordre de démarrage

1. `PgPool::connect` (fatal si échec — sans base, rien ne peut fonctionner).
2. `LiveRegistry::cold_start` (fatal si échec — comportement actuel, inchangé).
3. Constitution du `JoinSet` : spawn des `Dispatcher` et de la tâche `run_pg_listener` (§5, §6). **L'ordre relatif entre ces deux familles de tâches n'a pas d'impact sur la correction**, pour deux raisons indépendantes, vérifiées plutôt que supposées :
   - `tokio::sync::Notify::notify_one()` appelé alors qu'aucune tâche n'est encore en attente sur `notified()` stocke un permis unique, consommé immédiatement par le prochain appel à `notified()` — ce permis n'est jamais perdu (sémantique documentée de `Notify`, pas une supposition).
   - `Collector` est une structure statique persistante (bit-vector), indépendante de tout consommateur : `insert()` modifie son état quel que soit l'instant, `flush()` retourne tout l'état accumulé au moment de l'appel, peu importe combien de temps s'est écoulé depuis les `insert()`. Aucune notification ni aucun id ne peut être "perdu" par un démarrage du `Dispatcher` postérieur à celui du `PgListener`.
4. `axum::serve` concourant avec la supervision du `JoinSet` via `tokio::select!` (§6) — la première des deux branches à se terminer détermine l'arrêt du processus.

---

## 9. Nettoyage `dispatcher.rs`

Le bloc d'en-tête (commentaire "Modification Phase 4 — destruction de l'Append", incluant le paragraphe posant comme non résolue la question *"si `Collector::flush()` retourne un delta... à confirmer une fois marius-collector lu"*) est de la documentation périmée : `regenerate.rs` résout explicitement cette question et la teste (`untouched_entities_survive_successive_incremental_merges_then_delete`). À condenser en un état résolu factuel. **Ne pas toucher** : `run()`, `adapt_tick()`, les champs de la struct, `DispatcherConfig` — tous à jour et corrects. Le commentaire du champ `total_cap` ("Contrat de capacité pour le `BatchRenderer` interne") reste exact (`fetch_delta_batch` instancie bien un `BatchRenderer`) — ne pas le modifier.

---

## 10. Recommandation séparée, non actée

Renommage de `product_core_updates` → `commerce_product_core_updates` dans `triggers_notify_dml.sql`, pour que les deux shards suivent la même convention `{packfile_key}_updates`. N'affecte pas la table de configuration unifiée (§3), qui référence les canaux par leur valeur littérale quel que soit le nom retenu — recommandation de cohérence, pas un prérequis technique.

---

## 11. Critères de validation de cette phase

- Démarrage à froid : les deux `Dispatcher` démarrent, le `PgListener` écoute les deux canaux réels, le serveur Axum répond, sans intervention manuelle.
- Bout-en-bout : un `INSERT`/`UPDATE`/`DELETE` réel sur `content.core` ou `commerce.product_core` déclenche, dans l'ordre, `pg_notify` → réception `PgListener` → `Collector::insert` → (si seuil atteint) `notify_one()` → réveil du `Dispatcher` concerné → `regenerate_and_swap` → nouvelle génération servie en lecture, **sans redémarrage du processus**.
- Un shard sans trigger (`pages_homepage`) n'a ni `Collector`, ni `Dispatcher`, ni entrée dans `SHARDS` — confirmé absent du périmètre, pas oublié.
- **Résilience du `PgListener`** : couper la connexion Postgres en cours de fonctionnement (ex. `pg_terminate_backend` sur la session du listener) — le serveur continue de répondre en lecture, le listener se reconnecte et reprend la réception dans la fenêtre de `backoff`, sans redémarrage du processus, avec trace journalisée de la coupure et de la reprise.
- **Supervision fail-fast** : provoquer un panic dans la boucle d'un `Dispatcher` (injection de test) — le processus entier se termine avec un code de sortie non nul, sans rester dans un état où certaines routes répondent normalement et d'autres servent des données figées sans signal.
- Démarrer le `JoinSet` (Dispatchers + listener) avant ou après n'a aucun effet observable sur la livraison des premières mutations en attente — à vérifier explicitement par un test d'ordre inversé, pas seulement par lecture du raisonnement (§8).
