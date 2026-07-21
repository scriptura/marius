# Data Flow Specification — Cible Architecturale (Phase 1)
## Réactivité Copy-on-Write à double `merge` — Marius

**Statut du document** : spécification de **cible**, antérieure au code. N'implémente ni ne documente l'état actuel (`fetch_batch` lisant un `store.bin` périmé, non rafraîchi par le cycle réactif) ni l'anti-pattern rejeté (couplage SQL live directement dans le moteur de rendu). Toute divergence entre ce document et un futur audit de code doit être traitée comme une régression à corriger, pas comme une mise à jour de la spec — sauf décision explicite contraire, actée par un nouvel arbitrage.

**Arbitrages actés en amont, non rediscutés ici** :
- Le code généré (`db-forge`) fait autorité sur toute documentation antérieure divergente.
- Modèle Copy-on-Write, pas de patch in-place — physiquement impossible sur un layout DOD à sections contiguës et offsets absolus interdépendants (`store.bin`).
- Algorithme `merge_store` dédié, zéro-allocation, obligatoire dès la V1 — la réutilisation directe de `PackfileBuilder::push_batch` sur des lignes non modifiées (matérialisation `VarlenRefs → VarlenOwned`) est rejetée : coût O(taille totale de la table) par tick, incompatible avec la discipline d'allocation minimale déjà en vigueur ailleurs dans le système.

---

## 1. Vue d'ensemble

```
UPDATE SQL ──▶ TRIGGER ──▶ NOTIFY(id) ──▶ Collector.insert(id)
                                              │
                                    Dispatcher::run() (tick ou notify)
                                              │
                                     ids = Collector.flush()
                                              │
              ┌───────────────────────────────┴────────────────────────────────┐
              │  ÉTAGE 1 — Ingestion DOD (merge_store)  [implémenté, Étapes 1-4]│
              │  fetch_delta_from_pg(ids) = P::fetch_from_pg(pool, ids)         │
              │  merge_store(old store.bin mmap, delta) → store.bin.tmp        │
              │  fsync → VALIDATION (PackfileReader::open sur .tmp)            │
              │  rename atomique OS → store.bin (même inode que le handle validé)│
              │  P::store_registry().swap(Arc<PackfileReader<P>>)              │
              └───────────────────────────────┬────────────────────────────────┘
                                              │
              ┌───────────────────────────────┴────────────────────────────────┐
              │  ÉTAGE 2 — Rendu AOT (regenerate_and_swap, INCHANGÉ)            │
              │  fetch_batch(ids) → P::store_registry().load().lookup(id)      │
              │                     (mmap, zéro SQL, zéro allocation)          │
              │  BatchRenderer::render_batch → pack.bin.tmp (merge_sweep)      │
              │  rename atomique OS → pack.bin                                 │
              │  LiveRegistry.store(key, Arc<PackHtmlIndex>)                   │
              └───────────────────────────────┬────────────────────────────────┘
                                              │
                                   Requête HTTP ── pread ──▶ pack.bin
                                   (chemin chaud, zéro SQL, zéro allocation)
```

Coût explicitement assumé : deux écritures disque complètes (rename + fsync) par tick de régénération au lieu d'une, en échange d'un chemin chaud HTTP inchangé — lock-free, O(1), densité de cache CPU maximale, zéro SQL sur le chemin de lecture.

---

## 2. Formats binaires concernés

Aucun changement de format n'est requis. Les deux protocoles binaires existants restent la source de vérité unique, inchangée :

| Format | Défini dans | Consommé par |
|---|---|---|
| `store.bin` (`PackfileStoreHeader`) | `marius_projection::lib.rs` | `PackfileBuilder` (écriture), `PackfileReader` (lecture) — **et désormais `merge_store`** |
| `pack.bin` (`PackfileFooter`/`PackfileEntry`) | `crates/shell/render/src/pack_html_format.rs` | `BatchRenderer` (écriture), `pack_html_index.rs` (lecture), `merge_sweep` (fusion) |

`store.bin` : trois sections physiques dépendantes, calculées par offsets cumulatifs — `id_index` (i64, stride 8B), `varlena_toc` (`VarlenSlot{offset,len}`, stride 8B × nombre de champs varlena), `varlena_heap` (tassé, sans padding entre entrées). `records` (stride fixe `sizeof(P::Record)`, `#[repr(C)]`/`Pod`) précède `id_index`.

---

## 3. Étage 1 — Ingestion DOD

### 3.1 Déclenchement

Identique au mécanisme `Dispatcher` existant (`dispatcher.rs`, inchangé sur ce point) : `tokio::select!` entre tick adaptatif et `Notify`, `ids = Collector.flush()`, tri (`ids.sort_unstable()`).

### 3.2 `fetch_delta_from_pg`

Pas de nouvelle méthode sur le trait `Projection`. Réemploi direct de `P::fetch_from_pg(pool, ids)` — déjà généré par `db-forge` (`codegen/projection.rs`), déjà exercé par `dumper::dump_table` pour le dump complet. Ici, appelé avec le sous-ensemble `ids` du delta plutôt qu'avec la totalité de la table. Aucune troisième voie d'accès SQL à faire naître sur le trait.

Résultat : `Vec<(P::Record, P::VarlenOwned)>`, trié par id croissant (hérité de la clause `ORDER BY {pk} ASC` déjà présente dans le SQL généré — `codegen/projection.rs`).

**Discipline de suppression** : un id présent dans `ids` mais absent du résultat de `fetch_from_pg` est une suppression. Réplique la sentinelle déjà en vigueur côté `pack.bin` (`regenerate.rs`, `DeltaEntry{offset:0,length:0}`) — ici, matérialisée par l'absence de l'id dans le `Vec` de résultat plutôt que par une entrée à longueur nulle explicite (le format `store.bin` n'a pas de notion de "delta" à ce niveau, seulement de "ligne présente ou absente").

### 3.3 `merge_store` — algorithme [implémenté, Étape 2]

Module `crates/shell/render/src/merge_store.rs` — **pas** `crates/core/projection` comme envisagé initialement : `PackfileBuilder` vit dans `marius-render` (Shell), jamais l'inverse (Core ne dépend pas de Shell), donc `merge_store` doit être co-localisé avec lui. Discipline Core conservée (aucune dépendance mmap/fichier/runtime async au-delà de ce que `PackfileReader`/`PackfileBuilder` exposent déjà), mais pas au sens du placement de crate. TODO architectural posé dans le code : réévaluer ce placement en fin de Contrat d'Implémentation, si `PackfileBuilder` migre un jour vers `core/projection`.

**Signature réelle** :

```rust
pub fn merge_store<P: Projection>(
    old: &PackfileReader<P>,
    delta: &[(P::Record, P::VarlenOwned)],
    deleted_ids: &[i64],
    out: &mut PackfileBuilder<P>,
) -> MergeStoreReport
where
    P::Record: Pod,
```

`MergeStoreReport` réel : `runs_count`, `rows_copied_from_old`, `rows_inserted_from_delta`, `rows_updated`, `rows_deleted` — pas de champ `bytes_written` (retiré, redondant avec la taille du fichier produit, disponible autrement si besoin).

**Principe** : sweep à deux curseurs sur `old.id_index()` (trié, invariant du format) et `delta` (trié par construction SQL), identique en forme au sweep déjà validé dans `sweep.rs` (`old_lt_delta`, extension de run, flush obligatoire à la rupture) — mais la décision prise à chaque pas doit être appliquée **simultanément aux trois canaux** (`id_index`, `records`, `varlena_toc`+`heap`), pas à un seul comme `merge_sweep`/`pack.bin`.

**Deux extensions d'API requises et implémentées** (non anticipées avant l'écriture du code, cf. rapport d'Étape 2) :
- `PackfileReader` : `records()`, `id_index()`, `toc()`, `heap()`, `varlena_field_count()` — rendues publiques (étaient privées).
- `PackfileBuilder` : nouvelle méthode `push_raw_run(records, toc, heap_base_offset, heap)` — memcpy pur, aucun passage par `encode_varlena`. Documentée comme invariant d'API logique (contrat non vérifiable par le type système), pas comme dette.

**Contrainte zéro-allocation pour les runs non modifiées** :
- `id_index` / `records` : stride fixe. Une run de lignes non touchées `[run_start, run_end)` se copie par un **unique `memcpy`** sur la tranche `records[run_start*stride .. run_end*stride]` de l'ancien mmap — pas de boucle par ligne, adressage positionnel direct (`position = index × stride`), plus simple que `flush_run` (pas d'offset+len à porter par entrée).
- `varlena_toc` + `heap` : chaque ligne écrit ses champs varlena de façon contiguë dans le heap (propriété héritée de `PackfileBuilder::push_batch`, qui appelle `encode_varlena` séquentiellement ligne par ligne — `packfile_builder.rs` l.57-68). Une run de lignes non touchées a donc un span heap contigu, span = `[toc[run_start].0.offset .. toc[run_end-1].dernier_champ.offset+len]` — traité par un memcpy unique du heap, exactement sur le modèle de `flush_run` (`sweep.rs`), le TOC correspondant recopié puis ses offsets décalés par un `shift` constant sur toute la run (même logique que `sweep.rs` l.199-217, appliquée au niveau slot plutôt qu'entrée).
- Lignes touchées (insert/update) : `record`/`VarlenOwned` déjà matérialisés par `fetch_from_pg` (allocation déjà payée à l'extraction SQL, pas une allocation supplémentaire introduite par le merge) — écrites via `PackfileBuilder::push_batch` standard.

**Sortie** : `merge_store` alimente directement un `PackfileBuilder<P>` réutilisé (mêmes types, mêmes méthodes `push_batch`/`write` que `dumper.rs`) — pas de nouveau format binaire, pas de code d'écriture dupliqué. Seule la logique de *sélection/fusion* des lignes à pousser est nouvelle ; la sérialisation finale reste celle déjà testée dans `packfile_builder.rs`.

**Rapport de fusion** (`MergeStoreReport`) : `runs_count`, `rows_copied_from_old`, `rows_inserted_from_delta`, `rows_updated`, `rows_deleted` — pas de champ de taille en octets (retiré par rapport à l'esquisse initiale, jugé redondant).

**Invariants requis, non vérifiés par le format lui-même** (responsabilité de l'appelant, mêmes principes que `sweep.rs` C1/C2) :
- `old.id_index()` strictement trié — garanti par construction du format (`PackfileReader::lookup` en dépend déjà).
- `delta` strictement trié par id — hérité de la clause SQL `ORDER BY`.

### 3.4 Écriture et bascule [implémenté, Étapes 1, 3, 4 — plus à l'état de cible]

**Mécanisme réel, plus un invariant abstrait** : `StoreRegistry<P>` (`crates/core/projection/src/store_registry.rs`) — mono-slot par `Projection` (pas de `HashMap`/clé, contrairement à `LiveRegistry` pour `pack.bin` : `fetch_batch` est monomorphisé sur `P` à la compilation, donc une `static` par table suffit). `std::sync::RwLock<Option<Arc<PackfileReader<P>>>>` — tranché contre `arc-swap` par l'absence de cette dépendance dans `Cargo.toml` et la classification « Core (no_std attitude) » de `crates/core/projection`. Spécification complète : `DESIGN-store-registry.md`.

Accès générique : `Projection::store_registry() -> &'static StoreRegistry<Self>`, méthode de trait (pas seulement `cold_start_store()`, inhérente) — nécessaire pour qu'une fonction générique `<P: Projection>` (`ingest_and_swap`) atteigne la `static` propre à `P` sans la nommer. Absente de la première version du code généré (Étape 3), ajoutée rétroactivement à l'Étape 4 quand le besoin est apparu à l'implémentation.

**Séquence réelle** (`ingest_and_swap`, `crates/shell/render/src/ingest_and_swap.rs`) :

```
P::fetch_from_pg(pool, ids).await         // async, hors permis d'I/O disque
merge_store(...) → PackfileBuilder rempli // spawn_blocking
File::create(store.bin.tmp) + write + fsync
PackfileReader::open(&store.bin.tmp)      // VALIDATION — avant le rename, pas après
    ├─ échec → supprime le .tmp, retourne Err, store.bin/registre intacts
    └─ succès → rename(store.bin.tmp → store.bin)   // même filesystem, métadonnées seules
               → P::store_registry().swap(Arc::new(reader_déjà_validé))
```

**Écart assumé vis-à-vis de `DESIGN-store-registry.md` §6, documenté, pas silencieux** : ce document prévoyait une réouverture de validation *après* le `rename`. L'implémentation valide *avant*, sur le `.tmp` — sur un `rename` intra-filesystem (garanti par construction), c'est strictement plus sûr : aucune fenêtre, même théorique, où un fichier non validé pourrait porter le chemin canonique, y compris en cas de crash entre `rename` et `swap`. Le handle obtenu par cette validation est réutilisé pour le `swap` (`rename` ne change pas l'inode) — pas de second `open()`. `DESIGN-store-registry.md` §6 corrigé en conséquence.

**Permis d'I/O disque** : `ingest_and_swap` accepte `io_semaphore: &tokio::sync::Semaphore` — la **même instance** que celle transmise à `regenerate_and_swap` (pas une instance indépendante), pour que la pression disque totale d'un tick (deux étages désormais) reste bornée par un seul budget plutôt que doublée — point que cette DFS ne tranchait pas explicitement et qui a été réglé à l'implémentation.

**Transactionnalité des effets de bord, vérifiée par test** (`ingest_and_swap.rs::tests`) : tout échec avant le `rename` (SQL, écriture, validation) laisse `store.bin` et `StoreRegistry` strictement inchangés — testé explicitement pour un échec SQL et un échec d'écriture, dans les deux cas disque et registre restent bit-à-bit identiques à l'état antérieur.

**Note sur `create_dir_all`, absent de l'implémentation réelle** : contrairement à `regenerate_and_swap` (qui le fait, cas du dump initial), `ingest_and_swap` ne crée pas le répertoire parent. Ce n'est pas un oubli : par construction, `ingest_and_swap` ne peut s'exécuter qu'après un `cold_start_store()` réussi au bootstrap (Étape 7), lequel exige déjà que `store.bin` existe — donc que son répertoire parent existe. Si cette précondition venait à changer, ce point serait à revoir.

---

## 4. Étage 2 — Rendu AOT

**Inchangé dans son mécanisme.** `regenerate_and_swap` (`regenerate.rs`), `fetch_delta_batch`, `BatchRenderer::render_batch`, `apply_merge_io_sync`, `merge_sweep` (`sweep.rs`) restent exactement le code déjà audité et testé. Le seul changement de comportement, invisible à ce code, est que `P::fetch_batch(pool, ids)` retourne désormais des données **fraîches** (lues via `StoreRegistry`, alimenté à l'étage 1 juste avant), plutôt que des données figées au dernier `marius-dump`.

---

## 5. Étage 3 — Chemin chaud HTTP

Inchangé. `pread` sur `pack.bin` via `LiveRegistry`, décrit par le manifeste §5. Aucun fichier de ce périmètre n'a été audité directement cette session (`handlers.rs` non fourni) — ce point reste non vérifié en code, à confirmer séparément si nécessaire.

---

## 6. Orchestration des deux étages

**Décision actée** : un seul `Dispatcher::run()`, exécution séquentielle des deux étages dans le même tick, sur le même `ids` — pas deux boucles indépendantes couplées par un second canal de notification interne. Justification : (a) cohérence temporelle forte — un pack.bin ne doit jamais être régénéré à partir d'un store.bin plus vieux que le delta qu'il est censé refléter ; un découplage par notification introduirait une fenêtre où ce n'est pas garanti ; (b) réutilise directement la boucle `tokio::select!`/tick adaptatif déjà existante, aucune structure de contrôle nouvelle à concevoir/tester.

```rust
// Dispatcher::run(), forme réelle [Étape 4 ; Étape 6 = câblage restant]
let ids = self.collector.flush();
if ids.is_empty() { continue; }
ids.sort_unstable();

ingest_and_swap::<P>(&self.pool, &ids, &self.io_semaphore).await?;
regenerate_and_swap::<P>(&self.pool, &ids, self.total_cap, self.packfile_key, &self.registry, &self.io_semaphore).await?;
```

`ingest_and_swap` n'a pas besoin de recevoir explicitement un registre ou une clé (contrairement à l'esquisse initiale de cette section) : l'accès à `StoreRegistry<P>` se fait en interne, via `P::store_registry()` (méthode de trait, §3.4). Seul `io_semaphore` est partagé explicitement entre les deux appels — **la même instance**, pas une par étage (cf. §3.4).

**Échec à l'étage 1** : si `ingest_and_swap` échoue, l'étage 2 ne doit pas s'exécuter (sinon `pack.bin` serait régénéré à partir d'un `store.bin` non rafraîchi, silencieusement incohérent avec le delta courant) — comportement `?`/`continue` du tick, identique à la gestion d'erreur déjà en place dans `Dispatcher::run` pour `regenerate_and_swap`.

---

## 7. Hors périmètre explicite de cette spécification

- ~~Le bug de génération du JOIN...~~ **Résolu** : correctif appliqué et vérifié (Étape 5) — `ON {schema}.{table}.{pk_col} = {vs}.{vt}.{fk_col}`, échec de build explicite si PK composite + jointure varlena.
- ~~La conception du registre atomiquement remplaçable...~~ **Résolu** : `StoreRegistry<P>` est implémenté et testé (Étapes 1, 3, 4), spécifié dans `DESIGN-store-registry.md`. Ce point n'est plus hors périmètre — cf. §3.4.
- Le cas `author_biography`/jointure multi-saut (`content.document → content.core.author_entity_id → identity.entity → identity.person_biography.entity_id`) — hors périmètre, déjà exclu explicitement par `SPEC-phase0-varlena-et-js-deps.md`.
- `js_deps` (Phase 1 au sens du handoff original) — hors périmètre de ce document, qui ne couvre que la réactivité varlena.

---

## 8. Composants à créer ou modifier — récapitulatif

| Composant | Nature | Statut |
|---|---|---|
| `merge_store.rs` (`crates/shell/render`, pas `core/projection` — cf. §3.3) | Nouveau | **Fait** — Étape 2 |
| `StoreRegistry<P>` (`crates/core/projection`) | Nouveau | **Fait** — Étapes 1, 3 ; spec `DESIGN-store-registry.md` |
| `ingest_and_swap` (`crates/shell/render`) | Nouveau | **Fait** — Étape 4 |
| `PackfileReader` — `records()`/`id_index()`/`toc()`/`heap()` publiques | Modification, non anticipée avant l'Étape 2 | **Fait** |
| `PackfileBuilder` — `push_raw_run()` | Ajout, non anticipé avant l'Étape 2 | **Fait** |
| `Projection` — `store_registry()` sur le trait | Ajout, non anticipé avant l'Étape 3 | **Fait** — Étape 4 (correction rétroactive) |
| `codegen/projection.rs` — `fetch_batch`/`cold_start_store`/`store_registry` | Modification | **Fait** — Étape 3 |
| `codegen/projection.rs` — correctif JOIN (`fetch_from_pg`) | Modification distincte | **Fait** — Étape 5 |
| `Dispatcher::run` | Modification | **Fait** — Étape 6 |
| `batch_renderer.rs`/`regenerate.rs` — fixtures de test (`store_registry()`, `StubRecord` sans padding) | Modification, non anticipée avant l'Étape 9 | **Fait** — trouvé en branchant le code réel complet |
| Bootstrap (`main.rs`, `cold_start_store()` avant tout `Dispatcher`) | Câblage | **Fait** — Étape 7, confronté au vrai `main.rs` |
| Audit `handlers.rs`/`crates/shell/server` | Audit | **Clos** — Étape 8, aucun appel direct trouvé |
| Audit `registry.rs`/`pack_html_index.rs` | Audit | **Clos** — avant Étape 9, aucune divergence trouvée |
| Validation bout-en-bout (critère n°5) | Test d'intégration | **Fait** — Étape 9, exécuté avec le code réel, `tests/e2e_criterion5.rs` |
| `PackfileReader`, `merge_sweep`, `regenerate_and_swap` (algorithme), `BatchRenderer`, `pack_html_format`, `dumper`, `LiveRegistry`, `PackHtmlIndex` | Inchangés dans leur logique | Réutilisés tels quels |

**Toutes les étapes du Contrat d'Implémentation (1 à 9) sont closes.** Cette DFS est désormais synchronisée avec le code livré — plus aucun écart connu entre ce document et l'implémentation.
