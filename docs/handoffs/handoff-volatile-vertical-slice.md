# Handoff final — Vertical slice Volatile `content + username` (T2A, K=3)

**Ce document est la SEULE source des directives de cadrage de la prochaine
session.** Il est autosuffisant : il ne suppose l'envoi d'aucun second prompt
correctif. Écrit le 21 septembre 2026 à l'issue de la cartographie V0 (sessions
« Forge → T2A K=1 » puis « premier cas Volatile »). Il ne contient aucun code.

---

## 1. Statut et contexte

**Nature :** document de transition, pas une spécification. La mission de la
prochaine session est **l'implémentation** (V1, puis V2, puis V3), par
incréments courts, avec la discipline de I1→I6 et du premier incrément Forge→T2A.
Elle n'a pas à refaire une cartographie générale, ni à rouvrir ADR-011 ou la
SPEC T2A v2.

**Hiérarchie normative à respecter :** ADR-011 › SPECIFICATION-transport-segmente-t2a
(v2) › ce handoff › anciennes sections du DESIGN. Les sections §4, §7 et §9 du
DESIGN (`EmissionPlan`, `IoSlice[]`, backend `SingleFile/Scatter`) sont en retard
sur la SPEC v2 et ne sont **pas** une référence ; le modèle « arène par worker »
du DESIGN §11 n'est plus un invariant (voir §4).

**Posture :** architecte système / ingénieur Rust / spécialiste AOT-DOD-ECS.
Vouvoiement. Réponses concises, structurées, opérationnelles. Pas de généralités,
pas de métaphores.

**Si un message de l'utilisateur en cours de session semble contredire ce
document, le signaler avant d'agir** (voir §14). Les réponses de l'utilisateur
aux questions de démarrage (§10) complètent ce document ; elles ne le contredisent
pas.

---

## 2. Règles de session (non négociables)

1. Si un fichier ou une ressource manque, **s'arrêter et le demander** ; ne
   jamais conjecturer son contenu.
2. **Clause d'échappement** : en cas de contradiction entre deux directives, ou
   entre le dépôt et ADR-011 / SPEC v2 / ce handoff, s'arrêter et rapporter la
   contradiction précisément — ne pas la résoudre soi-même.
3. **Toujours livrer les modifications en fichiers complets sur disque**
   (chemins du dépôt sous `/mnt/user-data/outputs/`), jamais de simples diffs.
4. **Incréments courts.** Après chaque incrément : `cargo build`, `cargo test`,
   `cargo clippy` chez l'utilisateur, puis **validation de l'utilisateur avant
   de poursuivre**. Ne jamais enchaîner deux incréments sans cette validation.
5. Ne pas fabriquer de segmentation artificielle. Ne pas généraliser K=3.
6. Dire explicitement ce qui n'a pas pu être exécuté dans le sandbox (voir §15).
7. Ne jamais redemander un fichier sous un nom déjà utilisé : `registry.rs`
   existe côté `render` ET côté `db-forge` (deux contenus différents, l'un a déjà
   écrasé l'autre sur disque). Exiger des noms distincts.

---

## 3. Acquis K=1 (validé par l'utilisateur : build + test + clippy OK)

Ne pas rouvrir :

- `crates/core/schema/publication.toml` — manifeste AOT : `[[artifact]]` (`key`,
  `component?`) et `[[route]]` (`name`, `pattern`, `artifact`, `parameter`,
  `selection = "primary_key"`). Aujourd'hui : un artefact `content_core`
  (composant `content.core`) et une route `/content/{id}` (`content_document`).
- `crates/core/projection/src/publication.rs` — types neutres du runtime :
  `ArtifactKey`, `ArtifactSpec`, `RouteSelection::PrimaryKey { column }`,
  `RouteSpec`, `artifact_for_source`, `source_key_for_artifact`.
- `crates/core/schema/build/publication.rs` — parsing (`toml::Table`),
  validation structurelle et croisée avec les composants et leur PK résolue,
  génération dans `generated_schema.rs` : `ARTIFACTS`, `<KEY>_ARTIFACT`,
  `<KEY>_SOURCE_KEY`, `<NAME>_ROUTE`, `ROUTES`, `ROUTE_DESCRIPTORS` (K=1). Ses
  tests sont montés sous `cfg(test)` par `schema/src/lib.rs`
  (`#[path = "../build/publication.rs"] mod publication_build`).
- `crates/core/schema/src/publication_tests.rs` — tests du code généré.
- `crates/shell/render/src/route_derive.rs` — `route_entry_from_spec` (const) ;
  `ROUTE_TABLE` et `DUMP_ROUTE_TABLE` en dérivent ; `dump.rs` ne redéclare plus
  ni la route ni la clé d'artefact.
- `crates/shell/server/src/experimental_t2a.rs` — routes T2A NON publiques sous
  `/__experimental/t2a` : fixtures historiques K=1/K=3 (statiques, PROVISOIRES) et
  une route par entrée de `ROUTES` (`/__experimental/t2a/content/{id}`).
  Catalogue `SourceKey → artefact` réel. `resolve_route_to_response` factorise la
  résolution ; `RESOLUTION_CAPACITY = 4` est une limite locale du prototype, pas
  un invariant Forge.
- `crates/shell/server/src/main.rs` — test unique
  `t2a_experimental_regression_suite` (monolithique et T2A servent les mêmes
  octets, 404/400 identiques, rotation `ArcSwap`). Un seul test écrit le pack
  réel de l'artefact du contenu (collision de parallélisme sinon).
- Audits textuels existants à respecter : le code de production
  d'`experimental_t2a.rs` ne doit contenir ni `read_at` ni la chaîne `Vec<u8>`
  ni les littéraux de clé/motif de route ; celui de `main.rs` ne doit contenir
  ni `"content_core"` ni `"/content/{id}"`. Toute évolution qui heurte un audit
  se **signale**, elle ne le contourne pas en silence.

Frontière T2A démontrée : `ResolvedRange[] → Bytes::from_owner → Body → Hyper`,
sans copie du payload mmap ; génération résolue conservée pendant la requête
malgré une rotation. `RouteDescriptor.backend_kind` reste un champ obligatoire
de la structure, posé à une valeur neutre non consommée : ce n'est pas un concept
à réintroduire.

**Aucune segmentation K>1 de production n'existe encore.**

---

## 4. Décisions verrouillées

### 4.1 K=3 (C1)

Pour ce cas réel, la réponse est :

```text
segment 0 = static prefix
segment 1 = volatile <li>
segment 2 = static suffix
```

Motif : dans `navigation.marius`, le `<ul class="sub-nav">` a du statique avant
**et** après l'item ; le flux `.marius` aplati par `lower` est un flux unique.
K=3 n'est **pas** une borne globale ni une constante architecturale : le nombre
de segments est une propriété de la représentation AOT réellement produite.
Pas de `K_AOT`. La limite « K ≤ 2 » de la première démonstration est retirée.

Le découpage préfixe/suffixe est le mécanisme nécessaire au vrai Volatile, pas
une « segmentation statique de démonstration ». Un test K=3 purement statique ne
peut servir que de **test mécanique isolé** du découpage Forge ; il ne prouve pas
le motif ADR-011. La preuve architecturale est l'indépendance des cycles
(`G1 + V1 → G1 + V2` sans régénération de `content_core`).

### 4.2 Contrat de stockage volatile (C2)

```text
producer
  → owned storage
  → effective_len
  → ResolvedRange
  → owner conservé jusqu'au drop du Body
```

Verrouillé : pas de raw pointer non possédé ; pas d'arène de worker réutilisée
prématurément ; `effective_len <= capacity` ; capacité AOT (`VolatileSlot.capacity`)
distincte de la longueur effective ; owner `Send + 'static` approprié au passage
jusqu'à Hyper ; pas de copie intermédiaire du payload lorsque le modèle de
stockage choisi permet naturellement de l'éviter. Le modèle « arène par worker
avec reset à l'acquisition » n'est **pas** un invariant : il est incompatible avec
`Bytes::from_owner` (Hyper écrit le corps après le retour du handler).

Une allocation par requête n'est **pas** une violation automatique du contrat
Marius si elle est nécessaire à la propriété sûre du chemin Volatile : la
**documenter** ; ne pas chercher de pool ni de zero-allocation prématurément.

### 4.3 Sélection et producteur

```text
StaticArtifact
    → sélection
    → lookup

VolatileSlot
    → ProducerKey
    → NotApplicable
    → matérialisation directe
```

Un segment volatile n'a **aucune** sélection par PK. Ne jamais utiliser
`Constant(0)` ni `RequestSlot(0)` comme valeur fictive. Noms de travail :
`SegmentSelection::NotApplicable` et `ProducerKey` (clé de producteur opaque,
catalogue de build, même règle de numérotation que `SourceKey`). Ne les changer
que si un conflit réel dans le code l'impose — et alors le rapporter.

### 4.4 Identités (à conserver explicitement)

```text
component_id  ≠  ArtifactKey  ≠  SourceKey(u16)
```

- `component_id` : identité logique du composant Forge / modèle de données.
- `ArtifactKey` : identité canonique de l'artefact publiable ; un artefact peut
  exister sans composant, un composant peut produire plusieurs artefacts.
- `SourceKey(n)` : position dans `ARTIFACTS`, handle de catalogue **non
  persistant** (peut changer entre deux builds).

Le préfixe et le suffixe du premier slice montreront précisément pourquoi un même
composant peut produire plusieurs artefacts. `content_core_head` /
`content_core_tail` sont des **noms de travail des artefacts du premier vertical
slice**, jamais une convention universelle obligatoire.

### 4.5 Statut du monolithique et du segmenté

- Le chemin monolithique `/content/{id}` (artefact complet `content_core`) peut
  rester présent **pendant l'expérimentation, comme voie de non-régression et de
  transition**. Il n'est pas la cible architecturale de cette route et n'est pas
  non plus « du legacy à éliminer partout ».
- Pour `/content/{id}` **avec le profil utilisateur**, la représentation T2A
  segmentée est la **cible fonctionnelle** : ce mécanisme n'est pas destiné à être
  jeté après le test. La finalité est d'ajouter ensuite authentification/session
  au-dessus.
- Règle de fond :

```text
Représentation monolithique
    = légitime lorsqu'une représentation unique suffit
      et qu'aucun cycle indépendant ne doit être isolé.

Représentation segmentée
    = produite lorsqu'il existe des projections / cycles
      indépendants qui doivent être réunis dans la réponse
      sans explosion combinatoire.
```

- Ne **pas** en faire la règle « chaque page → full + prefix + suffix ». Ne pas
  demander le retrait de l'artefact complet en V1/V2 sans étude spécifique ; ne pas
  transformer leur coexistence en obligation permanente de produire trois
  artefacts par composant.

### 4.6 Autres décisions

- Le runtime reste ignorant de `.marius`, du DOM, de HTMX, du SQL. Toute
  connaissance de domaine (table, requête) est confinée à l'implémentation du
  producteur ; `emission.rs` n'appelle que des clés opaques.
- **HTMX est écarté** au profit d'un script frontend minimal type micro-kernel.
  Cibler les encarts volatils par des `data-*` dans les templates est souhaité
  **plus tard** ; ne pas s'y attarder (pointeur : `static_markers.rs` /
  `extract_static_data_attribute_tokens` existent déjà côté Forge).
- Ne pas réintroduire `EmissionPlan`, `IoSlice`, `writev`/`sendmsg`,
  `MSG_ZEROCOPY`, `SingleFile/Scatter`, `IOV_MAX`, `K_AOT`.
- Le contrat Volatile modifie ce que SPEC v2 §8 laisse hors périmètre : il
  **étend** la SPEC sans la contredire ; une courte **note normative du contrat
  Volatile** (propriétés P1–P8 du §6) est à livrer avec V1.

---

## 5. Raison d'être du vertical slice

Le `username` n'est pas un simple substitut pratique à un `site_notice`. Il
représente le **premier cas réel de projection éparse** cherché depuis ADR-011 :

```text
document
+ profil utilisateur
+ future projection panier
+ future notifications
+ ...
```

Ces informations peuvent être étrangères à la route elle-même, étrangères les unes
aux autres, et produites selon des cycles distincts. Le problème que T2A doit
résoudre est d'éviter l'explosion combinatoire

```text
document × profil × panier × notifications × ...
```

de pages AOT complètes. Le slice `content + username` démontre le mécanisme sur un
seul cas simple, **sans élargir le périmètre de V1** : pas de panier, pas de
notifications, pas d'authentification ici.

### Ce que chaque étape établit

- **V1 prouve** que le runtime sait recevoir `Static → Volatile → Static` avec :
  ownership sûr, longueur effective, capacité AOT, `ResolvedRange`,
  `Bytes::from_owner`, Body HTTP, et un instantané cohérent pendant la durée de vie
  du Body.
- **V2 rend réel** côté Forge : à partir du template réel, produire
  `AOT prefix → VolatileSlot → AOT suffix`.
- **V3 rend réel** côté source : brancher le producteur volatile sur
  `identity.account_core → premier utilisateur déterministe → username`, et
  démontrer l'indépendance du cycle (`G1 + Alpha → username change → G1 + Bob`,
  sans régénération du pack `content_core`).
- **Après (non traité ici)** : authentification/session, plusieurs projections
  indépendantes.

---

## 6. Contrat Volatile

Le producteur injecté de V1 est un **double expérimental** permettant de tester le
runtime sans PostgreSQL. Ce n'est pas encore une API métier définitive, ni un
producteur volatile générique final, ni une stratégie de cache ou d'invalidation.
Le contrat doit toutefois être **assez réel** pour que V2 et V3 puissent s'y
brancher sans le jeter.

**Propriétés :**

- **P1** — aucun raw pointer dans `MaterializedSource::Volatile`.
- **P2** — la longueur effective est portée par le stockage ; `ResolvedRange`
  = (ptr, longueur effective). `longueur > capacité` au moment de la
  matérialisation = erreur contrôlée (500), jamais de lecture ou d'écriture hors
  bornes. Le traitement produit (troncature ?) n'est pas décidé : 500 par défaut.
- **P3** — `MaterializedSource::Volatile` porte un handle **possédé et
  partageable**. `marius-render` n'a aucune dépendance `bytes`/`axum`/`hyper` : types
  `std` uniquement.
- **P4** — l'adaptateur côté server (comme `MmapOwner`) clone ce handle dans un
  owner passé à `Bytes::from_owner` : le stockage vit jusqu'au drop de la frame.
- **P5** — production **avant** la construction du `Body` ; les octets sont un
  instantané ; aucune lecture après libération de l'owner. Le point d'appel du
  producteur est dans le handler asynchrone, **avant** la résolution synchrone des
  plages statiques, afin qu'aucune référence empruntée ne traverse un `await`
  (V3 : lecture SQL asynchrone).
- **P6** — capacité dérivée de la Forge (`VolatileSlot.capacity`) ; le total
  `RouteDescriptor.volatile_capacity` devient une somme dérivée et vérifiée, jamais
  choisie par le runtime.
- **P7** — `NotApplicable` : invariant `VolatileSlot ⇔ NotApplicable`, vérifié par
  le générateur (erreur de build) et par le runtime (500, jamais de panic).
- **P8** — `SourceSpec::VolatileSlot` porte une `ProducerKey` opaque ; le runtime ne
  connaît que cette clé. Le dispatch vers l'implémentation du producteur vit
  **hors** de `emission.rs`.

**État actuel à faire évoluer** (code réel) : `MaterializedSource::Volatile {
arena_ptr: *const u8 }` — pas de longueur, raw pointer donc `!Send` ;
`resolve_generation` et `resolve_range` renvoient `None` pour Volatile ;
`SourceSpec::VolatileSlot { capacity }` — pas d'identité de producteur ;
`SegmentSelection` n'a que `Constant(i64)` et `RequestSlot(RequestValueId)` (pas de
`repr(C)`, pas d'assertion de layout : ajouter une variante est peu coûteux) ;
`RequestArena` (`Box<[u8]>` + curseur) existe dans `emission.rs`, non branchée. Les
`match` exhaustifs et les tests concernés (projection, `emission.rs`,
`experimental_t2a.rs`) sont à mettre à jour.

**Réalisations comparées** — à confirmer avec l'utilisateur au début de V1b :

| | Stockage | Allocations / requête | Copie après production | Remarques |
|---|---|---|---|---|
| **R-A** (référence proposée) | `String::with_capacity(cap)` (buffer natif du code généré) dans un newtype `Arc`-é, types `std` | buffer (≤ cap) + `Arc` + owner de `Bytes::from_owner` | aucune | `Arc::from(Vec<u8>)` copierait : envelopper le stockage, ne pas le convertir |
| R-B | `Bytes::from(String)` côté server | 1 | aucune | contourne `MaterializedSource`/`ResolvedRange` : ne respecte pas « `ResolvedRange[]` = dernier niveau Marius » (SPEC §2) |
| R-C | `RequestArena` possédée par requête | buffer + `Arc` | **copie** String→arène (le code généré écrit dans `String`) | sauf modification du codegen |
| R-D | arène poolée + garde RAII | ~0 en régime | aucune | **hors périmètre** (pool) |
| R-E | arène par worker, reset à l'acquisition | 0 | — | **non sûre** avec un corps streamé |

Coût documenté de R-A : quelques petites allocations par requête, bornées par la
capacité AOT (jamais par un payload arbitraire), conformes à l'esprit de SPEC §4
(coût borné, hors contrat zero-alloc de la famille segmentée).

---

## 7. Cas fonctionnel `content + username` et faits confirmés

### Cas

```text
GET /content/{id}        (route T2A non publique pendant l'expérimentation)

segment 0  StaticArtifact(préfixe)   sélection = RequestSlot(0)  ← document_id
segment 1  VolatileSlot(ProducerKey) sélection = NotApplicable
segment 2  StaticArtifact(suffixe)   sélection = RequestSlot(0)  ← document_id
```

Le `<li>` volatile est le **segment entier** (squelette AOT : classes, icône via
`{% asset %}` résolu au build ; plus le `username` échappé), pas une interpolation
dans le segment AOT. Il se trouve dans le `<ul>` du menu principal, juste avant
l'item « Repository » de `navigation.marius`. Les deux segments statiques sont
sélectionnés par le **même** `document_id` : c'est le même contexte de route.

### Base de données

- Table **`identity.account_core`**, PK **`entity_id INT`** (FK →
  `identity.entity(id)`), colonne **`username VARCHAR(32) NOT NULL UNIQUE`**. Aucun
  CHECK de charset sur `username` (le CHECK `^[a-z0-9-]+$` porte sur `slug`) →
  **échappement HTML obligatoire**. Borne pire cas : `32 × 6 = 192` octets
  (`HTML_ESCAPE_FACTOR = 6`).
- Seed (`db/dml/master_schema_dml.pgsql`) : entités 1–4 = personnes ; comptes
  entités 5–74 (70). Entités 5–8 : `Alpha`, `Beta`, `Delta`, `Rogue One` ; 9–74 :
  `seed_usr_NN`. **Premier utilisateur déterministe :** `ORDER BY entity_id ASC
  LIMIT 1` sur `identity.account_core` → **`Alpha`** (entité 5).
  `anonymize_person(5)` le changerait en `user_5` ; `is_visible` n'est pas
  filtré (cas de démonstration).
- **RLS** : activé sur `account_core` ; SELECT autorisé si `entity_id =
  rls_user_id()` ou bit 256. Requête anonyme (GUC absents → `-1`/`0`) : **0 ligne**
  pour un rôle soumis au RLS. `marius_admin` (BYPASSRLS) et `postgres` contournent.
  `identity.v_account` porte un `WHERE` GUC miroir : aucun chemin sanctionné ne
  donne aujourd'hui un username « public » à un rôle soumis au RLS.
- **Grants** : `marius_user` a `SELECT` sur les tables identity (sauf `auth`,
  `person_contact`), aucun DML. `content.identity` et `content.body` ont leur
  `SELECT` révoqué pour `marius_user` alors que le pipeline réactif les lit : le
  rôle de `DATABASE_URL` au runtime n'est donc pas un `marius_user` simple —
  **inconnu à ce jour** (question §10).
- `identity.account_core` **n'est pas** un composant Forge (14 entrées dans
  `meta.containment_intent`, 14 projections générées) : aucune `Projection` pour
  elle.

### Cycle de mutation

- **Username** : `UPDATE identity.account_core SET username = …` → aucun NOTIFY
  (aucun trigger de notification sur `identity.*` ; triggers présents : dédup de
  slug, immuabilité de `entity_id`, audit vers `identity.dml_audit_log` qui stocke le
  username en clair) → aucun Collector, aucun Dispatcher, aucune régénération.
  L'indépendance est donc vraie **parce que le username n'a aucun cycle réactif
  aujourd'hui** : le test doit vérifier l'absence de régénération sans prétendre
  prouver l'existence d'un cycle réactif côté username.
- **Contenu** : `UPDATE content.core` → NOTIFY `content_core_updates` → Collector
  → `Dispatcher` → `ingest_and_swap` (met à jour `store.bin`) → `regenerate_and_swap`
  (lit le store via `fetch_batch`, écrit le pack, `LiveRegistry.store`).
- `content.body` et `content.identity` notifient le même canal ; `fn_sync_js_deps`
  recalcule `content.core.js_deps` depuis le corps : le corps n'est pas un second
  cycle.
- `anonymize_person` modifie aussi `content.core.author_entity_id` (notifie
  `content_core`) : effet de l'anonymisation, pas d'un renommage.

### Forge / build

- Mode Page (`crates/core/schema/build/template/page.rs`,
  `resolve_page_template`) : `link_chain` → `lower` → `validate_ast` →
  `eliminate_recordless_conditions` → `hoist_and_dedupe_scripts` →
  `split_static_at_marker(SCRIPTS_PLACEHOLDER)` + `splice_hoisted_scripts` →
  `split_static_at_marker(MODULES_PLACEHOLDER)` + `ModulesPlaceholder` →
  `resolve_and_measure` → `generate_segmented_snippet` / `generate_aot_snippet`.
- **`split_static_at_marker`** (`build/template/common.rs`) est un primitif
  **existant et testé** : cherche le marqueur comme sous-chaîne du **premier**
  `FlatPageToken::Static` qui le contient, le scinde en (avant, après), **supprime le
  texte du marqueur**, omet une moitié vide, renvoie `(tokens, splice_index)` ;
  `None` si absent. Deux appels successifs permettent une paire de marqueurs ; le
  fragment peut contenir des tokens non `Static` (`AssetRef`, `Field`). `FlatPageToken`
  (12 variantes, AST « gelé ») n'a aucune variante liée aux routes ; le parseur
  rejette tout mot-clé de bloc inconnu (`RelationalKeyword`).
- **`GENERATED_HEADER`** (même fichier) place `marius_html_escape` (fonction
  privée du crate `marius-schema`) et `push_varlen_slot` en tête de
  `generated_schema.rs` : toute fonction volatile générée doit donc vivre dans ce
  même fichier généré pour réutiliser l'échappeur.
- **`fetch_varlena_cols(pool, schema, table)`** (`db-forge/src/introspect.rs`) ne
  dépend d'aucun composant : pour `identity.account_core` il décrit `username`
  (`Some(32)`, `Escaped`, 192 octets), `slug`, `language`, et `time_zone` (TEXT non
  borné → `cargo:warning` à chaque build). `fetch_pk_column` donne `Single("entity_id")`.
  Un `SchemaIndex` pour le fragment volatile est donc constructible **sans faire de
  `account_core` un composant**.
- **`resolve_and_measure`** ignore le nom d'entité (`Field { entity, field }` est
  résolu par `field` seul) et somme statique + dynamique pire cas de **toutes** les
  branches : un fragment `<li>` mesuré avec `varlena = [username]` donne exactement
  `octets statiques + 192`.
- **`generate_*_snippet`** écrivent dans **`buf: &mut String`**, référencent
  `varlena.<champ>.as_deref()` et `marius_html_escape` ; le contrat « capacité de
  `buf` inchangée » est le contrat de sûreté de la Forge.
- `content.core` est `is_segment` : sa page est rendue en `RenderChunk`s
  `[Buffered][Borrowed body][Buffered]`, **aplatis** à l'écriture du pack
  (`BatchRenderer`) : un id = une entrée contiguë ; `PackHtmlIndex::lookup(i64)` est
  unique. Un préfixe et un suffixe d'un même enregistrement **ne sont pas
  représentables** comme deux entrées d'un même pack sous la même sélection : d'où
  deux artefacts distincts.
- **`Dispatcher::run`** enchaîne toujours `ingest_and_swap::<P>` puis
  `regenerate_and_swap::<P>(…, packfile_key, …)` : une clé de pack par `Dispatcher`,
  pas de mode « store seul ». Deux Dispatchers sur le même canal ingéreraient deux
  fois le même `store.bin`.
- **`LiveRegistry`** (`render/src/registry.rs`) : `HashMap<&'static str,
  ArcSwap<PackHtmlIndex>>` figé à la construction. `cold_start(&'static [RouteEntry])`
  ouvre chaque `packfile_key` une fois ; `load(key)` → `None` si clé inconnue ;
  `store(key, …)` **panique** si la clé n'est pas dans la topologie ; donc
  `regenerate_and_swap` sur une clé hors topologie panique. `with_indices(HashMap)`
  construit un registre **sans `RouteEntry`**. `RouteEntry` et `IdSource` dérivent
  `Debug, Clone, Copy` (pas `PartialEq`).
- **Provisionnement** : `ensure_provisioned(packfile_key)` (dans `regenerate.rs`)
  et `ensure_store_provisioned::<P>()` (`store_provisioning.rs`) écrivent un fichier
  vide valide si absent, ne touchent jamais un fichier présent, sont idempotents et
  ne dépendent que d'une clé ou d'un chemin.
- **`packfile_path_for(key)`** honore `MARIUS_ARTIFACTS_DIR` (défaut `artifacts`) →
  `<dir>/<key>.bin`, comme `P::store_path()` ; le guide runtime §8 (« relatif au
  CWD ») est périmé.
- Dette de doc (à ne pas corriger ici) : `fragment-forge/src/lib.rs` annonce
  `Normal` ×5 alors que le code est à ×6.

---

## 8. Plan d'implémentation

### V1 — Contrat Volatile runtime (sans Forge, sans SQL, sans vrai template)

Fixtures écrites à la main et PROVISOIRES, comme I1→I6 ; producteur = double
expérimental injecté par le code de test. Ne toucher ni à Forge, ni au SQL, ni au
vrai template. Validation utilisateur (build/test/clippy) **après chaque sous-étape**.

- **V1a — contrat : types + tests.** `SegmentSelection::NotApplicable` ;
  `SourceSpec::VolatileSlot` avec `ProducerKey` ; invariants de cohérence
  (`VolatileSlot ⇔ NotApplicable`, flag `VOLATILE`) ; mise à jour des `match`
  exhaustifs et des tests de `projection`. Note normative courte du contrat
  Volatile (P1–P8).
- **V1b — `MaterializedSource` + owner + `resolve_range`.** Variante
  `MaterializedSource::Volatile` possédée (P1–P4) ; `ResolvedRange` volatile avec
  longueur effective ; vérification `longueur ≤ capacité` ; aucun dispatch
  producteur dans `emission.rs`. Tests unitaires : longueur effective ≠ capacité,
  dépassement → erreur contrôlée, absence de copie (égalité de pointeur), drop de
  l'owner.
- **V1c — adaptateur HTTP + fixture K=3 + tests verticaux.** Owner d'adaptation
  volatile → `Bytes::from_owner` dans `experimental_t2a.rs` ; fixture `[statique]
  [volatile] [statique]` (segments statiques pris dans le pack réel de l'artefact du
  contenu, uniquement pour exercer la mécanique du runtime — ce n'est **pas** la
  représentation de la vraie page ni la preuve du motif) ; producteur injecté ;
  scénario R1–R4 avec le double ; ordre, `Content-Length`, échappement, capacité.

### V2 — Forge : préfixe / volatile / suffixe à partir du template réel

Intention (à réaliser, pas à figer) :

```text
template
→ marqueur Volatile
→ séparation AOT
→ artefacts statiques publiables
→ RouteDescriptor K=3
```

**Ne pas figer d'avance** : les noms exacts de tous les artefacts, la manière dont
le générateur paramètre les « parties » de rendu, la forme finale du catalogue,
le mécanisme précis de provisionnement. Ces points se déterminent d'après le code
réel de Forge lu au début de V2, avec l'utilisateur. Éléments établis à utiliser :

- Mécanisme candidat de marquage : commentaires HTML textuels traités par
  `split_static_at_marker` (précédents `SCRIPTS_PLACEHOLDER` et
  `MODULES_PLACEHOLDER`), sans nouveau token ni nouvelle syntaxe. Le point de coupe
  doit être à profondeur `if` nulle (vérification et erreur de build explicite). Le
  point d'insertion recommandé dans `resolve_page_template` : après les splices de
  `SCRIPTS_PLACEHOLDER`/`MODULES_PLACEHOLDER`, avant `resolve_and_measure` (le
  fragment volatile doit être retiré avant la mesure principale, sinon
  `Field { username }` échoue contre le schéma de `content.core`).
- Le fragment volatile est mesuré avec un `SchemaIndex` construit par
  `fetch_varlena_cols("identity", "account_core")` restreint à `username`
  (capacité = statique + 192) ; sa fonction générée doit être émise dans
  `generated_schema.rs`.
- Artefact complet : `prefix ++ suffix` (fragment retiré) se mesure et se génère
  comme aujourd'hui ; il est **conservé** pendant la transition (§4.5).
- Production de plusieurs artefacts pour un même composant : **un seul ingest puis
  N régénérations** ; jamais deux `ingest_and_swap` sur le même store.
- Topologie : une clé d'artefact hors topologie fait paniquer `store()` ;
  les artefacts sans `RouteEntry` monolithique exigent un mécanisme de
  provisionnement/topologie (`with_indices`, `ensure_provisioned` par clé sont
  disponibles) — décision de V2.
- `publication.toml` / `RouteSpec` : plusieurs artefacts pour un composant ; route à
  segments ordonnés ; génération du `RouteDescriptor` K=3 et de `volatile_capacity`.
- Test mécanique isolé : `préfixe ++ suffixe` est égal, octet pour octet, à la page
  complète sans région volatile (par id). Ce test valide le découpage, pas le motif
  ADR-011.

### V3 — Producteur réel et scénario vertical

Brancher le producteur sur `identity.account_core` : lecture du premier utilisateur
par `ORDER BY entity_id ASC LIMIT 1`, échappement via la fonction générée, rôle et
RLS résolus (§10), scénario R1–R4 sur PostgreSQL ; conserver la variante à
producteur injecté pour la CI.

**Mécanisme retenu pour la source (justification factuelle) :** lecture SQLx à la
requête via le `PgPool` existant (créé dans `main()`, aujourd'hui déplacé dans le
`Dispatcher` : à cloner). Aucun état maintenu n'existe pour `account_core` ; un état
maintenu (`StoreRegistry`) exige un composant complet, un trigger NOTIFY, un shard,
et régénère un pack inutile ; ADR-011 §11 cite « buffer PostgreSQL live » comme
source volatile ; c'est le plus petit producteur réel. Conditionné à la résolution
du rôle DB. L'accès aux données reste injectable (sur le patron de `fetch` de
`resolve_generation`) pour tester sans PostgreSQL et laisser la place à un futur
producteur maintenu.

---

## 9. Tests d'acceptation finaux

```text
Initial : contenu G1, username = Alpha
R1 : G1 + Alpha

username → Bob                    (aucun événement)
R2 : G1 + Bob                     (aucune régénération de content_core)

content → G2
R3 : G2 + Bob

R4 matérialise Bob ; username devient Carol pendant que le Body de R4
n'est pas consommé → R4 reste Bob
```

Assertions communes :

- `K = 3` (`segments.len()`) ; ordre préfixe → volatile → suffixe ;
- `Content-Length` = somme exacte des longueurs ;
- capacité vérifiée ; longueur effective **distincte** de la capacité ;
  dépassement de capacité → 500 contrôlé ;
- échappement HTML (`<b>&"'` sort échappé, jamais plus de 192 octets pour un
  username) ;
- absence de copie du payload volatile lorsque démontrable (égalité de pointeur entre
  le stockage produit et les octets de la frame) ;
- owner vivant jusqu'au Body : compteur de drop observable, non libéré tant que le
  corps n'est pas consommé ou abandonné ; aucune lecture après libération ;
- cohérence de génération statique : une rotation de génération pendant R4 laisse le
  statique de R4 inchangé (patron de I4) ;
- **aucune régénération de `content_core`** sur mutation du username : `Arc::ptr_eq`
  de la génération avant/après, et (V3) absence d'événement Dispatcher ;
- non-régression : route monolithique `/content/{id}`, fixtures existantes et audits
  textuels existants.

**Portée honnête de R4.** Le test établit que le volatile est matérialisé **avant**
l'envoi des en-têtes et **possédé** par la réponse jusqu'à son drop, si bien qu'une
mutation ultérieure de la source ne change pas les octets de cette réponse. Le
scénario suit le patron de I4 : le client obtient les en-têtes (handler terminé),
la source est mutée, puis le corps est lu. Il **ne prouve pas** l'entrelacement de
requêtes concurrentes, ni le moment exact où Hyper/Tokio/l'OS ont déjà lu des
frames, ni un comportement de backpressure. Ne pas prétendre démontrer plus que ce
que le test établit ; adapter le scénario au comportement réel de Hyper/Body.

Répartition : V1 exécute R1–R4 avec le double ; V2 n'ajoute que le test mécanique de
découpage ; V3 exécute R1–R4 sur la vraie source.

---

## 10. Questions à poser à l'utilisateur au démarrage

1. **Rôle de `DATABASE_URL` au runtime** (`marius_user`, `marius_admin`, `postgres`,
   propriétaire ?) et donc le comportement face au RLS d'`identity.account_core`. S'il
   est soumis au RLS : accord pour une fonction `SECURITY DEFINER` / vue publique
   dédiée, ou autre chemin ? (Ne bloque pas V1/V2, seulement V3.)
2. **Emplacement de la requête SQL** du producteur : écrite côté server, ou
   déclarée dans `publication.toml` (table, champ, mode « premier par clé primaire »)
   et validée/générée par le build — sachant que la seconde option évite de recréer la
   duplication composant/PK supprimée lors du premier incrément.
3. **Confirmer la réalisation de stockage R-A** (ou en choisir une autre) avant V1b.
4. **Dépassement de capacité** : 500 contrôlé (défaut) ou troncature.
5. **Noms de travail** `ProducerKey` / `NotApplicable` à confirmer (V1a).

---

## 11. Fichiers à lire

Documents (déjà fournis lors des sessions précédentes ; redemander s'ils ont disparu
du disque) : `ADR-011-projections-ordonnancees.md`,
`SPECIFICATION-transport-segmente-t2a.md` (v2), `DESIGN-runtime-segment-pipeline
(post-ADR-011).md` (§2–§3 et §11–§13 seulement), `runtime-lifecycle-guide.md`, `fragment-forge-guide.md`,
`handoff-t2a-experimental-integration-i1-i6.md`,
`handoff-forge-t2a-production-integration.md`.

Code : `crates/core/projection/src/{lib.rs, publication.rs, store_registry.rs}` ;
`crates/shell/render/src/{emission.rs, registry.rs (fourni sous le nom
render_registry.rs), lib.rs, route_derive.rs, dispatcher.rs, regenerate.rs,
ingest_and_swap.rs, store_provisioning.rs, batch_renderer.rs, pack_html_index.rs,
bin/dump.rs}` ; `crates/shell/server/src/{experimental_t2a.rs, main.rs, handlers.rs}` ;
`crates/core/schema/{Cargo.toml, publication.toml, src/lib.rs,
src/publication_tests.rs, build/main.rs, build/publication.rs,
build/template/{common,page,static_page,dynamic}.rs,
templates/{base,navigation,head,footer}.marius, templates/content/core.marius}` ;
`crates/forge/fragment-forge/src/{lib.rs, schema.rs, fragment/{token,resolver,codegen,mod}.rs}` ;
`crates/forge/db-forge/src/{registry.rs (dbforge_registry), naming.rs, introspect.rs,
codegen/projection.rs}` ; SQL : `db/02_identity/{01_components,02_systems}.sql`,
`db/08_dcl/01_grants.sql`, `db/09_rls/01_policies.sql`, `db/10_meta_seed/01_manifest.sql`,
`db/12_events/01_notify.sql`, `db/dml/master_schema_dml.pgsql`.

Non encore lus, à demander seulement si nécessaires : `build/{modules_lowering,
capabilities}.rs` (si le lowering des modules est touché) et les `Cargo.toml` de
`server`, `render`, `projection` avant toute dépendance nouvelle.

---

## 12. Fichiers probablement à modifier

- **V1** : `projection/src/lib.rs`, `render/src/emission.rs`, `render/src/lib.rs`
  (réexports), `server/src/experimental_t2a.rs`, `server/src/main.rs` (tests), note
  normative du contrat Volatile.
- **V2** : `build/main.rs`, `build/publication.rs`, `build/template/page.rs` (et
  `common.rs` si nécessaire), `publication.toml`, `templates/navigation.marius`,
  `db-forge/src/codegen/projection.rs`, `projection/src/lib.rs` (rendu par
  parties), `render/src/{dispatcher,regenerate,registry}.rs`, `server/src/main.rs`
  (shards, topologie), `publication_tests.rs`.
- **V3** : `server/src/{main,experimental_t2a}.rs` ; éventuellement un script SQL de
  droits (fonction `SECURITY DEFINER` ou rôle) — **uniquement après accord explicite**.

---

## 13. Hors périmètre (à ne pas traiter)

Authentification ; session ; cookies ; navigateur ; `data-*` ; HTMX ;
WebSocket/SSE ; stratégie générale de notification ; multi-producteurs génériques ;
pool ou zero-allocation ; optimisation transport ; `EmissionPlan` ; `IoSlice` ;
`writev`/`sendmsg`/`MSG_ZEROCOPY` ; `backend_kind` comme concept ; `K_AOT` ;
`IOV_MAX` ; migration globale des chemins d'artefacts ; dette documentaire D1/D2 ;
retrait de l'artefact complet `content_core` ; généralisation de K=3 ; panier et
notifications (mentionnés seulement comme finalité).

---

## 14. Risques résiduels et règle d'arrêt

**Risques :**

- Rôle DB / RLS inconnus (bloquant pour V3 seulement).
- Latence SQL par requête sur le chemin chaud (acceptée pour ce slice ; ADR-011 §11 le
  prévoit) et `await` dans le handler : produire le volatile avant la résolution
  synchrone des plages.
- Production supplémentaire de packs (préfixe + suffixe, en plus du complet
  conservé pendant la transition) : coût disque et régénération.
- « Premier utilisateur » peut devenir `user_5` après anonymisation ; comptes
  `is_visible = false` non filtrés ; username en clair dans `dml_audit_log`.
- Changement de `SegmentSelection`/`SourceSpec` : `match` exhaustifs et tests de
  `projection`, `emission.rs`, `experimental_t2a.rs`.
- Audits textuels existants (`read_at`, `Vec<u8>`, littéraux de clé et de motif) :
  toute évolution qui les heurte se signale.
- `fetch_varlena_cols` sur `account_core` émet un `cargo:warning` (`time_zone` TEXT
  non borné) à chaque build.
- `RESOLUTION_CAPACITY = 4` et `SourceResolutionContext<N>` : contraintes
  d'implémentation du prototype, jamais des invariants Forge.
- Une réponse contenant un volatile n'est pas cacheable comme un pack statique (hors
  périmètre, à ne pas oublier plus tard).

**Règle d'arrêt.** Si l'implémentation ou la lecture du dépôt révèle une
contradiction avec ADR-011, la SPEC T2A v2, le contrat T2A actuel, le modèle
`producer → owned storage → effective_len → ResolvedRange → owner`, ou une
décision verrouillée de ce document : **s'arrêter et rapporter précisément la
contradiction** au lieu de l'arbitrer. Même règle si deux directives de ce document
se contredisent, ou si un message ultérieur de l'utilisateur semble contredire ce
document.

---

## 15. Méthode de vérification et rappel final

Le sandbox n'a ni le dépôt complet ni PostgreSQL. Méthode utilisée jusqu'ici :
installer `rustc-1.89`, `cargo-1.89` et `rustfmt-1.89` par `apt` ; monter un espace
de travail scratch reprenant les fichiers **réels** fournis avec des mocks minimaux
pour ce qui manque (`PackHtmlIndex`, `LiveRegistry`, `store_registry.rs`) ;
extraire verbatim les tests de `main.rs` dans le scratch ; `rustfmt --edition 2024
--check` sur les fichiers livrés. `clippy` n'y est pas disponible pour l'édition
2024 (le paquet Ubuntu correspond à 1.75) : l'utilisateur lance
build/test/clippy chez lui. Toujours dire ce qui n'a pas pu être exécuté.

**Rappel explicite : ce handoff est la source unique des directives de la prochaine
session.** Il n'existe pas de second prompt correctif ; en cas de doute ou de
contradiction, appliquer la règle d'arrêt du §14 plutôt que de deviner.
