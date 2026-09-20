# Handoff — T2A experimental integration I1–I6

**Statut :** Clôture de session de construction/prototypage. Document de
transition, pas une spécification. Les décisions architecturales normatives
restent `ADR-011-projections-ordonnancees.md` et
`SPECIFICATION-transport-segmente-t2a.md` (v2) — ce handoff ne les modifie
pas et ne les remplace pas.

**Portée :** compte-rendu factuel du prototype `experimental_t2a.rs`
(`crates/shell/server/`), construit en six incréments (I1→I6) au cours
d'une session unique. Objectif de la session : démontrer réellement, dans
le chemin HTTP, que la frontière transport T2A (`ResolvedRange[] → Bytes →
Body → Hyper`) fonctionne de bout en bout — pas préparer une intégration
Forge définitive.

---

## 1. Chemin effectivement démontré

```text
RouteDescriptor (fixture statique, PROVISOIRE, écrite à la main)
    │
    ▼
source_spec_for(route, SourceId) → SourceSpec::StaticArtifact
    │
    ▼
resolve_generation(spec, fetch)              (emission.rs, marius-render — inchangé)
    │  fetch = stub local : ignore la SourceKey reçue, capture
    │  "content_core" en dur (aucun catalogue générique introduit)
    ▼
MaterializedSource::Mmap { handle: Arc<PackHtmlIndex> }
    │
    ▼
resolve_range(&source, id)                   (emission.rs — inchangé)
    │
    ▼
ResolvedRange<'_>                            (vérifié, jamais consommé au-delà de ptr()/len())
    │
    ▼  ── frontière marius-render / marius-server ──
    │
MmapOwner { Arc::clone(handle), offset, len }     (type local à experimental_t2a.rs)
    │
    ▼
Bytes::from_owner(owner)                     (1 à K fois, ordre des segments préservé)
    │
    ▼
FrameStream (impl manuelle de futures_core::Stream)
    │
    ▼
Body::from_stream                            (axum_core::body::Body)
    │
    ▼
Response { Content-Length: somme exacte, Content-Type }
    │
    ▼
Hyper (boucle d'acceptation Phase 5, main.rs — non modifiée)
    │
    ▼
HTTP
```

Deux routes expérimentales exercent ce chemin, toutes deux hors
`ROUTE_TABLE` :

| Route | K (segments) | Source réelle |
| --- | --- | --- |
| `GET /__experimental/t2a/single` | 1 | `content_core`, id=1 |
| `GET /__experimental/t2a` | 3 | `content_core`, ids 1/2/3 — un seul `SourceKey` partagé par les trois segments |

Le chemin **AOT monolithique** (`ROUTE_TABLE` → `serve_route` →
`deliver` → `read_at`/`spawn_blocking`, `handlers.rs`) n'a été modifié à
aucune étape et reste fonctionnel, y compris une fois les routes T2A
mergées dans le même `Router` de production (vérifié, I6).

---

## 2. Invariants réellement testés

Chacun correspond à un test exécuté et confirmé vert par l'utilisateur
dans l'environnement réel (`cargo build` / `cargo test` / `cargo clippy`
à chaque incrément) :

| # | Invariant | Incrément | Mécanisme de preuve |
| --- | --- | --- | --- |
| 1 | Un segment unique traverse réellement `ResolvedRange → Bytes → Body → Hyper → HTTP` et produit exactement le payload attendu | I1 | Round-trip HTTP réel, `Content-Length` + corps comparés octet à octet |
| 2 | Plusieurs segments (K=3), partageant un seul `SourceKey`, sont émis dans l'ordre déclaré | I3 | Round-trip HTTP, corps comparé à la concaténation exacte attendue |
| 3 | `Content-Length` égale exactement la somme des longueurs des segments émis | I3 | Assertion directe sur l'en-tête HTTP |
| 4 | Une requête ayant déjà résolu sa génération (`Arc<PackHtmlIndex>` cloné) continue de produire l'ancien contenu après une rotation `ArcSwap` survenue pendant la requête ; une requête ultérieure observe la nouvelle génération | I4 | `send()` (reqwest) synchronisé sur la réception des en-têtes (donc sur la fin réelle du handler, entièrement synchrone) ; rotation injectée entre `send()` et la lecture du corps |
| 5 | Aucune copie du payload mmap n'est introduite entre `ResolvedRange` et `MmapOwner` | I5 | Égalité de **pointeur** (`owner.as_ref().as_ptr() == range.ptr()`), pas seulement de contenu |
| 6 | Absence textuelle de `read_at`/`Vec<u8>` dans le code de production d'`experimental_t2a.rs` | I5 | Audit statique (`include_str!` du fichier, section de test exclue) |
| 7 | Le chemin monolithique reste fonctionnel une fois les routes T2A mergées dans le même `app` | I6 | Round-trip HTTP sur `/content/1` (ROUTE_TABLE réelle), sur le même `app` que les routes T2A |

---

## 3. Propriétés déduites ou non mesurées — à ne pas confondre avec le tableau ci-dessus

- Le comportement interne exact de `Bytes::from_owner` (allocation de
  bookkeeping propre à la crate `bytes`, jamais du payload) — déduit de la
  documentation de la crate `bytes` (version résolue : 1.11.1), jamais
  mesuré directement dans cette session.
- Tout ce que Hyper effectue en aval de `Body` (mise en file, buffers
  d'écriture, vectorisation `writev` éventuelle) — explicitement hors du
  contrat de performance Core Marius (SPEC v2 §4/§6), jamais instrumenté.
- Toute copie ou allocation interne à Tokio ou à l'OS — hors périmètre,
  non mesurée. Les affirmations « aucune copie » de ce document et du code
  portent strictement sur ce que Marius/l'adaptateur T2A introduisent,
  jamais sur ces couches.
- L'absence de `read_at`/`Vec<u8>` (invariant #6 ci-dessus) est un audit
  limité au seul fichier `experimental_t2a.rs` — elle ne prouve rien sur
  `emission.rs` (inchangé, non ré-audité dans cette session) ni sur les
  dépendances.
- Aucune mesure d'allocations séparée en trois couches (Marius/Core,
  Bytes/Body, Hyper) n'a été instrumentée — jugée disproportionnée pour ce
  prototype (aurait nécessité un allocateur global pour tout le binaire de
  test `marius-server`). La séparation reste qualitative dans ce document
  et dans le guide runtime, pas mesurée.

---

## 4. Fichiers modifiés cumulativement (I1→I6)

- **Nouveau** — `crates/shell/server/src/experimental_t2a.rs` : routes
  expérimentales K=1/K=3, `MmapOwner`, `FrameStream`, tests de non-copie
  (I5).
- **Modifié** — `crates/shell/server/src/main.rs` : déclaration du module
  (`mod experimental_t2a;`), clonage de `registry` pour alimenter le merge,
  un seul test `t2a_experimental_regression_suite` couvrant la régression
  du chemin monolithique (T6), K=1, K=3 et la rotation `ArcSwap` (I4).
- **Modifié** — `Cargo.toml` (racine) : ajout de `marius-projection` et
  `futures-core` à `workspace.dependencies` (versions déjà résolues dans
  `Cargo.lock` avant tout ajout, jamais de nouvelle résolution).
- **Modifié** — `crates/shell/server/Cargo.toml` : raccordement de ces deux
  dépendances.
- **Jamais modifiés** — `crates/shell/render/src/emission.rs` et le reste
  du crate `marius-render` ; `crates/shell/server/src/handlers.rs` ;
  `ROUTE_TABLE`/`RouteEntry`/`IdSource` ; tout fichier `.marius` ou
  pipeline Forge.

---

## 5. Statut de `experimental_t2a.rs`

**PROVISOIRE.** Aucune route de production réelle. Non intégré à
`ROUTE_TABLE`. Le `RouteDescriptor` qu'il expose est écrit à la main, pas
généré par la Forge. Le stub de résolution (`fetch` capturant
`"content_core"` en dur) n'est pas un catalogue `SourceKey → packfile_key`
et ne doit jamais être lu comme une préfiguration d'un tel catalogue.

Les routes `/__experimental/t2a` et `/__experimental/t2a/single` ne
doivent pas être exposées publiquement sans décision séparée — leur seul
rôle est d'avoir permis de démontrer, sur une route HTTP réelle, que la
frontière transport T2A fonctionne.

---

## 6. Écarts restants par rapport à la SPEC T2A finale

- Aucune intégration Forge — le catalogue `SourceKey → packfile_key`,
  `RouteDescriptor`/`SegmentDescriptor` générés automatiquement, restent
  entièrement à faire (explicitement hors périmètre de cette session dès
  le départ).
- `IdSource::PathParam → SegmentSelection` n'a jamais été raccordé — seul
  `SegmentSelection::Constant` a été exercé.
- `K_AOT` (budget de segments par route) n'est ni défini ni calculé — les
  routes expérimentales ont un K fixe, écrit à la main.
- `EmissionBackendKind::Scatter` est posé par défaut sur les deux routes
  expérimentales ; `is_single_file_compatible` n'a jamais été appelé, donc
  cette valeur n'est jamais réellement discriminée par du code.
- Aucune mesure d'allocations en trois couches (§3 ci-dessus).
- Le Volatile, `RequestArena`, HTTP/2, `MSG_ZEROCOPY`, `io_uring` restent
  entièrement non traités — hors périmètre dès l'origine de la SPEC v2 et
  d'ADR-011.

---

## 7. Sujets explicitement laissés au futur chantier d'intégration Forge/runtime

- Décider comment la Forge génère un `RouteDescriptor` réel par route
  (aujourd'hui : aucune trace dans `generated_schema.rs`).
- Décider du mécanisme définitif de raccordement `IdSource →
  SegmentSelection`, au-delà du seul cas `Constant` exercé ici.
- Définir et calculer `K_AOT` (portée par route vs globale, mode de calcul
  par la Forge, stockage runtime).
- Décider, route par route, du choix Forge entre chemin monolithique et
  chemin segmenté — cette session n'a exercé que des routes créées
  manuellement à cette fin, jamais une décision de production.
- Étendre `MmapOwner`/le mécanisme d'adaptation `Bytes`/`Body` au-delà du
  module expérimental, si et seulement si une route de production réelle
  en a besoin — pas de généralisation préventive.
- Toute mesure de performance ou d'allocation formelle, en trois couches
  distinctes, si le besoin en est démontré.

---

## 8. Documents liés

- `ADR-011-projections-ordonnancees.md` — décision architecturale normative
  (Projection/Artefact/Segment/Réponse HTTP).
- `SPECIFICATION-transport-segmente-t2a.md` (v2) — contrat normatif de la
  frontière transport T2A.
- `DESIGN-runtime-segment-pipeline (post-ADR-011).md` — détail des
  primitives `SourceKey`/`SegmentDescriptor`/`MaterializedSource`/
  `ResolvedRange`.
- `runtime-lifecycle-guide.md` §11 — situe ce prototype par rapport au
  cycle de vie runtime existant (AOT monolithique, ArcSwap, artefacts).
- `fragment-forge-guide.md` §2.3bis/§4.8/§4.8ter — contextualisation de
  route résolue entièrement en amont (`if`/`else`, `==`/`!=`,
  `eliminate_recordless_conditions`), jamais une logique du runtime T2A.

---

_Session de construction et de clôture du prototype T2A (I1→I6). Rédigé le
19 septembre 2026 à l'issue de la validation réelle (build/tests/clippy)
de I6 par l'utilisateur._
