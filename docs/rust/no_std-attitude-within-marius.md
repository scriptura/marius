# Directives de Conception : L'Attitude "no_std" au sein de l'Écosystème Marius

Ce document définit la rigueur de développement pour le moteur Marius. L'objectif est d'adopter une discipline logicielle propre aux systèmes embarqués (`no_std`) tout en exploitant les capacités d'orchestration réseau de la bibliothèque standard (`std`).

---

## 1. Philosophie : "Core Logic, Std Shell"

Le système est segmenté en deux zones d'influence distinctes :

- **Le Shell (I/O & Orchestration) :** Utilise `std` pour la gestion des sockets (Axum), de l'ordonnancement (Tokio) et des drivers (SQLx).
- **Le Core (Traitement & Projection) :** Doit être écrit avec une "attitude `no_std`". Il ne doit dépendre que de `core` et `alloc` (si nécessaire), garantissant un chemin critique sans indirection inutile.

## 2. Invariants de la Zone "Core"

Pour maintenir l'efficacité DOD et AOT, les règles suivantes s'appliquent au pipeline de transformation :

### A. Gestion de la Mémoire (Zero-Allocation Hot Path)

- **Interdiction des allocations dynamiques** dans le cycle du Dispatcher.
- **Utilisation de structures fixes :** Privilégier les tableaux statiques ou les `Box` pré-allouées au démarrage pour les buffers de signalisation.
- **Passage par référence :** Les structures de données circulant entre le Collector et le moteur de rendu Maud doivent être empruntées (`&T`), jamais clonées.

### B. Abstraction des Primitives de Synchronisation

- **Atomics over Mutex :** Le Core doit privilégier `core::sync::atomic` pour la communication inter-thread (ex: le signalement de présence dans le Bit-Vector).
- **Lock-Free :** Un composant du Core ne doit jamais bloquer un thread de l'ordonnancement Tokio par l'attente d'un verrou OS.

### C. Découplage de la Logique de Rendu

- **Templates Maud Purs :** Les macros de rendu doivent transformer des structures mémoire contiguës sans effectuer d'appels système ou de requêtes I/O internes.
- **Pré-calcul :** Toute donnée nécessaire à la projection doit être extraite en amont par le Dispatcher via SQLx.

## 3. Mécanique d'Intégration

| Élément        | Attitude `no_std` (Core)              | Rôle du Shell (`std`)                          |
| :------------- | :------------------------------------ | :--------------------------------------------- |
| **Collector**  | Bit-Vector atomique en mémoire plate. | Réception des signaux `pg_notify`.             |
| **Dispatcher** | Algorithme de scan via `TZCNT`.       | Gestion du `Timer` et des threads Rayon/Tokio. |
| **Projection** | Génération de fragments HTML (Maud).  | Envoi des buffers via HTTP/Axum.               |

---

## 4. Vérification de Conformité

Un module est considéré comme respectant l'attitude Marius s'il peut être extrait dans une crate `no_std` indépendante avec un minimum d'effort. L'usage de `std` dans le Core doit être traité comme une dette technique à justifier par une contrainte de performance réseau insurmontable.

**Résultat attendu :** Une latence de traitement plate, une saturation optimale des caches CPU et une immunité totale contre les ralentissements liés à la gestion dynamique de la mémoire.
