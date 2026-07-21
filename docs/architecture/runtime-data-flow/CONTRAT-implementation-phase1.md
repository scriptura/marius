# Contrat d'Implémentation — Réactivité CoW (Phase 1)

**Fonde sur** : `DFS-phase1-reactivite-cow.md` (topologie, algorithmes) + `DESIGN-store-registry.md` (composant `StoreRegistry`), tous deux figés. Ce document ne redéfinit ni l'un ni l'autre — il séquence leur mise en œuvre.

**Discipline d'exécution** : chaque étape est atomique — compilable et testable isolément, sans dépendre d'une étape postérieure. L'ordre encode les dépendances réelles ; aucune étape ne doit être commencée avant que celles dont elle dépend soient terminées et testées.

---

### Étape 1 — `StoreRegistry<P>`
**Crate** : `crates/core/projection`, nouveau module `store_registry.rs`.
**Contenu** : API exacte de `DESIGN-store-registry.md` §3 (`new`, `cold_start`, `load`, `swap`), `std::sync::RwLock<Option<Arc<PackfileReader<P>>>>`, invariants INV-1 à INV-5.
**Dépend de** : rien (n'utilise que des types déjà existants — `PackfileReader`, `Arc`, `std::sync`).
**Critère de complétion** : tests unitaires couvrant — `load()` panique avant `cold_start` ; `cold_start` panique/`Err` si fichier absent ou invalide ; `load()` après `swap()` retourne la nouvelle version ; un `Arc` obtenu par `load()` avant un `swap()` reste valide et inchangé après (INV-3) ; deux `load()` consécutifs sans `swap()` entre les deux retournent le même pointeur `Arc` (`Arc::ptr_eq`).

### Étape 2 — `merge_store`
**Crate** : `crates/core/projection`, nouveau module `merge_store.rs`.
**Contenu** : algorithme à trois canaux synchronisés (DFS §3.3) — sweep à deux curseurs sur `old.id_index()`/`delta` trié, memcpy de run pour `id_index`+`records` (stride fixe), memcpy+shift de run pour `varlena_toc`+`heap`, alimentation d'un `PackfileBuilder<P>` fourni par l'appelant. Aucune dépendance I/O/mmap-ouverture/réseau — opère sur un `&PackfileReader<P>` déjà monté et un `&[(P::Record, P::VarlenOwned)]` déjà en mémoire.
**Dépend de** : rien de nouveau (types déjà existants : `PackfileReader`, `PackfileBuilder`, `VarlenSlot`).
**Critère de complétion** : tests unitaires sur le modèle de `sweep.rs` — delta vide (copie intégrale), delta = table entière (aucune run copiée), insertions en tête/milieu/queue, updates de longueur varlena différente (vérifier le recalcul de shift), suppressions (id du delta absent), mix des quatre cas dans un seul appel, `MergeStoreReport` correct sur chaque cas.
**Point de vigilance explicite** (déjà signalé DFS §3.3, à vérifier au moment d'écrire le test, pas supposé résolu) : confirmer que le heap de `store.bin`, tel que produit par `PackfileBuilder::push_batch`/`encode_varlena`, est bien tassé sans padding inter-champs au sein d'une même ligne — l'hypothèse posée en DFS s'appuie sur la lecture du code de `packfile_builder.rs`, pas sur une vérification par test à ce jour.

### Étape 3 — Modification de `codegen/projection.rs` (db-forge)
**Crate** : `crates/forge/db-forge` (build-time uniquement — n'affecte aucun crate runtime directement, seulement le code qu'il génère).
**Contenu** :
- Remplacer la génération `static {SCREAMING}_STORE: OnceLock<PackfileReader<P>> = OnceLock::new();` par `static {SCREAMING}_STORE: StoreRegistry<{ProjName}Projection> = StoreRegistry::new();`.
- Générer une fonction publique `pub fn cold_start_store() -> std::io::Result<()>` par `Projection`, appelant `{SCREAMING}_STORE.cold_start(&Self::store_path())`.
- Réécrire le corps généré de `fetch_batch` : un seul `let reader = {SCREAMING}_STORE.load();` en tête de fonction, la boucle sur `ids` réutilise `reader` pour chaque `lookup` — jamais de `load()` dans la boucle (INV-5).
- Supprimer la génération du `_pool` ignoré si elle devient inutile, ou le conserver commenté selon la signature de trait figée (`fetch_batch(pool, ids)`) — ne pas changer la signature publique du trait à cette étape, seulement le corps généré.
**Dépend de** : Étape 1 (le type `StoreRegistry` doit exister et être importable depuis le code généré).
**Critère de complétion** : `cargo build` du crate cible (celui qui `include!`/consomme la sortie de `db-forge`) compile ; un test d'intégration appelle `cold_start_store()` puis `fetch_batch(pool, &[id_connu])` et vérifie un résultat identique à celui obtenu avant modification (non-régression sur le cas mono-load déjà exercé par les tests existants de `regenerate.rs`/`batch_renderer.rs`).

### Étape 4 — `ingest_and_swap`
**Crate** : `crates/shell/render`, nouveau fichier ou ajout à `regenerate.rs` (à trancher selon convention déjà en place — `regenerate.rs` héberge déjà l'équivalent pour `pack.bin`, cohérence à privilégier).
**Contenu** (DFS §3.4) : `P::fetch_from_pg(pool, ids)` (async) → `spawn_blocking` : `merge_store` (Étape 2) → `PackfileBuilder::write` vers `.tmp` → `fsync` → `rename` OS → `PackfileReader::open` de revalidation (§6 du design StoreRegistry) → `{Proj}::store_registry().swap(Arc::new(reader))` — nécessite d'exposer un accesseur `pub fn store_registry() -> &'static StoreRegistry<Self>` généré à l'Étape 3, ou d'accéder directement à la `static` si visibilité suffisante.
**Dépend de** : Étape 2 (`merge_store`), Étape 3 (existence de `StoreRegistry` généré et accessible).
**Critère de complétion** : test d'intégration — `UPDATE` simulé (delta construit à la main) → `ingest_and_swap` → `fetch_batch` d'un id du delta retourne la nouvelle valeur ; échec simulé à l'étape `fsync`/`rename` → `StoreRegistry` inchangé (ancienne version toujours servie).

### Étape 5 — Correctif du générateur de JOIN (`codegen/projection.rs`, prérequis fonctionnel pour `content.body`)
**Crate** : `crates/forge/db-forge`.
**Contenu** : remplacer `ON {schema}.{table}.{_fk} = {vs}.{vt}.{_fk}` par `ON {schema}.{table}.{pk_col} = {vs}.{vt}.{_fk}` (Constat n°2, déjà arbitré), et faire échouer le build explicitement (`panic!` en position codegen) si `varlena_join.is_some() && matches!(pk, PrimaryKey::Composite)`.
**Dépend de** : rien techniquement (indépendant des étapes 1-4), mais **prérequis obligatoire** pour que l'Étape 7 (validation bout-en-bout sur `content.body`) fonctionne, puisque `content.body` est précisément le cas 1:1 concerné par ce bug.
**Critère de complétion** : `fetch_from_pg` généré pour `content.document`/`content.body` produit un SQL valide, exécutable, retournant les lignes attendues (test contre une base de test réelle ou un mock SQL).

### Étape 6 — `Dispatcher::run` — orchestration séquentielle
**Crate** : crate hébergeant `Dispatcher` (à confirmer — probablement `crates/shell/render`, non vérifié explicitement cette session).
**Contenu** (DFS §6) : remplacer l'unique appel à `regenerate_and_swap` par la séquence `ingest_and_swap` puis `regenerate_and_swap`, sur le même `ids` trié, avec abandon du tick (pas d'appel au second étage) si le premier échoue.
**Dépend de** : Étape 4 (`ingest_and_swap` doit exister et être testé indépendamment).
**Critère de complétion** : test reprenant `test_hot_path_pipeline_stress` (déjà existant, `dispatcher.rs`) étendu pour vérifier qu'un `UPDATE` réel se reflète dans `pack.bin` après un seul cycle `Dispatcher::run`, sans dump manuel intermédiaire.

### Étape 7 — Bootstrap (`main.rs`)
**Crate** : `crates/marius` (facade) ou point d'entrée du serveur, non identifié précisément cette session.
**Contenu** : pour chaque `Projection` générée, appel de `cold_start_store()` (Étape 3) avant tout `tokio::spawn` de `Dispatcher` et avant le démarrage du serveur Axum — fail-fast si un `store.bin` est absent.
**Dépend de** : Étape 3.
**Critère de complétion** : démarrage du serveur échoue proprement (message clair, pas de panic opaque) si un `store.bin` attendu n'existe pas ; démarrage réussit et sert des données à jour si présent.

### Étape 8 — Audit séparé, non bloquant pour les étapes 1-7
**Crate** : `crates/shell/server` (`handlers.rs` ou équivalent, non fourni à ce jour).
**Contenu** : vérifier qu'aucun handler HTTP n'appelle `fetch_batch` directement (cf. `DESIGN-store-registry.md` §7/§9) — le chemin chaud doit rester exclusivement `pread` sur `pack.bin`. Si un tel appel existe, il constitue une divergence à corriger séparément, pas une contrainte à absorber rétroactivement dans les étapes précédentes.
**Dépend de** : rien — peut être mené en parallèle de toutes les autres étapes.
**Critère de complétion** : confirmation ou infirmation, documentée, de l'absence d'appel direct.

### Étape 9 — Validation bout-en-bout (restaure le critère d'acceptation n°5 de `SPEC-phase0-varlena-et-js-deps.md`)
**Contenu** : exécuter littéralement le scénario que ce critère décrit — `UPDATE content.body SET content = ...` → `NOTIFY` → `Dispatcher` → observation du nouveau contenu en HTTP, sans redémarrage du serveur.
**Dépend de** : Étapes 1 à 7 terminées et testées individuellement ; Étape 5 obligatoire (sans elle, `content.body` spécifiquement ne peut pas être exercé, indépendamment du reste de la topologie CoW).
**Critère de complétion** : le critère n°5, jugé insatisfiable en l'état du code audité en début de session, est démontré satisfait — pas par relecture de code, par exécution réelle.

---

## Dépendances entre étapes — résumé

```
1 (StoreRegistry) ──┬──▶ 3 (codegen fetch_batch) ──▶ 4 (ingest_and_swap) ──▶ 6 (Dispatcher) ──▶ 9
2 (merge_store) ─────┘                                    │                      │
5 (correctif JOIN) ────────────────────────────────────────────────────────────┘──▶ 9
                                                            7 (bootstrap) ──────────▶ 9
8 (audit HTTP) — indépendant, parallélisable, non bloquant pour 9
```
