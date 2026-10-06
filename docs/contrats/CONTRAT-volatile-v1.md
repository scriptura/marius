# Contrat Volatile V1

**Statut :** normatif pour le pipeline runtime T2A des segments volatils.
Ne modifie ni ADR-011 ni `SPECIFICATION-transport-segmente-t2a.md` (v2) :
**étend** ce que la SPEC v2 §8 laisse hors périmètre (« Le Volatile : production,
cycle de vie, `VolatileSlot` »), sans les contredire. Toute contradiction
découverte avec l'un de ces documents fait l'objet d'un audit séparé, jamais
d'une résolution silencieuse via ce contrat (même clause que SPEC v2 §10).

Ce document décrit l'architecture retenue et son état d'application dans le code.
Il ne retrace pas l'historique de son élaboration.

---

## 1. Modèle

Un segment volatile est une plage mémoire **produite à la requête**, par opposition
à un segment statique **sélectionné** dans un artefact AOT.

```text
producteur (clé opaque)
  → stockage possédé
  → longueur effective
  → ResolvedRange
  → owner conservé jusqu'au drop du Body
```

Le runtime reste ignorant de `.marius`, du DOM, de HTMX et du SQL. Toute
connaissance de domaine est confinée à l'implémentation du producteur ;
`emission.rs` ne manipule que des clés opaques.

```text
StaticArtifact                      VolatileSlot
    → sélection (RequestSlot)           → ProducerKey
    → lookup                            → SegmentSelection::NotApplicable
                                        → matérialisation directe
```

Un segment volatile n'a **aucune** sélection par clé primaire. `Constant(0)` et
`RequestSlot(0)` ne sont jamais des valeurs fictives pour `NotApplicable`.

## 2. Propriétés P1–P8

- **P1** — Aucun raw pointer dans `MaterializedSource::Volatile`.
- **P2** — La longueur effective est portée par le stockage ; `ResolvedRange` est
  (pointeur, longueur effective). `longueur > capacité` à la matérialisation donne :

  ```text
  → erreur contrôlée
  → HTTP 500 dans l'adaptateur
  → aucune troncature
  ```

  Jamais de lecture ni d'écriture hors bornes.
- **P3** — `MaterializedSource::Volatile` porte un handle **possédé et partageable**
  (`Arc<VolatileStorage>`). `marius-render` n'a aucune dépendance `bytes`, `axum` ou
  `hyper` : types `std` uniquement.
- **P4** — L'adaptateur côté `marius-server` clone ce handle dans un owner
  (`VolatileOwner`) passé à `Bytes::from_owner` : le stockage vit jusqu'au drop de
  la frame.
- **P5** — Production **avant** la construction du `Body` ; les octets sont un
  instantané ; aucune lecture après libération de l'owner. Aucune référence
  empruntée ne traverse un point de suspension (`await`). *État actuel :* la
  production est synchrone et a lieu dans la boucle de résolution des segments,
  dans l'ordre du descripteur ; rien ne suspend. Si un producteur devient
  asynchrone (lecture SQL), sa production devra précéder la résolution synchrone
  des plages statiques.
- **P6** — La capacité vient de la Forge (`SourceSpec::VolatileSlot.capacity`) ;
  le total `RouteDescriptor.volatile_capacity` est une somme dérivée, jamais
  choisie par le runtime.
- **P7** — Invariant :
  `SourceSpec::VolatileSlot ⇔ SegmentSelection::NotApplicable ⇔ SegmentFlags::VOLATILE`.
  `marius_projection::segment_matches_source` en est le prédicat pur. *État
  d'application :* la Forge émet la forme cohérente par construction (elle ne
  consulte pas le prédicat) ; le runtime (`content_document.rs`) n'accepte que les
  deux combinaisons attendues (statique + `RequestSlot`, volatile + `NotApplicable`)
  et répond 500 pour toute autre, sans passer par le prédicat ; le prédicat est
  exercé par les tests.
- **P8** — `SourceSpec::VolatileSlot` porte une `ProducerKey` opaque ; le runtime ne
  connaît que cette clé. Le dispatch vers l'implémentation du producteur vit
  **hors** de `emission.rs`.

## 3. Types et responsabilités

| Élément | Crate / module | Rôle |
| --- | --- | --- |
| `ProducerKey(u16)` | `marius-projection` | identité opaque d'un producteur ; catalogue distinct de `SourceKey` |
| `SegmentSelection::NotApplicable` | `marius-projection` | sélection d'un segment volatile |
| `SourceSpec::VolatileSlot { capacity: u32, producer: ProducerKey }` | `marius-projection` | source volatile : capacité AOT et producteur |
| `segment_matches_source` | `marius-projection` | prédicat de cohérence P7 |
| `VolatileStorage` | `marius-render::emission` | stockage possédé : `capacity` (borne AOT) et `effective_len()` (longueur produite) |
| `VolatileCapacityExceeded` | `marius-render::emission` | erreur contrôlée de P2 |
| `resolve_volatile_generation` / `resolve_volatile_range` | `marius-render::emission` | pendants volatils de `resolve_generation` / `resolve_range` |
| `VolatileContext`, `materialize_volatile`, `produce_nav_profile`, `NAV_PROFILE_PRODUCER` | `marius-render::volatile_producers` | producteur `nav_profile` et sélection par `ProducerKey` |
| `VolatileOwner`, `MmapOwner` | `marius-server::content_document` | adaptation vers `Bytes::from_owner` |

`resolve_volatile_generation` et `resolve_volatile_range` sont délibérément
distincts de `resolve_generation` et `resolve_range` : un volatile est *produit*,
jamais *récupéré* par sélection ; on ne détourne pas le mécanisme
`StaticArtifact → lookup(selection)`.

## 4. Capacité, longueur effective, ownership

- `VolatileStorage::from_produced(payload, capacity)` prend possession du `Vec<u8>`
  du producteur (`into_boxed_slice()`) et vérifie `effective_len <= capacity` **une
  seule fois**, à la construction. Le buffer est dimensionné à ce qui a été produit,
  jamais à la capacité.
- `ResolvedRange` emprunte directement le buffer de `VolatileStorage` : aucune copie
  entre le stockage et la plage résolue (vérifié par égalité de pointeur).
- `VolatileStorage` ne dérive ni `PartialEq` ni `Debug`. Le `Debug` de
  `MaterializedSource` est écrit à la main et n'expose que la variante, la
  longueur effective et la capacité : aucune fuite de contenu par un trait de
  diagnostic.
- Le `Content-Length` d'une réponse est la somme des longueurs **effectives** des
  frames ; jamais la capacité d'un segment volatile.

Le modèle « arène par worker réutilisée avec reset à l'acquisition » n'est pas un
invariant : il est incompatible avec `Bytes::from_owner`, Hyper écrivant le corps
après le retour du handler.

## 5. Allocations et copies

Une allocation par requête n'est **pas** une violation du contrat Marius lorsqu'elle
est nécessaire à la propriété sûre du chemin volatile. Coût d'un segment volatile :

- le `Vec<u8>` construit par le producteur (hors du contrôle de `emission.rs`), au
  plus la capacité AOT ;
- un `Arc<VolatileStorage>` ;
- l'owner de `Bytes::from_owner` (bookkeeping de la crate `bytes`).

Ce coût est borné par la capacité AOT, jamais par un payload arbitraire, et conforme
à la SPEC T2A v2 §4 (le zéro-allocation n'est pas une propriété de la famille
segmentée). Aucune copie du contenu n'est introduite après la production ;
`into_boxed_slice()` peut, si le `Vec` du producteur a une capacité excédentaire,
réallouer en interne (`shrink_to_fit`, détail de la bibliothèque standard) — un
producteur qui alloue exactement la longueur produite l'évite. Pas de pool, pas
d'optimisation prématurée.

## 6. Producteur `nav_profile` et contexte

- Un seul producteur, sélectionné par un `match` sur la `ProducerKey` de la source
  (`NAV_PROFILE_PRODUCER = ProducerKey(0)`) : pas de registre, pas de trait de
  producteur. Un second producteur ajoute un bras au `match`.
- `VolatileContext { username: Option<String> }` est **possédé**, sans lifetime. Il est
  construit par l'adaptateur à partir de la requête, avant toute matérialisation.
  `materialize_volatile` est synchrone ; le résultat est un `MaterializedSource` possédé.
- Sortie : `<li class="nav-profile">{username échappé}</li>`. Contexte anonyme :
  `<li class="nav-profile"></li>`.
- Le nom d'utilisateur est une donnée externe : `& < > " '` sont échappés, et
  l'échappement compte dans la capacité.
- Erreurs contrôlées (`VolatileProductionError`) : source non volatile, producteur
  inconnu, capacité dépassée. Jamais un `panic`.
- Le producteur ne lit aucune base de données.

**Contexte expérimental.** Dans l'adaptateur actuel, `username` provient du paramètre
de requête `?user=…`. C'est un contexte de démonstration, **pas** l'architecture
d'identité définitive : l'authentification et la session réelles sont hors périmètre.
Ce que le contrat fixe, c'est la chaîne `requête → VolatileContext → producteur`, pas
la provenance du nom.

## 7. Déclaration côté Forge

Déclarée dans `crates/core/schema/publication.toml` (`[[volatile_region]]` :
`component`, `marker`, `head_artifact`, `tail_artifact`, `capacity`), traitée par
`build/publication.rs`.

- Au plus une région par composant ; `head_artifact` et `tail_artifact` sont déclarés
  en `[[artifact]]`, distincts, et portent le même composant.
- Une route dont l'artefact appartient à un composant couvert par une région est
  générée en **K=3** dans `ROUTE_DESCRIPTORS` ; toute autre route reste en K=1.

```text
segment 0  StaticArtifact(content_core_head)  RequestSlot(0)   ← paramètre de route
segment 1  VolatileSlot(ProducerKey(0))       NotApplicable    flags = VOLATILE
segment 2  StaticArtifact(content_core_tail)  RequestSlot(0)   ← même paramètre
```

- `capacity` (valeur actuelle : 512 octets, provisoire) devient
  `SourceSpec::VolatileSlot.capacity` et `RouteDescriptor.volatile_capacity` (P6) ;
  la `ProducerKey` du slot est fixée par le générateur à `ProducerKey(0)`.
- K=3 n'est pas une borne globale ni une constante architecturale : le nombre de
  segments est une propriété de la représentation AOT réellement produite.
- Le monolithique (`content_core`) est conservé : voie de non-régression et de
  comparaison (`/__monolithic/content/{id}`), pas du code à éliminer.
- `content_core_head` et `content_core_tail` sont des noms propres à ce cas, jamais
  une convention obligatoire. Représentation segmentée ≠ « chaque page produit trois
  artefacts ».

Identités : `component_id ≠ ArtifactKey ≠ SourceKey(u16)`, auxquelles s'ajoute
`ProducerKey(u16)` ; aucune n'est dérivée d'une autre.

## 8. Adaptateur HTTP

Dans `crates/shell/server/src/content_document.rs` (`marius-server` ; `marius-render`
n'importe ni `axum`, ni `hyper`, ni `bytes`) :

```text
HTTP → ROUTE_DESCRIPTORS["content_document"]
     → segments statiques : resolve_generation → resolve_range → MmapOwner → Bytes::from_owner
     → segment volatile   : VolatileContext → materialize_volatile → VolatileOwner → Bytes::from_owner
     → somme des longueurs effectives → Content-Length
     → Body → Hyper
```

Toute incohérence répond par un statut contrôlé : 400 (paramètre invalide), 404 (id
absent du pack statique), 500 (incohérence source/sélection, producteur inconnu,
capacité dépassée). Aucun `unwrap` ni `expect` sur la production volatile.

## 9. Garanties démontrées par les tests

- Longueur effective distincte de la capacité ; dépassement de capacité → erreur
  contrôlée sans troncature, y compris lorsque l'échappement HTML provoque le
  dépassement ; capacité exacte acceptée, un octet de moins refusée.
- Ownership : le stockage survit au `MaterializedSource` d'origine via un handle
  cloné ; un instantané n'est pas affecté par une production ultérieure ni par le drop
  du contexte.
- Absence de copie : égalité de pointeur entre le buffer possédé et la plage résolue.
  Ne couvre pas le pointeur interne du `Bytes` construit, que la crate `bytes` ne
  garantit pas.
- Route de bout en bout (`content_document`) : ordre tête → volatile → queue,
  `Content-Length` exact, échappement HTML, 404 sur id absent, 500 sur dépassement
  de capacité.
- Instantané sous rotation : une requête en vol garde sa matérialisation statique et
  volatile après une rotation `ArcSwap` et un changement de producteur (suite de
  fixtures `experimental_volatile_t2a`, `#[cfg(test)]`).
- Le slot généré par la Forge (`ROUTE_DESCRIPTORS`) est matérialisable par le
  producteur `ProducerKey(0)`.

**Portée honnête.** Ces tests établissent que le volatile est matérialisé **avant**
l'envoi des en-têtes et **possédé** par la réponse jusqu'à son drop : une mutation
ultérieure de la source ne change pas les octets de cette réponse. Ils ne prouvent
ni l'entrelacement de requêtes concurrentes, ni le moment où Hyper, Tokio ou l'OS
lisent les frames, ni un comportement de backpressure.

## 10. Hors périmètre

Authentification, session, cookies ; producteur lisant PostgreSQL ; plusieurs
régions ou plusieurs producteurs génériques ; catalogue de producteurs ; stratégie
de notification ou d'invalidation d'un volatile ; pool ou zéro-allocation ;
politique de cache d'une réponse contenant un volatile (elle n'est pas cacheable
comme un pack statique) ; combinaison d'une région volatile avec un champ
`marius:large_content` ; `EmissionPlan`, `IoSlice[]`, `writev`/`sendmsg`,
`MSG_ZEROCOPY` (transport : SPEC T2A v2) ; contrat navigateur.
