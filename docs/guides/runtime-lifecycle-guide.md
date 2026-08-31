# Guide runtime — du `render()` compilé à la requête HTTP servie

> Complémentaire de `guide-fragment-forge.md` (compilation `.marius` → `render()`).
> Ce document couvre la couche suivante : comment `render()` est effectivement
> invoqué, sur quel déclencheur, et ce qui invalide un artefact déjà écrit.
> Périmètre disjoint par construction — voir le renvoi en tête du guide
> `fragment-forge`.
>
> **Créé le 7 juillet 2026**, à la suite d'une session de débogage complète du
> pipeline `.marius` → HTTP.
>
> **Mis à jour après l'introduction du pipeline de fusion incrémentale
> CoW/Sweep Merge (Phases 4.2/4.3)** : le `store.bin` n'est pas une source de
> lecture de la régénération HTML. Le delta est récupéré directement depuis
> PostgreSQL via `Projection::fetch_batch`, rendu en mémoire, puis fusionné
> avec la génération HTML actuellement servie.

## Schéma global — deux pipelines de nature différente et leur jonction runtime

Il faut distinguer deux temporalités.

Le premier graphe est un graphe **AOT/build-time** : il transforme les
templates `.marius` en code Rust compilé contenant les fonctions `render()`.

Le second est le graphe **réactif/runtime** : une mutation SQL produit un
événement `NOTIFY`, qui déclenche la récupération du delta depuis PostgreSQL,
son rendu, sa fusion avec le pack HTML courant et enfin le swap atomique du
registre de lecture.

```
              BUILD TIME (cargo build)                     RUNTIME (processus vivant)
              =======================                      ==========================

        templates/*.marius                                      UPDATE / INSERT / DELETE (SQL)
                │                                                          │
                ▼                                                          ▼
          fragment-forge                                          trigger PostgreSQL
                │                                                          │
                ▼                                                          ▼
        render() généré (source)                                       pg_notify
                │                                                          │
                ▼                                                          ▼
           cargo build                                               PgListener
                │                                                          │
                ▼                                                          ▼
         ┌─────────────┐                                            Collector::insert(id)
         │   binaire   │                                                  │
         └─────────────┘                                                  ▼
                │                                                  Collector::flush()
                │                                                          │
                │                                                          ▼
                │                                                    Dispatcher
                │                                                          │
                │                                                          ▼
                │                                                P::fetch_batch(pool, ids)
                │                                                          │
                │                                                          ▼
                │                                                BatchRenderer::render_batch
                │                                                          │
                │                                                          ▼
                │                                                   DeltaBatch en mémoire
                │                                                          │
                └────────────── render() compilé ◄─────────────────────────┤
                                                                           ▼
                                                              merge_sweep(old_pack, delta)
                                                                           │
                                                                           ▼
                                                               nouveau pack HTML (.bin)
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
binaire runtime, puis est invoqué lorsque le chemin réactif traite un delta.

Inversement, le graphe runtime ne « recompile » jamais `render()`.

Ainsi :

* modifier un `.marius` puis effectuer `cargo build` produit un nouveau
  `render()` dans le binaire ;
* cette recompilation ne régénère aucun pack HTML existant ;
* une fois le nouveau binaire lancé, une régénération HTML doit encore être
  déclenchée par le mécanisme runtime approprié ;
* cette régénération récupère les données concernées depuis PostgreSQL,
  exécute le `render()` compilé, puis fusionne le résultat avec le pack
  actuellement servi.

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

> **`{table}_store.bin` et `{table}.bin` ne forment pas une chaîne de lecture
> `store → pack`.**

Le premier est un artefact de transport/dump des données brutes. Le second
est le pack HTML effectivement servi. Le chemin normal de régénération du
second récupère ses données directement depuis PostgreSQL.

Il existe par ailleurs une catégorie distincte de pages statiques (§1bis)
qui ne participe pas du tout au cycle `NOTIFY`/Dispatcher.

## 1. Les artefacts et leurs responsabilités — ne jamais les confondre

| Artefact                           | Producteur                                 | Contenu                                        | Déclencheur / invalidation                                             |
| ---------------------------------- | ------------------------------------------ | ---------------------------------------------- | ---------------------------------------------------------------------- |
| `render()` (dans le binaire)       | `cargo build` → `fragment-forge`           | Code Rust généré depuis `.marius`              | Modification du `.marius` ou des entrées surveillées par `build.rs`    |
| `{table}_store.bin`                | `marius-dump` / `dumper::dump_table`       | Lignes brutes DOD, format de transport interne | Ré-exécution de `marius-dump`                                          |
| `{table}.bin`                      | `regenerate_and_swap`                      | Pack HTML fusionné, avec blob + index + footer | Delta runtime traité par le `Dispatcher` ; provisioning initial séparé |
| `{table}.html` pour `STATIC_PAGES` | `resolve_static_page` / `emit_static_html` | HTML statique déjà composé                     | `cargo build` de `core/schema`                                         |

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

Le `store.bin` contient les données brutes nécessaires au sous-système de
stockage/dump.

Il est produit par `dumper::dump_table`.

**Il n'est pas lu par `regenerate_and_swap`.**

Le code réel de `regenerate.rs` établit explicitement cette séparation :

```rust
let delta = fetch_delta_batch::<P>(pool, ids, total_cap).await?;
```

puis :

```rust
P::fetch_batch(pool, chunk)
```

La source des données du rendu est donc PostgreSQL, et non le
`{table}_store.bin`.

### 1.3 `{table}.bin` — le pack HTML

Le pack HTML est l'artefact effectivement consommé par le chemin HTTP.

`regenerate_and_swap` :

1. récupère les identifiants du delta ;
2. interroge PostgreSQL avec `P::fetch_batch` ;
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
déclarées dans `STATIC_PAGES` (`crates/core/schema/build.rs`).

Aujourd'hui, cette catégorie comprend notamment `offline`.

Ces pages ne participent à aucun des cycles précédents :

| Artefact       | Producteur                                 | Contenu               | Invalidation                   |
| -------------- | ------------------------------------------ | --------------------- | ------------------------------ |
| `{table}.html` | `resolve_static_page` / `emit_static_html` | HTML composé au build | `cargo build` de `core/schema` |

Aucun `NOTIFY`, `PgListener`, `Collector`, `Dispatcher` ou
`regenerate_and_swap` n'est nécessaire.

Cette séparation est structurelle : une page sans donnée SQL dynamique n'a
pas de raison de traverser le pipeline réactif.

Voir `guide-fragment-forge.md` §4.8 pour le garde-fou empêchant une page
statique de référencer silencieusement une donnée dynamique.

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
                              regenerate_and_swap
                                      │
                                      ▼
                            P::fetch_batch(pool, ids)
                                      │
                                      ▼
                              render_batch()
                                      │
                                      ▼
                                DeltaBatch
                                      │
                                      ▼
                         merge_sweep(old, delta)
                                      │
                                      ▼
                              {table}.bin.tmp
                                      │
                                      ▼
                            fsync + rename
                                      │
                                      ▼
                         LiveRegistry::store()
```

### 3.1 Le delta ne vient pas du `store.bin`

C'est le point de correction le plus important par rapport à l'ancienne
documentation.

Le `Collector` fournit à `regenerate_and_swap` les `ids` du tick courant :

```rust
regenerate_and_swap::<P>(
    pool,
    ids,
    ...
)
```

Ces identifiants sont ensuite transformés en données de rendu par :

```rust
P::fetch_batch(pool, chunk)
```

Le flux réel est donc :

```text
ids
 ↓
PostgreSQL
 ↓
Record
 ↓
render()
 ↓
DeltaBatch
 ↓
merge_sweep()
```

et non :

```text
ids
 ↓
store.bin
 ↓
render()
 ↓
pack
```

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

`fetch_delta_batch` récupère uniquement ces identifiants, par chunks de
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

## 4bis. Suppression — absence PostgreSQL transformée en delta de suppression

Le contrat de `fetch_delta_batch` mérite également d'être explicité.

Pour chaque ID demandé, si PostgreSQL ne retourne aucune ligne, cet ID est
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
P::fetch_batch()
   │
   └── aucune ligne
          │
          ▼
    DeltaEntry(id=42,
               offset=0,
               length=0)
          │
          ▼
      merge_sweep
          │
          ▼
      suppression du
      fragment existant
```

La suppression n'est donc pas propagée par un fichier `store.bin` : elle est
déduite directement du résultat de la requête PostgreSQL correspondant au
delta.

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

Le fetch PostgreSQL est volontairement hors du sémaphore :

```text
fetch PostgreSQL
       │
       ▼
attente io_semaphore
       │
       ▼
spawn_blocking
       │
       ▼
merge + I/O disque
```

Le sémaphore régule la pression d'I/O disque et les risques de
dirty-page storm ; il ne limite pas artificiellement les requêtes PostgreSQL.

Le permis est acquis juste avant `spawn_blocking` et reste détenu pendant
tout le noyau physique.

## 6. `marius-dump` — deux artefacts indépendants

`marius-dump` peut produire deux choses distinctes :

1. le `{table}_store.bin`, via `dumper::dump_table` ;
2. le pack HTML `{table}.bin`, lorsqu'il appelle le chemin de régénération
   correspondant.

Ces deux opérations ne doivent pas être comprises comme :

```text
store.bin → pack.bin
```

mais comme deux productions indépendantes :

```text
                    PostgreSQL
                    /        \
                   /          \
                  ▼            ▼
          dump_table()    regenerate_and_swap()
                  │            │
                  ▼            ▼
             store.bin      table.bin
```

Le chemin normal de `regenerate_and_swap` ne lit pas le premier pour produire
le second.

Lorsqu'un dump initial doit rendre immédiatement cohérents les artefacts
d'un environnement, il faut donc considérer séparément :

* la production du store ;
* le provisioning/rendu du pack HTML.

## 7. Provisioning initial du pack HTML

Un packfile absent n'est pas nécessairement une corruption.

`ensure_provisioned` distingue :

```text
packfile absent
     │
     ▼
provisionnement
     │
     ▼
packfile vide mais valide
```

et :

```text
packfile déjà présent
     │
     ▼
aucune écriture
```

Le provisioning est idempotent.

Il ne vérifie volontairement pas la validité d'un fichier déjà présent : cette
responsabilité appartient au lecteur (`PackHtmlIndex::open`) lors du
`cold_start`.

Le séquencement est donc conceptuellement :

```text
ensure_provisioned()
        │
        ▼
cold_start()
        │
        ▼
LiveRegistry
```

Le provisioning ne dépend ni de `PgPool` ni de `LiveRegistry`.

## 8. Résolution de chemin des artefacts — un seul `artifacts/`, par convention de CWD

`packfile_path_for(key)` résout un chemin relatif au répertoire courant du
processus au lancement.

Lancer un binaire depuis un autre répertoire peut donc produire un autre
`artifacts/`.

Par exemple :

```text
workspace/
└── artifacts/
    └── article.bin
```

n'est pas nécessairement le fichier utilisé si le processus a été lancé
depuis :

```text
workspace/crates/shell/server/
```

Dans ce cas, un autre :

```text
crates/shell/server/artifacts/article.bin
```

peut être créé.

Règle opérationnelle :

> lancer les binaires du projet depuis la racine du workspace.

En cas de doute :

```bash
find / -name "{table}*.bin" -exec ls -la {} \;
```

permet de retrouver les exemplaires parasites et de comparer leurs mtime et
leurs tailles.

La page statique `build/{theme}/{table}.html` relève en revanche de
`CARGO_MANIFEST_DIR` dans `core/schema/build.rs` et n'obéit pas à cette même
résolution CWD-relative.

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
regenerate_and_swap
```

Commencer par vérifier le trigger directement en base.

### 4. Le `fetch_batch` récupère-t-il bien les données attendues ?

La régénération lit PostgreSQL directement.

Le `store.bin` n'est pas le bon endroit à inspecter pour déterminer pourquoi
le HTML n'a pas été rendu avec une valeur SQL récente.

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

Le point important est que **l'échec du fetch PostgreSQL intervient avant toute
écriture disque**. Le test `fetch_failure_leaves_old_packfile_and_registry_untouched`
formalise cette propriété.

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
                    IDs du delta
                            │
                            ▼
                  PostgreSQL fetch_batch
                            │
                            ▼
                    records en mémoire
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

Et surtout, **il ne faut pas insérer `store.bin` dans ce graphe**.

`store.bin` appartient à un autre chemin :

```text
PostgreSQL
    │
    ▼
marius-dump / dumper::dump_table
    │
    ▼
store.bin
```

Les deux chemins peuvent être produits par le même outillage de dump, mais
ils ne constituent pas une chaîne de dépendances de lecture.

---

_Créé le 7 juillet 2026._
_Mis à jour le 25 août 2026_
