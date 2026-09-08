# Guide `fragment-forge` — Écrire des spécifications `.marius`

> Compilateur de projections HTML AOT du projet Marius.
> Spécification de référence : `specification-marius-compilateur-projections-html.md` v1.1.

---

## 0. Statut de ce document

| Partie | Contenu | Statut |
| --- | --- | --- |
| **Partie 1** | Mode fragment : `{{ }}`, `{% if %}`, `{% include %}` | Implémenté — pipeline câblé dans `crates/core/schema/build/template/dynamic.rs` |
| **Partie 2** | Mode page : `{% extends %}` (chaîne N-aire), `{% block %}`, `{% import %}`, `{% static %}`, `{% asset %}`, `{% script %}` | Implémenté — pipeline câblé dans `crates/core/schema/build/template/page.rs` et `static_page.rs` |

**Hors périmètre de ce document** : ce guide couvre la compilation `.marius` → `render()`/HTML statique. Il ne couvre pas ce qui se passe *après* — comment `render()` est invoqué, à quelle fréquence, ni ce qui invalide le HTML déjà servi. Un `.marius` correct est une condition nécessaire, jamais suffisante, pour qu'un changement atteigne le navigateur (voir `guide-cycle-de-vie-runtime.md`).

---

## 1. Introduction — le contrat de lecture

Avant d'écrire la première ligne de `.marius`, trois principes gouvernent tout le reste. Si vous ne retenez qu'une chose de ce document, retenez celle-ci : **`fragment-forge` n'est pas un moteur de template**. C'est un compilateur. La syntaxe vous trompera si vous ne lisez pas cette section.

### 1.1 Tool piggybacking

Marius utilise la syntaxe Jinja/Twig (`{{ field }}`, `{% if %}`, `{% extends %}`/`{% block %}`) sans utiliser de moteur Jinja. C'est un détournement délibéré de l'écosystème existant :

- coloration syntaxique automatique dans tout IDE qui reconnaît `.html.j2`/`.twig` ;
- formatters et linters disponibles gratuitement, sans outillage maison ;
- un développeur qui connaît Jinja sait lire un fichier `.marius` au premier coup d'œil.

Ce que ce détournement **ne** signifie **pas** : que les constructions Jinja usuelles fonctionnent. `fragment-forge` reconnaît un sous-ensemble volontairement restreint de cette syntaxe et rejette tout le reste à la compilation, pas à l'exécution. En particulier : `{% import %}` (Partie 2, §4.5bis) n'a **pas** la sémantique de l'`{% import %}` Jinja (import de macros) — c'est un nom réutilisé pour une sémantique Marius spécifique, plus proche de l'`{% include %}` *réel* de Jinja (qui reparse le fichier inclus) que du `{% static %}`/`{% include %}` de ce projet (opaques). Ne présumez d'aucune sémantique Jinja sur ce mot-clé au-delà de ce que documente ce guide.

### 1.2 Principe de moindre surprise (POLA)

Un réflexe Jinja légitime — `{% for product in products %}`, `{% if user.role == "admin" %}` — est rejeté par le compilateur AOT. Ce n'est pas un bug, ni une limitation provisoire : c'est une conséquence directe de la gestion mémoire du moteur.

`render()` est une fonction qui écrit dans un buffer **pré-alloué à une taille calculée au build-time** (`{NAME}_TOTAL_CAP`). Une boucle de longueur non bornée (`{% for %}`) rend ce calcul impossible — la taille de sortie dépendrait du nombre d'éléments en base, connu seulement à l'exécution. D'où l'interdiction structurelle, pas stylistique.

Attendez-vous à des erreurs de compilation, pas des comportements silencieux. C'est le compromis : moins de syntaxe disponible, en échange d'une garantie zéro-réallocation vérifiée par test (`test_{name}_no_realloc`).

### 1.3 Souveraineté du schéma PostgreSQL

Le modèle actuel fait dicter la structure par PostgreSQL. Le template ne fait que sélectionner, parmi les champs déjà exposés par le schéma (`FieldSpec`, `VarlenField`), lesquels apparaissent dans le HTML généré. Conséquence pratique pour vous : un `.marius` ne peut référencer qu'un champ qui existe déjà dans la table ou la jointure varlena associée. Si le champ n'existe pas, on l'ajoute côté SQL — jamais en contournant le compilateur.

**Précision structurelle — « côté SQL » signifie la table physique du composant, jamais une vue.** `fetch_varlena_cols` résout les bornes via `pg_constraint` (`CHECK`) — une vue n'en porte jamais, seule une table physique en a. `ref_table` dans `meta.component_varlena_join` est donc, par construction, une table de composant ECS physique (`content.identity`, `identity.person_biography`, etc.), jamais une vue sémantique (`content.v_article`). Les vues sémantiques sont une interface de lecture **parallèle**, destinée à d'autres consommateurs SQL — elles n'ont aucune arête avec ce pipeline. Modifier `content.v_article` n'a **aucun effet** sur ce que `fragment-forge` peut introspecter : seule l'existence physique du champ dans la table jointe compte.

---

## 2. Le langage `.marius` — mode fragment

### 2.1 Syntaxe autorisée

| Construction | Effet | Génère |
| --- | --- | --- |
| `{{ entity.field }}` | Interpolation d'un champ | `write_fmt` (fixed-length) ou `marius_html_escape` (varlena) |
| `{% if entity.field %} … {% endif %}` | Inclusion conditionnelle | `if record.{field} != 0 { … }` |
| `{% include chemin %}` | Inclusion d'un fragment statique résolu au build | `buf.push_str(include_str!(...))` |
| texte brut | HTML verbatim | `buf.push_str("...")` |
| `{# … #}` | Commentaire — avalé par le scanner | rien : zéro span, zéro code généré (§4.5ter) |

Trois constructions qui produisent effectivement quelque chose au build, plus un quatrième mécanisme — le commentaire — qui par définition n'en produit aucun. Tout le reste est une erreur de compilation. `{# … #}` n'est pas spécifique au mode fragment : la même syntaxe fonctionne à l'identique en mode page (Partie 2), le scanner qui la reconnaît étant partagé par construction entre les deux modes.

**Piège de syntaxe, vérifié contre le scanner** : un chemin (`include`, et en Partie 2 `extends`/`static`/`import`) s'écrit **sans guillemets** — `{% include templates/partials/nav.html %}`, jamais `{% include "templates/partials/nav.html" %}`. Le scanner ne connaît aucun token de littéral de chaîne : il découpe tout contenu de bloc en séquences contiguës non-blanc. Des guillemets écrits par réflexe Jinja ne provoquent **pas** une erreur de syntaxe immédiate — ils sont capturés tels quels comme partie du chemin, et l'échec n'apparaît qu'en aval, au moment de la résolution du fichier, avec un chemin visiblement corrompu par les guillemets dans le message — un symptôme trompeur si vous ne savez pas d'où il vient.

### 2.2 La convention d'entité — et son piège contre-intuitif

La grammaire impose la forme `entity.field` (`{{ record.title }}`, pas `{{ title }}`). C'est un héritage du modèle relationnel — chaque template est lié à exactement une entité — mais **dans l'implémentation actuelle, le nom d'entité n'est pas validé contre le schéma**. Seul `field` est recherché dans `SchemaIndex` (`find_fixed` puis `find_varlena`). `entity` est syntaxiquement obligatoire, sémantiquement décoratif.

Concrètement : `{{ record.description }}` et `{{ nimporte_quoi.description }}` compilent à l'identique tant que `description` existe dans le schéma. Ce n'est pas une autorisation à écrire n'importe quoi — c'est un point de vigilance : le compilateur ne vous protège pas contre un nom d'entité incohérent. La convention en vigueur dans les templates existants : `record` pour tout champ, fixed-length comme varlena.

`UnknownEntity` n'existe dans le code, **ni en mode fragment ni en mode page** — le seul contrôle réel porte sur `field`, via `ResolverError::UnknownField`. Le nom d'entité reste syntaxiquement obligatoire, sémantiquement décoratif, dans les deux modes.

### 2.3 Ce qui est banni, et pourquoi

| Interdit | Raison structurelle |
| --- | --- |
| `{% for … %}` | Sortie de longueur non bornée → rend `{NAME}_TOTAL_CAP` incalculable au build-time |
| `{% else %}` | Réflexe Jinja le plus probable après un `{% if %}` — aucune grammaire dédiée : tombe dans le mot-clé inconnu, `InvalidBlockSequence` |
| Imbrication `{% if %}` dans `{% if %}` | La FSM de validation (`validate_ast`) est un automate à un seul niveau d'état (`current_open_if: Option<(entity, field)>`) — une imbrication ouvre une erreur `NestedIfNotSupported` |
| Mots-clés relationnels (`join`, `where`, `filter`, `group`) | Appartiennent au Write Path PostgreSQL, jamais au Read Path |
| `{% if %}` sur un champ non booléen | Romprait la largeur de struct statiquement connue (`StorageRow #[repr(C)]`) |

Toute séquence de bloc non reconnue (mot-clé inconnu après `{%`) échoue avec `PageParseError::InvalidBlockSequence` en mode fragment.

### 2.4 Champs varlena — disjoncteur Hot / Cold / Erreur (ADR-007)

Un champ `TEXT` sans borne exploitable (`VARCHAR(N)` ou `CHECK (length(col) <= N)`) n'est **pas** automatiquement une erreur. La règle :

- **non référencé** dans le template → champ "Cold", invisible, aucune erreur ;
- **référencé** et borné → "Hot", sa capacité (`max_len × 6`, facteur d'échappement HTML pire cas) entre dans `total_dynamic_bytes` ;
- **référencé** et non borné → erreur de compilation (`ResolverError::UnboundedField`).

Le facteur ×6 n'est pas arbitraire : c'est la longueur de la plus longue entité HTML parmi les caractères échappés (`"` → `&quot;`, 6 octets pour 1 caractère source). Dimensionner sur ce pire cas garantit qu'aucune combinaison de caractères ne peut jamais dépasser `max_len × 6`.

**Deux mécanismes de détection de la borne, ni plus ni moins** (`VarlenField`) :

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

Il n'existe **pas** de troisième mécanisme de *bornage* par annotation — seuls `VARCHAR(N)` et `CHECK` déterminent `max_len`. Un `TEXT` sans l'un de ces deux mécanismes reste `max_len: None`, quoi que vous mettiez en commentaire SQL. **Distinct** de la politique d'*échappement* (`marius:pre_escaped`/`marius:raw`/`marius:large_content`) — trois tags `pg_description` bien réels, qui ne bornent rien : ils choisissent comment le contenu déjà borné (ou non, pour `large_content`) est traité au runtime.

**Forme exacte requise pour un `CHECK` détectable.** La détection repose sur un parsing textuel de la définition de contrainte, pas sur une analyse structurelle de l'arbre SQL — une déviation de forme, sémantiquement équivalente, échoue à être bornée (`cargo:warning` émis avec le texte brut de la contrainte, visible en `cargo build -vv`). Pour une détection fiable dès l'écriture du DDL :

- **une seule** contrainte `CHECK` par colonne portant sur sa longueur ;
- la forme littérale `length(col) <= N`, jamais `N >= length(col)` ni `char_length(col) <= N` mêlé à une autre fonction ;
- `N` un entier littéral nu, jamais une expression ni un cast ;
- la contrainte doit être **`VALID`**.

En cas de doute, préférez `VARCHAR(N)` : borne extraite de `pg_attribute.atttypmod`, aucun parsing.

**Champ nullable** : un varlena `TEXT` sans `NOT NULL` réserve exactement la même capacité qu'un champ non-nullable. Tout champ varlena issu d'un `LEFT JOIN` est systématiquement `Option<String>` côté Rust ; une valeur `NULL` au runtime réduit simplement les octets effectivement écrits, jamais la capacité pré-allouée.

**Politique d'échappement, trois tags `pg_description`, correspondant aux trois variantes de l'enum fermé `EscapePolicy`** :

| Tag | `EscapePolicy` | Facteur | Échappé au runtime ? | Dans `buf` ? |
| --- | --- | --- | --- | --- |
| *(aucun)* | `Escaped` | × 6 | Oui | Oui |
| `marius:pre_escaped` | `PreEscaped` | × 1 | Oui (défense en profondeur) | Oui |
| `marius:raw` | `Raw` | × 1 | **Jamais** | Oui |
| `marius:large_content` | `Raw` + segmenté | **0** | **Jamais** | **Non** |

⚠️ **`pre_escaped` désactive tout échappement HTML pour ce champ** — aucun filet de rattrapage à l'exécution. Cette annotation certifie l'absence de `<`, `>`, `&`, `"`, `'` dans toute valeur possible de la colonne. Réservez-la aux champs contrôlés par l'application — jamais à une donnée saisie par un utilisateur.

**`marius:raw`** : le contenu est du HTML déjà constitué, à l'opposé de `pre_escaped` qui certifie l'*absence* de caractères spéciaux — `raw` certifie au contraire leur présence *intentionnelle*.

**`marius:large_content`** : variante de `raw` pour un champ qui ne doit **jamais dimensionner le buffer partagé** — typiquement un corps d'article pouvant atteindre plusieurs centaines de Ko. Contribution nulle à `total_dynamic_bytes`, exempté du seuil AOT absolu de 64 Ko, et traité différemment par le compilateur : le composant génère `render_segments()` au lieu du simple `render()` — voir §4.8bis. Un seul tag à la fois par colonne.

**Plusieurs champs varlena distincts dans un même template** : autorisé, sans limite de nombre. `total_dynamic_bytes` est la somme de `max_escaped_len()` de chaque champ varlena référencé, sans interaction entre eux. Un seul non borné suffit à faire échouer tout le template, même si les autres sont correctement contraints. Un champ `marius:large_content` contribue **0** à cette somme.

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

Notez `{{ record.description }}` référencé deux fois : chaque occurrence est comptée séparément dans `total_dynamic_bytes` — un champ référencé N fois est mesuré N fois, jamais dédupliqué.

**Amplification par composition (Partie 2)** : la fusion `{% extends %}`/`{% block %}`/`{% import %}` ne réduit jamais ce compte, elle l'agrège sur l'ensemble de la chaîne. Un champ référencé dans un bloc du Root **et** dans un bloc d'un maillon intermédiaire ou de la feuille est compté deux fois dans `PAGE_TOTAL_CAP` — le risque de doublon involontaire grandit avec la profondeur de composition : chaque fichier est écrit séparément, sans vue d'ensemble immédiate sur les champs déjà référencés ailleurs dans la chaîne.

### 2.6 Messages d'erreur que vous rencontrerez

| Source | Erreur | Déclencheur |
| --- | --- | --- |
| `parse_tokens` | `UnexpectedToken { expected, got }` | Token syntaxiquement hors séquence |
| `parse_tokens` | `InvalidBlockSequence` | Mot-clé de bloc inconnu (`for`, `extends`, `block`…) |
| `validate_ast` | `NestedIfNotSupported` | `{% if %}` ouvert dans un `{% if %}` déjà ouvert |
| `validate_ast` | `UnexpectedEndIf` | `{% endif %}` sans `{% if %}` correspondant |
| `validate_ast` | `UnclosedIf` | Fin de fichier avec un `{% if %}` resté ouvert |
| `resolve_and_measure` | `UnknownField` | `field` absent du schéma (ni fixed, ni varlena) |
| `resolve_and_measure` | `UnboundedField` | Varlena référencé sans `max_len` connu |
| `resolve_and_measure` | `IoError` | `{% include %}` pointant vers un fichier introuvable |

Toutes les erreurs de `resolve_and_measure` sont accumulées en une seule passe (stratégie fail-slow) : un template référençant trois champs inconnus remonte trois erreurs en un seul `cargo build`.

---

## 3. Le moteur — ce que `fragment-forge` n'est pas

### 3.1 Démystification

`fragment-forge` n'a pas d'évaluateur, pas de boucle d'interprétation, pas de représentation intermédiaire conservée au runtime. C'est une bibliothèque de pure transformation de texte, appelée depuis `crates/core/schema/build/`, **uniquement pendant `cargo build`**. Son unique sortie observable est un fichier Rust écrit dans `OUT_DIR` et inclus via `include!()`. À l'exécution de l'application, `fragment-forge` n'existe pas : le binaire final ne contient que les `push_str`/`write_fmt`/`marius_html_escape` qu'il a émis.

### 3.2 Le pipeline mode fragment

```
scan(src)              → Iterator<RawSpan>        (tokenisation lexicale, zéro alloc heap)
parse_tokens(spans)     → Vec<FlatPageToken>        (syntaxe, fail-fast)
validate_ast(&tokens)   → Result<(), Vec<SemanticError>>   (équilibre if/endif, FSM 1 niveau)
resolve_and_measure(…)  → Result<TemplateMetrics, Vec<ResolverError>>
                          (résolution I/O des include + calcul de capacité, en une seule passe)
generate_aot_snippet(…) → String                    (transpilation vers Rust natif)
```

Toute l'I/O disque (lecture du `.marius`) vit dans `build.rs` — `fragment-forge` lui-même ne touche jamais le système de fichiers, à l'exception de la résolution des tailles d'`{% include %}` via une closure injectée (`get_file_size`), ce qui le rend testable sans disque réel.

### 3.3 Ce qui sort de la forge

Pour chaque table, le fichier généré contient :

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

C'est le contrat que `fragment-forge` vous garantit en échange des restrictions du §2.3 : si ce test échoue, ce n'est jamais une marge insuffisante à corriger à la main — c'est `max_display_width()` ou `max_escaped_len()` qui sous-estime un type. La capacité n'a délibérément aucune marge arbitraire : toute marge masquerait une sous-estimation réelle.

---

## 4. Composition de pages — `{% extends %}`, `{% block %}`, `{% import %}`, `{% static %}`, `{% asset %}`, `{% script %}`

### 4.1 Toujours du piggybacking — mais sur l'héritage de templates

Le mode fragment (Partie 1) produit un fragment HTML par enregistrement — utile pour les mises à jour partielles HTMX, pas pour une page complète (en-tête, navigation, contenu, pied de page). Le mode page réutilise le modèle d'héritage de templates Jinja/Twig (`extends`/`block`), pour les mêmes raisons qu'au §1.1.

Le principe ne change pas : ces opérateurs n'ont **aucune existence au runtime**. Ce sont des **opérateurs de composition**, résolus une fois pour toutes au build-time — à distinguer des **opérateurs de projection** (`{{ }}`, `{% if %}`) qui, eux, génèrent du code exécuté à chaque enregistrement.

### 4.2 Discriminant fragment / page

Une spécification `.marius` est en **mode page** si et seulement si sa première construction non-whitespace est `{% extends chemin %}` — sans guillemets (§2.1). BOM, lignes vides et whitespace sont tolérés avant. Toute autre première construction → mode fragment.

Si `{% extends %}` est présent mais pas en première position, ou apparaît une seconde fois dans le même fichier : `PageComposeParseError::ExtendsNotFirst`.

### 4.3 `{% extends chemin %}` — chaîne d'héritage, jusqu'à 4 niveaux

Déclare la spécification parente. Chaque fichier de la chaîne (sauf le dernier) porte exactement un `{% extends %}`, en tête. Le dernier maillon — celui qui n'a **aucun** `{% extends %}` — est le **Root** : seul lui porte les positions physiques des `{% block %}` réellement traversées au moment de la fusion.

```jinja
{# templates/content/core.marius — feuille #}
{% extends templates/base.marius %}

{% block head_title %}{{ record.headline }}{% endblock %}

{% block main_content %}
<article class="content-core">
  ...
</article>
{% endblock %}
```

La chaîne peut compter jusqu'à **4 fichiers** (la feuille incluse, jusqu'à 3 ancêtres) — `enfant → parent → grand-parent → Root`. Deux gardes s'appliquent à chaque maillon découvert :

- **profondeur** : au-delà de 4 fichiers, `cargo:error` nommé, la chaîne complète est affichée ;
- **cycle** : un chemin qui réapparaît dans la chaîne (`A extends B`, `B extends A`) est détecté et rejeté avec la chaîne complète dans le message, jamais une récursion qui bloquerait le build.

À chaque `cargo build`, un `cargo:warning` affiche la chaîne complète résolue pour chaque table Mode Page — utile pour retrouver, sans ouvrir chaque fichier, quel est le Root réel d'un maillon intermédiaire :

```
cargo:warning=DB-Forge [blog.post] : chaîne extends : post.marius -> layout_blog.marius -> base.marius
```

### 4.4 `{% block name %} … {% endblock %}`

Déclare, dans le Root, un point de substitution avec une valeur par défaut ; dans n'importe quel maillon non-Root de la chaîne (feuille ou intermédiaire), la valeur de remplacement. La résolution est une **substitution textuelle pure** au moment de la fusion — pas d'évaluation, pas de portée.

```jinja
{# templates/base.marius — Root #}
<!DOCTYPE html>
<html lang="fr">
<head>
  <title>{% block head_title %}Marius{% endblock %}</title>
</head>
<body>
{% block main_content %}{% endblock %}
</body>
</html>
```

**Règle de résolution sur une chaîne à N niveaux — « le maillon le plus proche de la feuille gagne »** : pour chaque bloc déclaré par le Root, la substitution retenue est celle du premier maillon, en remontant de la feuille vers le Root, qui redéfinit ce nom. Si aucun ne le redéfinit, le contenu par défaut du Root est conservé. Un maillon intermédiaire peut donc voir son propre override d'un bloc être lui-même écrasé par la feuille — c'est le comportement attendu, pas une ambiguïté.

**`OrphanBlock` : contre le Root, quel que soit le niveau de déclaration.** Un bloc déclaré à n'importe quel maillon non-Root (feuille ou intermédiaire) sans `{% block %}` de même nom dans le Root est rejeté — seul le Root porte des positions physiques, un bloc qui n'y correspond à rien est du code mort par construction. Le message d'erreur nomme le fichier fautif précisément (pas seulement le nom du bloc), quel que soit sa profondeur dans la chaîne :

```
cargo:error=DB-Forge [blog.post] : bloc `sidebar` déclaré dans layout_blog.marius
ne correspond à aucun slot du Root (base.marius) — bloc mort, à supprimer ou renommer
```

**Imbrication `{% block %}` — supportée, écrasement complet du parent (« Option A »).** Un `{% block %}` peut désormais être déclaré à l'intérieur d'un autre `{% block %}`, sur autant de niveaux que nécessaire — aucune limite de profondeur n'est imposée. La règle qui gouverne la résolution d'un sous-bloc est celle-ci, et elle a une conséquence qui surprend si elle n'est pas anticipée : **redéfinir un bloc parent efface tous ses sous-blocs**. Un maillon qui redéfinit un bloc parent doit redéclarer sa propre structure interne (y compris ses éventuels sous-blocs) s'il veut que ses propres descendants dans la chaîne puissent encore la surcharger finement — le contenu par défaut du sous-bloc, tel que déclaré par le Root, n'est jamais consulté une fois qu'une source différente du Root a été retenue pour le parent.

```jinja
{# templates/base.marius — Root #}
{% block main_nav %}
  <nav>Navigation par défaut</nav>
  {% block current_tab %}<span>Accueil</span>{% endblock %}
{% endblock %}
```

```jinja
{# templates/shop_layout.marius — extends base.marius #}
{% block main_nav %}
  <nav>Navigation boutique</nav>
  {% block current_tab %}<span>Boutique</span>{% endblock %}
{% endblock %}
```

```jinja
{# templates/product_page.marius — extends shop_layout.marius #}
{% block current_tab %}<span>Fiche produit</span>{% endblock %}
```

Ici, `product_page.marius` ne touche jamais `main_nav` : sa structure retenue reste celle de `shop_layout.marius` (« Navigation boutique »). En revanche `current_tab` remonte jusqu'à `product_page.marius`, parce que `shop_layout.marius` a pris soin de redéclarer ce sous-bloc en le redéfinissant — s'il ne l'avait pas fait, `current_tab` aurait disparu avec le reste du contenu par défaut de `main_nav`, et rien dans `product_page.marius` n'aurait pu le faire réapparaître, quel que soit son nom.

**La résolution d'un sous-bloc repart toujours de la source retenue pour son parent, jamais du Root.** Concrètement : une fois qu'un maillon a été retenu comme source d'un bloc, seuls les maillons **strictement plus proches de la feuille que ce maillon** sont consultés pour résoudre ses sous-blocs — jamais les maillons plus proches du Root, et jamais les sous-blocs par défaut du Root lui-même. C'est ce mécanisme, appliqué récursivement à chaque niveau de redéfinition, qui produit l'effet d'écrasement décrit ci-dessus.

**`OrphanBlock` reste détectable à n'importe quelle profondeur.** Un sous-bloc déclaré par un maillon non-Root, imbriqué ou non, dont le nom ne correspond à aucun bloc du Root — de premier niveau ou lui-même imbriqué — est rejeté exactement comme un bloc de premier niveau orphelin : la vérification reste purement par nom sur l'ensemble complet des blocs du Root, sans égard à la profondeur de la déclaration fautive.

**Empreinte mémoire d'un bloc du Root surchargé.** Quand un maillon plus dérivé redéfinit un bloc, le contenu par défaut du Root pour ce bloc **n'est jamais projeté** dans l'AST fusionné — `lower()` ne parcourt et n'émet que les tokens de la source retenue, jamais les deux. Le contenu perdant n'atteint jamais `generate_aot_snippet` : zéro octet en `.rodata`, par construction du pipeline de fusion. Pour un bloc imbriqué, cette règle s'applique à toute la sous-arborescence effacée par un écrasement complet (Option A ci-dessus) : ni le contenu par défaut du sous-bloc du Root, ni ses éventuels propres descendants, ne sont jamais parcourus une fois que le parent a été résolu vers une source différente du Root.

### 4.5 `{% static chemin %}`

Inclut un fichier d'octets HTML **opaque**, lu au build-time, inliné comme `&'static str` dans un module généré `static_partials`. Le contenu n'est **jamais reparsé comme template** : aucun `{% block %}`, `{{ champ }}`, `{% asset %}` n'y a d'effet, tout sort littéralement — à distinguer nettement de `{% import %}` (§4.5bis) qui, lui, reparse intégralement le fichier ciblé.

Si plusieurs pages référencent le même fichier `{% static %}`, elles partagent **la même constante** — déduplication structurelle en `.rodata`, pas une copie par page.

**Aucun seuil de taille n'est vérifié aujourd'hui** — un fichier `{% static %}` de plusieurs mégaoctets compile sans avertissement. À évaluer au cas par cas.

### 4.5bis `{% import chemin %}` — composition horizontale de fragments

Développe intégralement le fichier ciblé comme un vrai template Mode Page — à la différence de `{% static %}`, le contenu est reparsé, et ses propres `{% block %}`/`{% asset %}`/`{% script %}` de premier niveau rejoignent l'espace de noms plat du fichier qui l'importe, exactement comme s'ils y avaient été écrits en dur à cet endroit.

```jinja
{# templates/base.marius — Root #}
<head>
    {% import templates/head.marius %}
    <!-- MARIUS_SCRIPTS -->
    <!-- MARIUS_MODULES -->
</head>
<body>
    {% import templates/navigation.marius %}
    {% block main_content %}{% endblock %}
    {% import templates/footer.marius %}
</body>
```

```jinja
{# templates/head.marius — fragment importé #}
<meta charset="utf-8">
<title>{% block head_title %}Default title{% endblock %}</title>
<meta name="description" content="{% block head_description %}Default description{% endblock %}">
<link rel="stylesheet" href="{% asset styles/main.css %}" media="screen">
```

Une fois `{% import templates/head.marius %}` développé, `head_title`/`head_description` deviennent des blocs de premier niveau de `base.marius` — n'importe quel maillon de la chaîne `extends` peut les overrider exactement comme s'ils avaient été déclarés directement dans `base.marius`.

**Contraintes, vérifiées à chaque niveau indépendamment :**

- **Position top-level uniquement.** Un `{% import %}` à l'intérieur d'un `{% block %}` ouvert est rejeté (`ImportInsideBlock`) — jamais toléré, même si le fragment ciblé ne contient lui-même aucun bloc. Un import ne peut se substituer qu'à un slot de premier niveau, jamais à du contenu à l'intérieur d'un bloc. Contrainte maintenue telle quelle depuis que l'imbrication des `{% block %}` a été admise (§4.4) : question jugée orthogonale, non revue à cette occasion — un import positionné à l'intérieur d'un bloc reste rejeté, quelle que soit la profondeur d'imbrication désormais admise pour les blocs eux-mêmes.
- **Un fragment importé ne peut pas lui-même `{% extends %}`.** Il n'a aucune position physique propre — un fragment importé n'est jamais le Root d'une fusion, `{% extends %}` y serait dénué de sens.
- **L'imbrication de `{% block %}` à travers un import suit la même règle que partout ailleurs (§4.4).** Si `head.marius` déclare `{% block main_head %}...{% block head_title %}...{% endblock %}...{% endblock %}`, l'expansion produit exactement la même structure imbriquée que si ce contenu avait été écrit en dur dans `base.marius` — plus une erreur depuis que l'imbrication est admise. Un fragment conçu à l'origine comme un `{% block %}` unique enveloppant plusieurs sous-parties n'a donc plus besoin d'être aplati avant extraction ; l'enveloppe peut être conservée telle quelle, avec les mêmes conséquences de résolution qu'un bloc imbriqué écrit directement dans le fichier qui importe (redéfinir l'enveloppe efface ses sous-blocs, sauf redéclaration explicite — §4.4).
- **Profondeur bornée à 4 niveaux, indépendamment de la profondeur `extends`.** Un fragment importé peut lui-même importer d'autres fragments, jusqu'à 4 niveaux d'imbrication d'imports — comptés séparément de la chaîne `extends` (un Root peut être à la fois au bout d'une chaîne `extends` de 4 fichiers *et* importer sur 4 niveaux : les deux compteurs ne se cumulent jamais).
- **Détection de cycle**, propre à chaque branche d'import (deux fragments distincts important indépendamment le même troisième fragment n'est jamais un cycle — seul un fragment qui s'importe lui-même, directement ou via une chaîne de sous-imports, l'est).
- **Aucune déduplication.** Contrairement à `{% static %}`, deux occurrences de `{% import same.marius %}` produisent deux expansions indépendantes, chacune pouvant contenir ses propres blocs.
- **Traçabilité partielle des erreurs.** `OrphanBlock`/erreurs de linking pointant vers du contenu importé désignent le fichier du maillon `extends` qui a fait l'import, pas le fragment importé lui-même — un bloc orphelin déclaré dans `head.marius` sera rapporté comme déclaré dans `base.marius` si c'est `base.marius` qui l'importe. Limite connue, pas un bug.

### 4.5ter Commentaires `.marius` — syntaxe `{# … #}`, et le piège HTML qui subsiste

**Syntaxe** : `{# contenu quelconque #}`. Le scanner avale tout le bloc en interne — aucun span n'est produit pour son contenu, donc aucune trace, ni dans le HTML compilé ni dans l'AST intermédiaire. C'est la façon correcte de désactiver temporairement une ligne (un `{% import %}`, un `{% static %}`, un `{% block %}`…) pendant une itération, sans la retirer du fichier.

Trois règles gouvernent son comportement :

- **Disponible dans les deux modes**, fragment et page, sans distinction — le scanner qui le reconnaît est partagé par construction entre les deux.
- **Pas d'imbrication.** Un commentaire se ferme à la **première** occurrence de `#}` rencontrée, point. Cohérent avec le modèle HTML (`<!-- -->` ne s'imbrique pas non plus). Piège à connaître : commenter une région qui contient déjà un `{# #}` tronque le commentaire plus tôt que prévu — la suite redevient du HTML actif, pas du commentaire.
- **Reconnu uniquement hors de `{{ }}`/`{% %}`.** `{#` n'est cherché que dans le flux HTML de premier niveau — jamais à l'intérieur d'une expression ou d'un bloc déjà ouverts. Un `{# %}` glissé au milieu d'un identifiant n'est jamais interprété comme un commentaire. En pratique : `{# #}` commente une ligne ou un bloc entier, jamais un fragment d'expression isolé.

**Effet de bord cosmétique** : le blanc (espaces, retour à la ligne) qui entourait la ligne commentée reste, lui, un `Literal` HTML ordinaire — commenter une ligne entière peut laisser une ligne vide dans le HTML compilé. Jamais fonctionnel, jamais une erreur.

**Non fermé, c'est une erreur de compilation nommée**, pas un comportement silencieux : un `{#` jamais refermé avant la fin du fichier fait échouer `cargo build`, au même titre qu'un `{{`/`{%` non fermé.

**Le piège qui subsiste malgré `{# #}` : un commentaire HTML n'est toujours pas un commentaire pour le compilateur.**

```jinja
<!-- {% import templates/foo.marius %} -->
```

Le scanner ne connaît pas la syntaxe `<!-- -->` — il cherche `{{`/`{%`/`{#` n'importe où dans le texte, y compris à l'intérieur de ce qu'un navigateur afficherait comme un commentaire. La ligne ci-dessus est développée exactement comme si les chevrons de commentaire n'existaient pas : `foo.marius` est activement importé. `{# … #}` est la seule construction qui neutralise réellement ce genre de ligne — pas `<!-- -->`, même maintenant.

**Deux marqueurs échappent à ce piège, mais par un mécanisme entièrement différent** : `<!-- MARIUS_SCRIPTS -->` et `<!-- MARIUS_MODULES -->` ne sont jamais interprétés comme des commentaires *ni* comme des constructions `.marius` — ce sont des sous-chaînes littérales, recherchées par `str::find` dans le contenu déjà résolu d'un token `Static`, après que tout le reste du pipeline de composition a tourné. Ils fonctionnent précisément *parce qu'ils ne contiennent aucun délimiteur* `{{`/`{%`/`{#`, pas parce que le scanner les reconnaît comme des ancres spéciales. Ne les enveloppez pas dans un `{# #}` : ça les ferait disparaître du contenu `Static` avant que le hoisting ne les cherche.

### 4.6 Algorithme de fusion — ce qui se passe à `cargo build`

```
1. Découvrir la chaîne extends (feuille → Root), bornée à 4 fichiers, cycle détecté
2. Pour chaque maillon de la chaîne : découvrir récursivement ses {% import %}
   top-level, bornés à 4 niveaux, cycle détecté indépendamment
3. Admettre chaque maillon en arène : parser, développer ses imports
   (remplacement positionnel des tokens Import par les tokens du fragment ciblé)
4. Collecter les blocs de chaque maillon — l'imbrication est admise, un bloc
   déclaré à l'intérieur d'un autre est rattaché à son parent, jamais rejeté
5. Résoudre chaque bloc de premier niveau du Root : maillon le plus proche de
   la feuille qui le redéfinit, sinon valeur par défaut du Root (OrphanBlock
   si un maillon non-Root déclare un bloc absent du Root, à n'importe quelle
   profondeur). Pour un bloc imbriqué : la résolution repart des sous-blocs
   de LA SOURCE retenue pour son parent, jamais des sous-blocs du Root —
   redéfinir un parent efface ses sous-blocs par défaut (§4.4, Option A)
6. Fusionner (lower) : projection plate Vec<FlatPageToken> à partir du seul Root
7. Résoudre chaque {% static %} : taille réelle, chemin relatif, cargo:rerun-if-changed
8. Validation sémantique : entité, champs, type bool des conditions, absence de
   {% for %}, absence de mot-clé relationnel, absence d'imbrication
→ Vec<FlatPageToken> : Static | Field | IfBool | EndIf | StaticInclude | AssetRef | ScriptStart | ScriptEnd
  (ScriptStart/ScriptEnd retirés du flux par hoist_and_dedupe_scripts avant l'étape 8 ci-dessus)
```

Le résultat de la fusion est un AST **plat**, du même type `FlatPageToken` que le mode fragment. Conséquence directe : tout ce que vous avez appris en Partie 1 sur les contraintes de `{{ }}`/`{% if %}` s'applique identiquement après fusion — la composition de page ne compose que des fragments qui doivent chacun déjà s'y conformer.

`PAGE_TOTAL_CAP` est calculé sur cet AST fusionné selon la même formule qu'en §3.4, en une seule passe fusionnée avec la résolution sémantique.

### 4.7 Erreurs spécifiques au mode page

| Erreur | Déclencheur |
| --- | --- |
| `ExtendsNotFirst` | `{% extends %}` pas en première position, ou en double dans le même fichier |
| Chaîne extends trop profonde | Plus de 4 fichiers dans la chaîne `extends` |
| Cycle extends | Un chemin réapparaît dans la chaîne `extends` |
| Extends introuvable | Chemin de `{% extends %}` introuvable sur disque |
| `ImportInsideBlock` | `{% import %}` rencontré à l'intérieur d'un `{% block %}` ouvert |
| Chaîne import trop profonde | Plus de 4 niveaux d'imports imbriqués |
| Cycle import | Un chemin réapparaît dans une branche d'imports |
| Import introuvable | Chemin de `{% import %}` introuvable sur disque |
| Fragment importé avec `extends` | Un fichier ciblé par `{% import %}` déclare lui-même `{% extends %}` |
| `StaticFileNotFound` | Chemin de `{% static %}` introuvable |
| `OrphanBlock` | Bloc déclaré à un niveau non-Root sans correspondant dans le Root |
| `NestedIfNotSupported` | `{% if %}` imbriqué dans un autre `{% if %}` (mode fragment ou AST fusionné) — contrainte distincte de l'imbrication `{% block %}` (§4.4, admise), imposée par `STATIC_CAP`/`DYNAMIC_CAP` sur le chemin HTTP chaud |
| `UnknownField` | Champ absent du schéma (point de convergence, identique au mode fragment) |
| `NonBoolIfCondition` | `{% if %}` sur un champ non `bool` |
| `ForLoopDetected` | `{% for %}` détecté |
| `RelationalKeyword` | `join`/`where`/`filter`/`group` détecté — également tout mot-clé de bloc inconnu, y compris une faute de frappe sur `asset`/`script` |
| *(pas de variante nommée)* | `{% asset %}` référençant une clé absente de `manifest.toml` — `cargo:error` émis directement par `build.rs`, pas par une erreur `fragment-forge` dédiée |

Toutes les erreurs listées comme accumulables (`collect_blocks`, `link_chain`, `resolve_and_measure`) le sont réellement en une seule passe — fail-slow. Les erreurs de découverte de chaîne (profondeur, cycle, fichier introuvable) sont fail-fast : elles interrompent la résolution dès la première rencontrée plutôt que d'accumuler, chaque niveau supplémentaire coûtant une lecture disque inutile une fois l'échec avéré.

### 4.5bis (suite) `{% asset key %}` et `{% script %} … {% endscript %}`

**`{% asset key %}`** — résout `key` (un nom de fichier logique, ex. `main.js`, jamais un chemin complet) contre le manifeste d'assets produit par `marius-assets` (`build/{theme}/manifest.toml`, table `[assets."clé"]`). Généré comme `FlatPageToken::AssetRef(key)` par le parseur.

**`{% script %} … {% endscript %}`** — capture un bloc `<script>...</script>` complet, le "hisse" (déduplication si plusieurs blocs identiques apparaissent), et le réinjecte à l'emplacement du marqueur littéral `<!-- MARIUS_SCRIPTS -->` dans le Root. Le marqueur est cherché comme sous-chaîne exacte (§4.5ter) — un simple point d'ancrage textuel.

**Où vit quoi :**

- `fragment-forge` définit les token types et les fonctions de manipulation du flux : `hoist_and_dedupe_scripts`, `splice_hoisted_scripts`. `resolve_and_measure` accepte un closure `resolve_asset_len` injecté par l'appelant — `fragment-forge` ne lit jamais lui-même `manifest.toml`.
- `crates/core/schema/build/` lit `manifest.toml`, fournit la closure de résolution, et échoue avec un `cargo:error` explicite si une clé est absente du manifeste — diagnostic par distance de Levenshtein sur les clés existantes.

**Piège de syntaxe** : le mot-clé est `asset`, **singulier** — `{% assets utils.svg %}` (pluriel) tombe dans le rejet générique `RelationalKeyword { keyword }`, partagé avec `join`/`where`/`filter`/`group`.

### 4.8 `STATIC_PAGES` — pages sans donnée dynamique

Certaines pages `.marius` (aujourd'hui : `offline`/`offline`, une page de routage sans donnée dynamique) ne suivent pas le chemin `fetch_component_list`. `build/template/static_page.rs` les détecte via une liste explicite (`STATIC_PAGES`, `(schema, table)`), **avant** même l'ouverture du pool Postgres, et les fait passer par le **même** pipeline de composition que les pages pilotées par une table (chaîne `extends` N-aire, `{% import %}`, `link_chain`, `lower` — briques partagées avec `build/template/page.rs`, pas une copie séparée), mais avec un `SchemaIndex` **toujours vide** (`fixed: &[], varlena: &[]`) — garde-fou structurel : la moindre référence `{{ record.* }}`/`{% if %}` échoue avec `UnknownField` avant qu'un seul octet ne soit produit.

Le flux de tokens résolu est ensuite matérialisé **directement en HTML** (`emit_static_html`) et écrit une fois sur disque (`build/{theme}/{table}.html`) — aucune fonction `render()` n'est jamais générée ni compilée pour ces pages. Conséquence pour le cycle de vie runtime : ces pages ne participent à aucun des artefacts habituels, ne sont jamais invalidées par `NOTIFY`, et leur seul déclencheur de régénération est un `cargo build` du crate `core/schema`.

À réserver aux pages qui n'ont structurellement aucune raison de dépendre d'une ligne de base de données.

### 4.8bis `render_segments()` — composants `marius:large_content`

Un composant portant au moins un champ `is_segment` (tag `marius:large_content`, §2.4) reçoit une troisième forme de sortie, générée par `generate_segmented_snippet` au lieu de `generate_aot_snippet` :

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

`render()` devient un stub qui **panique s'il est appelé** — jamais en usage normal, puisque `BatchRenderer::render_batch`/`render_batch_pure` appellent systématiquement `render_segments()` (implémentation par défaut sur le trait `Projection`, qui délègue à `render()` pour tout composant sans champ segmenté — aucun changement pour l'immense majorité des composants).

**Conséquence pratique si vous ajoutez `marius:large_content` à un champ existant** : tout code appelant `P::render()` directement pour ce composant se met à paniquer — vérifiez tout appelant direct, pas seulement `BatchRenderer`.

### 4.9 Ce qui ne change pas en passant au mode page

- L'AOT absolu : la vue reste résolue au build-time, jamais interprétée au runtime.
- Zéro allocation intermédiaire : `buf.reserve(PAGE_TOTAL_CAP)` reste la première instruction.
- L'entité unique par spécification : une page composée référence toujours une seule entité porteuse de données dynamiques ; les données d'entités secondaires doivent être pré-agrégées côté PostgreSQL — jamais un second `fetch_batch` au moment du rendu.

---

## 5. Référence rapide

```
Commun aux deux modes :
  {# … #}                         ← commentaire, zéro span émis, pas d'imbrication (§4.5ter)

Mode fragment :
  {{ entity.field }}
  {% if entity.bool_field %} … {% endif %}
  {% include chemin %}

Mode page :
  {% extends chemin %}            ← 1re construction du fichier, chaîne jusqu'à 4 fichiers
  {% block name %} … {% endblock %}   ← imbrication admise, résolu contre le Root, écrasement complet du parent (§4.4)
  {% import chemin %}             ← top-level uniquement, jusqu'à 4 niveaux, fragment reparsé
  {% static chemin %}             ← contenu opaque, jamais reparsé, dédupliqué
  {% asset clé %}                 ← résolu contre manifest.toml (marius-assets)
  {% script %} … {% endscript %}  ← hissé + déduplié, réinjecté sur <!-- MARIUS_SCRIPTS -->

Interdit, dans les deux modes :
  {% for … %}
  {% else %}
  {% if %} sur un champ non bool
  Imbrication if/if (mode fragment et AST fusionné), et {# #} imbriqué
  join / where / filter / group
  <!-- --> comme commentaire .marius — reste actif, pas neutralisé (§4.5ter) ; utiliser {# #}
```

Toute violation est une erreur de compilation (`cargo build` échoue), jamais un comportement silencieux au runtime.

---

_Document mis à jour le 8 septembre 2026_
