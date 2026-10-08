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
>
> **Mis à jour le 6 octobre 2026 — chemin T2A segmenté.** Le §11 décrit désormais
> l'état stabilisé du pipeline T2A (route `/content/{id}` en K=3, segment volatile
> `nav_profile`, régénération à trois artefacts) et non plus un prototype I1→I6.
> Les §6, §7 et §10 portent les compléments correspondants ; les autres sections
> décrivent le chemin monolithique et sont inchangées.


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
ensure_provisioned(clé) pour chaque artefact + LiveRegistry::cold_start_with_extra_keys
    │
    ▼
regenerate_and_swap_with_volatile_split()
                             (une ingestion fetch_batch ; écrit {content_core}.bin,
                              {content_core_head}.bin et {content_core_tail}.bin)
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
(§11.9) : `DUMP_ROUTE_TABLE` provient de `route_entry_from_spec`, et les clés
`content_core_head`/`content_core_tail` (aucune route) s'y ajoutent comme clés
supplémentaires. Elle ne réutilise pas la `ROUTE_TABLE` de `marius-server`
(couplage inverse `render → server` proscrit).

`marius-dump` est aussi le seul mécanisme de **première population** : le
`Dispatcher` ne régénère que les ids signalés par le `Collector` après son propre
démarrage, jamais un rattrapage des lignes déjà présentes. Depuis l'introduction
de la région volatile, il peuple les artefacts `head` et `tail` en plus de
l'artefact monolithique (§11.10).


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
* `cold_start_with_extra_keys(&'static [RouteEntry], &[&'static str])` ouvre en
  plus des clés qu'aucune route ne porte — les artefacts `content_core_head` et
  `content_core_tail`, que la régénération doit pouvoir `store()` ; `main.rs` y
  ajoute aussi `content_core`, retiré de `ROUTE_TABLE` mais toujours régénéré ;
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
regenerate_and_swap            (composant sans région volatile)
    │
    ▼
{key}.bin
```

Pour un composant couvert par une région volatile (`content.core`), l'Étage 2
devient `regenerate_and_swap_with_volatile_split` : une seule ingestion, trois
packs (§11.10).


## 11. Chemin T2A — émission AOT segmentée

> Ce chapitre situe la seconde famille d'émission par rapport au reste du guide.
> Il ne se substitue pas aux textes normatifs : ADR-011
> (`ADR-011-projections-ordonnancees.md`), la frontière de transport
> (`docs/archived/SPECIFICATION-transport-segmente-t2a.md`, v2), le contrat Volatile
> (`docs/contrats/CONTRAT-volatile-v1.md`) et le contrat d'augmentation
> (`docs/contrats/CONTRAT-marius-one-page-extension.md`).

### 11.1 Deux familles d'émission, une règle de choix

Le chemin décrit par les sections 1 à 10 (Forge → `render()` →
`regenerate_and_swap` → `LiveRegistry` → HTTP → `pread()`) est l'émission **AOT
monolithique** : une page = une source mmap contiguë, servie par
`read_at`/`spawn_blocking` (`handlers.rs::deliver`). Il n'a pas été modifié par
le chemin segmenté.

Le chemin **AOT segmenté (T2A)** compose une réponse de plusieurs segments,
éventuellement issus de plusieurs sources (`SegmentDescriptor[]`, nombre de
segments fixé AOT par route). Les deux coexistent :

```text
Représentation monolithique
    = légitime lorsqu'une représentation unique suffit
      et qu'aucun cycle indépendant ne doit être isolé.

Représentation segmentée
    = produite lorsqu'il existe des projections / cycles
      indépendants qui doivent être réunis dans la réponse
      sans explosion combinatoire.
```

Ce n'est pas la règle « chaque page → trois artefacts ». Aujourd'hui une seule
route est segmentée.

| Route | Famille | Source |
| --- | --- | --- |
| `/content/{id}` | segmentée, K=3 | `content_document.rs` (`marius-server`), `ROUTE_DESCRIPTORS` généré |
| `/__monolithic/content/{id}` | monolithique | artefact `content_core`, `handlers::serve_route` ; voie de comparaison et de non-régression |
| `/` | monolithique | `ROUTE_TABLE` (artefact `pages_homepage`) |
| `/__experimental/t2a…` | segmentée, fixtures | `experimental_t2a.rs`, PROVISOIRE (§11.11) |

`ROUTE_TABLE` ne contient plus que la route `/` : `/content/{id}` en a été
retirée (un même motif ne se monte pas deux fois dans un `Router` Axum), et le
chemin monolithique du contenu est monté séparément à `/__monolithic/content/{id}`
(`MONOLITHIC_CONTENT_ROUTE`, même handler `serve_route`, aucune seconde
implémentation de lecture). L'artefact `content_core` reste provisionné, ouvert
dans le registre et régénéré par le `Dispatcher`.

### 11.2 Contextualisation de route : résolue en amont, jamais au runtime

Le garde-fou documenté en §1bis (`if`/`else`, `==`/`!=`,
`eliminate_recordless_conditions` — voir `fragment-forge-guide.md` §2.3bis et
§4.8ter) s'applique directement : la contextualisation d'une page (menu courant,
fil d'Ariane, toute condition `record.*`) est intégralement résolue à la
compilation. Le runtime T2A ne manipule que des segments déjà tranchés
(ADR-011 §3 : il ne connaît que les niveaux Segment et Réponse). Il n'interprète
jamais `.marius`, ni `record.*`, ni les nœuds `IfEq`/`IfNeq`/`Else`.

### 11.3 Sources, matérialisation et résolution

Deux sortes de sources (`SourceSpec`, `marius-projection`) :

| `SourceSpec` | Sélection du segment | Matérialisation (`marius-render::emission`) |
| --- | --- | --- |
| `StaticArtifact { key: SourceKey }` | `RequestSlot(n)` : valeur du paramètre de requête | `resolve_generation` → `MaterializedSource::Mmap { handle: Arc<PackHtmlIndex> }` ; `resolve_range` → `ResolvedRange` |
| `VolatileSlot { capacity, producer: ProducerKey }` | `NotApplicable` (jamais `Constant(0)` ni `RequestSlot(0)`) | production hors `emission.rs` ; `resolve_volatile_generation` → `MaterializedSource::Volatile { storage: Arc<VolatileStorage> }` ; `resolve_volatile_range` → `ResolvedRange` |

Les deux chemins de résolution sont distincts par construction : un segment
statique est *sélectionné* dans un artefact, un segment volatile est *produit*.
`resolve_generation` reçoit sa source de vérité par injection d'une fonction
`fetch` — jamais d'appel direct à `LiveRegistry` depuis `emission.rs`, qui reste
agnostique du transport.

`ResolvedRange<'a>` est une tranche empruntée, liée à la durée de vie du
`MaterializedSource` qui l'a produite (`ptr()`, `len()`, `is_empty()`, `as_slice()`).
La cohérence de génération est une propriété du `SourceKey`, jamais du `SourceId` :
`SourceResolutionContext<N>` résout chaque `SourceKey` distinct une seule fois
par requête (`N = 2` dans `content_document.rs`).

Trois identités à ne pas confondre :

```text
component_id  ≠  ArtifactKey  ≠  SourceKey(u16)
```

* `component_id` : identité logique du composant Forge (`content.core`) ;
* `ArtifactKey` : identité de l'artefact publiable ; son `as_str()` **est** le
  `packfile_key` du runtime (`{racine}/{clé}.bin`, §8) ;
* `SourceKey(n)` : position de l'artefact dans `ARTIFACTS`, handle de catalogue
  **non persistant** (peut changer entre deux builds) ; la résolution
  `SourceKey → packfile_key` passe par `artifact_for_source(ARTIFACTS, key)`.

Un composant peut produire plusieurs artefacts (cas de `content.core` :
`content_core`, `content_core_head`, `content_core_tail`) ; un artefact peut
exister sans composant (`pages_homepage`). `ProducerKey` est une troisième
identité de catalogue, distincte de `SourceKey` : elle désigne un producteur de
contenu volatile, pas un artefact.

### 11.4 Rotation `ArcSwap` — la génération vit tant qu'une requête la détient

`LiveRegistry::store()` ne mute jamais l'instance déjà chargée : il republie le
pointeur que verront les *prochaines* résolutions (`load()`). Un
`Arc<PackHtmlIndex>` déjà cloné par une requête en cours reste valide quelle que
soit la rotation survenue ensuite : une requête T2A en vol ne peut jamais observer
une génération partiellement remplacée. C'est une propriété standard du comptage
de références, dont le chemin segmenté dépend directement. Les artefacts `head` et
`tail` d'une même réponse sont résolus par deux `SourceKey` distincts : la
cohérence de génération est garantie *par source*. Le régénérateur (§11.10)
écrit les trois packs dans un même `spawn_blocking`, puis publie leurs générations
par des appels `registry.store()` **successifs** : aucun mécanisme du code actuel
ne rend ces publications atomiques entre elles.

> **Limite actuelle.** Chaque source statique résolue reste cohérente pendant la
> durée de vie de la requête. Les artefacts multiples d'une même représentation
> segmentée (`head`, `tail`) ne bénéficient pas encore d'une publication atomique
> inter-sources : lors d'une rotation, une requête peut théoriquement combiner un
> `head` et un `tail` de générations différentes. Une identité de génération pour
> un ensemble corrélé de sources (un « bundle » côté registre) est un sujet de
> conception futur, non implémenté ; il demande un arbitrage dédié.

### 11.5 La frontière : `marius-render` s'arrête à `ResolvedRange`

`marius-render` ne dépend d'aucun de `axum`/`hyper`/`bytes` (contrat Volatile P3 ;
types `std` uniquement). La construction de `Bytes`/`Body` est portée par `marius-server`
(`content_document.rs`), qui adapte chaque `ResolvedRange` en un type local :

```text
ResolvedRange[]                 (dernier niveau Marius — emission.rs)
    │
    ▼  (marius-server uniquement, à partir d'ici)
MmapOwner (Arc<PackHtmlIndex> cloné + offset/len)     segment statique
VolatileOwner (Arc<VolatileStorage> cloné)            segment volatile
    │
    ▼
Bytes::from_owner
    │
    ▼
Body (flux de frames) → Hyper → HTTP
```

L'owner est conservé vivant par `Bytes::from_owner` jusqu'au drop de la frame :
le stockage volatile vit tant que le `Body` n'est pas consommé ou abandonné. Le
`Content-Length` est la somme des longueurs *effectives* de toutes les frames —
jamais la capacité maximale du segment volatile. Socket, framing, écritures
partielles, backpressure et écriture vectorisée appartiennent à Hyper ; `IoSlice[]`
n'est pas une étape du pipeline Marius.

### 11.6 Ce que Marius garantit, ce qui reste hors de son contrat

**Démontré à la frontière `marius-render`/`marius-server` :** aucune copie du
payload n'est introduite entre `ResolvedRange` et `Bytes`, que la source soit un
mmap ou un `VolatileStorage` — vérifié par égalité de *pointeur*, pas seulement de
contenu.

**Coût accepté, borné par le nombre de segments :** la matérialisation
(`ResolvedRange → Bytes`, un `Vec<Bytes>` par réponse, un `Arc` par segment
volatile, le buffer du producteur). Pour le volatile : une allocation du buffer du
producteur (au plus la capacité AOT), un `Arc`, un owner de `Bytes::from_owner`.
Ce n'est pas une violation du contrat Marius : le zéro-allocation reste un objectif
de la famille monolithique, pas de la famille segmentée (SPEC T2A v2 §4).

**Hors du contrat Marius, jamais mesuré ici :** les copies et allocations internes
de Hyper, Tokio, du noyau, et le bookkeeping de la crate `bytes`.

### 11.7 La route `/content/{id}` (`content_document.rs`)

La route est montée par `content_document::mount(registry)`, mergée dans le
`Router` principal. Elle lit le `RouteDescriptor` généré `content_document`
(K=3) :

```text
segment 0  StaticArtifact(content_core_head)  RequestSlot(0)   ← id
segment 1  VolatileSlot(ProducerKey(0))       NotApplicable
segment 2  StaticArtifact(content_core_tail)  RequestSlot(0)   ← id
```

Les deux segments statiques sont sélectionnés par le **même** paramètre `id` :
c'est le même contexte de route. Le segment volatile est le `<li>` entier
(`<li class="nav-profile">…</li>`), pas une interpolation dans un segment AOT.

Ordre des opérations dans le handler : paramètre `id` (400 si non numérique) →
`VolatileContext` construit depuis la requête → pour chaque segment, dans l'ordre
du descripteur : résolution statique (404 si l'id est absent du pack) ou
production volatile → somme des longueurs → `Body`. Toute incohérence (source ou
sélection incompatibles, producteur inconnu, capacité dépassée) répond par un
statut contrôlé (500), jamais par un `panic`, jamais par une troncature.
La production volatile est synchrone dans l'implémentation actuelle : aucune
référence empruntée ne traverse un point de suspension.

**Contexte expérimental.** Le nom d'utilisateur provient aujourd'hui du paramètre
de requête `?user=…`. C'est un contexte de démonstration, déterministe et sans
authentification : ce n'est **pas** l'architecture d'identité définitive.
L'authentification et la session réelles sont hors périmètre ; le chemin
`requête → VolatileContext → producteur` est, lui, réel, et la source de
`VolatileContext.username` pourra changer sans toucher au producteur.

```text
GET /content/16                    → <li class="nav-profile"></li>
GET /content/16?user=Olivier       → <li class="nav-profile">Olivier</li>
GET /content/16?user=<script>…     → nom d'utilisateur échappé HTML
```

### 11.8 Producteur volatile `nav_profile`

`marius-render::volatile_producers` expose un producteur unique, sélectionné par
un `match` sur la `ProducerKey` de la source (pas de registre, pas de trait de
producteur ; un second producteur ajouterait un bras au `match`) :

```text
VolatileContext { username: Option<String> }     possédé, sans lifetime
        ↓
materialize_volatile(spec, &ctx)
        ↓  match sur ProducerKey (NAV_PROFILE_PRODUCER = ProducerKey(0))
produce_nav_profile(&ctx) → Vec<u8>              nom échappé HTML
        ↓
resolve_volatile_generation → VolatileStorage::from_produced
        ↓                      (effective_len ≤ capacity, sinon erreur contrôlée)
MaterializedSource::Volatile
```

Le nom d'utilisateur est une donnée externe : `& < > " '` sont échappés avant
écriture, et l'échappement compte dans la capacité. La capacité vient de la Forge
(`SourceSpec::VolatileSlot.capacity`, déclarée dans `publication.toml`), jamais du
runtime ; un dépassement est une erreur contrôlée (500), pas une troncature. Le
producteur ne lit aucune base de données : il ne consomme que le
`VolatileContext`.

Le contenu `nav_profile` est produit côté serveur : il n'a besoin d'aucun
JavaScript pour être fonctionnel.

Le contrat complet (propriétés P1–P8, ownership, capacité) est dans
`docs/contrats/CONTRAT-volatile-v1.md`.

### 11.9 Déclaration de publication — `publication.toml`

La relation *route → artefact → paramètre*, et la partition d'un template en
(head, volatile, tail), sont déclarées **une seule fois** dans
`crates/core/schema/publication.toml`, lu par le build de `core/schema`
(`build/publication.rs`) :

* `[[artifact]]` : `key` (l'`ArtifactKey`) et `component` optionnel. Plusieurs
  artefacts peuvent partager un même composant.
* `[[route]]` : `name`, `pattern`, `artifact`, `parameter`, `selection =
  "primary_key"`. `artifact` désigne l'artefact **monolithique**.
* `[[volatile_region]]` : `component`, `marker`, `head_artifact`, `tail_artifact`,
  `capacity`. Au plus une région par composant ; `head_artifact` et
  `tail_artifact` doivent être déclarés, distincts, et porter le même composant.
  `capacity` est la borne AOT du contenu volatile (valeur actuelle : 512 octets,
  provisoire).

Le build valide le manifeste (structure, existence du composant, PK simple) et
génère dans `generated_schema.rs` : `ARTIFACTS`, `<KEY>_ARTIFACT`,
`<KEY>_SOURCE_KEY`, `<NAME>_ROUTE` (`RouteSpec` neutre), `ROUTES` et
`ROUTE_DESCRIPTORS`. Une route dont l'artefact appartient à un composant couvert
par une `[[volatile_region]]` est générée en **K=3** ; toute autre route reste en
K=1. Les capacités de `render_head`/`render_tail` sont émises sous la forme
`{NAME}_HEAD_TOTAL_CAP` et `{NAME}_TAIL_TOTAL_CAP` ; la `ProducerKey` du slot est
fixée par le générateur à `ProducerKey(0)`.

```text
RouteSpec (marius-projection, neutre)
  ├─ RouteEntry      (marius-render — route_entry_from_spec, const)
  │     → DUMP_ROUTE_TABLE (marius-dump)
  └─ RouteDescriptor (généré par le build — représentation T2A, K=1 ou K=3)
```

Le paramètre HTTP (`id`) et la colonne SQL de la clé primaire (`document_id`) sont
deux identités distinctes, jamais renommées pour coïncider ; la politique de
parsing du paramètre et le code 400 restent côté serveur.

Côté template, la région est bornée par une paire de commentaires HTML
`<!-- MARIUS_VOLATILE_BEGIN {marker} -->` / `<!-- MARIUS_VOLATILE_END -->`
(`navigation.marius`) ; leur traitement est décrit dans `fragment-forge-guide.md`
§4.10.

### 11.10 Régénération : une ingestion, trois artefacts

Un composant couvert par une région volatile produit trois artefacts à partir
d'**une seule** ingestion :

```text
Étage 1 — ingest_and_swap          (inchangé : store.bin)
Étage 2 — regenerate_stage
            ├─ sans région : regenerate_and_swap              (K=1, inchangé)
            └─ avec région : regenerate_and_swap_with_volatile_split
                 fetch_batch (une fois par chunk)
                   ├─ rendu monolithique   → {content_core}.bin
                   ├─ render_head          → {content_core_head}.bin
                   └─ render_tail          → {content_core_tail}.bin
```

* `Dispatcher::with_volatile_split(head, tail)` configure la paire
  `SplitRenderTarget { packfile_key, total_cap, render }` pour le seul shard
  concerné (`main.rs`) ; `Dispatcher::new` seul garde le comportement K=1.
* `render_head` et `render_tail` sont des fonctions inhérentes émises par
  `db-forge`, **hors** du trait `Projection` : aucune projection `head`/`tail`
  distincte n'existe.
* Les trois rendus partagent le même batch emprunté : jamais une seconde
  ingestion, jamais deux `Dispatcher` sur le même canal.
* Une mutation du volatile (le nom d'utilisateur) ne passe par aucun de ces
  étages : le producteur lit son contexte à la requête, aucun événement
  `NOTIFY`, aucune régénération, aucun `Arc` de génération statique touché.

**Population initiale.** Le `Dispatcher` ne régénère que les ids signalés par le
`Collector` après son démarrage ; `marius-dump` reste le seul mécanisme de
première population. Il régénère les trois artefacts (store, pack monolithique,
head, tail), faute de quoi `/content/{id}` répond 404 tant qu'aucune écriture n'a
eu lieu en base.

**Topologie.** Les clés `content_core_head` et `content_core_tail` ne sont portées
par aucune `RouteEntry` : elles sont provisionnées (`ensure_provisioned`) puis
ouvertes par `LiveRegistry::cold_start_with_extra_keys`, dans `main.rs` comme dans
`dump.rs`. La clé du pack suit la convention `packfile_path_for(ArtifactKey::as_str())`
(`{racine}/{clé}.bin`) ; `P::packfile_path()` n'est appelé par aucun code de
production (§8).

### 11.11 Statut et périmètre

* **Réel :** `content_document.rs` (route `/content/{id}`), le producteur
  `nav_profile`, le catalogue `SourceKey → artefact`, la régénération à trois
  artefacts, `marius-dump`.
* **PROVISOIRE :** `experimental_t2a.rs`, monté sous `/__experimental/t2a`, hors
  `ROUTE_TABLE` : des fixtures statiques K=1/K=3 écrites à la main (pas une sortie
  de la Forge) et une route par entrée de `ROUTES`. Le comportement de ces routes
  face au descripteur K=3 de `content_document` n'a pas été revérifié pour cette
  mise à jour. `experimental_volatile_t2a.rs` est
  `#[cfg(test)]` : il ne sert que la suite de tests du contrat Volatile.
* **Non couvert :** authentification et session ; producteur lisant PostgreSQL
  (`identity.account_core` n'est pas un composant Forge) ; plusieurs régions ou
  plusieurs producteurs ; combinaison d'une région volatile avec un champ
  `marius:large_content` (`render_head`/`render_tail` sont générés par
  `generate_aot_snippet`, jamais `generate_segmented_snippet`) ; politique de cache
  d'une réponse contenant un volatile (elle n'est pas cacheable comme un pack
  statique) ; HTTP/2.

---

_Créé le 7 juillet 2026._
_Mis à jour le 25 août 2026_
_Mis à jour le 18 septembre 2026 — corrections de nommage (renvois croisés vers `fragment-forge-guide.md`) et précision sur le garde-fou §4.8/§4.8ter (conditions `record.*` désormais éliminées, pas seulement rejetées)._
_Mis à jour le 19 septembre 2026 — ajout §11 (chemin T2A expérimental I1→I6, statut PROVISOIRE) ; sections 1→10 inchangées, chemin AOT monolithique non affecté._
_Corrigé le 21 septembre 2026 — pipeline réactif à deux étages (`ingest_and_swap` puis `regenerate_and_swap` : le `fetch_batch` généré lit le `store.bin`, il n'interroge pas PostgreSQL) ; résolution des chemins d'artefacts (`MARIUS_ARTIFACTS_DIR`) ; provisioning du store ; topologie du `LiveRegistry` ; `marius-dump` ; §11.7 mis à jour et §11.8 ajouté (déclaration de publication `publication.toml`, `ArtifactKey`/`SourceKey`). Aucun changement au format du pack ni à la fusion `merge_sweep`._
_Mis à jour le 6 octobre 2026 — §11 réécrit pour l'état stabilisé du pipeline T2A segmenté (vertical slice Volatile `content + username`, K=3) : sources statiques et volatiles, frontière `VolatileOwner`, route `/content/{id}` et `/__monolithic/content/{id}`, `?user=` présenté comme contexte expérimental, `[[volatile_region]]`, régénération à trois artefacts. §6, §7, §10 complétés._
