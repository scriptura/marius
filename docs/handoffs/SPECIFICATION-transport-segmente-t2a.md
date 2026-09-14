# Spécification — Frontière transport T2A (émission AOT segmentée)

**Statut :** v2 normative (corrections d'audit intégrées — sémantique segment/source, portée de `Content-Length`)
**Projet :** `scriptura/marius`
**Dépendances architecturales :** ADR-011, `DESIGN-runtime-segment-pipeline.md`, `specification-AOT-boundary-volatile-and-emission-plan-contract.md` (v1)
**Portée :** cette spécification fixe le contrat de la frontière entre la représentation Marius (`ResolvedRange[]`) et le transport HTTP (Hyper/Axum), pour la famille d'émission **AOT segmentée** uniquement. Elle ne modifie aucun document existant et ne remplace aucune décision antérieure.

---

## 1. Objet

Marius distingue désormais explicitement **deux familles d'émission HTTP**, qui ne partagent pas le même contrat de performance :

- **AOT monolithique** : une page = une source mmap contigüe, livrée par un chemin mono-source direct. Le zéro-allocation Marius reste, sur ce chemin, un objectif architectural préservé — cette spécification ne le modifie pas.
- **AOT segmentée** : une réponse est composée de **plusieurs segments, éventuellement issus de plusieurs sources** (`SegmentDescriptor[]`, budget `K` fixé AOT par route), résolus puis adaptés au transport. **T2A** est la stratégie retenue pour cette seconde famille.

Rappel du modèle déjà normatif ailleurs, pertinent pour cette distinction :

```text
SourceKey = identité globale de la source (registre)
SourceId  = référence locale à la route (index dans RouteDescriptor.sources)
Segment   = unité d'émission (SegmentDescriptor → résolu en un ResolvedRange)
```

Plusieurs `SegmentDescriptor` d'une même route peuvent référencer le même `SourceKey`. Le budget `K` et le pipeline de cette spécification portent donc sur le nombre de **segments** d'une réponse, pas sur le nombre de sources distinctes qui les alimentent — ces deux cardinalités ne coïncident pas nécessairement.

Cette spécification ne traite que la frontière de sortie de la famille segmentée. Elle ne redéfinit pas la résolution en amont (`SourceKey`/`SourceId`/génération), déjà normative ailleurs.

---

## 2. Pipeline normatif

```text
Projection
    ↓
Artefact
    ↓
SegmentDescriptor[]
    ↓
Source resolution
    ↓
Selection resolution
    ↓
ResolvedRange[]
    ↓
──────────── frontière transport (objet de cette spec) ────────────
    ↓
Bytes
    ↓
Body
    ↓
Hyper
    ↓
transport HTTP (socket, framing, vectored I/O éventuel)
```

**`ResolvedRange[]` est le dernier niveau de représentation appartenant à Marius.** Tout ce qui se trouve après la frontière appartient au transport :

- `Bytes` : représentation d'adaptation, propriété du point de conversion, pas un concept Marius.
- `Body` : appartient entièrement à la couche HTTP (`axum_core::body::Body`).
- `IoSlice` : détail interne à Hyper, jamais construit ni possédé par Marius.
- `writev`/`sendmsg`, gestion des écritures partielles, backpressure, propriété du socket : hors du Core Marius, délégués entièrement à Hyper.

---

## 3. T2A — décision actée

> **T2A est la stratégie retenue pour l'émission des réponses AOT segmentées.**

Adaptation normative :

```text
ResolvedRange
    →
Bytes::from_owner(owner)
    →
Body (Frame de données)
    →
Hyper
```

**Invariant :** le payload mmap référencé par un `ResolvedRange` n'est jamais copié dans un `Vec<u8>` pour franchir cette frontière.

---

## 4. Zéro-allocation — portée exacte

Le zéro-allocation **n'est pas une propriété universelle de toute émission HTTP Marius**. Sa portée est distincte par famille :

| | AOT monolithique | AOT segmentée (T2A) |
|---|---|---|
| Zéro-allocation Marius | Objectif architectural préservé, chemin mono-source direct | Non garanti — coût de matérialisation accepté |
| Nature du coût accepté | — | Borné par `K` (nombre de segments de la route), jamais par la taille des payloads |
| Copie du payload | Aucune (cible) | Aucune (invariant, §3) |

**Formulation à respecter — ne pas dévier :** le coût de matérialisation/adaptation des unités Marius (`ResolvedRange → Bytes`) est borné par `K`. Les coûts internes propres au transport Hyper (mise en file, boxing du `Body`, gestion des buffers d'écriture) **ne font pas partie du contrat de performance du Core Marius** — ils appartiennent à une couche que Marius ne contrôle pas et n'a pas vocation à borner.

Ne pas écrire, et ne pas laisser entendre, que « le hot path HTTP Marius n'est plus zéro-allocation » — cette formulation efface la distinction par famille que cette spécification établit.

---

## 5. `Content-Length`

**Pour l'incrément T2A actuel**, l'hypothèse opérationnelle retenue est : la longueur totale de la réponse est connue avant construction du `Body` (somme des longueurs des segments résolus), et elle est émise via `Content-Length`. Cela évite le framing `Transfer-Encoding: chunked` et préserve la transmission des segments de payload sans octets de framing additionnels par segment.

Cette spécification n'érige pas `Content-Length` en obligation universelle pour tout transport ou protocole futur de Marius :

```text
Décision T2A actuelle :
longueur connue avant construction du Body → Content-Length

Question future :
autres protocoles (HTTP/2), autres formes d'émission où la longueur
totale ne serait pas connue à l'avance
→ hors périmètre de cette spécification
```

---

## 6. Hyper — répartition des responsabilités

Acté, sans exception pour T2A :

- Hyper conserve la propriété du socket.
- Hyper gère le framing HTTP.
- Hyper gère les écritures partielles (`partial writes`).
- Hyper gère le backpressure.
- Hyper peut exploiter l'écriture vectorisée (`poll_write_vectored`) lorsque le transport sous-jacent le permet.
- Marius ne reproduit aucun de ces mécanismes.

**Invariant :** les valeurs internes observées dans l'implémentation actuelle de Hyper (à titre d'exemple, des plafonds internes de mise en file ou de nombre de buffers par écriture vectorisée) ne deviennent **jamais** des invariants Marius. Ce sont des détails d'implémentation d'une dépendance externe, sujets à changement sans préavis pour Marius, et ne doivent apparaître dans aucun contrat normatif Marius.

---

## 7. `K` — ce qui est décidé, et ce qui ne l'est pas

Décidé :

- Le nombre de segments d'une réponse AOT segmentée est borné AOT, par route.
- Ce budget (`K` par route) est une notion Marius, distincte des limites internes du transport Hyper.
- Dépasser un plafond interne de Hyper (mise en file, taille d'écriture vectorisée) peut entraîner davantage de cycles de flush/écriture côté transport — **ce n'est pas une invalidité architecturale Marius**.

Non décidé, volontairement laissé ouvert par cette spécification :

- La définition finale de `K_AOT` (nom, portée : par route vs globale).
- Son mode de calcul par la Forge.
- Son stockage runtime définitif.

---

## 8. Hors périmètre explicite

Cette spécification ne traite pas, et ne doit pas être invoquée pour trancher :

- HTTP/2 (mécanisme de transport distinct, non étudié).
- Le Volatile (production, cycle de vie, `VolatileSlot`).
- `RequestArena`.
- Le mécanisme complet d'augmentation côté navigateur.
- Le choix définitif, au niveau Forge, entre chemin monolithique et chemin segmenté pour une route donnée.
- La génération automatique de `RouteDescriptor`/`SegmentDescriptor` par la Forge.
- La définition finale et le calcul de `K_AOT`.
- Toute optimisation spécifique du transport au-delà de ce que cette spécification acte.
- Toute mesure de performance définitive ou validation expérimentale.

---

## 9. Suppression de l'ancien modèle — confirmation normative

- `EmissionPlan` **n'est pas conservé** comme IR d'exécution. Sa non-nécessité a été établie par analyse préalable (aucune garantie identifiée qu'il apportait au-delà de ce que la frontière transport ci-dessus fournit déjà).
- **Aucun nouvel agrégat équivalent** ne doit être créé sous un autre nom pour reproduire son rôle.
- `IoSlice[]` **n'est pas une étape persistante** du pipeline Marius — c'est un détail interne à Hyper, construit et détruit à l'intérieur du transport, jamais matérialisé ni possédé côté Marius.

---

## 10. Statut

Cette spécification est normative pour la frontière transport de la famille AOT segmentée uniquement. Elle n'altère aucun document existant. Toute contradiction découverte avec ADR-011, le CONTRAT, le DESIGN ou la spécification AOT/Volatile v1 doit faire l'objet d'un audit séparé, pas d'une correction silencieuse via ce document.
