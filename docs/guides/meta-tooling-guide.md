# Meta Tooling Guide — Marius

> **Créé le 17 juin 2026. Vérifié et daté le 7 juillet 2026. Note de statut
> corrigée le 22 juillet 2026.**

Guide opérationnel du pipeline AOT. Destiné au développeur qui ajoute, modifie
ou débogue un composant dans Marius. Ne documente pas les internals de la Forge
(voir les commentaires inline dans `crates/forge/db-forge/src/`).

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
    ▼ NOTIFY Postgres (UPDATE/INSERT/DELETE sur la table source)
    ▼ ÉTAGE 1 — ingest_and_swap() : fetch SQL live, fusion (merge_store),
    ▼ réécrit {schema}_{table}_store.bin, bascule StoreRegistry (atomique).
    ▼ Corrige un défaut réel du code antérieur au 22 juillet 2026 : sans cet
    ▼ étage, store.bin restait figé au dernier marius-dump, et l'étage
    ▼ suivant régénérait un pack HTML à partir d'une donnée périmée.
    ▼
    ▼ ÉTAGE 2 — regenerate_and_swap() : lit store.bin (désormais frais) via
    ▼ StoreRegistry/fetch_batch, rend, bascule LiveRegistry (atomique).
    ▼ Détail complet des deux étages : DFS-phase1-reactivite-cow.md,
    ▼ guide-cycle-de-vie-runtime.md.
    │
    ▼ requête HTTP → pread sur {table}.bin via LiveRegistry — zéro SQL,
    ▼ zéro allocation, jamais store.bin, jamais fetch_batch au moment de la
    ▼ requête. Ceci reste exact et n'a jamais été remis en cause.
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

| Colonne         | Type      | Description                                                                                                                                          |
| --------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `component_id`  | `text` FK | Référence `containment_intent`                                                                                                                       |
| `join_slot_idx` | `int2`    | Ordre déterministe du JOIN — **plusieurs slots par composant sont supportés** (corrigé le 23/07/2026, `CONTRAT-implementation-multi-slot-varlena.md` : `registry.rs` chargeait auparavant uniquement `join_slot_idx = 0`, limite Phase 1 jamais comblée, retirée depuis). `content.core` porte réellement 2 slots aujourd'hui : `0` → `content.identity`, `1` → `content.body` |
| `ref_schema`    | `text`    | Schéma de la table varlena                                                                                                                           |
| `ref_table`     | `text`    | Table varlena — **table physique uniquement**, jamais une vue sémantique (ADR-012) : la détection de borne (`CHECK`, §3) n'existe que sur les tables |
| `fk_column`     | `text`    | Colonne FK entre les deux tables                                                                                                                     |

```sql
INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES
    ('content.article', 0, 'content', 'identity', 'document_id');
```

**Attention au choix de `component_id`** (piège réel rencontré en session, corrigé après confrontation au schéma réel — pas une supposition) : ce doit être le composant qui **rend** effectivement le champ (celui qui porte `render()`/`render_segments()` dans `generated_schema.rs`), pas nécessairement la table « logique » qui semblerait porter la donnée. Sur le schéma réel du projet, `content.document` est la spine identifiant pure (`id`, `doc_type`) — sans `VarlenOwned` ni rendu propre ; c'est `content.core` qui porte la jointure varlena et le rendu HTML. Vérifier contre le seed réel (`10_meta_seed/01_manifest.sql`) avant d'inventer un `component_id`, plutôt que de supposer par analogie de nom.

**Plusieurs slots sur un même composant** — exemple réel, `content.core` (2 slots) :

```sql
INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES
    ('content.core', 0, 'content', 'identity', 'document_id'),
    ('content.core', 1, 'content', 'body',     'document_id');
```

`join_slot_idx` dicte l'ordre déterministe des champs varlena en mémoire (`{Name}VarlenOwned`, INV-8) — les champs du slot 0 précèdent toujours ceux du slot 1, quel que soit l'ordre d'insertion SQL (le tri `ORDER BY component_id, join_slot_idx` de `registry.rs` le garantit).

**Collision de nom entre deux slots, ou entre un slot et une colonne propre du composant** : échec de build explicite (`panic!` nommant le composant, la colonne, et les deux tables sources), jamais une désambiguïsation automatique — politique DDL-driven arbitrée le 22/07/2026, `CONTRAT-implementation-multi-slot-varlena.md` Étape 3. Renommer la colonne en conflit côté SQL est la seule issue.

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

### 3.2 Politique d'échappement varlena — trois états, pas un booléen (corrigé 23/07/2026)

Ce document affirmait auparavant que `marius:pre_escaped` était **le seul** tag
`pg_description` reconnu pour l'échappement. **Faux depuis
`CONTRAT-implementation-varlena-raw.md`/`CONTRAT-implementation-projection-
segmentee.md`** : trois tags coexistent, correspondant à trois valeurs de
l'enum fermé `EscapePolicy` (`marius_projection`/`fragment-forge`) :

| Tag `pg_description`       | `EscapePolicy` | Facteur capacité | Échappé au runtime ? | Concaténé dans `buf` ? |
| --------------------------- | -------------- | ---------------- | --------------------- | ----------------------- |
| *(aucun)*                   | `Escaped`      | × 6               | Oui                    | Oui                      |
| `marius:pre_escaped`        | `PreEscaped`   | × 1               | Oui (défense en profondeur) | Oui                |
| `marius:raw`                | `Raw`          | × 1               | **Jamais**             | Oui                      |
| `marius:large_content`      | `Raw` + segmenté | **0** (aucune contribution) | **Jamais** | **Non** — `Segment::Borrowed` autonome |

**`marius:pre_escaped`** — inchangé depuis la version précédente de ce guide :
```sql
COMMENT ON COLUMN content.identity.description
    IS 'marius:pre_escaped';
```
Certifie un contenu sans caractère spécial (slug, titre normalisé) — échappé
quand même au runtime par défense en profondeur, facteur de capacité réduit à 1.

**`marius:raw`** — HTML déjà constitué, à injecter tel quel, **jamais**
échappé au runtime (`buf.push_str` direct, aucun appel à
`marius_html_escape`) :
```sql
COMMENT ON COLUMN content.body.content
    IS 'marius:raw';
```
Distinct de `pre_escaped` : le contenu contient au contraire potentiellement
beaucoup de caractères spéciaux intentionnels (balises) — ce n'est pas leur
absence qui justifie l'exemption d'échappement, c'est leur nature de balisage
déjà voulu tel quel. ⚠️ Réservé à un contenu HTML dont la production est
maîtrisée côté application (jamais une saisie utilisateur brute non
sanitizée en amont).

**`marius:large_content`** — variante de `marius:raw` pour un contenu
**volumineux**, qui ne doit jamais dimensionner le buffer partagé de rendu
(`{NAME}_TOTAL_CAP`) :
```sql
COMMENT ON COLUMN content.body.content
    IS 'marius:large_content';
```
Implique toujours `EscapePolicy::Raw` (un seul tag, pas deux à cumuler) et
marque en plus le champ `is_segment == true` : `fragment-forge` génère alors
`render_segments()` au lieu de `render()` pour ce composant — le champ devient
un `Segment::Borrowed` autonome (référence zéro-copie sur la donnée déjà
possédée), jamais concaténé dans `buf`, jamais compté dans `DYNAMIC_CAP`, et
**exempté du seuil AOT absolu de 64 Ko** (`introspect.rs`) qui s'applique à
tout autre champ varlena. `render()` devient alors un stub `unreachable!()` —
`BatchRenderer`/`render_batch_pure` appellent systématiquement
`render_segments()`, jamais `render()` directement, pour tout composant
portant un tel champ. Voir `CONTRAT-implementation-projection-segmentee.md`
pour le détail complet du mécanisme (`Segment<'a>`, `MAX_SEGMENTS`).

**Un seul tag à la fois par colonne** — `marius:raw` et `marius:large_content`
ne se cumulent jamais : le second implique déjà le premier.

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
par `validate_layout()` dans `crates/forge/db-forge/src/validate.rs`.

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

Exemple minimal ci-dessus : un seul slot, échappement par défaut. Pour un
second JOIN varlena sur le même composant (`join_slot_idx` suivant), ou un
champ HTML pré-constitué/volumineux (`marius:raw`/`marius:large_content`),
voir §2.2/§3.2-3.4 ci-dessus.

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
# Le serveur ouvre au démarrage (cold_start) les packs déjà présents sur
# disque, régénérés uniquement sur NOTIFY Postgres — jamais à la première
# requête HTTP. Si le pack n'existe pas encore pour ce composant :
# cargo run --bin marius-dump appelle déjà regenerate_and_swap après avoir
# écrit le store.bin (pas seulement écrire le store.bin) — mais doit
# d'abord provisionner et monter le StoreRegistry de ce composant
# (ensure_store_provisioned + cold_start_store, Phase 1 réactivité CoW,
# 22 juillet 2026), sans quoi regenerate_and_swap panique en tentant de
# relire un store.bin via un registre jamais monté. Ce câblage est déjà en
# place dans le binaire marius-dump réel du projet — rien à faire de
# spécial ici, mentionné pour comprendre l'ordre des opérations si le
# binaire échoue. Détail : PHASE1-CLOSURE.md.
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

_Créé le 17 Juin 2026 (Phase 4 db-forge)._
_Dernière mise à jour le 23-25 juillet 2026_
