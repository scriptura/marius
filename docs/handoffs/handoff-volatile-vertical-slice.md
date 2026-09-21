# Handoff — Vertical slice Volatile : `content + username` (K=3)

**Statut :** document de transition pour une nouvelle session Claude, pas une
spécification. Écrit le 21 septembre 2026 à l'issue de la cartographie V0
(sessions « Forge → T2A K=1 » puis « premier cas Volatile »). Ne rouvre ni
ADR-011, ni SPEC T2A v2, ni la cartographie générale : la mission de cette
session est **l'implémentation**, par incréments courts, avec la même discipline
que I1→I6.

**Rappel de posture :** architecte système / ingénieur Rust / spécialiste
AOT-DOD-ECS. Vouvoiement. Réponses concises et opérationnelles.

---

## 0. Règles de session (non négociables)

1. Si un fichier ou une ressource manque, **s'arrêter et le demander** ; ne
   jamais conjecturer son contenu.
2. **Clause d'échappement :** si une contradiction apparaît entre deux
   directives, ou entre le dépôt et ADR-011 / SPEC v2 / ce handoff, s'arrêter
   et rapporter précisément le conflit — ne pas l'arbitrer soi-même.
3. **Toujours livrer les modifications en fichiers complets sur disque**
   (chemins du dépôt sous `/mnt/user-data/outputs/`), jamais de simples diffs.
4. Les incréments sont **courts** : V1, puis V2, puis V3, chacun validé par
   l'utilisateur (`cargo build` / `cargo test` / `cargo clippy`) avant le
   suivant.
5. Ne pas fabriquer de segmentation artificielle. Ne pas généraliser K=3.

---

## 1. Acquis — ne pas rouvrir

Livré et **validé par l'utilisateur** (build + test + clippy OK) :

- `crates/core/schema/publication.toml` — manifeste AOT : `[[artifact]]`
  (`key`, `component?`) et `[[route]]` (`name`, `pattern`, `artifact`,
  `parameter`, `selection = "primary_key"`). Aujourd'hui : un artefact
  `content_core` (composant `content.core`) et une route
  `/content/{id}` (`content_document`).
- `crates/core/projection/src/publication.rs` — types neutres `ArtifactKey`,
  `ArtifactSpec`, `RouteSelection::PrimaryKey { column }`, `RouteSpec`,
  `artifact_for_source`, `source_key_for_artifact`.
- `crates/core/schema/build/publication.rs` — parsing (`toml::Table`),
  validation (structure + croisement avec les composants et leur PK résolue),
  génération dans `generated_schema.rs` : `ARTIFACTS`, `<KEY>_ARTIFACT`,
  `<KEY>_SOURCE_KEY`, `<NAME>_ROUTE`, `ROUTES`, `ROUTE_DESCRIPTORS` (K=1).
  Ses tests sont montés sous `cfg(test)` par `schema/src/lib.rs`
  (`#[path = "../build/publication.rs"] mod publication_build`).
- `crates/core/schema/src/publication_tests.rs` — tests du code généré.
- `crates/shell/render/src/route_derive.rs` — `route_entry_from_spec` (const)
  ; `ROUTE_TABLE`, `DUMP_ROUTE_TABLE` en dérivent ; `dump.rs` ne redéclare
  plus la route.
- `crates/shell/server/src/experimental_t2a.rs` — routes T2A NON publiques
  sous `/__experimental/t2a` : fixtures historiques K=1 / K=3 (statiques,
  PROVISOIRES) et une route par entrée de `ROUTES`
  (`/__experimental/t2a/content/{id}`). Catalogue `SourceKey → artefact` réel
  (`artifact_for_source(ARTIFACTS, …)`). `resolve_route_to_response` factorise
  la résolution ; `RESOLUTION_CAPACITY = 4` est une limite locale du prototype.
- `crates/shell/server/src/main.rs` — test unique
  `t2a_experimental_regression_suite` (monolithique vs T2A, mêmes octets,
  404/400 identiques, rotation `ArcSwap`).

Frontière T2A démontrée (I1→I6) : `ResolvedRange[] → Bytes::from_owner → Body →
Hyper`, sans copie du payload mmap ; génération résolue conservée pendant la
requête malgré une rotation.

Tout est en place pour K=1 ; **aucune segmentation K>1 de production n'existe**.

---

## 2. Décisions verrouillées

### Architecture

- **K=3** pour ce cas : segment 0 = préfixe AOT, segment 1 = `<li>` volatile,
  segment 2 = suffixe AOT. Motif : dans `navigation.marius`, le
  `<ul class="sub-nav">` a du contenu statique avant **et** après l'item ;
  le flux `.marius` aplati par `lower` est un flux unique. K=3 n'est **pas**
  une nouvelle borne architecturale : le nombre de segments est une propriété
  de la représentation AOT réellement produite. **Pas de `K_AOT`.** Ne pas
  réintroduire `IOV_MAX`, `backend_kind` (comme concept), `EmissionPlan`,
  `IoSlice`, `SingleFile/Scatter`.
- **C1-bis :** le découpage préfixe/suffixe est le mécanisme nécessaire au vrai
  Volatile. Un test K=3 purement statique n'est qu'un **test mécanique** du
  découpage Forge ; il ne prouve pas le motif ADR-011. La preuve
  architecturale = indépendance des cycles :
  `content G1 + username V1 → G1 + V2` sans régénération de `content_core`.
- **Un segment volatile n'a pas de sélection par PK.** Ne pas réutiliser
  `Constant(0)` / `RequestSlot(0)` pour simuler une sélection.
  `StaticArtifact → lookup(selection)` ; `VolatileSlot → matérialisation
  directe`.
- **`component_id` ≠ `ArtifactKey` ≠ `SourceKey(u16)`.** Un composant peut
  produire plusieurs artefacts (c'est exactement le cas préfixe/suffixe de
  `content.core`). `SourceKey(n)` = position dans `ARTIFACTS`, non persistante.
- Le runtime reste ignorant de `.marius`, du DOM, de HTMX, de SQL dans
  `emission.rs`. Toute connaissance de domaine (table, requête) est confinée
  au producteur volatile, jamais à `emission.rs` ni au routage.
- **HTMX est écarté** (script frontend minimal type micro-kernel à la place).
  `data-*` pour cibler les encarts volatils est souhaité **plus tard** :
  ne pas s'y attarder ici. (Pointeur pour plus tard : `static_markers.rs` /
  `extract_static_data_attribute_tokens` existent déjà côté Forge.)

### Ownership Volatile (C2)

- Le modèle « arène par worker avec reset à l'acquisition » du DESIGN §11 n'est
  **pas** un invariant. Il est incompatible avec `Bytes::from_owner` : Hyper
  écrit le corps après le retour du handler ; un tampon réutilisé par la
  requête suivante serait corrompu.
- Le stockage volatile doit vivre **tant que le `Body` peut encore lire les
  octets**, sans raw pointer non possédé, sans UAF, sans copie inutile.
- Une allocation par requête n'est **pas** une violation automatique du contrat
  Marius si elle est nécessaire à la propriété sûre : la **documenter**
  (coût), ne pas chercher de pool ni de zero-allocation prématurément.
- `VolatileSlot.capacity` (borne AOT) ≠ longueur effective produite.

---

## 3. Faits confirmés

### Base de données

- Table : **`identity.account_core`**. PK **`entity_id INT`** (FK →
  `identity.entity(id)`). Colonne : **`username VARCHAR(32) NOT NULL UNIQUE`**.
  Aucun CHECK de charset sur `username` (le CHECK `^[a-z0-9-]+$` porte sur
  `slug`) → **échappement HTML obligatoire**. Borne pire cas :
  `32 × 6 = 192` octets (`VarlenField::HTML_ESCAPE_FACTOR = 6`).
- Seed (`db/dml/master_schema_dml.pgsql`) : entités 1–4 = personnes ; comptes
  entités 5–74 (70 comptes). Entités 5–8 : `Alpha`, `Beta`, `Delta`,
  `Rogue One` ; 9–74 : `seed_usr_NN`. **Premier utilisateur déterministe :**
  `SELECT username FROM identity.account_core ORDER BY entity_id ASC LIMIT 1`
  → **`Alpha`** (entité 5). `anonymize_person(5)` le changerait en `user_5` ;
  `is_visible` n'est pas pris en compte (cas de démonstration).
- **RLS :** `identity.account_core` a le RLS activé ; politique SELECT :
  `entity_id = rls_user_id() OR (rls_auth_bits() & 256) = 256`. Requête
  anonyme (GUC absents → `-1` / `0`) : **0 ligne** pour `marius_user`.
  `marius_admin` et `postgres` contournent le RLS. `identity.v_account` porte
  un `WHERE` GUC miroir : aucun chemin sanctionné ne donne aujourd'hui un
  username « public » à un rôle soumis au RLS.
- **Grants :** `marius_user` a `SELECT` sur les tables identity (sauf `auth`,
  `person_contact`) mais aucun DML. `content.identity` et `content.body` ont
  leur `SELECT` révoqué pour `marius_user` alors que le pipeline réactif les lit
  (JOIN de `content.core`) : **le rôle de `DATABASE_URL` au runtime n'est donc
  pas un `marius_user` simple — inconnu à ce jour.**
- `identity.account_core` **n'est pas** dans `meta.containment_intent`
  (`10_meta_seed/01_manifest.sql`) : ce n'est pas un composant Forge, aucune
  `Projection` n'est générée pour elle.

### Cycle de mutation

- `db/12_events/01_notify.sql` : NOTIFY uniquement sur `content.core`,
  `commerce.product_core`, `content.identity`, `content.body` (canaux
  `content_core_updates`, `commerce_product_core_updates`). **Aucun NOTIFY sur
  `identity.*`.** Triggers sur `account_core` : dédup de slug, immuabilité
  `entity_id`, audit (`identity.dml_audit_log`, `row_to_json` : le username y
  figure). Aucun ne notifie.
- Donc : `UPDATE identity.account_core SET username = …` → **aucun événement,
  aucun Collector, aucun Dispatcher, aucune régénération de `content_core`**.
  Cette indépendance est vraie parce que le username n'a **aucun** cycle
  réactif aujourd'hui.
- Exception : `identity.anonymize_person` modifie aussi
  `content.core.author_entity_id` (donc notifie `content_core`) — effet de
  l'anonymisation, pas d'un renommage.
- `content.body` et `content.identity` notifient le même canal que
  `content.core` ; `fn_sync_js_deps` recalcule `content.core.js_deps` depuis le
  corps (donc le `<head>` dépend du corps) : le corps n'est pas un second cycle.

### Forge / build

- Flux Mode Page (`crates/core/schema/build/template/page.rs`,
  `resolve_page_template`) : `link_chain` → `lower` → `validate_ast` →
  `eliminate_recordless_conditions` → `hoist_and_dedupe_scripts` →
  `split_static_at_marker(tokens, SCRIPTS_PLACEHOLDER)` +
  `splice_hoisted_scripts` → `split_static_at_marker(tokens,
  MODULES_PLACEHOLDER)` + insertion de `FlatPageToken::ModulesPlaceholder` →
  `resolve_and_measure` → `generate_segmented_snippet` /
  `generate_aot_snippet`.
- **`split_static_at_marker`** (`build/template/common.rs`, LU) est un primitif
  **existant** et testé (5 tests). Sémantique exacte : cherche `marker` comme
  sous-chaîne du **premier** `FlatPageToken::Static` qui le contient ; scinde ce
  token en (avant, après), **supprime le texte du marqueur**, omet une moitié
  vide (jamais de `Static("")`), renvoie `(tokens, splice_index)` où l'indice
  tombe exactement entre les deux moitiés ; `None` si absent. Pour une paire
  BEGIN/END : deux appels successifs (BEGIN, puis END sur le flux déjà scindé) ;
  le fragment volatile est `tokens[i_begin..i_end]`, le préfixe `tokens[..i_begin]`,
  le suffixe `tokens[i_end..]`. Le fragment peut contenir des tokens non
  `Static` (`AssetRef`, `Field`) : seuls les marqueurs doivent être dans des
  `Static`. `FlatPageToken` (12 variantes, AST « gelé ») n'a aucune variante
  liée aux routes ; le parseur rejette tout mot-clé de bloc inconnu
  (`RelationalKeyword`).
- **`GENERATED_HEADER`** (même fichier) place `marius_html_escape` (fonction
  privée du crate `marius-schema`, `#[allow(dead_code)]`) et `push_varlen_slot`
  en tête de `generated_schema.rs`. La fonction volatile générée doit donc être
  émise **dans ce même fichier généré** (donc dans `marius-schema`, exportée en
  `pub fn`) pour réutiliser `marius_html_escape` sans nouvel échappeur.
- **`fetch_varlena_cols(pool, schema, table)`** (`db-forge/src/introspect.rs`,
  LU) ne dépend d'**aucun composant** : il liste les colonnes
  `varchar`/`bpchar`/`text` de n'importe quelle table, avec `max_len`
  (`typmod - 4` pour `VARCHAR(N)`), politique d'échappement issue du
  `COMMENT ON COLUMN` (`marius:pre_escaped|raw|large_content`, sinon `Escaped`)
  et `HTML_ESCAPE_FACTOR = 6`. Pour `identity.account_core` : `username`
  (`Some(32)`, `Escaped`, 192 octets pire cas), `slug`, `language`, et
  `time_zone` (TEXT sans borne → `cargo:warning` à chaque build, inoffensif si
  non référencé). `fetch_pk_column(pool, "identity", "account_core")` →
  `Single("entity_id")`. Le `SchemaIndex` du fragment volatile peut donc être
  construit **sans enregistrer `account_core` comme composant**.
- **Composants générés :** `generated_schema.rs` contient 14 projections
  (`cold_start_store` × 14) = les 14 entrées de `meta.containment_intent` ;
  confirmation que `identity.account_core` n'en est pas un.
- **Point d'insertion recommandé de la coupe** [déduit] : dans
  `resolve_page_template`, **après** le splice de `MODULES_PLACEHOLDER` (le flux
  est alors complet : scripts hissés, `ModulesPlaceholder` posé, faits statiques
  calculés) et **avant** `resolve_and_measure`. Le fragment doit être retiré
  avant la mesure principale, sinon `Field { username }` échoue en
  `UnknownField` contre le schéma de `content.core`. L'artefact complet
  (`prefix ++ suffix`) se mesure et se génère alors exactement comme
  aujourd'hui, sans coût de build supplémentaire ; préfixe et suffixe se
  mesurent/génèrent séparément (le choix `generate_segmented_snippet` vs
  `generate_aot_snippet` reste celui du composant : `has_segment`).
- **`resolve_and_measure` ignore le nom d'entité** (`Field { entity, field }`
  est résolu par `field` seul contre un `SchemaIndex`) et somme
  `total_static_bytes` + `total_dynamic_bytes` (pires cas de toutes les
  branches). Un fragment `<li>` (Static + `AssetRef` + `Field`) mesuré avec
  `SchemaIndex { fixed: &[], varlena: &[username] }` donne exactement la
  capacité volatile : `static_bytes + 192`.
- `generate_aot_snippet` écrit dans **`buf: &mut String`** et référence
  `varlena.<champ>.as_deref()` + `marius_html_escape(s, buf)` (fonction émise
  par `generated_file_header`). `Option<&str>` convient comme champ (pas
  d'allocation). Le contrat « capacité de `buf` inchangée » est le contrat de
  sûreté de la Forge.
- `content.core` est `is_segment` (large_content) : sa page est rendue en
  `RenderChunk`s `[Buffered][Borrowed body][Buffered]`, **aplatis** à l'écriture
  du pack par `BatchRenderer` (un id = une entrée contiguë de `PackfileEntry` ;
  `PackHtmlIndex::lookup(i64)` est unique). Un préfixe et un suffixe d'un même
  enregistrement **ne sont pas représentables** comme deux entrées d'un même
  pack sous la même clé de sélection.
- `Dispatcher::run` enchaîne toujours `ingest_and_swap::<P>` puis
  `regenerate_and_swap::<P>(…, packfile_key, …)` — une clé de pack par
  `Dispatcher`, pas de mode « store seul ». Deux Dispatchers sur le même canal
  ingéreraient deux fois le même `store.bin`.
- **`LiveRegistry`** (`render/src/registry.rs`, LU) : `HashMap<&'static str,
  ArcSwap<PackHtmlIndex>>` figé à la construction. `cold_start(&'static
  [RouteEntry])` ouvre le pack de chaque `packfile_key` **une seule fois** (dédup
  par clé) et échoue si un pack est absent ; `load(key)` → `None` si clé
  inconnue ; `store(key, …)` **panique** si la clé n'est pas dans la topologie
  (invariant AOT). Donc une clé d'artefact absente de `ROUTE_TABLE` n'est jamais
  ouverte, et `regenerate_and_swap` sur une telle clé panique. **Un hook existe
  déjà** : `LiveRegistry::with_indices(HashMap<…>)` construit un registre depuis
  une table de clés **sans passer par des `RouteEntry`** — les artefacts
  `head`/`tail` (sans route monolithique) peuvent donc être provisionnés par un
  constructeur dérivé de `ARTIFACTS`, sans fabriquer de fausses routes (décision
  V2). `RouteEntry` et `IdSource` dérivent `Debug, Clone, Copy` (pas
  `PartialEq`).
- **Provisionnement (LU)** : `ensure_provisioned(packfile_key: &'static str)`
  (dans `render/src/regenerate.rs`) écrit un `pack.bin` vide valide
  (`tmp` → `fsync` → `rename`) s'il est absent et ne touche jamais un fichier
  présent ; `ensure_store_provisioned::<P>()` (`render/src/store_provisioning.rs`)
  fait de même pour `P::store_path()` ; toutes deux renvoient
  `ProvisionOutcome::{AlreadyPresent, Provisioned}` et sont idempotentes. Les
  deux ne dépendent que d'une **clé / d'un chemin**, jamais d'un `RouteEntry` :
  pour les artefacts `head`/`tail`, provisionnement = `ensure_provisioned(key)`
  par clé d'artefact, puis construction du registre (`with_indices` ou
  constructeur dérivé de `ARTIFACTS`). Le store étant partagé par les parties de
  rendu d'un même composant, `ensure_store_provisioned` reste appelé une fois
  par type de projection (un second appel serait `AlreadyPresent`).
- **`packfile_path_for(key)`** honore `MARIUS_ARTIFACTS_DIR` (lu une fois dans un
  `OnceLock`, défaut `artifacts`) → `<dir>/<key>.bin`, comme `P::store_path()`.
  **Le guide runtime §8 (« relatif au CWD ») est périmé sur ce point** ; ne pas
  introduire de divergence entre les deux chemins.
- **Dette de doc :** `fragment-forge/src/lib.rs` annonce `Normal` ×5 ; le code
  (`schema.rs`) et le guide sont à ×6.

### Contrat Volatile actuel (insuffisant)

- `MaterializedSource::Volatile { arena_ptr: *const u8 }` : **pas de longueur**,
  raw pointer `!Send`. `resolve_generation` et `resolve_range` renvoient `None`
  pour Volatile. `SourceSpec::VolatileSlot { capacity: u32 }` : **pas
  d'identité de producteur** — rien n'indique *quoi* produire. `SegmentSelection`
  n'a que `Constant(i64)` et `RequestSlot(RequestValueId)` (pas de repr(C), pas
  d'assertion de layout : ajouter une variante est peu coûteux).
  `RequestArena` (`Box<[u8]>` + curseur) existe dans `emission.rs`, non branchée.
- SPEC T2A v2 §8 laisse le Volatile hors périmètre : cette session **étend** le
  contrat, elle ne le contredit pas. Le contrat retenu ci-dessous doit être
  consigné dans une courte note normative livrée avec V1.

---

## 4. Contrat Volatile retenu (minimal)

```text
production volatile
  → stockage possédé (Send + 'static, indépendant de tout worker / arène partagée)
  → (ptr, effective_len)          effective_len ≤ VolatileSlot.capacity
  → ResolvedRange                 borrowed sur ce stockage, longueur = effective_len
  → propriétaire conservé jusqu'à l'émission HTTP (drop quand Hyper lâche la frame)
```

Propriétés :

- **P1** — pas de raw pointer dans `MaterializedSource::Volatile`.
- **P2** — la longueur effective est portée par le stockage ; `ResolvedRange`
  = (ptr, len effectif). `len > capacity` au moment de la matérialisation =
  erreur contrôlée (500), jamais une écriture/lecture hors bornes. Traitement
  produit (troncature ?) : **non décidé**.
- **P3** — `MaterializedSource::Volatile` porte un handle **partageable et
  possédé** (`marius-render` n'a **aucune** dépendance `bytes`/`axum`/`hyper` :
  types `std` uniquement, ex. `Arc<…>`).
- **P4** — l'adaptateur (côté server, comme `MmapOwner`) clone ce handle dans un
  owner passé à `Bytes::from_owner` : le stockage vit jusqu'au drop de la frame.
- **P5** — production **avant** la construction du `Body` ; les octets sont un
  instantané ; aucune lecture après libération de l'owner.
- **P6** — capacité dérivée de la Forge (`VolatileSlot.capacity` ; le total
  `RouteDescriptor.volatile_capacity` devient une somme dérivée/vérifiée), jamais
  choisie par le runtime.
- **P7** — sélection : nouvelle forme explicite « sans sélection »
  (`SegmentSelection::NotApplicable`, nom à confirmer) ; invariant
  `VolatileSlot ⇔ NotApplicable`, vérifié par le générateur (erreur de build) et
  par le runtime (500, jamais de panic).
- **P8** — identité du producteur : `SourceSpec::VolatileSlot` reçoit une clé de
  producteur opaque (catalogue de build, même règle de numérotation que
  `SourceKey`, nom à confirmer). Le runtime ne connaît que cette clé.

### Réalisations comparées (coût / garanties) — à confirmer au démarrage de V1

| | Stockage | Allocations / requête | Copie après production | Send/'static | Remarques |
|---|---|---|---|---|---|
| **R-A** (référence proposée) | `String::with_capacity(cap)` (buffer natif du code généré) dans un newtype `Arc`-é, type `std` | buffer (≤ cap) + `Arc` + owner de `Bytes::from_owner` | aucune | oui, sans unsafe | `Arc::from(Vec<u8>)` **copierait** : utiliser `Arc::new(newtype)` |
| R-B | `Bytes::from(String)` directement côté server | 1 | aucune | oui | contourne `MaterializedSource`/`ResolvedRange` : ne respecte pas « `ResolvedRange[]` = dernier niveau Marius » (SPEC §2) |
| R-C | `RequestArena` possédée par requête | buffer + `Arc` | **copie** String→arène (le code généré écrit dans `String`) | oui | sauf à changer le codegen |
| R-D | arène poolée + garde RAII au drop de l'owner | ~0 en régime | aucune | oui | **hors périmètre** (pool) ; risque UAF si la garde est mal conçue |
| R-E | arène par worker, reset à l'acquisition | 0 | — | — | **non sûre** avec un corps streamé (fait établi) |

Coût documenté de R-A : quelques petites allocations par requête, bornées par
`VolatileSlot.capacity` (jamais par un payload arbitraire), conforme à
l'esprit de SPEC §4 (coût borné, hors contrat zero-alloc de la famille
segmentée).

---

## 5. Le vertical slice

```text
GET /__experimental/t2a/content/{id}     (route non publique, comme aujourd'hui)

segment 0  StaticArtifact(head)   selection = RequestSlot(0)  ← document_id
segment 1  VolatileSlot(nav_profile)  selection = NotApplicable
segment 2  StaticArtifact(tail)   selection = RequestSlot(0)  ← document_id
```

- Le `<li>` volatile est le **segment entier** (pas une interpolation dans le
  segment AOT) : squelette AOT (classes, `{% asset %}` résolu au build) +
  `username` échappé.
- **Marqueurs dans `navigation.marius`** (proposition, à valider) : une paire
  `<!-- MARIUS_VOLATILE_BEGIN nav_profile -->` … `<!-- MARIUS_VOLATILE_END -->`
  encadrant le `<li>` volatile, juste avant `<li>Repository</li>` (profondeur
  `if` = 0). Commentaires HTML textuels, mécanisme de `SCRIPTS_PLACEHOLDER`,
  **aucun nouveau token ni nouvelle syntaxe**.
- Build : après `lower` (et avant la mesure principale), extraire la plage de
  tokens entre les marqueurs (fragment volatile) ; le flux restant est coupé en
  `prefix_tokens` / `suffix_tokens`. Vérifier que le point de coupe est à
  profondeur `if` nulle (sinon erreur de build). Mesurer et générer **chaque
  partie séparément** (`resolve_and_measure` + `generate_segmented_snippet`) ;
  `ModulesPlaceholder` et le `modules_snippet` restent dans la partie qui porte
  le marqueur (le préfixe) ; les faits statiques pour le lowering des modules
  sont calculés sur le flux complet. Le fragment volatile est mesuré avec
  `SchemaIndex { varlena: [username] }` (capacité = statique + 192) et généré
  en fonction dédiée (wrapper avec `buf: String`, `varlena` local).
- **Deux artefacts** `content_core_head` et `content_core_tail`, même composant
  `content.core`, mêmes clés de sélection `document_id`. (Noms à confirmer.)
- Le **chemin monolithique** `/content/{id}` (artefact `content_core` complet)
  reste inchangé et sans username : la région volatile est **retirée** du flux
  complet. (Décision ouverte : conserver l'artefact complet ou non — voir §10.)
- **Source du username (mécanisme retenu pour le slice) :** lecture SQLx à la
  requête via le `PgPool` existant (`main()` le crée et le déplace dans le
  `Dispatcher` : à cloner). Justification factuelle : aucun état maintenu pour
  `account_core` (pas de composant, pas de NOTIFY, pas de store) ; le
  pipeline `Projection` exige composant + trigger + shard et régénère un pack
  inutile ; ADR-011 §11 cite explicitement « buffer PostgreSQL live » comme
  source volatile ; c'est le plus petit producteur réel. Conditionné à la
  résolution du rôle DB (§10). L'accès aux données est **injecté** (closure /
  producteur enregistré par clé), comme `resolve_generation` reçoit `fetch`,
  pour tester sans PostgreSQL et pour permettre un futur producteur maintenu.
- `await` : le handler devient asynchrone sur la production ; produire le
  volatile **avant** la résolution synchrone des plages statiques, pour ne
  garder aucune référence empruntée à travers un `await`.

---

## 6. Plan d'implémentation

### V1 — Contrat Volatile runtime, **sans Forge, sans SQL**

Miroir de I1→I6 : fixtures écrites à la main (provisoires), producteur
volatile **injecté** (double de test).

- `projection` : `SegmentSelection::NotApplicable` ; `SourceSpec::VolatileSlot`
  avec identité de producteur ; assertions/tests mis à jour.
- `render/emission.rs` : `MaterializedSource::Volatile` possédé (P1–P3) ;
  `ResolvedRange` volatile ; `resolve_range` volatile ; le dispatch producteur
  reste **hors** de `emission.rs`.
- `server/experimental_t2a.rs` : owner d'adaptation volatile → `Bytes::from_owner`
  ; fixture K=3 `[static][volatile][static]` (ids statiques réels du pack
  `content_core`) ; producteur injecté ; capacité dépassée → 500.
- Note normative courte du contrat Volatile (P1–P8).
- Sortie : tests d'ownership, longueur effective, ordre, `Content-Length`
  exact, échappement, absence de copie (égalité de pointeur), drop de l'owner
  après consommation du corps, R4 (snapshot) avec producteur injecté.

### V2 — Forge : préfixe / volatile / suffixe

- `build/template` : marqueurs, extraction du fragment, coupe, mesure et
  codegen par partie ; vérification de profondeur ; erreurs de build explicites.
- `db-forge`/`projection`/`render` : production de **deux artefacts** depuis un
  même composant (variantes de rendu ou paramètre de « partie » ; **un seul
  ingest** puis N régénérations — ne pas doubler `ingest_and_swap`).
- Provisionnement : les artefacts sans `RouteEntry` monolithique doivent être
  ouverts (topologie dérivée de `ARTIFACTS`, ou équivalent — décision à
  prendre avec l'utilisateur).
- `publication.toml` / `RouteSpec` : plusieurs artefacts pour un composant ;
  route à segments ordonnés ; génération du `RouteDescriptor` K=3.
- Test mécanique isolé : `head ++ tail` = page complète sans région volatile
  (égalité d'octets, par id).

### V3 — Producteur réel + scénario vertical

- Producteur SQL `ORDER BY entity_id ASC LIMIT 1` ; rôle / RLS résolus (§10) ;
  échappement via la fonction générée.
- Scénario d'acceptation complet (§7) sur PostgreSQL ; variante sans PG avec
  producteur injecté conservée pour la CI.

---

## 7. Tests d'acceptation finaux

```text
Initial : content G1, username = Alpha
R1 : G1 + Alpha

UPDATE username → Bob            (aucun événement)
R2 : G1 + Bob

UPDATE content.core → G2         (régénération content_core / head / tail)
R3 : G2 + Bob

R4 commence avec Bob ; le username passe à Carol pendant que le Body de R4
est encore en cours de consommation → R4 reste Bob
```

- **R4 réel :** le volatile est matérialisé **avant** l'envoi des en-têtes ;
  `send()` (reqwest) rend la main sur les en-têtes ; muter ensuite ; lire le
  corps → `Bob`. Ne pas simuler de concurrence impossible. Vérifier aussi
  qu'une rotation de génération statique pendant R4 laisse le statique de R4
  inchangé (patron de I4).
- Assertions : `K = 3` (`segments.len()`) ; ordre préfixe → volatile →
  suffixe ; `Content-Length` = somme exacte ; longueur effective ≠ capacité ;
  capacité insuffisante → 500 contrôlé ; échappement (`<b>&"'` → entités,
  jamais > 192 octets) ; aucun payload volatile copié (égalité de pointeur
  buffer ↔ `Bytes`) ; **aucune régénération de `content_core`** sur mutation du
  username (`Arc::ptr_eq` de la génération avant/après, et absence de NOTIFY) ;
  générations statiques cohérentes ; aucune lecture après libération de
  l'owner (compteur de drop observable : non libéré tant que le corps n'est pas
  consommé/abandonné).
- Non-régression : route monolithique `/content/{id}` et fixtures existantes.

---

## 8. Fichiers à lire

Déjà fournis lors des sessions précédentes (redemander si absents du disque) :
`ADR-011-projections-ordonnancees.md`, `SPECIFICATION-transport-segmente-t2a.md`
(v2), `DESIGN-runtime-segment-pipeline (post-ADR-011).md` (§2–§3, §11–§13
seulement ; §4/§7/§9 sont en retard sur la SPEC v2), `runtime-lifecycle-guide.md`
(§1.2/§3.1/§10 périmés sur la source de `regenerate` : c'est le `store.bin` via
`fetch_batch`), `fragment-forge-guide.md`, `handoff-t2a-experimental-…-i1-i6.md`,
`handoff-forge-t2a-production-integration.md`.

Code : `crates/core/projection/src/lib.rs`, `publication.rs`,
`store_registry.rs` ; `crates/shell/render/src/emission.rs`, `registry.rs`,
`lib.rs`, `route_derive.rs`, `dispatcher.rs`, `regenerate.rs`,
`ingest_and_swap.rs`, `batch_renderer.rs`, `pack_html_index.rs`,
`bin/dump.rs` ; `crates/shell/server/src/experimental_t2a.rs`, `main.rs`,
`handlers.rs` ; `crates/core/schema/build/main.rs`, `build/publication.rs`,
`build/template/{page,static_page,dynamic}.rs`, `src/lib.rs`,
`src/publication_tests.rs`, `publication.toml`, `templates/{base,navigation,head,footer}.marius`,
`templates/content/core.marius` ; `crates/forge/fragment-forge/src/{lib.rs,
schema.rs,fragment/{token,resolver,codegen,mod}.rs}` ;
`crates/forge/db-forge/src/{registry,naming}.rs`, `codegen/projection.rs` ;
SQL : `db/02_identity/{01_components,02_systems}.sql`,
`db/08_dcl/01_grants.sql`, `db/09_rls/01_policies.sql`,
`db/10_meta_seed/01_manifest.sql`, `db/12_events/01_notify.sql`,
`db/dml/master_schema_dml.pgsql`.

**À demander à l'utilisateur :**
Aucun fichier de lecture n'est plus en attente pour V1/V2. Ne jamais
redemander un fichier sous un nom déjà utilisé (`registry.rs` existe côté
`render` ET côté `db-forge` : exiger un nom distinct, ex. `render_registry.rs` /
`dbforge_registry.rs`) ;
`crates/core/schema/build/{modules_lowering,capabilities}.rs` si le lowering
des modules est touché ; les `Cargo.toml` concernés avant toute dépendance.

**Déjà lus (à redemander seulement s'ils ont disparu du disque) :**
`build/template/common.rs`, `db-forge/src/introspect.rs`, `db-forge/src/naming.rs`,
`db-forge/src/registry.rs` (côté db-forge), `render/src/registry.rs` (fourni sous le
nom `render_registry.rs`), `render/src/store_provisioning.rs`, `codegen/projection.rs`,
`store_registry.rs`.

## 9. Fichiers probablement modifiés

- V1 : `projection/src/lib.rs`, `render/src/emission.rs`, `render/src/lib.rs`
  (réexports), `server/src/experimental_t2a.rs`, `server/src/main.rs` (tests),
  note normative.
- V2 : `build/main.rs`, `build/publication.rs`, `build/template/page.rs` (+
  éventuellement `common.rs`), `publication.toml`, `templates/navigation.marius`,
  `db-forge/src/codegen/projection.rs`, `projection/src/lib.rs` (trait
  `Projection`/parties), `render/src/{dispatcher,regenerate,registry}.rs`,
  `server/src/main.rs` (`SHARDS`, topologie), `publication_tests.rs`.
- V3 : `server/src/{main,experimental_t2a}.rs`, éventuellement un script SQL de
  droits (fonction `SECURITY DEFINER` ou rôle) — **uniquement après accord**.

---

## 10. Questions à poser à l'utilisateur au démarrage

1. **Rôle de `DATABASE_URL` au runtime** (`marius_user`, `marius_admin`,
   `postgres`, propriétaire ?) et donc le comportement attendu face au RLS de
   `identity.account_core`. Si le rôle est soumis au RLS : accord pour une
   fonction `SECURITY DEFINER` / vue publique dédiée, ou pour un autre chemin ?
2. **Emplacement de la requête SQL** du producteur : écrite dans le server, ou
   déclarée dans `publication.toml` (table, champ, mode `first_by_primary_key`)
   et validée/générée par le build — sachant que la seconde option évite de
   recréer la duplication composant/PK que la session précédente a supprimée.
3. **Artefact complet `content_core`** : conservé (route monolithique inchangée)
   ou remplacé ? (Défaut supposé : conservé.)
4. **Noms** : `content_core_head` / `content_core_tail`, marqueurs
   `MARIUS_VOLATILE_BEGIN/END`, `SegmentSelection::NotApplicable`, clé de
   producteur.
5. **Traitement du dépassement de capacité** : 500 contrôlé (défaut) ou
   troncature.

## 11. Périmètre strict

Dans le périmètre : contrat Volatile (P1–P8), K=3 pour ce cas, découpage Forge
préfixe/suffixe, producteur SQL minimal, scénario R1–R4.

Hors périmètre : authentification, session, cookies, navigateur, `data-*`,
HTMX, synchronisation client, WebSocket/SSE, nouvelle stratégie générale de
notification, optimisation transport, `K>3`, Volatile générique multi-producteurs,
pool ou zero-allocation, migration générale des chemins d'artefacts, correction
globale de la dette documentaire D1/D2, `EmissionPlan` / `IoSlice` /
`writev` / `MSG_ZEROCOPY` / `backend_kind` (comme concept) / `K_AOT` /
`IOV_MAX`.

## 12. Méthode de vérification utilisée jusqu'ici (à reproduire si possible)

Le sandbox n'a ni le dépôt complet ni PostgreSQL. Méthode qui a fonctionné :
`apt-get install rustc-1.89 cargo-1.89 rustfmt-1.89` ; espace de travail
scratch reprenant les fichiers **réels** fournis (`emission.rs`,
`handlers.rs`, `experimental_t2a.rs`, `build/publication.rs`, `lib.rs` de
projection…) avec des mocks minimaux pour ce qui manque (`PackHtmlIndex`,
`LiveRegistry`, `store_registry.rs`) ; extraction verbatim du test de
`main.rs` dans le scratch. `rustfmt --edition 2024 --check` sur les fichiers
livrés. `clippy` n'y est pas disponible (rust-clippy Ubuntu = 1.75) : l'utilisateur
lance build/test/clippy chez lui. Dire explicitement ce qui n'a pas pu être
exécuté.

## 13. Risques résiduels

- Rôle DB / RLS inconnus (bloquant pour V3, pas pour V1/V2).
- Latence SQL sur le chemin chaud (accepté pour ce slice, ADR-011 §11 le prévoit).
- Doublement de la production de packs (head + tail, + complet) ; disque et
  temps de régénération.
- « Premier utilisateur » peut devenir `user_5` après anonymisation ; comptes
  `is_visible = false` non filtrés ; le username figure en clair dans
  `dml_audit_log`.
- `fetch_varlena_cols` sur `identity.account_core` émet un `cargo:warning`
  (`time_zone` TEXT non borné) à chaque build ; à filtrer ou tolérer.
- Changement de `SegmentSelection` / `SourceSpec` : tous les `match` exhaustifs
  et les tests de `projection`, `emission.rs`, `experimental_t2a.rs` sont à
  mettre à jour.
- `RESOLUTION_CAPACITY = 4` (prototype) et `SourceResolutionContext<N>` :
  contraintes d'implémentation, pas des invariants Forge.
- Réponse contenant un volatile : non cacheable comme un pack statique (hors
  périmètre, à ne pas oublier plus tard).
