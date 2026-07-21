# PHASE1-CLOSURE.md
## Réactivité varlena — pipeline Copy-on-Write à double `merge`

**Statut : clos.** Neuf étapes du Contrat d'Implémentation exécutées, vérifiées, testées. Le critère d'acceptation qui avait motivé l'ouverture de cette Phase 1 est démontré satisfait par exécution réelle, pas par relecture.

---

## 1. Objectif initial

Exercer de bout en bout le mécanisme varlena (TOC + heap, ADR-007) sur un premier champ réel (`content.body.content`), et vérifier qu'une mutation SQL se reflète en HTTP sans redémarrage du serveur — critère d'acceptation n°5 de `SPEC-phase0-varlena-et-js-deps.md`.

## 2. Problème découvert

En confrontant la documentation (manifeste, guide de cycle de vie, scénario, spec) au code réellement généré par `db-forge`, une contradiction structurelle est apparue : `fetch_batch` — le point d'entrée que `regenerate_and_swap` appelle réellement — ne faisait aucune requête SQL. Il lisait `store.bin` via un `OnceLock<PackfileReader<P>>` monté une seule fois pour toute la durée du processus. Le pipeline réactif (`NOTIFY` → `Dispatcher` → `regenerate_and_swap`) régénérait donc `pack.bin` à partir d'un instantané figé au dernier `marius-dump`, jamais de la donnée fraîchement mutée. Le critère n°5, tel qu'écrit, était insatisfiable dans l'état du code audité.

Une seconde divergence, indépendante, a été découverte au même moment : le générateur de jointure varlena (`fetch_from_pg`) supposait que la colonne de jointure portait le même nom des deux côtés du `JOIN` — invalide pour le cas 1:1 (`content.body.document_id` référençant `content.document.id`).

## 3. Méthode employée

- Lecture systématique du code réel avant toute décision — jamais d'extrapolation à partir de la documentation seule, conformément à la clause d'échappement posée en début de session.
- Chaque contradiction ou hypothèse non vérifiée a été signalée explicitement et soumise à arbitrage avant de continuer.
- Le Contrat d'Implémentation a été découpé en neuf étapes atomiques, chacune : vérifiée pour cohérence avec les documents gelés (DFS, `DESIGN-store-registry.md`), implémentée, compilée et testée avec du code réel avant de passer à la suivante.
- Un rapport standardisé (objectif atteint/non atteint, fichiers modifiés, invariants vérifiés, impact aval, dette créée) a clos chaque étape.
- La documentation de conception (DFS, `DESIGN-store-registry.md`) a été traitée comme gelée jusqu'à rattrapage effectif par le code, puis corrigée à chaque écart constaté — jamais laissée silencieusement obsolète.

## 4. Modifications réalisées

Détail complet et statut de placement : `PLACEMENT-fichiers-phase1.md`. Résumé :

| Composant | Nature |
|---|---|
| `StoreRegistry<P>` (`core/projection`) | Nouveau — registre mono-slot, atomiquement remplaçable, remplace le `OnceLock` |
| `merge_store` (`shell/render`) | Nouveau — fusion à trois canaux synchronisés, zéro-allocation pour les runs non modifiées |
| `ingest_and_swap` (`shell/render`) | Nouveau — étage 1 du pipeline CoW, transactionnel |
| `PackfileReader` — accesseurs bruts publics | Modification |
| `PackfileBuilder` — `push_raw_run()` | Ajout |
| `Projection` — `store_registry()` sur le trait | Ajout |
| `codegen/projection.rs` | Modification — génération `StoreRegistry`/`cold_start_store`/`store_registry`, correctif JOIN |
| `Dispatcher::run` | Modification — séquencement `ingest_and_swap` → `regenerate_and_swap` |
| `main.rs` | Modification — provisionnement `StoreRegistry` au bootstrap |
| `batch_renderer.rs`, `regenerate.rs` | Modification mineure — fixtures de test mises en conformité avec `store_registry()` |

## 5. Critères d'acceptation

- Mécanisme varlena exercé sur `content.body.content` — **atteint** (Étapes 1-5).
- Correctif JOIN validé sur le cas réel `content.document`/`content.body` — **atteint** (Étape 5).
- `UPDATE` → régénération → contenu frais visible en HTTP, sans redémarrage — **atteint** (Étape 9), avec la réserve explicite ci-dessous sur la nature de la validation.

## 6. Résultat

Chaîne complète recompilée et testée avec le code réel de la session (`registry.rs`, `pack_html_index.rs`, `sweep.rs`, `batch_renderer.rs`, `regenerate.rs`, `dumper.rs`, `pack_html_format.rs`, plus le code écrit cette session) : **47 tests préexistants/étendus + 1 test de bout en bout, stables sur exécutions répétées.**

Le test de bout en bout construit un `store.bin` initial, le fait lire par un premier `regenerate_and_swap` réel, confirme le contenu initial via `LiveRegistry`/`PackHtmlIndex`/`pread` (même paire d'opérations que `handlers.rs::deliver`), mute l'état SQL simulé, exécute `ingest_and_swap` puis `regenerate_and_swap` réels dans le même processus, et confirme que la lecture HTTP reflète la nouvelle valeur — sans redémarrage, sans second `cold_start`.

**Ce qui reste simulé, faute d'accès à un PostgreSQL réel dans cet environnement** : seul `fetch_from_pg` interroge un état en mémoire plutôt qu'une vraie base. Le serveur Axum lui-même et le `PgListener`/`NOTIFY` réel (`main.rs`) ont été lus et confrontés au code, jamais exécutés.

## 7. Enseignements

- **La documentation peut décrire un système qui n'a jamais existé dans le code**, même quand plusieurs documents indépendants se recoupent — le recoupement documentaire n'est pas une preuve, seule la lecture du code généré en est une.
- **Un composant testé en isolation peut cacher une extension d'API non anticipée** : `merge_store` a révélé, à l'écriture, que ni `PackfileReader` ni `PackfileBuilder` n'exposaient ce dont un algorithme zéro-allocation avait besoin — découvert en écrivant le code, pas en le concevant sur le papier.
- **Une correction de trait se propage plus loin qu'il n'y paraît** : l'ajout de `store_registry()` (Étape 4) a nécessité de revenir sur l'Étape 3 déjà close, puis sur des fixtures de test de fichiers jamais touchés jusqu'à l'Étape 9.
- **Brancher tout ensemble pour de vrai révèle des bugs qu'aucun test isolé ne peut trouver** : le premier essai du test de bout en bout a été écrit avec un `fetch_batch` inerte — l'échec immédiat a confirmé, en conditions réelles, exactement le mécanisme que toute la session avait établi sur dossier.
- **Un délai fixe dans un test asynchrone est un pari, pas une preuve** : la flakiness rencontrée à l'Étape 6/7 n'était pas un défaut du séquencement, mais du test lui-même — corrigée par attente active bornée plutôt que par un délai plus long choisi au hasard.

## 8. Dette technique restante

- **Placement de `merge_store`** (`crates/shell/render`, pas `crates/core/projection`) — TODO architectural posé à l'Étape 2, explicitement à réévaluer en fin de Contrat. Réévalué maintenant : aucune migration de `PackfileBuilder` n'a eu lieu pendant cette session, donc rien ne justifie de déplacer `merge_store` aujourd'hui. À reconsidérer seulement si `PackfileBuilder` migre un jour vers `core/projection` — pas une urgence.
- **Bootstrap** (`try_bootstrap_store_registries`, banc de validation) liste les Projections à la main — si le nombre de tables croît significativement, ce point mériterait d'être généré par `db-forge` plutôt qu'écrit à la main dans `main.rs`.
- **`push_raw_run`** repose sur un contrat logique non vérifiable par le type système (documenté comme invariant d'API, pas comme dette, à la demande explicite d'arbitrage antérieure — rappelé ici pour mémoire, pas comme point ouvert).
- **Aucune dette bloquante.**

## 9. Architecture désormais considérée comme référence

- `DFS-phase1-reactivite-cow.md` — synchronisée avec le code livré, aucun écart connu restant.
- `DESIGN-store-registry.md` — synchronisé, y compris la correction validation-avant-rename.
- `runtime-data-flow-invariants.md` — confirmé conforme par l'audit `handlers.rs`/`registry.rs`/`pack_html_index.rs`, aucune modification nécessaire.
- `CONTRAT-implementation-phase1.md` — les neuf étapes sont closes ; document à conserver comme trace du séquencement réel, pas à réexécuter.

Ces quatre documents constituent, à l'issue de cette session, la référence à jour du pipeline réactif. Toute évolution future de ce pipeline devrait partir d'eux, pas de la documentation antérieure au 20 juillet 2026, qui a été la source de la confusion initiale.
