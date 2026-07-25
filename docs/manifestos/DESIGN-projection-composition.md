# DESIGN — Composition AOT des projections HTTP

## La page n'est plus l'unité de compilation

### Statut

**Document de conception fondateur.**

Ce document redéfinit le modèle de composition des réponses HTTP de Marius. Il remplace implicitement le modèle historique de « page HTML compilée » par un modèle de **projections AOT ordonnancées au runtime**.

Les décisions techniques (format mémoire, `IoSlice`, `writev`, indexation, etc.) feront l'objet d'ADR séparées. Le présent document décrit uniquement l'architecture de référence.

---

# 1. Contexte

Les premières itérations de Marius reposaient sur une idée simple : une page HTML est compilée intégralement à l'avance.

Cette approche fonctionne parfaitement tant que toute la page partage les mêmes invariants.

Cependant, l'introduction d'états indépendants met rapidement en évidence une limite structurelle.

Prenons un exemple volontairement simple.

Une navigation possède dix états possibles correspondant à dix onglets actifs.

Un article possède mille variantes correspondant à mille contenus différents.

Compiler une page complète conduit naturellement à produire dix mille versions d'une même page, alors que seulement mille dix invariants existent réellement :

* dix états de navigation ;
* mille contenus d'article.

La duplication ne provient donc pas des données elles-mêmes mais de leur combinaison.

Cette croissance cartésienne devient encore plus problématique dès qu'apparaissent d'autres états :

* session utilisateur ;
* panier ;
* notifications ;
* préférences d'affichage ;
* composants interactifs.

Chaque nouvel état multiplie le nombre de pages compilées sans créer de nouvelle information métier.

Le problème n'est donc pas un problème de performances.

C'est un problème de modélisation.

---

# 2. Changement de paradigme

La réflexion menée autour des projections réactives conduit à abandonner une hypothèse implicite présente depuis les premières versions du projet.

Une page HTTP n'est plus considérée comme une unité de compilation.

La nouvelle unité fondamentale devient la **projection**.

Une projection représente un invariant métier autonome.

Une réponse HTTP devient alors un ordonnancement déterministe de plusieurs projections indépendantes.

Autrement dit, la compilation ne produit plus des pages.

Elle produit un catalogue de projections.

---

# 3. La projection

Une projection possède une identité métier.

Elle n'est jamais définie par son découpage graphique.

Ainsi, les projections suivantes sont légitimes :

* Navigation principale
* Navigation secondaire
* Fil d'Ariane
* Article
* Footer
* Encadré auteur

En revanche, les découpages suivants ne constituent pas des projections :

* titre ;
* image ;
* paragraphe ;
* bouton ;
* colonne de gauche.

Ils décrivent uniquement une structure visuelle.

Une projection existe parce qu'elle possède un cycle de vie propre et un invariant propre.

Elle ne dépend pas de la façon dont elle sera affichée.

Cette distinction est essentielle.

Elle empêche que le système ne dérive progressivement vers un moteur de fragments HTML classiques.

---

# 4. Principe de non-cartésianisation

Aucune projection ne doit être spécialisée par combinaison d'états appartenant à une autre projection.

Autrement dit, Marius interdit volontairement la création d'artefacts représentant des produits cartésiens d'invariants indépendants.

Les situations suivantes sont interdites :

* Navigation × Article
* Navigation × Utilisateur
* Footer × Langue × Session
* Article × Thème × Navigation

À l'inverse, les projections suivantes existent indépendamment :

* Navigation(active = Documentation)
* Navigation(active = Blog)
* Article(id = 42)
* Footer(version = 3)

Le runtime choisit ensuite quelles projections ordonnancer.

La combinaison est réalisée uniquement au moment de la réponse HTTP.

Cette règle supprime naturellement l'explosion combinatoire.

La quantité de données compilées devient proportionnelle au nombre d'invariants, et non au nombre de leurs combinaisons.

---

# 5. Le runtime devient un ordonnanceur

Cette évolution modifie profondément le rôle du runtime.

Le runtime ne construit plus de page.

Il ne concatène plus de chaînes.

Il n'interprète aucun template.

Il ne possède aucune logique de rendu.

Son rôle consiste exclusivement à résoudre une composition déjà déterminée à la compilation.

Conceptuellement, le pipeline devient :

```
URL

↓

RequestEntity

↓

ProjectionIds[]

↓

Ordonnancement

↓

Réponse HTTP
```

Une requête HTTP devient donc une entité décrivant uniquement quelles projections doivent être émises.

Le runtime résout les identifiants de projections puis délègue leur émission au système d'exploitation.

Il agit comme un ordonnanceur mémoire.

Jamais comme un moteur de templates.

---

# 6. Le rôle du système d'exploitation

Une projection compilée est un bloc d'octets immuable.

Le runtime ne cherche jamais à reconstruire une nouvelle chaîne contenant l'ensemble de la page.

Il fournit simplement au noyau un ensemble ordonné de segments mémoire.

L'émission de la réponse HTTP devient alors une opération de diffusion séquentielle de ces segments.

Le serveur ne réalise donc pas un assemblage au sens traditionnel du terme.

Il ordonne simplement plusieurs projections indépendantes.

Cette distinction est importante.

Assembler suppose une mutation mémoire.

Ordonnancer consiste uniquement à résoudre un graphe d'adresses.

---

# 7. Mémoire virtuelle et chemin chaud

Cette architecture ne garantit pas qu'aucune lecture disque n'aura jamais lieu.

Elle garantit une propriété différente.

Le chemin chaud ne dépend jamais explicitement du stockage.

Lorsque les projections sont mappées en mémoire (`mmap`), leur premier accès peut provoquer un défaut de page géré par le système de mémoire virtuelle.

Une fois ces pages présentes en mémoire, le runtime ne réalise plus aucune lecture disque explicite.

Il manipule uniquement des pointeurs vers des données déjà accessibles.

Le moteur délègue entièrement la gestion de cette pagination au système d'exploitation.

Cette délégation constitue un choix volontaire d'architecture.

---

# 8. Deux familles de projections

Toutes les projections ne possèdent pas le même cycle de vie.

Deux familles apparaissent naturellement.

## Projections structurelles

Elles décrivent les invariants publics du site.

Exemples :

* navigation ;
* article ;
* footer ;
* fil d'Ariane ;
* encadrés éditoriaux.

Ces projections sont compilées en AOT.

Elles participent directement au référencement.

Elles constituent le premier octet envoyé au navigateur.

## Projections transactionnelles

Elles décrivent un état propre à une session ou à un utilisateur.

Exemples :

* avatar ;
* panier ;
* notifications ;
* brouillons ;
* informations personnelles.

Ces projections ne doivent jamais invalider les projections structurelles.

Elles sont naturellement servies via HTMX ou tout autre mécanisme de mise à jour incrémentale.

Cette séparation protège les caches tout en empêchant la multiplication des variantes de pages.

---

# 9. Budget de projections

La fragmentation n'est pas une fin en soi.

Créer un très grand nombre de projections indépendantes dégraderait à son tour les performances d'ordonnancement.

Le nombre de projections composant une réponse HTTP doit donc rester suffisamment faible pour que le coût de leur orchestration demeure inférieur au coût qu'aurait représenté leur duplication.

Cette limite n'est pas figée.

Elle dépend :

* du système d'exploitation ;
* des capacités matérielles ;
* du coût réel des appels système ;
* des optimisations futures.

Elle sera déterminée expérimentalement par des campagnes de benchmarks et non par une constante arbitraire.

---

# 10. Conséquences sur l'architecture

Cette évolution modifie plusieurs hypothèses fondamentales.

Le compilateur AOT ne génère plus des pages monolithiques.

Il génère un ensemble cohérent de projections indépendantes.

Le runtime ne produit plus une représentation HTML.

Il résout un graphe de projections déjà compilées.

Les états indépendants cessent d'être combinés à la compilation.

Ils deviennent des projections autonomes.

Le référencement, le cache, le rendu AOT et la réactivité ne sont plus en opposition.

Ils deviennent les conséquences naturelles de la même modélisation.

---

# 11. Principe fondateur

Marius ne cherche plus à compiler des pages.

Il cherche à compiler les invariants métier les plus stables possibles.

Une réponse HTTP n'est alors plus un document construit au runtime.

Elle devient l'ordonnancement déterministe de projections AOT représentant chacune un invariant indépendant.

Les performances ne constituent plus un objectif isolé.

Elles émergent naturellement de la compression structurelle du système.

La page disparaît comme unité de compilation.

La projection devient l'unité fondamentale de l'architecture.

---

_Rédigé le 24 juillet 2026_