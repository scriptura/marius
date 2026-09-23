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
