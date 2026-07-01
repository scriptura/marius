# Guide `fragment-forge` — Écrire des spécifications `.marius`

> Compilateur de projections HTML AOT du projet Marius.
> Spécification de référence : `specification-marius-compilateur-projections-html.md` v1.1.

---

## 0. Statut de ce document

Ce guide a deux parties de statut différent :

| Partie | Contenu | Statut |
| --- | --- | --- |
| **Partie 1** | Mode fragment : `{{ }}`, `{% if %}`, `{% include %}` | **Implémenté** — pipeline réellement câblé dans `crates/core/schema/build.rs` |
| **Partie 2** | Mode page : `{% extends %}`, `{% block %}`, `{% static %}` | **Spécifié (v1.1), non implémenté** — décrit ici pour préparer l'écriture des futures spécifications |

Ne confondez pas les deux : tout ce qui est documenté en Partie 1 compile aujourd'hui avec `cargo build`. La Partie 2 décrit un comportement cible — si vous écrivez `{% extends %}` dans un fichier `.marius` aujourd'hui, le parseur actuel le rejette (`InvalidBlockSequence`, mot-clé `extends` non reconnu par l'automate de blocs en vigueur).

---

## 1. Introduction — le contrat de lecture

Avant d'écrire la première ligne de `.marius`, trois principes gouvernent tout le reste. Si vous ne retenez qu'une chose de ce document, retenez celle-ci : **`fragment-forge` n'est pas un moteur de template**. C'est un compilateur. La syntaxe vous trompera si vous ne lisez pas cette section.

### 1.1 Tool piggybacking

Marius utilise la syntaxe Jinja/Twig (`{{ field }}`, `{% if %}`) sans utiliser de moteur Jinja. C'est un détournement délibéré de l'écosystème existant :

- coloration syntaxique automatique dans tout IDE qui reconnaît `.html.j2`/`.twig` ;
- formatters et linters disponibles gratuitement, sans outillage maison ;
- un développeur qui connaît Jinja sait lire un fichier `.marius` au premier coup d'œil.

Ce que ce détournement **ne** signifie **pas** : que les constructions Jinja usuelles fonctionnent. `fragment-forge` reconnaît un sous-ensemble volontairement restreint de cette syntaxe et rejette tout le reste à la compilation, pas à l'exécution.

### 1.2 Principe de moindre surprise (POLA)

Un réflexe Jinja légitime — `{% for product in products %}`, `{% if user.role == "admin" %}` — est rejeté par le compilateur AOT. Ce n'est pas un bug, ni une limitation provisoire : c'est une conséquence directe de la gestion mémoire du moteur.

`render()` est une fonction qui écrit dans un buffer **pré-alloué à une taille calculée au build-time** (`{NAME}_TOTAL_CAP`). Une boucle de longueur non bornée (`{% for %}`) rend ce calcul impossible — la taille de sortie dépendrait du nombre d'éléments en base, connu seulement à l'exécution. D'où l'interdiction structurelle, pas stylistique.

Attendez-vous à des erreurs de compilation, pas des comportements silencieux. C'est le compromis : moins de syntaxe disponible, en échange d'une garantie zéro-réallocation vérifiée par test (`test_{name}_no_realloc`).

### 1.3 Souveraineté du schéma PostgreSQL

Une version antérieure de l'architecture (v0.1, dépréciée — voir `static-view-driven-data-pipeline.md`) faisait dicter le layout binaire de la base par le template HTML. Ce modèle a été abandonné : il inversait la hiérarchie logique d'un système d'information et rendait le moindre changement de vue destructeur sur le schéma.

Le modèle actuel inverse cette dépendance : **c'est PostgreSQL qui régit la structure**. Le template ne fait que sélectionner, parmi les champs déjà exposés par le schéma (`FieldSpec`, `VarlenField`), lesquels apparaissent dans le HTML généré. Conséquence pratique pour vous : un `.marius` ne peut référencer qu'un champ qui existe déjà dans la table ou la jointure varlena associée. Si le champ n'existe pas, on l'ajoute côté SQL — jamais en contournant le compilateur.

---

## 2. Le langage `.marius` — ce qui compile aujourd'hui

### 2.1 Syntaxe autorisée

| Construction | Effet | Génère |
| --- | --- | --- |
| `{{ entity.field }}` | Interpolation d'un champ | `write_fmt` (fixed-length) ou `marius_html_escape` (varlena) |
| `{% if entity.field %} … {% endif %}` | Inclusion conditionnelle | `if record.{field} != 0 { … }` |
| `{% include "chemin" %}` | Inclusion d'un fragment statique résolu au build | `buf.push_str(include_str!(...))` |
| texte brut | HTML verbatim | `buf.push_str("...")` |

Trois constructions, pas plus. Tout le reste est une erreur de compilation.

### 2.2 La convention d'entité — et son piège contre-intuitif

La grammaire impose la forme `entity.field` (`{{ record.title }}`, pas `{{ title }}`). C'est un héritage du modèle relationnel — chaque template est lié à exactement une entité — mais **dans l'implémentation actuelle, le nom d'entité n'est pas validé contre le schéma**. Seul `field` est recherché dans `SchemaIndex` (`find_fixed` puis `find_varlena`). `entity` est syntaxiquement obligatoire, sémantiquement décoratif, aujourd'hui.

Concrètement : `{{ record.description }}` et `{{ nimporte_quoi.description }}` compilent à l'identique tant que `description` existe dans le schéma. Ce n'est pas une autorisation à écrire n'importe quoi — c'est un point de vigilance : le compilateur ne vous protège pas (encore) contre un nom d'entité incohérent. La convention en vigueur dans les templates existants :

- `record` pour tout champ fixed-length (`FieldSpec`) ;
- `record` également pour les champs varlena dans les templates actuels (voir `content/core.marius` — `record.description` est en réalité un varlena, pas un fixed-length ; le compilateur tranche sur la présence du nom de champ, pas sur le préfixe).

> La spécification v1.1 (§1.2) prévoit une erreur `UnknownEntity` avec message de migration explicite (voir §4 de ce guide pour le mode page, où cette validation existe déjà dans la conception). En mode fragment, cette garde n'est pas encore câblée — gardez vos noms d'entité cohérents par discipline d'équipe, pas par confiance dans le compilateur.

### 2.3 Ce qui est banni, et pourquoi

| Interdit | Raison structurelle |
| --- | --- |
| `{% for … %}` | Sortie de longueur non bornée → rend `{NAME}_TOTAL_CAP` incalculable au build-time |
| Imbrication `{% if %}` dans `{% if %}` | La FSM de validation (`validate_ast`) est un automate à un seul niveau d'état (`current_open_if: Option<(entity, field)>`) — une imbrication ouvre une erreur `NestedIfNotSupported`, l'état reste sur le bloc externe |
| Mots-clés relationnels (`join`, `where`, `filter`, `group`) | Appartiennent au Write Path PostgreSQL, jamais au Read Path |
| `{% if %}` sur un champ non booléen | Romprait la largeur de struct statiquement connue (`StorageRow #[repr(C)]`) |

Toute séquence de bloc non reconnue (mot-clé inconnu après `{%`) échoue avec `PageParseError::InvalidBlockSequence` — c'est l'erreur que vous obtiendrez aujourd'hui si vous tentez `{% extends %}` ou `{% for %}`.

### 2.4 Champs varlena — disjoncteur Hot / Cold / Erreur (ADR-007)

Un champ `TEXT` sans borne exploitable (`VARCHAR(N)` ou `CHECK (length(col) <= N)`) n'est **pas** automatiquement une erreur. La règle :

- **non référencé** dans le template → champ "Cold", invisible, aucune erreur ;
- **référencé** et borné → "Hot", sa capacité (`max_len × 6`, facteur d'échappement HTML pire cas) entre dans `total_dynamic_bytes` ;
- **référencé** et non borné → erreur de compilation (`ResolverError::UnboundedField`).

Aucun fallback arbitraire n'est substitué à l'absence de borne. Si vous référencez un `TEXT` libre dans votre template, ajoutez une contrainte `CHECK` côté SQL — c'est la seule issue.

Un champ annoté `marius:pre_escaped` (commentaire SQL `COMMENT ON COLUMN ... IS 'marius:pre_escaped'`) bénéficie d'un facteur ×1 au lieu de ×6 : vous certifiez que son contenu est déjà sanitisé (slug, titre normalisé).

### 2.5 Exemples réels

`templates/content/core.marius` :

```jinja
<article class="content-core" id="{{ record.document_id }}">
  <h1>{{ record.headline }}</h1>
  <h2>{{ record.alternative_headline }}</h2>
  {% if record.is_readable %}
  <p class="body">{{ record.description }}</p>
  {{ record.description }}
  {% endif %}
</article>
```

Notez `{{ record.description }}` référencé deux fois : chaque occurrence est comptée séparément dans `total_dynamic_bytes` (§2.4, principe de multiplicité — un champ référencé N fois est mesuré N fois, jamais déduppliqué).

`templates/commerce/product_core.marius` :

```jinja
<article class="product-core" id="{{ record.id }}">
  <p>Stock disponible : {{ record.stock }}</p>
</article>
```

Aucun bloc conditionnel, aucun varlena ici — exemple minimal valide.

### 2.6 Messages d'erreur que vous rencontrerez

| Source | Erreur | Déclencheur |
| --- | --- | --- |
| `parse_tokens` | `UnexpectedToken { expected, got }` | Token syntaxiquement hors séquence (`{{` non suivi de `entity.field}}`) |
| `parse_tokens` | `InvalidBlockSequence` | Mot-clé de bloc inconnu (`for`, `extends`, `block`…) |
| `validate_ast` | `NestedIfNotSupported` | `{% if %}` ouvert dans un `{% if %}` déjà ouvert |
| `validate_ast` | `UnexpectedEndIf` | `{% endif %}` sans `{% if %}` correspondant |
| `validate_ast` | `UnclosedIf` | Fin de fichier avec un `{% if %}` resté ouvert |
| `resolve_and_measure` | `UnknownField` | `field` absent du schéma (ni fixed, ni varlena) |
| `resolve_and_measure` | `UnboundedField` | Varlena référencé sans `max_len` connu (§2.4) |
| `resolve_and_measure` | `IoError` | `{% include %}` pointant vers un fichier introuvable |

Toutes les erreurs de `resolve_and_measure` sont accumulées en une seule passe (stratégie fail-slow) : un template référençant trois champs inconnus remonte trois erreurs en un seul `cargo build`, pas une à la fois.

---

## 3. Le moteur — ce que `fragment-forge` n'est pas

### 3.1 Démystification

`fragment-forge` n'a pas d'évaluateur, pas de boucle d'interprétation, pas de représentation intermédiaire conservée au runtime. C'est une bibliothèque de pure transformation de texte, appelée depuis `crates/core/schema/build.rs`, **uniquement pendant `cargo build`**. Son unique sortie observable est un fichier Rust, `generated_schema.rs`, écrit dans `OUT_DIR` et inclus via `include!()` dans `lib.rs`. À l'exécution de l'application, `fragment-forge` n'existe pas : le binaire final ne contient que les `push_str`/`write_fmt`/`marius_html_escape` qu'il a émis.

### 3.2 Le pipeline réel

```
scan(src)              → Iterator<RawSpan>        (tokenisation lexicale, zéro alloc heap)
parse_tokens(spans)     → Vec<FlatPageToken>        (syntaxe, fail-fast)
validate_ast(&tokens)   → Result<(), Vec<SemanticError>>   (équilibre if/endif, FSM 1 niveau)
resolve_and_measure(…)  → Result<TemplateMetrics, Vec<ResolverError>>
                          (résolution I/O des include + calcul de capacité, en une seule passe)
generate_aot_snippet(…) → String                    (transpilation vers Rust natif)
```

Ce pipeline est appelé par `resolve_template()` dans `crates/core/schema/build.rs`, pour chaque table déclarée dans le registry (`fetch_component_list`). Toute l'I/O disque (lecture du `.marius`) vit dans `build.rs` — `fragment-forge` lui-même ne touche jamais le système de fichiers, à l'exception de la résolution des tailles d'`{% include %}` via une closure injectée (`get_file_size`), ce qui le rend testable sans disque réel.

### 3.3 Ce qui sort de la forge

Pour chaque table, `generated_schema.rs` contient :

- `{Name}Row` : transport `sqlx::FromRow`, éphémère.
- `{Name}StorageRow` : `#[repr(C)]`, stockage contigu, types fixed-length uniquement.
- `{Name}VarlenOwned` : `Option<String>` par champ varlena, `Send + 'static`.
- `impl Projection` : `fetch_batch()`, `render()`.
- Constantes : `{NAME}_STATIC_CAP`, `{NAME}_DYNAMIC_CAP`, `{NAME}_TOTAL_CAP`.

`{NAME}_TOTAL_CAP` est l'unique borne utilisée dans le hot path : `buf.reserve({NAME}_TOTAL_CAP)` est la première instruction de `render()`.

### 3.4 L'invariant no-realloc

```rust
let mut buf = String::with_capacity(CONTENT_CORE_TOTAL_CAP);
ContentCoreProjection::render(&storage, &varlena, &mut buf);
assert_eq!(buf.capacity(), CONTENT_CORE_TOTAL_CAP); // doit tenir, toujours
```

C'est le contrat que `fragment-forge` vous garantit en échange des restrictions du §2.3 : si ce test échoue, ce n'est jamais une marge insuffisante à corriger à la main — c'est `max_display_width()` ou `max_escaped_len()` qui sous-estime un type. La capacité n'a délibérément aucune marge arbitraire (§0.5 de la spécification) : toute marge masquerait une sous-estimation réelle.

---

## 4. Composition de pages — `{% extends %}`, `{% block %}`, `{% static %}`

> **Statut : spécifié (v1.1), non implémenté.** Cette partie décrit la cible de conception (`specification-marius-compilateur-projections-html.md` §1–9) pour que vous puissiez anticiper l'écriture de vos futures spécifications de page. Rien ici ne compile avec la version actuelle de `fragment-forge`.

### 4.1 Toujours du piggybacking — mais sur l'héritage de templates

Le mode fragment (Partie 1) produit un fragment HTML par enregistrement — utile pour les mises à jour partielles HTMX, pas pour une page complète (en-tête, navigation, contenu, pied de page). Plutôt qu'inventer une syntaxe de composition propriétaire, la spécification réutilise — toujours par piggybacking — le modèle d'héritage de templates Jinja/Twig (`extends`/`block`), pour les mêmes raisons qu'au §1.1 : tooling IDE gratuit, intuition immédiate pour quiconque a déjà écrit du Django ou du Twig.

Le principe ne change pas : ces opérateurs n'ont **aucune existence au runtime**. Ce sont des **opérateurs de composition**, résolus une fois pour toutes au build-time — à distinguer des **opérateurs de projection** (`{{ }}`, `{% if %}`) qui, eux, génèrent du code exécuté à chaque enregistrement.

### 4.2 Discriminant fragment / page

Une spécification `.marius` est en **mode page** si et seulement si sa première construction non-whitespace est `{% extends "chemin" %}`. BOM, lignes vides et whitespace sont tolérés avant. Toute autre première construction → mode fragment, comportement identique à la Partie 1, aucune `PageProjection` générée.

Si `{% extends %}` est présent mais pas en première position : `PageParseError::ExtendsNotFirst`.

### 4.3 `{% extends "path" %}`

Déclare la spécification parente. Déclenche la fusion des deux AST au build-time. Une seule occurrence, en tête de fichier — pas d'héritage multi-niveaux en v1 (la spécification ne prévoit qu'un parent direct, pas de chaîne `enfant → parent → grand-parent`).

```jinja
{# templates/content/core.marius #}
{% extends "templates/base.marius" %}

{% block title %}{{ content_core.headline }}{% endblock %}

{% block content %}
<article class="content-core">
  ...
</article>
{% endblock %}
```

### 4.4 `{% block name %} … {% endblock %}`

Déclare, dans le parent, un point de substitution avec une valeur par défaut ; dans l'enfant, la valeur de remplacement. La résolution est une **substitution textuelle pure** au moment de la fusion — pas d'évaluation, pas de portée, pas d'héritage de variables entre blocs.

```jinja
{# templates/base.marius — parent #}
<!DOCTYPE html>
<html lang="fr">
<head>
  <title>{% block title %}Marius{% endblock %}</title>
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

Contrainte AOT à respecter : **un bloc de l'enfant absent du parent est une erreur** (`OrphanBlock`) — pas un avertissement silencieusement ignoré. Et, comme pour `{% if %}` en mode fragment, **aucune imbrication `{% block %}`/`{% if %}` n'est supportée en v1** : la pile d'état nécessaire est explicitement hors périmètre.

### 4.5 `{% static "path" %}`

Inclut un fichier d'octets HTML statiques, lu au build-time, inliné comme `&'static str` dans un module généré `static_partials`. Distinct de `{% include %}` du mode fragment (§2.1) par un détail qui compte en mémoire : si plusieurs pages référencent le même fichier `{% static %}`, elles partagent **la même constante** — déduplication structurelle en `.rodata`, pas une copie par page.

Politique de taille (configurable via `FRAGMENT_FORGE_STATIC_WARN_BYTES`, défaut 32 768) :

| Taille | Stratégie |
| --- | --- |
| < 32 Ko | `static_partials`, par défaut |
| 32–200 Ko | `cargo:warning!`, à évaluer au cas par cas |
| > 200 Ko | Service statique externe obligatoire (nginx, CDN) — pas une option en v1 pour ce volume |

### 4.6 Algorithme de fusion — ce qui se passe à `cargo build`

```
1. Lire child.marius     → extends_path + child_blocks (par nom de bloc)
2. Lire base.marius      → tokeniser
3. Fusionner : chaque bloc du parent est remplacé par le bloc enfant de même nom,
   sinon conserve la valeur par défaut du parent.
   → erreur si un bloc de l'enfant est orphelin (absent du parent)
4. Résoudre chaque {% static %} : taille réelle, chemin relatif, cargo:rerun-if-changed
5. Validation sémantique : entité, champs, type bool des conditions, absence de {% for %},
   absence de mot-clé relationnel, absence d'imbrication
→ Vec<FlatPageToken> : uniquement Static | Field | IfBool | EndIf | StaticInclude
```

Le résultat de la fusion est un AST **plat**, du même type `FlatPageToken` que le mode fragment. Conséquence directe : tout ce que vous avez appris en Partie 1 sur les contraintes de `{{ }}`/`{% if %}` (entité unique, types booléens, pas de boucle) s'applique identiquement après fusion — la composition de page n'assouplit aucune règle du mode fragment, elle compose des fragments qui doivent chacun déjà s'y conformer.

`PAGE_TOTAL_CAP` est calculé sur cet AST fusionné selon la même formule qu'en §3.4 (`page_sc + page_dc`), en une seule passe fusionnée avec la résolution sémantique — pas deux parcours distincts, pour la localité de cache CPU sur l'itération de l'AST.

### 4.7 Erreurs spécifiques au mode page

| Erreur | Déclencheur |
| --- | --- |
| `ExtendsNotFound` | Chemin de `{% extends %}` introuvable |
| `ExtendsNotFirst` | `{% extends %}` pas en première position non-whitespace |
| `StaticFileNotFound` | Chemin de `{% static %}` introuvable |
| `OrphanBlock` | Bloc enfant sans correspondant dans le parent |
| `UnknownEntity` | Entité référencée ≠ entité de la spécification (voir le message de migration §1.3) |
| `UnknownField` | Champ absent du schéma |
| `NonBoolIfCondition` | `{% if %}` sur un champ non `bool` |
| `ForLoopDetected` | `{% for %}` détecté |
| `RelationalKeyword` | `join`/`where`/`filter`/`group` détecté |
| `NestedBlock` / `NestedIf` | Imbrication détectée |

Contrairement au mode fragment actuel, la spécification de page prévoit explicitement `UnknownEntity` avec message de migration (exemple §1.3) — c'est la garde qui manque aujourd'hui au mode fragment (§2.2).

### 4.8 Au runtime : `render()` et `render_page()` coexistent, jamais l'un n'appelle l'autre

Une fois compilées, les deux fonctions vivent dans le même `impl` :

- `render()` (mode fragment) : mises à jour partielles HTMX.
- `render_page()` (mode page) : génération initiale du document complet, écrit une fois sur disque.

`render_page()` n'appelle **pas** `render()` — chacune est une transpilation indépendante du même type d'AST, pas une composition runtime de l'une par l'autre.

Ce découplage n'est pas accessoire : ADR-008 tranche qu'**aucune page complète n'est jamais stockée comme artefact monolithique pré-composé**. Chaque composant (en-tête, contenu, pied de page) reste stocké et invalidé indépendamment. La composition résolue par `{% extends %}`/`{% block %}` a lieu **à l'écriture** (au moment où l'application matérialise la page sur disque), jamais à la lecture — le contrat de lecture reste un `sendfile(2)` unique sur un fichier déjà composé, jamais une résolution applicative au moment de la requête.

### 4.9 Ce qui ne change pas en passant au mode page

- L'AOT absolu : la vue reste résolue au build-time, jamais interprétée au runtime.
- Zéro allocation intermédiaire : `buf.reserve(PAGE_TOTAL_CAP)` reste la première instruction.
- L'entité unique par spécification (§1.2 de la spécification) : une page composée référence toujours une seule entité porteuse de données dynamiques ; les données d'entités secondaires (auteur, notifications) doivent être pré-agrégées côté PostgreSQL (`GENERATED STORED`, vue matérialisée) — jamais récupérées par un second `fetch_batch` au moment du rendu.

---

## 5. Référence rapide

```
Mode fragment (implémenté) :
  {{ entity.field }}
  {% if entity.bool_field %} … {% endif %}
  {% include "chemin" %}

Mode page (spécifié, non implémenté) :
  {% extends "chemin" %}          ← doit être la première construction du fichier
  {% block name %} … {% endblock %}
  {% static "chemin" %}

Interdit, dans les deux modes :
  {% for … %}
  {% if %} sur un champ non bool
  Imbrication if/if, block/block, block/if
  join / where / filter / group
```

Toute violation est une erreur de compilation (`cargo build` échoue), jamais un comportement silencieux au runtime.
