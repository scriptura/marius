# Contrat d'Implémentation — Support Multi-Slot Varlena

**Fonde sur** : constats de session (22/07/2026) — `registry.rs` filtre explicitement
`join_slot_idx = 0` (« Phase 1 : slot unique »), `ComponentConfig.varlena_join` est
`Option<VarlenJoin>` (cardinalité 0/1), `codegen/projection.rs::write_projection_stub`
prend `varlena_join: Option<(&str, &str, &str)>` et qualifie **tous** les champs
varlena avec un unique `vt` capturé hors boucle (`format!("{vt}.{}", v.name)`, ligne
92-99). Aucun de ces trois points n'est un bug local — c'est une limite de portée
Phase 1, documentée comme telle dans le code, jamais comblée. `meta.component_varlena_join`
(schéma SQL) autorise déjà plusieurs `join_slot_idx` par composant ; le code ne l'a
jamais exercé avant l'ajout du slot 1 sur `content.core` (session du 22/07/2026).

**Discipline d'exécution** : identique à `CONTRAT-implementation-phase1.md` — chaque
étape est atomique, testée isolément. Plusieurs étapes listent des fichiers non
encore vus cette session (`build.rs`, `codegen/row.rs`, `mapping.rs`) : ne pas
extrapoler leur contenu — les demander au moment de l'étape concernée.

**Arbitrage reçu (22/07/2026)** : politique de collision de nom tranchée — échec de
build explicite (a), portée étendue à la collision varlena/fixed (cf. Étape 3).
Plus aucun point de design ouvert dans ce document ; les seuls blocages restants
sont des fichiers non encore vus, listés à chaque étape concernée.

---

### Étape 1 — `registry.rs` : `Option<VarlenJoin>` → `Vec<VarlenJoin>`

**Crate** : `crates/forge/db-forge`, `registry.rs`.
**Contenu** :
- Retirer `AND cvj.join_slot_idx = 0` de la requête `fetch_component_list`.
- `ComponentConfig.varlena_join : Option<VarlenJoin>` → `Vec<VarlenJoin>` (vide = aucun
  join, cohérent avec le `None` actuel).
- Regroupement des lignes par `component_id` côté Rust (le `LEFT JOIN` SQL produit
  désormais 0..N lignes par composant) ; tri par `join_slot_idx ASC` — conserver le
  déterminisme O(1) INV-8, actuellement garanti côté SQL par le tri `component_id ASC`
  seul (le tri interne des slots doit maintenant être explicite, en Rust ou en SQL
  secondaire `ORDER BY component_id, join_slot_idx`).
- `VarlenJoin` gagne implicitement une notion d'ordre (position dans le `Vec`) —
  pas de champ `join_slot_idx` à ajouter à la struct sauf si un consommateur en a
  besoin explicitement (à statuer à l'Étape 4 selon ce que `build.rs` exige).
**Dépend de** : rien.
**Critère de complétion** : test unitaire — composant avec 2 lignes
(`join_slot_idx` 0 et 1) → `Vec` de longueur 2, ordre respecté ; composant sans
ligne → `Vec` vide ; incohérence partielle NULL sur une ligne → erreur `Decode`
inchangée (comportement déjà testé, à ne pas régresser).

### Étape 2 — Provenance explicite dans `VarlenField` (marius-fragment-forge)

**Crate** : `crates/core/schema` (ou là où `VarlenField` est réellement défini —
confirmé dans `lib.rs` fourni cette session, `crates/forge/fragment-forge`).
**Contenu** : ajouter à `VarlenField` la table source du champ (`ref_schema`,
`ref_table`, ou un identifiant de slot suffisant pour reconstruire `{vt}.{name}`
côté `codegen/projection.rs`). Sans cette provenance, impossible de qualifier
correctement chaque colonne dans un `SELECT` multi-JOIN — c'est le bug exact de
`projection.rs` ligne 92-99 (un seul `vt` capturé hors boucle, appliqué à tort à
tous les champs).
**Dépend de** : rien de structurel, mais **casse tout site de construction actuel
de `VarlenField`** — recensement nécessaire avant d'écrire quoi que ce soit :
`introspect.rs::fetch_varlena_cols` (vu cette session, à adapter — il connaît déjà
`schema`/`table` en paramètres, simple propagation) et tout site de test
(`lib.rs` lignes ~1956, 1966, 2816 — construction manuelle de `VarlenField` dans
les tests unitaires de `fragment-forge`, à mettre à jour avec le nouveau champ).
**Critère de complétion** : `cargo build`/`cargo test` de `fragment-forge` après
ajout du champ — aucune régression sur les tests existants (mono-slot, provenance
renseignée trivialement).

### Étape 3 — Détection de collision de nom, échec de build explicite (CLOSE CÔTÉ CODE)

**Politique retenue (arbitrage du 22/07/2026)** : **(a) échec de build explicite**.
Système strictement DDL-driven — aucune désambiguïsation automatique, aucun
renommage silencieux côté généré. Toute collision de nom est une erreur de
modélisation SQL à corriger dans le schéma, jamais un cas que le générateur
absorbe pour son propre compte.

**Portée réellement implémentée, élargie au-delà du constat initial** — confrontée
à `row.rs` avant écriture, comme prévu : la vérification porte sur **toutes** les
colonnes propres du composant (`own_columns : &[Column]`, fixed-length ET varlena
inline confondues), pas seulement le sous-ensemble `fixed_cols` filtré ailleurs
dans `codegen/projection.rs`. Raison : `row.rs` génère un champ Rust nommé
`col.name` pour toute colonne du composant, qu'elle soit fixed ou varlena directe
(non jointe, branches « varlena table principale » / « varlena NULLABLE table
principale ») — une collision sur ce type de colonne casserait exactement de la
même façon qu'une collision inter-slots.

**Implémenté** (`validate.rs`, fonction `check_no_name_collision`) :
1. **Collision inter-slots** : deux `VarlenField` de slots différents (même
   composant) partageant le même `.name` — comparaison par paires, `O(n²)`,
   acceptable vu la cardinalité réelle (quelques champs varlena par composant).
2. **Collision varlena/colonne propre** : un `VarlenField` dont le nom coïncide
   avec une colonne de `own_columns`.

`panic!` non utilisé ici — cohérence avec le style déjà en place pour
`validate_layout` dans le même fichier : `Result<(), String>`, à charge de
l'appelant (`build.rs`, Étape 4) d'émettre `cargo:error` + `std::process::exit(1)`,
exactement le pattern déjà utilisé lignes 1772-1781 de `build.rs` pour
`validate_layout`.

**Dépend de** : Étape 2 (provenance nécessaire pour un message d'erreur nommant
les deux tables sources, cas 1) — close.
**Non fait à ce stade, délibérément** : `check_no_name_collision` n'est pas encore
appelée depuis `build.rs` — le câblage réel (avec le `varlena: Vec<VarlenField>`
multi-slot correctement assemblé) est la responsabilité de l'Étape 4, pas de
celle-ci. Une fonction non appelée ne peut pas être considérée comme validée en
conditions réelles — seuls les tests unitaires synthétiques de `validate.rs`
couvrent cette étape pour l'instant.
**Critère de complétion** : quatre tests unitaires écrits (cas nominal sans
collision sur `content.core` réel ; collision inter-slots synthétique ; collision
varlena/fixed synthétique ; collision varlena/varlena-inline synthétique) — à
confirmer par `cargo test` réel côté dépôt. Aucun des cas de collision n'existe
dans le schéma réel actuel.

### Étape 4 — `codegen/projection.rs` : `SELECT`/`FROM` multi-JOIN

**Crate** : `crates/forge/db-forge`.
**Contenu** :
- `varlena_join: Option<(&str, &str, &str)>` → `&[(&str, &str, &str)]` (un triplet
  par slot, ordre = ordre du `Vec<VarlenJoin>` de l'Étape 1).
- Garde-fou PK composite (ligne 76-83) : `.is_some()` → `!.is_empty()`, sémantique
  inchangée (interdiction dès qu'au moins un JOIN varlena existe).
- `FROM` : une clause `LEFT JOIN` par triplet, enchaînées sur la même table pivot
  (`{schema}.{table} LEFT JOIN {vs1}.{vt1} ON ... LEFT JOIN {vs2}.{vt2} ON ...`).
- `SELECT` : chaque `VarlenField` qualifié par **sa propre** table source (Étape 2),
  plus alias `AS` si la politique (b) de l'Étape 3 est retenue — corrige directement
  le bug du `vt` unique capturé hors boucle.
**Dépend de** : Étape 1, Étape 2, Étape 3 (arbitrage tranché).
**Fichier à confronter avant d'écrire, non vu cette session** : l'appelant de
`write_projection_stub` — vraisemblablement `crates/core/schema/build.rs` — pour
voir comment il assemble aujourd'hui `varlena_join`/`varlena` à partir de
`ComponentConfig` (probablement un seul appel à `fetch_varlena_cols` par composant,
à réécrire en boucle sur le `Vec<VarlenJoin>` de l'Étape 1).
**Critère de complétion** : SQL généré pour `content.core` (2 slots réels,
`content.identity` + `content.body`) syntaxiquement valide et testé contre
PostgreSQL réel — retourne les colonnes attendues des deux tables.

### Étape 5 — `codegen/varlen.rs` et `codegen/row.rs` : noms de champs

**Crate** : `crates/forge/db-forge`.
**Contenu** : `write_varlen_owned_struct` (vu cette session) génère déjà
`pub {}: Option<String>` par nom brut — cohérent avec la politique de l'Étape 3
une fois celle-ci choisie (nom préfixé si (b), ou aucun changement si (a) puisque
la collision est alors interceptée avant d'atteindre cette fonction).
**Dépend de** : Étape 3, Étape 4.
**Fichier à confronter avant d'écrire, jamais vu cette session** :
`crates/forge/db-forge/src/codegen/row.rs` — `{name}Row` (`sqlx::FromRow`) est le
point exact où une collision de nom de colonne SQL pourrait, selon son
implémentation réelle (macro derive vs génération manuelle `row.try_get(name)`),
soit échouer à la compilation (probable), soit lier silencieusement la mauvaise
valeur au mauvais champ (à vérifier, pas à supposer).
**Critère de complétion** : `cargo build` réel sur `content.core` (2 slots) ; si un
second cas de test synthétique avec collision volontaire est nécessaire pour
exercer l'Étape 3, le construire à ce moment (le schéma réel n'a pas de collision
aujourd'hui — `identity` : `slug/headline/alternative_headline/description` vs
`body` : `content`).

### Étape 6 — Non-régression Phase 1 (mono-slot)

**Contenu** : tout composant à un seul slot varlena (ou aucun) ne doit produire
aucun changement de code généré après le passage à `Vec`/`&[..]` — le cas
`Vec::len() == 1` doit être bit-pour-bit identique au comportement `Option::Some`
actuel.
**Dépend de** : Étapes 1, 2, 4, 5.
**Critère de complétion** : diff nul sur le code généré pour tout composant
mono-slot déjà en production avant cette session (`content.core` slot 0 seul,
avant l'ajout du slot 1 — comparer contre une sauvegarde du code généré actuel
si disponible, sinon reconstruire ce cas de référence).

### Étape 7 — Validation bout-en-bout sur le cas réel (`content.core`, 2 slots)

**Contenu** : reprend le point 4 de la checklist `HANDOFF-v2` (directive `.marius`
du corps d'article) — `{{ record.content }}` dans `core.marius` doit résoudre sans
`UnknownField`, `cargo build`/`test`/`clippy` réels, avec le second slot
effectivement exercé par un template (contrairement à la validation du point 3 de
la checklist originale, qui n'exerçait aucun template sur le slot 1).
**Dépend de** : Étapes 1 à 6, toutes closes.
**Critère de complétion** : identique à celui déjà obtenu pour le slot 0 en
Phase 1 — build réel, pas relecture de code.

---

## Dépendances entre étapes — résumé

```
1 (registry Vec) ──┬──▶ 4 (SELECT/FROM multi-JOIN) ──▶ 6 (non-régression) ──▶ 7
2 (provenance) ─────┤              │
3 (arbitrage) ──────┘              ▼
                         5 (row.rs / varlen.rs) ──▶ 6
```

## Arbitrage clos

Politique (a), échec de build explicite, retenue le 22/07/2026 — portée étendue à
la collision varlena/fixed en plus de la collision inter-slots. Aucun point
d'arbitrage restant ouvert dans ce document ; l'Étape 3 ne dépend plus que de la
lecture de `validate.rs` (fichier, pas décision) pour être implémentée.
