# ADR-007 : Introspection des bornes varlena — frontière Hot/Cold

Statut : **Accepté** (frontière Hot/Cold) — implémentation non encore livrée.
Contexte projet : Marius, pipeline AOT (`marius-db-forge` / `marius-fragment-forge`).
Documents liés : ADR-002, ADR-003 (`crates/core/schema/src/lib.rs`).

---

## 1. Contexte

Le compilateur AOT doit connaître, à la compilation, une borne supérieure sur la
longueur de tout champ varlena (`TEXT`, `VARCHAR`) participant à une projection
HTML, afin de calculer `PAGE_DYNAMIC_CAP` et garantir l'invariant zéro-réallocation
(`buf.reserve(PAGE_TOTAL_CAP)` suffisant en toute circonstance).

`fetch_varlena_cols()` (`forge/db-forge/src/introspect.rs`) résout cette borne
selon trois cas :

1. `VARCHAR(N)` / `BPCHAR(N)` → lecture directe de `atttypmod`.
2. `TEXT` avec `CHECK (length(col) <= N)` → extraction par parsing du texte de
   la contrainte.
3. `TEXT` sans contrainte → exclusion du listing render, avec un _fallback_
   silencieux à 10 000 caractères dans un chemin de code voisin (capacité
   manuelle forcée).

Le symptôme déclencheur : `commerce.product_content.description` est un `TEXT`
non borné. Le fallback à 10 000 caractères (× facteur d'échappement 6 = 60 000B)
fait chuter le ratio statique/dynamique de la table sous le seuil critique de
validation (3 %), sans qu'aucune erreur de compilation explicite ne soit levée
plus tôt dans le pipeline.

L'investigation de ce symptôme a révélé des défauts indépendants dans le
mécanisme d'introspection lui-même, détaillés en §2.

---

## 2. Audit des hypothèses implicites (H1–H10)

`fetch_varlena_cols()` et `parse_check_length_limit()` reposent sur des
hypothèses non documentées au moment de l'audit. Chaque hypothèse est évaluée
selon trois critères : garantie par PostgreSQL, garantie par discipline de
schéma, ou pure supposition du code.

| #   | Hypothèse                                                                                   | Garantie PG                                                                                         | Garantie schéma                   | Statut                                                                    |
| --- | ------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | --------------------------------- | ------------------------------------------------------------------------- |
| H1  | Au plus une contrainte CHECK pertinente matche `length(col)` par colonne                    | Non                                                                                                 | Non                               | Pure supposition                                                          |
| H2  | L'opérande gauche est toujours `length(col)`, jamais `N >= length(col)`                     | Non                                                                                                 | Non                               | Pure supposition                                                          |
| H3  | La fonction utilisée est toujours `length()` ou `char_length()`                             | Non                                                                                                 | Non                               | Pure supposition                                                          |
| H4  | `pg_get_constraintdef()` a un format de sortie stable, parseable par recherche de substring | Partiellement — pretty-printer lisible, pas un format sérialisé contractuel inter-versions majeures | Non                               | Risque le plus élevé                                                      |
| H5  | `atttypmod - 4 = N` pour `VARCHAR(N)`/`BPCHAR(N)`                                           | **Oui** — encodage stable et documenté                                                              | N/A                               | Seule hypothèse solide                                                    |
| H6  | Cardinalité 0 ou 1 pour la requête CHECK (`fetch_optional`)                                 | Non — rien n'interdit plusieurs lignes ; sélection arbitraire sans ordre garanti                    | Non                               | Violation du déterminisme AOT                                             |
| H7  | Le CHECK ne référence que la colonne introspectée, sans sous-requête inter-tables           | **Oui** — un CHECK ne peut pas contenir de sous-requête référençant une autre table                 | N/A                               | Hypothèse "gratuite"                                                      |
| H8  | Le CHECK est `VALID` (vérifié sur les lignes existantes), pas `NOT VALID`                   | Non — `ADD CONSTRAINT ... NOT VALID` est un DDL légal                                               | Non                               | Trou non détecté avant cet audit ; `pg_constraint.convalidated` jamais lu |
| H9  | `N` est un littéral entier nu, jamais une expression (`2*1000`) ou un cast                  | Non                                                                                                 | Non                               | Pure supposition                                                          |
| H10 | Le nom de colonne dans le filtre `LIKE` correspond texte-à-texte au nom introspecté         | Non — un identifiant nécessitant des guillemets (majuscules, mot réservé) casse la correspondance   | Dépend des conventions de nommage | Risque réel, priorité basse si snake_case respecté                        |

**Défaut additionnel, indépendant des hypothèses H1–H10** :
`pg_constraint.consrc` a été supprimée en PostgreSQL 12. Son usage produit une
erreur SQL (`column "consrc" does not exist`), pas une dégradation silencieuse,
sur toute instance ≥ 12. Remplacement obligatoire par `pg_get_constraintdef(oid)`.

**Invariant de méta-niveau menacé** : Phase 0.15 exige un `generated_schema.rs`
bit-pour-bit identique entre deux builds sur le même schéma. H6 et H4 sont les
deux hypothèses de cette liste susceptibles de casser ce déterminisme — pas
seulement produire une valeur incorrecte, mais produire une valeur _différente
selon l'exécution_.

---

## 3. Alternatives étudiées

### 3.1 `VARCHAR(N)` (conversion de type)

Stockage physique strictement identique à `TEXT` côté PostgreSQL (même
représentation varlena, même seuil TOAST). Borne extraite sans aucun parsing,
via `atttypmod` (H5, seule garantie structurelle disponible dans le catalogue).
Coût : migration DDL, scan de validation sur les données existantes au moment
du `ALTER COLUMN TYPE`.

### 3.2 `TEXT` + `CHECK (length(col) <= N)` avec DSL strict

Conserve le type physique réel. Nécessite, pour être aussi sûr que 3.1 :
grammaire d'acceptation ancrée (rejet des formes composées/inversées/non
littérales), `fetch_all` + hard-fail sur cardinalité ≥ 2 (résout H6),
vérification de `pg_constraint.convalidated` (résout H8). Dépend de H4 (format
`pg_get_constraintdef`), risque non nul à long terme inter-versions majeures.

### 3.3 Annotation `pg_description` (`marius:max_len=N`)

**Écartée.** Argument décisif : seul un type de colonne ou un `CHECK` est
_vérifié_ par PostgreSQL à chaque écriture. Un commentaire ne l'est pas — rien
n'empêche une donnée réelle de dépasser la borne annotée, ce qui sous-dimensionne
silencieusement `buf.reserve()` et viole l'invariant zéro-allocation que le
pipeline existe pour garantir. Une borne non appliquée est une promesse non
tenue, structurellement inférieure à 3.1 ou 3.2.

### 3.4 Extraction structurelle (recherche d'alternative à 3.2)

Étudié : `pg_constraint.conbin` (arbre sérialisé `nodeToString()`) — rejeté,
format de débogage interne explicitement non documenté comme stable, pire
garantie que `pg_get_constraintdef()`. `DOMAIN` avec CHECK intégré — rejeté,
ne résout rien (un domaine sur `text` réintroduit le même problème textuel).
**Conclusion** : il n'existe aucune voie d'extraction de borne sans parsing
pour un `CHECK` sur `TEXT`. Le seul mécanisme structurel exempt de parsing dans
PostgreSQL est `atttypmod`, disponible uniquement pour `VARCHAR`/`BPCHAR`.

### 3.5 Frontière Hot/Cold (retenue — voir §4)

Reformule la question : un `TEXT` non borné ne doit pas être interdit
_globalement_ dans le schéma, seulement lorsqu'il participe effectivement à une
projection dont les capacités mémoire doivent être démontrées. Déplace
l'exigence de bornage du niveau "table" au niveau "champ effectivement
référencé par un template résolu".

---

## 4. Décision retenue

**Frontière Hot/Cold à la jonction `VarlenField` / `resolve_and_measure`.**

`fetch_varlena_cols()` continue d'introspecter toutes les colonnes varlena de
la table jointe, sans changement de portée. La borne devient une propriété
optionnelle (`max_len: Option<usize>`) plutôt qu'un nombre toujours présent
(fallback ou non). La classification émerge de la conjonction entre deux faits
déjà connus du compilateur au moment de `resolve_and_measure` :

| Référencé par l'AST | Borne connue | Classification                                              |
| ------------------- | ------------ | ----------------------------------------------------------- |
| Non                 | indifférent  | **Cold** — hors calcul, invisible au pipeline de capacité   |
| Oui                 | `Some(n)`    | **Hot** — contribue à `total_dynamic_bytes`                 |
| Oui                 | `None`       | **Erreur de compilation** — `ResolverError::UnboundedField` |

Aucune nouvelle phase de pipeline, aucune nouvelle E/S, aucun nouvel état
mutable. La classification est une fonction pure de l'instantané `pg_catalog`
et de l'AST résolu — tous deux déjà déterministes. Le test de non-régression
Phase 0.15 reste valide sans modification.

**Conséquence pour `commerce.product_content.description`** : reste `TEXT`
sans borne. Tant qu'aucun template `.marius` ne le référence, le champ est
Cold — aucun impact sur la capacité, aucun fallback arbitraire. S'il devient un
jour référencé par une projection, le build échoue explicitement
(`UnboundedField`) plutôt que de dégrader silencieusement le ratio mémoire —
c'est à ce moment, et seulement à ce moment, que le choix entre 3.1, 3.2 ou une
exclusion explicite doit être tranché, et il devient alors un choix _local à la
colonne référencée_ plutôt qu'une règle globale au schéma.

---

## 5. Invariants nouvellement vérifiables

- **INV-VARLENA-1** : tout champ contribuant à `total_dynamic_bytes` a une
  borne connue statiquement — jamais de fallback arbitraire.
- **INV-VARLENA-2** : un `TEXT` non borné jamais référencé par un template ne
  bloque aucun build, indéfiniment.
- **INV-VARLENA-3** : la classification Hot/Cold/Erreur est stable à schéma et
  template fixés (fonction pure, testable).

---

## 6. Statut d'implémentation

Non livré à la date de cet ADR. Reste à charge, indépendamment de l'ordre :

- `pg_constraint.consrc` → `pg_get_constraintdef()` (corrige le défaut bloquant
  sur PG ≥ 12, indépendant de toute autre décision de cet ADR).
- `VarlenField.max_len: usize` → `Option<usize>`.
- `ResolverError::UnboundedField` dans `marius-fragment-forge`.
- Branchement dans `resolve_and_measure` (Hot / Cold / Erreur selon le tableau
  §4).
- Si le DSL `CHECK` strict (3.2) est un jour retenu pour une colonne donnée :
  grammaire ancrée, `fetch_all` + hard-fail sur cardinalité, vérification
  `convalidated`, test d'intégration round-trip schéma réel → `VarlenField`.

---

_Rédigé à la suite d'un audit d'architecture sur l'introspection PostgreSQL du
pipeline AOT Marius. Conserve l'analyse H1–H10 pour référence future — ne pas
supprimer même si la décision §4 est révisée._
_20 juin 2026_
