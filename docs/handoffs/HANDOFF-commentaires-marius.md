# HANDOFF — Syntaxe de commentaire `{# #}` pour `.marius`

> Document de conception, pas d'implémentation. Rien ici ne compile.
> Contexte : aucune syntaxe de commentaire n'existe aujourd'hui dans `.marius`.
> Un commentaire HTML (`<!-- -->`) n'est **pas** neutralisé par le scanner —
> tout `{{ }}`/`{% %}` qu'il contient reste activement interprété (voir
> `fragment-forge-guide.md` §4.5ter, qui documente le piège tel quel en
> attendant ce chantier).

---

## 1. Pourquoi ce n'est pas un simple ajout de token

`{# #}` est structurellement différent de tout ce qui a été touché cette session (`{% import %}`, `link_chain`). Ces deux fonctionnalités vivaient entièrement dans le module `page` (`fragment-forge/src/page/`), jamais dans le Scanner. `{# #}` touche `lexer.rs` — le Scanner, **partagé** par construction entre Mode Fragment et Mode Page (`scan()` est appelé aussi bien par `parse_tokens`, gelé, que par `parse_page_tokens`). C'est le seul fichier de tout ce projet resté intégralement intact depuis sa Phase 1.2 initiale, y compris pendant les deux sessions qui ont généralisé `{% extends %}` puis introduit `{% import %}`. Y toucher est une décision de portée différente, pas une extension de plus dans la même veine.

---

## 2. Trois questions à trancher avant tout code

### 2.1 Portée — un seul mode, ou les deux ?

Faux dilemme en pratique : `scan()` est **agnostique du mode** — il produit un flux de `RawSpan` consommé indifféremment par `parse_tokens` (Fragment) ou `parse_page_tokens` (Page), sans qu'aucun des deux ne le paramètre. Restreindre `{# #}` à un seul mode demanderait de faire porter une notion de mode au Scanner lui-même, qu'il n'a jamais eue — plus de travail, pour un bénéfice qui reste à justifier. **Recommandation** : disponible dans les deux modes, symétrique de `{{ }}`/`{% %}`, sans paramètre de mode introduit dans le Scanner.

### 2.2 Imbrication — `{# un {# commentaire #} imbriqué #}`

Deux options, avec un compromis simplicité/robustesse net :

- **Sans imbrication** (recommandé) : un commentaire se ferme au premier `#}` rencontré, point. Cohérent avec le modèle mental HTML (`<!-- -->` ne s'imbrique pas non plus). Risque documenté, pas éliminé : commenter une région qui contient elle-même un `{# #}` tronque le commentaire plus tôt que prévu — à documenter explicitement dans le guide au moment de l'implémentation, au même titre que les autres pièges déjà recensés (chemins entre guillemets, etc.).
- **Avec imbrication** : un compteur de profondeur dans le mode `InComment` du Scanner (incrémenté sur chaque `{#` rencontré à l'intérieur, décrémenté sur chaque `#}`, fermeture réelle seulement au compteur nul). Robuste, mais complexité de scanner non négligeable pour un besoin qu'aucun cas d'usage réel n'a encore justifié.

**Recommandation** : sans imbrication, pour rester cohérent avec le reste du Scanner (aucun autre mode n'a de compteur de profondeur — `InExpr`/`InBlock` sont des automates à état plat, précisément parce que rien dans ce Scanner n'a jamais eu besoin de récursion).

### 2.3 Où le Scanner reconnaît-il `{#` ?

Seulement en mode `Literal` (flux HTML de premier niveau, là où `{{`/`{%` sont déjà recherchés aujourd'hui) — **pas** à l'intérieur d'un `{% %}`/`{{ }}` déjà ouvert. Un `{# %}` glissé au milieu d'un identifiant (`{% if user.a{# nope #}ctive %}`) ne serait donc jamais reconnu comme un commentaire — cas qu'aucun besoin réel ne motive (le cas d'usage exprimé est de commenter une ligne entière — un `{% import %}`, un bloc — pas un fragment d'expression). Restreindre la détection au seul mode `Literal` limite la surface de changement à une seule branche du Scanner (`Mode::Literal`), les modes `InExpr`/`InBlock` restant totalement intacts.

---

## 3. Forme d'implémentation recommandée — zéro token émis

Deux formes possibles pour faire remonter un commentaire jusqu'au flux de tokens :

- **(a) Le Scanner émet des spans dédiés** (`SpanKind::CommentOpen`/`CommentClose`, ou un unique `SpanKind::Comment` portant le contenu), et **chaque parseur** (`parse_tokens`, gelé, ET `parse_page_tokens`) doit apprendre à les reconnaître et les ignorer. Deux fichiers à toucher, dont un explicitement gelé pour tout le reste — à éviter si une alternative existe.
- **(b) Le Scanner avale le commentaire entièrement en interne, sans jamais émettre le moindre `RawSpan` pour cette portion** — dans `Mode::Literal`, à la détection de `{#` comme délimiteur le plus proche (recherche à trois branches désormais : `{{`/`{%`/`{#`, au lieu de deux), le Scanner avance en interne jusqu'au `#}` correspondant (recherche de sous-chaîne, comme la fermeture `%}` du mode `InBlock` aujourd'hui), sans changer de mode public ni produire de span, puis reprend sa recherche `Literal` normale à partir de la position suivante.

**Recommandation nette : (b).** Zéro modification de `parse_tokens` (reste gelé, aucune exception à justifier), zéro modification de `parse_page_tokens`, zéro nouveau `SpanKind` à faire connaître à deux automates différents. Toute la fonctionnalité tient dans une extension de la seule branche `Mode::Literal` du `Scanner::next()` — la plus petite surface de changement possible pour ce résultat, et la seule qui n'oblige à rouvrir aucun fichier déjà considéré clos.

Effet de bord mineur, à noter dans le guide au moment de l'implémentation : le blanc (espaces, retour à la ligne) qui entourait la ligne commentée reste, lui, un `Literal` HTML ordinaire — commenter une ligne entière peut laisser une ligne vide dans le HTML compilé. Cosmétique, jamais fonctionnel.

---

## 4. Ce que ce document ne tranche pas

- La forme exacte du contenu autorisé entre `{#` et `#}` — tout octet, ou une restriction quelconque (improbable, mais à écarter explicitement plutôt que par omission).
- Si un `{#` non fermé en fin de fichier doit produire une erreur nommée (cohérent avec `{{`/`{%` non fermés aujourd'hui, qui retournent `None` prématurément côté Scanner et remontent une erreur de Parser) ou être silencieusement toléré — la cohérence avec le traitement existant des délimiteurs non fermés (§1.2 du Scanner, `InExpr`/`InBlock`) penche pour une erreur nommée, mais à confirmer en session plutôt qu'à décider ici par défaut.

## 5. Séquencement suggéré pour la session future

1. Confirmer §2.1–2.3 par écrit (portée deux modes, sans imbrication, `Literal` uniquement) avant tout code.
2. Étendre `Scanner::next()`, mode `Literal` uniquement — recherche à trois délimiteurs, avalage interne de `{# … #}` sans émission de span (§3, option b).
3. Un test de non-régression explicite : un `.marius` contenant `{# {% import foo.marius %} #}` compile, et le AST produit ne contient **aucune** trace de `foo.marius` — la preuve positive que le commentaire a été neutralisé, pas seulement qu'aucune erreur n'a été levée.
4. Un test sur le cas non fermé (`{# jamais fermé` en fin de fichier) — comportement à figer explicitement (§4).
5. Mettre à jour `fragment-forge-guide.md` (§1.1, §2.1, §4.5ter, §5) une fois le tout compilé et testé — jamais avant.

---

# Addendum pour les arbitrages :

Voici la synthèse des **décisions** pour l'implémentation de `{# #}` :

## Décision 1 — Portée

> **Disponible dans les deux modes (Fragment ET Page)**

Le scanner reste agnostique du mode appelant — pas de paramètre `ScanMode` introduit. `{# #}` fonctionne partout où `{{ }}` et `{% %}` fonctionnent aujourd'hui.

## Décision 2 — Imbrication

> **Non autorisée** (pas de compteur de profondeur)

Un commentaire se ferme à la **première** occurrence de `#}` rencontrée. Comportement cohérent avec HTML (`<!-- -->`), et compatible avec l'architecture actuelle du scanner (automate à état plat, sans récursion).

**Piège documenté** : commenter une région qui contient déjà un `{# #}` tronque le commentaire — à documenter explicitement dans le guide.

## Décision 3 — Où le scanner reconnaît `{#`

> **Uniquement en mode `Literal`**

Pas de détection à l'intérieur d'un `{{ }}` ou `{% %}` déjà ouvert. Le commentaire ne peut commenter qu'une ligne/block entier, pas un fragment d'expression. Surface de changement minimale : une seule branche du scanner modifiée.

## Décision complémentaire — §4

| Point | Décision |
|-------|----------|
| **Contenu autorisé** | Tout octet entre `{#` et `#}` — pas de restriction |
| **Non fermé** | Erreur nommée, cohérente avec `{{` / `{%` non fermés |
