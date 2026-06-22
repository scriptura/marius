# ADR-009 : Pages non adressables par clé primaire unique

Statut : **Accepté**.
Contexte projet : Marius, pipeline AOT (`marius-db-forge` / `marius-fragment-forge` / `marius-render`).
Documents liés : ADR-002 (Projection Réactive & État Hybride), ADR-007 (frontière
Hot/Cold varlena), ADR-008 (Topologie de l'artéfact de lecture).

Portée : ce document tranche une seule question, délibérément étroite. Il ne
rouvre aucune des décisions d'ADR-008.

---

## 1. Contexte et question déclenchante

Un audit croisé d'ADR-008 (Gemini/GPT) a identifié un cas non couvert : toute
page dont le contenu n'est pas une projection d'**une** ligne PostgreSQL
identifiable par une clé primaire unique.

Exemple concret : une page d'accueil affichant les 10 derniers produits
publiés. Elle ne correspond à aucun `id: i64` — c'est une vue agrégée sur un
ensemble de lignes, potentiellement changeant à chaque insertion.

`PackfileEntry { id: i64, offset: u64, len: u32 }` (ADR-008 §4.4) suppose une
correspondance 1:1 avec une ligne PostgreSQL adressable par PK. Ce modèle ne
s'étend pas naturellement à une vue agrégée sans en dénaturer la simplicité.

---

## 2. Hypothèses examinées et écartées

### 2.1 Étendre `PackfileEntry`/le routage pour supporter des requêtes composites

Permettre à une entrée d'index de référencer un ensemble de lignes (une
liste d'ids, un critère de tri, une limite) plutôt qu'un id unique.

**Écartée.** `PackfileEntry` est `#[repr(C)]`, `bytemuck::Pod`, taille fixe
24B — l'invariant qui permet le cast zero-copy depuis le mmap. Une requête
composite (liste d'ids de longueur variable, critère de tri arbitraire) n'a
aucune représentation de taille fixe naturelle. La contourner imposerait soit
une taille maximale arbitraire (réintroduisant exactement le type de fallback
que ADR-007 a éliminé pour les bornes varlena), soit une indirection vers une
structure de taille variable (brisant l'invariant zero-copy qui justifie tout
le format `packfile`). Le coût d'ingénierie et la dette conceptuelle dépassent
largement le bénéfice : un sous-système de requêtes serait reconstruit dans
Rust alors que PostgreSQL en est déjà un, mieux outillé pour ça.

### 2.2 Résoudre la vue au moment de la requête (lookup dynamique + tri/limite en Rust)

Charger les N derniers ids depuis le `store.bin` de `product_core`, trier,
tronquer, composer — au moment de la requête HTTP.

**Écartée sans ambiguïté.** C'est exactement le modèle que ADR-008 a corrigé
en sens inverse (voir son post-mortem, PM-001) : aucune résolution applicative
sur le chemin de lecture. Réintroduire ça ici, pour ce cas particulier,
referait la même erreur que celle qu'ADR-008 vient de documenter comme un
post-mortem à ne pas répéter.

---

## 3. Décision retenue

**Toute page non adressable par PK unique doit être adossée à une table de
synthèse PostgreSQL dédiée, qui redevient une `Projection` ordinaire — même
pipeline, aucune exception.**

```sql
-- Table de synthèse, maintenue par trigger ou job planifié.
-- Une seule ligne (singleton), id fixe.
CREATE TABLE meta.homepage_latest_products (
    id            int4 PRIMARY KEY DEFAULT 1,
    product_ids   int4[] NOT NULL,   -- ou colonnes dénormalisées si besoin de varlena
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (id = 1)                   -- singleton garanti
);
```

La table de synthèse est peuplée par un trigger sur `commerce.product_core`
(ou un job planifié, selon la fraîcheur requise) — un mécanisme PostgreSQL
ordinaire, hors du périmètre de la Forge. Une fois cette table en place,
**elle suit exactement le pipeline déjà construit** :

- Un template `.marius` feuille pour `meta.homepage_latest_products`,
  identique en nature à n'importe quel autre template feuille (ADR-008 §4.2).
- Une `Projection` générée, un `store.bin`/`packfile` dédié, adressée par
  `id = 1` — la contrainte PK unique est respectée, simplement triviale
  (un singleton est un cas particulier de PK unique, pas une exception).
- Invalidation via `pg_notify` sur la table de synthèse — pas sur
  `commerce.product_core` directement. Le trigger qui maintient la synthèse
  décide *quand* la resynchronisation a lieu ; le Dispatcher ne voit qu'une
  mutation ordinaire sur une table ordinaire.

**Conséquence directe** : le problème de granularité d'invalidation ("quand
regénérer l'accueil si n'importe lequel des 10 derniers produits change")
se résout **dans PostgreSQL**, par la logique du trigger — pas dans Rust, pas
dans le Dispatcher, pas dans une nouvelle structure de données applicative.

---

## 4. Ce que cette décision interdit explicitement

- Aucune page du système ne peut être adressée par autre chose qu'une PK
  (éventuellement triviale, singleton). Une page qui semblerait avoir besoin
  d'un autre mode d'adressage (paramètres de requête arbitraires, pagination
  libre) doit être reformulée comme une ou plusieurs tables de synthèse
  paramétrées par avance (ex : une ligne par page de pagination, si le nombre
  de pages est borné et connu), pas comme une extension du modèle d'adressage.
- Si un besoin de pagination réellement dynamique (offset arbitraire, non
  borné) apparaît, **il ne doit pas être résolu en assouplissant cette
  décision** — c'est un signal qu'une page de ce type sort du modèle AOT et
  doit être servie par un mécanisme distinct (rendu à la demande,
  explicitement hors du pipeline `.marius`/`Projection`), à spécifier
  séparément si le besoin se confirme. Ne pas anticiper cette spécification
  ici.

---

## 5. Principe directeur reconduit

Même réflexe qu'ADR-007 (réutiliser `CHECK` PostgreSQL plutôt que construire
une validation applicative parallèle) et qu'ADR-008 (composition à l'écriture
plutôt qu'un moteur de résolution à la lecture) : **quand un problème a une
solution naturelle dans PostgreSQL, l'y résoudre plutôt que de l'importer
dans Rust.** Une vue agrégée est un problème de requête SQL — PostgreSQL est
l'outil conçu pour ça. Construire l'équivalent dans le format `packfile`
aurait dupliqué, moins bien, une capacité que la base de données possède déjà.

---

*Rédigé à la suite d'un audit croisé (Gemini, GPT) d'ADR-008, isolant cette
question comme seul point parmi quatre justifiant un document séparé plutôt
qu'un amendement — les trois autres ont été intégrés directement à ADR-008.*
