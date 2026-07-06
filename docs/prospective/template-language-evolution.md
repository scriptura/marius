# Évolution du Langage de Templates (Prospective)

## Objectif

Ce document consigne les évolutions prospectives du langage de templates Marius.

Il ne définit **pas** le langage actuel.

Son objectif est de capturer des idées considérées comme compatibles avec les principes architecturaux de Marius afin qu’elles puissent être évaluées ultérieurement sans perdre le raisonnement qui les sous-tend.

La contrainte principale reste inchangée :

> Chaque construction du langage doit admettre un abaissement déterministe AOT vers Rust sans introduire d’interpréteur d’expressions à l’exécution.

Le langage de templates est donc intentionnellement fermé et piloté par le compilateur.

---

# Orientations acceptées

## Opérateur de pipeline

L’opérateur de pipeline est considéré comme la syntaxe préférée pour les transformations de valeurs.

Exemple :

```marius
{{ record.headline | uppercase }}
```

ou

```marius
{{ record.slug | lowercase_ascii }}
```

Le séparateur `|` a été choisi car :

- il reste visuellement léger ;
- il se compose naturellement ;
- chaque transformation est explicite ;
- il évite d’introduire un langage d’expressions généraliste.

Chaque transformation correspond à une primitive connue du compilateur.

Aucune transformation définie par l’utilisateur n’existe.

---

## Catalogue de transformations

Les transformations appartiennent à un catalogue fermé.

Chaque transformation possède :

- une validation statique ;
- un abaissement dédié ;
- un coût d’exécution documenté ;
- une génération déterministe en Rust.

Exemples :

```marius
{{ record.headline | uppercase }}

{{ record.slug | lowercase_ascii }}

{{ record.title | escape_html }}

{{ record.path | escape_url }}
```

Chaque primitive génère du code Rust spécialisé.

Aucun dispatch à l’exécution n’est introduit.

---

## Variantes spécifiques à l’ASCII

Les variantes spécifiques à l’ASCII sont préférées dès qu’elles permettent d’éliminer des allocations.

Exemple :

```marius
{{ record.slug | uppercase_ascii }}
```

peut être abaissé vers une implémentation en flux direct écrivant dans le tampon de sortie.

Les variantes Unicode restent possibles mais présentent des caractéristiques d’exécution différentes.

---

## `join()`

La concaténation conditionnelle doit être représentée par une primitive dédiée plutôt que par des opérateurs d’expressions généraux.

Exemple :

```marius
{{ join(" · ",
    record.firstname,
    record.middlename,
    record.lastname
) }}
```

Sémantique :

- les valeurs optionnelles manquantes sont ignorées ;
- les séparateurs ne sont émis qu’entre les valeurs effectivement rendues ;
- pas de séparateurs dupliqués ;
- pas de séparateur en tête ;
- pas de séparateur en queue.

Le compilateur connaît :

- le séparateur ;
- le nombre d’arguments ;
- le type de chaque argument.

Par conséquent, le code Rust généré peut être complètement déroulé sans introduire de boucles ni de collections temporaires.

Cette primitive résout un problème récurrent de rendu HTML qui ne peut pas être exprimé élégamment avec une simple concaténation de templates.

---

## Classification des coûts

Chaque transformation devrait à terme être classifiée selon ses caractéristiques d’exécution.

Catégories possibles :

| Catégorie              | Allocation temporaire | Exemple                     |
| ---------------------- | --------------------- | --------------------------- |
| Streaming              | Non                   | `escape_html`, `escape_url` |
| Transformation ASCII   | Non                   | `uppercase_ascii`           |
| Transformation Unicode | Allocation bornée     | `uppercase`                 |

Cette classification est orientée documentation mais pourrait ultérieurement faire partie des diagnostics du compilateur.

---

## Transformations Unicode

Les transformations de casse Unicode sont acceptées malgré les allocations temporaires qu’elles nécessitent.

Le coût d’allocation est considéré comme acceptable car :

- il est déterministe ;
- une borne supérieure existe ;
- le compilateur peut documenter ce coût.

Des optimisations futures pourront préallouer des tampons selon le facteur d’expansion Unicode maximal au lieu de reposer sur des réallocations répétées.

---

# Principes de conception

Les ajouts futurs au langage doivent satisfaire les principes suivants.

## Primitives connues du compilateur

Chaque construction doit correspondre à une primitive du compilateur.

Le compilateur sait toujours exactement comment la construction s’abaisse en Rust.

---

## Pas d’interpréteur d’expressions

Le langage évite intentionnellement de devenir un langage de script.

Aucun moteur d’évaluation à l’exécution ne doit apparaître.

---

## Abaissement prévisible

Chaque syntaxe doit s’abaisser en code Rust déterministe.

Le code généré doit rester lisible, optimisable et facile à raisonner.

---

## Exécution orientée données

Dans la mesure du possible, les primitives doivent :

- éviter les allocations sur le tas ;
- éviter les collections temporaires ;
- écrire directement dans le tampon de sortie ;
- rester amicales pour le cache.

---

# Idées reportées

Les idées suivantes restent intéressantes mais n’ont pas été retenues pour le moment.

## Expressions Rust à l’intérieur des templates

Exemple :

```marius
{{ record.title.to_uppercase() }}
```

Rejeté car cela exposerait la syntaxe Rust dans les templates et augmenterait considérablement le couplage entre le langage de templates et le code généré.

---

## Langage d’expressions général

Exemples :

```marius
{{ a + b }}

{{ foo(bar(x)) }}

{{ x ?? y }}

{{ a && b }}
```

Rejeté car cela deviendrait progressivement un second langage de programmation nécessitant :

- un analyseur syntaxique ;
- des règles de précédence ;
- du typage ;
- des diagnostics ;
- de la maintenance du langage.

Cette direction est incompatible avec la philosophie actuelle de Marius.

---

## Opérateurs de concaténation génériques

Exemples :

```marius
{{ a + b }}
```

ou

```marius
{{ concat(a, b, c) }}
```

Une primitive dédiée `join()` est préférée car elle gère naturellement les valeurs optionnelles et les séparateurs tout en restant compatible avec le compilateur.

---

## Construction `group` dédiée

Une construction `group` séparée a été envisagée pour émettre conditionnellement des fragments HTML.

Elle n’a pas été retenue.

La famille de contrôle de flux existante (`if`, et d’éventuelles spécialisations futures telles que `if_some`) fournit déjà une abstraction cohérente sans introduire une nouvelle construction de haut niveau du langage.

---

# Questions ouvertes

Les sujets suivants restent volontairement non tranchés.

- Si les transformations Unicode devraient utiliser des tampons temporaires préalloués selon un facteur d’expansion maximal documenté.
- Si les diagnostics du compilateur devraient exposer le profil mémoire des transformations de templates.
- Si de futures spécialisations de contrôle de flux (par exemple `if_some`) devraient être introduites en tant que sucre syntaxique par-dessus la sémantique `if` existante.

---

_5 juillet 2026_
