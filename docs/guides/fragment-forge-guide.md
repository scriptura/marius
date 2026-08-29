# Guide `fragment-forge` — Écrire des spécifications `.marius`

> Compilateur de projections HTML AOT du projet Marius.
> Spécification de référence : `specification-marius-compilateur-projections-html.md` v1.1.
> **Vérifié et corrigé le 7 juillet 2026** (audit croisé + revue de code contre `fragment-forge/src/lib.rs`).

---

## 0. Statut de ce document

Ce guide a deux parties de statut différent :

| Partie | Contenu | Statut |
| --- | --- | --- |
| **Partie 1** | Mode fragment : `{{ }}`, `{% if %}`, `{% include %}` | **Implémenté** — pipeline réellement câblé dans `crates/core/schema/build.rs` |
| **Partie 2** | Mode page : `{% extends %}`, `{% block %}`, `{% static %}`, `{% asset %}`, `{% script %}` | **Implémenté** — `parse_page_block`/`lower`/`link` existent et compilent (`fragment-forge/src/lib.rs`), validé en production sur `content/core.marius` (extends/block) et `templates/base.marius`/`templates/offline/offline.marius` (asset/script, session du 17 juillet 2026) |

Ne confondez pas les deux avec « également testé de bout en bout » : les deux modes compilent aujourd'hui avec `cargo build`. Ce qui reste réellement non câblé, listé précisément en §4.7 et §4.5 : la validation `UnknownEntity` et l'application stricte du seuil de taille `{% static %}` — deux points où la spécification v1.1 va au-delà de ce que le code fait aujourd'hui. Le reste de la Partie 2 (composition, fusion de blocs, `{% static %}` lui-même, `{% asset %}`/`{% script %}` — §4.5bis) est du code qui tourne, pas une intention.

**Hors périmètre de ce document** : ce guide couvre la compilation `.marius` → `render()`. Il ne couvre pas ce qui se passe *après* — comment `render()` est invoqué, à quelle fréquence, ni ce qui invalide le HTML déjà servi. `cargo build` peut réussir intégralement (nouveau `.marius`, nouveau `render()` généré) sans que la moindre requête HTTP n'en voie la couleur : c'est un autre pipeline, un autre cycle d'invalidation, documenté dans `guide-cycle-de-vie-runtime.md`. Un `.marius` correct est une condition nécessaire, jamais suffisante, pour qu'un changement atteigne le navigateur.

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

**Précision structurelle — « côté SQL » signifie la table physique du composant, jamais une vue.** `fetch_varlena_cols` résout les bornes via `pg_constraint` (`CHECK`) — une vue n'en porte jamais, seule une table physique en a. `ref_table` dans `meta.component_varlena_join` est donc, par construction, une table de composant ECS physique (`content.identity`, `identity.person_biography`, etc.), jamais une vue sémantique (`content.v_article`). Les vues sémantiques (ADR-012, `db/tools/`) sont une interface de lecture **parallèle**, destinée à d'autres consommateurs SQL — elles n'ont aucune arête avec ce pipeline. Modifier `content.v_article` n'a **aucun effet** sur ce que `fragment-forge` peut introspecter : seule l'existence physique du champ dans la table jointe compte. Si le champ existe déjà et est correctement borné (`VARCHAR(N)`/`CHECK`), il est immédiatement disponible dans `.marius` sans aucune modification SQL, vue comprise.

---

## 2. Le langage `.marius` — ce qui compile aujourd'hui

### 2.1 Syntaxe autorisée

| Construction | Effet | Génère |
| --- | --- | --- |
| `{{ entity.field }}` | Interpolation d'un champ | `write_fmt` (fixed-length) ou `marius_html_escape` (varlena) |
| `{% if entity.field %} … {% endif %}` | Inclusion conditionnelle | `if record.{field} != 0 { … }` |
| `{% include chemin %}` | Inclusion d'un fragment statique résolu au build | `buf.push_str(include_str!(...))` |
| texte brut | HTML verbatim | `buf.push_str("...")` |

Trois constructions, pas plus. Tout le reste est une erreur de compilation.

**Piège de syntaxe, vérifié contre le scanner** : un chemin (`include`, et en Partie 2 `extends`/`static`) s'écrit **sans guillemets** — `{% include templates/partials/nav.html %}`, jamais `{% include "templates/partials/nav.html" %}`. Le scanner ne connaît aucun token de littéral de chaîne : il découpe tout contenu de bloc en séquences contiguës non-blanc. Des guillemets écrits par réflexe Jinja ne provoquent **pas** une erreur de syntaxe immédiate — ils sont capturés tels quels comme partie du chemin (`"templates/partials/nav.html"`, guillemets inclus), et l'échec n'apparaît qu'en aval, au moment de la résolution du fichier (`ExtendsNotFound`/`StaticFileNotFound`/erreur `include_str!`), avec un chemin visiblement corrompu par les guillemets dans le message — un symptôme trompeur si vous ne savez pas d'où il vient.

### 2.2 La convention d'entité — et son piège contre-intuitif

La grammaire impose la forme `entity.field` (`{{ record.title }}`, pas `{{ title }}`). C'est un héritage du modèle relationnel — chaque template est lié à exactement une entité — mais **dans l'implémentation actuelle, le nom d'entité n'est pas validé contre le schéma**. Seul `field` est recherché dans `SchemaIndex` (`find_fixed` puis `find_varlena`). `entity` est syntaxiquement obligatoire, sémantiquement décoratif, aujourd'hui.

Concrètement : `{{ record.description }}` et `{{ nimporte_quoi.description }}` compilent à l'identique tant que `description` existe dans le schéma. Ce n'est pas une autorisation à écrire n'importe quoi — c'est un point de vigilance : le compilateur ne vous protège pas (encore) contre un nom d'entité incohérent. La convention en vigueur dans les templates existants :

- `record` pour tout champ fixed-length (`FieldSpec`) ;
- `record` également pour les champs varlena dans les templates actuels (voir `content/core.marius` — `record.description` est en réalité un varlena, pas un fixed-length ; le compilateur tranche sur la présence du nom de champ, pas sur le préfixe).

> La spécification v1.1 (§1.2) prévoit une erreur `UnknownEntity` avec message de migration explicite — **non implémentée**, ni en mode fragment ni en mode page (voir §4.7 : aucune trace de cette variante dans le code). En l'état, cette garde n'est câblée nulle part — gardez vos noms d'entité cohérents par discipline d'équipe, pas par confiance dans le compilateur.

### 2.3 Ce qui est banni, et pourquoi

| Interdit | Raison structurelle |
| --- | --- |
| `{% for … %}` | Sortie de longueur non bornée → rend `{NAME}_TOTAL_CAP` incalculable au build-time |
| `{% else %}` | Réflexe Jinja le plus probable après un `{% if %}` — aucune grammaire dédiée : tombe dans le mot-clé inconnu ci-dessous, `InvalidBlockSequence` |
| Imbrication `{% if %}` dans `{% if %}` | La FSM de validation (`validate_ast`) est un automate à un seul niveau d'état (`current_open_if: Option<(entity, field)>`) — une imbrication ouvre une erreur `NestedIfNotSupported`, l'état reste sur le bloc externe |
| Mots-clés relationnels (`join`, `where`, `filter`, `group`) | Appartiennent au Write Path PostgreSQL, jamais au Read Path |
| `{% if %}` sur un champ non booléen | Romprait la largeur de struct statiquement connue (`StorageRow #[repr(C)]`) |

Toute séquence de bloc non reconnue (mot-clé inconnu après `{%`) échoue avec `PageParseError::InvalidBlockSequence` — c'est l'erreur que vous obtiendrez aujourd'hui si vous tentez `{% else %}` ou `{% for %}` : pas une erreur nommée par construction, la même que pour n'importe quel mot-clé absent de la grammaire (`if`/`endif`/`include`, `fragment-forge/src/lib.rs::parse_block`).

### 2.4 Champs varlena — disjoncteur Hot / Cold / Erreur (ADR-007)

Un champ `TEXT` sans borne exploitable (`VARCHAR(N)` ou `CHECK (length(col) <= N)`) n'est **pas** automatiquement une erreur. La règle :

- **non référencé** dans le template → champ "Cold", invisible, aucune erreur ;
- **référencé** et borné → "Hot", sa capacité (`max_len × 6`, facteur d'échappement HTML pire cas) entre dans `total_dynamic_bytes` ;
- **référencé** et non borné → erreur de compilation (`ResolverError::UnboundedField`).

Le facteur ×6 n'est pas arbitraire : c'est la longueur de la plus longue entité HTML parmi les caractères échappés (`"` → `&quot;`, 6 octets pour 1 caractère source) — le pire cas parmi `&amp;` (5), `&lt;`/`&gt;` (4), `&#39;` (5). Dimensionner sur ce pire cas garantit qu'aucune combinaison de caractères ne peut jamais dépasser `max_len × 6`, quelle que soit la donnée réelle en base.

**Deux mécanismes de détection de la borne, ni plus ni moins** (`VarlenField`, `fragment-forge/src/lib.rs`) :

```sql
-- 1. VARCHAR(N) — max_len extrait directement de pg_attribute.atttypmod
CREATE TABLE person (
  biography VARCHAR(2000)
);

-- 2. TEXT + CHECK — build.rs parse la contrainte pour en extraire N
CREATE TABLE person (
  biography TEXT
);
ALTER TABLE person ADD CONSTRAINT person_biography_length_check
  CHECK (length(biography) <= 2000);
```

Il n'existe **pas** de troisième mécanisme de *bornage* par annotation (`COMMENT ... 'marius:max_len=N'`) — seuls `VARCHAR(N)` et `CHECK` (ci-dessus) déterminent `max_len`. Un `TEXT` sans l'un de ces deux mécanismes reste `max_len: None`, quoi que vous mettiez en commentaire SQL. **Distinct** de la politique d'*échappement* (`marius:pre_escaped`/`marius:raw`/`marius:large_content`, ci-dessous) — trois tags `pg_description` bien réels, mais qui ne bornent rien : ils ne font que choisir comment le contenu déjà borné (ou non, pour `large_content`, cf. plus bas) est traité au runtime.

**Forme exacte requise pour un `CHECK` détectable.** La détection repose sur un parsing textuel de la définition de contrainte, pas sur une analyse structurelle de l'arbre SQL — une déviation de forme, sémantiquement équivalente, échoue à être bornée. **Ce n'est pas silencieux** : `cargo:warning` est émis avec le texte brut de la contrainte au moment de l'échec (`DB-Forge [...]: CHECK trouvé mais longueur non parsable : ...`) — visible dans `cargo build -vv`, absent du terminal en `cargo build` par défaut. Pour une détection fiable dès l'écriture du DDL, sans dépendre de ce filet :

- **une seule** contrainte `CHECK` par colonne portant sur sa longueur — une deuxième contrainte introduit une ambiguïté de sélection non garantie par PostgreSQL (`fetch_optional`, sélection arbitraire si plusieurs matchent) ;
- la forme littérale `length(col) <= N`, jamais `N >= length(col)` (opérandes inversés) ni `char_length(col) <= N` mêlé à une autre fonction ;
- `N` un entier littéral nu — `length(col) <= 2000`, jamais une expression (`2*1000`) ni un cast ;
- la contrainte doit être **`VALID`** (comportement par défaut de `ADD CONSTRAINT`) — une contrainte ajoutée via `NOT VALID` n'est pas exclue du parsing mais n'a, par définition PostgreSQL, jamais été vérifiée contre les données existantes : une borne "détectée" sur une contrainte `NOT VALID` n'est pas une garantie réelle sur les données déjà en place.

En cas de doute, préférez `VARCHAR(N)` (§ ci-dessus) : borne extraite de `pg_attribute.atttypmod`, aucun parsing, aucune des fragilités ci-dessus.

Aucun fallback arbitraire n'est substitué à l'absence de borne. Si vous référencez un `TEXT` libre dans votre template, ajoutez une contrainte `CHECK` côté SQL — c'est la seule issue.

**Champ nullable** : un varlena `TEXT` sans `NOT NULL` réserve exactement la même capacité qu'un champ non-nullable — `max_escaped_len()` ne dépend jamais de la nullabilité. En v1, la question est de toute façon pré-tranchée par construction : tout champ varlena issu d'un `LEFT JOIN` est systématiquement `Option<String>` côté Rust (`nullable: true` invariant, v1) ; une valeur `NULL` au runtime réduit simplement les octets effectivement écrits, jamais la capacité pré-allouée au build-time.

Un champ annoté `marius:pre_escaped` (commentaire SQL `COMMENT ON COLUMN ... IS 'marius:pre_escaped'`) bénéficie d'un facteur ×1 au lieu de ×6 : vous certifiez que son contenu est déjà sanitisé (slug, titre normalisé).

⚠️ **`pre_escaped` désactive tout échappement HTML pour ce champ** — aucun filet de rattrapage à l'exécution. Cette annotation certifie l'absence de `<`, `>`, `&`, `"`, `'` dans toute valeur possible de la colonne. Réservez-la aux champs contrôlés par l'application (slugs générés, identifiants techniques, dates formatées) — jamais à une donnée saisie par un utilisateur, même validée côté applicatif : la certification porte sur la colonne SQL elle-même, pas sur un chemin de saisie particulier.

**Correction (23/07/2026) — `pre_escaped` n'est plus le seul tag `pg_description` reconnu.** L'affirmation précédente de ce guide (« il n'existe pas de troisième mécanisme par annotation ») est devenue fausse : trois tags coexistent aujourd'hui, correspondant aux trois variantes de l'enum fermé `EscapePolicy` (`VarlenField::escape_policy`) :

| Tag                     | `EscapePolicy` | Facteur | Échappé au runtime ? | Dans `buf` ? |
| ------------------------ | -------------- | ------- | --------------------- | ------------- |
| *(aucun)*                | `Escaped`      | × 6     | Oui                    | Oui            |
| `marius:pre_escaped`      | `PreEscaped`   | × 1     | Oui (défense en profondeur) | Oui      |
| `marius:raw`              | `Raw`          | × 1     | **Jamais**             | Oui            |
| `marius:large_content`    | `Raw` + segmenté | **0** | **Jamais**             | **Non**        |

**`marius:raw`** : le contenu est du HTML déjà constitué (balisage voulu tel
quel), à l'opposé de `pre_escaped` qui certifie l'*absence* de caractères
spéciaux — `raw` certifie au contraire leur présence *intentionnelle*.
`buf.push_str(s)` direct dans le corps généré, jamais `marius_html_escape`.

**`marius:large_content`** : variante de `raw` pour un champ qui ne doit
**jamais dimensionner le buffer partagé** (`{NAME}_TOTAL_CAP`) — typiquement
un corps d'article pouvant atteindre plusieurs centaines de Ko à quelques Mo.
Contribution nulle à `total_dynamic_bytes` (contrairement à tout autre champ,
cf. principe de multiplicité ci-dessous), **exempté du seuil AOT absolu de
64 Ko** (`introspect.rs`) qui s'applique sinon à tout champ varlena borné, et
traité différemment par le compilateur : le composant génère
`render_segments()` (une séquence de `Segment::Buffered`/`Segment::Borrowed`)
au lieu du simple `render()` décrit dans ce guide jusqu'ici — voir §4.8bis
ci-dessous et `CONTRAT-implementation-projection-segmentee.md` pour le détail
complet. `render()` devient alors un stub qui panique s'il est appelé
directement (il ne l'est jamais en usage normal — `BatchRenderer` appelle
toujours `render_segments()`).

Un seul tag à la fois par colonne — `marius:large_content` implique déjà
`Raw`, ne jamais cumuler les deux.

**Plusieurs champs varlena distincts dans un même template** : autorisé, sans limite de nombre. `total_dynamic_bytes` est la somme de `max_escaped_len()` de chaque champ varlena référencé — `{{ person.biography }}` (borné à 2000) et `{{ person.summary }}` (borné à 500) contribuent chacun indépendamment (12 000 + 3 000 = 15 000 octets), sans interaction entre eux. Un seul d'entre eux non borné suffit à faire échouer tout le template (`UnboundedField`), même si les autres sont correctement contraints. **Exception (23/07/2026)** : un champ `marius:large_content` contribue **0** à cette somme, quelle que soit sa borne réelle — c'est tout l'objet du mécanisme de segmentation (§2.4, tableau ci-dessus) ; il reste néanmoins soumis à la même règle Hot/Cold/Erreur s'il est référencé sans aucune borne connue.

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

**Amplification par composition (Partie 2)** : la fusion `{% extends %}`/`{% block %}` ne réduit jamais ce compte, elle l'agrège. Un champ référencé dans le bloc `title` du parent (`<title>{{ record.headline }}</title>`) **et** dans le bloc `content` de l'enfant (`<h1>{{ record.headline }}</h1>`) est compté deux fois dans `PAGE_TOTAL_CAP` — même principe que §2.5, mais le risque de doublon involontaire grandit avec la composition : le parent et l'enfant sont deux fichiers écrits séparément, sans vue d'ensemble immédiate sur les champs déjà référencés ailleurs dans la page fusionnée.

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

## 4. Composition de pages — `{% extends %}`, `{% block %}`, `{% static %}`, `{% asset %}`, `{% script %}`

> **Statut : spécifié (v1.1), non implémenté.** Cette partie décrit la cible de conception (`specification-marius-compilateur-projections-html.md` §1–9) pour que vous puissiez anticiper l'écriture de vos futures spécifications de page. Rien ici ne compile avec la version actuelle de `fragment-forge`.

### 4.1 Toujours du piggybacking — mais sur l'héritage de templates

Le mode fragment (Partie 1) produit un fragment HTML par enregistrement — utile pour les mises à jour partielles HTMX, pas pour une page complète (en-tête, navigation, contenu, pied de page). Plutôt qu'inventer une syntaxe de composition propriétaire, la spécification réutilise — toujours par piggybacking — le modèle d'héritage de templates Jinja/Twig (`extends`/`block`), pour les mêmes raisons qu'au §1.1 : tooling IDE gratuit, intuition immédiate pour quiconque a déjà écrit du Django ou du Twig.

Le principe ne change pas : ces opérateurs n'ont **aucune existence au runtime**. Ce sont des **opérateurs de composition**, résolus une fois pour toutes au build-time — à distinguer des **opérateurs de projection** (`{{ }}`, `{% if %}`) qui, eux, génèrent du code exécuté à chaque enregistrement.

### 4.2 Discriminant fragment / page

Une spécification `.marius` est en **mode page** si et seulement si sa première construction non-whitespace est `{% extends chemin %}` — **sans guillemets**, voir le piège de syntaxe en §2.1 : il s'applique identiquement ici, `extends` utilisant la même convention non-quotée que `static` (symétrie délibérée, `fragment-forge/src/lib.rs::parse_page_block`, commentaire de la branche `"extends"`). BOM, lignes vides et whitespace sont tolérés avant. Toute autre première construction → mode fragment, comportement identique à la Partie 1, aucune `PageProjection` générée.

Si `{% extends %}` est présent mais pas en première position : `PageParseError::ExtendsNotFirst`.

### 4.3 `{% extends chemin %}`

Déclare la spécification parente. Déclenche la fusion des deux AST au build-time. Une seule occurrence, en tête de fichier — pas d'héritage multi-niveaux en v1 (la spécification ne prévoit qu'un parent direct, pas de chaîne `enfant → parent → grand-parent`).

```jinja
{# templates/content/core.marius #}
{% extends templates/base.marius %}

{% block title %}{{ record.headline }}{% endblock %}

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
{% static templates/partials/nav.html %}
<main>
{% block content %}{% endblock %}
</main>
{% static templates/partials/footer.html %}
</body>
</html>
```

Contrainte AOT à respecter : **un bloc de l'enfant absent du parent est une erreur** (`OrphanBlock`) — pas un avertissement silencieusement ignoré. Et, comme pour `{% if %}` en mode fragment, **aucune imbrication `{% block %}`/`{% if %}` n'est supportée en v1** : la pile d'état nécessaire est explicitement hors périmètre.

**Empreinte mémoire d'un bloc parent surchargé** : quand l'enfant redéfinit un bloc, le contenu par défaut du parent pour ce bloc **n'est jamais projeté** dans l'AST fusionné — `lower()` (`fragment-forge/src/lib.rs`) ne parcourt et n'émet que les tokens de la source retenue par `LinkPlan` (enfant si override, parent sinon), jamais les deux. Ce n'est pas une élision a posteriori par le compilateur Rust (dead-code elimination) : le contenu perdant n'atteint jamais `generate_aot_snippet`, donc n'existe jamais comme `buf.push_str(...)` — zéro octet en `.rodata`, par construction du pipeline de fusion, pas par optimisation.

### 4.5 `{% static chemin %}`

Inclut un fichier d'octets HTML statiques, lu au build-time, inliné comme `&'static str` dans un module généré `static_partials`. Distinct de `{% include %}` du mode fragment (§2.1) par un détail qui compte en mémoire : si plusieurs pages référencent le même fichier `{% static %}`, elles partagent **la même constante** — déduplication structurelle en `.rodata`, pas une copie par page.

Politique de taille visée (`FRAGMENT_FORGE_STATIC_WARN_BYTES`, défaut 32 768) :

| Taille | Stratégie |
| --- | --- |
| < 32 Ko | `static_partials`, par défaut |
| 32–200 Ko | `cargo:warning!`, à évaluer au cas par cas |
| > 200 Ko | Service statique externe obligatoire (nginx, CDN) — pas une option en v1 pour ce volume |

**Non appliqué aujourd'hui** : aucun de ces trois paliers n'est vérifié par le code actuel (`lib.rs`, `build.rs`) — `PageLinkError` ne porte que `ExtendsNotFound`, `OrphanBlock`, `StaticFileNotFound`, aucune variante liée à une taille. Un fichier `{% static %}` de 5 Mo compile aujourd'hui sans avertissement ni erreur. Cette politique est une spécification v1.1, pas un comportement observable — ne vous y fiez pas pour dimensionner un fichier réel tant qu'aucun `cargo:warning` de taille n'est vérifié en pratique.

### 4.5bis `{% asset key %}` et `{% script %} … {% endscript %}` — absents de ce guide jusqu'ici, réels depuis le début

**`{% asset key %}`** — résout `key` (un nom de fichier logique, ex. `main.js`, `utils.svg`, jamais un chemin complet) contre le manifeste d'assets produit par `marius-assets` (`build/{theme}/manifest.toml`, table `[assets."clé"]`). Généré comme `FlatPageToken::AssetRef(key)` par le parseur — même famille de token que `Static`/`Field`, mais résolu au moment de l'écriture (build.rs), pas au moment du parsing.

**`{% script %} … {% endscript %}`** — capture un bloc `<script>...</script>` complet, le "hisse" (déduplication si plusieurs blocs identiques apparaissent, par exemple parce que deux pages différentes chargent le même script), et le réinjecte à l'emplacement du marqueur littéral `<!-- MARIUS_SCRIPTS -->` dans le layout parent (`templates/base.marius`). Le marqueur est cherché comme sous-chaîne exacte, jamais interprété comme une construction `.marius` — un simple point d'ancrage textuel.

**Où vit quoi, même partage des responsabilités qu'au §3.2 pour `get_file_size`** :

- `fragment-forge` (`lib.rs`) définit les token types (`FlatPageToken::AssetRef`, `ScriptStart`, `ScriptEnd`) et les fonctions de manipulation du flux : `hoist_and_dedupe_scripts` (extrait les blocs `{% script %}` du flux, les déduplique), `splice_hoisted_scripts` (les réinsère à l'emplacement du marqueur). `resolve_and_measure` accepte un closure `resolve_asset_len: impl Fn(&str) -> AssetLookup` injecté par l'appelant — `fragment-forge` ne lit jamais lui-même `manifest.toml`, exactement la même discipline de testabilité que pour `{% include %}`/`get_file_size`.
- `crates/core/schema/build.rs` lit `manifest.toml` (`load_asset_manifest`), fournit la closure de résolution (`resolve_asset_lookup`), et c'est lui qui échoue avec un `cargo:error` explicite (pas une variante d'enum nommée dans `fragment-forge`) si une clé est absente du manifeste — diagnostic par distance de Levenshtein sur les clés existantes.

**Piège de syntaxe déjà rencontré** : le mot-clé est `asset`, **singulier** — `{% assets utils.svg %}` (pluriel) n'est reconnu par aucune grammaire et tombe dans le même rejet générique que tout mot-clé de bloc inconnu, `PageValidationError::RelationalKeyword { keyword }` (§2.3/§4.7 — ce n'est pas un mot-clé relationnel, c'est le fourre-tout de `collect_blocks` pour "mot-clé de bloc jamais vu", partagé avec `join`/`where`/`filter`/`group`). Le message ne nomme jamais explicitement "faute de frappe" — seul le mot-clé fautif apparaît, à charge du lecteur de reconnaître l'faute.

**Ordonnancement de résolution — vérifié empiriquement, pas seulement lu dans le code** : les URLs produites par `{% asset %}` reflètent le nom **physique** du fichier écrit sur disque par `marius-assets` (dérivé du stem du fichier source pour `[scripts.components]`, ex. `index.js` → `/scripts/index.HASH.js`), pas nécessairement le nom de la **cible logique** déclarée dans `theme.toml` (ex. `main`). La clé passée à `{% asset %}` doit correspondre à la clé du manifeste (`{cible}.js`), pas au nom de fichier physique si les deux diffèrent — source de confusion réelle en session, résolue en alignant la convention de nommage plutôt qu'en modifiant le mécanisme de résolution.

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
→ Vec<FlatPageToken> : Static | Field | IfBool | EndIf | StaticInclude | AssetRef | ScriptStart | ScriptEnd
  (ScriptStart/ScriptEnd retirés du flux par hoist_and_dedupe_scripts avant l'étape 5 ci-dessus — cf. §4.5bis)
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
| `UnknownField` | Champ absent du schéma |
| `NonBoolIfCondition` | `{% if %}` sur un champ non `bool` |
| `ForLoopDetected` | `{% for %}` détecté |
| `RelationalKeyword` | `join`/`where`/`filter`/`group` détecté — également tout mot-clé de bloc inconnu, y compris une faute de frappe sur `asset`/`script` (§4.5bis) |
| `NestedBlock` / `NestedIf` | Imbrication détectée |
| *(pas de variante nommée)* | `{% asset %}` référençant une clé absente de `manifest.toml` — `cargo:error` émis directement par `build.rs` (`resolve_asset_lookup`), pas par une erreur `fragment-forge` dédiée (§4.5bis) |

**`UnknownEntity` n'existe pas dans le code** — ni en mode page, ni en mode fragment. Ce n'était pas une omission de ce guide, c'était une erreur : la spécification v1.1 la prévoit, mais après fusion (`lower()`), l'AST rejoint le point de convergence documenté au §4.6 — `validate_ast`/`resolve_and_measure`, **gelés, identiques aux deux modes**. Aucune fonction dédiée au mode page n'existe pour vérifier une entité ; le seul contrôle réel porte sur `field`, via `ResolverError::UnknownField` (§2.2, même limitation qu'en mode fragment). Autrement dit : le nom d'entité reste syntaxiquement obligatoire, sémantiquement décoratif, **dans les deux modes** — §2.2 ne décrit pas une lacune propre au fragment en attente d'un futur rattrapage page, c'est l'état réel, partout, aujourd'hui.

### 4.8 Au runtime : `render()` et `render_page()` coexistent, jamais l'un n'appelle l'autre

**Réserve ajoutée le 25/07/2026, à vérifier** : dans le code généré et le
générateur (`codegen/projection.rs`) confrontés en session, seule la méthode
`render()` (méthode du trait `Projection`) est jamais émise — y compris pour
`content.core`, un composant Mode Page. `render_page` n'apparaît que dans un
commentaire de `build.rs`, jamais comme fonction réellement générée dans tout
ce qui a été audité cette session. Il est possible que cette section décrive
une intention non (ou plus) implémentée telle quelle, ou que `render_page()`
existe dans une partie du pipeline non vue cette session — **non tranché,
faute d'avoir vu 100 % du code source**. Ne pas supposer l'existence de
`render_page()` sans l'avoir vérifiée sur le code réel au moment de la lecture.

Une fois compilées, les deux fonctions vivent dans le même `impl` :

- `render()` (mode fragment) : mises à jour partielles HTMX.
- `render_page()` (mode page) : génération initiale du document complet, écrit une fois sur disque.

`render_page()` n'appelle **pas** `render()` — chacune est une transpilation indépendante du même type d'AST, pas une composition runtime de l'une par l'autre.

Ce découplage n'est pas accessoire : ADR-008 tranche qu'**aucune page complète n'est jamais stockée comme artefact monolithique pré-composé**. Chaque composant (en-tête, contenu, pied de page) reste stocké et invalidé indépendamment. La composition résolue par `{% extends %}`/`{% block %}` a lieu **à l'écriture** (au moment où l'application matérialise la page sur disque), jamais à la lecture — le contrat de lecture reste un `sendfile(2)` unique sur un fichier déjà composé, jamais une résolution applicative au moment de la requête.

**Exception délibérée, ajoutée le 17 juillet 2026 : les pages de `STATIC_PAGES`.** Certaines pages `.marius` (aujourd'hui : `offline`/`offline`, une page de routage sans donnée dynamique — pas une sous-ressource) ne suivent **aucun** des deux chemins ci-dessus. `build.rs` les détecte via une liste explicite (`STATIC_PAGES`, `(schema, table)`), **avant** même l'ouverture du pool Postgres, et les fait passer par le même pipeline `scan → parse_page_tokens → link → lower → validate_ast → resolve_and_measure`, mais avec un `SchemaIndex` **toujours vide** (`fixed: &[], varlena: &[]`) — garde-fou structurel : la moindre référence `{{ record.* }}`/`{% if %}` échoue avec `UnknownField` avant qu'un seul octet ne soit produit. Le flux de tokens résolu est ensuite matérialisé **directement en HTML** (`emit_static_html`, nouvelle fonction de `build.rs`) et écrit une fois sur disque (`build/{theme}/{table}.html`) — **aucun `render_page()` n'est jamais généré ni compilé pour ces pages**. Conséquence pour le cycle de vie runtime (voir `guide-cycle-de-vie-runtime.md`, désormais mis à jour en conséquence) : ces pages ne participent à AUCUN des trois artefacts habituels, ne sont jamais invalidées par `NOTIFY`, et leur seul déclencheur de régénération est un `cargo build` du crate `core/schema`.

À réserver aux pages qui n'ont structurellement aucune raison de dépendre d'une ligne de base de données — une page candidate qui référencerait un jour `{{ record.* }}` casse le build plutôt que de produire un HTML figé et faux, par construction du garde-fou ci-dessus, pas par discipline d'équipe.

### 4.8bis `render_segments()` — troisième fonction possible, composants `marius:large_content` (25/07/2026)

Un composant portant au moins un champ `is_segment` (tag `marius:large_content`,
§2.4) reçoit une troisième forme de sortie, générée par
`generate_segmented_snippet` (`fragment-forge/lib.rs`) au lieu de
`generate_aot_snippet` :

```rust
fn render(record: &Self::Record, _varlena: &{Name}VarlenOwned, buf: &mut String) {
    unreachable!("...composant segmenté, BatchRenderer appelle toujours render_segments().");
}

const MAX_SEGMENTS: usize = 2 * N + 1; // N = nombre de champs is_segment référencés

fn render_segments<'seg>(record: &Self::Record, varlena: &'seg {Name}VarlenOwned, buf: &mut String, segments: &mut Vec<marius_projection::Segment<'seg>>) {
    // en-têtes/pieds statiques → buf.push_str/marius_html_escape, comme render()
    // champ is_segment → segments.push(Segment::Borrowed(...)), jamais dans buf
}
```

`render()` devient un stub qui **panique s'il est appelé** — jamais en usage
normal, puisque `BatchRenderer::render_batch`/`render_batch_pure` appellent
systématiquement `render_segments()` (avec une implémentation par défaut, sur
le trait `Projection`, qui délègue à `render()` pour tout composant sans champ
segmenté — donc aucun changement de comportement pour l'immense majorité des
composants). Voir `CONTRAT-implementation-projection-segmentee.md` pour
l'algorithme complet de scission du token stream (§ « Étape 5 »).

**Conséquence pratique si vous ajoutez `marius:large_content` à un champ
existant** : tout code appelant `P::render()` directement (au lieu de
`P::render_segments()`) pour ce composant se met à paniquer. Régression déjà
rencontrée en session (23/07/2026) sur `render_batch_pure`/un test de
`dispatcher.rs`, non détectée immédiatement car ces fichiers ne font partie
d'aucun Contrat touchant `fragment-forge`/`db-forge` — à garder en tête si
vous étendez ce mécanisme à un nouveau composant : vérifier tout appelant
direct de `render()` pour ce type, pas seulement les chemins déjà connus
(`BatchRenderer`).

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
  {% include chemin %}

Mode page (implémenté) :
  {% extends chemin %}            ← doit être la première construction du fichier
  {% block name %} … {% endblock %}
  {% static chemin %}
  {% asset clé %}                 ← résolu contre manifest.toml (marius-assets), §4.5bis
  {% script %} … {% endscript %}  ← hissé + déduplicé, réinjecté sur <!-- MARIUS_SCRIPTS -->

Interdit, dans les deux modes :
  {% for … %}
  {% else %}
  {% if %} sur un champ non bool
  Imbrication if/if, block/block, block/if
  join / where / filter / group
```

Toute violation est une erreur de compilation (`cargo build` échoue), jamais un comportement silencieux au runtime.

---

_Vérifié et corrigé le 7 juillet 2026._
_Mis à jour le 17 juillet 2026 (v3)._
_Corrigé le 25 juillet 2026 (v4)._
