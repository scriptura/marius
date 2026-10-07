# DESIGN — Pipeline Runtime de Segments (ADR-011)

**Statut :** Accepté — état stabilisé du pipeline runtime T2A (7 octobre 2026). Ce document décrit l'architecture réellement retenue, et non l'historique des hésitations qui y ont conduit.

**Documents amont :**

- `ADR-011-projections-ordonnancees.md` — ontologie Projection / Artefact / Segment / Réponse HTTP ;
- `docs/archived/SPECIFICATION-transport-segmente-t2a.md` (v2) — frontière transport T2A ;
- `docs/contrats/CONTRAT-volatile-v1.md` — contrat des segments volatils (P1–P8) ;
- `docs/contrats/CONTRAT-marius-one-page-extension.md` — invariants d'augmentation ;
- ADR-008 (Minimum Viable Document) et ADR-009 (adressage par PK), non remis en cause.

**Hors périmètre de ce document :** intégration `hyper`/Axum au-delà de la frontière du §4 (SPEC T2A v2), devenir du trait `Projection` historique (ADR-011 §3), authentification et session, HTTP/2.

---

## 1. Chaîne d'IR — vue d'ensemble

```text
Forge (AOT)                                   Runtime Marius (par requête)            Transport
───────────                                   ────────────────────────────            ─────────
Projection → Artefact → SegmentDescriptor[]
                          │
                          ▼
                   résolution des Sources  →  MaterializedSource  →  ResolvedRange[]  →  Bytes → Body → Hyper
                   (par SourceKey distinct)   (Mmap | Volatile)      (par segment)       (frontière, §4)
```

Frontière stricte : tout ce qui précède la résolution des Sources est produit une fois, à la compilation ou à la régénération d'un artefact, et ne varie plus par requête. Tout ce qui la suit est reconstruit à chaque requête.

Chaque niveau perd de la sémantique métier et gagne en proximité matérielle : la Forge ne connaît que des Projections, le runtime ne connaît que des Sources et des plages, le transport ne connaît que des octets.

**Stratification en trois familles :**

| Famille | Éléments | Nature |
| --- | --- | --- |
| IR statique (Forge) | Projection, Artefact, `SegmentDescriptor[]`, `RouteDescriptor` | compilée une fois, figée par route |
| IR d'exécution (Runtime Marius) | `MaterializedSource`, `ResolvedRange` | instanciée par requête, propre à Marius |
| Transport | `Bytes`, `Body`, `IoSlice`, socket | n'est plus une IR de Marius : adaptation, puis propriété de Hyper |

**`ResolvedRange[]` est le dernier niveau de représentation Marius.** `EmissionPlan` n'est pas conservé comme IR d'exécution, et aucun agrégat équivalent n'existe sous un autre nom. `IoSlice[]` n'est pas une étape du pipeline : c'est un détail interne de Hyper.

## 2. `SegmentDescriptor` — IR produite par la Forge

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub source:    SourceId,         // origine logique, locale à la route — §8.2
    pub selection: SegmentSelection, // référence AOT opaque, jamais une valeur runtime — §2.1
    pub flags:     SegmentFlags,     // `SegmentFlags::VOLATILE` pour un segment volatile
}
```

Propriétés :

- `Copy`, POD, `#[repr(C)]` : pas de `Vec`, `String`, `Box`, `Arc`, pas de lifetime propre.
- `SourceId` désigne une **origine logique locale à la route** (un indice dans `RouteDescriptor.sources`), jamais une adresse ni un indice d'implémentation.
- Le tableau `SegmentDescriptor[]` d'une route est généré par la Forge ; son cardinal est la propriété AOT « nombre de segments » de la route (§7). Le runtime ne le modifie ni ne le régénère : il le **résout**, en deux étapes distinctes (§3 puis §3.2).
- `SegmentDescriptor` décrit un **emplacement logique** de la réponse (Source, sélection, drapeaux). Il ne porte **jamais** de plage physique (`offset`/`len`) : une plage physique est un fait de génération publiée, pas un fait de compilation (§3.1).

### 2.1 Sélection AOT — référence, pas valeur

Le Core IR ne connaît aucune sémantique HTTP. `SegmentSelection` a trois formes :

```rust
pub enum SegmentSelection {
    Constant(i64),                // valeur connue à la compilation
    RequestSlot(RequestValueId),  // emplacement du contexte de requête — jamais une valeur HTTP
    NotApplicable,                // aucune sélection : réservé à SourceSpec::VolatileSlot (P7)
}
```

La correspondance « le slot N est rempli par le paramètre `{id}` de l'URL » reste extérieure au Core IR : elle est portée par l'adaptateur HTTP (`marius-server`). `Constant` et `RequestSlot` ne sont pas deux chemins architecturaux : seule l'obtention de la valeur diffère ; une fois obtenue, la résolution de plage est unique.

La sélection est portée par `SegmentDescriptor` lui-même, jamais par un tableau parallèle (§8.1) : deux tableaux désynchronisables introduiraient un invariant que le compilateur ne peut pas vérifier.

`NotApplicable` n'est jamais encodé par `Constant(0)` ni par `RequestSlot(0)` : un segment volatil est *produit*, pas *extrait* d'une collection par clé.

## 3. `MaterializedSource` — matérialisation d'une origine

`SourceId` est un identifiant fermé, pas un trait object. Sa résolution via `SourceSpec` (§8.2) produit une valeur concrète :

```rust
pub enum MaterializedSource {
    Mmap     { handle: Arc<PackHtmlIndex> },     // artefact statique
    Volatile { storage: Arc<VolatileStorage> },  // segment produit à la requête — §6
}
```

Enum fermé : le dispatch est un `match`, pas une vtable.

> **Invariant :** une résolution de génération par **`SourceKey` distinct** référencé par la route et par requête — jamais par `SourceId`. Deux `SourceId` partageant un `SourceKey` aboutissent à la même valeur résolue.

`SourceId` est local à la route ; deux `SourceId` peuvent légalement référencer le même `SourceKey` (identité globale au catalogue). Résoudre par `SourceId` pourrait faire observer deux générations d'un même artefact dans une seule réponse en cas de `store()` concurrent.

`MaterializedSource` n'est **pas `Copy`** : la variante `Mmap` porte un `Arc`, qui implémente `Drop`. Propriétés :

- **propriétaire** : le contexte de résolution de la requête (`SourceResolutionContext<N>`), structure à capacité fixe bornée par le nombre de `SourceKey` **distincts** de la route — jamais un `Vec`, et jamais égal par construction au nombre de segments ;
- **durée de vie** : garantie pour toute la requête par la détention de l'`Arc` cloné ;
- pour une Source `StaticArtifact` : un seul point de résolution par `SourceKey` distinct, qui applique l'invariant de `DESIGN-store-registry.md` (une requête observe exactement une génération du monde statique). Ce DESIGN dépend de l'invariant, pas du mécanisme (`ArcSwap` aujourd'hui) qui le réalise ;
- pour une Source `VolatileSlot` : un `VolatileStorage` possédé et partageable, produit par le producteur désigné par sa `ProducerKey` (§6).

Cette résolution de génération est le seul endroit du pipeline qui touche un `Arc` d'artefact. Elle ne produit pas, à elle seule, une plage physique : c'est le rôle de §3.2.

### 3.1 Quatre cycles de validité — invariant à ne jamais perdre

1. **Compilation du binaire** (AOT, Forge) : `SegmentDescriptor`, `SourceSpec`, sélection AOT — figés pour toute la durée de vie du binaire.
2. **Publication d'une génération** : à chaque régénération réactive (`NOTIFY` → ingestion → régénération), sans rapport de fréquence avec (1), typiquement bien plus fréquente.
3. **Durée de vie d'une génération publiée** : l'intervalle entre deux `store()` successifs sur une entrée du registre. Une plage physique résolue n'est stable que **dans cet intervalle**.
4. **Durée d'une requête** : toujours incluse dans (3) ; une requête qui résout une génération la retient pour toute sa durée (détention de l'`Arc`).

> Une plage physique résolue est stable pour une génération publiée (cycle 3), jamais pour la durée de vie du binaire (cycle 1). `SegmentDescriptor` ne connaît que le cycle 1.

Pour un segment volatile, la durée de vie est celle du `VolatileStorage` détenu par la requête (cycle 4) ; il n'existe pas de génération publiée au sens du cycle 2.

### 3.2 `ResolvedRange` — résolution de plage, par segment

Une fois une Source résolue (§3), chaque segment résout **sa propre plage** :

```text
(MaterializedSource, valeur de sélection) → resolve_range / resolve_volatile_range → ResolvedRange
```

`ResolvedRange<'a>` est une tranche empruntée, liée à la durée de vie du `MaterializedSource` qui la produit (`ptr()`, `len()`, `is_empty()`, `as_slice()`). Propriétés :

- résolue **une fois par segment**, jamais dédupliquée entre segments partageant une Source (le coût d'une recherche dichotomique en mémoire déjà mappée est négligeable) ;
- correspondance 1:1, par position, entre `SegmentDescriptor[]` et les plages résolues : ordre conservé, pas de réordonnancement, pas de filtrage ;
- elle ne porte pas `source_id` : la correspondance par index suffit ;
- pour un segment volatile, la longueur est la **longueur effective** produite, jamais la capacité (§6).

Le résultat de la résolution en tête de requête est donc double : une structure de `MaterializedSource` indexée par `SourceKey` distinct (§3), et une plage par segment.

## 4. Frontière transport — où s'arrête Marius

`marius-render` ne dépend d'aucun de `axum`/`hyper`/`bytes` : il s'arrête à `ResolvedRange`. L'adaptation vers le transport est réalisée par `marius-server` :

```text
ResolvedRange
    │   adaptation (marius-server) — un owner par segment :
    │     segment statique → MmapOwner     { Arc<PackHtmlIndex>, offset, len }
    │     segment volatile → VolatileOwner { Arc<VolatileStorage> }
    ▼
Bytes::from_owner(owner)
    ▼
Body (frames) → Hyper → socket
```

Règles (`docs/archived/SPECIFICATION-transport-segmente-t2a.md` v2) :

- le payload référencé par un `ResolvedRange` n'est **jamais copié** dans un `Vec<u8>` pour franchir cette frontière ;
- `Bytes::from_owner` conserve l'owner vivant jusqu'au drop de la frame ;
- la longueur totale est connue avant la construction du `Body` (somme des longueurs *effectives*) et émise en `Content-Length` ; cette hypothèse est celle de l'incrément actuel, pas une obligation universelle ;
- Hyper conserve la propriété du socket et gère le framing, les écritures partielles, le backpressure et l'écriture vectorisée éventuelle. Marius ne reproduit aucun de ces mécanismes ;
- les valeurs internes de Hyper (plafonds de mise en file, nombre de buffers par écriture vectorisée) ne deviennent **jamais** des invariants Marius.

Le zéro-allocation n'est pas une propriété universelle de l'émission : le coût de matérialisation `ResolvedRange → Bytes` (une `Bytes` par segment, un `Arc`, le buffer du producteur volatile) est accepté et borné par le nombre de segments de la route. Les coûts internes du transport n'appartiennent pas au contrat de performance du Core Marius.

## 5. Principe directeur — descente monotone, aucune remontée

```text
Projection → Artefact → SegmentDescriptor[] → MaterializedSource[] → ResolvedRange[] → (transport)
```

Chaque étape abaisse le niveau d'abstraction vers le matériel. Aucune étape ne recompose une information déjà perdue par la précédente — la discipline d'une chaîne de compilation, jamais un aller-retour. Concrètement :

- `ResolvedRange` ne réintroduit aucune sémantique métier : uniquement un pointeur et une longueur ;
- la frontière transport ne prend aucune décision sur *quoi* émettre : cette décision est entièrement figée par `SegmentDescriptor[]` (Forge) et les Sources résolues ;
- le runtime ne distingue jamais `Mmap` de `Volatile` au-delà de la matérialisation (§3) : à partir de `ResolvedRange`, il ne manipule que `(ptr, len)`. Un `match` sur la variante de Source après cette étape romprait la séparation ;
- si une modification du transport exigeait de consulter à nouveau une Projection ou un Artefact, la descente n'est plus monotone et la modification est mal placée.

## 6. Segments volatils — support mémoire

Un segment volatil est *produit* à la requête. Son support mémoire est un **`VolatileStorage` possédé**, partageable par `Arc` — pas une arène de requête réinitialisée.

- `SourceSpec::VolatileSlot { capacity, producer: ProducerKey }` : la `capacity` est une **borne AOT** déterminée par la Forge ; le runtime ne la choisit jamais.
- `VolatileStorage::from_produced` prend possession du `Vec<u8>` du producteur et vérifie `effective_len <= capacity` **une seule fois**, à la construction ; un dépassement est une erreur contrôlée (HTTP 500), jamais une troncature (P2).
- La capacité (borne) et la longueur effective (produite) sont deux informations distinctes : **ne jamais affirmer `len = capacity`**.
- Le dispatch vers l'implémentation du producteur se fait par `ProducerKey` et vit **hors** de `emission.rs` (P8).
- La production est synchrone et possédée : aucune référence empruntée ne traverse un point de suspension.

Pourquoi pas une arène par worker réinitialisée à l'acquisition : `Bytes::from_owner` exige que le stockage vive jusqu'au drop de la frame, or Hyper écrit le corps **après** le retour du handler. Un buffer réutilisé par une requête suivante serait invalide pendant l'écriture. L'ownership par `Arc` rend ce cas impossible par construction.

Contrat complet : `docs/contrats/CONTRAT-volatile-v1.md`.

## 7. Budget de segments

Le nombre de segments d'une route (`K`) est une propriété AOT de la représentation générée, par route : 1 pour une route sans région volatile, 3 pour une route couverte par une région volatile (`head`, `volatile`, `tail`). Il est calculé et vérifié par la Forge ; le runtime suppose cette garantie acquise et ne réalise aucune correction dynamique. Aucune formule fermée n'est normative.

Quatre bornes distinctes, à ne jamais fusionner :

1. `Projection::MAX_RENDER_CHUNKS` — budget de rendu Forge, par enregistrement, interne à une Projection ;
2. le nombre de segments d'une route (`SegmentBudget`) ;
3. `SourceSpec::VolatileSlot.capacity` — borne AOT de la production volatile, par Source ;
4. les plafonds du transport (`IOV_MAX`, files internes de Hyper) — **externes à Marius** ; dépasser un plafond interne de Hyper peut entraîner davantage de cycles d'écriture côté transport, ce n'est pas une invalidité architecturale Marius.

## 8. `RouteDescriptor` — le contrat explicite Forge → Runtime

### 8.1 Forme

`RouteDescriptor` porte uniquement des métadonnées **produites par la Forge** : un contrat AOT pur, sans type runtime (`PackfileEntry`, `Arc`, `RawFd`).

```rust
#[repr(C)]
pub struct RouteDescriptor {
    pub segments:          &'static [SegmentDescriptor],  // fixe par route ; porte la sélection, pas de table parallèle
    pub sources:           &'static [SourceSpec],          // table de résolution des SourceId
    pub backend_kind:      EmissionBackendKind,            // champ hérité — voir ci-dessous
    pub volatile_capacity: u32,                            // somme des capacités des segments volatils (P6)
}
```

**Invariant :** la sélection AOT est portée par `SegmentDescriptor`, il n'existe volontairement aucun tableau parallèle de sélections.

`backend_kind` est un **champ hérité d'un modèle antérieur** : la Forge le génère à `EmissionBackendKind::Scatter`, valeur neutre, non consommée par T2A et sans signification architecturale. Le transport n'est pas choisi par la Forge (§4). Le champ, le type `EmissionBackendKind` et les helpers associés subsistent dans `marius-projection` ; leur retrait relève d'un arbitrage séparé.

### 8.2 `SourceSpec` — d'un `SourceId` logique à une recette de résolution

```rust
#[repr(C)]
pub enum SourceSpec {
    StaticArtifact { key: SourceKey },                             // résolu via LiveRegistry
    VolatileSlot   { capacity: u32, producer: ProducerKey },       // produit à la requête (§6)
}
```

Trois identités de portée différente, à ne pas confondre :

- **`SourceId`** : portée **locale à la route** (indice dans `sources`). Deux routes peuvent réutiliser la même valeur numérique pour des origines différentes ;
- **`SourceKey(u16)`** : portée **globale** au catalogue de build, position de l'artefact dans `ARTIFACTS` ; non persistante, peut changer entre deux builds. La résolution de génération est dédupliquée par `SourceKey`, jamais par `SourceId` ;
- **`ProducerKey(u16)`** : identité opaque d'un producteur de contenu volatile, catalogue distinct de `SourceKey`.

S'y ajoute `ArtifactKey` (identité logique de l'artefact publiable, dont `as_str()` est la clé de packfile) ; `ArtifactKey ≠ SourceKey ≠ component_id`.

**`SourceSpec` décrit l'origine d'une Source, jamais l'élément sélectionné en son sein.** Plusieurs segments d'une même Source peuvent légalement nécessiter des sélections différentes ; si la sélection vivait sur `SourceSpec`, ce cas serait irreprésentable. Pour `VolatileSlot`, l'absence de sélection est une propriété de la production (`NotApplicable`), pas une règle générale sur les Sources.

**Invariant de cohérence (P7) :** `VolatileSlot ⇔ SegmentSelection::NotApplicable ⇔ SegmentFlags::VOLATILE`. Le prédicat pur est `segment_matches_source`.

### 8.3 Production par la Forge

Les `RouteDescriptor` ne sont pas écrits à la main : le build de `marius-schema` les génère depuis `publication.toml` (`ROUTE_DESCRIPTORS`, aligné par index sur `ROUTES`), de même que `ARTIFACTS` (le catalogue des `SourceKey`) et les `RouteSpec` neutres. Une route dont l'artefact appartient à un composant couvert par une région volatile est générée en K=3 (`StaticArtifact(head)` → `VolatileSlot` → `StaticArtifact(tail)`, head et tail partageant la même sélection `RequestSlot(0)`) ; toute autre route est en K=1.

Le runtime résout une route par son nom (`ROUTES`) vers son `RouteDescriptor`, puis parcourt `sources` pour résoudre une génération par `SourceKey` distinct (§3), et `segments` pour résoudre une plage par segment (§3.2). `ROUTE_TABLE` (`RouteEntry`) ne décrit que la représentation serveur de la voie monolithique.

## 9. Ce que ce document ne tranche pas

- La forme Rust exacte de `RequestValueId` et le mécanisme de remplissage des slots de sélection depuis les paramètres HTTP réels : portés aujourd'hui par `marius-server`, hors du Core IR.
- Le devenir de `EmissionBackendKind`/`backend_kind`, de `SegmentBudget`/`segment_budget_fits_iov_limit`/`IOV_MAX_CURRENT_PLATFORM`, du type `RequestArena` et des helpers associés, qui subsistent dans le code sans être consommés par le chemin T2A retenu.
- Le mode de calcul et le stockage définitifs du budget de segments par la Forge.
- Les mécanismes de zéro-copie réseau (`MSG_ZEROCOPY`) et toute optimisation du transport : hors contrat Marius pour l'émission segmentée.
- HTTP/2 et les protocoles où la longueur totale ne serait pas connue à l'avance.
- Le contexte applicatif réel des segments volatils (authentification, session) : l'adaptateur actuel utilise un paramètre de requête expérimental.