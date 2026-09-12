# Spécification — Frontière AOT / Volatile et contrat d’`EmissionPlan`

**Statut :** Draft de travail — à auditer  
**Projet :** `scriptura/marius`  
**Dépendances architecturales :** ADR-011, `DESIGN-runtime-segment-pipeline.md`, `CONTRAT-marius-one-page-extension.md`

---

## 1. Objet

Cette spécification définit la frontière entre :

- représentation AOT ;
- contextualisation déterminée par la route ;
- projections / augmentations à cycle de mutation indépendant ;
- données volatiles produites dans le contexte d'une requête ;
- et `EmissionPlan`, qui constitue l'IR d'exécution situé immédiatement avant la matérialisation en `IoSlice[]`.

Elle ne modifie pas le principe fondamental de Marius :

> PostgreSQL est la source de vérité et le moteur de calcul ; Forge produit les artefacts AOT ; le runtime exécute un plan préétabli et ne reconstruit pas dynamiquement une page.

---

# 2. Invariants de la page complète

Toute représentation AOT d'une page doit rester fonctionnellement complète sans JavaScript.

Elle doit notamment conserver :

- son contenu ;
- sa navigation ;
- son breadcrumb ;
- ses liens ;
- son accessibilité ;
- son référencement.

JavaScript peut accélérer ou enrichir l'expérience, mais ne constitue jamais une dépendance fonctionnelle à la représentation initiale.

Une augmentation éventuelle ne doit donc pas transformer la page AOT en coquille fonctionnellement inutilisable.

---

# 3. Définition de la volatilité

Une donnée n'est pas volatile en raison :

- de sa position dans le DOM ;
- de son caractère visuellement secondaire ;
- de sa petite taille ;
- du fait qu'elle soit affichée dans un composant ;
- ni simplement parce qu'elle change fréquemment.

La volatilité caractérise le **cycle de vie d'une source par rapport à l'artefact AOT**.

Une source est candidate à une matérialisation volatile lorsqu'elle possède un cycle de production, de mutation ou de validité qui doit être indépendant de celui de la représentation AOT qui l'entoure.

La volatilité est donc une propriété de la **source et de son cycle de mutation**, et non du composant HTML qui l'affiche.

---

# 4. Contextualisation par la route

La variation déterminée par la représentation de la route ne constitue pas une volatilité.

Une représentation AOT peut dépendre de son identité de route.

Exemples :

- onglet actif de la navigation principale ;
- `aria-current` ;
- suppression ou modification du lien correspondant à la représentation courante ;
- breadcrumb correspondant à la route ;
- identité de la représentation servie.

Cette contextualisation doit être résolue par Forge.

Le runtime ne doit pas :

- interpréter `.marius` ;
- calculer `.current` ;
- comparer sémantiquement le DOM ;
- reconstruire la navigation ;
- exécuter un `if path == ...` issu du template.

Le runtime reçoit une représentation déjà déterminée.

### Conséquence

Pour une route `/articles`, Forge peut produire une représentation dans laquelle l'entrée `Articles` est marquée comme courante.

Pour `/users`, Forge produit une autre représentation.

Cette variation appartient au corpus AOT et ne constitue pas une dimension volatile.

---

# 5. Volatilité et personnalisation utilisateur

L'authentification ne constitue pas, à elle seule, une définition de la volatilité.

Il serait incorrect d'appliquer automatiquement :

```text
authenticated => volatile
```

ou :

```text
user-specific => volatile
```

La question est de savoir comment la donnée doit être produite et publiée.

## 5.1 Page privée entièrement AOT

Une page dédiée à un utilisateur peut théoriquement être matérialisée comme un artefact AOT propre à cet utilisateur.

Cela peut produire :

```text
User A → artefact AOT
User B → artefact AOT
User C → artefact AOT
...
```

La cardinalité peut alors devenir `O(N)` pour `N` utilisateurs.

Ce modèle n'est pas interdit par principe.

Il devient toutefois architecturalement intéressant lorsque la multiplication des variantes AOT devient disproportionnée par rapport au bénéfice obtenu.

## 5.2 Page privée avec personnalisation volatile

Une autre représentation peut factoriser :

```text
page commune AOT
+
données propres à l'utilisateur
```

La partie personnalisée peut alors être produite comme une source volatile ou comme une projection indépendante selon son cycle de vie.

Cette stratégie évite de transformer chaque contexte utilisateur en variante du même artefact AOT.

## 5.3 Aucune obligation de fragmentation artificielle

Il ne doit pas exister de règle disant :

> toute page privée doit être fragmentée.

Une page privée peut rester une représentation complète.

La décision doit être fondée sur l'architecture de production, la cardinalité des contextes, le cycle de mutation et le coût de matérialisation, pas sur la présence d'une authentification.

---

# 6. Exemple canonique : identité authentifiée

Un encart tel que :

```text
[avatar] Marius
```

dans une navigation autrement commune constitue un candidat naturel à une source volatile.

La représentation peut conceptuellement être :

```text
StaticArtifact
    navigation
    page content
    footer

VolatileSlot
    authenticated identity
        avatar
        display name
```

La page conserve son identité AOT tandis que la source dépendant de la session peut être matérialisée dans le contexte de requête.

Cette source possède son propre cycle de mutation :

- connexion ;
- changement d'identité ;
- changement d'avatar ;
- déconnexion ;
- etc.

Elle ne doit donc pas être transformée en dimension combinatoire de l'artefact AOT.

---

# 7. Factorisation et explosion combinatoire

Le problème architectural n'est pas simplement le nombre de données personnalisées.

Le problème est la multiplication des **dimensions indépendantes de variation**.

Une génération naïve pourrait tendre vers :

```text
routes
× users
× cart states
× notification states
× ...
```

La factorisation recherchée par ADR-011 consiste au contraire à conserver des représentations AOT partagées et à isoler les sources possédant leur propre cycle de vie.

Conceptuellement :

```text
                    ┌── navigation AOT
                    ├── contenu AOT
representation ─────┤
                    ├── identity volatile
                    ├── cart projection
                    └── notifications projection
```

plutôt que de transformer chaque combinaison en page AOT distincte.

Cette factorisation ne signifie toutefois pas que toute donnée personnalisée doit devenir volatile.

---

# 8. Volatile ≠ fragment DOM

Une donnée volatile n'est pas définie comme un « fragment HTML ».

Une source volatile est une unité de production / matérialisation.

Elle peut produire :

- HTML ;
- JSON ;
- XML ;
- texte ;
- ou une autre représentation supportée par le contrat.

La cible DOM, lorsqu'elle existe, appartient au contrat navigateur et non au moteur de projection.

Le serveur ne doit pas connaître :

- un sélecteur CSS ;
- un identifiant DOM ;
- un emplacement dans le document ;
- ni une mécanique HTMX.

---

# 9. `VolatileSlot`

Dans l'IR statique, une source volatile est décrite par :

```rust
SourceSpec::VolatileSlot {
    capacity: u32,
}
```

Cette description ne contient pas la valeur produite.

Elle définit uniquement une origine logique dont la matérialisation sera fournie dans le contexte d'exécution.

Le `capacity` est une borne AOT.

Il ne constitue pas la longueur effectivement émise.

La longueur effective appartient à l'exécution.

---

# 10. Rôle de `EmissionPlan`

`EmissionPlan` est un IR d'exécution.

Il ne décide pas :

- quelle route représente la page ;
- si une donnée est conceptuellement volatile ;
- quelle projection SQL doit être exécutée ;
- comment interpréter `.marius` ;
- comment cibler le DOM ;
- ni comment un navigateur doit appliquer une augmentation.

Ces décisions appartiennent aux couches situées en amont.

`EmissionPlan` prend un ensemble de segments dont les sources et sélections ont été résolues et décrit **ce qui doit être émis pour cette requête**.

La descente est monotone :

```text
Projection
    ↓
Artefact
    ↓
SegmentDescriptor[]
    ↓
MaterializedSource[]
    ↓
EmissionPlan
    ↓
IoSlice[]
    ↓
Backend POSIX
```

Aucune étape inférieure ne remonte vers une abstraction supérieure.

---

# 11. Séparation des responsabilités

## Forge / Static IR

Responsable de :

- l'identité des sources ;
- les `SourceId` ;
- les `SourceKey` ;
- les segments ;
- les sélections ;
- les budgets ;
- les bornes de capacité ;
- les représentations AOT ;
- la contextualisation déterminée par la représentation.

## Request runtime

Responsable de :

- déterminer les valeurs de sélection provenant de la requête ;
- obtenir une génération cohérente pour chaque `SourceKey` ;
- matérialiser les sources ;
- produire les données volatiles ;
- résoudre les sélections en plages physiques ;
- construire l'`EmissionPlan`.

## Backend

Responsable uniquement de :

```text
(ptr, len)
```

ou de l'équivalent d'émission final.

Il ne connaît ni :

- `SourceKey` ;
- `SourceId` ;
- `Mmap` ;
- `Volatile` ;
- `Projection` ;
- `SegmentDescriptor` ;
- `.marius` ;
- ni les concepts métier.

---

# 12. Génération et cohérence

Une requête doit observer une génération cohérente pour chaque `SourceKey` utilisé.

La génération publiée constitue le contexte dans lequel les plages physiques sont valides.

Une plage :

```text
(offset, len)
```

n'est donc pas une propriété AOT permanente.

Elle est valide dans la génération publiée à laquelle elle appartient.

La durée de vie de cette génération doit couvrir toute l'émission correspondante.

Deux `SourceId` différents peuvent référencer le même `SourceKey`.

La résolution doit alors préserver la cohérence de génération.

La résolution unique par `SourceKey` est une exigence de correction, et non simplement une optimisation.

---

# 13. `ResolvedRange`

Après matérialisation d'une source et résolution de sa sélection, le runtime obtient une plage émissible :

```text
(ptr, len)
```

Conceptuellement :

```rust
ResolvedRange {
    ptr,
    len,
}
```

Cette structure ne porte plus la sémantique de la source.

Elle représente uniquement une plage mémoire effectivement émissible.

Elle constitue le niveau immédiatement antérieur à l'émission.

---

# 14. Contrat provisoire de `EmissionPlan`

Un `EmissionPlan` valide doit garantir :

1. que tous les segments prévus par la représentation sont présents ;
2. que chaque segment possède une source résolue ;
3. que chaque sélection nécessaire a été résolue ;
4. que chaque plage possède une durée de vie suffisante pour l'émission ;
5. que les sources statiques observent une génération cohérente ;
6. que les données volatiles ont été matérialisées dans leur capacité autorisée ;
7. que l'ordre des segments est celui imposé par l'artefact ;
8. que le backend peut descendre le plan vers des `IoSlice` sans nouvelle interprétation sémantique ;
9. qu'aucune allocation dynamique non prévue par les bornes AOT n'est nécessaire à cette construction.

Un plan incomplet ne doit pas être présenté au backend comme émissible.

---

# 15. Backend et `IoSlice`

La dernière transformation :

```text
EmissionPlan
    ↓
IoSlice[]
```

est une transformation mécanique.

Elle ne doit pas rechercher de source, interpréter de sélection ou effectuer de logique métier.

À ce niveau :

```text
IoSlice = (ptr, len)
```

Le backend peut alors choisir ultérieurement entre :

- `writev` ;
- `sendmsg` ;
- éventuellement `MSG_ZEROCOPY` ;
- ultérieurement `io_uring` ;
- ou d'autres backends de transport.

Le choix du backend est déterminé en amont par l'`EmissionPlan` / `EmissionBackendKind`, mais le backend lui-même ne doit pas inspecter les sources Marius.

---

# 16. Ce que cette spécification ne tranche pas encore

Les points suivants restent explicitement ouverts :

- représentation finale de `EmissionPlan` en Rust ;
- `const K` versus représentation alternative ;
- représentation exacte de `IoSlice[]` ;
- propriétaire exact de la conversion `EmissionPlan → IoSlice[]` ;
- acquisition et recyclage du `RequestArena` ;
- gestion d'overflow du `RequestArena` ;
- protocole de production des `VolatileSlot` ;
- cycle d'invalidation des sources volatiles ;
- représentation exacte des données volatiles de longueur variable ;
- stratégie HTTP d'une augmentation indépendante ;
- détermination exacte par Forge de la représentation canonique de chaque route ;
- intégration finale avec Hyper/Axum.

Ces points doivent être résolus sans violer les invariants précédents.

---

# 17. Principe directeur

Marius ne doit pas demander :

> « Où cette donnée apparaît-elle dans la page ? »

pour déterminer son statut.

Il doit demander :

> « De quel artefact cette donnée dépend-elle, et quel est son cycle de production, de mutation et de validité ? »

La position dans le document HTML est sans pertinence architecturale.

Une donnée affichée dans le header peut être volatile.

Une donnée affichée au milieu du contenu peut être AOT.

Une variation de navigation peut être AOT parce qu'elle est déterminée par la représentation de route.

Une donnée utilisateur peut rester AOT si le choix de matérialisation le justifie.

La frontière AOT / volatile est donc une frontière de **production et de cycle de vie**, pas une frontière visuelle du document.

---

# 18. Statut

Cette spécification est une base de travail destinée à être confrontée aux documents normatifs existants et à l'implémentation des Phases 0.A à 5.

Elle ne doit pas être considérée comme supérieure à ADR-011 ou au contrat central tant que l'audit documentaire n'est pas effectué.