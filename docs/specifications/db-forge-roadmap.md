# Roadmap `db-forge`

## Phase 0 — Extraction (Refactoring)

Objectif : déplacer la logique inline de `build.rs` vers `crates/forge/db-forge/src/`
sans modifier le comportement. L'output `generated_schema.rs` doit être
bit-pour-bit identique avant et après.

**0.1** Créer la structure de répertoires :
`crates/forge/db-forge/src/codegen/` avec fichiers vides.

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

**0.12** Mettre à jour `crates/forge/db-forge/Cargo.toml` :
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

---

Le 12 juin 2026.
