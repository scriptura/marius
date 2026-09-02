# Contrat d'augmentation d'une page Marius

**Statut :** Proposition — post-ADR-011
**Dépendances :** ADR-008, ADR-011, DESIGN — Pipeline Runtime de Segments
**Nature :** Contrat architectural

---

## 1. Objet

Ce document définit le contrat architectural permettant à une page Marius, produite AOT et fonctionnelle sans JavaScript, d'être **augmentée par des projections dont le cycle de vie est indépendant de celui de la page**.

L'objectif premier de ce mécanisme est d'éviter l'explosion combinatoire qui résulterait de la génération d'une page complète distincte pour chaque combinaison de contextes indépendants :

* état de session ;
* état du panier ;
* notifications ;
* état d'authentification lorsqu'il constitue une variation indépendante ;
* autres données à cycle de mutation propre.

L'augmentation ne constitue ni un système de composants UI, ni un moteur de rendu côté client, ni une nouvelle étape du Runtime.

Elle exprime une relation architecturale :

> **une page complète peut contenir des points dont le contenu peut évoluer indépendamment de la projection qui a produit le reste de la page.**

---

# 2. Principe fondamental

Une page Marius demeure une **unité AOT complète et fonctionnelle**.

Elle doit pouvoir être servie et utilisée sans JavaScript.

L'augmentation ne remet donc pas en cause le principe d'ADR-008 selon lequel une réponse peut être pré-composée en amont avec notamment :

* la structure minimale du document ;
* la navigation ;
* le fil d'Ariane ;
* le contenu nécessaire ;
* le pied de page ;
* les liens et mécanismes de navigation ;
* les garanties d'accessibilité et de SEO applicables.

ADR-011 introduit une capacité supplémentaire : certaines projections peuvent ne plus être incorporées définitivement à cette page lorsqu'elles possèdent un **cycle de mutation indépendant**.

Le but n'est donc pas de découper arbitrairement une page en composants.

Le critère d'augmentation est :

> **indépendance du cycle de vie et de mutation d'une partie du résultat vis-à-vis de la page qui l'accueille.**

---

# 3. Ce qu'est une augmentation

Une augmentation est l'association de trois éléments conceptuellement distincts :

1. une **page AOT de base**, complète et fonctionnelle ;
2. une **projection indépendante**, produisant un résultat pouvant évoluer séparément ;
3. un **point d'intégration déterministe** permettant au résultat de cette projection d'être présenté dans la page.

L'augmentation permet ainsi de remplacer :

```text
Page × Contexte A × Contexte B × Contexte C
```

par :

```text
Page
 ├── Projection de page
 ├── augmentation A
 ├── augmentation B
 └── augmentation C
```

sans transformer le Runtime en moteur de composition dynamique.

Cette factorisation est une propriété de l'organisation AOT des projections et de leur matérialisation ; elle n'implique pas que la page soit découpée en composants génériques.

---

# 4. Une page personnalisée n'est pas nécessairement une page augmentée

La personnalisation ne constitue pas, à elle seule, un critère d'augmentation.

Une page peut être entièrement dédiée à un contexte particulier et rester une projection AOT complète.

Par exemple :

```text
/admin/dashboard
```

peut être une page intrinsèquement liée au contexte administrateur et être produite comme un artefact complet.

Il n'est pas nécessaire d'extraire artificiellement ses différentes parties en augmentations.

À l'inverse, un état tel qu'un panier, des notifications ou une information de session peut justifier une augmentation lorsque son cycle de mutation est indépendant de celui de la page qui l'accueille.

Le critère est donc :

```text
personnalisation
        ≠
augmentation
```

mais :

```text
cycle de mutation indépendant
        →
augmentation potentielle
```

---

# 5. L'augmentation n'est pas un Segment

Les concepts suivants doivent rester strictement distincts.

### Projection

Concept du domaine Forge/AOT.

Une Projection décrit une unité de transformation déterminée par ses données sources, son cycle d'invalidation et ses invariants de cohérence.

### Artefact

Produit matérialisé d'une Projection.

Dans l'architecture actuelle, l'artefact est notamment matérialisé dans le packfile.

### SegmentDescriptor

IR AOT décrivant une portion de source :

```text
SourceId + offset + len + flags
```

Il est produit par Forge et ne doit pas être reconstruit ou modifié par le Runtime.

### SourceId

Identifiant logique local à une route permettant de référencer une entrée de `SourceSpec`.

Il ne constitue ni une adresse mémoire, ni un identifiant global d'artefact.

### SourceKey

Identifiant global et stable permettant au Runtime de résoudre un artefact matérialisé dans le registre.

### MaterializedSource

Représentation Runtime de la source effectivement disponible pour une requête.

Elle peut notamment être :

```text
Mmap
Volatile
```

### Segment

Notion Runtime correspondant à une portion adressable d'une source.

### Point d'augmentation dans le navigateur

Identifiant ou autre mécanisme permettant au navigateur d'associer le résultat d'une projection à une zone déterminée du document.

Ce point appartient au **contrat navigateur**, pas à l'ontologie du Segment Runtime.

---

# 6. Conséquence essentielle : le Segment n'est pas un fragment DOM

Il ne doit exister aucune équivalence architecturale implicite :

```text
Segment = fragment DOM
```

ou :

```text
Projection = élément HTML
```

Un Segment est une primitive de mémoire et d'émission côté serveur.

Le navigateur reçoit des octets HTTP.

Le mécanisme permettant ensuite de présenter ou de remplacer une augmentation dans le DOM constitue une préoccupation distincte.

Ainsi :

```text
Forge
Projection
    ↓
Artefact
    ↓
SegmentDescriptor[]
    ↓
Runtime
SourceId → MaterializedSource
    ↓
EmissionPlan
    ↓
IoSlice[]
    ↓
HTTP
    ↓
Navigateur
    ↓
DOM Target
```

La frontière entre Runtime et navigateur est donc explicite.

---

# 7. Contrat serveur

L'augmentation doit être compatible avec le pipeline Runtime déjà défini.

Le Runtime ne reçoit pas une instruction du type :

```text
"rendre l'augmentation du panier"
```

Il reçoit une description AOT de sources et de segments.

Pour une route donnée, le chemin est :

```text
RouteDescriptor
    ↓
SegmentDescriptor[]
    ↓
SourceSpec
    ↓
résolution des sources
    ↓
MaterializedSource[]
    ↓
EmissionPlan
    ↓
IoSlice[]
    ↓
backend
```

Cette chaîne demeure inchangée par le concept d'augmentation.

---

# 8. Source statique et source volatile

Une augmentation peut être représentée par une source statique ou volatile selon son mode de matérialisation.

Une source statique peut être un artefact matérialisé dans le packfile et résolu via :

```text
SourceSpec::StaticArtifact
        ↓
SourceKey
        ↓
LiveRegistry
```

Une source volatile peut être réservée par :

```text
SourceSpec::VolatileSlot
        ↓
RequestArena
        ↓
MaterializedSource::Volatile
```

Cette distinction est importante :

> **la volatilité est une propriété de la source Runtime ; elle ne constitue pas une nouvelle catégorie de Projection dans l'ontologie Forge.**

Une Projection peut donc contribuer à une route dont le plan comporte une source volatile sans que le Runtime ait à connaître la sémantique métier de cette Projection.

---

# 9. Production du contenu volatil

Le Runtime de segments définit la capacité de matérialiser une source volatile dans le `RequestArena`.

Il ne définit pas, à ce stade, le mécanisme métier complet permettant de produire les octets de cette source.

La chaîne :

```text
contexte de requête
    ↓
données/session/cart/etc.
    ↓
production du contenu volatil
    ↓
écriture dans RequestArena
    ↓
MaterializedSource::Volatile
```

constitue un contrat complémentaire qui devra être défini avant l'implémentation effective de composants volatils.

Ce mécanisme ne devra pas réintroduire implicitement :

* l'interprétation runtime des templates ;
* une reconstruction générale de page ;
* un moteur de rendu HTML générique ;
* une allocation sur le hot path ;
* une dépendance du Runtime à la sémantique métier des projections.

Le présent contrat **ne préjuge pas de l'implémentation de cette production**.

---

# 10. Un seul monde de données par requête

Lorsqu'une augmentation statique dépend du registre de matérialisation, elle respecte la même garantie de génération que le reste de la réponse.

Une requête ne doit pas observer simultanément plusieurs générations incompatibles du monde matérialisé.

La résolution des sources intervient en tête de requête et produit les `MaterializedSource` nécessaires à la construction de l'`EmissionPlan`.

L'augmentation ne constitue donc pas une exception à la cohérence du Runtime.

---

# 11. Budget et déterminisme

Le nombre et la nature des segments nécessaires à une route sont déterminés AOT.

L'augmentation ne doit pas introduire de composition dynamique non bornée.

Le Forge doit pouvoir vérifier les contraintes applicables, notamment :

* nombre maximal de segments ;
* nombre maximal de sources ;
* capacité réservée aux sources volatiles ;
* compatibilité avec les limites `IOV_MAX` / `UIO_MAXIOV` ;
* capacité totale du `RequestArena` ;
* backend d'émission applicable à la route.

Le Runtime ne découvre donc pas dynamiquement combien de segments une augmentation nécessite.

---

# 12. Atomicité d'une réponse

Lorsqu'une requête produit plusieurs segments, ceux-ci constituent le plan d'émission de cette réponse.

Le Runtime ne doit pas avoir à comprendre quelles portions correspondent à la page de base et lesquelles correspondent à des augmentations.

Il doit seulement émettre le plan déterminé AOT.

Ainsi, au niveau serveur :

```text
page + augmentations
```

est une notion sémantique de l'architecture AOT, tandis que :

```text
segments → IoSlice → émission
```

est la réalité du Runtime.

---

# 13. Contrat navigateur

Le navigateur constitue une couche distincte.

Son rôle éventuel est de permettre à une augmentation déjà définie côté serveur de :

* être identifiée dans le document ;
* être obtenue ou rafraîchie indépendamment ;
* être associée à un point d'intégration déterminé ;
* remplacer ou mettre à jour uniquement la partie concernée.

Ce mécanisme ne doit pas transformer le navigateur en moteur de rendu Marius.

En particulier, le client ne doit pas avoir à :

* interpréter les templates `.marius` ;
* reconstruire la page ;
* exécuter la logique métier du serveur ;
* connaître PostgreSQL ;
* connaître le packfile ;
* connaître `SegmentDescriptor` ;
* connaître `SourceId` ou `SourceKey`.

Le protocole navigateur est donc un **contrat de présentation et de synchronisation**, pas une extension du Runtime de segments.

---

# 14. JavaScript reste une amélioration progressive

La page de base doit demeurer fonctionnelle sans JavaScript.

L'absence de JavaScript ne doit pas supprimer :

* le contenu essentiel ;
* la navigation ;
* les liens ;
* les informations nécessaires à l'utilisateur ;
* les propriétés d'accessibilité attendues ;
* la découvrabilité SEO lorsque celle-ci s'applique.

Une augmentation volatile peut avoir un comportement différent sans JavaScript, mais le système ne doit pas faire de JavaScript une dépendance générale à l'utilisation de la page.

Le contrat devra donc distinguer :

```text
fonctionnalité fondamentale de la page
```

et :

```text
actualisation progressive d'un état indépendant
```

---

# 15. Invalidation serveur et synchronisation navigateur sont distinctes

Le mécanisme d'invalidation Marius :

```text
PostgreSQL
    ↓
LISTEN / NOTIFY
    ↓
Collector
    ↓
Dispatcher
    ↓
régénération
    ↓
matérialisation
```

ne constitue pas en lui-même un mécanisme de synchronisation du DOM d'un navigateur déjà connecté.

Une modification de données peut provoquer la régénération d'un artefact ou d'une source sans qu'un navigateur existant soit immédiatement informé.

Inversement, un mécanisme navigateur permettant de rafraîchir une augmentation ne doit pas être assimilé au mécanisme de causalité `LISTEN/NOTIFY`.

On distingue donc explicitement :

```text
invalidation / matérialisation serveur
```

et :

```text
synchronisation / actualisation navigateur
```

Le protocole reliant éventuellement les deux constitue une décision ultérieure.

---

# 16. Aucun choix technologique implicite

Le présent contrat ne choisit pas entre :

* HTML natif + `fetch` ;
* SSE ;
* WebSocket ;
* EventSource ;
* HTMX ;
* micro-runtime JavaScript Marius ;
* autre mécanisme.

Ces technologies sont des moyens éventuels d'implémenter le contrat navigateur.

Le choix ne doit intervenir qu'après définition du protocole navigateur lui-même.

En particulier, le fait qu'un mécanisme soit capable de remplacer un fragment HTML ne suffit pas à en faire une bonne abstraction pour Marius.

Le protocole devra être évalué selon les invariants du système, notamment :

* absence de dépendance fonctionnelle au JavaScript ;
* simplicité du protocole ;
* absence de logique métier côté client ;
* compatibilité avec l'AOT ;
* coût d'exécution ;
* capacité à cibler précisément une augmentation ;
* comportement en cas de navigation ou de rafraîchissement ;
* cohérence avec les cycles de mutation définis côté serveur.

---

# 17. Hyper et Axum sont hors du contrat d'augmentation

L'intégration Hyper/Axum relève de l'adaptateur HTTP.

Le pipeline de segments ne doit pas devenir dépendant des abstractions Hyper/Axum.

La séparation est :

```text
Architecture Marius
──────────────────────────────────────
RouteDescriptor
SegmentDescriptor
MaterializedSource
EmissionPlan
IoSlice
backend
──────────────────────────────────────
             frontière HTTP
──────────────────────────────────────
Hyper / Axum
socket / transport
```

Le travail de cartographie Hyper/Axum peut déterminer comment `IoSlice`, `writev`, `sendmsg` ou les mécanismes propres à Hyper peuvent être exploités.

Il ne modifie pas le contrat d'augmentation.

De même, `hyper::upgrade` ne fait pas partie du contrat architectural de l'augmentation. Sa nécessité éventuelle relève de l'étude d'un protocole de transport particulier.

---

# 18. Ce que l'augmentation interdit

L'introduction d'augmentations ne doit pas conduire à :

### 18.1 Reconstituer la page côté serveur

Le Runtime ne doit pas reconstruire dynamiquement une page à partir de composants.

### 18.2 Introduire un moteur de template runtime

Les templates `.marius` restent compilés AOT.

### 18.3 Transformer les Projections en composants UI

Une Projection demeure une unité AOT déterminée par ses données, son cycle d'invalidation et ses invariants de cohérence.

### 18.4 Faire du Segment une abstraction DOM

Segment et DOM Target appartiennent à deux niveaux différents.

### 18.5 Introduire une composition non bornée

Les contraintes de capacité et de nombre de segments restent vérifiables AOT.

### 18.6 Rendre le navigateur responsable de la logique métier

Le navigateur présente et actualise le résultat ; il ne reproduit pas le modèle métier Marius.

### 18.7 Faire dépendre la page de JavaScript

L'amélioration progressive ne doit pas devenir une dépendance fonctionnelle.

---

# 19. Critère d'utilisation d'une augmentation

Avant d'introduire une augmentation, la question doit être :

> **Cette partie du résultat possède-t-elle un cycle de mutation suffisamment indépendant de celui de la page pour que sa mutualisation réduise réellement une duplication AOT ?**

Si la réponse est non, l'augmentation ne doit pas être introduite par principe.

Une partie peut rester intégrée à la page complète.

Le mécanisme est donc un outil de **factorisation des cycles de projection**, et non une doctrine générale de découpage de l'interface.

---

# 20. Exemple conceptuel

Une route peut être décrite conceptuellement comme :

```text
/article/123

Page AOT
 ├── structure
 ├── navigation
 ├── breadcrumb
 ├── article
 ├── footer
 └── augmentation : panier
```

Le Forge peut produire un `RouteDescriptor` dont les sources correspondent conceptuellement à :

```text
StaticArtifact(article/page)
StaticArtifact(navigation)
StaticArtifact(footer)
VolatileSlot(cart)
```

Le Runtime ne voit pas :

```text
article
navigation
footer
cart
```

comme des composants UI.

Il voit :

```text
RouteDescriptor
    ↓
SegmentDescriptor[]
    ↓
SourceSpec[]
    ↓
MaterializedSource[]
    ↓
EmissionPlan
    ↓
IoSlice[]
    ↓
backend
```

Le navigateur, lui, peut connaître un point d'intégration correspondant au panier.

Ces deux représentations sont volontairement différentes.

---

# 21. Invariants du contrat

Une augmentation Marius doit respecter simultanément les invariants suivants :

1. **La page de base reste complète et fonctionnelle.**
2. **Le JavaScript n'est jamais une dépendance fonctionnelle générale.**
3. **L'augmentation est motivée par l'indépendance d'un cycle de mutation, pas par la seule localisation DOM.**
4. **La personnalisation seule ne justifie pas une augmentation.**
5. **Une Projection n'est pas un composant DOM.**
6. **Un Segment n'est pas un fragment DOM.**
7. **Le Runtime ne connaît pas la sémantique métier de l'augmentation.**
8. **La composition des segments reste déterminée et bornée AOT.**
9. **Les contraintes de capacité restent vérifiables par Forge.**
10. **Une source volatile respecte le modèle `VolatileSlot → RequestArena → MaterializedSource::Volatile`.**
11. **La production concrète du contenu d'un slot volatil reste séparée du pipeline d'émission.**
12. **Le Runtime ne réinterprète pas les templates et ne reconstruit pas dynamiquement la page.**
13. **La cohérence de génération du `LiveRegistry` s'applique de la même manière aux sources d'une réponse.**
14. **L'invalidation PostgreSQL et la synchronisation navigateur restent deux mécanismes distincts.**
15. **Hyper/Axum restent confinés à la frontière HTTP et ne contaminent pas l'ontologie du Runtime.**
16. **Aucun choix de technologie cliente n'est imposé par ce contrat.**

---

# 22. Périmètre restant à spécifier

Le contrat permet désormais de considérer comme établies les relations :

```text
Projection
    ↓
Artefact
    ↓
SegmentDescriptor[]
    ↓
SourceSpec
    ↓
MaterializedSource[]
    ↓
EmissionPlan
    ↓
IoSlice[]
    ↓
HTTP
```

et, séparément :

```text
résultat HTTP
    ↓
point d'intégration navigateur
    ↓
DOM
```

Les points suivants restent volontairement hors décision :

1. **production exacte du contenu des `VolatileSlot`** ;
2. **contrat précis du point d'intégration navigateur** ;
3. **protocole de rafraîchissement ou de synchronisation navigateur** ;
4. **choix éventuel entre fetch, SSE, WebSocket, HTMX ou micro-runtime natif** ;
5. **intégration concrète du pipeline `IoSlice` avec Hyper/Axum** ;
6. **fate du trait `Projection` historique** ;
7. **évaluation expérimentale de `MSG_ZEROCOPY`**.

Ces questions constituent des chantiers distincts et ne doivent pas être résolues implicitement par le présent contrat.

---

# 23. Formulation synthétique

Le contrat d'augmentation peut finalement être résumé ainsi :

> **Marius produit toujours des pages complètes AOT.**
>
> **ADR-011 permet de ne plus confondre l'unité de projection avec l'unité de page lorsque certaines projections possèdent un cycle de mutation indépendant.**
>
> **L'augmentation factorise donc des cycles de projection indépendants ; elle ne transforme pas la page en assemblage de composants.**
>
> **Côté serveur, cette factorisation est matérialisée par le plan AOT de sources et de segments déjà défini par le Runtime de segments.**
>
> **Côté navigateur, elle pourra être exploitée par un mécanisme d'amélioration progressive permettant de cibler et d'actualiser le résultat concerné.**
>
> **Ces deux niveaux restent séparés : le Segment est une primitive d'émission mémoire ; le point d'intégration DOM est une primitive du protocole navigateur.**
>
> **Aucun moteur de rendu runtime, aucune composition dynamique de page et aucune dépendance JavaScript ne sont introduits.**

---

_Document rédigé le 2 septembre 2026_
