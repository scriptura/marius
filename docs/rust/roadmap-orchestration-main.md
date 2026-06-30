# Roadmap — Découpage de la Phase 5 (Orchestration `main.rs`)

Décision : **oui**, découpage en trois sous-phases. Justification par classe de risque, pas par volume de code — le code de Phase 5 est modeste (~150-250 lignes neuves), mais trois sous-ensembles appellent des boucles de debug de nature différente, exactement comme 4.1/4.2/4.3 l'ont déjà imposé pour des raisons analogues.

---

## Phase 5.1 — Ressources & Dispatchers (risque faible)

**Contenu** :
- Nettoyage du bloc d'en-tête périmé de `dispatcher.rs` (§9 de la spec) — fait en premier, zéro risque, prime la session.
- `PgPool::connect`, `Arc<Semaphore>` (+ `MARIUS_IO_PERMITS`), `LiveRegistry::cold_start`.
- `ShardMetadata`, `DEFAULT_DISPATCHER_CONFIG`, `SHARDS` — y compris la vérification des noms générés réels pour `commerce_product_core` dans `marius_schema` (seul point d'incertitude restant, borné : erreur de compilation immédiate et explicite si le nom est faux, pas un bug silencieux).
- Construction des deux `Dispatcher` (`Dispatcher::new(...)`), `tokio::spawn` simple (la supervision `JoinSet` arrive en 5.3, pas ici).
- Composition avec le `build_router`/`axum::serve` déjà existants — aucune modification du Read Path.

**Ce qui n'est délibérément pas couvert ici** : aucun `PgListener`. Les `Collector` restent vides toute la session — c'est attendu, pas un bug à chasser.

**Jalon 5.1** :
- `cargo build` propre, aucun warning sur le bloc nettoyé.
- Démarrage réel : les deux `Dispatcher` tournent (tick visible en log), le serveur répond sur les routes existantes exactement comme avant — régression nulle sur le Read Path.
- Vérification explicite, pas supposée : plusieurs ticks s'écoulent avec `Collector` vide, sans `flush` parasite, sans appel à `regenerate_and_swap`.

---

## Phase 5.2 — `PgListener` réactif (risque élevé : premier système externe réel)

**Prérequis à vérifier avant de démarrer cette session** : une instance Postgres réellement accessible, migrations et `triggers_notify_dml.sql` déjà appliqués, au moins une ligne existante dans `content.core` et `commerce.product_core` pour tester une mutation réelle. Sans ça, la session se bloque immédiatement sur de l'environnement, pas sur du code.

**Contenu** :
- Boucle `run_pg_listener` complète : connexion, `listen_all` via `SHARDS`, boucle de réception, routage par canal, parsing payload, `Collector::insert` + `notify_one`.
- Boucle externe de reconnexion avec backoff (l'invariant de disponibilité acté dans la spec) — c'est le morceau le plus délicat à déboguer correctement : il faut une vraie coupure pour le valider, pas juste une relecture du code.
- Premier test bout-en-bout réel : `INSERT`/`UPDATE`/`DELETE` manuel (`psql`) → packfile régénéré → réponse HTTP mise à jour, sans redémarrage du processus.

**Jalon 5.2** :
- Une mutation manuelle sur l'une des deux tables se traduit, en quelques secondes et sans intervention, par un contenu HTTP à jour.
- `pg_terminate_backend` sur la session du listener, en cours de fonctionnement : reconnexion et reprise de la réception observées dans les logs, sans redémarrage du processus, dans la fenêtre de backoff attendue.

---

## Phase 5.3 — Supervision & Résilience (risque élevé : infrastructure de test nouvelle)

**Contenu** :
- `JoinSet` regroupant les deux `Dispatcher` et la tâche `PgListener`, `tokio::select!` avec `axum::serve`, `std::process::exit(1)` fail-fast sur toute terminaison inattendue.
- Harnais de test par sous-processus : spawn du binaire réel, injection contrôlée d'un panic dans un `Dispatcher` (flag de debug ou point d'injection dédié), assertion sur le code de sortie du processus — pas un test in-process.
- Test d'invariance de l'ordre de démarrage (§8 de la spec, claim sur `Notify`/`Collector`) : démarrer le `JoinSet` avant/après les premières notifications, vérifier qu'aucune mutation n'est perdue dans les deux ordres.

**Jalon 5.3** :
- Panic injecté dans un `Dispatcher` → processus entier sorti avec code non nul, vérifié par un test qui observe un processus séparé, pas par lecture du code.
- Les deux ordres de démarrage testés passent sans perte de mutation.
- Tous les critères de validation du §11 de la spécification sont couverts par un test automatisé exécutable, plus aucun n'est seulement "vérifié par le raisonnement".

---

## Hors découpage : pas une sous-phase

Le renommage de `product_core_updates` → `commerce_product_core_updates` (§10 de la spécification) est une migration SQL isolée, sans dépendance Rust, sans risque de debug. Elle ne mérite pas de numéro de phase — à faire quand vous voulez, indépendamment de 5.1/5.2/5.3.
