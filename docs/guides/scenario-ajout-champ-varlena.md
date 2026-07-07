# Scénario opérationnel — Ajout d'un champ varlena (`author_biography`)

> Corrige un scénario produit par un tiers (Gemini), audité et invalidé sur
> trois points le 7 juillet 2026. Complémentaire de `guide-fragment-forge.md`
> et `guide-cycle-de-vie-runtime.md` — ne les duplique pas, y renvoie.

**Objectif** : rendre `author_biography` (TEXT) disponible dans le rendu HTML
de `content.document`.

**Préalable à trancher avant toute action** : la colonne existe-t-elle déjà
physiquement dans une table de composant déjà jointe (`meta.component_varlena_join`) ?
Deux chemins radicalement différents en découlent.

---

## Cas A — Le champ n'existe nulle part physiquement

### 1. Table physique de composant + registre

`author_biography` est une donnée d'accès rare (ADR-005 : fragmentation par
fréquence d'accès) — elle rejoint un composant `Content`/`Biography`, jamais
`content.core` (hot path). Si le composant n'a pas encore de jointure varlena :

```sql
-- Table physique du composant — jamais une vue sémantique
ALTER TABLE identity.person_biography ADD COLUMN author_biography TEXT;
ALTER TABLE identity.person_biography ADD CONSTRAINT
  person_biography_author_biography_length_check
  CHECK (length(author_biography) <= 2000);

-- Enregistrement de la jointure — schéma réel, pas de colonne "field_name" :
-- ce registre déclare une TABLE entière, pas un champ. Tous les varlena de
-- ref_table sont introspectés automatiquement (fetch_varlena_cols).
INSERT INTO meta.component_varlena_join
    (component_id, join_slot_idx, ref_schema, ref_table, fk_column)
VALUES ('content.document', 0, 'identity', 'person_biography', 'entity_id')
ON CONFLICT (component_id, join_slot_idx) DO NOTHING;
```

**Point de vigilance** : `ref_table` doit être une **table physique**. Une vue
sémantique (`content.v_article`, ADR-012) ne porte jamais de contrainte `CHECK`
— la détection de borne y échouerait systématiquement. Les vues sémantiques et
ce pipeline sont deux interfaces de lecture parallèles, sans arête commune.

### 2. Compilation du data layout

```bash
cargo build --workspace
```

**Invariant à ne pas confondre** : `sizeof(StorageRow)` (le `stride` fixed-length,
`#[repr(C)]`) **ne change jamais** en ajoutant un varlena. `Projection::Record`
et `Projection::VarlenOwned` sont deux types distincts (`batch_renderer.rs`,
`dispatcher.rs`) — le `VarlenSlot` de 8 octets vit dans une section séparée du
fichier (`Varlena TOC`), jamais inline dans la structure fixe. C'est l'invariant
qui protège la localité de cache du scan `ID Index`/`StorageRow[]` : il ne se
dégrade jamais, quel que soit le nombre de champs texte ajoutés.

### 3. Vue et directive `.marius`

Si `author_biography` doit aussi être exposé à d'autres consommateurs SQL,
mettre à jour la vue sémantique (ADR-012) — étape **indépendante**, sans effet
sur ce qui suit :

```sql
CREATE OR REPLACE VIEW content.v_article AS
SELECT ..., pb.author_biography
FROM content.document d
JOIN identity.person_biography pb ON pb.entity_id = d.author_entity_id;
```

Directive `.marius` — seule étape réellement requise côté Marius :

```jinja
{% if record.author_biography %}
<aside class="bio">{{ record.author_biography }}</aside>
{% endif %}
```

`cargo build` : `fragment-forge` détecte l'invocation, applique le facteur ×6,
recalcule `PAGE_TOTAL_CAP`. Voir `guide-fragment-forge.md` §2.4 pour le détail
du mécanisme Hot/Cold/Erreur.

### 4. Extraction et invalidation

```bash
cargo run --bin marius-dump   # store.bin, pour verify/audit uniquement
```

```sql
UPDATE identity.person_biography SET author_biography = 'Nouvelle bio'
WHERE entity_id = 123;
```

**Ce qui se passe réellement** — voir `guide-cycle-de-vie-runtime.md` §3 pour
le détail complet :

1. Trigger PostgreSQL → `NOTIFY`.
2. `PgListener` → `Collector::insert` → `Dispatcher::run()`.
3. `regenerate_and_swap` appelle `P::fetch_batch(pool, ids)` — **une requête
   PostgreSQL live**, jamais une lecture de `{table}_store.bin`. Les deux
   artefacts `.bin` n'ont aucune dépendance de lecture entre eux.
4. `render()` (nouveau binaire, capacité déjà recalculée) écrit dans le buffer
   pré-alloué, zéro réallocation.
5. Swap atomique de l'artefact pack HTML.
6. Prochaine requête HTTP : `pread`/`sendfile` sur le nouvel artefact.

---

## Cas B — Le champ existe déjà, déjà joint, déjà borné

Aucune étape SQL. Aucun `marius-dump`. Aucune vue à toucher. Seule l'étape 3
du Cas A (directive `.marius` + `cargo build`) s'applique — le champ était
déjà « Cold » (introspecté, non référencé, coût nul), il devient « Hot » au
premier `{{ record.author_biography }}` écrit. C'est le seul scénario où
« ajouter un champ » se réduit réellement à une opération de vue/template,
et il suppose une colonne déjà provisionnée avec un nom et une borne réels —
jamais un slot générique non nommé (coût de stride non nul si sur-provisionné,
voir discussion de session sur le pattern « Property Bag », écarté).

---

_Créé le 7 juillet 2026, à la suite de l'audit d'un scénario tiers (Gemini)
contenant trois erreurs de fond : schéma de registre inventé, stride supposé
croissant, source de données de régénération supposée être `store.bin`._
