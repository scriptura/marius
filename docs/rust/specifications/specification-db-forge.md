# Spécification `marius-db-forge`

**Statut :** Design Validé  
**Cible :** `crates/forge/db-forge/src/lib.rs` + refactoring `crates/core/schema/build.rs`  
**Dépendances directes :** `sqlx`, `tokio`, `marius-fragment-forge`

---

## I. Diagnostic de l'Existant

Le `build.rs` de `crates/core/schema/` contient une implémentation complète et
fonctionnelle de la logique `db-forge`. La crate `crates/forge/db-forge/` est vide.

La mission est une **extraction avec extension**, pas une écriture depuis zéro :

| Existant (inline dans `build.rs`)                         | À extraire vers `db-forge` | À ajouter                               |
| --------------------------------------------------------- | -------------------------- | --------------------------------------- |
| Introspection `pg_attribute`, `pg_constraint`, `pg_stats` | ✓                          | —                                       |
| Mapping SQL → Rust (`TypeMapping`, `map_type()`)          | ✓                          | —                                       |
| Génération des 6 artefacts par table                      | ✓                          | —                                       |
| Liste `watched` hardcodée                                 | ✓ (temporairement)         | Remplacer par `meta.containment_intent` |
| Validation layout vs `intent_density_bytes`               | ✗                          | À implémenter                           |
| Détection FK automatique (JOIN varlena)                   | ✗                          | À implémenter                           |
| Sentinel policy via `pg_description`                      | ✗                          | Phase future                            |

---

## II. Périmètre Strict

**Responsabilité de `db-forge` :**

- Lire `meta.containment_intent` → liste des composants surveillés
- Introspecter `pg_attribute`, `pg_constraint`, `information_schema`, `pg_stats`,
  `pg_description`
- Mapper les types SQL → Rust (fixed-length et varlena)
- Générer les types et infrastructure Rust (`{Name}Row`, `{Name}StorageRow`,
  `{Name}VarlenOwned`, `From`, `Collector`, stub `Projection`)
- Valider le layout calculé contre `intent_density_bytes`

**Hors périmètre (frontières strictes) :**

- Corps `render()` → responsabilité exclusive de `fragment-forge`
- Constantes `STATIC_CAP`, `DYNAMIC_CAP`, `TOTAL_CAP` → `fragment-forge`
- Écriture sur disque → `build.rs` (orchestrateur mince)

---

## III. Invariants Architecturaux

**INV-1 — Source de Vérité Unique.** La liste des composants surveillés est lue
depuis `meta.containment_intent`. Aucun composant n'est déclaré en dur dans
`build.rs` ni dans `db-forge`. Le registre est la loi.

**INV-2 — Symétrie Mécanique.** L'ordre des champs dans `{Name}StorageRow` est
dicté par `attnum ASC` (ordre physique du heap tuple PostgreSQL). La Forge garantit
cette cohérence ; aucun tri alternatif n'est appliqué.

**INV-3 — Validation AOT.** La taille paddée calculée par `db-forge` est comparée
à `intent_density_bytes - header_bytes`. Toute divergence est un `cargo:error`,
pas un warning.

**INV-4 — Isolation des Responsabilités.** L'interface entre `db-forge` et
`fragment-forge` est `Vec<FieldSpec>` + `Vec<VarlenField>`. Aucune des deux forges
n'importe les types internes de l'autre.

---

## IV. Types Publics

### `ComponentConfig`

Représente une ligne `meta.containment_intent` enrichie de la détection FK.
C'est l'unité d'entrée du pipeline de génération pour chaque table.

```rust
// forge/db-forge/src/registry.rs

pub struct ComponentConfig {
    pub schema:            String,
    pub table:             String,
    pub intent_density:    i16,           // meta.containment_intent.intent_density_bytes
    pub rls_guard_bitmask: Option<i32>,   // meta.containment_intent.rls_guard_bitmask
    pub varlena_join:      Option<VarlenJoin>,  // détecté via pg_constraint FK
}

pub struct VarlenJoin {
    pub schema: String,
    pub table:  String,
    pub fk_col: String,
}
```

### `Column` (existant, à déplacer)

```rust
// forge/db-forge/src/mapping.rs

pub struct Column {
    pub attnum:     i16,
    pub name:       String,
    pub sql_type:   String,
    pub is_notnull: bool,
}
```

### `TypeMapping` (existant, à déplacer)

```rust
// forge/db-forge/src/mapping.rs

pub struct TypeMapping {
    pub row_type:   &'static str,  // type dans Row (sqlx)
    pub store_type: &'static str,  // type dans StorageRow (#[repr(C)])
    pub from_expr:  &'static str,  // expr Row → StorageRow (placeholder "{field}")
    pub is_fixed:   bool,
    pub size_bytes: usize,
    pub alignment:  usize,
}
```

### `PrimaryKey` (existant, à déplacer)

```rust
// forge/db-forge/src/mapping.rs

pub enum PrimaryKey {
    Single(String),
    Composite,
}
```

---

## V. API Publique du Crate

```rust
// forge/db-forge/src/lib.rs

// Introspection
pub use introspect::{
    fetch_columns, fetch_pk_column, fetch_max_id, fetch_varlena_cols,
};

// Mapping de types
pub use mapping::{map_type, TypeMapping, Column, PrimaryKey};

// Nommage
pub use naming::{to_pascal, to_screaming};

// Registre
pub use registry::{fetch_component_list, ComponentConfig, VarlenJoin};

// Validation
pub use validate::validate_layout;

// Génération
pub use codegen::{
    write_section_header,
    write_row_struct,
    write_store_struct,
    write_varlen_owned_struct,
    write_from_impl,
    write_collector,
    write_projection_stub,
};
```

---

## VI. Pipeline d'Introspection

### A. Lecture du registre (`meta.containment_intent`)

```sql
SELECT component_id, intent_density_bytes, rls_guard_bitmask
FROM   meta.containment_intent
WHERE  to_regclass(component_id) IS NOT NULL
ORDER  BY component_id
```

`to_regclass(component_id) IS NOT NULL` exclut les composants pré-déclarés dont
la table n'existe pas encore (workflow migration). `component_id` est splitté sur
`'.'` pour extraire `(schema, table)`.

### B. Détection du JOIN varlena (`pg_constraint`)

Pour chaque `(schema, table)` extrait du registre, `db-forge` détermine
automatiquement l'existence d'une table jointe porteuse de colonnes varlena.

```sql
SELECT
    ns2.nspname   AS ref_schema,
    cls2.relname  AS ref_table,
    att.attname   AS fk_col
FROM   pg_constraint  con
JOIN   pg_class       cls  ON cls.oid  = con.conrelid
JOIN   pg_namespace   ns   ON ns.oid   = cls.relnamespace
JOIN   pg_class       cls2 ON cls2.oid = con.confrelid
JOIN   pg_namespace   ns2  ON ns2.oid  = cls2.relnamespace
JOIN   pg_attribute   att  ON att.attrelid = cls.oid
                           AND att.attnum   = con.conkey[1]
WHERE  ns.nspname   = $1
  AND  cls.relname  = $2
  AND  con.contype  = 'f'
  AND  array_length(con.conkey, 1) = 1
```

La table référencée n'est retenue comme `VarlenJoin` que si elle contient au moins
une colonne varlena (`typlen = -1` dans `pg_type`).

En cas de FK multiples, la première FK vers une table avec colonnes varlena est
retenue. Si zéro FK varlena : `varlena_join = None`.

### C. Introspection colonnes (`pg_attribute`)

Requête existante dans `build.rs` — extraction directe vers `introspect.rs`.
Retourne `Vec<Column>` trié `attnum ASC` (INV-2).

### D. Introspection varlena (`pg_attribute` + `pg_stats` + `pg_description`)

Requête existante dans `build.rs` — extraction directe vers `introspect.rs`.
Retourne `Vec<VarlenField>` avec `max_len`, `is_pre_escaped`, et valeur moyenne
observée pour la validation de pression sur `DYNAMIC_CAP`.

### E. Validation layout (`validate.rs`)

```
n_fixed_cols    = columns.iter().filter(|c| map_type(&c.sql_type).is_fixed).count()
header_bytes    = MAXALIGN(23 + ceil(n_fixed_cols / 8))
                  où MAXALIGN arrondit au multiple de 8 supérieur
padded_size     = somme(size_bytes des cols fixed) arrondie au multiple de max_align
computed_total  = header_bytes + padded_size
registered      = component_config.intent_density

si computed_total != registered :
    cargo:error=DB-Forge [{schema}.{table}]: layout diverge du registre.
                Calculé={computed_total}B, Enregistré={registered}B.
                Relancer meta.f_generate_dod_template et mettre à jour containment_intent.
```

Note : `intent_density_bytes` dans le registre inclut le header heap PostgreSQL
(`f_generate_dod_template` le calcule ainsi). La comparaison porte sur le total.

---

## VII. Mapping de Types

Le tableau complet est extrait de `build.rs` tel quel. Aucune modification.

| SQL Type                                     | `row_type`              | `store_type`       | Sentinel nullable | Taille      |
| -------------------------------------------- | ----------------------- | ------------------ | ----------------- | ----------- |
| `int8`, `bigint`                             | `i64`                   | `i64`              | `-1`              | 8 B         |
| `int4`, `integer`, `serial`                  | `i32`                   | `i32`              | `0`               | 4 B         |
| `int2`, `smallint`                           | `i16`                   | `i16`              | `0`               | 2 B         |
| `bool`, `boolean`                            | `bool`                  | `bool`             | `false`           | 1 B         |
| `uuid`                                       | `[u8; 16]`              | `[u8; 16]`         | `[0u8; 16]`       | 16 B        |
| `timestamptz`                                | `chrono::DateTime<Utc>` | `i64` µs           | `0`               | 8 B         |
| `timestamp`                                  | `chrono::NaiveDateTime` | `i64` µs           | `0`               | 8 B         |
| `date`                                       | `chrono::NaiveDate`     | `i32` jours CE     | `0`               | 4 B         |
| `float4`, `real`                             | `f32`                   | `f32`              | `0.0`             | 4 B         |
| `float8`, `double precision`                 | `f64`                   | `f64`              | `0.0`             | 8 B         |
| `text`, `varchar`, `jsonb`, `bytea`, `ltree` | `String`                | exclu `repr(C)`    | —                 | varlena     |
| `pg_lsn`                                     | commentaire Phase2      | commentaire Phase2 | —                 | 8 B (futur) |

Les sentinels sont des valeurs par défaut de Phase 1. Phase 3 introduit la lecture
depuis `pg_description` (`COMMENT ON COLUMN ... IS 'marius:sentinel=-1'`).

---

## VIII. Artefacts Générés par Table

Six artefacts sont émis pour chaque composant, dans l'ordre suivant :

1. **`{Name}Row`** — struct `sqlx::FromRow`, non-repr(C). Champs fixed NOT NULL +
   nullable `Option<T>`. Champs varlena table principale : `String` ou
   `Option<String>`. Champs varlena JOIN : `Option<String>` (LEFT JOIN possible NULL).

2. **`{Name}StorageRow`** — struct `#[repr(C), Clone, Copy, Default]`. Champs
   fixed uniquement. Nullable → sentinel. Deux `static_assertions` compilateur :
   `size_of == padded_size` et `align_of == max_align`.

3. **`{Name}VarlenOwned`** — struct `Debug, Default`, champs `Option<String>`.
   Absente si aucun varlena JOIN (`type VarlenOwned = ()` dans le trait).

4. **`From<{Name}Row> for {Name}StorageRow`** — conversion directe des champs
   fixed. Conversions chrono inline. Sentinels pour nullable.

5. **`{SCREAMING}_COLLECTOR`** — static `Collector<MAX, WORDS>`. Absent si PK
   composite. `MAX` et `WORDS` calculés depuis `fetch_max_id()` (marge 20% +
   arrondi puissance de deux).

6. **`impl Projection for {Name}Projection`** — stub complet :
   `type Record`, `type VarlenOwned`, `fetch_batch()`, `render()`, `artifact_path()`.
   Le corps de `render()` est fourni par `fragment-forge` via `generate_render()`.

---

## IX. Architecture Interne

```
forge/db-forge/src/
├── lib.rs              re-exports publics
├── introspect.rs       fetch_columns(), fetch_pk_column(), fetch_max_id(),
│                       fetch_varlena_cols(), parse_check_length_limit()
├── mapping.rs          TypeMapping, Column, PrimaryKey, map_type()
├── naming.rs           to_pascal(), to_screaming()
├── registry.rs         ComponentConfig, VarlenJoin, fetch_component_list()
├── validate.rs         validate_layout()
└── codegen/
    ├── mod.rs          write_section_header()
    ├── row.rs          write_row_struct()
    ├── storage.rs      write_store_struct() + static_assertions
    ├── varlen.rs       write_varlen_owned_struct()
    ├── from_impl.rs    write_from_impl()
    ├── collector.rs    write_collector()
    └── projection.rs   write_projection_stub()
```

---

## X. `build.rs` Post-Refactoring

```rust
// crates/core/schema/build.rs — après Phase 0 + Phase 1

use marius_db_forge::{
    fetch_component_list, fetch_columns, fetch_pk_column, fetch_max_id,
    fetch_varlena_cols, validate_layout, map_type,
    write_section_header, write_row_struct, write_store_struct,
    write_varlen_owned_struct, write_from_impl, write_collector,
    write_projection_stub,
};
use marius_fragment_forge::{
    FieldSpec, FieldKind, generate_render, generate_capacity_consts,
    generated_file_header,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let pool       = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    let components = fetch_component_list(&pool).await?;
    let mut output = String::from(generated_file_header());

    for comp in &components {
        let columns = fetch_columns(&pool, &comp.schema, &comp.table).await?;
        let pk      = fetch_pk_column(&pool, &comp.schema, &comp.table).await?;
        let max_id  = match &pk {
            marius_db_forge::PrimaryKey::Single(col) =>
                Some(fetch_max_id(&pool, &comp.schema, &comp.table, col).await?),
            marius_db_forge::PrimaryKey::Composite => None,
        };
        let varlena = match &comp.varlena_join {
            Some(j) => fetch_varlena_cols(&pool, &j.schema, &j.table).await?,
            None    => vec![],
        };

        validate_layout(&columns, comp.intent_density)?;

        write_section_header(&mut output, &comp.schema, &comp.table, &pk);
        write_row_struct(&mut output, &comp.schema, &comp.table, &columns, &varlena);
        write_store_struct(&mut output, &comp.schema, &comp.table, &columns);
        write_varlen_owned_struct(&mut output, &comp.schema, &comp.table, &varlena);
        write_from_impl(&mut output, &comp.schema, &comp.table, &columns);

        if let (marius_db_forge::PrimaryKey::Single(col), Some(max)) = (&pk, max_id) {
            write_collector(&mut output, &comp.schema, &comp.table, col, max);
        }

        write_projection_stub(
            &mut output, &comp.schema, &comp.table,
            &columns, &pk, &varlena,
            comp.varlena_join.as_ref().map(|j| (j.schema.as_str(), j.table.as_str(), j.fk_col.as_str())),
        );
    }

    let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR")?)
        .join("generated_schema.rs");
    std::fs::write(&out_path, &output)?;
    Ok(())
}
```

---

## XI. Points de Vigilance

**Sentinels nullable.** Le choix du sentinel (`-1` pour `int8`, `0` pour `int4`)
est domain-specific et peut être incorrect selon la contrainte CHECK de la colonne.
La Phase 3 résout cela via `pg_description`. En Phase 1, tout sentinel incorrect est
visible via les tests `no-realloc` et les tests de ratio dans `crates/core/schema/`.

**FK multiples vers tables varlena.** Si une table a deux FK vers deux tables
varlena, seule la première est retenue. Ce cas n'existe pas dans le schéma actuel.
Extension : stocker les JOIN multiples dans une colonne `TEXT[]` de `containment_intent`,
ou introduire une table de liaison `meta.varlena_join_config`.

**`pg_lsn` (Phase 2).** Les colonnes `pg_lsn` sont commentées dans le code généré.
Le mapping `u64` via mmap sera introduit en Phase 2 (shared memory) sans impact
sur les artefacts Phase 1.

**PK non-entière.** Un composant avec PK `uuid` ne peut pas utiliser le
`Collector` (bit-vector sur domaine entier). `fetch_pk_column()` retourne
`PrimaryKey::Composite` dans ce cas ; le Collector est omis.

---

Le 12 juin 2026.
