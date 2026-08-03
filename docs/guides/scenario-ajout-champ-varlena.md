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

**Note ajoutée le 25/07/2026, à vérifier avant d'exécuter cet exemple tel
quel** : `component_id = 'content.document'` ci-dessus n'a jamais été
confronté au schéma réel du projet dans cette session (l'exemple de ce guide
est un cas pédagogique, `author_biography`, distinct du cas réellement traité
en session, `content.body.content`). Une session précédente a découvert, sur
un cas voisin, que le `component_id` correct n'est pas toujours la table qui
semble porter la donnée « logiquement » — c'est le composant qui **rend**
effectivement le champ (celui qui porte `render()`/`render_segments()` dans
`generated_schema.rs`) qui compte. Sur le schéma réel, `content.document` est
la spine identifiant pure (`id`, `doc_type`), sans jointure varlena ni rendu
HTML propre à ce jour — vérifier contre `meta.containment_intent`/le seed réel
(`10_meta_seed/01_manifest.sql`) avant d'exécuter cet exemple, plutôt que de
supposer que `content.document` est le bon choix par analogie de nom.

**Si le composant cible porte déjà une jointure varlena** (`join_slot_idx = 0`
déjà pris), incrémenter `join_slot_idx` plutôt que d'écraser — le multi-slot
est supporté nativement (`CONTRAT-implementation-multi-slot-varlena.md`),
plusieurs `ref_table` distinctes peuvent cohabiter sur un même composant.

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

**Si `author_biography` est du HTML déjà constitué plutôt que du texte à
échapper** (note ajoutée le 25/07/2026) : le facteur ×6 ci-dessus est le
mauvais mécanisme — il échapperait les balises au lieu de les rendre. Deux
tags `pg_description` couvrent ce cas, à poser sur la colonne AVANT `cargo
build` :

```sql
-- Contenu HTML de taille normale, reste dans le buffer partagé :
COMMENT ON COLUMN identity.person_biography.author_biography IS 'marius:raw';

-- Contenu HTML volumineux (au-delà de quelques dizaines de Ko), ne doit
-- jamais dimensionner le buffer partagé — devient un segment autonome :
COMMENT ON COLUMN identity.person_biography.author_biography IS 'marius:large_content';
```

Dans le second cas, `cargo build` génère `render_segments()` au lieu de
`render()` pour le composant concerné — voir `guide-fragment-forge.md` §4.8bis
et `CONTRAT-implementation-projection-segmentee.md` pour le mécanisme complet.
Sans objet pour une biographie de 2000 caractères borné par `CHECK`
ci-dessus (§1) — mentionné ici pour complétude, pas parce que ce cas
particulier en a besoin.

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

_Complété le 25 juillet 2026, en préparation d'une interruption prolongée de
disponibilité — non revérifié par exécution réelle après cette révision :
note de prudence sur le choix de `component_id` (Cas A, étape 1) et mention
des tags `marius:raw`/`marius:large_content` comme alternative au facteur ×6
par défaut (Cas A, étape 3) — deux mécanismes réels ajoutés en session,
absents de la version précédente de ce document. Voir
`CONTRAT-implementation-multi-slot-varlena.md`, `CONTRAT-implementation-
varlena-raw.md`, `CONTRAT-implementation-projection-segmentee.md`._
