# Spécification Architecturale : Pipeline de Sélection Statique et Rendu DOD

**Version :** 0.2.1 (Consolidée avec Bridge RLS)
**Cible :** Moteur Marius (Crates `marius-schema`, `marius-projection`, `marius-render`, `marius-parser`)

## 1. Objectif Système

Optimiser le transit de données et le pipeline de rendu en éliminant à la compilation les composants du schéma non consommés par les vues (_Data Tree-Shaking_). La vue est un masque de sélection statique sur un modèle souverain. L'objectif de Zéro-Allocation (Heap) au runtime est maintenu absolu grâce à un adressage par offsets relatifs sur des blocs mémoire contigus générés directement par PostgreSQL.

## 2. Invariants et Contrats Fondamentaux

- **Souveraineté du Schéma (Top-Down Flow) :** Le flux de vérité est strictement descendant : Schéma → Projection → Vue. La vue sélectionne, elle ne définit pas. Les évaluations complexes (ex: comparaisons de chaînes) sont interdites dans la vue et doivent être abaissées (_lowering_) en drapeaux booléens calculés en amont par le moteur de base de données.
- **Grammaire Surchargée Stricte (Standard Bridé) :** Le DSL de la vue adopte la syntaxe `{{ ... }}` et `{% ... %}` pour le support natif de l'outillage. Toute logique d'évaluation dynamique au runtime est proscrite. L'accès est plat (`{{ entity.title }}`). Le compilateur rejette toute tentative de chaînage complexe (`entity.author.name`) ou d'utilisation d'opérateurs de comparaison (`==`).
- **PostgreSQL comme Registre ECS :** La base de données est le registre central. L'entité fondamentale (ex: `identity.entity`) ne porte que le cycle de vie physique. Les données métier sont des composants (ex: `identity.auth`) rattachés par un `entity_id` immuable.
- **Résolution DOD (Exemple Identity) :** Les permissions sont encodées dans un registre binaire `INT4`. L'évaluation des droits est une opération scalaire `(permissions & p_permission) <> 0` résolue par la base, évitant tout parcours de graphe côté applicatif.
- **Normalisation de l'Indexation :** Les configurations externes (ex: YAML d'agencement pour humains) utilisent une indexation base-1. Le compilateur normalise obligatoirement ces valeurs en base-0 pour générer les offsets mémoires internes.
- **Séparation Taxonomie / Algorithme :** L'arborescence des répertoires du projet documente la taxonomie métier. Aucun algorithme de routage ou de résolution au runtime ne dépend de cette structure spatiale.

## 3. Contrat d'Interface Rust ↔ PostgreSQL (Le Bridge RLS)

Le Bridge Rust (`marius-render`) est strictement apatride (_stateless_). Il ne gère aucune session en RAM. Le filtrage spatial et sécuritaire (Row Level Security) est délégué à PostgreSQL via l'injection d'un contexte de transaction.

### 3.1. Protocole et Sérialisation

- **Extended Protocol Exclusif :** La communication utilise strictement l'Extended Protocol. Le _Simple Query Protocol_ est banni pour empêcher la sérialisation textuelle.
- **Flux Binaire Strict :** Les requêtes retournent un flux d'octets natif correspondant au layout `#[repr(C)]` attendu par la machine virtuelle, interdisant toute phase de parsing en Rust.

### 3.2. Contextualisation par Injection (CTE)

L'état local (`user_id`, `auth_bits`) est injecté sans round-trip supplémentaire via une _Common Table Expression_ croisée avec la fonction de projection.

```sql
WITH _ctx AS (
    SELECT
        set_config('marius.user_id', $1::text, true),
        set_config('marius.auth_bits', $2::text, true)
)
SELECT p.arena_buffer
FROM _ctx
CROSS JOIN projection.page_render($3) p;

```

Le marqueur `true` (`is_local`) garantit la destruction immédiate des variables transactionnelles (GUC) à la fin de la requête, restituant une connexion vierge au pool. La CTE force le _Query Planner_ à séquencer l'affectation mémoire avant le parcours de l'arbre relationnel.

## 4. Empreinte Physique et Layout Mémoire

Pour garantir l'itération sans allocation et la sécurité de lecture sur un segment partagé (mmap ou buffer réseau), la structure `#[repr(C)]` n'utilise aucun pointeur RAM, mais une arithmétique de décalage relatif.

### 4.1. Le concept de Span

Toute donnée de taille dynamique (chaîne de caractères, tableau) est adressée par un `Span` pointant vers une zone plus lointaine dans le même buffer.

```rust
#[repr(C)]
pub struct Span {
    pub offset: u32, // Décalage en octets depuis le DÉBUT de la projection
    pub count: u32,  // Nombre d'éléments (ou d'octets pour un texte)
}

```

### 4.2. L'Arène Contiguë et le Pattern Varlena

Le layout binaire retourné par la base de données est divisé en deux segments stricts pour maximiser la localité de cache et maintenir un _stride_ prédictible :

- **Segment Structuré :** Contient le header de l'entité et les tableaux plats de sous-entités (`[Struct; N]`).
- **Segment Varlena (Tail) :** Contient les chaînes de caractères opaques empilées à la fin du bloc mémoire.

### 4.3. Gestion Branchless des Optionnels (NULL)

L'absence de donnée ne génère pas de branchement conditionnel au runtime.

- **Varlena / Listes :** Un champ NULL est sérialisé en `Span { offset: 0, count: 0 }`. L'Opcode copiera 0 octet (No-Op matériel).
- **Scalaires :** Remplacement par la valeur par défaut du type. Si la vue exige un test conditionnel (`{% if entity.score %}`), le Bridge SQL génère un layout incluant un drapeau booléen (`has_score: bool`) exploité par un Opcode de saut.

## 5. Topologie du Pipeline de Compilation (AOT)

### Étape 1 : Fusion d'AST et Extraction d'Usage (`marius-parser`)

Le parseur résout l'héritage à la compilation (`{% extends %}`, `{% block %}`). Les modificateurs `prepend` et `append` injectent des nœuds statiquement, sans appel à `parent()` au runtime. Le parseur génère ensuite le Masque de Sélection (Bit-Vector) recensant les composants requis.

### Étape 2 : Spécialisation du Layout (`bridge-forge`)

Application du Masque de Sélection sur le Schéma pour générer le `#[repr(C)]`. Les champs non appelés sont éliminés de la structure physique.

### Étape 3 : Compilation en Jeu d'Instructions (Opcodes)

L'AST est aplati en un tableau statique d'instructions. La machine virtuelle est restreinte à 6 instructions :

```rust
pub enum RenderOp {
    /// Copie absolue depuis le buffer binaire du template (HTML brut)
    Static { offset: u32, len: u32 },

    /// Copie d'un champ scalaire (taille fixe) depuis la projection
    DynamicRaw { projection_offset: u32, len: u32 },

    /// Lecture d'un Span et copie d'un Varlena (avec échappement HTML au vol)
    DynamicSpan { span_offset: u32 },

    /// Boucle matérielle sans allocation (Arène Contiguë)
    /// Avance le curseur de `stride` octets à chaque itération
    LoopContiguous {
        span_offset: u32,
        stride: u32,
        inner_opcodes: &'static [RenderOp],
    },

    /// Saut conditionnel. Si la condition est 0, saute le bloc if (`jump_len` instructions)
    JumpIfFalse { condition_offset: u32, jump_len: usize },

    /// Saut inconditionnel (utilisé en fin de bloc if pour esquiver le else)
    Jump { jump_len: usize },
}

```

## 6. Architecture de Résolution au Runtime

Le système d'exécution (`marius-render`) ignore les concepts de "vue" ou de "schéma". Son exécution est un pipeline linéaire :

1. **Extraction :** Récupération de `user_id` et `auth_bits` depuis le réseau.
2. **Acquisition & Binding :** Emprunt d'une connexion au pool et liaison binaire native des arguments.
3. **Fetch Spatial :** PostgreSQL évalue la CTE RLS, filtre le domaine DOD, construit l'Arène et streame le `Vec<u8>`.
4. **Streaming VM :** Le thread charge les `RenderOp` AOT associés à l'URL. Il itère sur les instructions via un pattern match strict et copie la mémoire aveuglément vers le socket TCP de sortie.

## 7. Ergonomie et Garde-fous AOT

- Toute tentative de calcul ou de comparaison (`==`, `!=`, `+`) dans les délimiteurs génère une erreur AOT stipulant la nécessité d'un abaissement en drapeau booléen dans PostgreSQL.
- Toute navigation relationnelle profonde est interceptée. Si la donnée est nécessaire, elle doit être aplatie dans l'archétype parent lors de la génération de la projection.
- Les Opcodes générés pour le mode _shadow_ (vérification croisée en développement) sont strictement identiques aux Opcodes de production.
