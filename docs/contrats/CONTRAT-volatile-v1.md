# Note normative — Contrat Volatile V1

**Statut :** normative pour le pipeline runtime T2A des segments volatils. Ne
modifie ni ADR-011 ni `SPECIFICATION-transport-segmente-t2a.md` (v2) —
**étend** ce que la SPEC v2 §8 laisse explicitement hors périmètre (« Le
Volatile (production, cycle de vie, `VolatileSlot`) »), sans les contredire.
Toute contradiction découverte avec l'un de ces deux documents doit faire
l'objet d'un audit séparé, pas d'une résolution silencieuse via cette note
(même clause que SPEC v2 §10).

**Source :** `handoff-volatile-vertical-slice.md` §6. Cette note n'introduit
aucune propriété nouvelle — elle fixe par écrit, comme référence normative
indépendante du handoff (document de transition, pas une spécification), les
huit propriétés déjà actées.

---

## Propriétés (P1–P8)

- **P1** — Aucun raw pointer dans `MaterializedSource::Volatile`.
- **P2** — La longueur effective est portée par le stockage ; `ResolvedRange`
  = (ptr, longueur effective). `longueur > capacité` au moment de la
  matérialisation :
  ```
  → erreur contrôlée
  → HTTP 500 dans l'adaptateur
  → aucune troncature
  ```
  Jamais une lecture ou une écriture hors bornes.
- **P3** — `MaterializedSource::Volatile` porte un handle **possédé et
  partageable**. `marius-render` n'a aucune dépendance `bytes`/`axum`/`hyper` :
  types `std` uniquement.
- **P4** — L'adaptateur côté server (ex. `MmapOwner`) clone ce handle dans un
  owner passé à `Bytes::from_owner` : le stockage vit jusqu'au drop de la
  frame.
- **P5** — Production **avant** la construction du `Body` ; les octets sont
  un instantané ; aucune lecture après libération de l'owner. Le point
  d'appel du producteur est dans le handler asynchrone, **avant** la
  résolution synchrone des plages statiques, afin qu'aucune référence
  empruntée ne traverse un `await` (pertinent dès V3 : lecture SQL
  asynchrone).
- **P6** — Capacité dérivée de la Forge (`SourceSpec::VolatileSlot.capacity`) ;
  le total `RouteDescriptor.volatile_capacity` est une somme dérivée et
  vérifiée, jamais choisie par le runtime.
- **P7** — `SegmentSelection::NotApplicable` : invariant
  `SourceSpec::VolatileSlot ⇔ SegmentSelection::NotApplicable`, vérifié par
  le générateur (erreur de build, V2) et par le runtime (500, jamais de
  panic, V1c). V1a fournit uniquement le prédicat pur commun aux deux
  futurs appelants (`marius_projection::segment_matches_source`) — ni l'un
  ni l'autre mécanisme de vérification n'est câblé à ce stade.
- **P8** — `SourceSpec::VolatileSlot` porte une `ProducerKey` opaque ; le
  runtime ne connaît que cette clé. Le dispatch vers l'implémentation du
  producteur vit **hors** de `emission.rs`.

## État couvert par V1a (ce livrable)

Types et invariants purs, dans `marius-projection` uniquement :

- `SegmentSelection::NotApplicable` (nouvelle variante).
- `ProducerKey` (identité opaque, même convention que `SourceKey`).
- `SourceSpec::VolatileSlot { capacity: u32, producer: ProducerKey }` (P8).
- `segment_matches_source(&SegmentDescriptor, &SourceSpec) -> bool` : prédicat
  de cohérence P7, couvrant à la fois `NotApplicable ⇔ VolatileSlot` et
  `SegmentFlags::VOLATILE ⇔ VolatileSlot`.

**Explicitement non traité par V1a** (P4, P5 restent des propriétés à
satisfaire par le code de V1c ; P1, P3 sont couverts depuis **V1b**, voir
section suivante) :

- `MaterializedSource::Volatile` reste, dans l'état actuel du dépôt fourni,
  `{ arena_ptr: *const u8 }` — raw pointer, `!Send`, sans longueur. Sa mise en
  conformité avec P1/P2/P3 est le travail de **V1b**.
- `resolve_generation`/`resolve_range` continuent de renvoyer `None` pour
  `Volatile` — inchangé par V1a.
- Aucun producteur, réel ou expérimental, n'existe encore.
- L'adaptateur `Bytes::from_owner`/le handler HTTP (P4/P5) sont V1c.

## État couvert par V1b

Contrat mémoire/runtime, dans `marius-render::emission` (`marius-projection`
inchangé depuis V1a) :

- `MaterializedSource::Volatile` porte désormais `storage: Arc<VolatileStorage>`
  — plus de raw pointer (P1), handle possédé et partageable via `Arc::clone`
  (P3). Toujours zéro dépendance `bytes`/`axum`/`hyper` dans `marius-render`
  (vérifié : `Cargo.toml` du crate inchangé).
- `VolatileStorage` — stockage possédé, `capacity` (borne AOT) distincte
  d'`effective_len()` (= longueur réellement produite). `from_produced`
  vérifie `effective_len <= capacity` une seule fois, à la construction ;
  dépassement → `VolatileCapacityExceeded` (erreur contrôlée, P2 —
  traduction en HTTP 500 toujours différée à l'adaptateur, V1c).
- `resolve_volatile_generation` / `resolve_volatile_range` — pendants de
  `resolve_generation`/`resolve_range` pour le chemin volatile, délibérément
  **distincts** (pas un paramètre supplémentaire sur les fonctions
  existantes) : un Volatile est *produit*, jamais *récupéré* par sélection
  (contrainte V1b §5 — ne pas détourner le mécanisme `StaticArtifact →
  lookup(selection)`). `resolve_generation`/`resolve_range` restent inchangés
  pour `StaticArtifact`.
- Producteur toujours **injecté** (closure `FnOnce(ProducerKey) -> Vec<u8>`)
  — aucun catalogue réel, aucun SQL, aucune Forge (V3).

**Allocations et copies observées** (cf. commentaire de section
`VolatileStorage` dans `emission.rs`) :

- Une allocation par production est acceptable et effectivement présente :
  celle du `Vec<u8>` que le producteur construit lui-même (hors du contrôle
  de ce module).
- `VolatileStorage::from_produced` ne recopie jamais ce contenu dans un
  second buffer dimensionné à `capacity` — il prend possession du `Vec<u8>`
  produit (`into_boxed_slice()`). Réserve honnête : si le `Vec` du
  producteur a une capacité excédentaire au moment de l'appel,
  `into_boxed_slice()` peut réallouer en interne (`shrink_to_fit`) — détail
  de la bibliothèque standard, pas une copie introduite par ce module ; un
  producteur qui alloue exactement `payload.len()` (ex. `String::with_capacity`
  puis aucun `push` supplémentaire) rend ce cas inexistant en pratique.
- `resolve_volatile_range` ne copie jamais : `ResolvedRange` emprunte
  directement le buffer de `VolatileStorage` — vérifié par égalité de
  pointeur (test E, même méthode que l'invariant I5 du T2A démontré en
  session précédente).

**Toujours non traité** (P4, P5, V1c) : aucun adaptateur HTTP, aucun
`Bytes::from_owner`, aucun point d'appel dans un handler asynchrone.

## État couvert par V1c (clôture de V1)

Frontière HTTP réelle, dans `crates/shell/server/src/experimental_volatile_t2a.rs`
(nouveau module, **test-only** — voir réserve ci-dessous) + un ajout mécanique
oublié en V1b dans la façade `crates/shell/render/src/lib.rs` :

- `VolatileOwner { storage: Arc<VolatileStorage> }` — pendant de `MmapOwner`
  (`experimental_t2a.rs`, inchangé) pour le chemin volatile. `Bytes::from_owner`
  conserve ce owner vivant jusqu'au drop de tous les `Bytes`/`Body` qui en
  dérivent (P4) — garantie de la crate `bytes` elle-même, pas un mécanisme
  ajouté par ce module.
- Fixture K=3 réelle : `StaticArtifact(prefix) → VolatileSlot → StaticArtifact(suffix)`,
  ordre d'émission = ordre de déclaration (volatile jamais déplacé en fin de
  réponse).
- Producteur toujours **injecté** (`std::sync::RwLock<Vec<u8>>` mutable par
  les tests) — aucun catalogue réel, aucun SQL, aucune Forge (V3, inchangé).
- Chemin de résolution entièrement synchrone (aucun `.await` dans
  `resolve_volatile_route_to_response`) — P5 (ordre producteur → statiques,
  aucun emprunt en vol à travers un point de suspension) est donc
  **trivialement** satisfaite ici : rien ne suspend. La discipline
  d'ordonnancement réelle (emprunt à travers une I/O `.await` véritable)
  reste à démontrer en V3, quand le producteur deviendra une lecture SQL
  asynchrone.

**Réserve explicite — non monté en production (`main()`)** : contrairement à
`experimental_t2a::mount_experimental` (mergé dans `main()`, réutilisant un
packfile déjà provisionné par `ROUTE_TABLE`), ce module est déclaré
`#[cfg(test)]` de bout en bout. Le monter en production exigerait de
provisionner/cold-start un nouvel artefact au démarrage — explicitement exclu
du périmètre V1c (« production réelle »). La démonstration reste réelle (vrai
`TcpListener`, vrai `reqwest::Client`, vrai `Bytes::from_owner`/`Body`/Hyper)
mais uniquement au travers de `cargo test`. Décision prise et signalée dans
cette note, pas arbitrée silencieusement — à confirmer ou infirmer par
l'utilisateur avant V2.

**Garanties réellement démontrées par les tests** (voir rapport de session
pour le détail par test) :
- round-trip HTTP K=3 mixte, octets exacts, `Content-Length` = somme des
  longueurs réelles (jamais la capacité) ;
- snapshot : une requête en vol garde sa propre matérialisation statique ET
  volatile après une rotation `ArcSwap` et un changement de producteur
  survenus pendant qu'elle est en vol ;
- dépassement de capacité → 500 contrôlé, jamais de panic, jamais de
  troncature ;
- absence de copie entre `VolatileStorage::as_slice()` et ce que
  `VolatileOwner` remet à `Bytes::from_owner` (égalité de pointeur — ne
  couvre pas le pointeur interne du `Bytes` construit, non garanti par la
  crate `bytes`) ;
- non-régression : `experimental_t2a.rs`/`t2a_experimental_regression_suite`
  non modifiés.

**Non démontré, explicitement reporté à V2/V3** : catalogue `ProducerKey →
implémentation` réel, producteur asynchrone (SQL), Forge/`publication.toml`,
route volatile en production, mesure d'allocations en couches séparées.
