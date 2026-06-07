# Manifeste d'Ingénierie : L'Attitude "no_std" au sein de l'Écosystème Marius

Ce document définit la rigueur de développement pour le moteur Marius. L'objectif est d'adopter une discipline logicielle propre aux systèmes embarqués (`no_std`) tout en exploitant les capacités d'orchestration réseau de la bibliothèque standard (`std`).

---

## 1. Philosophie : "Core Logic, Std Shell"

Le système est segmenté en deux zones d'influence distinctes :

- **Le Shell (I/O & Orchestration) :** Utilise `std` pour la gestion des sockets (Axum), de l'ordonnancement (Tokio) et des drivers (SQLx).

- **Le Core (Traitement & Projection) :** Doit être écrit avec une "attitude `no_std`". Il ne doit dépendre que de `core` et `alloc` (si strictement nécessaire pour l'initialisation), garantissant un chemin critique sans indirection inutile.

## 2. Invariants de la Zone "Core"

Pour maintenir l'efficacité DOD (_Data-Oriented Design_) et AOT (_Ahead-Of-Time_), les règles suivantes s'appliquent au pipeline de transformation :

### A. Gestion de la Mémoire (Zero-Allocation Hot Path)

- **Interdiction des allocations dynamiques** dans le cycle du Dispatcher.

- **Utilisation de structures fixes :** Privilégier les tableaux statiques ou les buffers pré-alloués (ex: `Box` ou arènes) au démarrage pour les signaux d'événements.

- **Passage par référence stricte :** Les structures de données circulant entre le Collector et le pipeline de rendu généré (via le préprocesseur `.marius`) doivent être empruntées (`&T`), jamais clonées.

### B. Abstraction des Primitives de Synchronisation

- **Atomics over Mutex :** Le Core doit privilégier `core::sync::atomic` pour la communication inter-thread (ex: le signalement de présence dans le Bit-Vector).

- **Lock-Free :** Un composant du Core ne doit jamais bloquer un thread de l'ordonnancement Tokio par l'attente d'un verrou OS (_Operating System_).

### C. Découplage de la Logique de Rendu

- **Templates `.marius` Compilés (AOT) :** Le code Rust natif (`push_str` / `write_fmt`) généré au moment de la compilation par le préprocesseur doit transformer des structures mémoire contiguës sans effectuer d'appels système ou de requêtes I/O internes.

- **Pré-calcul :** Toute donnée nécessaire à la projection doit être extraite en amont par le Dispatcher via SQLx.

## 3. Mécanique d'Intégration

| Élément       | Attitude `no_std` (Core)              | Rôle du Shell (`std`) |
| ------------- | ------------------------------------- | --------------------- |
| **Collector** | Bit-Vector atomique en mémoire plate. |

| Réception des signaux `pg_notify`.

|
| **Dispatcher** | Algorithme de scan via `TZCNT` (Count Trailing Zeros).

| Gestion du Timer et des threads d'arrière-plan.

|
| **Projection** | Génération de fragments HTML (via code natif issu de `.marius`).

| Écriture sur disque et streaming différé via `sendfile(2)`. |

---

## 4. Vérification de Conformité

Un module est considéré comme respectant l'attitude Marius s'il peut être extrait dans une crate `no_std` indépendante avec un minimum d'effort. L'usage de `std` dans le Core doit être traité comme une dette technique à justifier par une contrainte de performance réseau insurmontable.

**Résultat attendu :** Une latence de traitement plate, une saturation optimale des caches CPU (L1/L2) et une immunité totale contre les ralentissements liés à la gestion dynamique de la mémoire (Garbage Collection ou allocateur global).
