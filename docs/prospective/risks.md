Voici la version intégrale, fusionnée et augmentée de la spécification technique de référence pour Marius. Ce document sert de manifeste d'ingénierie et de garde-fou contre les dérives architecturales.

---

# Spécification Technique des Points d'Attention & Doctrine de Couplage : Moteur Marius (Phase 2)

## 1. La Forge (Build-Time) : Le Point de Fléchissement Unique

Le déplacement de la complexité du runtime vers le build-time est un invariant absolu de Marius. La Forge n'est pas un outil d'accompagnement, c'est l'infrastructure de génération qui valide la cohérence structurelle et injecte les contraintes physiques avant compilation.

### Alignement et Padding Mémoire (`#[repr(C)]`)

- **Risque mécanique :** Lors du passage au Niveau 2 (Shared Memory), le cœur Rust exécute une lecture directe (`std::ptr::read`) via un pointeur de mémoire partagée (`POSIX shm`) cartographié par `mmap`. Si l'alignement des octets ou le rembourrage (_padding_) des structures dans PostgreSQL (`pg_attribute`) diverge d'un seul bit par rapport au layout généré en Rust, le CPU lira des données corrompues ou lèvera une faute de bus (_bus error_) sans aucune levée d'exception gérable au runtime.
- **Point d'attention :** La **DB-Forge** doit injecter des macro-tests de symétrie binaire statiques (`static_assertions`) bloquant la compilation si la taille exacte en octets (`mem::size_of`) ou l'alignement (`mem::align_of`) des types Rust ne matchent pas au bit près la structure de la table PostgreSQL correspondante.

### Désynchronisation Inter-Forges

- **Risque de pipeline :** Un changement de schéma DDL dans la base de données peut être correctement traité par la **DB-Forge**, mais rompre silencieusement les structures attendues par la **Bridge-Forge** (requêtes vectorielles) ou la **Fragment-Forge** (macros Maud).
- **Point d'attention :** L'ordonnanceur de build (Nix / Cargo) doit imposer une séquentialité stricte et étanche : `DDL DB -> DB-Forge (Structs) -> Bridge-Forge (Queries) -> Fragment-Forge (Templates)`. Toute erreur de correspondance à un niveau intermédiaire doit invalider instantanément le build de manière déterministe.

---

## 2. Le Chemin de Lecture (Read Path) & Localité de Cache

Le Read Path de Marius élimine les abstractions pour s'exécuter à la vitesse de la mémoire vive. Le code est structuré pour maximiser l'efficacité du matériel, mais reste vulnérable aux ruptures de flux séquentiels.

### Rupture de l'Invariant de Tri

- **Risque matériel :** L'efficacité du _Hardware Prefetcher_ du CPU (L1/L2 data caches) dépend de la contiguïté en RAM des structures traitées. Si le vecteur retourné par le `fetch_batch` ne respecte pas l'invariant de tri `ORDER BY id ASC`, la boucle de rendu de la Fragment-Forge provoquera des sauts d'adresses mémoires aléatoires, écroulant les performances à cause de _cache misses_ répétitifs.
- **Point d'attention :** La **Bridge-Forge** doit interdire la génération de toute requête SQL d'extraction collective dépourvue de la clause explicite `ORDER BY id ASC`.

### Surcharge d'Allocations dans le Rendu

- **Risque système :** La Fragment-Forge pré-calcule la taille statique des chaînes HTML pour allouer les buffers via `String::with_capacity` en une seule passe mémoire. Si des variables dynamiques de taille imprévisible (ex: longs textes libres) sont injectées sans pondération statistique, le buffer subira des réallocations dynamiques (`realloc`), forçant des appels système coûteux et fragmentant la mémoire de la pile.
- **Point d'attention :** Définir une marge de sécurité statique (_over-provisioning_) calculée par la Forge pour chaque slot dynamique basé sur les contraintes de taille (`VARCHAR(N)`) définies dans le DDL.

---

## 3. L'Orchestrator Réactif & I/O Smoothing

L'Orchestrator agit comme un filtre passe-bas pour convertir une tempête d'événements asynchrones en écritures séquentielles par lots. Sa stabilité dépend du calibrage de sa boucle de régulation.

### Oscillation Violente du Régulateur (_Hunting_)

- **Risque algorithmique :** La boucle de régulation (_Adaptive Tick_) ajuste dynamiquement la période de vidage entre `tick_min` (ex: 50ms) et `tick_max` (ex: 2000ms) en mesurant la latence d'I/O du Shell. Si les seuils de saturation sont trop proches ou non filtrés, le système peut entrer en résonance, oscillant violemment d'un extrême à l'autre, ce qui génère des saccades de performances (_jitter_).
- **Point d'attention :** Introduire une logique d'hystérésis mathématique dans l'algorithme d'asservissement du Tick pour amortir les changements de fréquence et stabiliser le débit d'I/O.

### Saturation Topologique du Bit-Vector

- **Risque d'allocation :** Le Collector utilise un Bit-Vector atomique pour enregistrer et dédoublonner les mutations en $O(1)$. Si l'éventail des identifiants d'entités (`EntityId`) est trop large ou fragmenté, la taille de la structure en RAM peut dépasser la capacité des caches processeur, dégradant l'atomicité des opérations de tri.
- **Point d'attention :** Segmenter le Bit-Vector par plages d'IDs denses ou utiliser des structures de type _Roaring Bitmaps_ compactes pour garantir que les opérations binaires restent confinées dans les registres L1/L2 du CPU.

---

## 4. La Machine d'État Hybride (Anti-Clobbering)

Marius rejette le morphing de DOM dynamique au runtime et délègue la gestion de l'état d'écriture au client via le cycle d'états _Draft vs. Committed_.

### Fuite d'Événements et Écrasement DOM (_Clobbering_)

- **Risque d'interface :** L'interception HTMX (`htmx:beforeSwap`) repose sur la présence des attributs `data-state="dirty"` ou `"sync"`. Si la Fragment-Forge génère un nœud réactif dont les éléments modifiables (`contenteditable`) propagent mal leurs événements de focus, HTMX écrasera le tampon de saisie utilisateur lors de la réception d'une projection asynchrone descendante.
- **Point d'attention :** La Fragment-Forge doit imposer une validation topologique stricte lors de l'analyse du DSL (.pug) : tout nœud marqué du jeton d'identité `@` doit encapsuler de manière étanche et atomique l'ensemble de ses zones de saisie et de statut.

### Blocage en État Orphelin (`sync` infini)

- **Risque de flux :** Lorsqu'un composant passe en `data-state="sync"`, les mises à jour serveur sont ignorées pour protéger la saisie. Si la requête HTTP de mutation échoue ou que la base de données rejette la transaction, le composant reste figé dans cet état, bloquant l'interface.
- **Point d'attention :** Le moteur JavaScript du Shell doit injecter un script sentinelle assurant un _Timeout_ automatique qui force le repli de l'élément vers l'état `dirty` avec notification visuelle en cas de non-réponse du serveur.

---

## 5. Topologie et Herméticité du Workspace

L'organisation des répertoires doit sanctuariser la séparation physique entre les outils de génération et l'environnement d'exécution pure.

### Préservation de la Pureté `no_std` du Core

- **Risque d'architecture :** La crate `crates/core/` est une zone de calcul pur à haute performance. L'introduction accidentelle de primitives de synchronisation asynchrones liées à la couche réseau (ex: Tokio) ou au framework HTTP du Shell détruirait l'immuabilité du moteur et sa capacité à interagir directement avec la mémoire partagée.
- **Point d'attention :** Utiliser exclusivement des traits de couplage abstraits générés par les Forges (`AutonomousProjection`) pour isoler le Core des implémentations concrètes d'I/O.

---

## 6. Gouvernance du Couplage Physique (Doctrine DOD)

L'architecture Marius rejette la définition classique du couplage basée sur la séparation sémantique métier (Domain-Driven Design). Le couplage est évalué exclusivement sur son **coût en cycles d'horloge (CPU) et en allocations mémoire**.

Le système recherche et assume un couplage fort si ce dernier protège un invariant de performance ou de déterminisme au runtime.

### 6.1 Couplage Physique Utile (Obligatoire)

Dépendances structurelles strictes déplaçant le coût de traitement du runtime vers le build-time. Elles sont le fondement du modèle haute performance.

- **Génération `DDL -> Rust #[repr(C)]` :** Le schéma de la base de données dicte directement l'agencement et la taille des octets en RAM dans l'application. C'est le prix à payer pour éliminer le parsing et obtenir de la Zéro-Copie.
- **Goulot d'étranglement d'écriture (`SECURITY DEFINER`) :** La concentration de la logique d'écriture dans des fonctions PostgreSQL closes garantit l'émission systématique et exacte du signal `NOTIFY`, sécurisant l'approvisionnement du Collector.

### 6.2 Couplage Logique Fragile (À Contenir)

Dépendances forçant le CPU à évaluer des embranchements logiques au runtime plutôt qu'à dérouler un pipeline de données rectiligne.

- **Logique et Arithmétique dans le DSL (Fragment-Forge) :** L'introduction de conditions complexes (`if`, `match`) dans les templates `.pug`. Ce couplage brise le pré-calcul de capacité des buffers et induit des risques de _branch misprediction_. La transformation de donnée doit être résolue en amont dans les vues SQL de la base de données.

### 6.3 Couplage Mécanique Toxique (À Interdire)

Abstractions introduisant des indirections dynamiques en mémoire sous prétexte de flexibilité sémantique ou de découplage de code.

- **Polymorphisme dynamique au Runtime (Dynamic Dispatch / VTables) :** L'inspection du type d'une entité à l'exécution pour router le rendu HTML. Les sauts de pointeurs induits par les tables virtuelles détruisent le cache d'instructions du processeur. Le routage doit être résolu de manière monomorphe et statique (AOT) lors de la phase de génération par la Forge.

---

## 7. Matrice de Criticité des Risques (Marius Phase 2)

| Risque Évalué                           | Impact Physique                                 | Catégorie de Couplage    | Atténuation Automatisée                                                    |
| --------------------------------------- | ----------------------------------------------- | ------------------------ | -------------------------------------------------------------------------- |
| **Désalignement RAM (SHM)**             | Corruption de données / Crash bus               | **Utile (Assumé)**       | Validation par `static_assertions` à la compilation.                       |
| **Rupture du tri d'extraction**         | Effondrement des lignes de cache CPU            | **Utile (Assumé)**       | Clause `ORDER BY` injectée de force par la Bridge-Forge.                   |
| **Indirection dynamique (VTable)**      | Éclatement du cache d'instructions              | **Toxique (Interdit)**   | Monomorphisation stricte et inlining via Fragment-Forge.                   |
| **Logique métier dans le DSL**          | Réallocations mémoires / _Branch misprediction_ | **Fragile (À Contenir)** | Rejet par le parseur de la Forge ; déport de la logique vers les Vues SQL. |
| **Erreur topologique de l'état client** | Écrasement de la saisie utilisateur             | **Utile (Assumé)**       | Encapsulation structurelle automatisée par la Fragment-Forge.              |
| **Instabilité de l'Adaptive Tick**      | Pics de latence I/O (_Jitter_)                  | —                        | Amortissement par hystérésis dans la boucle de l'Orchestrator.             |
