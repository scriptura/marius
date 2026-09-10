# Handoff — Checkpoint de délibération : résolution des segments (`SegmentDescriptor`)

**Emplacement prévu :** `docs/handoffs/handoff-checkpoint-segment-resolution.md`
**Statut :** checkpoint de délibération intermédiaire — **pas un ADR, ne remplace pas le DESIGN**
**Date de rédaction :** 2026-09
**Portée :** confrontation du `DESIGN-runtime-segment-pipeline.md` (post-ADR-011) au code réel du repository, préalable à l'implémentation de Phase 3

---

## Comment lire ce document

Ce document n'est pas normatif. Il documente une **délibération**, pas une décision figée dans le DESIGN. Le DESIGN reste la référence technique ; ce checkpoint enregistre ce qu'une session de confrontation au code réel a découvert, quelles hypothèses ont été rejetées, quel modèle est retenu, et — point le plus important pour la reprise — **quelles questions restent volontairement ouvertes**. Une session qui reprend ce travail ne doit pas chercher à "résoudre" les points de la section 14 : ils sont différés par choix, pas par oubli.

La chronologie est délibérément préservée (section 4) : ce que le DESIGN affirmait initialement n'est jamais réécrit rétroactivement comme si l'ambiguïté avait été visible dès le départ.

---

## A. Contexte

**Marius** est un moteur web réactif écrit en Rust (ECS/DOD/AOT), dont ADR-011 a redéfini l'unité fondamentale de génération : la page HTML cesse d'être l'unité indivisible, remplacée par la **Projection** (Forge/AOT) → **Artefact** (Forge/AOT) → **Segment** (Runtime, plage mémoire contiguë) → **Réponse HTTP**. Le runtime devient un ordonnanceur de segments, pas un moteur de rendu.

`DESIGN-runtime-segment-pipeline.md` (post-ADR-011) est le document de conception technique détaillée qui formalise cette architecture cible : chaîne `SegmentDescriptor[] → MaterializedSource[] → EmissionPlan → IoSlice[] → backend`.

La migration suit un séquencement en phases (`confrontation-code-sequencement-phase0A-5.md`) :

```
Phase 0.A  renommage Segment → RenderChunk (lever la collision de nom)
Phase 0.B  mmap complet persistant du pack HTML (blob + index)
Phase 1    SourceKey (identité AOT globale d'une source)
Phase 2    SourceId + SourceSpec (référence locale à une route + nature de la source)
Phase 3    SegmentDescriptor, SegmentFlags, EmissionBackendKind, budgets, IOV_MAX
Phase 4    RouteDescriptor, résolution runtime, MaterializedSource, ResolvedRange, EmissionPlan
Phase 5    intégration Hyper/Axum, backend d'émission réel
```

Ce checkpoint documente le travail de préparation de **Phase 3**, interrompu par la découverte d'un problème de fond dans la forme proposée par le DESIGN pour `SegmentDescriptor`.

---

## B. État avant la confrontation — ce que disait le DESIGN initialement

Le DESIGN (§13.2 et alentours) proposait :

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub source: SourceId,
    pub offset: u64,
    pub len: u32,
    pub flags: SegmentFlags,
}
```

Cette forme semblait initialement cohérente pour une raison précise : ADR-011 §7 établit qu'un artefact statique est produit intégralement par la Forge, sans calcul de rendu au runtime — il était donc naturel d'en déduire que la *position* d'un fragment dans cet artefact (son `offset`/`len`) serait, elle aussi, un fait connu à la compilation, au même titre que son contenu. Le DESIGN présentait `offset: u64, len: u32` comme des **faits AOT**, une table `&'static [SegmentDescriptor]` figée par route (§7 : *« offset et len sont l'un et l'autre des faits AOT »*).

Rien dans le DESIGN, à ce stade, ne mentionnait `id`, `IdSource`, ni de mécanisme de sélection d'un enregistrement au sein d'un artefact.

---

## C. Découverte issue de la confrontation au code réel

La confrontation à `handlers.rs`/`registry.rs`/`main.rs` a fait apparaître un chemin que le DESIGN ne modélisait pas :

```
RouteEntry { pattern, packfile_key: &'static str, id_source: IdSource, content_type }
        │
        ▼
serve_route()
        │  match route.id_source { Fixed(n) => n, PathParam(name) => params.get(name)... }
        ▼
id: i64                                        ← connu ICI, au runtime pour PathParam
        │
        ▼
registry.load(route.packfile_key) -> Arc<PackHtmlIndex>
        │
        ▼
index_arc.lookup(id) -> Option<(u64, u32)>      ← résolution absente du DESIGN
        │
        ▼
deliver(index_arc, offset, len)
```

**Découverte fondamentale** : `(offset, len)` n'est jamais un fait connu à la compilation du binaire Rust, y compris pour `IdSource::Fixed(n)`. Il est le résultat d'une recherche (`PackHtmlIndex::lookup`) exécutée à chaque requête, contre la génération de l'artefact **actuellement publiée** — une génération qui change à chaque régénération réactive (cycle `LISTEN`/`NOTIFY` → `merge_sweep`, indépendant de toute recompilation). Un même binaire, sans être recompilé, peut donc vivre à travers des milliers de générations différentes d'un même artefact, chacune avec des positions physiques potentiellement différentes pour un même enregistrement.

`(offset, len)` ne peut donc pas être une constante AOT générique de `SegmentDescriptor` — c'était l'hypothèse implicite du DESIGN initial, invalidée par le code réel.

---

## D. Les quatre niveaux désormais distingués

| Niveau | Ce qu'il désigne | Exemple |
|---|---|---|
| **Source identity** | Quel artefact catalogué, globalement (`SourceKey`) ou localement à une route (`SourceId`) | « le pack `content_core` » |
| **Record selection** | Quel enregistrement au sein de cette Source | `Fixed(n)` (constante) ou valeur issue du contexte de requête (`PathParam`) |
| **Physical range** | Les octets exacts dans le blob, pour UNE génération donnée | `(offset, len)` retourné par `lookup()` |
| **Logical segment** | Un emplacement de la réponse HTTP, référençant les trois niveaux précédents sans en figer les valeurs runtime | `SegmentDescriptor` |

**`SegmentDescriptor` n'est plus assimilé à une plage physique.** C'est le changement de modèle central de cette délibération.

---

## E. Les quatre cycles de validité

1. **Compilation du binaire** (AOT Forge) — `SegmentDescriptor`, `SourceSpec`, les règles de sélection : figés pour toute la durée de vie du binaire.
2. **Publication d'une génération** — se produit à chaque régénération réactive, sans rapport avec (1), arbitrairement plus fréquente.
3. **Durée de vie d'une génération publiée** — l'intervalle entre deux `store()` successifs sur une entrée de `LiveRegistry`. `(offset, len)` d'un enregistrement donné n'est stable que **dans cet intervalle**.
4. **Durée d'une requête** — toujours incluse dans (3) : une requête qui résout une génération (`Arc::load_full()`, garanti par **Phase 0.B**) la retient pour toute sa durée, indépendamment de tout `store()` concurrent.

**Invariant établi, à faire figurer dans le DESIGN :**

> `(offset, len)` est stable pour une génération publiée donnée (cycle 3), mais jamais pour la durée de vie du binaire (cycle 1). Une requête doit conserver la génération contre laquelle ses plages ont été résolues — ce que garantit déjà `Arc<PackHtmlIndex>` (Phase 0.B, mapping persistant `[0, footer_start)`, validé par test de concurrence explicite).

---

## F. Modèles étudiés

### Modèle A — segment entièrement AOT (forme initiale du DESIGN)

```
SegmentDescriptor { source, offset, len, flags }
```

**Rejeté.** Ne fonctionne que si identité de source, sélection d'enregistrement, ET stabilité de la position après régénération sont toutes trois garanties à la compilation. Aucune route actuelle ne garantit la troisième condition ; `IdSource::PathParam` échoue aussi la deuxième.

### Modèle B — Source AOT + résolution runtime (**retenu**)

```
Source (identité AOT)
  → sélection (référence AOT vers une constante ou une valeur runtime)
  → génération publiée (résolution runtime, par SourceKey)
  → plage physique (résolution runtime, par segment)
```

Retenu parce qu'il préserve l'invariant déjà établi (résolution unique d'`Arc`/verrou par requête) en y ajoutant une opération de lecture pure, sans déplacer d'information AOT vers le runtime ni l'inverse.

### Modèle C — `record_id` dans `SegmentDescriptor`

**Rejeté.** Analysé explicitement (pas écarté par facilité) : `record_id` viendrait de `PathParam`, donc varierait par requête — contradiction directe avec « IR statique, figée par route ». Mélangerait une information de route dans une structure censée décrire une Source indépendamment de la route qui la référence. `i64` verrouillerait par avance la forme de toute sélection future.

### Autres éléments explicitement rejetés au cours de la délibération

- `IdSource` réimporté tel quel dans le Core IR (sémantique HTTP, ne doit jamais y figurer).
- `lookup()` exécuté dans le backend.
- Backend connaissant `PackHtmlIndex`, `Mmap`, `ArcSwap`, `Volatile`, `SourceKey`.
- `SegmentDescriptor` assimilé/aliasé à `PackfileEntry` (confusion de niveau ADR-011 : `PackfileEntry` est un fait de niveau 2/Artefact, stable seulement pour le cycle 3 ; un `SegmentDescriptor` AOT est un fait de niveau 1/compilation, stable pour le cycle 1 — les confondre revient à traiter une information stable "par génération" comme si elle l'était "par binaire").
- Reconstruction/allocation de `SegmentDescriptor` par requête.
- Nouvelle abstraction de routage parallèle à `RouteDescriptor`.
- Deux variantes de `SourceSpec::StaticArtifact` (« directement adressable » vs « indexé ») — démontré que c'est le même cas, la distinction relevant entièrement du niveau sélection, pas du niveau Source (cf. section H).

---

## G. Modèle actuellement retenu

```
SourceKey (identité AOT globale)
    ↓
SourceSpec (nature AOT de la Source — StaticArtifact{key} | VolatileSlot{capacity})
    ↓
SourceId (référence locale à la route)
    ↓
SegmentDescriptor (AOT : source + sélection opaque + flags — PAS de plage physique)
    ↓
── frontière AOT / Runtime ──
    ↓
sélection runtime (résolution des valeurs de sélection, une fois par requête)
    ↓
MaterializedSource (Source effectivement résolue — possède un Arc/handle, PAS Copy)
    ↓
ResolvedRange (plage physique résultant de la sélection appliquée à une Source résolue — Copy, léger)
    ↓
EmissionPlan (IR d'exécution — ranges + flags + backend_kind, rien de l'origine)
    ↓
IoSlice[]
    ↓
Backend (totalement ignorant de l'origine des données)
```

---

## H. Définition actuellement retenue de `SegmentDescriptor`

> `SegmentDescriptor` est l'emplacement logique d'un morceau de la réponse, référençant une Source (`SourceId`), une sélection au sein de cette Source, et des propriétés d'émission (`SegmentFlags`) — sans contenir de plage physique résolue.

**Ne contient pas** : `record_id` (la valeur, distincte de la référence de sélection), `offset`, `len`, `Arc`, `Mmap`, sémantique HTTP, sémantique Projection, pointeur runtime.

**Doit pouvoir représenter** : `Fixed(n)` (sélection = constante AOT — reste une sélection, cf. section N) ; `PathParam` (sélection = référence à une valeur runtime) ; plusieurs segments partageant une même Source, avec des sélections différentes ; des générations successives de l'artefact sans jamais être reconstruit ; `Volatile` (sélection absente, dans l'état actuel du modèle — cf. section N pour la nuance importante).

---

## I. Sélection — distinction règle / valeur

Le Core AOT IR ne doit **jamais** connaître `PathParam("id")` comme sémantique HTTP. Il connaît seulement une référence opaque, conceptuellement à deux formes :

- **`Constant(i64)`** — valeur connue à la compilation (`Fixed(n)`).
- **une référence opaque vers un emplacement du contexte de requête** (nommée provisoirement `RequestValueId` dans la délibération — **nom non figé, décision de nommage Rust non prise**) — un petit index désignant *« la valeur au slot N du contexte de requête »*, sans que le Core sache ce que ce slot représente en HTTP.

La correspondance *« le slot N est rempli par le paramètre `:id` de l'URL »* reste entièrement extérieure au Core — hors de `marius_projection`, probablement dans une table compagnon de `RouteEntry`/le futur Request Context.

---

## J. `MaterializedSource` / `ResolvedRange` — correction importante actée pendant la délibération

**Correction** : `MaterializedSource` ne peut pas être `Copy` s'il porte un `Arc<PackHtmlIndex>` (`Arc` implémente `Drop`, incompatible avec `Copy` par construction du langage). Une affirmation antérieure de la délibération l'avait présenté comme `Copy` — erreur signalée et corrigée en cours de session, conservée ici pour la trace.

- **`MaterializedSource`** = Source effectivement résolue pour la requête, dont la durée de vie est garantie pendant toute la requête. Possède l'`Arc<PackHtmlIndex>` (cas `Mmap`) ou un handle d'arène (cas `Volatile`). **Non-`Copy`.** Le propriétaire est le **Request Context**, dans une structure à capacité fixe (bornée par le nombre de `SourceKey` distincts référencés par la route — jamais un `Vec`).
- **`ResolvedRange`** = plage physique résultant de l'application de la sélection à une Source déjà résolue. Peut être `Copy`/léger (un pointeur et une longueur), parce que sa validité est garantie par l'`Arc` déjà détenu en amont — pas par lui-même.

Aucun type Rust définitif n'est figé à ce stade.

---

## K. Unité de cohérence : `SourceKey`, pas `SourceId`

**Correction importante actée pendant la délibération.** `SourceId` est une référence locale à la route ; deux `SourceId` différents peuvent légalement référencer le même `SourceKey`. Si la résolution de génération était dédupliquée par `SourceId` plutôt que par `SourceKey`, un `store()` concurrent survenant entre deux résolutions pourrait faire observer **deux générations différentes** du même artefact au sein d'une seule réponse — une incohérence, pas seulement un gaspillage.

> **Invariant retenu** : une résolution physique de génération par `SourceKey` distinct référencé par la route, par requête — jamais par `SourceId`. Deux `SourceId` partageant un `SourceKey` doivent aboutir à la même valeur résolue.

Cette règle est une optimisation ET une garantie de cohérence — elle n'interdit pas structurellement à deux `SourceId` de référencer le même `SourceKey` (aucune interdiction imposée à la Forge sans justification supplémentaire).

---

## L. Plusieurs segments, même Source

```
Segment 0 → Source 0 → sélection A
Segment 1 → Source 0 → sélection B
Segment 2 → Source 0 → sélection A
```

Décidé :
- une génération résolue une fois par `SourceKey` distinct (section K) ;
- une plage (`ResolvedRange`) résolue par segment — **aucune déduplication des `lookup()` répétés** (le coût d'une recherche dichotomique en mémoire déjà mappée est jugé négligeable ; dédupliquer ajouterait une table de correspondance supplémentaire pour un gain non démontré — même discipline qu'ADR-007/ADR-008 sur `MSG_ZEROCOPY`) ;
- correspondance stricte 1:1, par position, entre `SegmentDescriptor[]` et `ResolvedRange[]` ;
- ordre conservé strictement — pas de réordonnancement, pas de filtrage ;
- `source_id` n'a pas besoin d'être porté par `ResolvedRange` tant que cette correspondance 1:1 par index est garantie par construction.

---

## M. Volatile — homogénéité structurelle, pas matérielle

Décidé :

```
VolatileSlot → MaterializedSource → ResolvedRange → EmissionPlan
```

La **forme** du pipeline est homogène — même type `ResolvedRange`, même étapes, aucune branche ontologique séparée nécessaire.

**Non décidé, explicitement différé** : mécanisme de production du contenu volatile ; longueur effective réelle ; cycle de vie ; invalidation ; détails du `RequestArena`.

**Ne jamais affirmer** `len = capacity` : la capacité AOT est une **borne**, la longueur effective produite est une **information différente**, non encore disponible dans le modèle actuel. Un `ResolvedRange` complet pour `Volatile` ne peut pas être honnêtement construit tant que ce mécanisme n'existe pas — l'homogénéité est structurelle, pas encore matérielle.

---

## N. Réserve importante — nuance à ne pas sur-généraliser

Une formulation antérieure de cette délibération avait présenté l'absence de sélection comme concernant *« exclusivement Volatile »*. **Cette formulation est trop forte et a été corrigée** :

> La réalité actuelle est que les artefacts statiques de Marius sont adressés par sélection d'un enregistrement — `Fixed(n)` constitue donc déjà une sélection (constante). `Volatile` n'a pas de sélection d'enregistrement parce que son contenu est *produit* à la requête, pas *extrait* d'une collection préexistante.

**Ne pas transformer ceci en contrainte universelle de `SegmentDescriptor`.** Le modèle général ne doit pas interdire, à l'avenir, une Source statique véritablement directement adressable (sans sélection d'aucune sorte) si un tel cas apparaît. L'absence de sélection est aujourd'hui une propriété du seul cas `Volatile` — pas un invariant structurel figé pour toujours.

---

## O. Points volontairement différés — ne pas chercher à les résoudre

- Mécanisme Forge → `VolatileSlot` (déclaration author-facing de volatilité) — recherché explicitement (y compris inspection de `fragment-forge/src/fragment/static_markers.rs`, qui s'est révélé sans rapport : distinction lexicale statique/dynamique de template, pas la distinction ADR-011 artefact-stable/session-scoped). **Confirmé absent du repository, chaîne incomplète, pas seulement non implémentée.**
- Remplissage effectif des slots de sélection runtime depuis les paramètres HTTP réels.
- Longueur effective, cycle de vie et invalidation d'un segment `Volatile`.
- Toute forme Rust définitive de `MaterializedSource`, `ResolvedRange`, `EmissionPlan`, `RequestValueId` (nom provisoire).
- Propriétaire outillé exact du calcul `backend_kind`/de la vérification `IOV_MAX` — `crates/shell/server/build.rs` a été inspecté : il ne connaît aujourd'hui ni les routes, ni `IdSource`, ne génère que `ASSET_ROUTES` (assets statiques, axe disjoint) ; candidat plausible pour héberger cette responsabilité future, non confirmé.
- `MSG_ZEROCOPY` (différé depuis ADR-011/handoff Hyper-Axum, sans rapport direct avec cette délibération mais toujours en attente).

---

## P. Séquencement — état exact

```
Phase 0.A  CLOSED — renommage Segment → RenderChunk
Phase 0.B  CLOSED — mmap persistant [0, footer_start), primitive blob()
Phase 1    CLOSED — SourceKey
Phase 2    CLOSED — SourceId + SourceSpec (aucune incompatibilité structurelle révélée par cette délibération)
Phase 3    EN ATTENTE DE GO — périmètre élargi par cette délibération (voir ci-dessous)
Phase 4    NON COMMENCÉE
Phase 5    NON COMMENCÉE
```

### Phase 3 — doit désormais fixer

- `SegmentDescriptor` (forme retenue : `source: SourceId`, sélection AOT opaque, `flags: SegmentFlags` — **sans** `offset`/`len`) ;
- la représentation de la sélection AOT opaque (constante vs référence à une valeur runtime) ;
- `SegmentFlags` ;
- `EmissionBackendKind` ;
- budgets (distincts de `MAX_RENDER_CHUNKS`, qui reste un budget de rendu Forge par enregistrement, sans rapport avec le budget de composition HTTP par route) ;
- vérification AOT `IOV_MAX`/`UIO_MAXIOV` (emplacement outillé encore non confirmé, cf. section O).

**Note de périmètre** : le concept de sélection AOT opaque a été déplacé de Phase 4 vers Phase 3 au cours de cette délibération — il doit être fixé en même temps que `SegmentDescriptor` pour éviter d'avoir à rouvrir sa forme en Phase 4.

### Phase 3 — ne doit **pas** implémenter

- `MaterializedSource` runtime ;
- `ResolvedRange` ;
- Request Context ;
- lookup runtime intégré au nouveau pipeline ;
- `EmissionPlan` ;
- Hyper/Axum.

### Phase 4 — portera

`RouteDescriptor` ; résolution des valeurs de sélection depuis HTTP ; résolution des générations par `SourceKey` (déduplication, section K) ; `MaterializedSource` ; `ResolvedRange` ; `EmissionPlan` ; Request Context.

### Phase 5 — portera

Intégration réelle Hyper/Axum, backend d'émission (`SingleFile`/`Scatter`).

---

## Q. Hiérarchie documentaire

```
ADR-011 (décision architecturale)
    ↓
DESIGN-runtime-segment-pipeline.md (modèle technique cible)
    ↓
CONTRAT-marius-one-page-extension.md (invariants et interdictions)
    ↓
confrontation-code-sequencement-phase0A-5.md (état du repository, séquencement)
    ↓
handoff Hyper/Axum Phase 5 (cartographie Hyper/Axum, différée)
    ↓
CE CHECKPOINT (délibération intermédiaire — non normatif)
```

**Ce checkpoint documente une délibération qui devra être répercutée dans le DESIGN** (amendements listés en section R) — il ne doit jamais devenir une seconde source normative concurrente. Une fois le DESIGN amendé en conséquence, ce document perd sa raison d'être active et peut être archivé.

---

## R. Amendements à apporter au DESIGN (proposés, non appliqués)

| § concerné | Problème actuel | Formulation proposée | Raison |
|---|---|---|---|
| §7 | `offset`/`len` présentés comme faits AOT génériques de `SegmentDescriptor` | `SegmentDescriptor` ne porte ni `offset` ni `len` — voir section H pour la définition retenue | Invalide pour toute source indexée (majorité des routes réelles) |
| §7/§8 | Pas de distinction des cycles de validité | Ajouter l'invariant des 4 cycles — section E | `(offset,len)` stable seulement pour le cycle 3, jamais le cycle 1 |
| §8 | `MaterializedSegment` réservé au seul cas `Volatile` | Généraliser en `ResolvedRange` (nom non figé), applicable à toute origine, homogène en forme mais pas nécessairement complet pour `Volatile` — section M | Le besoin n'est pas spécifique à `Volatile` |
| §3 | « résolution des Sources une fois par requête » | « une résolution physique par `SourceKey` distinct référencé par la route, par requête — jamais par `SourceId` » | Risque de génération incohérente sinon — section K |
| §13.1 | `RouteDescriptor` sans notion de sélection | Noter que la sélection est portée par `SegmentDescriptor` lui-même, pas par un tableau parallèle | Évite un invariant non typé entre deux tableaux désynchronisables |
| §13.2 | `SourceSpec::StaticArtifact{key}` — suffisance non justifiée | Ajouter l'invariant « `SourceSpec` décrit l'origine ; la sélection décrit l'élément » — avec la nuance de la section N | Ferme la question initiale sans sur-généraliser |
| *(nouveau)* | Aucune mention d'un concept de slot de sélection runtime | Introduire un concept opaque (nom non figé), jamais porteur de sémantique HTTP | Nécessaire pour `PathParam` sans réimporter `IdSource` — section I |
| *(nouveau)* | `MaterializedSource` implicitement traité comme `Copy` dans les échanges informels | Documenter explicitement : non-`Copy`, possédé par le Request Context, borné | Correction actée — section J |

Aucune autre section du DESIGN n'est apparue incohérente à l'issue de cette délibération (`IOV_MAX` §7, critères `EmissionBackendKind` §9, budgets : inchangés).

---

## État de reprise

```
Phase 3 n'est pas encore implémentée.

La sémantique de SegmentDescriptor a été conceptuellement verrouillée
autour du Modèle B :

    Source identity → selection → published generation → physical range → emission

Le DESIGN doit encore être amendé conformément à cette délibération
(amendements listés en section R — proposés, non appliqués).

Le prochain arbitrage consiste à valider ce checkpoint et les amendements
du DESIGN, puis seulement à donner le GO d'implémentation à Phase 3.
```

**Points volontairement différés — ne pas tenter de les résoudre à la reprise** (détail complet en section O) : mécanisme Forge → `VolatileSlot` ; remplissage runtime des slots de sélection ; longueur effective/cycle de vie de `Volatile` ; formes Rust définitives de `MaterializedSource`/`ResolvedRange`/`EmissionPlan` ; propriétaire outillé du calcul `backend_kind`/vérification `IOV_MAX` ; `MSG_ZEROCOPY`.

**Nuance à ne pas perdre à la reprise** (section N) : l'absence de sélection n'est une propriété que du cas `Volatile` actuel — ne jamais la coder comme une contrainte universelle interdisant une future Source statique sans sélection.
