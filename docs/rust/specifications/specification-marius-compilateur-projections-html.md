# Spécification : Compilateur de Projections HTML AOT — Moteur Marius

**Version :** 1.1 (stabilisation éditoriale post-revue finale)
**Périmètre :** `crates/forge/fragment-forge`, `crates/core/projection`, `crates/core/schema`
**Date :** 9 juin 2026

---

## 0. Cadrage architectural

_Ce chapitre est le point d'entrée obligatoire. Il établit le modèle conceptuel dans lequel toute la suite doit être lue. Un lecteur qui comprend ce chapitre comprend pourquoi chaque contrainte technique existe._

### 0.1 Ce qu'est Marius

Marius est un **compilateur de projections HTML**. Son pipeline complet est :

```
Build-time ─────────────────────────────────────────────────────────────

  Schéma PostgreSQL          Spécifications .marius
       │                              │
       └──────────────┬───────────────┘
                      ▼
            forge/fragment-forge
          (compilateur AOT, build.rs)
                      │
                      ▼
           generated_schema.rs
          (code source Rust natif)
                      │
                      ▼
                    rustc
                      │
                      ▼
          fn render_page(&Record, &VarlenOwned, &mut String)
          (instruction machine dans le binaire)

Runtime ─────────────────────────────────────────────────────────────────

  pg_notify ──▶ Collector ──▶ Dispatcher
                                  │
                                  ├── fetch_batch (SQLx) ──▶ Vec<(Record, VarlenOwned)>
                                  │
                                  ├── render_page() ──▶ séquence d'octets HTML
                                  │
                                  └── write(2) ──▶ fichier .html sur disque

Read Path ───────────────────────────────────────────────────────────────

  Requête HTTP ──▶ Axum ──▶ sendfile(2) ──▶ octet stream réseau
```

**PostgreSQL est le moteur de calcul.** Les projections SQL (jointures, agrégations, conditions) constituent le modèle de lecture. Chaque colonne présente dans un `StorageRow` est le résultat d'une décision de dénormalisation : la complexité a été absorbée lors de l'écriture, pas lors de la lecture.

**Les spécifications `.marius` sont des descripteurs de mise en forme.** Ils décrivent _où_ placer les données dans la sortie HTML. Ils ne calculent rien, ne joignent rien, n'agrègent rien.

**Le code Rust généré est le moteur de rendu.** `render_page()` est une séquence déterministe d'écritures en mémoire. Elle ne décide pas, n'interprète pas, ne résout pas.

**Le runtime est passif.** Le Read Path se réduit à `sendfile(2)`. L'existence d'un fichier sur disque garantit le droit d'accès — son absence produit un 404. Zéro calcul, zéro logique métier.

### 0.2 Ce que Marius n'est pas

Marius n'est pas un moteur de templates. Cette distinction n'est pas cosmétique — elle détermine les contraintes du système.

| Moteur de templates (Jinja2, Twig, Tera)       | Marius                                                                               |
| ---------------------------------------------- | ------------------------------------------------------------------------------------ |
| Interprète les gabarits au runtime             | Compile les spécifications au build-time                                             |
| Résout `{{ var }}` par lookup dans un contexte | Traduit `{{ field }}` en `push_str` / `write_fmt` natif                              |
| Évalue `{% if expr %}` dynamiquement           | `{% if bool_field %}` devient un `if` Rust sur un bit du tuple                       |
| Supporte filtres, fonctions, boucles           | Aucun de ces mécanismes — ils sont incompatibles avec l'invariant de capacité bornée |
| Le gabarit existe au runtime                   | La spécification `.marius` disparaît après compilation                               |

### 0.3 Syntaxe empruntée, sémantique propre

La syntaxe `{{ field }}` et `{% if flag %}` est délibérément empruntée aux langages de templates populaires. Cette ressemblance est **intentionnelle et instrumentale** — elle ne signifie pas que Marius soit un moteur de templates.

**Motivations de l'emprunt.**

L'outillage autour de la syntaxe Jinja2/Twig est mature : coloration syntaxique, extensions IDE, formatters, linters. Construire et maintenir un DSL propriétaire aurait exigé de développer cet écosystème en parallèle du compilateur — un coût structurellement sous-estimé. Emprunter la syntaxe transfère ce coût vers une communauté externe. Le bénéfice ergonomique est accessoire : il découle du choix d'outillage, pas l'inverse.

**Ce que la ressemblance ne signifie pas.**

Les restrictions de la grammaire Marius ne sont pas une liste de fonctionnalités manquantes à ajouter plus tard. `{% for %}` est absent parce que son introduction produirait une sortie de longueur variable non bornée au build-time, ce qui exigerait soit une surallocation conservative, soit une allocation dynamique — les deux violent l'invariant de capacité exacte. La restriction est structurelle, pas temporaire.

**La règle de lecture.**

Une spécification `.marius` se lit comme de la **notation de mise en forme** : elle dit où projeter les données et quelle structure HTML les entoure. Elle ne dit pas comment les données sont obtenues (`fetch_batch`), ni comment elles sont transformées (Write Path PostgreSQL).

### 0.4 Déplacement de la complexité — compromis assumés

Trois compromis architecturaux sont délibérément assumés dans Marius. Les nommer explicitement évite de les traiter comme des dettes techniques à corriger.

**Déplacement vers le Write Path.** La complexité relationnelle (jointures, agrégations, calculs dérivés) est résolue lors de l'écriture dans PostgreSQL et stockée dénormalisée. Chaque colonne `author_name` présente dans `content.identity` est une décision explicite : payer le coût de stockage pour éliminer le coût de calcul au moment de la lecture.

**Dénormalisation contrôlée.** La duplication de données de lecture est assumée. Ce n'est pas une dette technique — c'est le mécanisme qui rend le Read Path trivial. La règle : si une donnée est nécessaire à une spécification de page, elle doit être présente dans le schéma de l'entité correspondante, calculée en amont.

**Pre-rendering vers le disque.** Les fichiers HTML sont générés à l'écriture et servis statiquement. Ce choix échange de l'espace disque contre l'élimination totale du calcul de rendu au moment de la requête. La fraîcheur des données est garantie par le Write Path (pg_notify → Collector → Dispatcher), pas par la requête de lecture.

### 0.5 Hiérarchie des décisions

Cette hiérarchie sert un objectif de maintenabilité : un futur contributeur doit savoir immédiatement quelle catégorie de discussion est nécessaire avant de modifier une partie du système.

**Invariants fondateurs** — modifier un invariant change la nature de Marius. Toute évolution dans cette catégorie nécessite une révision architecturale complète.

- PostgreSQL est le moteur de calcul ; le runtime n'en est pas un.
- Projection déterministe : tuple plat de largeur fixe (entendu : largeur de struct statiquement connue — voir §1.2 et §13) → séquence d'octets de longueur bornée.
- Zéro allocation heap sur le hot path (`render_page()` une fois le buffer stable).
- `PAGE_TOTAL_CAP` est une borne supérieure **sans marge arbitraire** : calculée analytiquement sur les maxima théoriques de chaque champ (`max_display_width`, `max_escaped_len`), atteignable au pire cas (tous champs à leur maximum, toutes branches `{% if %}` prises).
- Zéro interprétation runtime : les spécifications `.marius` n'existent plus à l'exécution.
- Une entité par spécification de page (préservation de la largeur de struct statiquement connue — voir §1.2).

**Compromis assumés** — décisions intentionnelles fondées sur des équilibres coût/bénéfice. Réviser un compromis nécessite une décision d'équipe explicite.

- Syntaxe `{{ }}` / `{% %}` empruntée à Jinja2/Twig pour réduire le coût d'outillage.
- `Option<String>` pour tous les champs varlena en v1 (uniformité API — optimisation v2 identifiée).
- Pre-rendering vers le disque (stockage contre latence de rendu nulle).
- Dénormalisation contrôlée des données de lecture.
- `{% if %}` restreint aux champs `bool` DDL uniquement (maintien de la largeur de struct statiquement connue et pipeline déterministe).

**Choix d'implémentation** — détails remplaçables sans modifier les invariants ni les compromis. Un contributeur peut les faire évoluer par PR normale.

- `build.rs` + `include!()` pour la génération de code (vs. macros procédurales).
- Rayon pour le parallélisme de rendu (vs. threads manuels).
- Organisation interne des crates (`core/shell/forge`).
- Convention de nommage des constantes (`SCREAMING_SNAKE_CASE`).
- Format des chemins `rel_from_manifest`.

### 0.6 Journal des versions

| Version | Delta                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0.1–0.4 | Spec technique : Token enum, PageProjection, generate_page_render, module static_partials, PAGE_STATIC_CAP compile-time, FieldInfo pre-indexing, PageRenderOutput, lifecycle Rayon                                                                                                                                                                                                                                                                                                                                                 |
| 1.0     | Consolidation : cadrage architectural (§0), syntaxe empruntée (§0.3), compromis assumés (§0.4), hiérarchie des décisions (§0.5), taxonomie projection/composition (§1), entité unique reformulée sur tuple plat (§1.2), vocabulaire harmonisé                                                                                                                                                                                                                                                                                      |
| 1.1     | Stabilisation éditoriale : C-01 §1.2 "bloc mémoire contigu" corrigé ; C-02 §13 "sans indirection" corrigé ; C-03 "borne supérieure exacte" → "borne sans marge arbitraire" (§0.5, §4.1, §5.8, §6, §7, §13) ; C-04 tracking `extends_path` dans §9 ; C-05 `unreachable!` homogénéisé §5.3 ; C-06 `marius:pre_escaped` défini dans §13 ; C-07 commentaire `buf.capacity()` corrigé §7 ; D-01 `render_batch_page_persistent` déplacé en Appendice A ; D-02 §5.9 callout v2 ajouté ; F-01–F-06 clarifications terminologiques mineures |

---

## 1. Grammaire de spécification v1

### 1.1 Deux familles d'opérateurs

Les six opérateurs de la grammaire v1 appartiennent à deux familles sémantiquement distinctes. Cette distinction reflète le pipeline : certains opérateurs s'exécutent au runtime sur chaque enregistrement, d'autres n'existent qu'au build-time.

**Opérateurs de projection** — sémantique runtime. Ils se traduisent en instructions Rust dans le corps de `render_page()` et s'exécutent sur chaque enregistrement.

| Opérateur                                  | Sémantique                                                                                                                                                                                                              |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `{{ entity.field }}`                       | Projette un champ du tuple vers une position dans la séquence HTML. Équivalent de la clause `SELECT field` en SQL. Génère `write_fmt` (fixed-length) ou `marius_html_escape` + `push_str` (varlena).                    |
| `{% if entity.bool_field %} … {% endif %}` | Inclut conditionnellement un fragment HTML selon un bit booléen du tuple. Génère un `if record.field { … }` natif. Restreint aux champs `bool` DDL pour préserver la largeur de struct statiquement connue (voir §1.2). |

**Opérateurs de composition** — sémantique build-time uniquement. La forge les consomme et les résout pendant la compilation. Ils ne laissent aucune trace dans le code généré (sauf leurs effets : fusion d'ASTs, inclusion d'octets).

| Opérateur                           | Sémantique                                                                                                                                                                     |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `{% extends "path" %}`              | Déclare la spécification parent. Déclenche la fusion des ASTs au build-time. Doit être la première construction non-whitespace du fichier.                                     |
| `{% block name %} … {% endblock %}` | Déclare un point de substitution dans la spécification parent, ou sa valeur de remplacement dans la spécification enfant. Résolu par substitution textuelle pendant la fusion. |
| `{% static "path" %}`               | Inclut un fichier d'octets HTML statiques. Lu au build-time, inliné comme `&'static str` dans `static_partials`.                                                               |

**Token de base — texte brut.** Le contenu HTML hors opérateurs `{{ }}` et `{% %}` est tokenisé directement comme `Static(String)`. Ce n'est pas un opérateur au sens grammatical — c'est la matière première de la spécification, opaque, non inspectée, non échappée. Devient un `push_str` dans le code généré.

**Constructions interdites en v1.** Ces interdictions ne sont pas des limitations temporaires — elles protègent les invariants fondateurs.

- `{% for … %}` : sortie de longueur variable non bornée → viole `PAGE_TOTAL_CAP` exact.
- `{% if %}` sur un champ non-`bool` : logique d'évaluation au runtime → viole la largeur de struct statiquement connue.
- Imbrication `{% if %}`/`{% block %}` : pile d'état nécessaire → hors périmètre v1.
- Mots-clés relationnels (`join`, `where`, `filter`, `group`) dans `{% %}` : logique relationnelle → appartient au Write Path PostgreSQL.

### 1.2 Contrainte d'entité unique — largeur de struct statiquement connue et pipeline déterministe

**Fondement.** `render_page()` est définie comme une projection déterministe d'un **tuple de largeur de struct fixe et connue** vers une **séquence d'octets de longueur bornée et connue**. Cette définition exige qu'une spécification soit liée à exactement une entité.

**Tuple plat.** `StorageRow` est un `#[repr(C)]` dont la taille est une constante de compilation. `VarlenOwned` porte les données variables dont la borne supérieure est calculée statiquement via `max_escaped_len`. La taille des deux structs est connue à la compilation. Le vecteur `Vec<(Record, VarlenOwned)>` est contigu en mémoire pour les métadonnées de struct ; le payload des champs varlena est en heap. L'itération séquentielle sur le vecteur bénéficie du prefetch CPU pour les métadonnées. L'introduction d'une seconde entité imposerait soit une structure composite (sous-structures imbriquées — largeur de struct non statique), soit deux pointeurs séparés (indirection supplémentaire, perte de localité cache et rupture du modèle de tuple atomique).

**Pipeline linéaire.** `fetch_batch` retourne `Vec<(Record, VarlenOwned)>` — un vecteur de tuples atomiques, chacun extrait en une passe SQL. Fusionner deux entités nécessiterait un JOIN dans `fetch_batch` ou deux batches coordonnés, introduisant une synchronisation absente du pipeline actuel.

**Capacité déterministe.** `PAGE_TOTAL_CAP = PAGE_STATIC_CAP + PAGE_DYNAMIC_CAP` est calculé sur l'ensemble exact des champs d'une entité connue au build-time. Avec deux entités dont les champs pourraient partager des noms, la résolution `max_display_width` / `max_escaped_len` deviendrait ambiguë sans qualification supplémentaire — complexité que la contrainte d'entité unique supprime structurellement.

**Conséquence opérationnelle.** Toute donnée d'une entité secondaire doit être pré-agrégée dans l'entité principale lors du Write Path (colonne `GENERATED STORED`, vue matérialisée). La complexité du JOIN est absorbée une fois à l'écriture ; le Read Path en hérite zéro.

**Exemple d'erreur AOT et guide de migration.** Si un développeur écrit `{{ author.name }}` dans une spécification liée à `content_core` :

```
[fragment-forge] Erreur de compilation :
templates/content/core.marius:18 — entité inconnue `author`.
Cette spécification est liée à `content_core`.

  18 | <span class="byline">{{ author.name }}</span>
                             ^^^^^^^^^^^^

Les données d'entités secondaires doivent être pré-agrégées côté PostgreSQL.
Options :
  1. Colonne GENERATED STORED dans content.identity :
       ALTER TABLE content.identity
           ADD COLUMN author_name text
           GENERATED ALWAYS AS (
               (SELECT name FROM entities.person WHERE id = author_entity_id)
           ) STORED;
  2. Vue matérialisée content.core_with_author + schéma Marius dédié.
  3. `{{ content_core.author_name }}` si le champ existe déjà dans le schéma.
```

---

## 2. Spécifications de page — exemples

### 2.1 `templates/base.marius` — spécification parent

```
<!DOCTYPE html>
<html lang="fr">
<head>
  <meta charset="UTF-8">
  <title>{% block title %}Marius{% endblock %}</title>
  <link rel="stylesheet" href="/static/app.css">
</head>
<body>
{% static "templates/partials/nav.html" %}
<main>
{% block content %}{% endblock %}
</main>
{% static "templates/partials/footer.html" %}
</body>
</html>
```

### 2.2 `templates/partials/nav.html` — composant statique

```html
<nav class="site-nav">
  <a href="/">Accueil</a>
  <a href="/articles">Articles</a>
</nav>
```

Contenu de confiance (origine développeur, hors PostgreSQL). Non passé dans `marius_html_escape`.

### 2.3 `templates/content/core.marius` — spécification enfant

```
{% extends "templates/base.marius" %}

{% block title %}{{ content_core.headline }}{% endblock %}

{% block content %}
<article class="content-core" data-id="{{ content_core.document_id }}">
  <h1>{{ content_core.headline }}</h1>
  {% if content_core.is_readable %}
  <p>{{ content_core.description }}</p>
  {% endif %}
</article>
{% endblock %}
```

Observations : `headline` apparaît dans deux blocs → `PAGE_DYNAMIC_CAP += 2 × max_escaped_len(headline)`. `is_readable: true` dans les tests pire cas (branche prise). `document_id` est `i32` fixed-length. `entity_name = "content_core"`.

---

## 3. Pipeline de compilation AOT

### 3.1 Détection du mode

La forge opère en **mode fragment** (comportement existant inchangé) ou en **mode page**. Le discriminant est la première construction non-whitespace du fichier : `{% extends "path" %}` → mode page, tout autre contenu → mode fragment.

En l'absence de `{% extends %}`, le fichier est traité en mode fragment — aucune `PageProjection` n'est générée pour cette spécification. Les invariants du mode fragment (spec fragment-forge existante) s'appliquent.

BOM, lignes vides et whitespace-only sont tolérés avant `{% extends %}`. Si `{% extends %}` est présent mais non en première position : `PageParseError::ExtendsNotFirst`.

### 3.2 Token enum interne

```rust
pub enum PageToken {
    Static(String),
    Field     { entity: String, field: String },
    IfBool    { entity: String, field: String },
    EndIf,
    BlockDecl { name: String, default: Vec<PageToken> },
    StaticFile { path: String },
}

/// Séquence aplatie post-merge.
/// Les opérateurs de composition (BlockDecl, StaticFile) ont été résolus
/// et n'apparaissent plus dans cette séquence.
pub enum FlatPageToken {
    Static(String),
    Field    { entity: String, field: String },
    IfBool   { entity: String, field: String },
    EndIf,
    /// Opérateur {% static %} résolu : fichier lu au build-time.
    /// `original_path` : chemin tel qu'écrit dans la spécification.
    /// `rel_from_manifest` : chemin normalisé pour include_str!.
    /// `len` : longueur lue, pour validation et PAGE_STATIC_CAP.
    StaticInclude { original_path: String, rel_from_manifest: String, len: usize },
}
```

### 3.3 Algorithme de fusion AST

```
Entrées : child_path, template_root, manifest_dir, entity_name, fields, varlena

Étape 1 — Parse spécification enfant :
  lire child.marius → extends_path + child_blocks : HashMap<name, Vec<PageToken>>

Étape 2 — Parse spécification parent :
  lire base.marius[extends_path] → tokenize → Vec<PageToken>

Étape 3 — Fusion (opérateurs de composition) :
  pour chaque BlockDecl(name, default) dans base_tokens :
    substitue child_blocks[name] si présent, conserve default sinon
  erreur AOT si un bloc de l'enfant est absent de la base (orphelin)

Étape 4 — Résolution des StaticFile (opérateurs d'inclusion) :
  pour chaque StaticFile { path } :
    absolute = (template_root / path).canonicalize()
    len      = read_to_string(absolute).len()
    rel      = relative_path_for_include_str(manifest_dir, absolute)
    émettre  : cargo:rerun-if-changed={absolute}
    émettre  : cargo:warning! si len > seuil (voir §5.7)
    remplacer par StaticInclude { original_path, rel_from_manifest: rel, len }

Étape 5 — Validation sémantique :
  pour chaque Field et IfBool :
    entity != entity_name  → UnknownEntity (avec guide de migration)
    field ∉ fields ∪ varlena  → UnknownField
    IfBool et field.kind != Bool  → NonBoolIfCondition
  {% for %}  → ForLoopDetected
  mot-clé relationnel  → RelationalKeyword
  imbrication bloc/if  → NestedBlock / NestedIf

Résultat : Vec<FlatPageToken>
  contient uniquement : Static | Field | IfBool | EndIf | StaticInclude
```

Tracking : `cargo:rerun-if-changed` pour `child_path` et `extends_path` (dans `build.rs`) ; pour chaque `StaticFile` (dans `parse_page_template`, étape 4).

---

## 4. Calcul de `PAGE_TOTAL_CAP`

### 4.1 Formule et propriétés

```
page_sc_build_time = Σ s.len()  pour FlatPageToken::Static(s)
                   + Σ len      pour FlatPageToken::StaticInclude { len, .. }

page_dc            = Σ max_width(field)  pour FlatPageToken::Field { field, .. }

  max_width(field) = max_display_width(kind)  si fixed-length
                   = max_escaped_len(v)       si varlena

PAGE_TOTAL_CAP = page_sc_build_time + page_dc
```

`IfBool` et `EndIf` : contribution nulle. Tokens dans un bloc `{% if %}` : inclus au worst-case (branche toujours prise — invariant de borne supérieure). Champ présent N fois → compté N fois.

`PAGE_TOTAL_CAP` est une borne supérieure **sans marge arbitraire** — calculée analytiquement sur les maxima théoriques de chaque champ, atteignable au pire cas (tous champs à leur maximum, toutes branches `{% if %}` prises). L'absence de marge est un invariant fondateur (voir §0.5) : toute marge ajoutée masquerait une sous-estimation dans `max_escaped_len` ou `max_display_width`. Le mécanisme de validation est le test `no_realloc` (§11) et le fuzz test `max_escaped_len` (§5.8).

### 4.2 Invariant de passe unique — résolution sémantique et mesure de capacité fusionnées

La résolution sémantique (validation des entités/champs référencés, résolution des `StaticInclude`) et le calcul de capacité (`PAGE_STATIC_CAP`, `PAGE_DYNAMIC_CAP`) ne sont **pas** deux parcours distincts de `flat`. Itérer deux fois sur le même AST — une fois pour résoudre la structure, une fois pour mesurer la capacité — gaspillerait des cycles CPU et de la localité de cache pour un gain de modularité illusoire : les deux opérations consomment le même flux de tokens dans le même ordre.

`resolve_and_measure` fusionne les deux responsabilités en une seule passe `O(N)` :

```rust
pub fn resolve_and_measure<'src>(
    tokens: &mut [FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>>;
```

Pour chaque token, dans le même parcours :

- `Static(s)` → accumule `s.len()` dans `total_static_bytes`.
- `StaticInclude { rel_from_manifest, len, .. }` → résout `len` via `get_file_size`, accumule dans `total_static_bytes`.
- `Field { field, .. }` → recherche `field` dans `SchemaIndex` (fixed ou varlena) ; absent → `ResolverError::UnknownField` ; présent → accumule `max_display_width` ou `max_escaped_len` dans `total_dynamic_bytes`.
- `IfBool { field, .. }` → validation de présence dans le schéma uniquement (pas de contribution à la capacité — le bloc est mesuré au pire cas via ses tokens internes, comptés normalement).
- `EndIf` → aucun effet.

La formule mathématique de capacité (§4.1) est inchangée : `PAGE_TOTAL_CAP = page_sc_build_time + page_dc`. Seul le mécanisme de calcul change — une passe au lieu de deux, ce qui maximise la localité du cache CPU sur l'itération de l'AST. `TemplateMetrics { total_static_bytes, total_dynamic_bytes, include_count }` est l'unique structure produite ; il n'existe plus de fonction de capacité indépendante de la résolution sémantique.

---

## 5. Fonctions publiques dans `fragment-forge/src/lib.rs`

### 5.1 Signatures

```rust
pub fn detect_extends(source: &str) -> Result<Option<&str>, PageParseError>;

pub fn parse_page_template(
    child_path:    &std::path::Path,
    template_root: &std::path::Path,
    manifest_dir:  &std::path::Path,
    entity_name:   &str,
    fields:        &[FieldSpec],
    varlena:       &[VarlenField],
) -> Result<Vec<FlatPageToken>, PageParseError>;

/// Unique point de vérité pour la résolution sémantique et la capacité (§4.2).
/// Remplace toute fonction de capacité indépendante — il n'existe pas de
/// `page_capacity` distinct : la mesure est fusionnée dans la même passe que
/// la validation des champs/entités référencés par l'AST.
pub fn resolve_and_measure<'src>(
    tokens: &mut [FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>>;

/// "templates/partials/nav.html" → "PARTIALS_NAV"
pub fn static_const_ident(original_path: &str) -> String;

/// Génère `pub mod static_partials { … }` — déduplication structurelle .rodata.
pub fn generate_static_partials_module(entries: &[(String, String)]) -> String;

pub struct PageRenderOutput {
    pub page_sc_build_time: usize,
    pub page_dc:            usize,
    /// Expression compile-time : "static_partials::X.len() + … + N"
    pub static_cap_expr:    String,
    pub body:               String,
}

pub fn generate_page_render(
    flat:          &[FlatPageToken],
    fields:        &[FieldSpec],
    varlena:       &[VarlenField],
    static_idents: &std::collections::HashMap<String, String>,
) -> PageRenderOutput;

pub fn generate_page_capacity_consts(
    screaming: &str, static_cap_expr: &str, page_dc: usize,
) -> String;
```

### 5.2 Messages d'erreur AOT

```rust
#[derive(Debug)]
pub struct SourceLocation { pub file: String, pub line: usize, pub excerpt: Option<String> }

#[derive(Debug)]
pub enum PageParseError {
    ExtendsNotFound    { path: String },
    ExtendsNotFirst    { loc: SourceLocation },
    StaticFileNotFound { path: String, loc: SourceLocation },
    OrphanBlock        { name: String, loc: SourceLocation },
    UnknownEntity      { found: String, expected: String, loc: SourceLocation },
    UnknownField       { entity: String, field: String,   loc: SourceLocation },
    NonBoolIfCondition { entity: String, field: String,   loc: SourceLocation },
    ForLoopDetected    { loc: SourceLocation },
    RelationalKeyword  { keyword: String,                 loc: SourceLocation },
    NestedBlock        { loc: SourceLocation },
    NestedIf           { loc: SourceLocation },
}
```

Format des messages : `fichier:ligne — description`, extrait de la ligne fautive, suggestion corrective. Exemples complets dans l'analyse `analyse-vocabulaire-marius-spec.md` §5.2.

Intégration `build.rs` :

```rust
.unwrap_or_else(|e| panic!("\n\n[fragment-forge] Erreur de compilation :\n{e}\n"))
```

### 5.3 Implémentation de `generate_page_render()`

```rust
pub fn generate_page_render(
    flat: &[FlatPageToken], fields: &[FieldSpec], varlena: &[VarlenField],
    static_idents: &std::collections::HashMap<String, String>,
) -> PageRenderOutput {
    let field_index = build_field_index(fields, varlena);
    let (mut sc, mut dc) = (0usize, 0usize);
    let mut static_literal_len = 0usize;
    let mut include_terms: Vec<String> = Vec::new();

    for token in flat {
        match token {
            FlatPageToken::Static(s) => { sc += s.len(); static_literal_len += s.len(); }
            FlatPageToken::StaticInclude { original_path, len, .. } => {
                sc += len;
                let ident = static_idents.get(original_path.as_str()).unwrap();
                include_terms.push(format!("static_partials::{ident}.len()"));
            }
            FlatPageToken::Field { field, .. } => match field_index.get(field.as_str()) {
                Some(FieldInfo::Fixed(w))   => dc += w,
                Some(FieldInfo::Varlena(w)) => dc += w,
                None => unreachable!("validé à l'étape 5"),
            },
            FlatPageToken::IfBool { .. } | FlatPageToken::EndIf => {}
        }
    }

    let static_cap_expr = {
        let mut terms = include_terms;
        if static_literal_len > 0 { terms.push(static_literal_len.to_string()); }
        if terms.is_empty() { "0".to_string() } else { terms.join("\n    + ") }
    };

    let mut c = String::new();
    // PAGE_STATIC_CAP : expression compile-time évaluée par rustc sur les octets réels.
    // Immune aux désynchronisations CRLF/LF entre la lecture forge (build.rs) et rustc.
    c.push_str(&format!(
        "const PAGE_STATIC_CAP: usize =\n    {static_cap_expr};\n\
         const PAGE_DYNAMIC_CAP: usize = {dc};\n\
         buf.reserve(PAGE_STATIC_CAP + PAGE_DYNAMIC_CAP);\n"
    ));

    // Reconstruction locale des &str depuis VarlenOwned (ADR-003 — zéro copie).
    let mut varlena_used: Vec<&str> = flat.iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. }
                if matches!(field_index.get(field.as_str()), Some(FieldInfo::Varlena(_)))
                => Some(field.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>().into_iter().collect();
    varlena_used.sort();
    for n in &varlena_used {
        c.push_str(&format!("let {n}_ref: Option<&str> = varlena.{n}.as_deref();\n"));
    }

    for token in flat {
        match token {
            FlatPageToken::Static(s) =>
                c.push_str(&format!("buf.push_str({});\n", rust_raw_str_lit(s))),
            FlatPageToken::StaticInclude { original_path, .. } => {
                let ident = static_idents.get(original_path.as_str()).unwrap();
                c.push_str(&format!("buf.push_str(static_partials::{ident});\n"));
            }
            FlatPageToken::Field { field, .. }
                if matches!(field_index.get(field.as_str()), Some(FieldInfo::Fixed(_))) =>
                c.push_str(&format!(
                    "::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{field})).ok();\n"
                )),
            FlatPageToken::Field { field, .. } =>
                c.push_str(&format!(
                    "if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}\n"
                )),
            FlatPageToken::IfBool { field, .. } =>
                c.push_str(&format!("if record.{field} {{\n")),
            FlatPageToken::EndIf => c.push_str("}\n"),
        }
    }

    PageRenderOutput { page_sc_build_time: sc, page_dc: dc, static_cap_expr, body: c }
}
```

### 5.4 `generate_static_partials_module()` et `static_const_ident()`

```rust
pub fn static_const_ident(original_path: &str) -> String {
    std::path::Path::new(original_path)
        .with_extension("")
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy().to_string();
            if s == "templates" { None } else { Some(s.to_uppercase()) }
        })
        .collect::<Vec<_>>()
        .join("_")
}

pub fn generate_static_partials_module(entries: &[(String, String)]) -> String {
    if entries.is_empty() { return String::new(); }
    let mut out = String::from(
        "/// Composants statiques partagés — garantie structurelle de déduplication .rodata.\n\
         /// Plusieurs spécifications référençant le même fichier partagent la même constante.\n\
         pub mod static_partials {\n"
    );
    for (rel, ident) in entries {
        out.push_str(&format!(
            "    pub const {ident}: &str = include_str!(concat!(\n\
             \        env!(\"CARGO_MANIFEST_DIR\"), \"{rel}\"\n\
             \    ));\n"
        ));
    }
    out.push_str("}\n");
    out
}
```

### 5.5 `rust_raw_str_lit` — implémentation et preuve d'invariance

```rust
fn rust_raw_str_lit(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut max_quote_hashes = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' { j += 1; }
            let count = j - i - 1;
            if count > max_quote_hashes { max_quote_hashes = count; }
            i = j;
        } else { i += 1; }
    }
    let hashes = "#".repeat(max_quote_hashes + 1);
    format!("r{hashes}\"{s}\"{hashes}")
}
```

**Preuve d'invariance.** Un raw string de niveau N termine sur `"#{N}` dans le contenu. L'algorithme calcule `level = max(k pour toute sous-chaîne "#{k}) + 1`. Toute sous-chaîne `"#{k}` a `k ≤ level − 1 < level`. Donc `"#{level}` n'apparaît jamais dans le contenu. ∎

### 5.6 `relative_path_for_include_str` — portabilité

```rust
fn relative_path_for_include_str(manifest_dir: &std::path::Path, target: &std::path::Path) -> String {
    let mc: Vec<_> = manifest_dir.components().collect();
    let tc: Vec<_> = target.components().collect();
    let common = mc.iter().zip(tc.iter()).take_while(|(a, b)| a == b).count();
    let mut result = String::new();
    for _ in 0..mc.len() - common { result.push_str("/.."); }
    for c in &tc[common..] {
        result.push('/');
        result.push_str(&c.as_os_str().to_string_lossy().replace('\\', "/"));
    }
    result
}
```

Normalisation `\` → `/` obligatoire : `include_str!` est portable avec `/` sur toutes plateformes. Tests unitaires Unix + Windows dans §15.

### 5.7 Alerte statics volumineux

```rust
const DEFAULT_STATIC_WARN_BYTES: usize = 32 * 1024;

fn static_size_threshold() -> usize {
    std::env::var("FRAGMENT_FORGE_STATIC_WARN_BYTES")
        .ok().and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_STATIC_WARN_BYTES)
}
// Dans resolve_static_file() :
if len > static_size_threshold() {
    println!("cargo:warning=fragment-forge : `{path}` dépasse {len}B. \
              Envisager un service statique externe. \
              Seuil : FRAGMENT_FORGE_STATIC_WARN_BYTES.");
}
```

Politique : < 32 KB → `static_partials` ; 32–200 KB → évaluer ; > 200 KB → service externe.

### 5.8 Fuzz test `max_escaped_len`

Facteur d'expansion maximal : `"` → `&quot;` = ×6. La valeur correcte est `max_escaped_len = max_len × 6` — ni inférieure (sous-estimation, le test échoue), ni supérieure (marge arbitraire violant l'invariant de borne sans marge, §0.5). Un échec du test doit pointer vers `VarlenField::max_escaped_len()`, pas vers `PAGE_TOTAL_CAP`.

```rust
#[cfg(test)]
mod tests_max_escaped_len {
    fn reference_escape(s: &str) -> String {
        s.chars().fold(String::with_capacity(s.len() * 6), |mut o, c| {
            match c { '&' => o.push_str("&amp;"), '<' => o.push_str("&lt;"),
                      '>' => o.push_str("&gt;"), '"' => o.push_str("&quot;"),
                      '\'' => o.push_str("&#39;"), _ => o.push(c) } o
        })
    }
    fn assert_sufficient(max_len: usize, max_escaped_len: usize) {
        for s in ["\"".repeat(max_len), "&".repeat(max_len),
                  r#"<>&"'"#.chars().cycle().take(max_len).collect::<String>()] {
            let escaped = reference_escape(&s);
            assert!(escaped.len() <= max_escaped_len,
                "max_escaped_len SOUS-ESTIMÉ (max_len={max_len}): \
                 max_escaped_len={max_escaped_len}, réel={} pour {s:?}. \
                 Corriger VarlenField::max_escaped_len().", escaped.len());
        }
    }
    #[test] fn lower_bound_factor_6() {
        for &n in &[10usize, 100, 255, 1000, 10_000] { assert_sufficient(n, n * 6); }
    }
}
```

### 5.9 Optimisation future (v2) — Non-nullité DDL

> **Note d'intention v2 — hors périmètre v1.** Cette section documente une évolution identifiée mais ne fait pas partie de la v1. Elle ne doit pas être implémentée sans une ADR dédiée (préconditions cassantes listées ci-dessous).

Pour les champs varlena `NOT NULL` en DDL, `if let Some(s) = field_ref { … }` est un branchement dont la branche `None` est sémantiquement impossible. Le remplacer par `marius_html_escape(varlena.field.as_str(), buf)` élimine le branchement.

**Préconditions (hors périmètre v1) :**

1. `VarlenField.nullable: bool` depuis l'introspection DDL dans DB-Forge.
2. Champs `NOT NULL` → `String` (pas `Option<String>`) dans `VarlenOwned` — changement cassant.

Compromis actuel (v1) : `Option<String>` uniforme pour toute l'API `VarlenOwned`. L'uniformité simplifie le trait `Projection` ; le branchement est bien prédit sur le hot path.

---

## 6. Génération des constantes de capacité

```rust
pub fn generate_page_capacity_consts(screaming: &str, static_cap_expr: &str, page_dc: usize) -> String {
    format!(
        "/// Octets statiques page (expression compile-time, immune CRLF/LF).\n\
         pub const {screaming}_PAGE_STATIC_CAP: usize =\n    {static_cap_expr};\n\
         /// Largeurs max des valeurs dynamiques — pire cas avec multiplicité.\n\
         pub const {screaming}_PAGE_DYNAMIC_CAP: usize = {page_dc};\n\
         /// Borne supérieure sans marge arbitraire — invariant fondateur (§0.5).\n\
         pub const {screaming}_PAGE_TOTAL_CAP: usize =\n\
             {screaming}_PAGE_STATIC_CAP + {screaming}_PAGE_DYNAMIC_CAP;\n"
    )
}
```

`PAGE_STATIC_CAP` est une expression Rust évaluée par rustc : `static_partials::X.len() + … + N`. rustc calcule sur les octets réels embarqués dans le binaire, indépendamment de ce que la forge a lu lors de la passe build.rs. Cette propriété élimine le risque de désynchronisation CRLF/LF sur des environnements mixtes.

> **Note — dualité constantes locales / constantes de module.** §5.3 génère des constantes locales à `render_page()` (`PAGE_STATIC_CAP`, `PAGE_DYNAMIC_CAP`) pour le `buf.reserve()` interne. Les constantes publiques générées ici (`{SCREAMING}_PAGE_STATIC_CAP`, `{SCREAMING}_PAGE_DYNAMIC_CAP`, `{SCREAMING}_PAGE_TOTAL_CAP`) exposent les mêmes valeurs à l'extérieur du module — elles permettent leur référencement hors du corps de `render_page()` (benchmarks, tests, Dispatcher). Ce n'est pas une duplication : ce sont deux portées distinctes pour la même quantité.

---

## 7. Trait `PageProjection`

```rust
// crates/core/projection/src/lib.rs

/// Extension optionnelle de Projection pour les entités ayant une spécification de page.
/// Le nom `Projection` est délibéré : il reflète la clause SELECT du SQL —
/// sélection et mise en forme d'un sous-ensemble de colonnes d'un tuple.
/// `render_page()` est l'équivalent HTML de cette opération.
pub trait PageProjection: Projection {
    /// Borne supérieure calculée analytiquement au build-time, sans marge arbitraire (§0.5).
    /// Après le premier appel à render_page(), buf.capacity() ≥ PAGE_TOTAL_CAP.
    /// Les appels suivants après buf.clear() sont sans allocation (invariant fondateur).
    const PAGE_TOTAL_CAP: usize;

    /// Projection déterministe du tuple (record, varlena) vers une séquence d'octets HTML.
    /// buf.reserve() est appelé en début de fonction. L'appelant passe String::new().
    fn render_page(record: &Self::Record, varlena: &Self::VarlenOwned, buf: &mut String);
}
```

**Dispatcher — `render_batch_page_pure`.** Pattern `map_with` symétrique à `render_batch_pure`, seed `(String::new(), 0usize /* compteur de bytes écrits par thread — pour métriques optionnelles */)`. O(T) allocations initiales par batch (une par thread Rayon), zéro par enregistrement ensuite (voir §14 pour le lifecycle complet).

---

## 8. Code généré dans `generated_schema.rs`

```rust
// ── Déduplication structurelle .rodata ───────────────────────────────────────
// Plusieurs spécifications référençant les mêmes fichiers statiques partagent
// ces constantes — un seul &'static str dans .rodata par fichier.

pub mod static_partials {
    pub const PARTIALS_NAV: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/../../../templates/partials/nav.html"
    ));
    pub const PARTIALS_FOOTER: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"), "/../../../templates/partials/footer.html"
    ));
}

// ── Capacités — content_core page ────────────────────────────────────────────

pub const CONTENT_CORE_PAGE_STATIC_CAP: usize =
    static_partials::PARTIALS_NAV.len()       // octets réels dans le binaire
    + static_partials::PARTIALS_FOOTER.len()  // octets réels dans le binaire
    + 114;  // Σ len() des Static tokens (littéraux du template)

pub const CONTENT_CORE_PAGE_DYNAMIC_CAP: usize = /* Σ max_width × occurrences */;
pub const CONTENT_CORE_PAGE_TOTAL_CAP: usize =
    CONTENT_CORE_PAGE_STATIC_CAP + CONTENT_CORE_PAGE_DYNAMIC_CAP;

// ── Projection déterministe ───────────────────────────────────────────────────

impl marius_projection::PageProjection for ContentCoreProjection {
    const PAGE_TOTAL_CAP: usize = CONTENT_CORE_PAGE_TOTAL_CAP;

    fn render_page(record: &ContentCoreStorageRow, varlena: &ContentCoreVarlenOwned, buf: &mut String) {
        const PAGE_STATIC_CAP: usize =
            static_partials::PARTIALS_NAV.len()
            + static_partials::PARTIALS_FOOTER.len()
            + 114;
        const PAGE_DYNAMIC_CAP: usize = /* idem */;
        buf.reserve(PAGE_STATIC_CAP + PAGE_DYNAMIC_CAP);

        let headline_ref:    Option<&str> = varlena.headline.as_deref();
        let description_ref: Option<&str> = varlena.description.as_deref();

        buf.push_str(r#"<!DOCTYPE html><html lang="fr"><head><meta charset="UTF-8"><title>"#);
        if let Some(s) = headline_ref { marius_html_escape(s, buf); }  // occurrence 1
        buf.push_str(r#"</title><link rel="stylesheet" href="/static/app.css"></head><body>"#);
        buf.push_str(static_partials::PARTIALS_NAV);
        buf.push_str(r#"<main><article class="content-core" data-id=""#);
        ::std::fmt::Write::write_fmt(buf, format_args!("{}", record.document_id)).ok();
        buf.push_str(r#""><h1>"#);
        if let Some(s) = headline_ref { marius_html_escape(s, buf); }  // occurrence 2
        buf.push_str("</h1>\n");
        if record.is_readable {
            buf.push_str("<p>");
            if let Some(s) = description_ref { marius_html_escape(s, buf); }
            buf.push_str("</p>\n");
        }
        buf.push_str("</article></main>\n");
        buf.push_str(static_partials::PARTIALS_FOOTER);
        buf.push_str("</body></html>");
    }
}
```

`render_page()` n'appelle pas `render()`. Les deux fonctions coexistent et sont indépendantes : `render()` sert aux mises à jour partielles HTMX ; `render_page()` sert à la génération initiale du document.

---

## 9. Intégration dans `build.rs` de `marius-schema`

```rust
use marius_fragment_forge::{detect_extends, parse_page_template, resolve_and_measure, SchemaIndex,
    generate_page_render, generate_page_capacity_consts,
    generate_static_partials_module, static_const_ident};
use std::collections::HashMap;

let manifest_dir  = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
    .canonicalize().expect("CARGO_MANIFEST_DIR");
let template_root = manifest_dir.join("../../..").canonicalize().expect("workspace root");

// Passe 1 : parser les spécifications de page.
// Tracking de child_path (voir §3.3 — child_path et extends_path doivent tous deux être trackés).
println!("cargo:rerun-if-changed={}", template_root.join("templates/content/core.marius").display());
// Tracking de extends_path (spec parent) — obligatoire selon §3.3 : un changement du parent
// invalide la PageProjection générée même si l'enfant est inchangé.
println!("cargo:rerun-if-changed={}", template_root.join("templates/base.marius").display());
let child_path = template_root.join("templates/content/core.marius");
let source = std::fs::read_to_string(&child_path).expect("spécification absente");

let content_core_flat = detect_extends(&source)
    .unwrap_or_else(|e| panic!("\n\n[fragment-forge] Erreur de compilation :\n{e}\n"))
    .map(|_| parse_page_template(
        &child_path, &template_root, &manifest_dir,
        "content_core", &content_core_fields, &content_core_varlena,
    ).unwrap_or_else(|e| panic!("\n\n[fragment-forge] Erreur de compilation :\n{e}\n")));

// Passe 2 : collecter les StaticInclude uniques → const_idents.
let mut unique_statics: HashMap<String, (String, String)> = HashMap::new(); // path → (rel, ident)
if let Some(flat) = &content_core_flat {
    for token in flat {
        if let marius_fragment_forge::FlatPageToken::StaticInclude { original_path, rel_from_manifest, .. } = token {
            unique_statics.entry(original_path.clone())
                .or_insert_with(|| (rel_from_manifest.clone(), static_const_ident(original_path)));
        }
    }
}

// Passe 3 : générer le module static_partials.
let mut partials: Vec<(String, String)> = unique_statics.values()
    .map(|(rel, ident)| (rel.clone(), ident.clone())).collect();
partials.sort_by_key(|(_, id)| id.clone());
out.push_str(&generate_static_partials_module(&partials));

// Passe 4 : générer les impl PageProjection.
let static_idents: HashMap<String, String> = unique_statics.into_iter()
    .map(|(path, (_, ident))| (path, ident)).collect();

if let Some(flat) = &content_core_flat {
    let mut flat_mut = flat.clone();
    let schema_index = SchemaIndex { fixed: &content_core_fields, varlena: &content_core_varlena };
    let metrics = resolve_and_measure(&mut flat_mut, &schema_index, get_file_size)
        .unwrap_or_else(|e| panic!("\n\n[fragment-forge] Erreur de résolution :\n{e:?}\n"));

    let output = generate_page_render(flat, &content_core_fields, &content_core_varlena, &static_idents);
    debug_assert_eq!(output.page_sc_build_time, metrics.total_static_bytes);

    out.push_str(&generate_page_capacity_consts("CONTENT_CORE", &output.static_cap_expr, metrics.total_dynamic_bytes));
    out.push_str(&format!(
        "impl marius_projection::PageProjection for ContentCoreProjection {{\n\
             const PAGE_TOTAL_CAP: usize = CONTENT_CORE_PAGE_TOTAL_CAP;\n\
             fn render_page(record: &ContentCoreStorageRow, varlena: &ContentCoreVarlenOwned, \
             buf: &mut String) {{\n{}}}\n}}\n",
        output.body
    ));
}
```

---

## 10. Certification et benchmarks

### 10.1 `hot_path_certify.rs` — extension page

```rust
use marius_schema::CONTENT_CORE_PAGE_TOTAL_CAP;
use marius_projection::PageProjection;

/// Certifie que render_page() n'alloue pas une fois le buffer stable.
/// Protocole : String::new() → premier render_page() (allocate) → clear() → reset() → render_page() → assert.
/// is_readable = true : branche {% if %} prise → pire cas de buf.len().
#[divan::bench(name = "certify/zero_alloc_in_render_page", sample_count = 100)]
fn bench_certify_zero_alloc_page(bencher: Bencher) {
    bencher
        .with_inputs(|| {
            let (mut storage, varlena) = record_worst_case();
            storage.is_readable = true;
            let mut buf = String::new();
            ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
            (storage, varlena, buf)
        })
        .bench_local_values(|(storage, varlena, mut buf)| {
            buf.clear();
            CountingAlloc::reset();
            ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
            assert_eq!(CountingAlloc::alloc_count(), 0,
                "CERTIFICATION ÉCHOUÉE render_page. \
                 Vérifier : (1) multiplicité Field tokens dans resolve_and_measure(), \
                 (2) VarlenField::max_escaped_len(), (3) len StaticInclude, \
                 (4) is_readable=true pour branche {% if %}.");
            black_box(&buf);
        });
}
```

### 10.2 `hot_path_render.rs` — extension page

```rust
#[divan::bench(name = "render/page/nominal")]
fn bench_render_page_nominal(bencher: Bencher) {
    let (storage, varlena) = record_nominal();
    bencher.counter(ItemsCount::new(1usize)).counter(BytesCount::new(CONTENT_CORE_PAGE_TOTAL_CAP))
        .with_inputs(String::new)
        .bench_local_values(|mut buf| {
            ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
            black_box(buf.len())
        });
}

#[divan::bench(name = "render/page/worst_case")]
fn bench_render_page_worst_case(bencher: Bencher) {
    let (mut storage, varlena) = record_worst_case();
    storage.is_readable = true;
    bencher.counter(ItemsCount::new(1usize)).counter(BytesCount::new(CONTENT_CORE_PAGE_TOTAL_CAP))
        .with_inputs(String::new)
        .bench_local_values(|mut buf| {
            ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
            black_box(buf.len())
        });
}

#[divan::bench(name = "render/rayon/page/nominal", args = BATCH_SIZES)]
fn bench_render_rayon_page_nominal(bencher: Bencher, batch_size: usize) {
    bencher.counter(ItemsCount::new(batch_size))
        .counter(BytesCount::new(batch_size * CONTENT_CORE_PAGE_TOTAL_CAP))
        .with_inputs(|| batch(batch_size, record_nominal))
        .bench_local_values(|records| {
            render_batch_page_pure::<ContentCoreProjection>(black_box(records));
        });
}
```

### 10.3 Jobs CI

```yaml
# .github/workflows/ci.yml

certify-no-alloc:
  name: Certify zero-alloc (single-thread)
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: RAYON_NUM_THREADS=1 cargo bench -p marius-render --bench hot_path_certify -- --quiet

bench-throughput:
  name: Benchmark throughput (multi-thread)
  runs-on: ubuntu-latest
  if: github.ref == 'refs/heads/main'
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo bench -p marius-render --bench hot_path_render

test-forge:
  name: Tests forge (${{ matrix.os }})
  runs-on: ${{ matrix.os }}
  strategy:
    matrix: { os: [ubuntu-latest, windows-latest] }
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo test -p marius-fragment-forge --lib

integration-aot:
  name: Integration AOT end-to-end
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo build -p marius-schema
    - run: cargo test -p marius-schema -- --nocapture
```

`RAYON_NUM_THREADS=1` : certification déterministe et reproductible en CI. Job `bench-throughput` conditionnel à `main` : mesure le débit réel en multi-thread, non bloquant.

---

## 11. Tests dans `marius-schema`

```rust
#[test]
fn test_content_core_page_no_realloc() {
    let storage = ContentCoreStorageRow {
        published_at: i64::MIN, created_at: i64::MIN, modified_at: i64::MIN,
        document_id: i32::MIN, author_entity_id: i32::MIN, status: i16::MIN,
        is_readable: true,   // branche {% if %} prise → pire cas
        is_commentable: false, is_visible_comments: false,
    };
    let varlena = ContentCoreVarlenOwned { ..Default::default() };
    let mut buf = String::new();
    ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
    let stable_cap = buf.capacity();
    buf.clear();
    ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
    assert_eq!(buf.capacity(), stable_cap,
        "REALLOC : PAGE_TOTAL_CAP ({CONTENT_CORE_PAGE_TOTAL_CAP}B) sous-estimé. \
         Vérifier : multiplicité Field tokens, max_escaped_len, len StaticInclude.");
    assert!(buf.starts_with("<!DOCTYPE html>") && buf.ends_with("</html>"));
    assert!(buf.contains("<nav") && buf.contains("<footer") && buf.contains("<p>"));
    println!("[no-realloc] page: cap={stable_cap}, len={}, ratio={:.0}%",
        buf.len(), buf.len() as f64 / stable_cap as f64 * 100.0);
}

#[test]
fn test_content_core_page_realistic_ratio() {
    let storage = ContentCoreStorageRow {
        published_at: 1_700_000_000_000_000i64, created_at: 1_700_000_000_000_000i64,
        modified_at: 1_700_000_000_000_000i64, document_id: 42, author_entity_id: 7,
        status: 1, is_readable: true, is_commentable: true, is_visible_comments: true,
    };
    let varlena = ContentCoreVarlenOwned {
        headline:    Some("Introduction à l'architecture DOD".to_string()),
        description: Some("Système de projection réactif AOT.".to_string()),
        ..Default::default()
    };
    let mut buf = String::new();
    ContentCoreProjection::render_page(&storage, &varlena, &mut buf);
    let ratio = buf.len() as f64 / CONTENT_CORE_PAGE_TOTAL_CAP as f64 * 100.0;
    assert!(ratio > 3.0 && ratio < 98.0,
        "Ratio pathologique : {ratio:.0}% — vérifier DYNAMIC_CAP.");
}

#[test]
fn test_static_partials_non_empty() {
    assert!(!static_partials::PARTIALS_NAV.is_empty());
    assert!(!static_partials::PARTIALS_FOOTER.is_empty());
}
```

---

## 12. Garde-fous et politique des assets

### 12.1 Vérification de conformité AOT

Un fichier `.marius` est conforme si `parse_page_template()` réussit sans erreur. Les erreurs sont des violations des invariants fondateurs — elles ne sont pas des avertissements récupérables.

### 12.2 Politique champs TEXT sans contrainte

Champ `TEXT` sans `CHECK (length(col) ≤ N)` et sans annotation `marius:pre_escaped` → `panic!` dans `build.rs` si le champ est référencé dans une spécification. Identique au comportement fragment.

### 12.3 Séparation fragment / page

`render()` (fragment) et `render_page()` (page complète) coexistent dans le même `impl`, générés indépendamment. `render()` sert aux mises à jour HTMX partielles ; `render_page()` sert à la génération initiale du fichier sur disque.

### 12.4 Read Path inchangé

`sendfile(2)` sur le fichier HTML pré-rendu. Existence = accès ; absence = 404. `render_page()` ne modifie pas ce contrat.

### 12.5 Politique assets statiques

> Les seuils ci-dessous sont des **valeurs par défaut configurables** — non des invariants architecturaux. Seule la règle de séparation (`static_partials` vs service externe) est un invariant fondateur.

| Taille    | Stratégie                                              |
| --------- | ------------------------------------------------------ |
| < 32 KB   | `static_partials` — bake-in dans `.rodata`, par défaut |
| 32–200 KB | `cargo:warning!` — évaluer au cas par cas              |
| > 200 KB  | Service statique externe obligatoire (nginx, CDN)      |

Seuil configurable : `FRAGMENT_FORGE_STATIC_WARN_BYTES`. Les valeurs 32 KB et 200 KB sont des défauts réglables via cette variable — elles ne doivent pas être traitées comme des constantes du système.

---

## 13. Glossaire

| Terme                              | Définition                                                                                                                                                                                                                                                                     |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Spécification de projection**    | Fichier `.marius` décrivant la structure HTML cible et les positions des champs. Compilé intégralement au build-time — n'existe plus au runtime.                                                                                                                               |
| **Opérateur de projection**        | `{{ field }}` et `{% if bool %}` — agissent sur les données du tuple au runtime.                                                                                                                                                                                               |
| **Opérateur de composition**       | `{% extends %}`, `{% block %}`, `{% static %}` — résolus au build-time par la forge ; aucune trace au runtime.                                                                                                                                                                 |
| **Compilateur de projections AOT** | Ce que la forge est réellement. Produit du code Rust natif depuis des spécifications `.marius` et un schéma PostgreSQL.                                                                                                                                                        |
| **Projection déterministe**        | Mapping `(Record, VarlenOwned) → séquence d'octets HTML` sans allocation, sans logique métier, sans interprétation.                                                                                                                                                            |
| `FlatPageToken`                    | Token post-merge : `Static`, `Field`, `IfBool`, `EndIf`, `StaticInclude`.                                                                                                                                                                                                      |
| `StaticInclude`                    | Token portant `original_path`, `rel_from_manifest`, `len`. Génère `buf.push_str(static_partials::X)`.                                                                                                                                                                          |
| `static_partials`                  | Module généré : une constante `&'static str` par fichier statique unique. Garantie structurelle de déduplication `.rodata`.                                                                                                                                                    |
| `PAGE_STATIC_CAP`                  | Expression Rust compile-time : `static_partials::X.len() + … + N`. Immune aux conversions CRLF/LF.                                                                                                                                                                             |
| `PAGE_DYNAMIC_CAP`                 | Somme des `max_display_width` / `max_escaped_len` de tous les `Field` tokens, avec multiplicité.                                                                                                                                                                               |
| `PAGE_TOTAL_CAP`                   | `PAGE_STATIC_CAP + PAGE_DYNAMIC_CAP`. Borne supérieure sans marge arbitraire — calculée analytiquement, atteignable au pire cas. Invariant fondateur.                                                                                                                          |
| `PageRenderOutput`                 | Struct : `page_sc_build_time`, `page_dc`, `static_cap_expr`, `body`.                                                                                                                                                                                                           |
| `FieldInfo`                        | Type interne `{ Fixed(usize), Varlena(usize) }` — lookup O(1) dans `build_field_index()`.                                                                                                                                                                                      |
| `render_page()`                    | Séquence linéaire générée : `push_str` / `static_partials::X` / `write_fmt` / `marius_html_escape` / `if`.                                                                                                                                                                     |
| `PageProjection`                   | Sous-trait optionnel de `Projection` : `PAGE_TOTAL_CAP` + `render_page()`.                                                                                                                                                                                                     |
| `FRAGMENT_FORGE_STATIC_WARN_BYTES` | Seuil de warning pour assets volumineux (défaut : 32 768). Valeur configurable — non invariant (voir §12.5).                                                                                                                                                                   |
| **`marius:pre_escaped`**           | Annotation DDL signalant qu'un champ `TEXT` est garanti exempt de caractères HTML spéciaux à l'écriture (Write Path). Exempte le champ de la contrainte `CHECK (length ≤ N)` dans `build.rs`. Comportement de rendu identique au mode fragment.                                |
| `marius_html_escape`               | Fonction de la crate `marius-core` (hors périmètre de cette spécification). Échappe les caractères HTML (`&`, `<`, `>`, `"`, `'`) en écriture directe vers un `&mut String`, sans allocation intermédiaire.                                                                    |
| **Tuple plat**                     | `(StorageRow #[repr(C)], VarlenOwned)` — struct de largeur connue à la compilation, sans indirection de structure (pas de sous-structures dynamiques, pas de pointeurs vers d'autres entités). Le payload varlena est en heap. Fondement de l'entité unique par spécification. |
| **Dénormalisation contrôlée**      | Duplication assumée de données de lecture dans le schéma PostgreSQL. Compromis : stockage contre latence de rendu nulle.                                                                                                                                                       |

---

## 14. Lifecycle des buffers Rayon

### 14.1 Cycle par batch

```
Début batch :  String::new() créé par le seed map_with — une fois par thread.
Record 1 :     buf.reserve(PAGE_TOTAL_CAP) → malloc(PAGE_TOTAL_CAP).
Records 2..N : buf.clear() → len=0, capacity inchangée → buf.reserve() = no-op.
Fin batch :    seed droppé → free(PAGE_TOTAL_CAP) par thread.
```

Coût par batch : O(T) malloc + O(T) free. Les allocateurs modernes (glibc, jemalloc, mimalloc) maintiennent des bins par classe de taille : le bloc libéré est retenu en liste libre et réattribué sans syscall au batch suivant. Fragmentation pour des allocations de taille fixe répétées : négligeable en pratique.

### 14.2 Distinction benchmark / production

`render_batch_page_pure` (benchmarks) retourne `Vec<String>` → `buf.clone()` par enregistrement. Le Dispatcher de production écrit `buf.as_bytes()` directement via `artifact_path()` — aucun clone, coût O(T) uniquement.

> **Optimisation Shell haute fréquence.** Pour des taux de `pg_notify` élevés ou un uptime long, un buffer TLS persistant entre batches élimine l'O(T) malloc/free par batch. Ce pattern est un choix d'implémentation du Shell — les invariants Core sont inchangés. Voir **Appendice A** pour la spécification de `render_batch_page_persistent`.

---

## 15. Tests unitaires forge

### 15.1 `rust_raw_str_lit`

```rust
#[cfg(test)]
mod tests_raw_str {
    use super::rust_raw_str_lit;
    fn check(s: &str) {
        let lit = rust_raw_str_lit(s);
        let level = lit.trim_start_matches('r').chars().take_while(|&c| c == '#').count();
        let h = "#".repeat(level);
        let inner = &lit[format!("r{h}\"").len()..lit.len() - format!("\"{h}").len()];
        assert_eq!(inner, s, "contenu altéré : {s:?}");
    }
    #[test] fn empty()              { check(""); }
    #[test] fn simple_html()        { check("<h1>Titre</h1>"); }
    #[test] fn contains_quote()     { check(r#"<a href="/">lien</a>"#); }
    #[test] fn quote_then_hash()    { check(r#"data="#val"#); }
    #[test] fn quote_ten_hashes()   { check("fin\"##########"); }
    #[test] fn multiline()          { check("<nav>\n  <a>lien</a>\n</nav>"); }
    #[test] fn unicode_dangerous()  { check("«»&<>\"'日本語"); }
    #[test] fn alternating()        { check("\"#\"##\"###"); }
}
```

### 15.2 `static_const_ident`

```rust
#[cfg(test)]
mod tests_ident {
    use super::static_const_ident;
    #[test] fn nav()    { assert_eq!(static_const_ident("templates/partials/nav.html"),    "PARTIALS_NAV"); }
    #[test] fn footer() { assert_eq!(static_const_ident("templates/partials/footer.html"), "PARTIALS_FOOTER"); }
    #[test] fn hero()   { assert_eq!(static_const_ident("templates/sections/hero.html"),   "SECTIONS_HERO"); }
    #[test] fn deep()   { assert_eq!(static_const_ident("templates/a/b/c.html"),           "A_B_C"); }
}
```

### 15.3 `relative_path_for_include_str`

```rust
#[cfg(test)]
mod tests_rel_path {
    use super::relative_path_for_include_str;
    use std::path::PathBuf;
    #[test] fn sibling_subdir() {
        let m = PathBuf::from("/workspace/crates/core/schema");
        let t = PathBuf::from("/workspace/templates/partials/nav.html");
        assert_eq!(relative_path_for_include_str(&m, &t), "/../../../templates/partials/nav.html");
    }
    #[test] fn no_backslash() {
        let m = PathBuf::from("/a/b"); let t = PathBuf::from("/a/c/f.html");
        assert!(!relative_path_for_include_str(&m, &t).contains('\\'));
    }
    #[test] fn starts_with_slash() {
        let m = PathBuf::from("/a/b"); let t = PathBuf::from("/a/c/f.html");
        assert!(relative_path_for_include_str(&m, &t).starts_with('/'));
    }
}
```

### 15.4 `resolve_and_measure` — capacité

```rust
#[cfg(test)]
mod tests_capacity {
    use super::*;

    #[test] fn field_counted_per_occurrence() {
        let mut flat = vec![
            FlatPageToken::Field { entity: "e", field: "title" },
            FlatPageToken::Field { entity: "e", field: "title" },
        ];
        let varlena = vec![VarlenField { name: "title".into(), max_len: 100,
            max_escaped_len_override: None, pre_escaped: false, nullable: true }];
        let schema = SchemaIndex { fixed: &[], varlena: &varlena };

        let metrics = resolve_and_measure(
            &mut flat, &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
        ).expect("résolution attendue en succès");

        // Deux occurrences du même champ → comptées deux fois (pas de déduplication
        // de la contribution à la capacité — seule la déclaration de _ref est dédupliquée
        // côté génération de code, voir generate_aot_snippet).
        assert_eq!(metrics.total_dynamic_bytes, 2 * 100 * 6);  // facteur HTML_ESCAPE_FACTOR = 6
        assert_eq!(metrics.total_static_bytes, 0);
    }

    #[test] fn static_include_len_counted() {
        let mut flat = vec![
            FlatPageToken::StaticInclude {
                original_path:      "templates/partials/nav.html",
                rel_from_manifest:  "templates/partials/nav.html",
                len: 0,  // valeur initiale sans importance — résolue par get_file_size
            }
        ];
        let schema = SchemaIndex { fixed: &[], varlena: &[] };

        let metrics = resolve_and_measure(
            &mut flat, &schema,
            |path| if path == "templates/partials/nav.html" { Ok(50) } else { Err("chemin inattendu".into()) },
        ).expect("résolution attendue en succès");

        assert_eq!(metrics.total_static_bytes, 50);
        assert_eq!(metrics.total_dynamic_bytes, 0);
        assert_eq!(metrics.include_count, 1);

        // Le token est muté en place : `len` reflète désormais la taille résolue.
        match &flat[0] {
            FlatPageToken::StaticInclude { len, .. } => assert_eq!(*len, 50),
            _ => unreachable!(),
        }
    }
}
```

---

## Appendice A — Choix d'implémentation Shell : buffer TLS persistant (`render_batch_page_persistent`)

> **Statut : choix d'implémentation du Shell — hors invariants Core.**
> Ce pattern ne fait pas partie de la surface normative de `fragment-forge`. Les invariants architecturaux définis aux §0.5, §5.1, et §7 sont inchangés. Une ADR dédiée est requise avant adoption en production.

### A.1 Motivation

Le cycle par batch standard (§14.1) alloue et libère O(T) blocs de `PAGE_TOTAL_CAP` bytes à chaque batch Rayon. Pour des taux de `pg_notify` élevés (indicatif : > 100/s) sur un long uptime, les allocateurs modernes absorbent ce coût via des bins de taille fixe, mais le profil d'allocation reste visible dans les flamegraphs. Le pattern TLS élimine ce résidu en rendant les buffers persistants entre batches.

### A.2 Signature

```rust
pub fn render_batch_page_persistent<P: PageProjection, F>(
    records: Vec<(P::Record, P::VarlenOwned)>,
    write_fn: F,
)
where
    F: Fn(&P::Record, &[u8]) + Sync,
{
    use rayon::prelude::*;
    thread_local! {
        static PAGE_BUF: std::cell::RefCell<String> = RefCell::new(String::new());
    }
    records.into_par_iter().for_each(|(record, varlena)| {
        PAGE_BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            buf.clear();
            P::render_page(&record, &varlena, &mut *buf);
            write_fn(&record, buf.as_bytes());
        });
    });
}
```

### A.3 Propriétés

- `RefCell::borrow_mut()` est sans conflit : Rayon garantit une closure par thread physique — pas de contention sur le `RefCell`.
- Zéro allocation après le premier enregistrement traité par thread.
- `write_fn: F` est le callback d'écriture du Shell (ex : écriture via `artifact_path()`) — il remplace le retour `Vec<String>` de `render_batch_page_pure`.
- Différence de signature par rapport à `render_batch_page_pure` : le paramètre `write_fn` est ajouté ; la valeur de retour passe de `Vec<String>` à `()`.

### A.4 Préconditions d'adoption

Avant d'adopter ce pattern, une ADR doit traiter :

- Le comportement en cas de `panic!` dans `write_fn` (le buffer TLS conserve son état — la capacité, pas les données) ;
- La politique de `drop` des buffers TLS en fin de process (`thread_local!` ne garantit pas l'ordre de destruction) ;
- La compatibilité avec les harness de benchmark existants (§10) qui utilisent `render_batch_page_pure`.
