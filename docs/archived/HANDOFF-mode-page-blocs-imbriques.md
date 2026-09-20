# HANDOFF — Imbrication de `{% block %}` en Mode Page

> Document de conception, pas d'implémentation. Rien ici ne compile.
> Statut : besoin anticipé, pas encore concrétisé par un second cas d'usage réel
> au-delà de `head_title`/`head_description` (déjà résolus sans imbrication,
> voir `templates/head.marius`). À ouvrir comme session dédiée le jour où un
> second cas confirme la direction (ex. navigation avec onglet courant,
> pied de page avec zones overridables indépendamment).

---

## 1. Pourquoi ce n'est pas déjà possible

Deux interdictions d'imbrication coexistent dans le projet, avec des justifications de nature différente :

- **Mode Fragment, `{% if %}` imbriqué** — interdit par une contrainte dure : `STATIC_CAP`/`DYNAMIC_CAP` exigent une taille pire-cas calculable sans branchement, sur le chemin HTTP chaud. Non négociable, au cœur du manifeste AOT/DOD.
- **Mode Page, `{% block %}` imbriqué** — interdit par choix d'implémentation, pas par une contrainte de cette nature. La composition Mode Page se joue entièrement dans `build.rs` ; `lower()` produit toujours, in fine, un `Vec<FlatPageToken>` plat, qu'il ait fallu 1 ou 5 niveaux de blocs pour l'assembler. **Aucune contrainte de capacité runtime n'interdit l'imbrication ici.**

Concrètement, trois éléments supposent aujourd'hui la platitude :

1. `NamedBlockRange { name, template, start, end }` — une plage linéaire `[start, end)` dans le flux d'un seul template. Aucune notion de sous-plage.
2. `link_chain` — résout chaque nom du Root en une seule passe, feuille → Root, en cherchant une correspondance de nom exacte parmi des plages elles-mêmes plates.
3. `lower()` / `lower_leaf_token` — projette le contenu d'une plage splicée en supposant qu'elle ne contient plus aucun `PageSourceToken::Block` ; un `Block` rencontré à ce niveau panique (`unreachable!`), précisément parce que l'imbrication est aujourd'hui une précondition violée par construction, jamais un cas à absorber.

---

## 2. La décision à trancher AVANT tout code

C'est le point bloquant réel — toutes les structures de données en dépendent, donc rien ne doit être codé avant que cette question ait une réponse explicite, écrite, validée.

**Le scénario qui force la décision** : le Root déclare un bloc `main_nav` qui contient, par défaut, un sous-bloc `current_tab`. Un maillon intermédiaire de la chaîne `extends` redéfinit `main_nav` en entier (nouvelle structure de navigation). Que devient `current_tab` pour un maillon encore plus dérivé qui voudrait continuer à ne surcharger que l'onglet courant, sans réécrire toute la navigation ?

Deux réponses possibles, non équivalentes, chacune avec des conséquences structurelles différentes :

### Option A — Écrasement complet

Redéfinir un bloc parent efface tous ses sous-blocs. Un maillon qui redéfinit `main_nav` doit redéclarer sa propre structure interne (y compris un éventuel `current_tab`) s'il veut que ses propres descendants puissent encore le surcharger finement.

- Plus simple à raisonner : une redéfinition est un remplacement total, point.
- Plus proche du modèle « le plus proche de la feuille gagne » déjà en place pour les blocs plats (§4.4 du guide) — une simple extension récursive de la même règle.
- Risque pratique : un maillon qui voulait juste changer `current_tab` doit recopier toute la structure de `main_nav` telle que déclarée par le Root pour ne pas la perdre — verbeux si `main_nav` est large.

### Option B — Sous-slots indépendants

Un sous-bloc reste overridable indépendamment de son parent, peu importe qui a redéfini ce dernier. Redéfinir `main_nav` ne « consomme » pas `current_tab` : un maillon plus dérivé peut continuer à ne toucher que `current_tab`, même si un maillon intermédiaire a réécrit tout le reste de `main_nav`.

- Correspond à l'intuition la plus courante venant de Jinja2 (où les blocs, imbriqués ou non, restent chacun individuellement overridables dans toute la chaîne).
- Complexité largement supérieure : la résolution d'un sous-bloc doit chercher sa redéfinition la plus proche de la feuille **indépendamment** de la résolution de son parent — deux recherches croisées, pas une seule passe descendante.
- Cas ambigu à trancher explicitement : si le maillon qui redéfinit `main_nav` ne mentionne pas du tout `current_tab` dans sa propre redéfinition, le sous-bloc du Root doit-il quand même s'insérer quelque part dans le nouveau contenu de `main_nav` ? Où ? La réponse naturelle (« nulle part, il disparaît avec le reste du contenu par défaut ») contredit déjà la promesse de « sous-slot indépendant » de cette option — à netttoyer avant tout code, pas après constat du problème en session.

**Recommandation de ce document, à confirmer, pas à acter unilatéralement** : commencer par l'Option A. Elle prolonge directement la règle déjà en place et déjà comprise (`link_chain` actuel), elle est implémentable sans introduire une double résolution croisée, et le cas d'usage concret cité en ouverture (onglet de navigation courant) ne réclame pas nécessairement l'Option B — un maillon qui change la structure de la navigation changera probablement aussi son onglet courant dans la foulée. L'Option B reste ouverte si un cas d'usage futur la justifie explicitement, mais elle ne doit pas être choisie par défaut pour sa seule familiarité Jinja2 : sa complexité d'implémentation et d'explication (piégeuse à documenter proprement pour un développeur frontend) est significativement plus élevée.

---

## 3. Conséquences structurelles (une fois l'Option A ou B tranchée)

### 3.1 `NamedBlockRange` → une forme arborescente

La plage plate actuelle ne peut plus suffire dès qu'un bloc peut contenir ses propres sous-blocs. Piste, à valider en session : au lieu d'une seule plage `[start, end)`, une structure récursive portant, pour un bloc donné, sa propre plage **et** la liste de ses sous-blocs directs (plages elles-mêmes potentiellement récursives) :

```rust
struct NamedBlockRange<'src> {
    name: &'src str,
    template: TemplateId,
    start: usize,
    end: usize,
    children: Vec<NamedBlockRange<'src>>, // vide aujourd'hui, dans tous les cas
}
```

Point de vigilance DOD à trancher en session : est-ce que `children` doit rester un `Vec` par plage (coût d'allocation par bloc, mais représentation directe), ou est-ce que la platitude doit être préservée autrement (un unique `Vec<NamedBlockRange>` global par template, avec un `parent_index: Option<usize>` par entrée, façon arène) ? La seconde forme est plus proche de l'esprit DOD déjà en place ailleurs dans ce crate (`PageArena`, `TemplateId`) — probablement la piste à privilégier, mais à concevoir explicitement, pas à décider en cours d'implémentation.

### 3.2 `collect_blocks` — la pile existe déjà, il faut l'exploiter plutôt que la rejeter

Ironie de la situation actuelle : `collect_blocks` maintient déjà une pile d'ouverture (`open_stack`) pour détecter l'imbrication — c'est CETTE pile qui produit aujourd'hui `NestedBlock` dès qu'elle est non vide à l'ouverture d'un nouveau bloc. Lever l'interdiction ne demande pas d'ajouter un mécanisme de suivi de la profondeur : il existe déjà. Il demande de changer ce que la fonction fait quand elle détecte un bloc pendant que la pile est non vide — aujourd'hui une erreur accumulée, demain un rattachement du nouveau bloc comme enfant du bloc actuellement au sommet de la pile.

### 3.3 `link_chain` — la résolution doit descendre récursivement

Aujourd'hui : une seule passe sur les blocs plats du Root, cherchant pour chacun la redéfinition la plus proche de la feuille parmi des ensembles eux-mêmes plats. Avec l'imbrication (Option A) : la résolution doit, après avoir choisi la source d'un bloc donné, redescendre dans les enfants de CETTE source (pas dans les enfants du Root) pour résoudre récursivement chaque sous-bloc — le point de départ de la recherche des enfants change à chaque niveau de redéfinition, jamais fixe sur le Root.

`OrphanBlock` doit lui aussi devenir récursif : un sous-bloc orphelin (déclaré par un maillon non-Root sans correspondance dans le sous-arbre — pas seulement dans l'ensemble global — de blocs du Root) doit rester détectable, avec le même niveau de précision de message qu'aujourd'hui (fichier fautif nommé via `TemplateId`).

### 3.4 `lower()` — remplacer le panic par une vraie récursion

`lower_leaf_token` panique aujourd'hui sur tout `Block` rencontré à l'intérieur d'une plage splicée — c'est exactement l'invariant qui change. Il faudra que la projection d'une plage retenue par `LinkPlan` puisse elle-même rencontrer des `Block(BlockOpen)`/`Block(BlockEnd)` et les résoudre à leur tour (même algorithme que le niveau racine, appliqué récursivement) plutôt que de les traiter comme une violation de précondition.

### 3.5 Interaction avec `{% import %}` — un point déjà identifié, pas nouveau

Cette session a déjà noté (voir guide, §4.5bis) que l'imbrication à travers un fragment importé produit aujourd'hui la même erreur `NestedBlock` que si le contenu avait été écrit en dur — cohérent, mais qui cessera d'être une erreur une fois l'imbrication levée. Point à revalider explicitement à ce moment-là : un fragment importé peut-il, une fois développé, introduire un bloc **enfant** d'un bloc déjà ouvert dans le fichier qui l'importe (import positionné à l'intérieur d'un bloc) ? Aujourd'hui `ImportInsideBlock` l'interdit catégoriquement, indépendamment de toute considération d'imbrication de blocs. Il n'y a aucune raison évidente de lever CETTE contrainte-là en même temps — les deux questions sont orthogonales, à traiter comme telles, pas fusionnées par simplicité de session.

---

## 4. Ce que ce document ne tranche pas

- Option A vs Option B (§2) — la décision de fond, à valider en ouverture de la session dédiée, avant toute ligne de code.
- La représentation exacte de l'arborescence (§3.1) — `Vec` récursif vs arène à plat avec `parent_index`.
- Si un bloc profondément imbriqué (au-delà de 2 niveaux) doit lui-même être borné en profondeur, par symétrie avec `MAX_EXTENDS_DEPTH`/`MAX_IMPORT_DEPTH` — aucun cas d'usage connu ne le justifie aujourd'hui, mais le principe « toute récursion non bornée mérite une borne nommée, avec message d'erreur explicite plutôt qu'un `cargo build` qui tourne indéfiniment » s'applique par défaut dans ce projet — à confirmer explicitement plutôt qu'à laisser un oubli.

## 5. Séquencement suggéré pour la session future

1. Trancher Option A vs Option B (§2), par écrit, avant tout code.
2. `NamedBlockRange` récursif (§3.1) — type seul, aucune fonction câblée, à l'image de la méthode déjà suivie pour `PageBlockToken`/`TemplateId` en leur temps.
3. `collect_blocks` — remplacer l'erreur `NestedBlock` par un rattachement réel (§3.2).
4. `link_chain` récursif (§3.3).
5. `lower()` récursif (§3.4).
6. Revalider `{% import %}` (§3.5) — sans en élargir la portée sans décision explicite séparée.
7. Mettre à jour le guide (`fragment-forge-guide.md`, §4.4) une fois le tout compilé et testé — jamais avant, pour ne pas documenter une intention non câblée (cf. l'historique de ce même guide, qui a longtemps décrit la Partie 2 comme « spécifiée, non implémentée »).

---

## Fichiers à fournir pour cette session :

- fragment-forge/src/page/model.rs — NamedBlockRange à faire évoluer
- fragment-forge/src/page/blocks.rs — collect_blocks, la pile d'ouverture déjà là à réutiliser
- fragment-forge/src/page/linker.rs — link_chain, la résolution à rendre récursive
- fragment-forge/src/page/lowering.rs — lower, le unreachable! à remplacer
- fragment-forge/src/page/token.rs — PageSourceToken, pour le contexte
- fragment-forge/src/page/importer.rs — le point d'interaction noté en §3.5 du handoff
- fragment-forge/src/page/mod.rs et fragment-forge/src/lib.rs — exports, pour éviter exactement le genre de trou qu'on vient de corriger deux fois
- crates/core/schema/build/template/page.rs et static_page.rs — les deux orchestrateurs partagent déjà discover_imports/splice_all_imports/etc. ; toute - évolution de NamedBlockRange/link_chain les concerne tous les deux, en même temps, sans exception

---

## Hiérarchie des fichiers :

Nous avons volontairement scopé le `tree` pour l'implémentation en cours, mais nous pourrons élargir la visualisation si besoin.

```
marius/crates/forge/fragment-forge/src$ tree
.
├── fragment
│   ├── codegen.rs
│   ├── lexer.rs
│   ├── mod.rs
│   ├── parser.rs
│   ├── resolver.rs
│   ├── script_hoisting.rs
│   ├── static_markers.rs
│   ├── token.rs
│   └── validator.rs
├── lib.rs
├── naming.rs
├── page
│   ├── blocks.rs
│   ├── importer.rs
│   ├── linker.rs
│   ├── lowering.rs
│   ├── model.rs
│   ├── mod.rs
│   ├── parser.rs
│   └── token.rs
└── schema.rs
```

```
marius/crates/core/schema/build/template$ tree
.
├── common.rs
├── dynamic.rs
├── mod.rs
├── page.rs
└── static_page.rs
```
