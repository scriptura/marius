# Spécification Architecturale : Pipeline de Sélection Statique guidé par l'Usage (Static Usage-Driven Selection Pipeline)

**Version :** 0.3
**Cible :** Moteur Marius (Crates `marius-schema`, `marius-projection`, `marius-render`, `marius-parser`)

## 1. Objectif Système

Optimiser le transit de données (SHM ou SQLx) et le pipeline de rendu en éliminant à la compilation les composants du schéma non consommés par les vues (_Data Tree-Shaking_). Le centre de gravité est ancré dans le Schéma : la vue ne définit aucune donnée, n'exécute aucune logique relationnelle et se comporte comme un simple **masque de sélection statique** sur un modèle souverain. L'objectif de Zéro-Allocation (Heap) au runtime est maintenu absolu.

## 2. Invariants et Contrats Fondamentaux

1. **Souveraineté du Schéma (Top-Down Flow) :** Le flux de vérité est strictement descendant : `Schéma (Source) → Projection (Spécialisation) → Vue (Consommation)`. Une vue ne peut en aucun cas découvrir ou instancier une donnée ou une relation absente du modèle sous-jacent. La vue sélectionne, elle ne définit pas.

2. **Grammaire HTML Surchargée (Spécification v1 minimale) :** Le DSL de la vue (`.marius`) adopte la syntaxe visuelle des macro-langages standards (`{{ ... }}` et `{% ... %}`) pour garantir la compatibilité avec l'outillage frontend. **Cependant, le parseur v1 est strictement restreint à cinq constructions** pour empêcher toute logique au runtime :

- `{{ entity.field }}` : Traduit en accès direct (génération de `write_fmt`/`push_str`). Le chaînage complexe et les filtres sont interdits.
- `{% extends "base.marius" %}` : Résolution statique stricte à la compilation ; le fichier est fusionné au build-time.
- `{% block name %}...{% endblock %}` : Substitution statique lors de l'unification de l'AST.
- `{% for item in items %}` : **Strictement interdit en v1** (déclenche une erreur de compilation AOT immédiate).
- `{% if flag %}` : Autorisé _uniquement_ sur des champs DDL strictement booléens issus de PostgreSQL.

3. **Sécurité par l'Existence (Auth AOT) :** L'authentification et les permissions (RLS, `auth_bits`) sont gérées exclusivement par PostgreSQL lors du Write Path. Si l'entité ne satisfait pas la condition de visibilité pour un rôle donné, le Dispatcher ne génère pas le fichier. Le Read Path est passif : l'existence du fichier sur le disque garantit le droit d'accès (`sendfile(2)` ou 404). Le middleware Rust se contente d'injecter l'identité (`user_id`, `auth_bits`) dans une CTE PostgreSQL (`WITH _ctx AS (SELECT set_config(...))`) lors du `fetch_batch`.
4. **Normalisation de l'Indexation :** Les configurations externes (ex: fichiers YAML d'agencement de blocs destinés aux humains) utilisent une indexation base-1. Le compilateur AOT normalise obligatoirement ces valeurs en indexation base-0 avant de générer les structures de données, pointeurs et offsets mémoires internes.

5. **Séparation Taxonomie / Algorithme :** L'arborescence des répertoires de gabarits documente exclusivement la taxonomie métier. Aucun algorithme de routage ou de résolution dynamique ne doit dépendre de cette structure de fichiers au runtime ; l'exécution est pilotée par le flux linéaire d'instructions compilées.

## 3. Topologie du Pipeline de Compilation (AOT)

L'optimisation par l'usage s'effectue lors de la phase de build via une analyse croisée entre les besoins des consommateurs (les vues) et le catalogue de données (le schéma).

```text
[Schéma Global] ──┐
                  ├─> [Intersection AOT] ─> Structure #[repr(C)] Spécialisée
[Vues .marius]  ──┘                         (Uniquement les composants consommés)

```

### Étape 1 : Extraction du Masque d'Usage (`marius-parser` & `fragment-forge`)

Le parseur analyse la syntaxe des fichiers `.marius`. Il extrait pour chaque gabarit un tableau plat des identifiants de champs requis (`{{ entity.field }}`). La Forge en déduit un **Masque de Sélection** explicite pour cette vue.

### Étape 2 : Spécialisation du Layout (`bridge-forge`)

La `bridge-forge` intercepte le Schéma Global et lui applique le Masque de Sélection. Elle génère une structure de données dédiée à cette vue spécifique.

- **Format :** Struct `#[repr(C)]` ou `#[repr(C, packed)]`.

- **Mécanique :** Les composants non référencés par la vue sont éliminés du layout de la projection. La structure ne contient que les octets utiles. Les types dynamiques sont convertis en fenêtres fixes (`[u8; N]`) ou en offsets vers l'arène (`VarlenArena`).

### Étape 3 : Génération Conditionnelle du Transport

La structure `#[repr(C)]` spécialisée impose son layout à la couche d'I/O via la `Bridge-Forge`. Le Core reste agnostique du transport :

- **`#[cfg(feature = "shm")]` (Appliance) :** Génération d'une lecture par arithmétique de pointeurs directs sur le segment `mmap`.

- **`#[cfg(feature = "sqlx")]` (Distribué) :** Génération d'une requête SQL vectorielle filtrée. L'implémentation pousse le flux binaire réseau directement dans les buffers fixes de la structure `#[repr(C)]`.

### Étape 4 : Génération du Pipeline de Rendu (AOT)

L'AST `.marius` est totalement aplati et fusionné. La `Fragment-Forge` génère une fonction Rust monomorphe pure. Chaque instruction statique (HTML) est traduite en un `buffer.push_str("...")`, et chaque instruction dynamique (`{{ field }}`) est traduite en une lecture directe (`buffer.push_str(arena.resolve(&entity.field))`). L'artifice de la Machine Virtuelle est banni.

## 4. Architecture de Résolution au Runtime

Au runtime, le système n'a plus conscience des concepts de "vue", de "moustaches" ou de "schéma relationnel". Il exécute une séquence mécanique :

1. **Phase d'Extraction :** Le transport (SQLx via le Dispatcher) remplit un vecteur de structures `#[repr(C)]` optimisées, en passant l'identité de l'appelant via la session PostgreSQL pour évaluer le RLS.

2. **Phase de Projection :** Le thread de rendu exécute l'appel de fonction natif généré à l'Étape 4. Le layout mémoire est copié séquentiellement dans le buffer de sortie alloué d'un seul trait, sans aucune introspection ni évaluation logique.

## 5. Limites, Protections et Ergonomie de Compilation (Garde-fous)

- **Ergonomie du Compilateur (Anti-Dérive) :** L'usage de la syntaxe standard créera une attente cognitive chez le développeur. La `fragment-forge` doit impérativement intercepter toute tentative d'injection de logique non supportée par la spécification v1 (boucles complexes, filtres) et émettre une erreur AOT chirurgicale expliquant la violation du contrat DOD/Zéro-Allocation.

- **Refus du Graphe :** Si un composant complexe (ex: un compteur d'éléments liés, une agrégation) est nécessaire à la vue, il doit obligatoirement être déclaré comme un composant atomique plat au niveau du Schéma original (`marius-schema`). Sa mise à jour incombe aux pipelines d'écriture PostgreSQL, jamais à la vue.

- **Sécurité de Syntaxe :** Toute tentative d'introduire un mot-clé relationnel (ex: `join`, `where`) dans un bloc `{% %}` provoque un échec immédiat de l'analyse lexicale.

---

Le 7 juin 2026.
