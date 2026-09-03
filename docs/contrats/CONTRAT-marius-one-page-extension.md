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
* certains états d'authentification ;
* autres données à cycle de mutation propre.

L'augmentation ne constitue ni un système de composants UI, ni un moteur de rendu côté client, ni une nouvelle étape du Runtime.

Elle exprime une relation architecturale :

> **Une page complète peut être accompagnée ou enrichie par le résultat d'une projection dont le cycle de production et de mutation est indépendant de celui de la page qui l'accueille.**

---

# 2. Principe fondamental : la page reste complète

Une page Marius demeure une **unité AOT complète et fonctionnelle**.

Elle doit pouvoir être servie et utilisée sans JavaScript.

L'augmentation ne remet donc pas en cause le principe d'ADR-008 selon lequel une réponse peut être pré-composée avec notamment :

* la structure minimale du document ;
* la navigation ;
* le fil d'Ariane ;
* le contenu nécessaire ;
* le pied de page ;
* les liens et mécanismes de navigation ;
* les garanties d'accessibilité et de SEO applicables.

ADR-011 introduit une capacité supplémentaire : certaines projections peuvent ne plus être incorporées définitivement à cette page lorsqu'elles possèdent un **cycle de mutation indépendant**.

Le but n'est donc pas de découper arbitrairement une page en composants.

---

# 3. Deux phénomènes doivent être distingués

Une partie variable d'une page peut relever de deux mécanismes architecturaux radicalement différents :

### 3.1 Contextualisation AOT

Une partie du résultat dépend du **contexte intrinsèque de la route ou de la page en cours de production**.

Cette variation doit être résolue dans la représentation AOT de cette route.

Elle ne constitue pas une augmentation.

### 3.2 Augmentation

Une partie du résultat possède un **cycle de production ou de mutation indépendant de celui de la page**.

Elle peut alors être factorisée comme projection indépendante et être matérialisée séparément.

Cette distinction constitue une frontière fondamentale du présent contrat.

---

# 4. La dépendance à la route n'est pas une augmentation

Une variation déterminée par le contexte intrinsèque de la route ne constitue pas une augmentation.

Lorsqu'une partie de la représentation dépend de la route qui porte la page, cette dépendance doit être résolue lors de la **construction AOT de la représentation de cette route**.

Elle ne doit pas être obtenue par :

* une mutation JavaScript ;
* une déduction du navigateur ;
* une synchronisation client ;
* une composition runtime ;
* un merge tardif de la page.

Le Forge ne « devine » pas l'état au moment du merge.

Il produit une représentation de route pour laquelle cet état est déjà déterminé.

---

# 5. Exemple canonique : navigation avec onglet courant

Considérons un menu principal :

```html
<nav>
  <ul>
    <li><a href="/">Accueil</a></li>
    <li><a href="/articles">Articles</a></li>
    <li><a href="/about">À propos</a></li>
  </ul>
</nav>
```

Sur `/articles`, le résultat attendu peut être :

```html
<nav>
  <ul>
    <li><a href="/">Accueil</a></li>
    <li class="current">Articles</li>
    <li><a href="/about">À propos</a></li>
  </ul>
</nav>
```

L'état :

```text
Articles = current
```

est déterminé par la route de la page.

Il peut également entraîner d'autres variations corrélées :

* ajout de `.current` ;
* suppression du lien ;
* modification de `aria-current` ;
* autre représentation déterminée par la route.

Toutes ces propriétés relèvent de la **contextualisation AOT de la page**.

Le Runtime ne doit pas exécuter :

```text
si route == /articles
    ajouter .current
    supprimer href
```

Le navigateur ne doit pas davantage effectuer ce calcul.

Le Forge connaît la table des routes et le contexte de production de la représentation.

Il peut donc produire conceptuellement :

```text
Route /articles
    → navigation contextualisée pour /articles

Route /about
    → navigation contextualisée pour /about

Route /contact
    → navigation contextualisée pour /contact
```

Le Runtime ne connaît pas la notion de `.current`.

Il reçoit simplement la description AOT correspondant à la route demandée.

---

# 6. Il ne faut pas confondre variation locale et composition

Le fait qu'un seul élément du menu change ne signifie pas qu'un merge runtime est nécessaire.

Dans l'exemple précédent, on pourrait être tenté de raisonner :

```text
menu générique
      +
état de l'onglet courant
      ↓
merge
```

Ce raisonnement introduit une composition tardive qui n'est pas nécessaire.

La représentation peut au contraire être pensée comme :

```text
Route /articles
    │
    ├── navigation contextualisée
    ├── breadcrumb
    ├── contenu
    └── footer
```

et être ensuite aplatie par Forge dans le modèle de segments prévu par ADR-011.

Le fait qu'une seule propriété ou qu'un seul élément diffère n'est pas en soi un argument en faveur de l'augmentation.

Le critère pertinent reste l'indépendance du cycle de production ou de mutation.

---

# 7. Critère fondamental d'une augmentation

Le critère d'augmentation est :

> **indépendance du cycle de production et de mutation d'une partie du résultat vis-à-vis de la page qui l'accueille.**

Une partie doit donc être considérée comme augmentation potentielle lorsque son état peut évoluer indépendamment de la page, et que cette indépendance apporte un bénéfice réel de factorisation AOT.

À l'inverse, une partie dont l'état est déterminé par la route courante appartient naturellement à la représentation AOT de cette route.

On peut résumer :

```text
Dépendance à la route courante
        ↓
Contextualisation AOT
        ↓
Pas une augmentation
```

alors que :

```text
Cycle de mutation indépendant
        ↓
Projection indépendante
        ↓
Augmentation potentielle
```

---

# 8. Une page personnalisée n'est pas nécessairement une page augmentée

La personnalisation ne constitue pas, à elle seule, un critère d'augmentation.

Une page peut être entièrement dédiée à un contexte particulier et rester une projection AOT complète.

Par exemple :

```text
/admin/dashboard
```

peut être une représentation intrinsèquement liée au contexte administrateur et être produite comme un artefact complet.

Il n'est pas nécessaire d'extraire artificiellement ses différentes parties en augmentations.

Ainsi :

```text
personnalisation
        ≠
augmentation
```

De même :

```text
variation selon la route
        ≠
augmentation
```

---

# 9. L'augmentation n'est pas un Segment

Les concepts suivants doivent rester strictement distincts.

### Projection

Concept du domaine Forge/AOT.

Une Projection constitue une unité déterminée par ses données sources, son cycle d'invalidation et ses invariants de cohérence.

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

Identifiant global et stable permettant de résoudre un artefact matérialisé dans le registre.

### MaterializedSource

Représentation Runtime de la source effectivement disponible pour une requête.

Elle peut notamment être :

```text
Mmap
Volatile
```

### Segment

Notion Runtime correspondant à une portion adressable d'une source.

### Point d'intégration navigateur

Identifiant ou mécanisme permettant au navigateur d'associer le résultat d'une projection à une zone déterminée du document.

Ce point appartient au **contrat navigateur**, pas à l'ontologie du Segment Runtime.

---

# 10. Le Segment n'est pas un fragment DOM

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

La chaîne conceptuelle est donc :

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

---

# 11. Contrat serveur

L'augmentation doit être compatible avec le pipeline Runtime déjà défini.

Le Runtime ne reçoit pas une instruction sémantique telle que :

```text
"rendre le panier"
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

Le Runtime n'a donc pas à savoir si une source correspond :

* à une page ;
* à une navigation ;
* à un panier ;
* à une notification ;
* ou à toute autre projection.

---

# 12. Contextualisation AOT et RouteDescriptor

La découverte du cas `.current` permet de préciser le rôle du `RouteDescriptor`.

Le `RouteDescriptor` représente le résultat AOT de la résolution d'une route.

La contextualisation déterminée par cette route doit donc être reflétée dans la représentation AOT de cette route avant l'exécution du Runtime.

Conceptuellement :

```text
                    FORGE
                      │
          ┌───────────┴───────────┐
          │                       │
      /articles                 /about
          │                       │
          ▼                       ▼
 RouteDescriptor             RouteDescriptor
          │                       │
          ├── page                 ├── page
          ├── navigation           ├── navigation
          │   current=articles     │   current=about
          └── footer               └── footer
```

Le Runtime ne déduit pas :

```text
URL = /articles
→ modifier le menu
```

Il résout et émet la représentation AOT correspondant à `/articles`.

**L'état courant est donc une propriété de la représentation de route, pas une opération de merge.**

---

# 13. Source statique et source volatile

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

Cette distinction doit rester explicite :

> **la volatilité est une propriété de la source Runtime ; elle ne constitue pas une nouvelle catégorie de Projection dans l'ontologie Forge.**

---

# 14. Production du contenu volatil

Le Runtime de segments définit la capacité de matérialiser une source volatile dans le `RequestArena`.

Il ne définit pas encore le mécanisme métier complet permettant de produire les octets de cette source.

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

constitue un contrat complémentaire à définir avant l'implémentation effective de composants volatils.

Ce mécanisme ne devra pas réintroduire implicitement :

* l'interprétation runtime des templates ;
* une reconstruction générale de page ;
* un moteur de rendu HTML générique ;
* une allocation sur le hot path ;
* une dépendance du Runtime à la sémantique métier des projections.

Le présent contrat ne préjuge pas de son implémentation.

---

# 15. Un seul monde de données par requête

Lorsqu'une augmentation statique dépend du registre de matérialisation, elle respecte la même garantie de génération que le reste de la réponse.

Une requête ne doit pas observer simultanément plusieurs générations incompatibles du monde matérialisé.

La résolution des sources intervient en tête de requête et produit les `MaterializedSource` nécessaires à la construction de l'`EmissionPlan`.

L'augmentation ne constitue donc pas une exception à la cohérence du Runtime.

---

# 16. Budget et déterminisme

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

# 17. Atomicité d'une réponse

Lorsqu'une requête produit plusieurs segments, ceux-ci constituent le plan d'émission de cette réponse.

Le Runtime ne doit pas avoir à comprendre quelles portions correspondent à la page de base et lesquelles correspondent à des augmentations.

Il doit seulement émettre le plan déterminé AOT.

Ainsi :

```text
page + contextualisations AOT + éventuelles augmentations
```

est une notion sémantique de l'architecture AOT, tandis que :

```text
segments → IoSlice → émission
```

est la réalité du Runtime.

---

# 18. Contrat navigateur

Le navigateur constitue une couche distincte.

Son rôle éventuel est de permettre à une augmentation déjà définie côté serveur de :

* être identifiée dans le document ;
* être obtenue ou rafraîchie indépendamment ;
* être associée à un point d'intégration déterminé ;
* remplacer ou mettre à jour uniquement la partie concernée.

Ce mécanisme ne doit pas transformer le navigateur en moteur de rendu Marius.

Le client ne doit pas avoir à :

* interpréter les templates `.marius` ;
* reconstruire la page ;
* exécuter la logique métier du serveur ;
* connaître PostgreSQL ;
* connaître le packfile ;
* connaître `SegmentDescriptor` ;
* connaître `SourceId` ou `SourceKey`.

---

# 19. JavaScript reste une amélioration progressive

La page de base doit demeurer fonctionnelle sans JavaScript.

Une augmentation peut bénéficier d'une actualisation progressive côté navigateur, mais cette capacité ne doit pas devenir une dépendance fonctionnelle générale de la page.

Il faut distinguer :

```text
fonctionnalité fondamentale de la page
```

de :

```text
actualisation progressive d'un état indépendant
```

Une absence de JavaScript peut donc empêcher une actualisation automatique d'un état volatile sans rendre la page elle-même inutilisable.

---

# 20. Invalidation serveur et synchronisation navigateur sont distinctes

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

On distingue donc :

```text
invalidation / matérialisation serveur
```

et :

```text
synchronisation / actualisation navigateur
```

Le protocole reliant éventuellement les deux constitue une décision ultérieure.

---

# 21. Aucun choix technologique implicite

Le présent contrat ne choisit pas entre :

* `fetch` ;
* SSE ;
* WebSocket ;
* EventSource ;
* HTMX ;
* micro-runtime JavaScript Marius ;
* autre mécanisme.

Ces technologies sont des moyens éventuels d'implémenter le contrat navigateur.

Le choix ne doit intervenir qu'après définition du protocole navigateur lui-même.

---

# 22. Hyper et Axum sont hors du contrat d'augmentation

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

# 23. Ce que l'augmentation interdit

L'introduction d'augmentations ne doit pas conduire à :

### 23.1 Reconstituer la page côté serveur

Le Runtime ne doit pas reconstruire dynamiquement une page à partir de composants.

### 23.2 Introduire un moteur de template runtime

Les templates `.marius` restent compilés AOT.

### 23.3 Transformer les Projections en composants UI

Une Projection demeure une unité AOT déterminée par ses données, son cycle d'invalidation et ses invariants de cohérence.

### 23.4 Traiter toute variation visuelle comme une augmentation

Une variation déterminée par la route ou le contexte intrinsèque de la page doit être résolue AOT dans la représentation de cette route.

### 23.5 Faire du Segment une abstraction DOM

Segment et DOM Target appartiennent à deux niveaux différents.

### 23.6 Introduire une composition non bornée

Les contraintes de capacité et de nombre de segments restent vérifiables AOT.

### 23.7 Rendre le navigateur responsable de la logique métier

Le navigateur présente et actualise le résultat ; il ne reproduit pas le modèle métier Marius.

### 23.8 Faire dépendre la page de JavaScript

L'amélioration progressive ne doit pas devenir une dépendance fonctionnelle.

---

# 24. Test architectural : « est-ce réellement indépendant ? »

Avant d'introduire une augmentation, il faut poser deux questions.

### Question 1 — La valeur dépend-elle intrinsèquement de la route courante ?

Si oui :

```text
→ contextualisation AOT
→ pas une augmentation
```

Exemple :

```text
/articles
→ onglet Articles.current
```

### Question 2 — Peut-elle muter alors que la page reste la même ?

Si oui, et si cette indépendance apporte une factorisation utile :

```text
→ projection indépendante
→ augmentation potentielle
```

Exemple :

```text
/article/123
        │
        └── panier
             ↑
       peut changer indépendamment
       de /article/123
```

Cette seconde propriété est nécessaire mais doit être évaluée avec le bénéfice réel de factorisation ; toute donnée susceptible de changer n'a pas vocation à devenir une augmentation.

---

# 25. Exemple comparatif

### Cas A — Navigation

```text
URL : /articles

Navigation
    └── Articles = current
```

La navigation dépend de la route.

```text
→ contextualisation AOT
→ aucune augmentation
```

### Cas B — Panier

```text
URL : /articles/123

Page
    └── état du panier
```

Le panier peut changer sans changement de route.

```text
→ cycle indépendant
→ augmentation potentielle
```

### Cas C — Tableau de bord administrateur

```text
/admin/dashboard
```

La page est intrinsèquement dédiée à un contexte.

```text
→ page AOT complète
→ aucune obligation d'augmentation
```

### Cas D — Notifications

```text
URL : /articles/123

Page
    └── notifications
```

Les notifications peuvent changer sans que la page change.

```text
→ cycle indépendant
→ augmentation potentielle
```

---

# 26. Invariants du contrat

Une architecture d'augmentation Marius doit respecter simultanément les invariants suivants :

1. **La page de base reste complète et fonctionnelle.**
2. **Le JavaScript n'est jamais une dépendance fonctionnelle générale.**
3. **Une dépendance à la route courante est une contextualisation AOT, pas une augmentation.**
4. **L'état d'une contextualisation de route est déterminé avant le Runtime.**
5. **Le Runtime ne déduit pas la sémantique de la route pour modifier la représentation.**
6. **La personnalisation seule ne justifie pas une augmentation.**
7. **L'augmentation est motivée par l'indépendance d'un cycle de production ou de mutation.**
8. **Une Projection n'est pas un composant DOM.**
9. **Un Segment n'est pas un fragment DOM.**
10. **Le Runtime ne connaît pas la sémantique métier de l'augmentation.**
11. **La composition des segments reste déterminée et bornée AOT.**
12. **Les contraintes de capacité restent vérifiables par Forge.**
13. **Une source volatile respecte le modèle `VolatileSlot → RequestArena → MaterializedSource::Volatile`.**
14. **La production concrète du contenu d'un slot volatil reste séparée du pipeline d'émission.**
15. **Le Runtime ne réinterprète pas les templates et ne reconstruit pas dynamiquement la page.**
16. **La cohérence de génération du `LiveRegistry` s'applique de la même manière aux sources d'une réponse.**
17. **L'invalidation PostgreSQL et la synchronisation navigateur restent deux mécanismes distincts.**
18. **Hyper/Axum restent confinés à la frontière HTTP et ne contaminent pas l'ontologie du Runtime.**
19. **Aucun choix de technologie cliente n'est imposé par ce contrat.**

---

# 27. Périmètre restant à spécifier

Les relations suivantes peuvent désormais être considérées comme établies :

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

Restent volontairement hors décision :

1. **production exacte du contenu des `VolatileSlot`** ;
2. **contrat précis du point d'intégration navigateur** ;
3. **protocole de rafraîchissement ou de synchronisation navigateur** ;
4. **choix éventuel entre fetch, SSE, WebSocket, HTMX ou micro-runtime natif** ;
5. **intégration concrète du pipeline `IoSlice` avec Hyper/Axum** ;
6. **fate du trait `Projection` historique** ;
7. **évaluation expérimentale de `MSG_ZEROCOPY`**.

Ces questions constituent des chantiers distincts et ne doivent pas être résolues implicitement par le présent contrat.

---

# 28. Formulation synthétique

Le contrat d'augmentation peut finalement être résumé ainsi :

> **Marius produit toujours des pages complètes AOT.**
>
> **Une variation déterminée par la route courante est une contextualisation AOT de cette page, et non une augmentation.**
>
> **L'état d'une telle contextualisation est déterminé lors de la construction AOT de la représentation de la route ; le Runtime ne le devine ni ne le calcule.**
>
> **ADR-011 permet en revanche de factoriser des projections dont le cycle de production ou de mutation est indépendant de celui de la page.**
>
> **L'augmentation ne transforme donc pas la page en assemblage de composants : elle factorise des cycles de projection indépendants.**
>
> **Côté serveur, cette factorisation est matérialisée par le plan AOT de sources et de segments déjà défini par le Runtime de segments.**
>
> **Côté navigateur, elle pourra être exploitée par un mécanisme d'amélioration progressive permettant de cibler et d'actualiser le résultat concerné.**
>
> **Le Segment reste une primitive d'émission mémoire ; le point d'intégration DOM reste une primitive du protocole navigateur.**
>
> **Aucun moteur de rendu runtime, aucune composition dynamique de page et aucune dépendance JavaScript ne sont introduits.**

---

_Document rédigé le 3 septembre 2026_
