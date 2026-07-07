# Meta Tooling Guide — Marius

> **Créé le 17 juin 2026. Vérifié et daté le 7 juillet 2026.**
> **Note de statut** : le §1 (dernière étape) et le §5 (étape 10) décrivent un
> modèle de service obsolète — résolution *à la requête* depuis `store.bin`
> directement (`OnceLock`/`fetch_batch` au premier appel HTTP). Ce modèle a été
> explicitement écarté par ADR-008 (22 juin 2026, postérieur à la dernière
> révision de ce guide) au profit d'une résolution *à l'écriture* : un pack
> HTML pré-rendu (`{table}.bin`, distinct de `{table}_store.bin`), régénéré
> uniquement sur `NOTIFY` Postgres, servi par `pread`. Le reste de ce document
> (registre, annotations, format binaire de `store.bin`, procédure d'ajout de
> composant) reste exact et complémentaire — voir `guide-cycle-de-vie-runtime.md`
> pour le modèle de service réel.

Guide opérationnel du pipeline AOT. Destiné au développeur qui ajoute, modifie
ou débogue un composant dans Marius. Ne documente pas les internals de la Forge
(voir les commentaires inline dans `forge/db-forge/src/`).

---

## 1. Pipeline en trois phases

```
DDL PostgreSQL
    │
    ▼
meta.containment_intent          ← registre des composants
meta.component_varlena_join      ← liaisons varlena
    │
    ▼ cargo build (build.rs → db-forge)
generated_schema.rs              ← types, From impl, Projection impl
    │
    ▼ cargo run --bin marius-dump
{schema}_{table}_store.bin       ← dump binaire AOT (StorageRow + varlena)
    │
    ▼ [OBSOLÈTE — voir note de statut ci-dessus]
    ▼ Le service réel passe par regenerate_and_swap() → pack HTML,
    ▼ déclenché par NOTIFY Postgres, pas par cargo run --bin marius-server
    ▼ ni par un fetch_batch() à la première requête. Détail complet :
    ▼ guide-cycle-de-vie-runtime.md, schéma global.
```

`cargo build` est le seul outil qui touche `generated_schema.rs`.
Ne jamais modifier ce fichier manuellement.

---

## 2. Registre des composants

### 2.1 `meta.containment_intent`

Déclare les tables gérées par le pipeline AOT.

| Colonne                | Type        | Description                                                                                                                                                 |
| ---------------------- | ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `component_id`         | `text` PK   | `schema.table` — doit correspondre à une table existante                                                                                                    |
| `intent_density_bytes` | `int2`      | Empreinte matérielle de la table dans PostgreSQL : header tuple MVCC + payload fixed-length aligné. **Distinct du `stride` du store.bin** (voir section 6). |
| `rls_guard_bitmask`    | `int4` NULL | Réservé Guard-Forge                                                                                                                                         |

```sql
-- Ajouter un composant
INSERT INTO meta.containment_intent (component_id, intent_density_bytes)
VALUES ('content.article', 0);  -- 0 = densité non encore validée

-- La densité correcte est affichée par cargo build si elle diverge.
-- Lire le message cargo:error pour obtenir la valeur calculée, puis mettre à jour.
```

**Pré-déclaration sans table :** `to_regclass(component_id) IS NOT NULL` filtre
silencieusement les composants dont la table n'existe pas encore. Aucun `cargo build`
ne cassera pour un composant pré-déclaré.

### 2.2 `meta.component_varlena_join`

Déclare la table portant les champs texte (varlena) d'un composant.

| Colonne         | Type      | Description                      |
| --------------- | --------- | -------------------------------- |
| `component_id`  | `text` FK | Référence `containment_intent`   |
| `join_slot_idx` | `int2`    | `0` en Phase 1 (slot unique)     |
| `ref_schema`    | `text`    | Schéma de la table varlena       |
| `ref_table`     | `text`    | Table varlena                    |
| `fk_column`     | `text`    | Colonne FK entre les deux tables |

```sql
INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES
    ('content.article', 0, 'content', 'identity', 'document_id');
```

---

## 3. Conventions `pg_description`

Les annotations dans les commentaires de colonnes pilotent le comportement de la Forge.
Format général : `COMMENT ON COLUMN schema.table.col IS '<annotation>';`

Les annotations sont cumulables en les séparant par `;` :

```sql
COMMENT ON COLUMN content.core.author_entity_id
    IS 'Auteur principal ; marius:sentinel=0';
```

### 3.1 `marius:sentinel=<valeur>`

Surcharge le sentinel par défaut pour une colonne nullable fixed-length.

Le sentinel est la valeur stockée dans le `StorageRow` quand la colonne est `NULL`
en base. Il permet de distinguer "absent" de 0 sans allouer un `Option<T>` dans
le layout `#[repr(C)]`.

**Sentinels par défaut (Phase 0) :**

| Type SQL            | Sentinel par défaut | Raison                                       |
| ------------------- | ------------------- | -------------------------------------------- |
| `int8` / `bigint`   | `-1`                | IDs commencent à 1, -1 inatteignable         |
| `int4` / `integer`  | `0`                 | IDs commencent à 1                           |
| `int2` / `smallint` | `0`                 |                                              |
| `bool` / `boolean`  | `false`             |                                              |
| `float4` / `float8` | `0.0`               |                                              |
| `timestamptz`       | `0`                 | epoch Unix = 1970, inatteignable en pratique |
| `date`              | `0`                 | jours depuis 0001-01-01                      |

**Surcharge domain-specific :**

```sql
-- author_entity_id peut valoir 0 (entité système) → sentinel à -1
COMMENT ON COLUMN content.core.author_entity_id
    IS 'marius:sentinel=-1';

-- status : 0 est un état valide → sentinel à -1
COMMENT ON COLUMN content.core.status
    IS 'marius:sentinel=-1';
```

Après modification, `cargo build` régénère `generated_schema.rs`.
Vérifier dans `target/debug/build/marius-schema-*/out/generated_schema.rs` :

```bash
grep "author_entity_id" target/debug/build/marius-schema-*/out/generated_schema.rs
# → author_entity_id: r.author_entity_id.unwrap_or(-1),
```

### 3.2 `marius:pre_escaped`

Indique que la colonne est déjà échappée HTML en base.
Fragment-Forge utilise un facteur d'échappement de 1 au lieu de 6,
réduisant `DYNAMIC_CAP` et l'empreinte mémoire des buffers de rendu.

```sql
COMMENT ON COLUMN content.identity.description
    IS 'marius:pre_escaped';
```

---

## 4. Calcul de `intent_density_bytes`

**Deux concepts distincts à ne pas confondre :**

| Grandeur               | Valeur              | Contenu                                                                          |
| ---------------------- | ------------------- | -------------------------------------------------------------------------------- |
| `intent_density_bytes` | header PG + payload | Empreinte d'un tuple dans le heap PostgreSQL. Validée par `validate_layout()`.   |
| `stride` (store.bin)   | payload seul        | `sizeof(StorageRow)` — la struct `#[repr(C)]` ne contient jamais le header MVCC. |

`intent_density_bytes` sert à vérifier que le DDL est maîtrisé et cohérent avec
la Forge. Le `stride` du store.bin est calculé indépendamment par `mem::size_of::<P::Record>()`.

`intent_density_bytes` doit correspondre exactement au layout calculé
par `validate_layout()` dans `forge/db-forge/src/validate.rs`.

**Formule :**

```
n_total       = nombre total de colonnes (fixed + varlena, hors colonnes système)
header_bytes  = MAXALIGN(8)(23 + ⌈n_total / 8⌉)
payload_bytes = Σ size_bytes(col) pour les colonnes fixed-length
padded_payload= ⌈payload_bytes / max_align⌉ × max_align
intent_density= header_bytes + padded_payload
```

La valeur exacte est affichée par `cargo build` quand elle diverge :

```
cargo:error=DB-Forge [content.core] : layout diverge du registre.
Calculé=72B (header=32B + payload=40B), Enregistré=56B.
```

Lire `Calculé=72B` et mettre à jour :

```sql
UPDATE meta.containment_intent
SET    intent_density_bytes = 72
WHERE  component_id = 'content.core';
```

---

## 5. Ajouter un composant — procédure complète

```sql
-- 1. Créer la table DDL (si pas encore existante)
CREATE TABLE content.article (
    document_id  int4 GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    status       int2 NOT NULL DEFAULT 1,
    author_id    int4,          -- nullable → sentinel 0
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- 2. Déclarer dans le registre (density=0 au départ)
INSERT INTO meta.containment_intent (component_id, intent_density_bytes)
VALUES ('content.article', 0);

-- 3. Déclarer le JOIN varlena si nécessaire
INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES ('content.article', 0, 'content', 'article_identity', 'document_id');

-- 4. Annoter les sentinels domain-specific si besoin
COMMENT ON COLUMN content.article.author_id IS 'marius:sentinel=0';
```

```bash
# 5. Laisser cargo build calculer la densité
cargo build 2>&1 | grep "Calculé="
# → Calculé=40B (header=32B + payload=8B)
```

```sql
-- 6. Mettre à jour intent_density
UPDATE meta.containment_intent
SET    intent_density_bytes = 40
WHERE  component_id = 'content.article';
```

```bash
# 7. Build propre
cargo build --workspace

# 8. Populer le store.bin
cargo run --bin marius-dump

# 9. Valider le store.bin
cargo run --bin marius-verify

# 10. Démarrer (ou redémarrer) le serveur
cargo run --bin marius
# [OBSOLÈTE, voir note de statut en tête de document] Ceci ne charge PAS le
# pack HTML au premier fetch_batch(). Le serveur ouvre au démarrage
# (cold_start) les packs déjà présents sur disque, régénérés uniquement sur
# NOTIFY Postgres — jamais à la première requête HTTP. Si le pack n'existe
# pas encore pour ce composant, voir guide-cycle-de-vie-runtime.md §5 :
# marius-dump doit aussi appeler regenerate_and_swap, pas seulement écrire
# le store.bin.
```

---

## 6. Format binaire du store.bin

Produit par `marius-dump`, consommé par `PackfileReader` via `mmap`.

```
Offset   Section            Description
──────   ────────────────   ──────────────────────────────────────────────
0        Header (64B)       Magic "MARIUSDB", version, stride, row_count,
                            varlena_field_count, offsets des sections
64       StorageRow[]       row_count × stride bytes, #[repr(C)] contigu
align8   ID Index           row_count × 8B (i64), trié ASC
align8   Varlena TOC        row_count × varlena_field_count × 8B (VarlenSlot)
align8   Varlena Heap       bytes UTF-8 concaténés des champs varlena
```

L'**ID Index est une section séparée** (et non entrelacé dans les StorageRow)
pour permettre une recherche binaire O(log N) sur les IDs sans charger les données.
`PackfileReader::lookup(id)` opère exclusivement sur l'ID Index, puis accède
au StorageRow correspondant par index direct — aucun scan séquentiel.

`VarlenSlot { offset: u32::MAX, len: 0 }` = sentinel null (champ absent ou vide).

Le `PackfileStoreHeader` et `align8` sont définis dans `marius_projection`
(source de vérité unique partagée par `PackfileBuilder` et `PackfileReader`).

---

## 7. Commandes opérationnelles

```bash
# Régénérer generated_schema.rs (requis après toute modification DDL ou registre)
cargo build --workspace

# Créer ou mettre à jour le store.bin (requis avant démarrage serveur)
cargo run --bin marius-dump

# Valider le store.bin (audit layout, contenu varlena)
cargo run --bin marius-verify

# Tests unitaires Forge (sans DATABASE_URL)
cargo test -p marius-db-forge

# Tests d'intégration Forge (requis : DATABASE_URL)
cargo test -p marius-db-forge -- --ignored

# Tests no-realloc et ratio de remplissage (sans DATABASE_URL)
cargo test -p marius-schema
```

---

_Créé le 17 Juin 2026 (Phase 4 db-forge). Vérifié et daté le 7 juillet 2026 — voir note de statut en tête de document._
