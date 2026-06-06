# Spécification Architecturale : Pipeline de Sélection Statique guidé par l'Usage (Static Usage-Driven Selection Pipeline)

**Version :** 0.1-B (Alternative)
**Cible :** Moteur Marius (Crates `marius-schema`, `marius-projection`, `marius-render`)

## 1. Objectif Système

Optimiser le transit de données (SHM ou SQLx) et le pipeline de rendu en éliminant à la compilation les composants du schéma non consommés par les vues (_Data Tree-Shaking_). Contrairement au modèle _View-Driven_, le centre de gravité reste ancré dans le Schéma : la vue ne définit aucune donnée, n'exécute aucune logique relationnelle (jointure, agrégation) et se comporte comme un simple **masque de sélection statique** sur un modèle souverain. L'objectif de Zéro-Allocation au runtime est maintenu.

## 2. Invariants et Contrats Fondamentaux

1. **Souveraineté du Schéma (Top-Down Flow) :** Le flux de vérité est strictement descendant : `Schéma (Source) → Projection (Spécialisation) → Vue (Consommation)`. Une vue ne peut en aucun cas découvrir ou instancier une donnée ou une relation absente du modèle sous-jacent. **La vue sélectionne, elle ne définit pas.**
2. **Grammaire d'Expression Plate (Flat Grammar) :** Pour interdire toute dérive vers un moteur d'exécution de graphe au runtime (type GraphQL), le DSL de la vue (syntaxe d'indentation `.marius`) interdit le chaînage de membres (`.`) et les appels de fonctions. L'accès aux données est strictement plat.

- _Valide :_ `h1 = entity.title` (Sélection directe d'un composant de l'archétype).
- _Non-Valide :_ `p = entity.comments.count()` ou `span = entity.author.name` (Navigation/Calcul de graphe).

3. **Normalisation de l'Indexation :** Les configurations externes (ex: fichiers YAML d'agencement de blocs destinés aux humains) utilisent une indexation base-1. Le compilateur AOT (Forges) normalise obligatoirement ces valeurs en indexation base-0 avant de générer les structures de données, pointeurs et offsets mémoires internes.
4. **Séparation Taxonomie / Algorithme :** L'arborescence des répertoires de gabarits documente exclusivement la taxonomie métier. Aucun algorithme de routage ou de résolution dynamique ne doit dépendre de cette structure de fichiers au runtime ; l'exécution est pilotée de manière déterministe par le flux linéaire d'instructions compilées.

## 3. Topologie du Pipeline de Compilation (AOT)

L'optimisation par l'usage s'effectue lors de la phase de build via une analyse croisée entre les besoins des consommateurs (les vues) et le catalogue de données (le schéma).

```
[Schéma Global] ──┐
                  ├─> [Intersection AOT] ─> Structure #[repr(C)] Spécialisée
[Vues .marius]  ──┘                         (Uniquement les composants consommés)

```

### Étape 1 : Extraction du Masque d'Usage (`fragment-forge`)

La `fragment-forge` analyse les fichiers `.marius`. Elle extrait pour chaque gabarit un tableau plat des identifiants de composants requis. Elle génère un **Masque de Sélection** (représenté sous forme de Bit-Vector statique à la compilation).

### Étape 2 : Spécialisation du Layout (`db-forge` & `bridge-forge`)

La `bridge-forge` intercepte le Schéma Global et lui applique le Masque de Sélection. Elle génère dans `marius-projection` une structure de données dédiée à cette vue spécifique.

- **Format :** Struct `#[repr(C)]` ou `#[repr(C, packed)]`.
- **Mécanique :** Les composants non marqués par la vue sont purement éliminés du layout de cette projection. La structure ne contient que les octets utiles. Les types dynamiques sont convertis en fenêtres fixes (`[u8; N]`) ou en paires d'offsets.

### Étape 3 : Génération Conditionnelle du Transport

La structure `#[repr(C)]` spécialisée impose son layout à la couche d'I/O via la `Bridge-Forge`. Le Core reste agnostique du transport :

- **`#[cfg(feature = "shm")]` (Appliance) :** Génération d'une lecture par arithmétique de pointeurs directs sur le segment `mmap`. Le layout de la projection correspond exactement à la structure du miroir binaire préalablement filtré.
- **`#[cfg(feature = "sqlx")]` (Distribué) :** Génération d'une requête SQL chirurgicale qui ne sélectionne que les colonnes correspondant aux composants actifs du masque. L'implémentation `FromRow` pousse le flux binaire réseau directement dans les buffers fixes de la structure `#[repr(C)]` (Zéro-Allocation au runtime).

### Étape 4 : Linéarisation du Rendu

Le template est compilé en un tableau d'`Opcodes` statiques stocké dans `marius-render`. Chaque instruction d'injection dynamique pointe directement sur l'offset mémoire exact du composant au sein de la structure `#[repr(C)]` générée à l'Étape 2.

## 4. Architecture de Résolution au Runtime

Au runtime, le système n'a plus conscience des concepts de "vue" ou de "schéma relationnel". Il exécute une séquence mécanique :

1. **Phase de Capture :** Le mécanisme de transport (SHM ou SQLx) remplit la structure `#[repr(C)]` optimisée pour l'usage en cours.
2. **Phase de Streaming :** Le thread de rendu exécute une boucle plate sur le tableau d'`Opcodes`. Il assemble le flux HTML de sortie via des copies mémoires contiguës (`ptr::copy_nonoverlapping`) entre le segment statique et la structure de projection.

## 5. Limites et Protections (Garde-fous de l'Audit)

- Si un composant complexe (ex: un compteur d'éléments liés, une agrégation) est nécessaire à la vue, il doit obligatoirement être déclaré comme un composant atomique plat au niveau du Schéma original (`marius-schema`). Sa mise à jour ou son calcul incombe aux pipelines d'écriture ou aux triggers de persistance, jamais au pipeline de rendu.
- Toute tentative d'introduire un mot-clé relationnel (ex: `join`, `where`, `select`) ou une indirection de table dans le fichier `.marius` doit provoquer une erreur d'analyse à la compilation de la `fragment-forge`.
