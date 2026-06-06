# Spécification Architecturale : Pipeline de Sélection Statique guidé par l'Usage (Static Usage-Driven Selection Pipeline)

**Version :** 0.2
**Cible :** Moteur Marius (Crates `marius-schema`, `marius-projection`, `marius-render`, `marius-parser`)

## 1. Objectif Système

Optimiser le transit de données (SHM ou SQLx) et le pipeline de rendu en éliminant à la compilation les composants du schéma non consommés par les vues (_Data Tree-Shaking_). Le centre de gravité est ancré dans le Schéma : la vue ne définit aucune donnée, n'exécute aucune logique relationnelle et se comporte comme un simple **masque de sélection statique** sur un modèle souverain. L'objectif de Zéro-Allocation (Heap) au runtime est maintenu absolu.

## 2. Invariants et Contrats Fondamentaux

1. **Souveraineté du Schéma (Top-Down Flow) :** Le flux de vérité est strictement descendant : `Schéma (Source) → Projection (Spécialisation) → Vue (Consommation)`. Une vue ne peut en aucun cas découvrir ou instancier une donnée ou une relation absente du modèle sous-jacent. La vue sélectionne, elle ne définit pas.
2. **Grammaire HTML Surchargée Stricte (Standard Bridé) :** Le DSL de la vue (`.marius`) adopte la syntaxe visuelle des macro-langages standards (`{{ ... }}` et `{% ... %}`) pour garantir une compatibilité immédiate avec l'outillage frontend (Prettier, LSP HTML). **Cependant, la présence de tout moteur d'évaluation dynamique au runtime est strictement proscrite.**

- _Injection Plate :_ `{{ entity.title }}` traduit un accès absolu à un composant de l'archétype. Le chaînage (`entity.author.name`) et les filtres d'évaluation (`| uppercase`) sont interdits.
- _Topologie de Compilation :_ `{% extends ... %}` et `{% block ... %}` sont des directives exclusivement résolues par la Forge lors de la fusion d'AST. Elles n'existent pas au runtime.

3. **Normalisation de l'Indexation :** Les configurations externes (ex: fichiers YAML d'agencement de blocs destinés aux humains) utilisent une indexation base-1. Le compilateur AOT normalise obligatoirement ces valeurs en indexation base-0 avant de générer les structures de données, pointeurs et offsets mémoires internes.
4. **Séparation Taxonomie / Algorithme :** L'arborescence des répertoires de gabarits documente exclusivement la taxonomie métier. Aucun algorithme de routage ou de résolution dynamique ne doit dépendre de cette structure de fichiers au runtime ; l'exécution est pilotée par le flux linéaire d'instructions compilées.

## 3. Topologie du Pipeline de Compilation (AOT)

L'optimisation par l'usage s'effectue lors de la phase de build via une analyse croisée entre les besoins des consommateurs (les vues) et le catalogue de données (le schéma).

```
[Schéma Global] ──┐
                  ├─> [Intersection AOT] ─> Structure #[repr(C)] Spécialisée
[Vues .marius]  ──┘                         (Uniquement les composants consommés)

```

### Étape 1 : Extraction du Masque d'Usage (`marius-parser` & `fragment-forge`)

Le parseur analyse la syntaxe `{{ }}` des fichiers `.marius`. Il extrait pour chaque gabarit un tableau plat des identifiants de composants requis. La Forge génère un **Masque de Sélection** (représenté sous forme de Bit-Vector statique à la compilation). Le reste du document HTML est traité comme un flux d'octets opaques.

### Étape 2 : Spécialisation du Layout (`bridge-forge`)

La `bridge-forge` intercepte le Schéma Global et lui applique le Masque de Sélection. Elle génère une structure de données dédiée à cette vue spécifique.

- **Format :** Struct `#[repr(C)]` ou `#[repr(C, packed)]`.
- **Mécanique :** Les composants non marqués par la vue sont purement éliminés du layout de cette projection. La structure ne contient que les octets utiles. Les types dynamiques sont convertis en fenêtres fixes (`[u8; N]`) ou en paires d'offsets.

### Étape 3 : Génération Conditionnelle du Transport

La structure `#[repr(C)]` spécialisée impose son layout à la couche d'I/O via la `Bridge-Forge`. Le Core reste agnostique du transport :

- **`#[cfg(feature = "shm")]` (Appliance) :** Génération d'une lecture par arithmétique de pointeurs directs sur le segment `mmap`.
- **`#[cfg(feature = "sqlx")]` (Distribué) :** Génération d'une requête SQL chirurgicale. L'implémentation `FromRow` pousse le flux binaire réseau directement dans les buffers fixes de la structure `#[repr(C)]`.

### Étape 4 : Linéarisation du Rendu

L'AST est aplati. Le template est compilé en un tableau d'`Opcodes` statiques stocké dans `marius-render`. Chaque instruction `{{ composant }}` devient une opération de copie ciblée pointant directement sur l'offset mémoire exact au sein de la structure `#[repr(C)]` générée à l'Étape 2.

## 4. Architecture de Résolution au Runtime

Au runtime, le système n'a plus conscience des concepts de "vue", de "moustaches" ou de "schéma relationnel". Il exécute une séquence mécanique :

1. **Phase de Capture :** Le mécanisme de transport (SHM ou SQLx) remplit la structure `#[repr(C)]` optimisée pour l'usage en cours.
2. **Phase de Streaming :** Le thread de rendu itère sur le tableau d'`Opcodes`. Il assemble le flux HTML de sortie via des copies mémoires contiguës (`ptr::copy_nonoverlapping`) entre le segment statique pré-compilé et la structure de projection.

## 5. Limites, Protections et Ergonomie de Compilation (Garde-fous)

- **Ergonomie du Compilateur (Anti-Dérive) :** L'usage de la syntaxe standard Jinja/Twig créera une attente cognitive chez le développeur. La `fragment-forge` doit impérativement intercepter toute tentative d'injection de logique dynamique (ex: `{{ entity.title | uppercase }}`) et émettre une erreur AOT chirurgicale expliquant la violation du contrat DOD/Zéro-Allocation.
- **Refus du Graphe :** Si un composant complexe (ex: un compteur d'éléments liés, une agrégation) est nécessaire à la vue, il doit obligatoirement être déclaré comme un composant atomique plat au niveau du Schéma original (`marius-schema`). Sa mise à jour incombe aux pipelines d'écriture PostgreSQL, jamais à la vue.
- **Sécurité de Synthaxe :** Toute tentative d'introduire un mot-clé relationnel (ex: `join`, `where`) dans un bloc `{% %}` provoque un échec immédiat de l'analyse lexicale.
