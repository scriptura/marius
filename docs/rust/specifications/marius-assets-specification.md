# Spécification — crate `marius-assets`

## 0. Statut de ce document

Ce document fige le périmètre fonctionnel et les invariants architecturaux de `marius-assets`, issus d'une session de conception avec l'auteur du projet Marius. Il ne contient **aucune API, aucune signature de fonction, aucun code**. Il est conçu pour être injecté seul dans une future session d'implémentation et doit suffire à démarrer la conception technique sans reconstituer le raisonnement qui l'a produit.

Un second document, `marius-assets-roadmap.md`, liste les décisions encore ouvertes et les évolutions différées. Les deux documents sont complémentaires : celui-ci décrit ce qui est **acté**, l'autre ce qui reste **à trancher**.

---

## 1. Rôle et positionnement dans le workspace

`marius-assets` est un **compilateur AOT spécialisé de ressources frontend**. Il transforme des sources de thème (CSS, JS, SVG, polices) en artefacts de build statiques, plus un manifeste décrivant leur exposition publique. Il ne s'exécute jamais au runtime.

- **Package Cargo** : `marius-assets`
- **Dossier** : `crates/assets/` (le dossier porte le nom court, le package porte le nom qualifié — convention déjà en usage dans le workspace pour `crates/core/collector`, `crates/core/projection`, `crates/core/schema`)
- **Ce qu'il n'est pas** :
  - **Pas un membre de la Forge.** La Forge est un ensemble d'outils de méta-programmation dont la cible de compilation exclusive est le **Core** — du code Rust généré, ingéré par `rustc`. `marius-assets` ne génère jamais de Rust et ne cible jamais le Core ; il n'y a donc aucune relation entre les deux familles, pas seulement une frontière à documenter.
  - **Pas un membre du Shell.** Le Shell exécute le runtime (réseau, service HTTP). `marius-assets` tourne une seule fois, en amont, avant tout `cargo build` — c'est un antécédent de build, pas un composant runtime.

---

## 2. Philosophie héritée — invariants transverses

Ces principes, déjà en vigueur ailleurs dans Marius, s'appliquent sans exception à `marius-assets` :

- **Compression structurelle et déterminisme** : mêmes entrées ⇒ mêmes sorties, toujours.
- **Séparation stricte source/artefact** : un fichier écrit par un développeur et un fichier produit par un compilateur ne se mélangent jamais dans le même document.
- **Refus de la solution générale** : le crate n'implémente que les transformations dont Marius a un besoin réel constaté (concaténation, substitution, sprite SVG, minification légère, manifeste) — jamais un framework de build généraliste (pas de Webpack/Vite/Gulp en Rust).
- **Grammaires fermées, pas d'AST complet** : là où une analyse de contenu est nécessaire (CSS, JS), le crate reconnaît un vocabulaire fermé et restreint de constructions — jamais un parseur général du langage.
- **Échec strict, jamais de correction silencieuse** : toute donnée invalide ou incohérente interrompt la compilation avec une erreur explicite ; aucun mécanisme ne réécrit ou ne devine une valeur à la place du développeur.

---

## 3. Ce que le crate ne connaît jamais (hors périmètre explicite)

- Il ne sert **jamais** de HTTP — c'est une responsabilité exclusive du Shell.
- Il ignore le Shell lui-même, le Dispatcher adaptatif, le protocole de transport temps réel (SSE, WebSocket ou autre), et la logique de synchronisation Draft/Committed (ADR-002). Ces éléments relèvent de l'architecture du Shell, jamais de la préparation des ressources.
- Il ne connaît **jamais la raison architecturale** de la présence d'une bibliothèque frontend vendored. HTMX est traité exactement comme Leaflet ou Prism : une entrée d'une liste de bibliothèques à préparer, sans savoir qu'elle sert la projection réactive. _Test de validation appliqué : si HTMX était remplacé demain par une autre bibliothèque hypermédia, la responsabilité du crate ne changerait pas — seul l'inventaire changerait._
- Il n'implémente ni ne référence Idiomorph/Morphing DOM (explicitement écarté par ADR-002) — mais cette information n'a même pas à transiter par le crate ; elle relève de la configuration de thème (liste de bibliothèques), pas de son code.
- Il ne fait pas de résolution de modules ES, pas de bundling de dépendances JS, pas d'AST CSS ou JS complet.
- Il n'a aucune notion de cache ou de TTL temporel — la seule invalidation est pilotée par le contenu (§6).

---

## 4. Entrées — configuration du thème

Le développeur décrit exclusivement les **sources** : où se trouvent les styles, les scripts, les icônes, les polices, quelles bibliothèques frontend sont utilisées, quelles options de compilation sont souhaitées. Il ne décrit jamais les fichiers produits.

Ce fichier de configuration est immuable du point de vue du compilateur : `marius-assets` le lit, ne l'écrit ni ne le modifie jamais.

**Format retenu : TOML.** Choisi pour sa platitude forcée — contrairement au YAML (indentation ambiguë) ou au JSON (verbeux, sans commentaires), TOML rend structurellement difficile d'y glisser une hiérarchie profonde ou de la logique conditionnelle. Un `theme.toml` n'exprime que des données immuables, cohérent avec le rôle de fichier source aveugle défini plus haut. Cohérence supplémentaire avec le reste de l'écosystème Cargo/Rust (parseurs matures, `toml` + `serde`).

---

## 5. Sorties — organisation interne du build

Un seul répertoire racine est indiqué par le développeur. L'organisation interne relève entièrement du compilateur — pas de `-o`/`--dir` par sous-outil, à l'inverse du modèle `package.json`.

```
build/
    <theme>/
        styles/
        scripts/
        fonts/
        images/
        sprites/
```

---

## 6. Versionnement des artefacts — adressage par contenu

L'identifiant de version de chaque artefact est dérivé de ses octets (hash rapide, non temporel — BLAKE3 recommandé, algorithme exact laissé à l'implémentation), jamais d'un timestamp ou d'un compteur incrémental.

- **Granularité retenue : hash individuel par artefact**, pas un identifiant global unique pour tout le build. Le compilateur reste stateless (aucun tree-diff, aucun watcher de dépendances) : il régénère systématiquement tous les buffers en mémoire, puis hache chaque buffer final juste avant écriture sur disque. Le coût de hachage d'un buffer typique est négligeable — le gain (granularité de cache navigateur fine, sans aucune logique de cache différentiel à implémenter) est obtenu gratuitement.
- **Bénéfice** : idempotence totale — une exécution répétée du compilateur sur un thème inchangé produit strictement le même manifeste, les mêmes URL.
- **Orthogonalité avec l'invalidation CSS binaire (§10.3)** : la régénération intégrale du pipeline CSS à chaque changement de variable globale porte sur le _coût de calcul_, pas sur l'_identité de sortie_. Un fichier `.mcss` recompilé qui ne référence pas la variable modifiée produit un buffer strictement identique, donc un hash identique, donc aucune invalidation de cache inutile pour ce fichier précis — la granularité fine côté client est un sous-produit gratuit du hash par artefact, pas une contrepartie de complexité supplémentaire côté compilateur.
- **Conséquence pratique** : les URL publiques changent uniquement lorsqu'une recompilation invalide réellement une ressource (`/styles/main.a81f9.css`), permettant un `Cache-Control: immutable` agressif côté Shell sans risque de servir une version obsolète.

---

## 7. Le manifeste de build — contrat central entre production et consommation

Le manifeste n'est pas une simple communication technique : c'est le **dictionnaire de données formel** produit par `marius-assets`, immuable, intégralement régénéré à chaque compilation (jamais patché incrémentalement).

**Schéma des entrées attendues** (par artefact) :

- URL publique
- Chemin physique
- Type MIME
- Taille
- Hash / ETag
- Version
- Métadonnées libres éventuelles

**Deux consommateurs, à deux moments distincts du pipeline global** :

| Consommateur | Moment                 | Usage                                                                                                                                                                 |
| ------------ | ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Forge**    | À la compilation (AOT) | Table de résolution O(1) pour abaisser les tokens `{% asset id %}` des templates `.marius` en chemins de fichiers versionnés, gravés en dur dans le code Rust généré. |
| **Shell**    | Au runtime             | Représentation statique (idéalement injectée dans le binaire) pour servir chaque fichier avec les bons en-têtes HTTP (MIME, ETag, taille), sans coût de parsing.      |

`marius-assets` écrit ce fichier et ignore tout de ses lecteurs — la lecture est enti��rement la responsabilité de chaque consommateur.

---

## 8. Topologie de pipeline — producteur unique en amont

**Interdiction stricte** : `marius-assets` ne doit jamais être invoqué indépendamment par les `build.rs` respectifs de la Forge et du Shell. Deux invocations concurrentes du même compilateur introduiraient un état mutable caché et casseraient le déterminisme (risque concret : deux manifestes divergents pour un même thème, donc des URL gravées en Forge ne correspondant plus à celles connues du Shell).

**Modèle retenu** :

- **Phase émettrice (amont)** : `marius-assets` s'exécute une seule fois, comme étape racine explicite de l'orchestrateur du workspace. Il produit le manifeste unique et immuable.
- **Phase réceptrice (compilation)** : les `build.rs` de la Forge et du Shell traitent ce manifeste comme un invariant en entrée brute, jamais régénéré par eux. Dépendance déclarée via `cargo:rerun-if-changed` sur le fichier manifeste ; intégration par inclusion statique directe (`include_str!`), sans allocation ni parsing au runtime.

Ce modèle est le même précédent que celui déjà en place pour `core/schema` et son propre `build.rs`, étendu à un producteur externe.

---

## 9. Syntaxe consommateur côté templates `.marius`

```html
<link rel="stylesheet" href="{% asset main.css %}" media="screen" />
<link
  rel="preload"
  href="{% asset notoSans-Regular.woff2 %}"
  as="font"
  type="font/woff2"
  crossorigin=""
/>
```

- Le token de chemin s'écrit **sans guillemets** — cohérent avec l'invariant du scanner `.marius` : aucun token de littéral de chaîne n'existe dans la grammaire, un guillemet serait capturé tel quel comme partie du chemin.
- Le développeur écrit la balise HTML de façon exhaustive ; l'identifiant logique (`main.css`) est garanti stable par le développeur, pas par le compilateur.
- La Forge, lors de l'abaissement AOT, substitue strictement le token par l'URL versionnée lue dans le manifeste.
- Le binaire final ne contient qu'un `push_str` contigu avec le chemin final gravé en dur — zéro indirection, zéro allocation au runtime.
- **Échec dur** : si l'identifiant ne correspond à aucune clé du manifeste, la Forge lève une erreur fatale de compilation (`AssetNotFound`). Aucun fallback, aucune résolution dynamique au runtime.

---

## 10. Périmètre fonctionnel par famille d'assets

### 10.1 Fonts

- **Transformation** : copie simple, aucune modification de contenu.
- **Validation croisée obligatoire** : le build CSS échoue explicitement si une police déclarée dans une règle `@font-face` ne correspond à aucune police effectivement copiée par le pipeline Fonts.
- **Conséquence d'ordonnancement** : le pipeline Fonts doit avoir résolu son registre de polices disponibles avant que la validation CSS ne s'exécute.
- **Le registre Fonts sert aussi de source de résolution d'URL, pas seulement de validation.** Puisque chaque police porte un hash de contenu individuel (§6), le chemin `url(...)` écrit par le développeur dans `@font-face` (référence logique/relative au thème) doit être réécrit par le compilateur vers l'URL publique versionnée finale avant écriture du CSS de sortie — sans quoi le navigateur demanderait un chemin non versionné, inexistant dans le manifeste. C'est la même consultation de registre que la validation, avec un rôle de substitution en plus, pas un mécanisme distinct.

### 10.2 SVG

- **Nettoyage** via SVGO.
- **Compilation** en un sprite unique exposant des éléments `<symbol>`.
- **Convention de nommage** : l'`id` de chaque symbole est déduit du nom de fichier source.
- **Échec dur** sur toute collision d'`id` entre deux fichiers — jamais de réécriture ou de renommage automatique silencieux.

### 10.3 CSS — extension `.mcss`

**Rationale du choix de syntaxe de variables** _(à documenter explicitement dans le code/commentaires, décision volontairement tracée)_ :

CSS natif interdit structurellement l'usage de `var()` dans une condition `@media` — ce n'est pas une lacune temporaire du langage mais un rejet architectural du CSS Working Group, lié à un risque de dépendance circulaire (les media queries ne sont pas attachées au DOM, alors que les custom properties se résolvent par héritage sur l'arbre DOM ; il n'existe rien à quoi rattacher une résolution en condition de media query). Singer une syntaxe `var()`-like qui ne franchira jamais cette limite native aurait été une erreur de conception. Marius adopte donc sa **propre syntaxe de substitution**, résolue entièrement au moment de la compilation (le token disparaît, remplacé par sa valeur littérale) — ce qui a l'avantage de rendre la valeur utilisable _partout_, y compris en condition `@media`, puisque la substitution a lieu avant que le navigateur ne voie le fichier.

- **Syntaxe retenue** : `$variable` (et non `_variable`). Justification : la grammaire Sass/SCSS, déjà reconnue nativement par la quasi-totalité des éditeurs de code, colore les tokens `$identifiant` comme des variables sans configuration additionnelle. Aucune grammaire existante (CSS, Sass, Less) n'accorde de sens particulier à un préfixe `_`, qui serait donc affiché comme texte ordinaire. Risque assumé et documenté : `$variable` évoque visuellement Sass et peut créer une attente erronée de fonctionnalités Sass absentes de `.mcss` (nesting via `&`, mixins, boucles) — un principe de moindre surprise (POLA) déjà appliqué ailleurs aux templates `.marius`, à tracer de la même façon ici.
- **Coexistence assumée avec les `--custom-properties` natives** : `$variable` (résolution compile-time) et `var()`/`--custom-property` (résolution native navigateur) ne se recouvrent jamais syntaxiquement — le résolveur de `marius-assets` ne traite que ses propres tokens `$`, laissant `var()` intact s'il apparaît. Ce cas reste théorique pour Marius : chaque variation de thème déclenche un nouveau build AOT complet, il n'y a pas de dynamisme runtime prévu au niveau des styles.
- **Résolution de graphe d'imports — pas une concaténation naïve.** Le fichier `main.css` d'exemple utilise `@layer` natif pour piloter l'ordre de cascade, indépendamment de l'ordre physique des fichiers. Le concaténateur doit donc traiter trois at-rules comme citoyens de première classe (grammaire fermée, pas d'AST complet) :
  1. Préserver telle quelle la déclaration d'ordre `@layer tokens, base, layout, ...;` en tête de sortie.
  2. Pour `@import 'x.css' layer(nom);` : inliner le contenu de `x.css`, enveloppé dans `@layer nom { … }`.
  3. Pour `@import 'x.css';` (sans `layer(...)`, ex. fichier de variables globales) : inliner tel quel, sans enveloppe.
- **Minification** : légère, non agressive ; collapse des espaces blancs.
- **Support IDE** : l'extension `.mcss` doit tirer parti d'une coloration syntaxique CSS native, ou à défaut Sass, sans outillage custom.
- **Invalidation de cache de build — granularité binaire (v1)** : toute modification du fichier de variables globales (`$variable`) invalide et recompile l'intégralité du pipeline CSS, sans suivi fin de dépendance par fichier (« quel `.mcss` importe quelle variable »). Cohérent avec la régénération intégrale déjà pratiquée ailleurs dans Marius ; une granularité plus fine coûterait trop cher en complexité pour une v1 et n'a pas été demandée.

### 10.4 JavaScript — deux sous-familles de nature distincte

#### 10.4.1 Scripts composants (ex. cibles `main.js`, `more.js`)

- **Contrat d'entrée** : un dossier source correspond exactement à une cible de compilation (`development/main/` → `main.js`), auto-descriptif, sans fichier de config redondant.
- **Ordre de concaténation** : convention de préfixe `_` (`_base.js` trié en premier, l'ASCII `_` précédant les minuscules). _Limite connue, non résolue : ce mécanisme ne généralise pas si plusieurs fichiers ont simultanément besoin d'une contrainte d'ordre précoce — voir Roadmap._
- **Transformation** : concaténation pure dans l'ordre résolu ; pas de résolution de modules ES, pas de bundler.
- **Invariant vérifiable (fail-fast)** : chaque fichier ne doit exposer qu'**une seule liaison top-level, nommée de façon unique** (`const NOM = …` ou `function NOM`). Détection par **lexer** (analyse lexicale à un seul passage), pas par scan textuel naïf (regex/sous-chaîne) et pas par AST complet :
  - Classification de tokens minimale : Commentaire, Chaîne (y compris template literal `` ` `` ), Regex, Mot-clé, Identifiant, Ponctuation — un scan textuel brut produirait des faux positifs sur les occurrences de `const NOM =` à l'intérieur d'un commentaire ou d'une chaîne.
  - **"Top-level" est déterminé par un compteur de profondeur d'accolades** (`{` incrémente, `}` décrémente), jamais par l'indentation — le JavaScript n'impose aucune contrainte d'indentation, ce critère serait systématiquement contournable.
  - Deux pièges de lexing à couvrir explicitement : la distinction `/` opérateur de division vs délimiteur de littéral regex, et la non-comptabilisation des accolades internes à un template literal (`` `${ }` ``) dans le compteur de profondeur.
  - Séquence recherchée : `[Mot-clé(const|let|var|function), Identifiant(NOM)]` à profondeur zéro. Le compilateur enregistre chaque identifiant dans un ensemble ; toute deuxième insertion du même nom au sein d'une même cible de compilation déclenche l'échec fatal.
- **Validation candidate, à confirmer pour la v1** : les fichiers JS composants peuvent contenir des chemins publics littéraux (ex. `/scripts/more.js`, `/libraries/leaflet/leaflet.js`, `/sprites/util.svg#icon`) référençant d'autres artefacts. Même rationale que la validation Fonts↔CSS : un scan des chaînes littérales correspondant à des préfixes publics connus (`/scripts/`, `/styles/`, `/sprites/`, `/libraries/`, `/fonts/`, `/images/`) pourrait échouer si la référence ne correspond à aucune entrée du manifeste — toujours un vocabulaire fermé, pas de parsing JS général.

#### 10.4.2 Bibliothèques vendored (Leaflet, Prism, HTMX + extensions, etc.)

- **Traitement** : strictement identique au pipeline Fonts — copie verbatim, aucune transformation, jamais concaténées avec les scripts composants.
- **Portée de la connaissance du crate** : `marius-assets` prépare la liste de bibliothèques déclarée dans la configuration du thème, sans jamais connaître pourquoi elles sont présentes (ex. HTMX sert la projection réactive du Shell — information qui ne franchit jamais la frontière du crate).
- **HTMX 4 (bêta)** retenu comme version cible pour le projet, malgré son statut non stabilisé, en cohérence avec le choix assumé par l'auteur du projet.
- _(Contrat exact de déclaration — nom/version/sous-liste d'extensions — non tranché : voir Roadmap.)_

---

## 11. Invariants de validation transverses (résumé)

| Famille                                 | Condition d'échec                                                   | Comportement                                   |
| --------------------------------------- | ------------------------------------------------------------------- | ---------------------------------------------- |
| SVG                                     | Collision d'`id` entre symboles                                     | Erreur fatale, jamais de renommage automatique |
| Fonts / CSS                             | Police référencée en `@font-face` absente du registre Fonts         | Erreur fatale de build CSS                     |
| Forge / Asset                           | `{% asset id %}` sans correspondance dans le manifeste              | Erreur fatale `AssetNotFound` à la compilation |
| JS composants _(candidat, à confirmer)_ | Collision de liaison top-level entre deux fichiers d'une même cible | Erreur fatale de build                         |
| JS composants _(candidat, à confirmer)_ | Référence de chemin public littéral absente du manifeste            | Erreur fatale de build                         |

Principe commun : **aucun de ces cas ne doit produire un artefact silencieusement dégradé ou corrigé automatiquement** — l'erreur doit toujours remonter à sa source, jamais être masquée par une résolution de repli.

---

## 12. Table de traçabilité des décisions majeures

| Décision                                                                                     | Raison                                                                                                                                                         |
| -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/assets/` (dossier) / `marius-assets` (package)                                       | Précédent déjà établi par `core/collector`, `core/projection`, `core/schema`                                                                                   |
| Hors Forge, hors Shell                                                                       | Test de substitution : aucune dépendance réelle dans un sens ni dans l'autre                                                                                   |
| Manifeste = contrat, pas juste communication                                                 | Deux consommateurs à deux moments distincts (Forge/compilation, Shell/runtime) exigent une frontière de données formelle, pas une convention implicite         |
| Producteur unique en amont                                                                   | Élimine le risque de désynchronisation d'identifiants entre invocations concurrentes                                                                           |
| Hash de contenu plutôt que temporel                                                          | Idempotence : mêmes entrées ⇒ mêmes sorties, cohérent avec l'éthique AOT du reste de Marius                                                                    |
| `$variable` plutôt que `_variable`                                                           | Support de coloration syntaxique existant (grammaire Sass), sans outillage custom                                                                              |
| Résolution de graphe `@import`/`@layer` plutôt que concaténation naïve                       | La cascade CSS native dépend de l'ordre des couches déclarées, pas de l'ordre physique des fichiers                                                            |
| HTMX traité comme bibliothèque vendored ordinaire                                            | Le crate ne doit jamais connaître la raison architecturale d'une dépendance frontend (test de substitution)                                                    |
| `{% asset id %}` sans guillemets                                                             | Cohérence avec le scanner `.marius`, qui ne connaît aucun token de littéral de chaîne                                                                          |
| Détection de collision JS par lexer (profondeur d'accolades), pas par scan textuel           | Un scan textuel brut produit des faux positifs sur commentaires/chaînes ; l'indentation n'est pas un critère fiable en JS                                      |
| Invalidation CSS binaire (tout changement de `$variable` globales invalide tout le pipeline) | Cohérent avec la régénération intégrale déjà pratiquée ailleurs dans Marius ; une granularité fine coûterait trop cher pour la v1                              |
| TOML pour la configuration de thème                                                          | Platitude forcée du format — empêche structurellement d'y glisser une hiérarchie profonde ou de la logique conditionnelle                                      |
| Hash individuel par artefact plutôt que global                                               | Compilateur stateless (aucun diff/watcher), coût de hachage négligeable, granularité de cache navigateur obtenue gratuitement en sous-produit                  |
| Registre Fonts étendu en résolveur d'URL (pas seulement validateur)                          | Le hash par artefact rend obsolète toute référence relative écrite par le développeur dans `@font-face` ; la même consultation de registre sert les deux rôles |
