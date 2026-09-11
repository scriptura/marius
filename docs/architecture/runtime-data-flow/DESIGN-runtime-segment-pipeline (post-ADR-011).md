# DESIGN — Pipeline Runtime de Segments (ADR-011)

**Statut** : Accepté — cinq sections (Ontologie/`SegmentDescriptor`/`MaterializedSource`/`EmissionPlan` ; `IoSlice` et descente monotone ; sélection de backend ; arène de requête ; contrat `RouteDescriptor`/`SourceSpec`). Reste hors périmètre, à traiter séparément : intégration `hyper`/Axum, devenir du trait `Projection` historique, mesure `MSG_ZEROCOPY`.

**Amendement (post-confrontation au code réel)** : cette révision intègre les amendements délibérés dans `handoff-checkpoint-segment-resolution.md` (Modèle B — Source AOT + résolution runtime). Le changement central : `SegmentDescriptor` ne porte plus de plage physique (`offset`/`len`) — voir §2, §3, §7, §8, §13. Le checkpoint reste archivé comme trace de la délibération ; ce document redevient la seule référence normative.

**Second amendement (post-implémentation Phase 3, clause d'échappement)** : la condition de compatibilité `SingleFile` (§9.1) présupposait encore, sous une forme résiduelle, que la contiguïté physique de deux segments est une propriété AOT — conséquence du Modèle B non tracée jusqu'au bout lors du premier amendement. Corrigé : `SingleFile` n'est certifié que lorsqu'il est AOT-prouvable ; dans l'état actuel de l'IR, seule une route à exactement un segment non volatil l'est. Voir §9.1.

**Documents amont** : ADR-011 (révisée), ADR-008 (Minimum Viable Document, non remis en cause), ADR-006 (amendée — périmètre restreint au cas sans volatil), ADR-009 (adressage PK, non remis en cause), CONTRAT-marius-one-page-extension.md (invariants d'augmentation, non remis en cause).

**Fichiers vus lors de la confrontation ayant motivé cet amendement** : `crates/shell/render/src/registry.rs` (`LiveRegistry`, `IdSource`, `RouteEntry`), `crates/shell/server/src/handlers.rs` (`serve_route`, `deliver`), `crates/core/projection/src/lib.rs` (`SourceKey`, `SourceId`, `SourceSpec`, `RenderChunk`, trait `Projection` — déjà en place, Phases 1-2 closes), `crates/shell/server/build.rs` (confirme l'absence de tout calcul `backend_kind`/`IOV_MAX` existant). Voir `handoff-checkpoint-segment-resolution.md` pour le détail de la confrontation.

---

## 1. Chaîne d'IR — vue d'ensemble

```
Forge (AOT, compile-time)                    Runtime (par requête)
──────────────────────────                   ──────────────────────
Projection → Artefact → SegmentDescriptor[]  →  résolution SourceId  →  EmissionPlan → IoSlice[] → backend
   (niveaux 1-2, ADR-011 §3)   (niveau 3, fixe   (SourceRuntime,         (résultat de    (niveau 4)  (writev/
                                par route)         snapshot des sources)  la résolution)              sendmsg/...)
```

Frontière stricte : tout ce qui est à gauche de « résolution SourceId » est produit une fois, à la compilation ou à la régénération d'un artefact, et ne varie plus par requête. Tout ce qui est à droite est reconstruit à chaque requête, à coût constant, sans allocation tas.

Chaque niveau perd de la sémantique métier et gagne en proximité matérielle — la Forge ne connaît que des Projections, `IoSlice` ne connaît que des adresses. Aucun niveau ne doit connaître les invariants du niveau qui le précède de plus d'un cran (le backend d'émission n'a pas besoin de savoir ce qu'est une Projection ; `EmissionPlan` n'a pas besoin de savoir ce qu'est un backend).

**Stratification à trois familles, pas une simple suite d'étapes :**

| Famille | Éléments | Nature |
| --- | --- | --- |
| IR statique (Forge) | Projection, Artefact, `SegmentDescriptor[]` | Compilée une fois, figée par route |
| IR d'exécution (Runtime Marius) | `MaterializedSource`, `EmissionPlan` | Instanciée une fois par requête, propre à Marius |
| Représentation POSIX (backend) | `IoSlice`, `msghdr`, appel `writev`/`sendmsg` | N'est plus une IR de Marius — un backend interchangeable |

`IoSlice` n'est pas le niveau 4 de l'ontologie métier, c'est déjà une traduction vers une API système particulière. Un futur backend (`io_uring`, QUIC/HTTP3) remplacerait cette seule ligne sans toucher à `SegmentDescriptor`, `MaterializedSource` ni `EmissionPlan`.

**Le backend d'émission ne distingue jamais `Mmap` de `Volatile`.** Cette distinction disparaît intégralement lors de la construction de l'`EmissionPlan` puis des `IoSlice` (§7) : à partir de ce point, le backend ne manipule que des couples `(ptr, len)` sans origine attachée. C'est une conséquence directe de la descente monotone (§6) — verrouillée ici explicitement pour éviter qu'une future implémentation ne réintroduise un `match` sur la variante de Source à l'intérieur du backend, ce qui romprait la séparation IR Marius / représentation POSIX.

---

## 2. `SegmentDescriptor` — IR produite par la Forge

**Correction de fond (post-confrontation au code réel, `handoff-checkpoint-segment-resolution.md` §B-D) :** une version antérieure de cette section présentait `offset`/`len` comme des faits AOT génériques, portés directement par `SegmentDescriptor`. Cette hypothèse est invalidée pour toute Source indexée (la majorité des routes réelles, cf. `handlers.rs::serve_route`/`deliver` : `(offset, len)` provient de `PackHtmlIndex::lookup()`, exécuté à chaque requête contre la génération **actuellement publiée** — jamais une constante figée à la compilation du binaire). `SegmentDescriptor` ne porte donc **ni `offset` ni `len`** :

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub source:    SourceId,        // origine logique, locale à la route — §13.2
    pub selection: SegmentSelection, // référence AOT opaque — constante ou slot du contexte de requête ; jamais une valeur runtime. Forme non figée — voir §2.1
    pub flags:     SegmentFlags,     // ex: Volatile, réservé pour extension — voir §4
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SourceId(u16);
```

`SegmentSelection` est un nom provisoire (aucune décision Rust définitive, cf. §2.1) porté ici pour rendre le contrat lisible — il ne doit pas être lu comme un type arrêté.

Propriétés non négociables, actées sur plusieurs tours de discussion :

- `Copy`, POD, `#[repr(C)]` — pas de `Vec`, `String`, `Box`, `Arc`, pas de lifetime propre. Ceci s'applique à `SegmentDescriptor` lui-même : une plage physique résolue (`offset`/`len` d'une génération donnée) n'a jamais cette propriété de stabilité et ne peut donc pas y figurer — voir l'invariant des quatre cycles de validité (§3.1).
- `SourceId` désigne une **origine logique** (« le packfile navigation », « l'emplacement du panier dans l'arène de requête »), jamais un indice d'implémentation ni une adresse. La Forge ne connaît que des Sources ; c'est le Runtime qui décide comment les matérialiser (§3).
- Le tableau `SegmentDescriptor[]` d'une route est fixe, généré par la Forge, et son cardinal est vérifié à la compilation contre le budget de segments (ADR-011 §8, et le plafond `IOV_MAX`, §7). Le runtime ne le modifie ni ne le régénère — il le **résout**, en deux étapes distinctes (résolution de génération puis résolution de plage, §3).
- `SegmentDescriptor` décrit un **emplacement logique** de la réponse (Source + sélection + drapeaux) — jamais une plage physique résolue. C'est le changement de modèle central de cet amendement (`handoff-checkpoint-segment-resolution.md` §D).

### 2.1 Sélection AOT — référence, pas valeur

Le Core IR AOT ne doit **jamais** connaître de sémantique HTTP (pas de `PathParam("id")`, pas d'`IdSource` réimporté tel quel). Il connaît seulement une référence de sélection opaque, conceptuellement à deux formes — ni l'une ni l'autre n'étant une décision de représentation Rust arrêtée à ce stade :

- une **constante connue à la compilation** (le cas `Fixed(n)` du routage actuel — reste une sélection, pas une absence de sélection) ;
- une **référence opaque vers un emplacement du contexte de requête** (nom provisoire dans la délibération : `RequestValueId`) — un indice désignant « la valeur au slot N du contexte de requête », sans que le Core IR sache ce que ce slot représente en HTTP.

La correspondance « le slot N est rempli par le paramètre `:id` de l'URL » reste entièrement extérieure au Core IR — hors de `marius_projection`, dans une table compagnon du futur Request Context (Phase 4). `Fixed` et `PathParam` (routage actuel) ne sont pas deux chemins architecturaux distincts : seule l'étape d'extraction de la valeur de sélection diffère entre eux ; une fois cette valeur obtenue, les deux convergent vers exactement le même mécanisme de résolution de plage (§3). Aucune branche séparée entre ces deux cas ne doit apparaître au niveau de la résolution — uniquement, en amont, au niveau de l'obtention de la valeur.

La sélection est portée par `SegmentDescriptor` lui-même, jamais par un tableau parallèle indexé séparément (cf. §13.1) : deux tableaux desynchronisables introduiraient un invariant non typé (« l'entrée N de la table de sélection correspond au segment N ») que le compilateur ne peut pas vérifier.

**Cas `Volatile`** : dans l'état actuel du modèle, une Source volatile n'a pas de sélection d'enregistrement, parce que son contenu est *produit* à la requête, pas *extrait* d'une collection préexistante. Ceci est une propriété du cas `Volatile` tel qu'il existe aujourd'hui dans le repository — pas une contrainte universelle interdisant, plus tard, une Source statique directement adressable sans sélection d'aucune sorte.

---

## 3. `MaterializedSource` — matérialisation d'une origine, résolue une fois par `SourceKey` distinct

**Correction de cardinalité (post-confrontation, `handoff-checkpoint-segment-resolution.md` §K) :** une version antérieure de cette section indexait la résolution par segment (`[MaterializedSource; K]`, K = budget de segments de la route). C'est incorrect : `SourceId` est une référence **locale à la route**, et deux `SourceId` distincts peuvent légalement référencer le même `SourceKey` (identité **globale** au catalogue, §13.2). Résoudre par `SourceId` plutôt que par `SourceKey` risquerait, en présence d'un `store()` concurrent entre deux résolutions, de faire observer **deux générations différentes** du même artefact au sein d'une seule réponse — une incohérence, pas seulement une redondance.

> **Invariant retenu** : une résolution de génération par `SourceKey` **distinct référencé par la route**, par requête — jamais par `SourceId`. Deux `SourceId` partageant un `SourceKey` doivent aboutir à la même valeur résolue. Cette règle n'interdit pas structurellement à deux `SourceId` de référencer le même `SourceKey` — elle garantit seulement la cohérence quand c'est le cas.

`SourceId` est un identifiant fermé, pas un type ouvert (pas de trait object — voir §5). Sa résolution (via `SourceSpec`, §13.2) produit une valeur concrète :

```rust
pub enum MaterializedSource {
    Mmap { handle: /* Arc<PackHtmlIndex> ou équivalent — non figé */ },  // artefact statique, aujourd'hui l'unique variante active
    Volatile { arena_ptr: *const u8 },      // segment de requête (session, panier...) — Phase 1 volatile uniquement
}
```

Enum fermé, pas trait object : le jeu de variantes est connu à la compilation, le dispatch est un `match`, pas une vtable — cohérent avec l'interdiction d'indirection dynamique sur le chemin chaud (ADR-011 §7).

**`MaterializedSource` n'est pas `Copy`.** Une version antérieure de cette délibération l'avait traité comme tel de façon informelle — erreur signalée et corrigée : la variante `Mmap` porte un `Arc<PackHtmlIndex>` (ou handle équivalent), et `Arc` implémente `Drop`, incompatible avec `Copy` par construction du langage. Propriétés arrêtées (la forme Rust exacte reste non figée) :

- **propriétaire** : le Request Context, dans une structure à capacité fixe — bornée par le nombre de `SourceKey` **distincts référencés par la route**, jamais un `Vec`, et jamais égale par construction au budget de segments `K` (qui peut être strictement supérieur, si plusieurs segments partagent une Source) ;
- **durée de vie** : garantie pendant toute la requête par la détention de l'`Arc` cloné (cas `Mmap`) ou du handle d'arène (cas `Volatile`) ;
- pour chaque Source de type artefact statique : une résolution qui applique l'invariant défini par `DESIGN-store-registry.md` §7 (« un batch/une requête observe exactement une génération du monde ») — un seul point de résolution par `SourceKey` distinct, jamais un par segment. Ce DESIGN dépend de cet invariant, pas du mécanisme (`RwLock`+`Arc`) qui le réalise aujourd'hui : si `StoreRegistry`/`LiveRegistry` change d'implémentation demain sans violer l'invariant, cette section reste valide sans modification ;
- pour chaque Source volatile : un pointeur dans l'arène de requête (buffer sur pile ou pool pré-alloué, jamais alloué sur le tas pour cette requête).

Cette résolution de génération est le seul endroit du pipeline qui touche un `Arc`/verrou. Elle ne produit pas, à elle seule, une plage physique : c'est le rôle distinct de la résolution de plage (§3.2).

### 3.1 Quatre cycles de validité — invariant à ne jamais perdre

Distinction à faire figurer explicitement, issue de la confrontation au code réel (`handoff-checkpoint-segment-resolution.md` §E) :

1. **Compilation du binaire** (AOT, Forge) — `SegmentDescriptor`, `SourceSpec`, la sélection AOT (§2.1) : figés pour toute la durée de vie du binaire.
2. **Publication d'une génération** — se produit à chaque régénération réactive (cycle `NOTIFY` → `merge_sweep`, cf. `manifest-reactive-projection.md`), sans rapport de fréquence avec (1), typiquement bien plus fréquente.
3. **Durée de vie d'une génération publiée** — l'intervalle entre deux `store()` successifs sur une entrée du registre vivant. Une plage physique résolue pour un enregistrement donné n'est stable que **dans cet intervalle**.
4. **Durée d'une requête** — toujours incluse dans (3) : une requête qui résout une génération la retient pour toute sa durée (détention de l'`Arc`), indépendamment de tout `store()` concurrent.

> **Une plage physique résolue est stable pour une génération publiée donnée (cycle 3), mais jamais pour la durée de vie du binaire (cycle 1).** Une requête doit conserver la génération contre laquelle ses plages ont été résolues — garanti par la détention de l'`Arc`/handle retourné à la résolution de génération (§3), pas par `SegmentDescriptor` (qui ne connaît, et ne doit connaître, que le cycle 1).

Confondre une plage physique (fait de cycle 3) avec `SegmentDescriptor` (fait de cycle 1) revient à traiter une information stable « par génération » comme si elle l'était « par binaire » — c'est précisément l'erreur corrigée par cet amendement (§2).

### 3.2 `ResolvedRange` — résolution de plage, par segment, distincte de la résolution de génération

Une fois une Source résolue (§3, par `SourceKey`), chaque segment qui la référence doit encore résoudre **sa propre plage physique** à partir de la valeur de sélection (§2.1) applicable à ce segment :

```text
(SourceKey résolue en MaterializedSource, valeur de sélection runtime) → lookup → ResolvedRange
```

Propriétés arrêtées pour `ResolvedRange` (forme Rust non figée — cf. §8) :

- résolu **une fois par segment**, jamais dédupliqué entre segments partageant une même Source (le coût d'une recherche dichotomique en mémoire déjà mappée est jugé négligeable ; dédupliquer ajouterait une table de correspondance supplémentaire pour un gain non démontré — même discipline qu'ADR-007/ADR-008 vis-à-vis de `MSG_ZEROCOPY`, §9.5) ;
- correspondance stricte 1:1, par position, entre `SegmentDescriptor[]` et `ResolvedRange[]` — ordre conservé strictement, pas de réordonnancement, pas de filtrage ;
- peut être `Copy`/léger (pointeur + longueur) : sa validité est garantie par l'`Arc` déjà détenu par le `MaterializedSource` résolu en amont, pas par `ResolvedRange` lui-même ;
- n'a pas besoin de porter `source_id`, tant que la correspondance 1:1 par index avec `SegmentDescriptor[]` est garantie par construction.

Le résultat de la résolution runtime en tête de requête est donc double, et non un unique tableau : une structure à capacité fixe de `MaterializedSource` (indexée par `SourceKey` distinct, §3) et un tableau `[ResolvedRange; K]` (indexé par segment, un par entrée de `SegmentDescriptor[]`). Aucun de ces deux tableaux ne porte plus de lifetime explicite au-delà de la durée de la requête — la durée de vie réelle est garantie par la détention de l'`Arc` cloné dans le Request Context, pas par `SegmentDescriptor`, `ResolvedRange` ni `MaterializedSource` eux-mêmes.

---

## 4. `EmissionPlan` — résultat de la résolution, jamais sa source

Point de vocabulaire introduit tardivement dans la discussion, avec une correction de position indispensable : `EmissionPlan` se situe **après** `SegmentDescriptor[]` dans la chaîne, jamais avant. Il nomme la combinaison, pour une requête donnée, du plan AOT fixe et des deux résultats distincts de sa résolution runtime (§3, §3.2) :

```rust
pub struct EmissionPlan<'req> {
    segments:     &'req [SegmentDescriptor],   // emprunté depuis la table statique générée par la Forge
    ranges:       [ResolvedRange; K],           // résolu une fois par segment (§3.2) — porte la plage finale
    sources:      /* structure à capacité fixe de MaterializedSource, indexée par SourceKey distinct — non figée */,
                                                 // retenu uniquement pour sa durée de vie (Arc), pas relu directement ici
    backend_kind: EmissionBackendKind,          // décidé par la Forge par route (§9.2), jamais redérivé ici
}
```

**Correction de forme (post-confrontation, cohérente avec §3/§3.2) :** une version antérieure de cette section faisait porter directement `[MaterializedSource; K]` à `EmissionPlan`, avec une cardinalité alignée sur le budget de segments. Cette cardinalité était incorrecte pour la même raison qu'en §3 (`SourceId` ≠ `SourceKey`) — `EmissionPlan` porte désormais le tableau `ResolvedRange[]` (cardinalité `K`, une entrée par segment, §3.2) comme source directe de la construction d'`IoSlice[]` (§7), et retient séparément les `MaterializedSource` résolus (cardinalité = nombre de `SourceKey` distincts, potentiellement `< K`) uniquement pour la durée de vie qu'ils garantissent — jamais réindexés par segment à ce stade.

`EmissionPlan` ne construit rien — il **porte** la combinaison (plan fixe, plages résolues, sources retenues pour leur durée de vie) le temps de la conversion finale vers `IoSlice[]` (section suivante du DESIGN). Aucune allocation, aucune copie de `SegmentDescriptor`. Sa seule responsabilité est de garantir, par construction du type, qu'on ne peut pas calculer un pointeur final sans être passé par la résolution complète des sources et des plages.

Aucune forme Rust définitive n'est figée ici pour `EmissionPlan`, `ResolvedRange` ni la structure portant les `MaterializedSource` — seules les propriétés (cardinalité, propriétaire, `Copy` ou non) sont arrêtées, cf. §3, §3.2, §8.

---

## 5. Ce que cette section n'tranche pas

- Construction exacte de `IoSlice[]` depuis `EmissionPlan` (section suivante).
- Nature du backend d'émission (`writev`/`sendmsg`/`MSG_ZEROCOPY`/futur `io_uring`) — volontairement indépendante de cette section, cf. ADR-011 §7 (trois invariants distincts) et l'amendement ADR-006.
- Devenir du trait `Projection` existant (ADR-011 §3) — sans impact sur cette section, qui ne s'appuie que sur `SegmentDescriptor`/`SourceId`, produits en aval de ce trait, quel que soit son nom final.
- Forme Rust définitive de `ResolvedRange`, de `EmissionPlan`, et de la structure portant les `MaterializedSource` résolus — cf. §3, §3.2, §8.

---

## 6. Principe directeur de la suite du DESIGN — descente monotone, aucune remontée

```
Projection → Artefact → SegmentDescriptor[] → MaterializedSource[] → EmissionPlan → IoSlice[] → Backend
```

Chaque étape abaisse le niveau d'abstraction vers le matériel. Aucune étape ne recompose une information déjà perdue par l'étape précédente — exactement la discipline d'une chaîne de compilation (AST → MIR → IR bas niveau → code machine), jamais un aller-retour. Concrètement pour la section qui suit :

- `IoSlice` ne réintroduit aucune sémantique métier (pas de notion de Projection, de Segment nommé, de domaine fonctionnel) — uniquement `(ptr, len)`.
- La construction d'`IoSlice[]` ne fait que traduire `EmissionPlan`, elle ne prend aucune décision nouvelle sur *quoi* émettre — cette décision est déjà entièrement figée par `SegmentDescriptor[]` (Forge) et `MaterializedSource[]` (résolution runtime, §3).
- Ce garde-fou sert de test pour toute extension future : si une modification de `IoSlice` ou du backend d'émission nécessite de consulter à nouveau une Projection ou un Artefact, c'est un signal que la descente n'est plus monotone, et que la modification est mal placée dans la chaîne.

---

## 7. `IoSlice[]` — traduction finale, représentation POSIX

Construction, sur pile, bornée par le budget de segments de la route (`K`, ADR-011 §8) :

```rust
fn build_io_slices<'req>(plan: &'req EmissionPlan<'req>) -> [IoSlice<'req>; K] {
    let mut slices: [MaybeUninit<IoSlice>; K] = /* ... */;
    for (i, range) in plan.ranges.iter().enumerate() {
        // range: ResolvedRange (§3.2) — déjà la plage physique finale pour ce
        // segment, pour la génération retenue par le MaterializedSource
        // correspondant. Aucun recalcul d'offset ici : la traduction ne fait
        // que lire (ptr, len) déjà résolus, cohérent avec la descente
        // monotone (§6) — IoSlice ne réintroduit aucune sémantique de Source.
        slices[i] = MaybeUninit::new(IoSlice::new(unsafe {
            std::slice::from_raw_parts(range.ptr(), range.len())
        }));
    }
    // transmute vers [IoSlice; K] une fois tous les éléments initialisés
}
```

**Correction de forme (post-confrontation, cohérente avec §2/§3/§3.2) :** une version antérieure de cette section calculait `ptr`/`len` ici même, en additionnant `seg.offset` à un pointeur de base retrouvé dans `plan.sources[seg.source.0]`. Cette construction présupposait que `SegmentDescriptor` porte `offset`/`len` (§2, invalidé) et indexait `sources` par `SourceId` avec une cardinalité `K` (§3, invalidé — la cardinalité correcte de `sources` est le nombre de `SourceKey` distincts, pas `K`). La traduction vers `IoSlice[]` lit désormais directement `plan.ranges[i]` (`ResolvedRange`, §3.2), déjà résolu en `(ptr, len)` par segment lors de la résolution runtime — cette section reste une traduction pure, sans nouveau calcul de position.

Aucune allocation : `K` est une constante par route, connue à la compilation (ADR-011 §8), le tableau vit sur la pile de l'appel.

**Vérification de plateforme à intégrer au build (nouveau point, pas encore couvert par ADR-011 §8) :** le budget de segments `K` doit être vérifié par la Forge non seulement contre un plafond arbitraire, mais contre `IOV_MAX` (`UIO_MAXIOV`, 1024 sur Linux) — la limite réelle acceptée par `writev`/`sendmsg` en un seul appel. Dépasser cette limite transformerait un budget de segments valide en erreur `EINVAL` au runtime, exactement le type d'échec que la Forge doit intercepter à la compilation plutôt que le runtime au chemin chaud (cf. le contexte initial de cette conversation : la même limite avait été identifiée comme motivant l'ADR-011 dans son ensemble). Cette vérification appartient à `build.rs`, aux côtés de la vérification déjà décrite en §8 d'ADR-011 — pas un nouveau mécanisme, une contrainte supplémentaire sur le même contrôle. L'emplacement outillé exact de cette vérification (candidat plausible : `crates/shell/server/build.rs`, qui ne connaît aujourd'hui ni les routes ni `IdSource` — vérifié, cf. `handoff-checkpoint-segment-resolution.md` §O) reste non confirmé, différé volontairement.

**Point de sûreté — capacité connue AOT, longueur effective connue à la résolution de plage (pas une exception au §6).** La distinction entre position AOT et position résolue est désormais générale, pas propre au seul cas `Volatile` (cf. correction §2/§3) : `SegmentDescriptor` ne porte jamais de plage physique, quelle que soit la variante de Source. Les segments issus d'artefacts statiques (`Mmap`) ont une longueur exacte connue de la Forge pour une génération donnée, mais cette valeur n'est lue qu'au moment de la résolution de plage (§3.2, via `lookup()`), jamais portée par `SegmentDescriptor` lui-même — cf. l'invariant des quatre cycles de validité (§3.1). Les segments volatils ne possèdent qu'une **capacité maximale** déterminée à la compilation (`SourceSpec::VolatileSlot.capacity`) ; leur **longueur effective** reste, à ce stade du DESIGN, une information non encore disponible (mécanisme de production du contenu volatile différé — §12). Cette opération complète une information volontairement laissée ouverte par la Forge — elle ne remet pas en cause la nature descendante du pipeline (§6), puisqu'aucune décision architecturale n'est prise à ce stade, seulement une valeur de fait renseignée dans les bornes déjà garanties.

Ne jamais affirmer `len = capacity` pour une Source volatile : la capacité AOT est une **borne**, la longueur effective produite est une information distincte, non encore disponible dans le modèle actuel (§12).

---

## 8. Ce que cette section n'tranche pas (complète §5)

- **Forme Rust exacte de `ResolvedRange`** (§3.2, introduite par cet amendement en remplacement du `MaterializedSegment` initialement envisagé pour le seul cas `Volatile`). Correction de portée : le besoin d'une résolution de plage distincte de la résolution de Source n'est **pas spécifique à `Volatile`** — toute Source indexée (le cas majoritaire des routes réelles, `Mmap` compris) en a besoin, puisque `(offset, len)` n'est jamais un fait AOT (§2, §3.1). `ResolvedRange` est donc générale, applicable à toute origine : même type, mêmes étapes, homogène en forme — mais pas nécessairement complète pour `Volatile` (cf. §12, longueur effective non encore disponible). Non tranché ici : le layout Rust précis (`ptr`/`len` bruts, ou une abstraction plus riche), et si `ResolvedRange` doit porter un discriminant de variante ou rester totalement neutre à l'origine de la Source qu'elle résout.
- Le mécanisme de bornage exact des segments volatils à longueur variable (§7, point de sûreté) — nécessite une décision avant tout composant volatil réel, hors périmètre Phase 1 (ADR-011 §11).
- Le choix du backend d'émission consommant `IoSlice[]` (`writev` vs `sendmsg` vs futur) — section suivante.
- La gestion d'erreur si `writev`/`sendmsg` retourne une écriture partielle (short write) — comportement POSIX standard à spécifier au niveau backend, pas au niveau `IoSlice`.

---

## 9. Backend d'émission — sélection, pas bifurcation de l'IR

Conformément au garde-fou du §6, cette section ne prend aucune décision qui remonterait vers `SegmentDescriptor`, `MaterializedSource` ou `EmissionPlan`. L'IR reste une chaîne unique et ne bifurque jamais :

```
SegmentDescriptor[] → MaterializedSource[] → EmissionPlan
```

Le backend n'est pas une nouvelle étape de cette chaîne : c'est un **consommateur** d'`EmissionPlan`, sélectionné une fois, jamais redérivé à chaque appel.

```
EmissionPlan
      │
      ▼
EmissionBackendKind   (déterminé par la Forge, cf. §9.2 — pas recalculé au runtime)
      │
      ├── SingleFile → sendfile(fd, offset, len_total)
      └── Scatter    → writev()/sendmsg() sur IoSlice[]
```

### 9.1 Condition de compatibilité `SingleFile` — AOT-prouvable, jamais supposée

**Amendement (post-confrontation, GO Phase 3) :** une version antérieure de cette sous-section posait une seconde condition — « tous les segments statiques proviennent de la même Source physique, à des offsets contigus » — qui présupposait implicitement que l'offset d'un segment est un fait AOT. C'est exactement l'hypothèse invalidée par la correction du Modèle B (§2, §3.1) : sous ce modèle, `SegmentDescriptor` ne porte jamais de plage physique, et la contiguïté de deux plages résolues n'est connue, pour deux segments quelconques partageant une même Source, qu'au moment de leur résolution runtime respective (§3.2) — jamais à la compilation. Une condition formulée en ces termes n'est donc **pas AOT-prouvable** avec les informations dont dispose la Forge aujourd'hui, et ne peut plus servir de critère de certification.

**Principe retenu** : `SingleFile` ne peut être sélectionné que lorsqu'il est **AOT-prouvable** avec les informations actuellement disponibles dans l'IR — jamais supposé, jamais approximé. Une route pour laquelle la contiguïté ne peut pas être établie à la compilation n'est pas « probablement `SingleFile` » : elle est `Scatter`, sans ambiguïté.

Dans l'état actuel de l'IR (un seul type de Source indexée, `Mmap`, et aucune preuve AOT de contiguïté inter-segments disponible à aucune couche), la condition se réduit à :

- **absence de tout segment `Volatile`** — pas seulement pour la requête courante : le gabarit d'une route (quels emplacements sont statiques, lesquels sont volatils) est fixé par la Forge, indépendant de l'état de la requête ; **et**
- **exactement un segment au total.**

Un seul segment rend la question de contiguïté inter-segments vide par construction : il n'y a rien avec quoi être contigu. C'est une condition **suffisante et conservatrice**, pas la définition architecturale définitive de `SingleFile` — elle certifie correctement un sous-ensemble des routes réellement éligibles, sans prétendre les couvrir toutes.

**Routes multi-segments, même entièrement statiques** : `Scatter`, dans l'état actuel de l'IR, faute de preuve AOT de leur contiguïté physique. La possibilité de certifier `SingleFile` pour plusieurs segments reste ouverte pour l'avenir, mais seulement lorsqu'une preuve AOT explicite de contiguïté existera — sa nature (un champ porté par `SourceSpec`, une propriété calculée par un futur générateur de routes, autre chose) et son propriétaire ne sont pas conçus par cet amendement, et ne doivent pas être anticipés par un champ, une variante ou un mécanisme introduit maintenant.

**Cas `0` segment** : n'est pas un cas d'exécution valide et ne relève donc pas de ce prédicat. Un `RouteDescriptor` sans aucun segment constitue une **erreur de génération AOT**, à détecter par la Forge en amont (§13, futur générateur de routes) — jamais un cas que le runtime, ou le prédicat de compatibilité `SingleFile`, aurait à interpréter comme `SingleFile` ou `Scatter` par défaut. Le prédicat suppose donc une précondition « route non vide » ; documenter et faire respecter cette précondition est une responsabilité de son appelant (Phase 4+), pas une valeur de retour à choisir arbitrairement ici.

### 9.2 Où cette décision est prise — AOT, pas par requête

Point de correction par rapport à une version antérieure de cette section : la compatibilité `SingleFile` **ne dépend d'aucune valeur connue seulement à la requête**. Elle ne dépend que du gabarit de la route (quels emplacements sont volatils, quelles Sources statiques sont utilisées) — deux informations entièrement connues de la Forge à la compilation, puisque c'est elle qui fixe la structure `SegmentDescriptor[]` de la route.

`EmissionBackendKind` est donc calculé **une fois, par la Forge, par route** — stocké à côté de `SegmentDescriptor[]` dans la table de routes (`ROUTE_TABLE`), pas recalculé ni redérivé par une méthode `EmissionPlan::is_sendfile_compatible()` au moment de la requête. Le Request Context lit ce champ, il ne le déduit jamais. C'est la même discipline que le budget de segments (§8 d'ADR-011) : le compilateur garantit, le runtime exécute — y compris pour le choix du backend, pas seulement pour ses bornes.

### 9.4 `writev`/`sendmsg` — comportement pour `EmissionBackendKind::Scatter`

```rust
fn emit(fd: RawFd, slices: &[IoSlice]) -> io::Result<usize> {
    // writev(2) : suffisant si aucune option socket (MSG_ZEROCOPY, flags) n'est requise
    // sendmsg(2) : requis si MSG_ZEROCOPY ou toute option socket est engagée (cf. §9.5)
}
```

**Écriture partielle (short write) — cas normal, pas une erreur.** `writev`/`sendmsg` peuvent retourner un nombre d'octets inférieur à la somme des `IoSlice`, sans que ce soit un échec (buffer socket plein, notamment sous forte charge). Le comportement retenu :

- Calculer les octets déjà émis, avancer dans le tableau de segments (ajuster le premier `IoSlice` partiellement consommé, sauter les suivants déjà émis) — mécanique standard, symétrique de ce que `sendfile()` gère déjà en interne pour un fichier unique.
- Aucune réallocation : l'avancement se fait par re-slicing des `IoSlice` existants, jamais par une copie.
- Ceci doit rester une boucle bornée (le nombre d'itérations ne peut pas dépasser le nombre de segments, lui-même borné par le budget AOT) — pas une boucle potentiellement non terminée.

### 9.5 `MSG_ZEROCOPY` — décision explicitement différée, pas présumée

Rappel de l'invariant déjà posé (ADR-011 §7, amendement ADR-006) : le passage à `writev`/`sendmsg` garantit zéro allocation et zéro reconstruction, **pas** zéro-copie réseau. Obtenir cette dernière propriété pour le cas composé exigerait `MSG_ZEROCOPY` (noyau ≥ 4.14), avec deux coûts propres, non encore chiffrés dans cette session :

- gestion asynchrone de la notification de complétion (`MSG_ERRQUEUE`) — le buffer ne peut pas être considéré libre tant que le noyau n'a pas confirmé la copie effective vers le NIC ;
- rentabilité dépendante de la taille : sous un certain seuil, le coût de la notification dépasse le gain, ce qui rendrait `MSG_ZEROCOPY` contre-productif précisément pour de petits segments volatils (le cas visé par ADR-011 §6).

**Décision retenue pour Phase 1 : `writev` sans `MSG_ZEROCOPY`.** La copie noyau résiduelle sur le chemin composé est acceptée comme coût connu et documenté (pas une régression silencieuse — l'amendement ADR-006 l'a déjà nommée). L'activation de `MSG_ZEROCOPY` reste une optimisation future, à ne considérer qu'après mesure, jamais par anticipation — même discipline que celle déjà appliquée par ADR-007/ADR-008 dans ce projet (ne pas construire avant la preuve du besoin).

---

## 10. Ce que cette section n'tranche pas

- Chiffrage réel du coût `MSG_ZEROCOPY` vs copie noyau simple — nécessite un banc de mesure, hors périmètre de ce DESIGN.
- Comportement exact en cas d'erreur irrécupérable en cours d'émission partielle (connexion coupée à mi-`writev`) — relève de la gestion de connexion HTTP générale, pas spécifique à cette section.
- Intégration avec `hyper::upgrade` ou équivalent pour obtenir un accès direct au socket sous Axum — point d'intégration pratique, pas une question de conception du pipeline de segments.

---

## 11. Arène de requête — support mémoire des segments volatils

Cette section verrouille des **propriétés**, pas une implémentation. L'implémentation de référence (arène par worker, réinitialisée entre requêtes) est un choix parmi d'autres compatibles avec ces propriétés — pas une obligation architecturale. Si le modèle d'exécution change un jour (workers, exécuteurs asynchrones, `io_uring`), seule l'implémentation est à revoir ; les invariants ci-dessous restent la référence.

### 11.1 Invariants verrouillés

1. Aucune allocation sur le chemin chaud (cohérent avec ADR-011 §7).
2. Allocation par curseur (« bump ») uniquement — jamais de structure d'allocation générale (pas de free-list, pas de recherche de bloc).
3. **Remise à zéro en O(1), à l'acquisition de l'arène par une requête, jamais à sa libération.**
4. Durée de vie strictement bornée à la requête qui l'a acquise.
5. Absence de partage concurrent d'une même arène entre deux requêtes simultanées.
6. Capacité exigée dérivée des bornes calculées par la Forge, jamais d'une constante choisie indépendamment.

### 11.2 Pourquoi le reset a lieu à l'acquisition, pas à la libération

Deux protocoles étaient possibles :

```
acquisition → utilisation → cleanup → libération      (rejeté)
acquisition → reset → utilisation → abandon            (retenu)
```

Le premier protocole dépend de l'exécution correcte d'un chemin de sortie (`cleanup`) pour rester sûr — un retour anticipé (erreur, panic, timeout) qui saute cette étape laisse l'arène dans un état incohérent pour la requête suivante, sans échec immédiat visible. Le second protocole est idempotent : aucune étape de nettoyage n'est requise sur les chemins d'erreur, parce que rien ne dépend de leur exécution — la garantie est reconstituée systématiquement à l'acquisition suivante, quel que soit l'état laissé par la précédente. C'est le même raisonnement que celui déjà appliqué ailleurs dans ce DESIGN et dans ADR-011 : préférer une garantie vérifiée en amont (ici, à l'acquisition) à une vérification tardive dont la fiabilité dépend de la discipline du code appelant.

### 11.3 Capacité — publiée par la Forge, jamais choisie indépendamment par le runtime

La Forge calcule et publie les exigences de capacité associées aux routes compilées (dérivées des capacités AOT de chaque segment volatil, §7-§8). Le runtime garantit que l'arène mise à disposition satisfait ces exigences — sans que cette section n'impose comment ces exigences sont agrégées (maximum global, par groupe de routes, par profil de worker, etc.). Cette latitude est volontaire : elle laisse la possibilité de spécialiser des profils mémoire différenciés plus tard, sans revoir cette section.

Ceci suit la même discipline que le budget de segments (§8 d'ADR-011) et la vérification `IOV_MAX` (§9.2) : une seule source de vérité (la Forge), le runtime consomme une exigence, il ne la redéfinit jamais.

### 11.4 Esquisse de structure (implémentation de référence, pas contrat)

```rust
pub struct RequestArena {
    buf:    *mut u8,   // pool pré-alloué, propriété du worker (ou de toute unité d'exécution retenue)
    cursor: usize,
    cap:    usize,     // dimensionné selon §11.3
}

impl RequestArena {
    fn acquire(&mut self) {
        self.cursor = 0;               // §11.2 : reset ici, pas ailleurs
    }
    fn bump(&mut self, len: usize) -> Option<*mut u8> {
        if self.cursor + len > self.cap { return None; }  // dépassement = erreur explicite, jamais une écriture hors bornes
        let ptr = unsafe { self.buf.add(self.cursor) };
        self.cursor += len;
        Some(ptr)
    }
}
```

Le cas de dépassement (`bump` retournant `None`) doit être un échec explicite et détectable — pas un déni silencieux ni une écriture hors bornes. Le traitement exact de ce cas (tronquer le segment volatil, rejeter la requête, autre) n'est pas tranché ici : c'est un point produit-métier, pas un point d'architecture mémoire.

**Alignement — hypothèse à expliciter, pas à laisser implicite.** L'esquisse ci-dessus avance octet par octet et présume une matérialisation de segments sous forme de `[u8]` plats, pour lesquels un alignement de 1 est suffisant — c'est le cas visé par cette section (contenu textuel/binaire opaque). Si le runtime devait un jour allouer dans cette arène des structures typées avec des contraintes d'alignement propres, le `bump` devrait intégrer un calcul de padding correspondant ; l'omettre serait un comportement indéfini classique des allocateurs bump en Rust. Non pertinent pour Phase 1, mais à ne pas oublier si l'usage de l'arène s'étend au-delà de segments `[u8]` plats.

---

## 12. Ce que cette section n'tranche pas

- Le traitement du cas de dépassement de capacité (`bump` → `None`, §11.4) : troncature, rejet, autre — décision produit, pas architecture.
- L'unité d'exécution exacte possédant l'arène (worker de thread, tâche asynchrone, autre) — implémentation de référence seulement, cf. préambule §11.
- Le mécanisme précis d'agrégation des exigences de capacité entre routes (§11.3) — volontairement laissé ouvert.

---

## 13. `RouteDescriptor` — le contrat explicite Forge → Runtime

Point identifié en relecture : les sections précédentes supposent que la Forge produit, par route, un ensemble d'informations cohérentes (`SegmentDescriptor[]`, §2 ; `EmissionBackendKind`, §9.2 ; exigence de capacité d'arène, §11.3) — mais aucune structure commune ne les rassemble, et rien ne spécifie comment un `SourceId` se rattache à un artefact réel. Ce contrat doit être explicite avant de figer ce document.

**Note de séquencement (sans impact sur le contenu de cette section) :** `RouteDescriptor` dépend structurellement de `SegmentDescriptor`/`EmissionBackendKind` (Phase 3 du séquencement d'implémentation) — l'introduire avant eux exigerait soit de le construire deux fois, soit de préjuger de leur forme. `confrontation-code-sequencement-phase0A-5.md` le place actuellement en Phase 2 ; `handoff-checkpoint-segment-resolution.md` propose de le déplacer en Phase 4, une fois `SegmentDescriptor` fermé. Cette divergence est documentée ici pour mémoire — la correction du document de séquencement lui-même est hors périmètre de cet amendement (voir rapport de session).

### 13.1 Séparation à respecter

`RouteDescriptor` porte uniquement des métadonnées **produites par la Forge** — un contrat AOT pur. Il ne contient jamais de type appartenant au runtime (pas de `PackfileEntry`, pas d'`Arc`, pas de `RawFd`). La manière dont le runtime associe ensuite ce contrat à un artefact concret via `ROUTE_TABLE`/`LiveRegistry` relève du render-shell-spec, pas de ce document.

```rust
#[repr(C)]
pub struct RouteDescriptor {
    pub segments:          &'static [SegmentDescriptor],  // §2 — fixe par route ; porte la sélection (§2.1), pas de table parallèle
    pub sources:           &'static [SourceSpec],          // §13.2 — table de résolution des SourceId
    pub backend_kind:      EmissionBackendKind,            // §9.2
    pub volatile_capacity: u32,                            // §11.3 — somme des capacités des segments Volatile
}
```

**Invariant à ne pas perdre** : la sélection AOT (§2.1) est portée par `SegmentDescriptor` lui-même — il n'existe volontairement aucun tableau parallèle de sélections indexé séparément dans `RouteDescriptor`. Un tel tableau introduirait un invariant non typé (« l'entrée N de la table de sélection correspond au segment N de `segments` ») que le compilateur ne peut pas vérifier par construction ; le porter directement sur `SegmentDescriptor` élimine cette classe d'erreur par le système de types.

### 13.2 `SourceSpec` — le chaînon manquant : d'un `SourceId` logique à une recette de résolution

`SegmentDescriptor.source` est un `SourceId` — une origine logique, jamais un indice d'implémentation (§2), mais dont la portée est **locale à la route** (un indice dans le `sources` de ce `RouteDescriptor`). Quelque chose doit dire au Runtime *comment* matérialiser cette origine (§3). C'est le rôle de `SourceSpec`, indexé par `SourceId` au sein d'une route :

```rust
#[repr(C)]
pub enum SourceSpec {
    StaticArtifact { key: SourceKey },   // à résoudre via LiveRegistry (render-shell-spec)
    VolatileSlot   { capacity: u32 },     // à réserver dans l'arène de requête (§11)
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SourceKey(u16);
```

Correction par rapport à une version antérieure de cette section : le champ était initialement `registry_key: &'static str`. Un identifiant de type chaîne détonnait dans un contrat qui se veut entièrement POD/AOT — il implique une résolution par hachage/comparaison de chaîne au runtime, alors que le reste du pipeline n'utilise que des indices compacts. `SourceKey(u16)` corrige ce point, avec une distinction à ne pas confondre :

- **`SourceId`** (§2) : portée **locale à une route** — indice dans le `sources: &'static [SourceSpec]` de ce `RouteDescriptor` uniquement. Deux routes différentes peuvent réutiliser la même valeur numérique de `SourceId` pour désigner des origines complètement différentes.
- **`SourceKey`** (ici) : portée **globale au registre**, attribuée une fois par la Forge à chaque artefact nommé du système (`nav`, `article`, `footer`...), stable across toutes les routes qui le référencent. C'est cette valeur, pas `SourceId`, que la Forge transmet ; le Runtime la résout en un `Arc<PackHtmlIndex>` via un tableau ou une table `LiveRegistry` indexée par `SourceKey`, plutôt que par une clé de chaîne. C'est également par `SourceKey`, jamais par `SourceId`, que la résolution de génération est dédupliquée par requête (§3, §3.1).

**Invariant à ajouter — suffisance de `SourceSpec` non justifiée dans une version antérieure de cette section, désormais explicite :** `SourceSpec` décrit l'**origine** d'une Source ; il ne décrit jamais l'**élément** sélectionné en son sein. La sélection (§2.1) ne peut pas être portée par `SourceSpec`, parce que plusieurs segments référençant la même Source peuvent légalement nécessiter des sélections différentes (exemple : deux segments d'une même route affichant chacun un enregistrement distinct du même artefact indexé) — si la sélection vivait sur `SourceSpec`, une Source ne pourrait porter qu'une seule règle, rendant ce cas structurellement irreprésentable. C'est cet argument, et non une préférence d'implémentation, qui exclut cette option.

**Nuance à ne pas sur-généraliser** : l'absence de sélection pour `SourceSpec::VolatileSlot` (son contenu est *produit* à la requête, pas *extrait* d'une collection préexistante — §2.1) est une propriété du seul cas `Volatile` tel qu'il existe aujourd'hui dans le repository, pas un invariant structurel qui interdirait, plus tard, une Source `StaticArtifact` véritablement directement adressable sans sélection d'aucune sorte. De même, la question d'une distinction interne à `StaticArtifact` (« directement adressable » vs « indexé ») a été examinée et écartée : les deux cas se ramènent au même niveau Source, la distinction relevant entièrement du niveau sélection.

`SourceSpec` reste ainsi un contrat AOT pur, entièrement POD. La résolution `SourceKey → Arc<PackHtmlIndex>` via `LiveRegistry` (§3) reste du ressort du runtime et du render-shell-spec — ce document ne spécifie que la forme du contrat, pas le mécanisme de lookup, ni la manière dont `SourceKey` est assignée (probablement par le même passage de build que celui qui calcule le budget de segments, §8 d'ADR-011 — à confirmer lors de l'implémentation).

Avec `RouteDescriptor`/`SourceSpec`, la boucle Forge → Runtime est complète : `ROUTE_TABLE` (render-shell-spec) résout une URL vers un `RouteDescriptor` ; le Request Context parcourt `sources` pour résoudre une génération par `SourceKey` distinct (§3), puis parcourt `segments` pour résoudre une plage par segment (§3.2) ; le reste du pipeline (§4 à §11) est inchangé.

### 13.3 Ce que cette section n'tranche pas

- La structure exacte de `ROUTE_TABLE` une fois qu'elle résout vers `RouteDescriptor` plutôt que vers `PackfileEntry` directement — modification du render-shell-spec, hors périmètre de ce DESIGN.
- Le mécanisme de lookup `SourceKey → Arc<PackHtmlIndex>` (existe déjà sous une forme voisine dans `LiveRegistry` — à confirmer par relecture du code, pas supposé ici).
- Le processus exact d'attribution des valeurs `SourceKey` par la Forge (numérotation séquentielle, hachage stable, autre) — détail de build, pas d'architecture.
- La forme Rust exacte de la sélection AOT opaque portée par `SegmentDescriptor.selection` (§2.1) — constante vs référence à un slot du contexte de requête, aucun `enum`/signature n'est encore fixé.
- Le mécanisme de remplissage effectif des slots de sélection runtime depuis les paramètres HTTP réels (Phase 4).
