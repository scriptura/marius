# Manifeste de la Projection Réactive

> Manifeste architectural du système de projection réactive de Marius.
> Il décrit le modèle d'exécution courant et ses invariants structurels.
> Les détails d'implémentation et les procédures de diagnostic sont décrits
> dans les guides runtime associés.

---

## 1. Vision stratégique

Le serveur web n'est pas un médiateur interactif qui reconstruit une réponse
à chaque requête : c'est un **Système de Projection**.

PostgreSQL constitue la source de vérité. Les mutations de cette source
produisent des signaux de changement qui déclenchent une régénération
incrémentale des artefacts HTML.

Le résultat de cette projection est un artefact statique ou semi-statique,
déjà rendu et directement exploitable par le chemin HTTP.

Le coût de transformation est ainsi déplacé du chemin de lecture vers le
chemin d'écriture / de régénération.

Le chemin HTTP n'exécute ni logique métier, ni requête SQL, ni rendu de
template : il transporte un artefact déjà matérialisé.

---

## 2. Résolution des problèmes classiques

### Invalidation du cache

Il n'existe pas de TTL comme mécanisme primaire d'invalidation.

L'artefact HTML est régénéré lorsqu'une mutation de la source de vérité
produit un événement `NOTIFY`.

L'invalidation est donc causale plutôt que temporelle :

```text
mutation PostgreSQL
        │
        ▼
      NOTIFY
        │
        ▼
régénération du delta
        │
        ▼
nouvelle génération du pack HTML
````

### Indirection de transformation

Le chemin HTTP ne passe ni par un ORM, ni par une sérialisation JSON, ni par
une reconstruction dynamique de la vue.

Le rendu est effectué en amont par le pipeline AOT et le résultat est
persisté dans le pack HTML.

La régénération, elle, récupère directement auprès de PostgreSQL les données
correspondant au delta à traiter via `P::fetch_batch`.

`store.bin` n'est pas une étape intermédiaire de cette régénération : il
constitue une projection persistée distincte et n'est jamais lu par
`regenerate_and_swap`.

### Gaspillage CPU

Le rendu HTML est calculé lors de la régénération et non lors de la requête
HTTP.

Le chemin de lecture se réduit ainsi à la résolution d'un index et à la
lecture du fragment déjà matérialisé dans le pack HTML.

---

## 3. Invariants structurels

L'architecture repose sur quatre piliers.

### 3.1 Source de vérité — PostgreSQL

PostgreSQL centralise l'état et la logique métier.

Une mutation n'est qualifiée comme telle que par la base de données.

Les artefacts `store.bin` et `pack.bin` sont des projections dérivées. Aucun
d'eux ne constitue une source de vérité concurrente.

### 3.2 Canal de transport — LISTEN/NOTIFY

PostgreSQL transmet les signaux de mutation au système applicatif via
`LISTEN/NOTIFY`.

Le signal transporte l'identifiant de l'entité concernée ; il ne constitue
pas lui-même la donnée à rendre.

Le système de projection récupère ensuite l'état courant auprès de
PostgreSQL.

### 3.3 Transformateur — Rust AOT

Le pipeline de projection transforme les données récupérées en fragments
HTML déjà matérialisés.

Les templates `.marius` sont compilés en code Rust. Le runtime n'interprète
pas les templates.

Le rendu est effectué hors du chemin HTTP.

### 3.4 Artefact de lecture — pack HTML

Le pack HTML est l'artefact directement consommé par le serveur HTTP.

Le `LiveRegistry` publie la génération actuellement servie.

Une nouvelle génération est construite hors de l'artefact actuellement
servi, puis publiée atomiquement après validation et finalisation.

---

## 4. Limite physiologique — l'amplification d'écriture

Une mutation massive en base peut produire un grand nombre de notifications.

Traiter chaque notification comme une opération de rendu indépendante
introduirait une amplification inutile du travail de projection et des
écritures disque.

Le système réduit cette entropie en regroupant les identifiants avant
régénération.

---

## 5. Modèle Collector / Dispatcher

### Collector — dédoublonnement

Les identifiants reçus via `NOTIFY` sont regroupés dans le `Collector`.

Un même identifiant peut être reçu plusieurs fois avant le flush ; il n'est
conservé qu'une seule fois dans le delta à traiter.

Le Collector constitue donc une frontière de déduplication entre le flux
d'événements PostgreSQL et le pipeline de régénération.

### Dispatcher — lissage de charge

Le `Dispatcher` transforme le flux regroupé en lots de régénération.

Deux conditions peuvent provoquer un flush :

* **volumétrique** : le seuil configuré est atteint ;
* **temporelle** : le tick périodique force le traitement du lot accumulé.

Le lot transmis à `regenerate_and_swap` représente le **delta du tick**, et
non l'intégralité de la table.

### Concurrence inter-shard / séquentialité intra-lot

Chaque artefact projeté possède son propre pipeline de dispatch.

La concurrence entre artefacts distincts est donc inter-shard.

À l'intérieur d'un lot, le rendu est séquentiel et exploite un buffer réutilisé.

La régulation de l'I/O disque est indépendante du fetch PostgreSQL : le
sémaphore d'I/O est acquis avant le noyau de fusion physique afin de limiter
la pression simultanée sur le système de fichiers et le cache de pages.

---

## 6. Pipeline mécanique global

Le cycle réactif d'une mutation suit ce flux :

```text
PostgreSQL
    │
    │ UPDATE / INSERT / DELETE
    ▼
trigger SQL
    │
    ▼
pg_notify
    │
    ▼
PgListener
    │
    ▼
Collector
    │
    ▼
Dispatcher
    │
    ▼
delta d'IDs
    │
    ▼
P::fetch_batch(pool, ids)
    │
    │ PostgreSQL live
    ▼
DeltaBatch
    │
    ▼
BatchRenderer
    │
    ▼
merge_sweep(old_pack, delta)
    │
    ▼
nouveau pack HTML
    │
    ▼
fsync + rename atomique
    │
    ▼
PackHtmlIndex::open()
    │
    ▼
LiveRegistry
    │
    ▼
HTTP
    │
    ▼
pread()
```

### 6.1 Mutation DB

Une mutation sur une table projetée déclenche le trigger PostgreSQL.

### 6.2 Signal

Le trigger émet un `NOTIFY` contenant l'identifiant concerné.

### 6.3 Capture

`PgListener` reçoit le signal et transmet l'identifiant au `Collector`.

### 6.4 Dispatch

Le `Collector` déduplique les identifiants. Le `Dispatcher` déclenche un
flush selon le seuil volumétrique ou le tick temporel.

### 6.5 Extraction des données

`regenerate_and_swap` récupère directement auprès de PostgreSQL l'état
courant des identifiants du delta via `P::fetch_batch`.

Cette étape ne lit jamais `store.bin`.

L'absence d'un identifiant dans le résultat de `fetch_batch` permet de
représenter une suppression dans le `DeltaBatch`.

### 6.6 Projection AOT

`BatchRenderer` transforme les lignes récupérées en fragments HTML.

Le rendu du lot est séquentiel et alimente un buffer de payload continu.

### 6.7 Fusion

`merge_sweep` fusionne le delta rendu avec la génération actuellement servie.

Les entités absentes du delta sont conservées depuis l'ancien pack.

Les entités modifiées ou insérées sont remplacées par leur nouvelle
génération ; les suppressions sont retirées.

La fusion ne modifie jamais l'ancien packfile.

### 6.8 Publication

La nouvelle génération est écrite dans un fichier temporaire.

Après finalisation, synchronisation et réduction à sa taille réelle, elle
remplace l'ancien fichier par `rename` atomique.

Le fichier final est ensuite rouvert et validé par `PackHtmlIndex::open`.

Ce n'est qu'après cette réussite que le `LiveRegistry` publie la nouvelle
génération.

### 6.9 Lecture HTTP

Le chemin HTTP ne rejoue aucune partie du pipeline précédent.

Il résout le fragment dans le `LiveRegistry` puis le lit directement dans le
pack HTML déjà matérialisé via `pread()`.

Ainsi :

```text
HTTP ≠ PostgreSQL
HTTP ≠ Collector
HTTP ≠ Dispatcher
HTTP ≠ render()
HTTP ≠ merge_sweep
HTTP = lecture d'un artefact déjà projeté
```

---

## 7. `store.bin` : projection distincte

`store.bin` appartient à un pipeline distinct de la projection HTML réactive.

Il représente une projection DOD persistée des données PostgreSQL et peut
être produit ou mis à jour par le pipeline d'extraction correspondant.

Il n'est toutefois **pas** une étape du chemin :

```text
NOTIFY → Collector → Dispatcher → regenerate_and_swap
```

En particulier :

* `regenerate_and_swap` ne lit jamais `store.bin` ;
* `P::fetch_batch` utilisé par la régénération récupère les données auprès de
  PostgreSQL ;
* `pack.bin` n'est pas reconstruit à partir de `store.bin` ;
* `store.bin` et `pack.bin` sont deux artefacts de projection distincts.

Cette séparation est structurelle : `store.bin` peut être plus ancien que
l'état PostgreSQL ayant déclenché le `NOTIFY`. L'utiliser comme source de la
régénération réactive introduirait donc une fenêtre de fraîcheur incompatible
avec la sémantique du pipeline.

---

## 8. Propriété fondamentale

La propriété centrale du système peut être résumée ainsi :

> **PostgreSQL est la source de vérité ; `NOTIFY` transporte le signal ;
> `fetch_batch` récupère l'état courant ; le pipeline AOT matérialise le
> résultat ; le pack HTML devient l'état de lecture ; HTTP ne fait que le
> transporter.**

Le système n'est donc pas un cache autour de PostgreSQL.

C'est une **projection matérialisée réactive**, dont le coût de transformation
est déplacé hors du chemin de lecture et dont la génération actuellement
servie est publiée atomiquement.

---

__Document initialement rédigé le 25 mars 2026.__
__Dernière révision le 25 août 2026.__
