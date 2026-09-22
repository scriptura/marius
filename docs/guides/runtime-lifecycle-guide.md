# Guide runtime — du `render()` compilé à la requête HTTP servie

> Complémentaire de `fragment-forge-guide.md` (compilation `.marius` → `render()`).
> Ce document couvre la couche suivante : comment `render()` est effectivement
> invoqué, sur quel déclencheur, et ce qui invalide un artefact déjà écrit.
> Périmètre disjoint par construction — voir le renvoi en tête du guide
> `fragment-forge`.
>
> **Créé le 7 juillet 2026**, à la suite d'une session de débogage complète du
> pipeline `.marius` → HTTP.
>
> **Corrigé le 21 septembre 2026 — pipeline réactif à deux étages.** Une version
> antérieure de ce guide (mise à jour des Phases 4.2/4.3) affirmait que le
> `store.bin` n'était pas une source de lecture de la régénération HTML et que
> `P::fetch_batch` interrogeait PostgreSQL. **Ce n'est plus exact** depuis
> l'introduction de `ingest_and_swap` (Étage 1) : le `fetch_batch` généré lit le
> `store.bin` (mmap, via `StoreRegistry`) et n'utilise jamais son paramètre
> `pool` ; la seule lecture SQL d'un tick est `P::fetch_from_pg`, appelée par
> l'Étage 1 (et par `marius-dump`). Les sections concernées (schéma global, §0,
> §1, §3, §4, §4bis, §5bis, §6, §7, §8, §9, §10, §11) ont été corrigées en
> conséquence. Le format du pack HTML et la fusion incrémentale (`merge_sweep`)
> sont inchangés.


## Schéma global — deux pipelines de nature différente et leur jonction runtime

Il faut distinguer deux temporalités.

Le premier graphe est un graphe **AOT/build-time** : il transforme les
templates `.marius` en code Rust compilé contenant les fonctions `render()`.

Le second est le graphe **réactif/runtime**, organisé en **deux étages** : une
mutation SQL produit un événement `NOTIFY` ; l'Étage 1 (`ingest_and_swap`)
récupère le delta depuis PostgreSQL et met à jour le `store.bin` (Copy-on-Write) ;
l'Étage 2 (`regenerate_and_swap`) lit ce `store.bin`, rend les enregistrements du
delta, les fusionne avec le pack HTML courant, puis swap atomiquement le registre
de lecture.

```
              BUILD TIME (cargo build)                     RUNTIME (processus vivant)
              =======================                      ==========================

        templates/*.marius                        UPDATE / INSERT / DELETE (SQL)
                │                                             │
                ▼                                             ▼
          fragment-forge                              trigger PostgreSQL
                │                                             │
                ▼                                             ▼
        render() généré (source)                          pg_notify
                │                                             │
                ▼                                             ▼
           cargo build                                    PgListener
                │                                             │
                ▼                                             ▼
         ┌─────────────┐                             Collector::insert(id)
         │   binaire   │                                      │
         └─────────────┘                                      ▼
                │                                    Collector::flush()
                │                                             │
                │                                             ▼
                │                                     Dispatcher (un tick)
                │                                             │
                │                                             ▼
                │                                  ÉTAGE 1 — ingest_and_swap
                │                                    P::fetch_from_pg(pool, ids)      ← seule lecture SQL du tick
                │                                    merge_store(ancien store, delta, supprimés)
                │                                    store.bin.tmp → fsync → validation → rename
                │                                    StoreRegistry::swap()
                │                                             │
                │                                             ▼
                │                                  ÉTAGE 2 — regenerate_and_swap
                │                                    P::fetch_batch(_pool, ids)       ← lecture mmap du store.bin frais
                │                                             │
                │                                             ▼
                │                                    BatchRenderer::render_batch
                │                                             │
                └────────────── render() compilé ◄┤
                                                              ▼
                                                     DeltaBatch en mémoire
                                                              │
                                                              ▼
                                                     merge_sweep(ancien pack, delta)
                                                              │
                                                              ▼
                                                     nouveau pack HTML ({key}.bin)
                                                              │
                                                              ▼
                                                        rename atomique
                                                              │
                                                              ▼
                                                     LiveRegistry::store()
                                                              │
                                                              ▼
                                                        HTTP → pread()
```

**Point essentiel :** le `render()` généré par `fragment-forge` n'est pas un
producteur de packfile au moment du build. Il est une partie du code du
binaire runtime, puis est invoqué lorsque le chemin réactif traite un delta
(Étage 2).

Inversement, le graphe runtime ne « recompile » jamais `render()`.

Ainsi :

* modifier un `.marius` puis effectuer `cargo build` produit un nouveau
  `render()` dans le binaire ;
* cette recompilation ne régénère aucun pack HTML existant ;
* une fois le nouveau binaire lancé, une régénération HTML doit encore être
  déclenchée par le mécanisme runtime approprié ;
* cette régénération lit les enregistrements du delta dans le `store.bin` (mis à
  jour depuis PostgreSQL à l'Étage 1), exécute le `render()` compilé, puis
  fusionne le résultat avec le pack actuellement servi.

La jonction entre les deux graphes n'est donc pas un fichier intermédiaire :
**c'est le code `render()` compilé qui est embarqué dans le processus runtime.**

## 0. La question à se poser avant toute autre

Face à un HTML qui ne change pas malgré un `cargo build` réussi, la question
n'est jamais « le template est-il correct ? » en premier.

Il faut d'abord déterminer :

> **quel artefact devrait avoir changé, quel composant le produit, et quel
> événement déclenche effectivement ce producteur ?**

Plusieurs artefacts coexistent et n'ont pas les mêmes producteurs ni les mêmes
cycles d'invalidation.

Une confusion particulièrement importante doit être évitée :

> **`{table}_store.bin` et `{table}.bin` sont deux artefacts distincts, mais ils
> forment bien une chaîne dans le chemin réactif normal :**
> `PostgreSQL → (Étage 1) store.bin → (Étage 2) pack HTML`.

Le premier est l'état de données brutes (format DOD, mmap, mis à jour en
Copy-on-Write). Le second est le pack HTML effectivement servi. Le pack est rendu
**à partir du store** ; seul l'Étage 1 interroge PostgreSQL. Diagnostiquer un HTML
périmé exige donc de vérifier les deux étages, dans l'ordre (§9).

Il existe par ailleurs une catégorie distincte de pages statiques (§1bis)
qui ne participe pas du tout au cycle `NOTIFY`/Dispatcher.

## 1. Les artefacts et leurs responsabilités — ne jamais les confondre

| Artefact                           | Producteur                                                        | Contenu                                        | Déclencheur / invalidation                                                          |
| ---------------------------------- | ----------------------------------------------------------------- | ---------------------------------------------- | ----------------------------------------------------------------------------------- |
| `render()` (dans le binaire)       | `cargo build` → `fragment-forge`                                  | Code Rust généré depuis `.marius`              | Modification du `.marius` ou des entrées surveillées par le build de `core/schema`  |
| `{table}_store.bin`                | `marius-dump` (`dumper::dump_table`) ; `ingest_and_swap` (Étage 1) | Lignes brutes DOD `#[repr(C)]`, mmap           | Dump manuel ; delta runtime traité par le `Dispatcher` (Étage 1)                    |
| `{key}.bin`                        | `regenerate_and_swap` (Étage 2)                                   | Pack HTML fusionné, avec blob + index + footer | Delta runtime, **après** l'Étage 1 ; provisioning initial séparé                    |
| `{table}.html` pour `STATIC_PAGES` | `resolve_static_page` / `emit_static_html`                        | HTML statique déjà composé                     | `cargo build` de `core/schema`                                                      |

Nommage exact des fichiers : voir §8 (`{key}` est le `packfile_key` de
l'artefact — aujourd'hui `content_core` —, différent du nom du store).


### 1.1 `render()`

Le `render()` généré est du **code compilé**.

Il n'est pas un artefact HTML et n'est pas écrit dans `artifacts/`. Sa
production relève exclusivement du build AOT.

Une modification du template peut donc produire :

```text
.marius
   ↓
fragment-forge
   ↓
nouveau render()
   ↓
cargo build
   ↓
nouveau binaire
```

mais pas :

```text
.marius
   ↓
nouveau {table}.bin
```

### 1.2 `{table}_store.bin`

Le `store.bin` contient les lignes brutes du composant, au format
`PackfileStoreHeader` + lignes `#[repr(C)]` + index d'ids + TOC/heap varlena. Il
est mappé en mémoire (`StoreRegistry<P>`, un `Arc<PackfileReader<P>>` remplaçable
atomiquement).

Il est produit par deux chemins :

* `dumper::dump_table` (`marius-dump`) — extraction complète initiale ;
* `ingest_and_swap` (Étage 1 de chaque tick) — fusion incrémentale du delta :
  `P::fetch_from_pg(pool, ids)` → `merge_store` → `.tmp` + `fsync` →
  validation → `rename` → `StoreRegistry::swap()`.

**Il est lu par `regenerate_and_swap`** (Étage 2), via `P::fetch_batch`. Le code
généré (`db-forge`, `codegen/projection.rs`) le montre sans ambiguïté :

```rust
async fn fetch_batch(_pool: &sqlx::PgPool, ids: &[i64]) -> Result<…> {
    let reader = {NAME}_STORE.load();      // un seul load() par batch (INV-5)
    // lookup O(log N) par id ; un id absent du store est ignoré (supprimé)
}
```

Le paramètre `_pool` est conservé par la signature du trait `Projection` mais
**n'est jamais utilisé** : aucun fallback réseau, un store non provisionné fait
paniquer (fail-fast). `P::fetch_from_pg` (SQLx) est la voie d'extraction : appelée
par `dumper::dump_table` et par `ingest_and_swap`, jamais par la régénération.

La source des données du rendu est donc le `store.bin`, lui-même alimenté depuis
PostgreSQL à l'Étage 1.

### 1.3 `{key}.bin` — le pack HTML

Le pack HTML est l'artefact effectivement consommé par le chemin HTTP.

`regenerate_and_swap` (Étage 2) :

1. reçoit les identifiants du delta (déjà appliqués au store par l'Étage 1) ;
2. lit les enregistrements correspondants dans le store avec `P::fetch_batch` ;
3. rend les lignes récupérées ;
4. construit un `DeltaBatch` en mémoire ;
5. fusionne ce delta avec l'ancien pack via `merge_sweep` ;
6. écrit un nouveau fichier `.tmp` ;
7. flush/fsync le contenu et la taille ;
8. effectue le `rename` atomique ;
9. ouvre le nouveau pack ;
10. publie son index dans `LiveRegistry`.

Le fichier précédent n'est donc pas reconstruit depuis zéro à chaque tick.

Les entités absentes du delta sont conservées par `merge_sweep`.

C'est précisément la propriété introduite par la stratégie de **Sweep Merge**.

## 1bis. La quatrième catégorie — pages `STATIC_PAGES`

Certaines pages `.marius` ne dépendent d'aucune donnée SQL dynamique et sont
déclarées dans `STATIC_PAGES` (`crates/core/schema/build/main.rs`).

Aujourd'hui, cette catégorie comprend notamment `offline`.

Ces pages ne participent à aucun des cycles précédents :

| Artefact       | Producteur                                 | Contenu               | Invalidation                   |
| -------------- | ------------------------------------------ | --------------------- | ------------------------------ |
| `{table}.html` | `resolve_static_page` / `emit_static_html` | HTML composé au build | `cargo build` de `core/schema` |

Aucun `NOTIFY`, `PgListener`, `Collector`, `Dispatcher` ou
`regenerate_and_swap` n'est nécessaire.

Cette séparation est structurelle : une page sans donnée SQL dynamique n'a
pas de raison de traverser le pipeline réactif.

Voir `fragment-forge-guide.md` §4.8 et §4.8ter pour la distinction entre
`{{ record.* }}` (interpolation, échoue toujours avec `UnknownField` sur une
page statique) et `{% if record.* %}` (conditions, désormais éliminées
statiquement à faux plutôt que de faire échouer la compilation — voir
`eliminate_recordless_conditions`).

## 2. Piège Cargo — `rerun-if-changed` conditionnel

Une directive :

```rust
if path.exists() {
    println!("cargo:rerun-if-changed={path}");
}
```

ne protège pas contre l'apparition ultérieure du fichier.

Cargo ne surveille que les chemins qui ont effectivement été déclarés lors du
build script précédent.

La règle à appliquer dans les `build.rs` du projet est donc :

> **Émettre `cargo:rerun-if-changed` inconditionnellement, avant tout test
> d'existence.**

Le répertoire parent peut également être surveillé comme filet de sécurité
lorsqu'un fichier peut apparaître ultérieurement.

Symptôme classique :

```text
Finished
```

sans nouvelle compilation du crate concerné alors qu'un template vient d'être
ajouté ou modifié.

Dans ce cas, utiliser :

```bash
cargo build -vv
```

et vérifier que le build script a effectivement réévalué le template concerné.

## 3. Le déclencheur du chemin réactif : `NOTIFY` PostgreSQL

Pour les projections dynamiques, le chemin normal est :

```text
UPDATE / INSERT / DELETE
        │
        ▼
trigger PostgreSQL
        │
        ▼
pg_notify(canal, id)
        │
        ▼
PgListener
        │
        ▼
Collector::insert(id)
        │
        ├── seuil atteint ──▶ flush
        │
        └── sinon ───────────▶ tick périodique
                                      │
                                      ▼
                                  Dispatcher
                                      │
                                      ▼
                      ingest_and_swap  (Étage 1)
                      fetch_from_pg → merge_store → store.bin
                                      │
                                      ▼
                      regenerate_and_swap  (Étage 2)
                      fetch_batch (mmap du store) → render_batch
                                      │
                                      ▼
                             DeltaBatch
                                      │
                                      ▼
                          merge_sweep(old, delta)
                                      │
                                      ▼
                               {key}.bin.tmp
                                      │
                                      ▼
                             fsync + rename
                                      │
                                      ▼
                          LiveRegistry::store()
```

### 3.1 Le delta transite par le `store.bin`

C'est le point qu'une version antérieure de ce guide décrivait de façon
inversée (voir l'en-tête).

Le `Collector` fournit au `Dispatcher` les `ids` du tick courant (triés
`ID ASC` par `ids.sort_unstable()` avant le rendu). Le `Dispatcher` exécute
ensuite **deux étages dans un ordre strict** :

```rust
ingest_and_swap::<P>(pool, &ids, io_semaphore)          // Étage 1
regenerate_and_swap::<P>(pool, &ids, total_cap,
                         packfile_key, registry, io_semaphore)   // Étage 2
```

Le flux réel est donc :

```text
ids
 ↓
PostgreSQL          (P::fetch_from_pg — Étage 1, seule lecture SQL)
 ↓
store.bin           (merge_store + rename + StoreRegistry::swap)
 ↓
Record              (P::fetch_batch — Étage 2, lecture mmap)
 ↓
render()
 ↓
DeltaBatch
 ↓
merge_sweep()
```

**Invariant de tolérance aux pannes :** tout échec de l'Étage 1 interrompt le
tick ; l'Étage 2 n'est jamais exécuté depuis un store non rafraîchi (il
persisterait un delta incohérent).


### 3.2 Un changement de code seul n'invalide pas le pack

Un changement dans :

* `.marius`,
* `render()`,
* une logique de rendu compilée,

ne génère aucun `NOTIFY`.

Donc :

```bash
cargo build
```

ne suffit pas à provoquer une régénération du pack actuellement servi.

Après déploiement du nouveau binaire, il faut provoquer le chemin runtime
approprié pour les projections dynamiques.

En développement, une écriture SQL triviale peut être utilisée :

```sql
UPDATE {schema}.{table}
SET {pk} = {pk}
WHERE {pk} = {valeur};
```

ou, pour toutes les lignes :

```sql
UPDATE {schema}.{table}
SET {pk} = {pk};
```

si le trigger `AFTER UPDATE` correspondant émet bien le `NOTIFY`.

### 3.3 Le serveur doit être à l'écoute avant l'événement

Un `NOTIFY` PostgreSQL n'est pas une file persistante de changements.

Si le processus n'est pas encore en `LISTEN` lorsque l'événement est émis,
l'événement ne sera pas rejoué au démarrage ultérieur.

La séquence correcte pour un test manuel est donc :

```text
démarrer le serveur
       ↓
[pg_listener] abonné
       ↓
effectuer UPDATE/INSERT/DELETE
       ↓
observer Collector / Dispatcher
       ↓
observer le nouveau pack
```

### 3.4 Le trigger PostgreSQL reste une dépendance de déploiement

La présence du SQL du trigger dans le dépôt ne signifie pas que le trigger
existe dans la base courante.

`cargo build` ne l'installe pas.

Vérification directe :

```sql
SELECT trigger_name
FROM information_schema.triggers
WHERE trigger_name = 'trg_{ma_table}_notify';
```

Zéro résultat signifie que le trigger attendu n'est pas installé sur cette
base.

## 4. La régénération incrémentale — `old pack + delta`, pas `table complète`

C'est désormais une propriété fondamentale du runtime et elle mérite d'être
explicitement documentée.

`ids` représente **le delta du tick courant**, pas l'ensemble de la table.

`fetch_delta_batch` lit uniquement ces identifiants dans le store (via `P::fetch_batch`), par chunks de
`CHUNK_SIZE` :

```text
ids du tick
    │
    ├── chunk 0 ──▶ P::fetch_batch()
    ├── chunk 1 ──▶ P::fetch_batch()
    ├── ...
    └── chunk N ──▶ P::fetch_batch()
```

Les résultats sont rendus dans un payload delta unique.

Puis :

```text
ancien pack
     +
delta rendu
     │
     ▼
 merge_sweep
     │
     ▼
nouveau pack
```

Les entités qui ne figurent pas dans le delta **ne repassent donc pas dans
`render()`**.

Elles sont conservées depuis le pack précédent.

C'est précisément ce que garantit le test :

```text
untouched_entities_survive_successive_incremental_merges_then_delete
```

Une entité peut ainsi survivre à plusieurs cycles sans être jamais refetchée
ni rerendue.

## 4bis. Suppression — absence dans PostgreSQL, puis absence dans le store

La suppression est détectée **à deux niveaux successifs**, un par étage.

**Étage 1 (`ingest_and_swap`).** Pour chaque ID du tick, si `P::fetch_from_pg`
ne retourne aucune ligne, cet ID est supprimé : il est passé à `merge_store` dans
`deleted_ids`, qui retire la ligne du nouveau `store.bin`.

**Étage 2 (`fetch_delta_batch`).** Pour chaque ID demandé, si `P::fetch_batch`
(lecture du store fraîchement mis à jour) ne retourne aucune ligne, cet ID est
considéré comme supprimé :

```rust
DeltaEntry {
    entity_id: id,
    offset: 0,
    length: 0,
}
```

Cette entrée constitue la sentinelle consommée par `merge_sweep`.

Le chemin est donc :

```text
Collector
   │
   ▼
id = 42
   │
   ▼
Étage 1 : fetch_from_pg → aucune ligne
   │        → deleted_ids → merge_store → ligne retirée du store.bin
   ▼
Étage 2 : fetch_batch (store) → aucune ligne
   │
   ▼
DeltaEntry(id=42, offset=0, length=0)
   │
   ▼
merge_sweep
   │
   ▼
suppression du fragment existant
```

La suppression est donc propagée **par le store** : l'Étage 1 la traduit en
retrait de ligne, l'Étage 2 la déduit de l'absence de l'ID dans ce store.


## 5. Écriture physique du pack — CoW, durabilité et swap atomique

`apply_merge_io_sync` constitue le noyau physique de la régénération.

Il est volontairement **strictement synchrone** et ne dépend pas de Tokio.

L'appelant `regenerate_and_swap` l'isole dans :

```rust
tokio::task::spawn_blocking(...)
```

Le cycle physique est :

```text
ancien pack
    │
    │ mmap lecture seule
    ▼
old_blob + old_index
    │
    │
delta en mémoire
    │
    ▼
merge_sweep()
    │
    ▼
.tmp
    │
    ├── écriture blob
    ├── padding aligné
    ├── index
    ├── footer
    │
    ▼
flush_range()
    │
    ▼
ftruncate(taille réelle)
    │
    ▼
fsync()
    │
    ▼
rename(.tmp → .bin)
    │
    ▼
PackHtmlIndex::open()
```

Le fichier final n'est jamais ouvert en écriture pendant la fusion.

Cette propriété est essentielle :

> **Tant que le `rename` n'a pas eu lieu, l'ancien pack reste intact et
> continue de pouvoir être servi.**

Puis seulement après le succès du `rename`, `regenerate_and_swap` effectue :

```rust
registry.store(packfile_key, Arc::new(new_index));
```

Le `LiveRegistry` ne publie donc jamais un index correspondant à une écriture
qui n'a pas été finalisée.

## 5bis. Le sémaphore I/O

Le fetch réseau PostgreSQL (Étage 1) est volontairement hors du sémaphore :

```text
Étage 1                                   Étage 2
fetch_from_pg (PostgreSQL)                fetch_batch (mmap du store, sans réseau)
       │                                         │
       ▼                                         ▼
attente io_semaphore                      attente io_semaphore
       │                                         │
       ▼                                         ▼
spawn_blocking                            spawn_blocking
       │                                         │
       ▼                                         ▼
merge_store + I/O disque                  merge_sweep + I/O disque
```

Le sémaphore régule la pression d'I/O disque et les risques de
dirty-page storm ; il ne limite pas artificiellement les requêtes PostgreSQL.

**La même instance de sémaphore** est transmise aux deux étages par
`Dispatcher::run` : la pression disque totale d'un tick (deux écritures,
`store.bin` puis pack) reste bornée par un seul budget, non doublée.

Chaque étage acquiert son permis juste avant son `spawn_blocking` et le
conserve pendant tout son noyau physique.


## 6. `marius-dump` — la chaîne store → pack, exécutée manuellement

`marius-dump` est exécuté manuellement au déploiement (`cargo run --bin
marius-dump`), jamais par `cargo build`, jamais par le `Dispatcher`. Il applique la
**même chaîne** que le chemin réactif, en une passe sur tous les ids :

```text
PostgreSQL
    │  P::fetch_from_pg
    ▼
dumper::dump_table()  ──▶  {schema}_{table}_store.bin
    │
    ▼
P::cold_start_store()        (monte le StoreRegistry local au process)
    │
    ▼
ensure_provisioned(clé) + LiveRegistry::cold_start(topologie locale)
    │
    ▼
regenerate_and_swap()        (lit le store via fetch_batch, écrit {key}.bin)
```

Le `cold_start_store()` est **obligatoire** entre les deux : sans lui,
`regenerate_and_swap` panique (`StoreRegistry` non provisionné) — `dump_table`
aurait réussi, écrit le fichier, puis la régénération tenterait de le relire par
un registre jamais monté dans ce process.

Le store brut est aussi consommé par `marius-verify`, indépendamment du pack HTML.

Lorsqu'un dump initial doit rendre immédiatement cohérents les artefacts d'un
environnement, le store et le pack sont donc produits **dans cet ordre**, par le
même binaire ; ils restent deux fichiers distincts (§1).

La topologie locale de `marius-dump` est dérivée de la déclaration de publication
(§11.8), sans réutiliser la `ROUTE_TABLE` de `marius-server` (couplage inverse
`render → server` proscrit).


## 7. Provisioning initial — pack HTML et store

Un fichier absent n'est pas nécessairement une corruption. Deux fonctions,
symétriques, garantissent l'existence d'un artefact **vide mais valide** :

* `ensure_provisioned(packfile_key)` (`regenerate.rs`) pour le pack HTML ;
* `ensure_store_provisioned::<P>()` (`store_provisioning.rs`) pour le store.

Elles distinguent :

```text
fichier absent
     │
     ▼
provisionnement  (.tmp → fsync → rename)
     │
     ▼
fichier vide mais valide
```

et :

```text
fichier déjà présent
     │
     ▼
aucune écriture
```

Le provisioning est idempotent (`ProvisionOutcome::{AlreadyPresent,
Provisioned}`).

Il ne vérifie volontairement pas la validité d'un fichier déjà présent : cette
responsabilité appartient au lecteur (`PackHtmlIndex::open`,
`StoreRegistry::cold_start`) lors du démarrage à froid.

Le séquencement du bootstrap (`main.rs`) est donc :

```text
ensure_provisioned(clé)              ensure_store_provisioned::<P>()
        │                                        │
        ▼                                        ▼
LiveRegistry::cold_start(topologie)      P::cold_start_store()
```

Le provisioning ne dépend ni de `PgPool` ni de `LiveRegistry` : il ne dépend que
d'une clé (pack) ou d'un chemin (store), jamais d'une route.

### Topologie du `LiveRegistry`

La topologie du `LiveRegistry` est **figée à sa construction** :

* `cold_start(&'static [RouteEntry])` ouvre le pack de chaque `packfile_key` (une
  seule fois par clé) et échoue si un pack est absent ;
* `with_indices(HashMap<…>)` construit un registre depuis une table de clés, sans
  passer par des `RouteEntry` ;
* `load(clé)` renvoie `None` pour une clé inconnue ; `store(clé, …)` **panique**
  (invariant AOT) — de même que `regenerate_and_swap` sur une clé hors topologie.

Un artefact dont la clé n'est pas dans la topologie n'est donc jamais ouvert.


## 8. Résolution de chemin des artefacts — `MARIUS_ARTIFACTS_DIR`, sinon CWD

Tous les chemins d'artefacts du runtime partagent la même racine :

* `packfile_path_for(key)` (`registry.rs`) : `{racine}/{packfile_key}.bin` ;
* `P::store_path()` (généré) : `{racine}/{schema}_{table}_store.bin`.

`racine` est lue dans la variable d'environnement **`MARIUS_ARTIFACTS_DIR`** ;
**si elle est absente, la racine vaut `artifacts`, relatif au répertoire courant
du processus au lancement.** `packfile_path_for` lit la variable une seule fois
(`OnceLock`) : un test in-process ne peut pas la faire varier.

Sans variable, lancer un binaire depuis un autre répertoire peut donc produire un
autre `artifacts/`.

Par exemple :

```text
workspace/
└── artifacts/
    └── content_core.bin
```

n'est pas nécessairement le fichier utilisé si le processus a été lancé
depuis :

```text
workspace/crates/shell/server/
```

Dans ce cas, un autre :

```text
crates/shell/server/artifacts/content_core.bin
```

peut être créé.

Règle opérationnelle :

> définir `MARIUS_ARTIFACTS_DIR` (chemin absolu) pour tous les binaires du
> projet, ou à défaut les lancer depuis la racine du workspace.

En cas de doute :

```bash
find / -name "{key}*.bin" -exec ls -la {} \;
```

permet de retrouver les exemplaires parasites et de comparer leurs mtime et
leurs tailles.

**Une convention de nommage n'est pas utilisée en production :** le trait
`Projection` expose aussi `packfile_path()`, généré sous la forme
`{racine}/{schema}_{table}_pack.bin`. Aucun code de production ne l'appelle : le
pack réellement servi est `{racine}/{packfile_key}.bin`. Ne pas s'appuyer sur
`packfile_path()`.

La page statique `build/{theme}/{table}.html` relève en revanche de
`CARGO_MANIFEST_DIR` dans `core/schema/build/main.rs` et n'obéit pas à cette même
résolution.


## 9. Checklist de diagnostic — « le HTML ne reflète pas mon changement »

À parcourir dans cet ordre.

### 0. La page est-elle dans `STATIC_PAGES` ?

Si oui :

* pas de PostgreSQL ;
* pas de `NOTIFY` ;
* pas de `PgListener` ;
* pas de `Collector` ;
* pas de `Dispatcher`.

Vérifier le build de `core/schema` et le fichier HTML produit.

### 1. Le `render()` du nouveau template est-il réellement dans le binaire ?

```bash
cargo build -vv
```

Vérifier que le crate concerné est effectivement recompilé.

Si le build script ne repasse pas alors que le template a changé, examiner
`rerun-if-changed`.

### 2. Le processus runtime utilise-t-il le nouveau binaire ?

C'est une étape qui devient importante avec la séparation AOT/runtime :

```text
nouveau .marius
      ↓
cargo build
      ↓
nouveau render()
      ↓
nouveau binaire
      ↓
processus effectivement lancé ?
```

Un build réussi n'implique pas que le processus actuellement en service
exécute ce binaire.

### 3. Un événement runtime a-t-il effectivement déclenché la régénération ?

Pour une projection dynamique, vérifier :

```text
trigger
 ↓
NOTIFY
 ↓
PgListener
 ↓
Collector
 ↓
Dispatcher
 ↓
ingest_and_swap        (Étage 1)
 ↓
regenerate_and_swap    (Étage 2)
```

Commencer par vérifier le trigger directement en base. Un log
`[dispatcher] ingest_and_swap (…)` en erreur signifie que le tick a été
interrompu **avant** l'Étage 2.


### 4. Le store contient-il les données attendues ?

`P::fetch_batch` lit le `store.bin`, pas PostgreSQL. Si le HTML n'a pas été rendu
avec une valeur SQL récente, vérifier d'abord que **l'Étage 1 a réussi** :
mtime et taille de `{schema}_{table}_store.bin` avant et après une mutation SQL de
test, et l'absence d'erreur `ingest_and_swap` dans les logs du `Dispatcher`.

Un HTML périmé avec un store à jour désigne l'Étage 2 (rendu, fusion, écriture) ;
un store périmé désigne l'Étage 1 (`fetch_from_pg`, `merge_store`) ou le
déclencheur.


### 5. Le pack HTML a-t-il été effectivement remplacé ?

Vérifier :

```bash
stat -c '%y %s' artifacts/{table}.bin
```

avant et après une mutation SQL de test.

### 6. Le fichier observé est-il celui réellement servi ?

```bash
find / -name "{table}*.bin" -exec ls -la {} \;
```

permet de détecter les exemplaires parasites dus au CWD.

### 7. Seulement maintenant : inspecter `regenerate_and_swap`

Si les étapes précédentes sont saines, examiner :

* `P::fetch_batch` ;
* `BatchRenderer::render_batch` ;
* `merge_sweep` ;
* l'écriture `.tmp` ;
* `flush_range` / `sync_all` ;
* `rename` ;
* `PackHtmlIndex::open` ;
* `LiveRegistry::store`.

Le point important est que **tout échec de lecture intervient avant toute
écriture disque du pack** : les tests
`fetch_failure_leaves_old_packfile_and_registry_untouched` (Étage 2) et
`fetch_failure_leaves_disk_and_registry_untouched` (Étage 1, `ingest_and_swap.rs`)
formalisent cette propriété pour chaque étage.


## 10. Modèle mental définitif

Pour une projection dynamique, le chemin de donnée à retenir est celui-ci :

```text
                         BUILD TIME
                            │
                    .marius template
                            │
                            ▼
                     fragment-forge
                            │
                            ▼
                       render()
                            │
                            ▼
                      binaire Rust
                            │
                 ───────────┼───────────
                            │
                         RUNTIME
                            │
                    mutation PostgreSQL
                            │
                            ▼
                         NOTIFY
                            │
                            ▼
                       Collector
                            │
                            ▼
                       Dispatcher
                            │
                            ▼
                    IDs du delta (triés)
                            │
                            ▼
              ÉTAGE 1 — ingest_and_swap
        fetch_from_pg → merge_store → store.bin (CoW)
                            │
                            ▼
              ÉTAGE 2 — regenerate_and_swap
          fetch_batch (mmap du store.bin) → records
                            │
                            ▼
                    render() compilé
                            │
                            ▼
                       DeltaBatch
                            │
                            ▼
                  ancien pack + delta
                            │
                            ▼
                       merge_sweep
                            │
                            ▼
                     nouveau pack
                            │
                       rename atomique
                            │
                            ▼
                      LiveRegistry
                            │
                            ▼
                          HTTP
                            │
                            ▼
                         pread()
```

Et surtout, **le `store.bin` fait partie de ce graphe** : il est l'étage
intermédiaire entre PostgreSQL et le pack. Ne pas l'omettre lors d'un diagnostic
(§9), mais ne pas non plus le confondre avec le pack : ce sont deux artefacts,
deux formats, deux producteurs (Étage 1 / Étage 2).

Le même enchaînement est joué une fois, sur tous les ids, par `marius-dump` :

```text
PostgreSQL
    │
    ▼
marius-dump / dumper::dump_table
    │
    ▼
store.bin
    │
    ▼
regenerate_and_swap
    │
    ▼
{key}.bin
```


## 11. Chemin T2A expérimental (I1→I6) — segment ordonnancé, statut PROVISOIRE

> **Ce qui suit situe un prototype expérimental
> (`crates/shell/server/src/experimental_t2a.rs`) par rapport au reste de ce
> guide — ce n'est pas une spécification.** Pour la décision architecturale
> normative, voir `ADR-011-projections-ordonnancees.md` et
> `SPECIFICATION-transport-segmente-t2a.md` (v2). Pour l'état
> d'implémentation détaillé et les écarts restants, voir
> `handoff-t2a-experimental-integration-i1-i6.md`.

### 11.1 Une frontière distincte, pas un remplacement

Le chemin décrit par les sections 1 à 10 de ce guide (Forge → `render()` →
`regenerate_and_swap` → `LiveRegistry` → HTTP → `pread()`) reste
intégralement la doctrine **AOT monolithique** — une page = une source mmap
contiguë, servie par `read_at`/`spawn_blocking` (`handlers.rs::deliver`).
Ce chemin n'a pas été modifié par le prototype T2A. Il reste légitime
lorsqu'**une représentation unique suffit** et qu'aucun cycle indépendant ne
doit être isolé dans la réponse ; il n'est pas pour autant la cible de toute
route (une réponse qui doit réunir des projections à cycles indépendants relève
de la représentation segmentée).

Le prototype ajoute, à titre expérimental, une **seconde famille
d'émission**, pour des réponses composées de plusieurs segments
(éventuellement multi-sources) : `SourceKey`/`SourceId`/`SourceSpec`/
`SegmentDescriptor`/`RouteDescriptor` (`crates/core/projection/src/lib.rs`),
et `MaterializedSource`/`ResolvedRange`/`resolve_generation`/`resolve_range`
(`crates/shell/render/src/emission.rs`, non modifié par le prototype). Ces
deux chemins coexistent ; le second n'est aujourd'hui exposé que par des routes
**expérimentales non publiques** — voir §11.7 et §11.8.

### 11.2 Contextualisation de route : résolue en amont, jamais au runtime

Le garde-fou déjà documenté en §1bis (`if`/`else`, `==`/`!=`,
`eliminate_recordless_conditions` — voir `fragment-forge-guide.md` §2.3bis
et §4.8ter pour le détail) s'applique ici directement : la
contextualisation d'une page (menu courant, fil d'Ariane, toute condition
`record.*`) est intégralement résolue à la compilation, avant même que
`RouteDescriptor`/`SegmentDescriptor` n'existent. Le runtime T2A ne
manipule que des `Segment`s déjà tranchés (ADR-011 §3 : « le runtime ne
connaît que le niveau 3 et 4 »). Il n'interprète jamais `.marius`, ni
`route.*`, ni `record.*`, ni les nœuds `IfEq`/`IfNeq`/`Else` de l'AST —
ces derniers n'existent plus une fois `render()` compilé.

### 11.3 `MaterializedSource`, `ResolvedRange` et conservation de génération

`resolve_generation` transforme un `SourceSpec` en `MaterializedSource`
(seule variante résolue aujourd'hui : `MaterializedSource::Mmap { handle:
Arc<PackHtmlIndex> }` ; la variante `Volatile` existe mais n'est pas exploitable :
son contrat est en cours de définition),
par injection d'une fonction `fetch` — jamais par appel direct à
`LiveRegistry` depuis `emission.rs`, qui reste ainsi agnostique du
transport et du mécanisme de résolution (voir §11.5).

`resolve_range` produit un `ResolvedRange<'a>` — une tranche empruntée
(`&'a [u8]`), liée à la durée de vie du `MaterializedSource` qui l'a
produite. Elle n'expose que `ptr()`/`len()`, jamais l'offset d'origine.

La cohérence de génération reste une propriété du `SourceKey`, jamais du
`SourceId` : plusieurs segments peuvent référencer le même `SourceKey` sans
provoquer plusieurs résolutions — c'est le rôle de
`SourceResolutionContext<N>`, qui résout chaque `SourceKey` distinct une
seule fois par requête.

### 11.4 Rotation `ArcSwap` — la génération vit tant qu'une requête la détient

`LiveRegistry::store()` (§5 de ce guide) ne mute jamais l'instance déjà
chargée : il republie uniquement le pointeur que verront les *prochaines*
résolutions (`load()`). Un `Arc<PackHtmlIndex>` déjà cloné par une requête
en cours reste valide indépendamment de toute rotation survenue après ce
clonage — propriété standard du comptage de références, pas un mécanisme
propre à T2A, mais dont le prototype dépend directement pour garantir
qu'une requête T2A en vol ne peut jamais observer une génération
partiellement remplacée. Démontré expérimentalement (I4) par un scénario
où une requête déjà en vol continue de produire l'ancienne génération
après une rotation, tandis qu'une requête ultérieure observe la nouvelle —
voir le handoff pour le détail exact du test.

### 11.5 La frontière expérimentale : `marius-render` s'arrête à `ResolvedRange`

`marius-render` (et donc `emission.rs`) ne dépend d'aucun de `axum`/
`hyper`/`bytes` — vérifié sur son `Cargo.toml`, pas seulement énoncé comme
discipline. La construction `Bytes`/`Body` n'existe donc pas dans
`marius-render` : elle est entièrement portée par le module expérimental
`experimental_t2a.rs`, dans `marius-server`, qui adapte chaque
`ResolvedRange` en un petit type local (`MmapOwner` : un `Arc` cloné +
offset/len, jamais une nouvelle primitive Marius) consommé par
`Bytes::from_owner`, puis assemblé en `Body` avant d'être remis à Hyper.

```text
ResolvedRange[]                     (dernier IR Marius — emission.rs, marius-render)
    │
    ▼  (marius-server uniquement, à partir d'ici)
MmapOwner (Arc cloné + offset/len)
    │
    ▼
Bytes::from_owner
    │
    ▼
Body (Hyper, boucle Phase 5 — inchangée)
    │
    ▼
HTTP
```

### 11.6 Ce que Marius garantit, ce qui reste hors de son contrat

**Démontré, à la frontière `marius-render`/`marius-server` :** aucune
copie du payload mmap n'est introduite par Marius ou par l'adaptateur T2A
entre `ResolvedRange` et `Bytes` — vérifié par égalité de *pointeur*
(`owner.as_ref().as_ptr() == range.ptr()`), pas seulement de contenu.

**Hors du contrat Marius, jamais mesuré ici :** les éventuelles copies
internes que Hyper, Tokio ou le noyau pourraient effectuer en aval (mise
en file, buffers d'écriture, vectorisation) ; le comportement interne
exact de `Bytes::from_owner` (allocation de bookkeeping propre à la crate
`bytes`) ; toute allocation propre à Hyper. Ces couches ne sont ni bornées
ni auditées par ce prototype — voir SPEC v2 §4/§6.

### 11.7 Statut

`experimental_t2a.rs` reste un module **PROVISOIRE** : ses routes sont montées
sous le préfixe non public `/__experimental/t2a`, hors `ROUTE_TABLE`. Deux
familles y coexistent :

* les **fixtures historiques K=1/K=3** (statiques, écrites à la main) — elles ne
  sont pas une sortie de la Forge et ne démontrent aucune segmentation de
  production ;
* **une route par entrée de la déclaration de publication** (§11.8), dont le
  `RouteDescriptor` K=1 est généré par le build, résolue par le catalogue réel
  `SourceKey → artefact` (plus aucun mapping écrit en dur).

Cette dernière route prouve la chaîne de production Forge → T2A sur
`/content/{id}` (mêmes octets que le chemin monolithique, mêmes statuts 404/400) ;
elle **ne démontre pas** une segmentation : K=1 n'a qu'un segment. Voir le handoff
pour les écarts restants.

### 11.8 Déclaration de publication — `publication.toml`

Depuis l'intégration Forge → T2A K=1, la relation *route → artefact → paramètre*
n'est plus écrite à la main dans chaque crate. Elle est déclarée **une seule
fois** dans `crates/core/schema/publication.toml`, lu par le build de
`core/schema` (`build/publication.rs`) :

* `[[artifact]]` : `key` (l'`ArtifactKey`, ex. `content_core`) et `component`
  optionnel (`content.core`) ;
* `[[route]]` : `name`, `pattern` (`/content/{id}`), `artifact`, `parameter`
  (`id`), `selection = "primary_key"`.

Le build valide le manifeste (structure, existence du composant, PK simple) et
génère dans `generated_schema.rs` : `ARTIFACTS`, `<KEY>_ARTIFACT`,
`<KEY>_SOURCE_KEY`, `<NAME>_ROUTE` (un `RouteSpec` neutre), `ROUTES` et
`ROUTE_DESCRIPTORS`. Les représentations propres à chaque crate en sont
**dérivées** :

```text
RouteSpec (marius-projection, neutre)
  ├─ RouteEntry      (marius-render — `route_entry_from_spec`, const)
  │     → ROUTE_TABLE, DUMP_ROUTE_TABLE, topologie du LiveRegistry
  └─ RouteDescriptor (généré par le build — représentation T2A, K=1)
```

Trois identités à ne pas confondre :

```text
component_id  ≠  ArtifactKey  ≠  SourceKey(u16)
```

* `component_id` : identité logique du composant Forge (`content.core`) ;
* `ArtifactKey` : identité de l'artefact publiable ; son `as_str()` **est** le
  `packfile_key` du runtime (nom du pack : `{racine}/{clé}.bin`, §8) ;
* `SourceKey(n)` : position de l'artefact dans `ARTIFACTS`, handle de catalogue
  **non persistant** (peut changer entre deux builds).

Un artefact peut exister sans composant, et un composant peut produire plusieurs
artefacts : la déclaration ne suppose ni l'un ni l'autre. Le paramètre HTTP (`id`)
et la colonne SQL de la clé primaire (`document_id`) sont deux identités
distinctes, jamais renommées pour coïncider ; la politique de parsing du
paramètre et le code 400 restent côté serveur.

Contrainte de topologie (§7) : une clé d'artefact servie par T2A doit figurer dans
la topologie du `LiveRegistry`, sans quoi son pack n'est jamais ouvert.

---

_Créé le 7 juillet 2026._
_Mis à jour le 25 août 2026_
_Mis à jour le 18 septembre 2026 — corrections de nommage (renvois croisés vers `fragment-forge-guide.md`) et précision sur le garde-fou §4.8/§4.8ter (conditions `record.*` désormais éliminées, pas seulement rejetées)._
_Mis à jour le 19 septembre 2026 — ajout §11 (chemin T2A expérimental I1→I6, statut PROVISOIRE) ; sections 1→10 inchangées, chemin AOT monolithique non affecté._
_Corrigé le 21 septembre 2026 — pipeline réactif à deux étages (`ingest_and_swap` puis `regenerate_and_swap` : le `fetch_batch` généré lit le `store.bin`, il n'interroge pas PostgreSQL) ; résolution des chemins d'artefacts (`MARIUS_ARTIFACTS_DIR`) ; provisioning du store ; topologie du `LiveRegistry` ; `marius-dump` ; §11.7 mis à jour et §11.8 ajouté (déclaration de publication `publication.toml`, `ArtifactKey`/`SourceKey`). Aucun changement au format du pack ni à la fusion `merge_sweep`._
