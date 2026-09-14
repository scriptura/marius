# Handoff — Implémentation de la frontière transport T2A

**Destinataire :** une session Claude sans accès à l'historique de délibération ayant produit ce document.
**Prérequis de lecture avant tout code :** `docs/specifications/SPEC-transport-segmente-t2a.md` (normatif, à lire en entier avant de commencer).
**Discipline de ce document :** chaque affirmation est taguée `[FAIT]` (observé dans le code réel), `[DÉCISION]` (acté, ne pas rouvrir sans raison forte), `[INVARIANT]` (contrainte à respecter dans l'implémentation), `[PROVISOIRE]` (choix acceptable pour ce seul incrément, pas une architecture finale), ou `[OUVERT]` (non tranché, à ne pas trancher prématurément).

**Statut :** v2 corrective (sémantique segment/source, point d'intégration expérimental, critère de non-copie, portée de `Content-Length`, statut de `Body::from_stream`).

---

## A. Contexte architectural minimal

`[FAIT]` Marius est un moteur de rendu AOT : PostgreSQL est la source de vérité, la Forge (`crates/core/schema/build/`, `crates/forge/db-forge/`) produit des artefacts au moment du build, le runtime exécute un plan préétabli sans reconstruire de page.

`[FAIT]` Deux familles d'émission coexistent architecturalement (cf. SPEC §1) :
- **AOT monolithique** : une page = une source mmap contigüe, chemin mono-source direct, objectif zéro-allocation préservé.
- **AOT segmentée** : une réponse composée de **plusieurs segments, éventuellement issus de plusieurs sources**, résolus puis assemblés à la requête — c'est la famille concernée par ce handoff.

`[FAIT]` Vocabulaire minimal nécessaire, déjà défini dans `core_projection_src_lib.rs` :
- `SourceKey` : identité globale d'une source dans le registre — base de toute déduplication/cohérence de génération.
- `SourceId` : référence locale à une route (index dans `RouteDescriptor.sources`) — jamais utilisé pour la cohérence ou la déduplication.
- `SourceSpec` : description AOT d'une source (mmap ou `VolatileSlot`).
- `SegmentDescriptor` : élément AOT décrivant un segment d'une route (`source`, `selection`, `flags`) — ne porte pas de plage physique. **Un segment est l'unité d'émission ; plusieurs `SegmentDescriptor` d'une même route peuvent référencer le même `SourceKey`** — ne pas confondre le nombre de segments (`K`) avec le nombre de sources distinctes.
- `ResolvedRange<'a>` : plage mémoire empruntée `(bytes: &'a [u8])`, obtenue après résolution — dernier niveau de représentation propre à Marius (SPEC §2).

`[DÉCISION]` `EmissionPlan` (type déjà existant dans `emission.rs`, const-generic sur `K`) **n'est pas conservé comme IR**. Il ne doit pas être instancié, ni recréé sous un autre nom. Sa non-nécessité a été établie par analyse préalable, hors périmètre de ce handoff — ne pas la rouvrir sans raison démontrée.

---

## B. État réel actuel du code

`[FAIT]` Chemin HTTP réel aujourd'hui, vérifié par lecture directe des fichiers listés :

```text
HTTP request
    → Axum Router (crates/shell/server/src/main.rs::build_router)
    → serve_route() (crates/shell/server/src/handlers.rs) — fonction UNIQUE, générique,
      montée pour toutes les routes de ROUTE_TABLE, différenciée par
      Extension<&'static RouteEntry> (valeur runtime, pas un type par route)
    → IdSource::{Fixed,PathParam} → id: i64
    → LiveRegistry::load(route.packfile_key) (crates/shell/render/src/registry.rs)
    → PackHtmlIndex::lookup(id) (crates/shell/render/src/pack_html_index.rs) → (offset, len)
    → deliver(index, offset, len) (handlers.rs)
        → spawn_blocking { index.file().read_at(&mut buf, offset) } → Vec<u8>
    → Response (Vec<u8> possédé)
```

`[FAIT]` Fichiers directement concernés par ce chemin :
- `crates/shell/server/src/main.rs` — accept loop (Hyper/hyper-util depuis Phase 5), montage du `Router`.
- `crates/shell/server/src/handlers.rs` — `serve_route`, `deliver`, `serve_asset`.
- `crates/shell/render/src/registry.rs` — `LiveRegistry`, `RouteEntry`, `IdSource`.
- `crates/shell/render/src/pack_html_index.rs` — `PackHtmlIndex::lookup`, `::blob`, mapping mmap.
- `crates/shell/render/src/emission.rs` — primitives de segmentation (`SegmentDescriptor` consommé via `core_projection`, `ResolvedRange`, `RequestArena`, `EmissionPlan`, `SourceResolutionContext`, `resolve_generation`, `resolve_range`).
- `crates/core/projection/src/lib.rs` — `SourceKey`, `SourceId`, `SourceSpec`, `SegmentDescriptor`, `SegmentSelection`, `SegmentFlags`, `RouteDescriptor`, `EmissionBackendKind`, trait `Projection` (`MAX_RENDER_CHUNKS`).

`[FAIT]` Constats critiques, vérifiés par grep exhaustif sur les fichiers ci-dessus :
- `PackHtmlIndex::blob()` existe, est zéro-copie/zéro-syscall (tranche empruntée sur mapping résident) — **mais n'est utilisé par aucun chemin HTTP réel**. `deliver()` utilise `read_at` (pread), pas `blob()`.
- Le chemin réel produit toujours un `Vec<u8>` par requête (`deliver`, ligne `let mut buf = vec![0u8; len as usize]`).
- `SegmentDescriptor`, `RouteDescriptor`, `ResolvedRange`, `RequestArena`, `EmissionPlan` existent, compilent, sont testés — **zéro usage hors `#[cfg(test)] mod tests` de `emission.rs`**. Un test prouve la primitive, jamais le branchement.
- La Forge (`crates/forge/db-forge/src/codegen/projection.rs::write_projection_stub`) ne référence aujourd'hui **aucun** de ces types — aucun `RouteDescriptor`/`SegmentDescriptor` n'est généré pour une route réelle.
- `Projection::MAX_RENDER_CHUNKS` existe et est généré par route — mais gouverne l'assemblage `Vec<RenderChunk>` du rendu Forge-time (`BatchRenderer`), **sans rapport** avec le budget de segments HTTP `K`. Ne pas confondre les deux.
- Phase 5 a remplacé `axum::serve` par une boucle `hyper_util::server::conn::auto::Builder` manuelle — mais, par le propre commentaire du fichier, **`Router`/routes/handlers restent inchangés**. Phase 5 donne le contrôle de l'acceptation de connexion, pas de l'émission d'une réponse individuelle.

---

## C. Objectif immédiat

`[DÉCISION]` Le seul objectif de ce handoff :

> Faire fonctionner une première émission AOT segmentée réelle jusqu'à `Body → Hyper`, sans introduire les concepts hors périmètre (§G de la SPEC, rappelés en §E/§G ci-dessous).

`[INVARIANT]` **Le chemin AOT monolithique existant ne doit être ni détruit, ni généralisé artificiellement.** Si l'incrément décrit ici ne concerne qu'une ou quelques routes de démonstration, les autres routes doivent continuer à fonctionner exactement comme aujourd'hui (`read_at → Vec<u8>`), sans y être forcées de passer par T2A « pour cohérence ».

`[PROVISOIRE]` **Point d'intégration expérimental.** §B établit qu'aucune route réelle ne possède aujourd'hui de `RouteDescriptor`/`SegmentDescriptor` généré par la Forge. Démontrer T2A exige donc, pour cet incrément uniquement, un chemin d'intégration minimal et explicitement provisoire — pas le raccordement Forge définitif. Ce mécanisme :
- peut prendre la forme d'un descripteur statique/local de démonstration (ou toute autre forme minimale appropriée) ;
- doit être marqué provisoire/expérimental sans ambiguïté dans le code (nommage, commentaire, emplacement isolé) ;
- ne doit pas devenir une nouvelle architecture de routage ;
- ne doit pas définir ou présupposer le mécanisme final de génération Forge ;
- n'introduit aucune nouvelle IR (cf. §E) ;
- ne modifie pas silencieusement le contrat de `RouteEntry` ;
- doit permettre de faire tourner réellement, au moins une fois, la chaîne complète : `SegmentDescriptor[] → Source resolution → Selection resolution → ResolvedRange[] → Bytes → Body → Hyper`.

L'objectif de cet incrément est de **démontrer le pipeline réel**, pas de résoudre son alimentation définitive par la Forge.

---

## D. Adaptation `ResolvedRange → Bytes`

`[FAIT]` `bytes::Bytes::from_owner(owner: T) -> Bytes` où `T: AsRef<[u8]> + Send + 'static` construit un `Bytes` `'static` sans copier le contenu référencé par `owner.as_ref()`. Vérifié dans le code source du crate `bytes` (1.12.1) : alloue un unique bloc de contrôle (`Box<Owned<T>>`, refcompte + `T` déplacé dedans), une fois par appel — pas de coût proportionnel au payload.

`[PROVISOIRE]` Le propriétaire (`owner`) minimal nécessaire tourne autour de :

```text
Arc<PackHtmlIndex>
offset
len
```

implémentant `AsRef<[u8]>` en résolvant `handle.blob(offset, len)` (ou équivalent). **Ceci n'est pas imposé comme forme finale** — si l'implémentation trouve une meilleure solution respectant les trois garanties ci-dessous, elle prévaut.

`[INVARIANT]` Le propriétaire doit garantir :
- la durée de vie du mmap sous-jacent (via l'`Arc<PackHtmlIndex>` ou équivalent, cloné, pas emprunté) ;
- une vue strictement limitée au `(offset, len)` demandé — jamais le blob entier ;
- aucune copie du payload à aucune étape de cette conversion.

---

## E. Résolution bornée (ancien chantier « C »)

`[FAIT]` Le stockage temporaire des `ResolvedRange` avant conversion a déjà été délibéré (hors de ce handoff) sous le nom de travail « C ». Conclusion retenue : un tableau de travail `[Option<ResolvedRange<'a>>; MAX_SEGMENTS]` pendant la boucle de résolution, dont on extrait ensuite une vue pleinement initialisée `&[ResolvedRange<'a>]` (sans `Option`) pour alimenter la conversion vers `Bytes`.

`[INVARIANT]`
- Pas de type public `EmissionPlan`, ni de nouvel agrégat équivalent portant un nom différent.
- Pas de nouvelle IR entre `ResolvedRange[]` et `Bytes[]`.
- Pas d'allocation heap inutile pour cette collection de travail (un tableau à capacité fixe convient).

`[OUVERT]` Ne pas concevoir dès cet incrément une architecture finale pour `MAX_SEGMENTS`/`K_AOT` (calcul Forge, stockage global). Si une valeur est nécessaire pour faire compiler ce premier incrément, elle doit être choisie localement et **explicitement documentée comme provisoire** dans le code (commentaire), pas présentée comme un budget normatif.

---

## F. Body / Hyper

`[FAIT]` `axum_core::body::Body::from_stream<S>(stream: S) -> Body` où `S: TryStream, S::Ok: Into<Bytes>` construit un `Body` diffusant chaque item du flux comme une frame de données. Vérifié dans le code source d'`axum-core` (0.5.6). **C'est un mécanisme d'implémentation possible, pas un contrat architectural** — la décision est `ResolvedRange[] → plusieurs unités Bytes → Body → Hyper`, `Body::from_stream` n'en est qu'une réalisation candidate parmi d'éventuelles autres.

`[FAIT]` Pour une réponse à `Content-Length` fixé (pas de `Transfer-Encoding: chunked`), Hyper transmet chaque `Bytes` sans copie ni octet de framing ajouté, et appelle réellement l'écriture vectorisée (`poll_write_vectored`) lorsque le transport le permet (vrai pour `TcpStream`) — vérifié dans le code source de `hyper` (1.11.1), `hyper-util` (0.1.20), `tokio` (1.53.1).

`[INVARIANT]`
- Plusieurs `Bytes` (un par segment) doivent devenir plusieurs frames du même `Body` — pas concaténés côté Marius avant émission.
- **Pour cet incrément T2A**, la longueur totale de la réponse doit être connue avant construction du `Body` (somme des `len` des `ResolvedRange` résolus) et est émise via `Content-Length` — hypothèse opérationnelle de cet incrément, pas une obligation universelle pour tout transport futur (cf. SPEC §5).
- Hyper reste seul responsable de la mise en file, de la vectorisation effective, et des écritures partielles.
- Marius ne construit **jamais** de `IoSlice` lui-même.

---

## G. Préservation du chemin monolithique

`[INVARIANT]` **Ne pas faire passer toutes les routes existantes artificiellement par T2A.** Le chemin :

```text
AOT monolithique → mmap → émission directe
```

doit rester disponible et fonctionnel, séparément du chemin segmenté introduit par cet incrément.

`[OUVERT]` Si le code actuel (dispatch générique unique, `serve_route`) ne permet pas encore de sélectionner proprement entre les deux chemins par route, **documenter ce blocage explicitement** (où, pourquoi, quelles options existent) plutôt que d'inventer immédiatement le mécanisme Forge définitif de sélection. Ce blocage a déjà été partiellement caractérisé : le dispatch actuel est une fonction unique différenciée par une valeur runtime (`RouteEntry`), pas par un type — toute solution de sélection propre devra en tenir compte, sans que cela soit tranché ici.

---

## H. Tests et validation

Critères concrets attendus pour cet incrément :

- Tests existants toujours verts (aucune régression).
- Une émission mono-segment correcte (K=1) via le chemin T2A.
- Une émission multi-segments correcte (K>1) via le chemin T2A.
- Absence de copie du payload — à vérifier explicitement à la frontière Marius → transport, **pas en observant la mémoire effectivement transmise par le noyau/réseau** (inobservable et hors de propos). La vérification porte sur la chaîne : `PackHtmlIndex`/mmap → `owner` → `Bytes::from_owner(...)` → `Body`, et doit confirmer qu'elle référence directement la plage mmapée demandée, sans passer par `mmap → Vec<u8> → copie du payload`. Validations possibles : absence de `read_at` dans le chemin T2A ; absence de tout `Vec<u8>` contenant le payload dans ce chemin ; vérification que `owner.as_ref()` correspond bien à la plage `(offset, len)` attendue ; instrumentation si nécessaire.
- `Content-Length` correct dans la réponse HTTP effective.
- Conservation du lifetime mmap jusqu'à émission complète (pas de crash/use-after-free sous charge ou lecture lente côté client).
- Comportement correct après une rotation du `ArcSwap` (registre) pendant qu'une émission T2A est en cours — cas de concurrence à tester explicitement.
- Aucune régression du chemin monolithique existant.
- Absence de toute réintroduction de `EmissionPlan`, `IoSlice` explicite Marius, `Volatile`, `RequestArena`, ou toute nouvelle IR équivalente sous un autre nom — à vérifier par relecture, pas seulement par intention.

`[INVARIANT]` Les mesures d'allocation, si effectuées, doivent distinguer explicitement trois couches :

```text
Marius/Core (résolution, ResolvedRange)
vs
adaptation Bytes/Body (conversion T2A)
vs
transport Hyper (boxing du Body, mise en file interne)
```

Ne jamais présenter le résultat global comme une certification « zéro allocation HTTP » — ce serait contraire à SPEC §4.

---

## I. Séquencement recommandé

1. Vérifier le point d'insertion exact dans le code actuel (quelle route, quel handler, comment coexister avec `serve_route` existant sans le casser — cf. §G).
2. Concevoir l'owner de `Bytes::from_owner` (§D).
3. Brancher la première résolution réellement exécutée par le chemin HTTP au moyen du point d'intégration expérimental minimal et explicitement provisoire décrit en §C, puisque la génération Forge de `RouteDescriptor` n'est pas encore en place — première fois que ces primitives sortiraient des tests.
4. Produire les `Bytes` (une conversion par `ResolvedRange` résolu).
5. Produire le `Body` (`Body::from_stream` ou équivalent).
6. Fixer `Content-Length` explicitement.
7. Tester (§H).
8. Mesurer (§H, trois couches distinctes).
9. **Seulement ensuite**, si nécessaire, envisager le raccordement Forge/`RouteDescriptor` généré — pas avant.

---

## Rappel final

Ce handoff ne remplace pas `docs/specifications/SPEC-transport-segmente-t2a.md` — il en est l'application opérationnelle pour un premier incrément. En cas de doute entre ce document et la SPEC, la SPEC prévaut. En cas de doute sur un point marqué `[OUVERT]`, ne pas trancher silencieusement en codant — documenter la question et remonter avant de figer un choix qui dépasserait le périmètre de cet incrément.
