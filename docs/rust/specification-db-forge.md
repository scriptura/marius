# Spécification `marius-db-forge`

**Statut :** Design Validé  
**Cible :** `forge/db-forge/src/lib.rs` + refactoring `crates/core/schema/build.rs`  
**Dépendances directes :** `sqlx`, `tokio`, `marius-fragment-forge`

---

## I. Diagnostic de l'Existant

Le `build.rs` de `crates/core/schema/` contient une implémentation complète et
fonctionnelle de la logique `db-forge`. La crate `forge/db-forge/` est vide.

La mission est une **extraction avec extension**, pas une écriture depuis zéro :

| Existant (inline dans `build.rs`) | À extraire vers `db-forge` | À ajouter |
|---|---|---|
| Introspection `pg_attribute`, `pg_constraint`, `pg_stats` | ✓ | — |
| Mapping SQL → Rust (`TypeMapping`, `map_type()`) | ✓ | — |
| Génération des 6 artefacts par table | ✓ | — |
| Liste `watched` hardcodée | ✓ (temporairement) | Remplacer par `meta.containment_intent` |
| Validation layout vs `intent_density_bytes` | ✗ | À implémenter |
| Détection FK automatique (JOIN varlena) | ✗ | À implémenter |
| Sentinel policy via `pg_description` | ✗ | Phase future |

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

| SQL Type | `row_type` | `store_type` | Sentinel nullable | Taille |
|---|---|---|---|---|
| `int8`, `bigint` | `i64` | `i64` | `-1` | 8 B |
| `int4`, `integer`, `serial` | `i32` | `i32` | `0` | 4 B |
| `int2`, `smallint` | `i16` | `i16` | `0` | 2 B |
| `bool`, `boolean` | `bool` | `bool` | `false` | 1 B |
| `uuid` | `[u8; 16]` | `[u8; 16]` | `[0u8; 16]` | 16 B |
| `timestamptz` | `chrono::DateTime<Utc>` | `i64` µs | `0` | 8 B |
| `timestamp` | `chrono::NaiveDateTime` | `i64` µs | `0` | 8 B |
| `date` | `chrono::NaiveDate` | `i32` jours CE | `0` | 4 B |
| `float4`, `real` | `f32` | `f32` | `0.0` | 4 B |
| `float8`, `double precision` | `f64` | `f64` | `0.0` | 8 B |
| `text`, `varchar`, `jsonb`, `bytea`, `ltree` | `String` | exclu `repr(C)` | — | varlena |
| `pg_lsn` | commentaire Phase2 | commentaire Phase2 | — | 8 B (futur) |

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

# Roadmap `db-forge`

## Phase 0 — Extraction (Refactoring)

Objectif : déplacer la logique inline de `build.rs` vers `forge/db-forge/src/`
sans modifier le comportement. L'output `generated_schema.rs` doit être
bit-pour-bit identique avant et après.

**0.1** Créer la structure de répertoires :
`forge/db-forge/src/codegen/` avec fichiers vides.

**0.2** Déplacer vers `mapping.rs` :
`TypeMapping`, `Column`, `PrimaryKey`, `map_type()`.

**0.3** Déplacer vers `naming.rs` :
`to_pascal()`, `to_screaming()`.

**0.4** Déplacer vers `introspect.rs` :
`fetch_columns()`, `fetch_pk_column()`, `fetch_max_id()`,
`fetch_varlena_cols()`, `parse_check_length_limit()`.

**0.5** Déplacer vers `codegen/row.rs` :
`write_row_struct()`.

**0.6** Déplacer vers `codegen/storage.rs` :
`write_store_struct()`.

**0.7** Déplacer vers `codegen/varlen.rs` :
`write_varlen_owned_struct()`.

**0.8** Déplacer vers `codegen/from_impl.rs` :
`write_from_impl()`.

**0.9** Déplacer vers `codegen/collector.rs` :
`write_collector()`.

**0.10** Déplacer vers `codegen/projection.rs` :
`write_projection_stub()`.

**0.11** Créer `lib.rs` avec tous les `pub use`.

**0.12** Mettre à jour `forge/db-forge/Cargo.toml` :
ajouter `sqlx`, `tokio` en dépendances workspace. Ajouter `chrono` si nécessaire.

**0.13** Mettre à jour `crates/core/schema/Cargo.toml` :
ajouter `marius-db-forge` comme build-dependency.

**0.14** Réécrire `crates/core/schema/build.rs` pour qu'il n'importe que
`marius_db_forge` et `marius_fragment_forge`. La liste `watched` reste
hardcodée temporairement.

**0.15** Vérification : `cargo build` produit un `generated_schema.rs` identique.
Test de non-régression : comparer le fichier généré avant/après refactoring via
checksum.

---

## Phase 1 — Registry Driver

Objectif : supprimer la liste `watched` hardcodée. `build.rs` est piloté
exclusivement par `meta.containment_intent`.

**1.1** Implémenter `ComponentConfig` et `VarlenJoin` dans `registry.rs`.

**1.2** Implémenter `fetch_component_list()` :
requête `meta.containment_intent` filtrée par `to_regclass(component_id) IS NOT NULL`.
Retourne `Vec<ComponentConfig>` trié par `component_id`.

**1.3** Implémenter la détection FK automatique pour `VarlenJoin` :
requête `pg_constraint` + vérification présence colonne varlena dans la table
référencée. Logique : requête unique avec sous-requête sur `pg_attribute` +
`pg_type` pour filtrer `typlen = -1`.

**1.4** Remplacer dans `build.rs` le tableau `watched` par
`fetch_component_list(&pool).await?`.

**1.5** Test de régression :
vérifier que les deux composants actuels (`content.core`, `commerce.product_core`)
produisent la même sortie qu'en Phase 0.

**1.6** Test de pré-déclaration :
insérer un `component_id` fictif dans `containment_intent` sans créer la table.
Vérifier que `fetch_component_list()` l'exclut silencieusement.

---

## Phase 2 — Validation Layout

Objectif : toute divergence entre le schéma DDL et `intent_density_bytes` est un
`cargo:error`. Garantit INV-3.

**2.1** Implémenter dans `validate.rs` la fonction :
```rust
pub fn validate_layout(
    columns:        &[Column],
    intent_density: i16,
) -> Result<(), String>
```

**2.2** Calcul `header_bytes` :
```
n_fixed = columns.iter().filter(|c| map_type(&c.sql_type).is_fixed).count()
header  = ((23 + n_fixed.div_ceil(8)) + 7) / 8 * 8  -- MAXALIGN(8)
```

**2.3** Calcul `padded_size` :
itérer sur les colonnes fixed, accumuler `size_bytes`, arrondir au multiple de
`max_align`. Logique identique à `write_store_struct()`.

**2.4** Comparaison :
`header + padded_size` versus `intent_density as usize`.
En cas de divergence : retourner `Err(message)`.

**2.5** Dans `build.rs` : appel de `validate_layout()` avant la génération.
En cas d'erreur : `println!("cargo:error=...")` + `std::process::exit(1)`.

**2.6** Test d'intégration (optionnel, requiert DATABASE_URL) :
modifier temporairement `intent_density_bytes` d'un composant dans le registre,
vérifier que `cargo build` échoue avec le message attendu.

---

## Phase 3 — Sentinel Policy AOT

Objectif : remplacer les sentinels hardcodés par des annotations `pg_description`,
permettant une politique par colonne.

**3.1** Enrichir `Column` avec un champ `sentinel: Option<String>`.

**3.2** Dans `fetch_columns()` (ou nouvelle fonction `fetch_column_sentinels()`) :
requête `pg_description` pour chaque colonne nullable :
```sql
SELECT col_description(c.oid, a.attnum)
FROM   pg_class c
JOIN   pg_namespace n ON n.oid = c.relnamespace
JOIN   pg_attribute a ON a.attrelid = c.oid
WHERE  n.nspname = $1 AND c.relname = $2 AND a.attnum = $3
```
Extraire `marius:sentinel=<valeur>` du commentaire.

**3.3** Dans `map_type()` : si `Column.sentinel` est `Some(v)`, utiliser `v`
à la place du sentinel par défaut dans `from_expr`.

**3.4** Documenter la convention dans `meta_tooling_guide.md` :
```sql
COMMENT ON COLUMN content.core.author_entity_id IS 'marius:sentinel=0';
```

---

## Phase 4 — Tests Unitaires et d'Intégration

Objectif : couverture minimale garantissant la non-régression.

**4.1** Tests unitaires `mapping.rs` :
`map_type()` pour les 12 types SQL connus. Vérifier `is_fixed`, `size_bytes`,
`alignment` pour chaque entrée du tableau.

**4.2** Tests unitaires `naming.rs` :
`to_pascal("content_core") == "ContentCore"`,
`to_pascal("commerce_product_core") == "CommerceProductCore"`,
`to_screaming("content_core") == "CONTENT_CORE"`.

**4.3** Tests unitaires `validate.rs` :
cas nominal (layout correct), cas divergence (émission d'erreur),
cas table vide (zéro colonnes fixed).

**4.4** Tests d'intégration (marqués `#[ignore]`, requièrent `DATABASE_URL`) :
`fetch_component_list()` retourne au moins 2 composants,
`fetch_columns()` pour `content.core` retourne des colonnes dans l'ordre `attnum ASC`,
`validate_layout()` passe pour tous les composants enregistrés.

---

## Séquence Recommandée

```
Phase 0  (refactoring)     → Phase 1 (registry driver)
                                    ↓
                           Phase 2 (validation layout)
                                    ↓
                           Phase 3 (sentinels)
                                    ↓
                           Phase 4 (tests)
```

Phase 3 est indépendante de Phase 2 et peut être traitée en parallèle.
Phase 4 peut commencer dès Phase 0 terminée pour `mapping.rs` et `naming.rs`.
