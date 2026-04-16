# Spécification : Génération de Structures Rust via Introspection PostgreSQL

**Statut** : Design Proposal  
**Cible** : `marius-core` / `marius-shell`  
**Objectif** : Synchronisation binaire bit-à-bit entre le layout PostgreSQL et les structures Rust.

---

## I. Philosophie : Le Principe du "Miroir Binaire"

L'architecture Marius repose sur la **Source de Vérité Unique (SoT)** située dans PostgreSQL. Toute divergence entre la définition d'un tuple en base de données et sa représentation en mémoire Rust constitue un risque systémique (panique au runtime, corruption de mémoire ou latence de transformation).

### 1. PostgreSQL comme Compilateur de Layout

Dans ce paradigme, le DDL (Data Definition Language) est considéré comme le "code source" de la donnée. Le pipeline de génération transforme ce DDL en structures Rust `#[repr(C)]`. La base de données ne stocke pas seulement la donnée ; elle dicte la forme de l'Engine.

### 2. Élimination de l'Indirection (Zero-Copy Ready)

La génération de code permet d'abandonner le mapping dynamique (type `HashMap` ou `serde_json::Value`) au profit de structures statiques alignées sur le cache CPU. En forçant `#[repr(C)]`, on assure que le padding de la structure Rust correspond exactement à l'alignement des pages Postgres, ouvrant la voie à une projection directe des buffers réseau vers la mémoire applicative.

### 3. Validation à la Compilation (Build-time Safety)

Si une procédure `SECURITY DEFINER` est modifiée ou qu'une colonne est renommée :

1. Le script de build détecte le changement.
2. Le fichier `.rs` est mis à jour.
3. Le compilateur Rust lève une erreur si le code de rendu (Maud) référence un champ obsolète.
   _Le bug meurt avant même d'exister en environnement de test._

---

## II. Proposition d'Implémentation

L'implémentation repose sur le mécanisme `build.rs` de Cargo, s'exécutant avant la phase de compilation du Core.

### 1. Invariant de l'Introspection (SQL)

Le script doit interroger le catalogue système pour extraire l'ordre physique des colonnes, garantissant la cohérence du layout mémoire.

```sql
-- Query cible pour le build.rs
SELECT
    a.attname AS name,
    format_type(a.atttypid, a.atttypmod) AS sql_type,
    a.attnotnull AS is_not_null
FROM pg_attribute a
JOIN pg_class c ON a.attrelid = c.oid
JOIN pg_namespace n ON c.relnamespace = n.oid
WHERE n.nspname = 'content'      -- Schéma défini dans ADR-001
  AND c.relname = 'document'     -- Table cible
  AND a.attnum > 0               -- Exclusion des colonnes système
  AND NOT a.attisdropped
ORDER BY a.attnum;               -- Strictement nécessaire pour repr(C)
```

### 2. Le Mapping de Types (Static Bridge)

Le générateur applique une table de correspondance stricte pour éviter toute ambiguïté de taille mémoire.

| SQL Type          | Rust Type            | Alignment (Bytes) |
| :---------------- | :------------------- | :---------------- |
| `int4`, `integer` | `i32`                | 4                 |
| `int8`, `bigint`  | `i64`                | 8                 |
| `boolean`         | `bool`               | 1                 |
| `uuid`            | `[u8; 16]`           | 16                |
| `timestamp`       | `i64` (Microseconds) | 8                 |

### 3. Structure du Générateur (`build.rs`)

Le script de build effectue les étapes suivantes :

```rust
// Logique simplifiée du build.rs
fn main() {
    // 1. Connexion à la base via SQLx (mode meta-data)
    // 2. Récupération de la liste des tables du schéma 'content'
    // 3. Pour chaque table, générer une struct Rust :

    /* #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct [TableName] {
        pub [FieldName]: [RustType],
        ...
    }
    */

    // 4. Écrire le résultat dans OUT_DIR/generated_schema.rs
}
```

### 4. Gestion des Nullables (`Option<T>`)

Pour maintenir l'attitude `no_std` et `repr(C)`, les champs `NULL` en SQL sont gérés via des types compatibles avec l'interface C.

- **Option A** : Utilisation de `Option<T>` (si T est FFI-safe).
- **Option B (DOD pur)** : Utilisation d'une structure `Nullable<T>` avec un booléen de présence et le padding approprié pour ne pas briser l'alignement de la structure parente.

---

## III. Intégration dans le Pipeline Réactif

Une fois les structures générées, le **Dispatcher** (décrit dans le Manifeste) les utilise comme types de retour pour ses requêtes de batch :

```rust
// Dans le moteur de rendu
let docs: Vec<generated::Document> = sqlx::query_as!(
    generated::Document,
    "SELECT * FROM content.document WHERE id = ANY($1)",
    ids
).fetch_all(&pool).await?;

// Projection immédiate sans transformation
for doc in docs {
    render_document(doc); // Utilise directement le layout binaire
}
```

**Conclusion :** Cette fonctionnalité de génération de code clôt la boucle de votre architecture. Elle transforme une intention de donnée (Postgres) en une réalité binaire (Rust) sans intervention humaine, garantissant la performance maximale promise par le modèle **AOT/DOD**.
