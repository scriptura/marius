# Handoff — Forge → T2A Production Integration

**Statut :** document de transition vers une nouvelle session Claude, pas une
spécification. Ne rouvre pas ADR-011. Écrit à l'issue de la clôture du
prototype `experimental_t2a.rs` (I1→I6) — voir
`handoff-t2a-experimental-integration-i1-i6.md` pour le détail complet de
cette session antérieure ; ce document-ci n'en reprend que ce qui est
directement utile au chantier suivant.

---

## 1. Point de départ désormais acquis

Ce qui suit est démontré par des tests réels, exécutés et validés par
l'utilisateur dans le dépôt réel (`cargo build`/`test`/`clippy`), pas
seulement raisonné :

- La frontière `ResolvedRange[] → Bytes → Body → Hyper` fonctionne
  réellement, de bout en bout, sur au moins une route HTTP réelle.
- Une génération (`Arc<PackHtmlIndex>`) déjà résolue par une requête reste
  valide après une rotation `ArcSwap` survenue pendant cette requête ; une
  requête ultérieure observe la nouvelle génération.
- K=1 et K=3 segments fonctionnent, y compris quand K=3 segments
  partagent un seul `SourceKey` (jamais 3 sources distinctes).
- L'ordre des segments est strictement préservé de la résolution à
  l'émission.
- `Content-Length` est calculé exactement (somme des longueurs des
  segments déjà résolus) avant construction du `Body`.
- Aucune copie du payload mmap n'est introduite entre `ResolvedRange` et
  l'adaptateur `Bytes` (vérifié par égalité de pointeur, pas seulement de
  contenu).
- Le chemin AOT monolithique (`ROUTE_TABLE`/`serve_route`/`deliver`)
  coexiste avec les routes T2A dans le même `Router` de production et y
  reste fonctionnel (régression fonctionnelle testée, pas de mesure de
  performance).

Ne redélibère pas ces points — ce sont des faits acquis, pas des
hypothèses à revalider.

---

## 2. Problème exact du prochain chantier

```text
Forge réelle (compilation, build.rs / fragment-forge)
   ↓
   ?                                    ← rien n'existe ici aujourd'hui
   ↓
RouteDescriptor / SegmentDescriptor[]   (crates/core/projection/src/lib.rs)
   ↓
SourceKey réel                          (catalogue à définir — aujourd'hui : stub à une seule clé, en dur)
   ↓
T2A runtime déjà démontré               (resolve_generation/resolve_range/Bytes/Body — I1→I6)
```

Le maillon manquant (`?`) est entier : rien, dans le dépôt tel qu'il est
aujourd'hui, ne produit de `RouteDescriptor`/`SegmentDescriptor[]` à partir
d'une compilation Forge réelle. Points expérimentaux du prototype I1→I6
qui doivent être remplacés, un par un, par quelque chose de réel :

- **Génération réelle des `RouteDescriptor`/`SegmentDescriptor[]`** —
  aujourd'hui : `static` écrits à la main dans `experimental_t2a.rs`, deux
  routes fixes (K=1, K=3), aucun rapport avec la compilation Forge.
- **Raccordement `IdSource → SegmentSelection`** — jamais exercé au-delà
  du cas `SegmentSelection::Constant`. `IdSource::PathParam` (utilisé par
  le chemin monolithique réel, `/content/{id}`) n'a jamais été relié à une
  résolution de segment.
- **Catalogue réel `SourceKey`** — aujourd'hui : une fermeture qui ignore
  la `SourceKey` reçue et capture `"content_core"` en dur. Aucun mapping
  `SourceKey → packfile_key` n'existe nulle part dans le dépôt.
- **`K_AOT`** (budget de segments par route) — non défini, non calculé.
  Le prototype a un K fixe, écrit à la main, par route expérimentale.
- **Choix Forge monolithique vs segmenté** — aucune décision, nulle part,
  sur quelles routes de production devraient un jour utiliser T2A plutôt
  que le chemin monolithique existant. Le prototype n'a jamais eu à
  trancher cette question : ses deux routes sont hors `ROUTE_TABLE`.

Ne décide pas encore comment résoudre ces cinq points — cartographie-les,
avec leurs contraintes réelles (types, signatures, crates), avant de
proposer quoi que ce soit.

---

## 3. Frontières qui ne doivent pas bouger

- La Forge résout la contextualisation AOT (navigation, breadcrumb,
  `if`/`else`, `==`/`!=`, recordless) — entièrement à la compilation.
- La Forge décide et produit les représentations physiques (artefacts,
  packfiles) — le runtime ne fait qu'ordonnancer des plages mémoire déjà
  produites.
- T2A ne réinterprète jamais `.marius`.
- Aucune logique `route.*`, `record.*`, `IfEq`, `IfNeq`, `Else` dans le
  runtime — ni aujourd'hui, ni après l'intégration Forge.
- `marius-render` reste transport-agnostique — vérifié par son
  `Cargo.toml` (aucune dépendance `axum`/`hyper`/`bytes`), pas seulement
  énoncé comme discipline. Toute production réelle de `RouteDescriptor`
  doit respecter cette frontière de crate.
- Hyper reste propriétaire du transport (socket, framing, backpressure,
  vectorisation interne).
- Pas d'`EmissionPlan`, `IoSlice[]`, `writev`, `sendmsg` dans Marius — ni
  sous ces noms, ni sous un autre.

---

## 4. Cartographie réelle du dépôt utile au prochain chantier

### Indispensables

| Chemin | Contenu / pertinence |
| --- | --- |
| `crates/core/projection/src/lib.rs` | Définit `SourceKey`/`SourceId`/`SourceSpec`/`SegmentDescriptor`/`RouteDescriptor`/`EmissionBackendKind`/`SegmentBudget` — le vocabulaire cible que la Forge devra apprendre à produire. |
| `crates/shell/render/src/emission.rs` | `resolve_generation`/`resolve_range`/`MaterializedSource`/`ResolvedRange`/`SourceResolutionContext` — le runtime déjà démontré (I1→I6), jamais modifié, à ne pas toucher sans nécessité démontrée. |
| `crates/shell/render/src/registry.rs` | `LiveRegistry`/`RouteEntry`/`IdSource`/`ArcSwap`/`cold_start`/`store` — le **seul** mécanisme de routage produit aujourd'hui, et il est écrit à la main (`ROUTE_TABLE`), jamais par la Forge. Point de comparaison direct avec `RouteDescriptor`. |
| `crates/shell/server/src/main.rs` | `ROUTE_TABLE` écrit à la main (déclaration `static`), `build_router`, point d'entrée réel du binaire — montre où un futur `RouteDescriptor` réel devrait se brancher en production. |
| `crates/shell/server/src/experimental_t2a.rs` | Le prototype PROVISOIRE lui-même — preuve que la frontière fonctionne, jamais un modèle à copier tel quel (catalogue en dur, K fixe). |
| `runtime-lifecycle-guide.md` §11 | Situe la frontière T2A par rapport au cycle de vie runtime existant (AOT monolithique, `ArcSwap`, artefacts) — directement pertinent pour la frontière Forge→T2A que ce chantier doit combler. |
| `fragment-forge-guide.md` §2.3bis/§4.8/§4.8ter | Documente précisément ce que la Forge résout déjà en amont (`if`/`else`, `==`/`!=`, recordless) — condition de la frontière §3 de ce handoff (« T2A ne réinterprète jamais `.marius` »). |
| `ADR-011-projections-ordonnancees.md` | Décision normative (Projection/Artefact/Segment/Réponse HTTP), ontologie à quatre niveaux. |
| `SPECIFICATION-transport-segmente-t2a.md` (v2) | Contrat normatif de la frontière transport T2A. |
| `DESIGN-runtime-segment-pipeline (post-ADR-011).md` | **Jamais lu intégralement pendant I1→I6** (seulement cité en commentaire de code, ex. « DESIGN §3.2 »). Contient le détail normatif de `SourceKey`/génération/primitives que le nouveau chantier consomme directement. |
| `handoff-t2a-experimental-integration-i1-i6.md` | État acquis détaillé (chemin démontré, invariants testés, fichiers modifiés, écarts). |

### Probablement nécessaires

| Chemin | Contenu / pertinence |
| --- | --- |
| `crates/core/schema/build/main.rs` (ex-`build.rs`) | Orchestrateur Forge — point d'entrée où une génération de `RouteDescriptor`/`SegmentDescriptor[]` s'insérerait vraisemblablement. |
| `crates/core/schema/build/template/{page.rs, static_page.rs, dynamic.rs}` | Pipeline Voie B (`.marius` → `render()`) — à examiner pour voir s'il produit déjà une notion de route/id source exploitable, ou s'il faut l'étendre. |
| `crates/forge/fragment-forge/src/{page/*, fragment/*}` | Le compilateur `.marius` lui-même — pour comprendre où un pattern de route ou un `IdSource` pourraient être déduits d'un template compilé. |
| `crates/core/schema/src/lib.rs` | Façade du crate schema — pour voir ce qu'il expose déjà (ex. `ContentCoreProjection`, cité dans `main.rs`). |
| `crates/shell/render/src/packfile_builder.rs`, `pack_html_index.rs` | Pour comprendre comment un `packfile_key`/`SourceKey` pourrait être relié à un artefact physique réel côté Forge. |

### Seulement contextuels

| Chemin | Contenu / pertinence |
| --- | --- |
| `CONTRAT-marius-one-page-extension.md` | Contexte historique ; jamais directement invoqué pendant I1→I6 sauf comme dépendance listée dans la SPEC. |
| `Cartographie-de-transmission.md` | Cartographie d'une session antérieure à I1→I6, désormais partiellement obsolète (le code qu'elle décrit a changé). Utile seulement pour l'historique. |
| `tree.md` | Vue d'ensemble de navigation initiale — à régénérer si le dépôt a changé depuis. |

Ne pas tout charger d'emblée : les indispensables suffisent pour démarrer la cartographie (étape 2 de l'ordre de travail, §6) ; le reste se demande au fil du besoin, pas en bloc.

---

## 5. Documents à transmettre au nouveau Claude

**Indispensable**
- `ADR-011-projections-ordonnancees.md`
- `SPECIFICATION-transport-segmente-t2a.md` (v2)
- `handoff-t2a-experimental-integration-i1-i6.md`
- `DESIGN-runtime-segment-pipeline (post-ADR-011).md`
- `runtime-lifecycle-guide.md` (à jour, §11)
- `fragment-forge-guide.md` (à jour, §2.3bis/§4.8/§4.8ter)
- ce document

**Utile**
- `tree.md`
- Les fichiers de la section « Indispensables »/« Probablement nécessaires » du §4 — à fournir au moment où le nouveau Claude les demande explicitement, pas en bloc au démarrage (même discipline que cette session : ne pas deviner, demander).

**Inutile pour ce chantier précis**
- `CONTRAT-marius-one-page-extension.md` (sauf besoin ponctuel identifié en cours de route)
- `Cartographie-de-transmission.md`
- Les ADR sans rapport direct (ADR-001 à ADR-010, sauf rappel ponctuel de la doctrine de pré-composition d'ADR-008, déjà résumée dans le handoff I1→I6 §rien — voir plutôt ADR-011 §1)
- Tout document du dossier `graveyard/`

---

## 6. Ordre de travail recommandé

1. Lecture des documents indispensables (§5) — en particulier
   `DESIGN-runtime-segment-pipeline.md`, jamais lu en entier jusqu'ici.
2. Cartographie des points de production Forge existants : inspecter
   réellement `crates/core/schema/build/main.rs` et le pipeline
   `build/template/*` pour établir precisément ce qu'ils produisent
   aujourd'hui (schémas, projections, artefacts) et ce qu'ils ne
   produisent pas (aucun `RouteDescriptor`, confirmé par I1→I6, mais à
   revérifier sur le dépôt réel au moment du chantier — il peut avoir
   changé).
3. Comparaison norme ↔ code : confronter ce que §2 identifie comme
   manquant à ce que la cartographie de l'étape 2 révèle réellement
   présent ou absent.
4. Identification du plus petit point d'injection permettant de produire,
   pour une seule route réelle, un `RouteDescriptor`/`SegmentDescriptor[]`
   authentiquement issu de la Forge — pas encore une généralisation.
5. Seulement ensuite, proposition d'implémentation — avec la même
   discipline d'incréments courts que I1→I6.

**Informations restant inconnues, à vérifier dans les sources avant de
décider quoi que ce soit :**
- Le chemin exact et le format de `generated_schema.rs` (mentionné par
  l'utilisateur comme grep effectué en conditions réelles pendant la
  session Forge précédente, jamais localisé précisément par Claude
  lui-même).
- Le contenu complet de `DESIGN-runtime-segment-pipeline.md`.
- S'il existe déjà, côté `db-forge` ou `bridge-forge`, une convention de
  nommage table SQL → `packfile_key` réutilisable pour un futur catalogue
  `SourceKey`.
- Si `crates/core/schema/src/lib.rs`/`ContentCoreProjection` expose déjà
  une notion de route ou d'identifiant réutilisable pour `IdSource`.
- L'état réel du dépôt au moment où ce chantier démarre — du temps aura
  pu s'écouler depuis la clôture d'I1→I6 ; ne pas supposer que rien n'a
  changé.

---

## 7. Pièges à ne pas reproduire

- **Prendre `experimental_t2a.rs` pour la future architecture.** C'est un
  prototype PROVISOIRE, sans route de production, jamais pensé pour être
  étendu tel quel.
- **Prendre le stub `"content_core"` en dur pour un catalogue.** C'est une
  fermeture qui ignore sa propre entrée — pas un mécanisme de résolution.
- **Confondre `RouteEntry` (registry.rs, monolithique) et
  `RouteDescriptor` (projection/lib.rs, T2A).** Deux types différents,
  dans deux crates différentes, aux noms proches — une confusion réelle,
  rencontrée pendant I1→I6.
- **Traiter K=3 comme trois sources.** Le DESIGN distingue explicitement
  le nombre de segments du nombre de `SourceKey` distincts — ne jamais
  déduire l'un de l'autre.
- **Remettre de la logique transport dans `marius-render`.** Vérifié par
  son `Cargo.toml`, pas seulement par discipline — toute tentation d'y
  faire apparaître `Bytes`/`Body`/`axum` est un signal d'alarme.
- **Réintroduire des primitives abandonnées** (`EmissionPlan`, `IoSlice[]`
  comme IR Marius, `writev`/`sendmsg` dans Marius) sous un nom différent.
- **Confondre contextualisation AOT et Volatile.** Le `if`/`else`/`==`/`!=`
  résolu par la Forge (navigation, breadcrumb) n'a rien à voir avec le
  Volatile (états dépendant de la requête/session) — ADR-011 §1 les
  distingue explicitement, et le Volatile reste hors périmètre.

---

## 8. Statut de ce document

**Normatif** — ce handoff lui-même ne l'est pas : c'est un document de transition, pas une spécification. Les contraintes rappelées en §3 sont normatives par héritage (ADR-011, SPECIFICATION-transport-segmente-t2a.md v2), pas par ce document — voir directement ces sources pour toute décision qui en dépend.

**Factuel** (démontré par test, dans le dépôt réel) : tout le §1.

**Provisoire** (prototype, à remplacer, pas à étendre) : `experimental_t2a.rs`, le stub `"content_core"`, les deux routes K=1/K=3 fixes.

**Inconnu** (à vérifier avant de décider quoi que ce soit) : contenu complet de `DESIGN-runtime-segment-pipeline.md`, localisation/format de `generated_schema.rs`, conventions de nommage éventuelles côté `db-forge`/`bridge-forge`, état réel du dépôt au moment où ce chantier démarrera.

---

_Rédigé le 19 septembre 2026, à l'issue de la clôture documentaire du
prototype T2A I1→I6._
