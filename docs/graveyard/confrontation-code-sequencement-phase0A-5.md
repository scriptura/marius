# Confrontation au code et séquencement d'implémentation — Phase 0.A → Phase 5

**Statut** : document de confrontation, aucune implémentation effectuée. Fondé sur les fichiers réellement lus cette session (liste en fin de document). Toute affirmation non vérifiable sur le code fourni est marquée `NON DÉTERMINÉ`.

---

## Finalité de la migration

Le runtime actuel sert principalement une réponse HTML comme un artefact unique : une route résout un `PackfileEntry`, puis le serveur lit `(offset, len)` dans le fichier et construit une réponse HTTP possédée.

L'architecture cible d'ADR-011 remplace ce modèle mono-artefact par une représentation AOT ordonnée d'une réponse : une route référence une séquence finie de segments provenant d'un ensemble fini de sources. La Forge détermine cette séquence, les sources et le backend d'émission ; le runtime ne fait que matérialiser la génération publiée et émettre les segments.

La migration vise donc à déplacer progressivement la connaissance de composition de la réponse depuis le runtime vers l'AOT, sans introduire d'interprétation dynamique et en conservant les invariants de publication atomique, déterminisme et absence d'allocation sur le chemin chaud.

**Distinction fondamentale — six notions qui manipulent toutes des couples `offset`/`len`, à ne jamais fusionner pour cette seule raison :**

- `RenderChunk` (ex-`Segment`) : mécanisme interne de production/écriture du HTML pendant le build/render.
- `PackfileEntry` : entrée physique actuelle du pack HTML.
- `SegmentDescriptor` : unité de composition d'une réponse dans le runtime post-ADR-011.
- `SourceKey` : identité globale AOT d'une source.
- `SourceId` : référence locale d'un segment vers une source, dans une route.
- `MaterializedSource` : incarnation runtime d'une source pour la génération publiée.

**Règle de méthode pour toute session future travaillant sur ce document :** le code existant est une preuve de l'état actuel ; il n'est pas la source normative de l'architecture cible. Lorsqu'une structure actuelle semble couvrir partiellement un concept cible (`RouteEntry`, `PackfileEntry`, `LiveRegistry`, `Projection`, etc.), distinguer systématiquement : (1) ce qui peut être réutilisé, (2) ce qui doit être remplacé, (3) ce qui n'est qu'une analogie accidentelle. Le fait qu'une structure existante « ressemble » au concept cible ne justifie jamais son extension par défaut. Les documents post-ADR-011 définissent la cible ; le repository définit seulement l'état de départ.

---

## Phase 0.A — Désambiguïsation `Segment` → `RenderChunk`

### État actuel

`marius_projection::Segment<'a>` (`crates/core/projection/src/lib.rs`) : enum `{ Buffered{start,end}, Borrowed(&'a str) }`, POD, `Copy`. Défini une fois, à un seul endroit.

**Producteurs :**
- `crates/core/projection/src/lib.rs` — implémentation par défaut de `render_segments()` (délègue à `render()` si aucun champ segmenté).
- `crates/forge/db-forge/src/codegen/projection.rs` — `write_projection_stub`, génère le corps réel de `render_segments()` et la constante `MAX_SEGMENTS = 2 * segment_count + 1` quand `has_segment` est vrai. Seul générateur trouvé : `crates/forge/db-forge/src/codegen/mod.rs` ne réexporte qu'un seul générateur de ce type (`write_projection_stub`) parmi six modules (`collector`, `from_impl`, `projection`, `row`, `storage`, `varlen`) — les cinq autres correspondent à des responsabilités nommément distinctes (Collector, impls `From`, ligne fixe, store, varlena). Contenu de ces cinq fichiers non vérifié directement cette session — confiance élevée sur l'absence d'un second site, non absolue.

**Consommateur unique** : `crates/shell/render/src/batch_renderer.rs`, `BatchRenderer::render_batch` — construit `Vec<Segment>` (capacité `P::MAX_SEGMENTS`), consomme immédiatement chaque `Segment` par écriture séquentielle, aplatit en un unique `PackfileEntry`.

**Tests/bancs référençant `Segment`/`MAX_SEGMENTS`/`render_segments`** (exhaustif sur les fichiers fournis) :
- `crates/shell/render/src/dispatcher.rs` — `test_hot_path_pipeline_stress` (module test).
- `crates/shell/render/benches/hot_path_certify.rs` — `bench_certify_zero_alloc`, `bench_certify_zero_alloc_large_body` (assertion `segments.len() == 3`).
- `crates/shell/render/benches/hot_path_render.rs` — `bench_render_single_nominal`, `bench_render_single_worst_case`, `bench_render_segmented_single_large` (assertion `segments.len() == 3`), `bench_render_segmented_sequential_large`.
- `crates/shell/render/benches/counting_alloc.rs` — aucune référence directe (allocateur générique, instrumente indirectement les bancs ci-dessus).

### État cible

Aucun changement architectural — renommage pur : `Segment` → `RenderChunk`, `render_segments()` → `render_chunks()`, `MAX_SEGMENTS` → `MAX_RENDER_CHUNKS`. Objectif unique : libérer le nom `Segment` avant l'introduction de `SegmentDescriptor`.

### Écart

Nominal seulement. Aucun écart sémantique — la Phase 1 du DESIGN a déjà établi (confrontation code, session précédente) que ce mécanisme est indépendant du futur `SegmentDescriptor`.

### Fichiers concernés

`crates/core/projection/src/lib.rs`, `crates/forge/db-forge/src/codegen/projection.rs`, `crates/shell/render/src/batch_renderer.rs`, `crates/shell/render/src/dispatcher.rs` (module test), `crates/shell/render/benches/hot_path_certify.rs`, `crates/shell/render/benches/hot_path_render.rs`. À vérifier par grep avant exécution : `crates/forge/db-forge/src/codegen/{collector,from_impl,row,storage,varlen}.rs` (non lus cette session).

### Dépendances

Aucune — peut s'exécuter indépendamment de tout le reste.

### Modifications envisagées

Renommage mécanique des trois symboles, dans les six fichiers listés, y compris les messages d'assertion et commentaires qui les nomment explicitement (plusieurs commentaires citent `Segment::Borrowed`, `MAX_SEGMENTS` dans leur texte — à mettre à jour pour éviter une documentation obsolète immédiatement après le renommage).

### Risques

Faible — renommage sans changement de type ni de logique. Risque principal : un site non détecté dans les cinq fichiers `codegen/*.rs` non vérifiés cette session.

### Tests / vérifications

Tous les tests/bancs listés doivent compiler et passer inchangés après renommage (aucune assertion de valeur ne doit changer, seulement les noms de symboles). `cargo build --workspace` doit passer sans avertissement de nom inconnu.

### Critère de sortie

Aucun symbole du mécanisme `marius_projection::Segment` ne subsiste sous son ancien nom : aucun `Segment` de ce type précis, aucun `render_segments`, aucun `MAX_SEGMENTS` correspondant à `large_content`. Tous les tests listés passent. Une occurrence ultérieure de `SegmentDescriptor`, ou d'un `Segment` appartenant à un autre domaine, ne constitue pas une violation de ce critère — la vérification est sémantique (ce mécanisme précis a disparu), pas une règle lexicale globale interdisant le mot « Segment » dans tout le projet.

---

## Phase 0.B — Pré-alignement du pack HTML sur l'émission mmap

### État actuel

**Layout physique** (`pack_html_format.rs`) : format *bottom-up*. Blob HTML contigu à partir de l'offset 0 → padding (0-7 octets, alignement 8) → index physique (`PackfileEntry[]`, 24 octets/entrée, trié par `id` ASC) → footer fixe (32 octets, magic `MARIUSPK`, toujours aux 32 derniers octets du fichier). `offset` dans chaque `PackfileEntry` est **absolu au fichier entier**, pas relatif à une région. Aucune métadonnée entre index et payload autre que le padding d'alignement. Le payload est contigu et directement mmap-able (aucun octet de contrôle intercalé). Les offsets sont stables après publication : `apply_merge_io_sync` (`regenerate.rs`) n'écrit jamais sur `final_path` directement — tout se construit dans `tmp_path`, puis `fs::rename` atomique ; `final_path` n'est plus jamais rouvert en écriture après coup.

**`PackHtmlIndex`** (`pack_html_index.rs`) possède : un `std::fs::File` (fd ouvert, jamais `seek()`), un `Option<memmap2::Mmap>` **borné exactement à la région d'index** (jamais au blob — choix de conception explicite, commenté : « le blob HTML n'entre jamais dans l'espace d'adressage virtuel de ce processus »), et `entry_count`. Relation avec `PackfileEntry` : l'index mmap'd est casté directement en `&[PackfileEntry]` via `bytemuck::cast_slice`, zéro-copie. `lookup()` retourne `(offset, len)`, jamais les octets. Les entrées peuvent être transformées directement en ranges dans un mapping complet **sans adaptation** : offsets absolus + payload contigu + stabilité post-publication rendent une opération `full_mmap[offset..offset+len]` directement valide.

**Publication/générations** (`registry.rs`, `regenerate.rs`) : `LiveRegistry { indices: HashMap<&'static str, ArcSwap<PackHtmlIndex>> }`, topologie figée à la construction. `store()` remplace l'`Arc` atomiquement. La propriété demandée par la mission est **déjà vérifiée aujourd'hui, mais par le mécanisme de fd, pas de mmap** : `apply_merge_io_sync` ouvre son propre mmap temporaire de l'ancien fichier (`memmap2::Mmap::map(old_file)`, via `old.file()`) uniquement pour la durée de la fusion, jamais stocké dans le registre — puis construit le nouveau fichier intégralement dans `tmp_path`, `fsync`, `rename` atomique. Toute requête en vol détient un `Arc<PackHtmlIndex>` cloné, donc un fd ouvert sur l'ancien inode ; un `rename()` POSIX sur le même système de fichiers ne détruit pas l'ancien inode tant qu'une référence (fd **ou** mmap) le retient — propriété garantie par le noyau, pas par une discipline de code. Testé explicitement (`registry.rs::concurrent_readers_never_observe_a_torn_read_during_swaps`, `main.rs::jalon3_concurrent_store_during_live_serving_never_serves_torn_fragment`). Cette même garantie POSIX s'appliquerait identiquement à un mmap complet du fichier (le mapping retient l'inode indépendamment de l'entrée de répertoire) — aucun changement d'invariant nécessaire pour l'étendre, seulement une extension de la région actuellement mmap'd.

**HTTP** (`handlers.rs`) : `serve_route` → `registry.load()` → `index_arc.lookup(id)` → `deliver(index_arc, offset, len)` → `spawn_blocking(|| index.file().read_at(&mut buf, offset))` → `Vec<u8>` → `Response` Axum (corps possédé, `'static`). Aucun body Axum/Hyper de bas niveau utilisé — construction via le tuple `(headers, bytes)` implémentant `IntoResponse`. Aucune slice empruntée n'est actuellement retournée : la durée de vie du corps de réponse ne dépend d'aucun `Arc`/mmap, elle est possédée.

### État cible

```text
HTML pack
    → mmap read-only (blob + index, ou blob seul en complément de l'index déjà mmap'd)
    → SegmentDescriptor { offset, len }
    → slice mémoire directement adressable, sans read_at ni copie
```

**Périmètre strict de cette phase — à ne pas dépasser** : 0.B produit une **source mmap runtime sûre et durable**, rien de plus. Elle doit : conserver dans chaque génération publiée le mapping permettant d'adresser le blob ; fournir une primitive interne sûre (ex. une méthode retournant `Option<&[u8]>` ou un type porteur équivalent) pour lire une plage `(offset, len)` directement depuis ce mapping ; démontrer par un test que cette plage reste valide à travers un `store()` concurrent (extension du mécanisme de génération déjà garanti aujourd'hui par le fd). 0.B ne doit pas : résoudre le problème du corps de réponse Axum/Hyper, modifier `deliver()` pour retourner directement une slice au réseau, ni décider entre `Bytes::from_owner`, un body custom, ou toute autre adaptation HTTP. Cette adaptation appartient exclusivement à la Phase 5, une fois `IoSlice`/le backend réseau en jeu — l'introduire ici mélangerait une décision de modèle mémoire (stable, indépendante du framework HTTP) avec une décision d'intégration réseau (encore ouverte, cf. Phase 5).

### Écart

Un seul écart réel dans le périmètre de cette phase : le blob n'est aujourd'hui **jamais** mmap'd de façon persistante — choix de conception explicite et documenté (`pack_html_index.rs`), pas un oubli. L'étendre change une décision existante, pas seulement une lacune. Le second écart identifié lors de la première rédaction de ce document (absence de mécanisme pour un corps de réponse emprunté) est réel mais **hors périmètre de 0.B** — reporté explicitement à la Phase 5 (voir ci-dessus).

### Fichiers concernés

`pack_html_index.rs` (extension de la région mmap'd, ou mapping complémentaire du blob, nouvelle primitive d'accès `(offset, len) → &[u8]`). `handlers.rs` n'est **pas** modifié par cette phase — il reste lu à titre de contexte (comportement actuel à préserver), sa modification appartient à la Phase 5.

### Dépendances

Aucune sur la Phase 0.A. Bloque en revanche toute la suite (Phases 3-5) : `SegmentDescriptor`/`MaterializedSource` présument une source mmap déjà disponible pour le cas statique.

### Modifications envisagées

1. `PackHtmlIndex::open()` : mmap la région `[0..index_start)` (blob) en plus de la région d'index déjà mmap'd — soit deux mappings distincts (préserve la séparation conceptuelle actuelle), soit un seul mapping `[0..footer_start)` couvrant les deux (plus simple, coût mémoire identique puisque le blob est de toute façon dans le page cache).
2. Une nouvelle méthode sur `PackHtmlIndex` retournant `&[u8]` directement depuis le mapping pour une plage `(offset, len)` donnée — remplace l'usage de `lookup()` seul pour les futurs appelants internes, sans toucher à `lookup()` lui-même (toujours nécessaire pour la résolution `id → (offset, len)`).
3. **Non inclus dans cette phase** : toute modification de `handlers.rs`/`deliver()`, tout choix de type de corps HTTP — reporté à la Phase 5.

### Risques

- **Lifetime** : la nouvelle primitive d'accès `(offset, len) → &[u8]` doit être liée par le type à la durée de vie du mapping (donc de l'`Arc<PackHtmlIndex>`) — risque de use-after-free si mal conçu, mitigé par Rust, mais la signature exacte reste à concevoir. La question, distincte, de faire survivre cette slice jusqu'à l'émission réseau complète est hors périmètre (Phase 5).
- **Concurrence** : aucun risque nouveau — la propriété de génération est déjà garantie par le mécanisme `Arc`/fd existant, étendue au mmap sans changement de nature.
- **Performance** : `spawn_blocking` disparaît potentiellement du chemin (un accès mmap déjà résident en page cache est un accès mémoire, pas un appel système) — mais un défaut de page sur une région froide reste bloquant pour le thread ; la disparition de `spawn_blocking` change la nature du risque (page fault non enveloppé) sans l'éliminer. À trancher explicitement en Phase 5, pas silencieusement en Phase 0.B.
- **Compatibilité** : aucune, le format sur disque ne change pas.

### Tests / vérifications

Préserver tous les tests existants de `pack_html_index.rs` (footer corrompu, magic invalide, mmap borné à l'index). Nouveaux tests nécessaires : lecture d'une slice mmap'd du blob identique à la lecture `read_at` actuelle (non-régression fonctionnelle) ; survie d'une slice empruntée à travers un `store()` concurrent (extension du test `concurrent_readers_never_observe_a_torn_read_during_swaps` existant).

### Verdict Phase 0.B

**B — mmap complet possible, mais nécessite une adaptation préalable.**

Pas A : le mmap restreint à l'index est une décision de conception explicite à réviser consciemment (pas un simple manque), et le handler HTTP n'a aujourd'hui aucun mécanisme pour retourner un corps emprunté — écart réel, pas seulement absent.
Pas C : le format sur disque (offsets absolus, payload contigu, stabilité post-publication, sémantique POSIX de rename-sur-fichier-ouvert) supporte nativement un mmap complet sans aucune modification du format binaire.

---

## Phase 1 — `SourceKey` et catalogue des sources

### État actuel

`packfile_key: &'static str` (`RouteEntry`, `registry.rs`) joue déjà, en production et testé sous charge, le rôle que `SourceKey` doit occuper : identifiant stable d'un artefact, utilisé comme clé de `LiveRegistry: HashMap<&'static str, ArcSwap<PackHtmlIndex>>`. Également utilisé côté écriture (`Dispatcher.packfile_key`, `dispatcher.rs`), **délibérément non dérivé** de `P::packfile_path()` (commentaire explicite, `dispatcher.rs` lignes 90-93) — un découplage déjà pratiqué entre l'identité de type `Projection` et la clé de routage.

### État cible

```text
nom textuel / configuration
        ↓ build-time
SourceKey(u16)
        ↓
catalogue AOT des sources
        ↓
runtime SourceId local (indice dans RouteDescriptor.sources)
```

### Écart

**Hypothèse architecturale retenue : `SourceKey(u16)`.** Cette phase ne cherche pas à trancher « chaîne ou `u16` ? » depuis zéro — ce choix a déjà été arbitré dans le DESIGN. Elle doit seulement vérifier que l'implémentation existante et les contraintes réelles du repository ne révèlent pas un motif qui invaliderait ce choix. Une révision de l'architecture n'est permise qu'en présence d'une incompatibilité démontrée ; le simple fait que `HashMap<&str, …>` fonctionne actuellement ne constitue pas un motif suffisant pour rouvrir le débat.

Le `HashMap<&'static str, ArcSwap<PackHtmlIndex>>` actuel (`LiveRegistry`) est une structure de **registre/runtime** — elle répond à « quel est l'index actuellement publié pour cette clé ». `SourceKey` est une composante de l'**IR AOT** — elle répond à « quelle est l'identité stable de cette source, décidée à la compilation ». Les deux peuvent coexister : `SourceKey(u16)` comme identifiant transporté dans `SegmentDescriptor`/`SourceSpec`, converti (à la construction du registre, pas par requête) vers la clé que `LiveRegistry` utilise réellement en interne — chaîne ou `u16`, cette conversion reste un détail d'implémentation du registre, pas une propriété de l'IR. C'est cette distinction, pas une mesure de performance, qui justifie de conserver `u16` au niveau de l'IR indépendamment du mécanisme interne du registre.

Le code existant confirme par ailleurs (`dispatcher.rs`) qu'un découplage entre identité de type et clé de routage est déjà pratiqué (`packfile_key` volontairement non dérivé de `P::packfile_path()`) — un précédent favorable à l'introduction d'un identifiant distinct de la représentation interne du registre.

### Fichiers concernés

`registry.rs` (`LiveRegistry`, `RouteEntry.packfile_key`), `dispatcher.rs` (`Dispatcher.packfile_key`, `ShardMetadata.packfile_key`), `main.rs` (`ROUTE_TABLE`, `SHARDS`, chaînes littérales).

### Dépendances

Nécessite Phase 2 (catalogue AOT des routes) pour avoir un point de génération unique où attribuer les valeurs `u16` — attribuer `SourceKey` isolément, avant que ce catalogue existe, recréerait une seconde source de vérité (chaîne à la main aujourd'hui, `u16` à la main demain) sans résoudre le problème de fond.

### Modifications envisagées

Aucune à ce stade — cette phase est analytique (déterminer ce qui appartient au build-time vs au runtime), pas une modification de code. Ce qui appartient au build-time : l'attribution du `u16` et sa stabilité inter-régénérations. Ce qui reste au runtime : rien de nouveau — `LiveRegistry` continuerait de fonctionner par indexation, `u16` ou chaîne. Comment gérer les collisions : `NON DÉTERMINÉ` — dépend du mécanisme de génération du catalogue (Phase 2/Forge), pas encore spécifié. Diagnostic : conserver `packfile_key: &'static str` comme métadonnée de debug à côté de `SourceKey(u16)`, jamais sur le chemin chaud.

### Risques

Risque principal : sur-ingénierie prématurée si `u16` remplace `&'static str` avant d'avoir mesuré que le hachage de chaîne coûte réellement quelque chose sur ce chemin (une seule résolution par requête, pas par segment, contrairement à l'hypothèse initiale du DESIGN).

### Tests / vérifications

Aucun nouveau test à ce stade (phase analytique).

### Critère de sortie

`SourceKey(u16)` confirmé compatible avec le registre existant (aucune incompatibilité démontrée), avec un mécanisme de conversion `SourceKey → clé de registre` défini au moins en esquisse. Le nom textuel (`packfile_key`) reste disponible à des fins de diagnostic, jamais sur le chemin chaud.

---

## Phase 2 — `RouteEntry` → `RouteDescriptor`

### État actuel

`RouteEntry { pattern: &'static str, packfile_key: &'static str, id_source: IdSource, content_type: &'static str }`. Écrit à la main dans `main.rs` (`static ROUTE_TABLE`). Consommé par `build_router` (montage Axum, un `Extension(entry)` par route) et par `handlers::serve_route` (résolution `id`/`packfile_key`). Aucun mécanisme de génération AOT n'existe : le commentaire de `main.rs` le confirme explicitement (« le compilateur de templates de pages qui générerait cette table n'existe pas — hors périmètre »).

### État cible

```rust
pub struct RouteDescriptor {
    pub segments: &'static [SegmentDescriptor],
    pub sources: &'static [SourceSpec],
    pub backend_kind: EmissionBackendKind,
    pub volatile_capacity: u32,
}
```

produit par la Forge, pas écrit à la main.

### Écart

**`RouteDescriptor` est le descripteur AOT de représentation, pas nécessairement l'ensemble des métadonnées nécessaires au montage HTTP.** Les métadonnées de routage/protocole peuvent être portées par une structure d'adaptation distincte qui référence un `RouteDescriptor` — l'architecture ne demande pas que `pattern`, `content_type`, le parsing des paramètres HTTP, etc. soient absorbés dans le modèle d'émission. **Ne pas inventer à ce stade un nouveau nom de wrapper ni modifier la structure cible sans nécessité démontrée** — le rôle de cette phase est de cartographier l'écart, pas de concevoir la structure d'adaptation.

1. **Champs actuels qui disparaissent** : aucun à proprement parler — `pattern` et `content_type` restent nécessaires côté adaptateur HTTP (montage Axum, en-tête `Content-Type`), mais n'apparaissent pas dans `RouteDescriptor` tel que spécifié — ils continuent de vivre dans une structure adjacente, pas absorbés par le contrat AOT.
2. **Champs qui deviennent des données AOT** : `segments`/`sources`/`backend_kind`/`volatile_capacity` n'ont aujourd'hui aucun équivalent — le modèle actuel est mono-artefact (un seul `packfile_key` par route), le futur modèle est multi-source.
3. **Champs qui restent nécessaires à l'adaptateur HTTP** : `pattern` (montage Axum), `content_type` (en-tête), `id_source` (résolution de paramètre — mécanisme sans équivalent direct dans `RouteDescriptor`, à clarifier : soit c'est déjà capturé implicitement par la résolution de `SourceId`, soit il faut un champ supplémentaire dans la structure d'adaptation, pas dans `RouteDescriptor` lui-même).
4. **Où la route est actuellement résolue** : à la main dans `main.rs`, jamais générée.
5. **Où la Forge doit produire la future table** : `NON DÉTERMINÉ` — aucun générateur de routes n'existe dans `db-forge`/`fragment-forge` à ce jour (vérifié par le commentaire de `main.rs` et l'absence d'un tel module dans l'arborescence de `crates/forge/`).
6. **Coexistence `RouteEntry`/`RouteDescriptor`** : risque réel — `handlers.rs` et `main.rs` dépendent aujourd'hui de `RouteEntry` pour fonctionner ; une migration big-bang casserait le chemin HTTP actuel. Une coexistence transitoire (les deux types, une fonction de conversion) est probable, à contenir dans le temps.

### Fichiers concernés

`registry.rs` (définition de `RouteEntry`/`IdSource`), `main.rs` (`ROUTE_TABLE`, `build_router`), `handlers.rs` (`serve_route`, dépend de `RouteEntry` via `Extension`).

### Dépendances

Nécessite Phase 1 (catalogue `SourceKey`) pour que `sources: &'static [SourceSpec]` référence quelque chose de stable. Nécessite Phase 0.B pour que `StaticArtifact` dans `SourceSpec` ait un sens opérationnel (source mmap disponible).

### Modifications envisagées

Analytique à ce stade selon la mission — pas de modification proposée, seulement la cartographie ci-dessus.

### Risques

Absence de générateur Forge pour les routes : la Phase 2 ne peut pas se limiter à changer un type de données, elle implique de construire un nouveau composant (générateur AOT de `ROUTE_TABLE`/`RouteDescriptor[]`) qui n'existe pas embryonnairement aujourd'hui — risque de sous-estimation de l'effort si traité comme un simple renommage de structure.

### Tests / vérifications

Les tests existants de `main.rs` (`jalon3_serves_200_404_400_correctly`, etc.) doivent continuer de passer à l'identique tant que `RouteEntry` reste le type consommé par `handlers.rs` — aucune régression tolérée pendant la période de coexistence.

### Critère de sortie

`RouteDescriptor` défini, un mécanisme (même minimal, pas nécessairement un générateur complet) produit au moins une route de test, `handlers.rs` peut consommer soit l'un soit l'autre sans dupliquer la logique de résolution.

---

## Phase 3 — `SegmentDescriptor` / budgets / backend AOT

### État actuel

Aucun équivalent. Le plus proche est `PackfileEntry{id, offset, len, _pad}` — un seul par enregistrement, jamais composé avec d'autres pour une même réponse.

### État cible

`SegmentDescriptor{source: SourceId, offset: u64, len: u32, flags: SegmentFlags}`, budget vérifié par la Forge contre `IOV_MAX`, `EmissionBackendKind` (SingleFile/Scatter) décidé par la Forge par route.

### Écart

Complet — aucune des trois briques (`SegmentDescriptor`, vérification `IOV_MAX` au build, sélection `EmissionBackendKind`) n'existe. `MAX_RENDER_CHUNKS` (ex-`MAX_SEGMENTS`, Phase 0.A) reste un mécanisme local au rendu `large_content`, sans lien avec ce budget — à garder distinct explicitement dans le code comme dans la documentation, pas seulement dans le vocabulaire.

### Fichiers concernés

Nouveau code exclusivement — pas de fichier existant à modifier pour introduire les types eux-mêmes. `crates/forge/db-forge/src/codegen/` (ou un nouveau module Forge) serait le site naturel de la vérification `IOV_MAX`/calcul du budget, par analogie avec `write_projection_stub` qui calcule déjà `MAX_RENDER_CHUNKS` au même endroit conceptuel (build-time, généré).

### Dépendances

Phase 0.B (source mmap disponible), Phase 1 (`SourceId`/`SourceKey` stabilisés), Phase 2 (`RouteDescriptor` comme contenant).

### Modifications envisagées

Analytique — la mission exclut explicitement toute heuristique runtime ; tout doit être calculé et vérifié à la compilation, sur le modèle déjà pratiqué par `write_projection_stub` pour `MAX_RENDER_CHUNKS`.

### Risques

Aucun générateur de graphe de segments par route n'existe (même remarque que Phase 2, point 5) — risque de sous-estimation similaire.

### Tests / vérifications

`NON DÉTERMINÉ` — dépend de la forme exacte du générateur, non encore conçu.

### Critère de sortie

Un `SegmentDescriptor[]` peut être produit pour au moins une route réelle (probablement `/content/{id}`, la seule route active aujourd'hui selon `main.rs`), avec vérification `IOV_MAX` fonctionnelle au `cargo build`.

---

## Phase 4 — `MaterializedSource` / `EmissionPlan`

### État actuel

Aucun équivalent — `LiveRegistry::load()` retourne directement un `Arc<PackHtmlIndex>` concret, jamais une résolution vers un enum fermé de variantes. Aucun contenu volatil n'existe dans le code (aucune trace de session, panier, notification).

### État cible

Résolution `SourceId → MaterializedSource` par requête, assemblage en `EmissionPlan<'req>`, séparation stricte AOT (`RouteDescriptor`)/runtime (`MaterializedSource`, dépend de la génération publiée).

### Écart

Complet. Un point positif néanmoins : `LiveRegistry::load()` fournit déjà exactement le mécanisme de résolution « une génération par accès » que `MaterializedSource::Mmap` présuppose — la Phase 4 consiste largement à envelopper ce mécanisme existant dans le nouveau type, pas à le réinventer.

### Fichiers concernés

Nouveau code. `registry.rs::LiveRegistry::load()` serait le point d'appel réutilisé tel quel pour la variante `Mmap`.

### Dépendances

Phase 3 (`SegmentDescriptor`/`SourceSpec` doivent exister pour avoir quelque chose à résoudre).

### Modifications envisagées

Analytique. Données réellement request-locales : la résolution elle-même (quel `Arc` est actuellement publié) et tout contenu volatil futur. Données restant statiques : `RouteDescriptor` dans son intégralité. Dépendant de la génération du pack : uniquement la variante `Mmap` de `MaterializedSource`. Garantie de lifetime : héritée du mécanisme `Arc`/`ArcSwap` déjà en place (Phase 0.B).

### Risques

`RequestArena` n'a aucun embryon dans le code actuel — sa conception (allocation bornée par requête) est un travail neuf complet, pas une extension.

### Tests / vérifications

`NON DÉTERMINÉ` — dépend de la conception de `RequestArena`, non commencée.

### Critère de sortie

`EmissionPlan` peut être construit pour une requête réelle sur `/content/{id}`, exclusivement à partir de sources `Mmap` (aucun volatil requis pour cette phase).

---

## Phase 5 — HTTP / `IoSlice` / `writev`

### État actuel

`Cargo.toml` racine : `axum = "0.8"` en dépendance de workspace. **Aucune dépendance directe à `hyper`** dans les deux `Cargo.toml` fournis (`crate_shell_server_Cargo.toml` ne liste que `axum`, `tokio`, `sqlx`, `phf` — `hyper` n'existe qu'en dépendance transitive d'Axum). Chemin actuel entièrement à l'intérieur de l'abstraction haut niveau d'Axum (`axum::routing::get`, `Router`, `.layer(Extension(...))`, `.with_state(...)`) — confirmé sur `main.rs`/`handlers.rs` cette session. Aucun accès à `hyper::server::conn` ni `hyper::upgrade` nulle part dans le code fourni.

### État cible

```text
RouteDescriptor → SegmentDescriptor[] → MaterializedSource → IoSlice[] → adapter Hyper/Axum → writev/sendmsg
```

### Écart

Total, et la nature exacte de l'écart est maintenant précisée par ce lot : le projet n'a **aucun point d'entrée bas niveau existant** vers le socket — toute la cartographie Hyper/Axum (déjà identifiée comme chantier séparé) partira de zéro, pas d'un accès déjà entamé. `axum = "0.8"` étant confirmé, la vérification de ce que cette version expose exactement (accès direct au socket via `hyper::upgrade`, ou nécessité de descendre à une dépendance directe sur `hyper` 1.x) reste `NON DÉTERMINÉ` sans consultation de la documentation Axum 0.8/Hyper correspondante — hors périmètre de cette confrontation au code seul.

### Fichiers concernés

`handlers.rs` (changement de signature de retour), `main.rs` (montage éventuellement différent si le bas niveau est requis), `crate_shell_server_Cargo.toml` (ajout probable d'une dépendance directe `hyper`).

### Dépendances

Phase 0.B (le corps de réponse doit déjà savoir porter un `Arc`/slice empruntée avant de parler de `writev`), Phase 4 (`EmissionPlan` doit exister pour avoir quelque chose à convertir en `IoSlice[]`).

### Modifications envisagées

Analytique — la mission exclut explicitement `MSG_ZEROCOPY` de cette phase. Allocations hors chemin chaud restant acceptables : construction ponctuelle de la table de routes au démarrage (`cold_start`, déjà le cas aujourd'hui), pas de restriction nouvelle à ce niveau.

### Risques

Le plus significatif de toutes les phases : version exacte d'Axum/Hyper et API d'accès bas niveau `NON DÉTERMINÉ` sans lecture de documentation externe — à traiter en tout début de cette phase, avant toute autre décision, conformément à la recommandation déjà faite en fin de session précédente (chantier hyper/Axum).

### Tests / vérifications

Tous les tests d'intégration existants (`jalon3_*`, `main.rs`) doivent continuer de passer — ils exercent le comportement observable (codes HTTP, corps de réponse), pas le mécanisme d'émission interne, donc a priori indépendants de cette migration si le comportement externe est préservé.

### Critère de sortie

Une requête sur `/content/{id}` sans contenu volatil est servie via `writev`/`sendmsg` (ou `sendfile` court-circuité, selon `EmissionBackendKind::SingleFile`), avec les mêmes codes de retour et corps qu'aujourd'hui, mesurée sans régression de débit par rapport à la mesure actuelle (`Vec<u8>`+`read_at`).

---

# Ordre d'implémentation recommandé

```text
Phase 0.A
    ↓
Phase 0.B
    ↓
Phase 1
    ↓
Phase 2
    ↓
Phase 3
    ↓
Phase 4
    ↓
Phase 5
```

- **0.A → 0.B** : le renommage ne dépend de rien et ne bloque rien ; il est placé en premier uniquement pour éviter que tout code écrit ensuite (0.B et au-delà) ne doive choisir entre les deux noms de `Segment` en même temps qu'il introduit le nouveau.
- **0.B → 1** : la Phase 0.B valide et prépare le modèle physique de la source statique sur lequel `SourceSpec::StaticArtifact` reposera. Elle ne constitue pas une dépendance conceptuelle de `SourceKey` lui-même (`SourceKey` existe indépendamment du mmap, comme identifiant AOT) — mais elle évite de concevoir le catalogue AOT en faisant abstraction de la matérialisation réelle des artefacts. Ce qui dépend du modèle de matérialisation, c'est `SourceSpec::StaticArtifact` et les phases suivantes, pas `SourceKey`.
- **1 → 2** : `RouteDescriptor.sources` a besoin d'un type de clé stabilisé avant que la structure qui le contient soit figée — changer `SourceKey` après coup romprait `RouteDescriptor`.
- **2 → 3** : `SegmentDescriptor.source: SourceId` est un indice local au `RouteDescriptor` — sans `RouteDescriptor` déjà défini, `SourceId` n'a pas de tableau où se résoudre.
- **3 → 4** : `MaterializedSource` résout des `SourceSpec` qui doivent déjà exister et être vérifiés (budget, `IOV_MAX`) avant qu'on tente de les résoudre à la requête — résoudre quelque chose de non encore borné réintroduirait une vérification runtime que la Forge doit avoir déjà faite.
- **4 → 5** : `IoSlice[]` se construit depuis `EmissionPlan`, qui n'existe qu'après la Phase 4 — écrire le backend réseau avant n'aurait rien à consommer.

---

# Points bloquants éventuels

1. **Cinq fichiers `codegen/*.rs` non lus** (`collector.rs`, `from_impl.rs`, `row.rs`, `storage.rs`, `varlen.rs`) — bloque la certitude absolue du critère de sortie de Phase 0.A (un site de `Segment`/`MAX_SEGMENTS` pourrait s'y trouver, bien que peu probable au vu des noms de responsabilités).
2. **Absence de tout générateur Forge pour les routes** — bloque une estimation réaliste de l'effort des Phases 2 et 3 tant que ce composant n'est pas au moins esquissé ; actuellement `NON DÉTERMINÉ` au-delà du constat de son absence.
3. **Version exacte d'Axum/Hyper et surface d'API bas niveau disponible** — bloque toute planification fine de la Phase 5 ; `Cargo.toml` confirme seulement `axum = "0.8"`, sans dépendance directe `hyper`.
4. **Mécanisme exact du corps de réponse Axum portant un `Arc` + slice empruntée** (Phase 0.B) — `NON DÉTERMINÉ`, nécessite soit une recherche dans l'écosystème `bytes`/Axum, soit un prototype, avant de committer la Phase 0.B.

---

# Décisions déjà considérées comme acquises

1. `Segment` actuel → `RenderChunk`.
2. `render_segments()` → `render_chunks()`.
3. `MAX_SEGMENTS` → `MAX_RENDER_CHUNKS`.
4. `2*N+1` reste un budget local au rendu `large_content` — confirmé par le code cette session (`codegen/projection.rs`), sans lien structurel avec le budget HTTP.
5. `SourceKey` cible = `u16` — hypothèse architecturale retenue ; Phase 1 vérifie l'absence d'incompatibilité, ne rouvre pas le choix sans preuve démontrée.
6. `RouteDescriptor` est la cible et doit remplacer progressivement `RouteEntry`.
7. Le catalogue des sources et les routes doivent être produits par Forge/AOT — actuellement absent, à construire, pas à adapter depuis un mécanisme existant.
8. `EmissionBackendKind` est une décision AOT.
9. Les limites IOV doivent être vérifiées au build-time.
10. `MSG_ZEROCOPY` est hors périmètre initial.
11. Le zéro-allocation et le zéro-copie réseau sont deux objectifs distincts.
12. Le trait `Projection` ne doit pas être refactoré pendant cette séquence — confirmé à cinq responsabilités réelles cette session (extraction SQL, lecture store, rendu, chemins d'artefacts, identité de monomorphisation pour `Dispatcher`), dette actée, non traitée ici.

---

# Fichiers consultés cette session

`lib.rs` (crates/core/projection), `store_registry.rs`, `projection.rs` (codegen db-forge), `batch_renderer.rs`, `dispatcher.rs`, `handlers.rs`, `main.rs`, `pack_html_format.rs`, `pack_html_index.rs`, `registry.rs`, `hot_path_certify.rs`, `hot_path_render.rs`, `counting_alloc.rs`, `mod.rs` (codegen db-forge), `packfile_builder.rs`, `dumper.rs`, `regenerate.rs`, `ingest_and_swap.rs`, `Cargo.toml` (racine), `Cargo.toml` (crates/shell/server).
