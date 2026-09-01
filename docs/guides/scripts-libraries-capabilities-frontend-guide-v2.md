# Guide — Scripts, bibliothèques vendorées et capacités frontend conditionnelles

Références :
- `HANDOFF-js-deps-capacites-frontend-v2.md` — implémentation d'origine du
  système de capacités conditionnelles (§6 de ce guide).
- `SPEC-canonical-asset-identity.md` — implémentation d'origine de
  `CanonicalAssetId` et des bibliothèques vendorées (§3 de ce guide).
- `runtime-lifecycle-guide.md` — modèle du cycle build/runtime (AOT vs
  réactif PostgreSQL) dont dépend directement tout ce qui touche à la
  branche dynamique des capacités (§6.2.2, §7, §8 de ce guide). À lire en
  cas de doute sur « pourquoi mon changement n'apparaît pas alors que le
  build est vert ».

Ce guide en est la version d'usage courant — pas d'historique de décisions,
juste : comprendre le système, l'étendre, le compiler.

> **Deux noms proches, deux mécanismes disjoints — à ne jamais confondre.**
> `deps` (`[scripts.capabilities.*].deps`, §4 de ce guide) est une
> dépendance de **chargement de script**, décidée une fois pour toutes dans
> `theme.toml`, jamais liée au contenu d'une page précise. `js_deps`
> (`content.core.js_deps`, §6 de ce guide) est un **bitset SQL** décrivant
> quelles capacités un enregistrement précis doit activer. Les deux se
> croisent (une dépendance `deps` d'une capacité dynamique hérite de la
> même conditionnalité que le bit `js_deps` de cette capacité, §4.4), mais
> ce sont deux données, deux mécanismes, deux moments de résolution
> entièrement différents.

## 1. Vue d'ensemble

Quatre besoins distincts, trois sections de `theme.toml`, un seul
compilateur (`marius-assets`) pour tout résoudre :

| Besoin | Section `theme.toml` | Condition de chargement |
|---|---|---|
| Script chargé sur des pages précises, décidé au moment d'écrire le template | `[scripts.components]` | Aucune — inclusion explicite via `{% asset %}` dans un `.marius` |
| Script chargé selon un marqueur structurel (`.marius`) ou éditorial (`content.body`) | `[scripts.capabilities.*]` | Bit `js_deps` (dynamique) ou détection statique au build |
| Code tiers vendoré, importé **littéralement** par le texte ESM d'un script | `[libraries.*]` (`module = true`, défaut) | Jamais seul — un `import` écrit à la main dans le script consommateur (§3.2) |
| Code tiers vendoré dont le **chargement** (pas l'import) doit précéder une capacité — notamment tout UMD/classique | `[libraries.*]` (`module = false` explicite) + `deps` sur la capacité consommatrice | Balise `<script>` distincte, injectée avant celle de la capacité — **jamais** un `import` (§4) |

Une bibliothèque n'est jamais un point d'entrée en elle-même : elle existe
pour être **consommée** par un component ou une capacité, de deux façons
orthogonales, choisies selon son format :

- **Import ESM direct** (§3.2) — la bibliothèque expose une vraie syntaxe
  de module (`export`), `module = true` (défaut) suffit, on l'importe comme
  n'importe quel autre module.
- **`deps`** (§4) — la bibliothèque est UMD/classique (`module = false`
  explicite) et expose son API via un global (`window.X`), ou on veut
  simplement garantir son chargement avant une capacité sans l'importer
  nommément. **Disponible uniquement sur `[scripts.capabilities.*]`** —
  aucun mécanisme équivalent pour un component à ce jour (limite actuelle,
  §9).

Si le besoin est conditionné par le contenu ou par le template, allez
directement en §6. Script sans condition, §2. Code tiers à vendorer, §3.
Garantir le chargement d'un script tiers avant une capacité, §4.

**`[static.verbatim]` n'est jamais une option pour du JavaScript** —
`[scripts.components]` et `[scripts.capabilities.*]` sont les deux seules
voies légitimes, décision actée en détail en §10.1.

## 2. Scripts inconditionnels — `[scripts.components]`

Dans `assets/default/theme.toml` :

```toml
[scripts.components]
main = "scripts/main/index.js"
```

- Clé (`main`) = nom logique = clé de manifeste `main.js` (voir §9 pour une
  asymétrie de convention à connaître sur cette clé).
- Aucun `markers`, aucune `activation`, aucun bit `scripts_registry.lock` —
  rien à synchroniser côté SQL, rien à câbler dans `compute_js_deps`.
- **Aucun `deps`** — ce champ n'existe que sur `[scripts.capabilities.*]`
  (`CapabilityConfig`) ; `[scripts.components]` reste une simple table
  `nom → chemin`, sans configuration additionnelle possible à ce jour.
- Résolu par le **même pipeline ESM** que les capacités
  (`build_module_arena` → tri topologique → `patch_and_hash_modules`) :
  les imports relatifs entre fichiers d'un même composant sont suivis et
  chaque module — y compris un module intermédiaire jamais nommé
  directement dans `theme.toml` (ex. `navigation.js` importé par
  `index.js`) — reçoit sa propre entrée de manifeste, sous son chemin
  canonique complet (`scripts/main/navigation.js`), jamais son seul nom de
  fichier.
- Un component peut importer une bibliothèque ESM vendorée exactement comme
  une capacité (§3.2) — le mécanisme de résolution est identique,
  indépendant de la section d'origine du point d'entrée.

Un component, `[scripts.components]`, n'est pas orchestré automatiquement
dans un `.marius` mais géré manuellement. La forme exacte de la balise
(ex : `<script type="module" src="`{% asset scripts/main.js %}`">`,
`<script src="`{% asset scripts/main.js %}`" defer>`, etc) est à la
discrétion du développeur frontend.

**Quand l'utiliser :** un script dont le besoin est connu à l'écriture du
template, indépendamment du contenu de la page — navigation globale,
recherche, tout ce qui n'a pas de sens à conditionner.

## 3. Bibliothèques vendorées — `[libraries.*]`

### 3.1 Déclarer et vendorer

```toml
[libraries.deckgl]
root = "libraries/deckgl"
module = false   # défaut : true — voir ci-dessous
```

- `root` autorise la découverte récursive de **tout** le sous-arbre
  `assets/default/libraries/deckgl/` — déterministe (chemins triés),
  aucune distinction de type de fichier (JS, CSS, images, `.map`, fonts...).
- Chaque fichier découvert est copié **tel quel** (même pipeline que
  `[static.verbatim]`, zéro transformation de contenu) et haché
  individuellement. Sa clé de registre/manifeste est son chemin canonique
  complet sous la racine du thème (`libraries/deckgl/deckgl.js`), jamais
  son seul nom de fichier — deux bibliothèques, ou une bibliothèque et le
  thème, peuvent contenir un fichier de même nom sans jamais s'écraser
  l'une l'autre.
- Le nom logique (`deckgl` dans `[libraries.deckgl]`) est une étiquette de
  configuration uniquement — il n'apparaît dans aucune identité d'asset ni
  aucun préfixe d'URL au-delà du chemin physique réel sous `root`.
- **`module` — Marius est ESM-first.** Défaut `true` (omis) : la
  bibliothèque est traitée comme un module ES partout où elle est
  consommée via `deps` (§4.3 — sans effet sur un import direct, §3.2, qui
  ne consulte jamais ce champ). `module = false` est une concession
  **explicite**, jamais une supposition, réservée aux bibliothèques qui
  restent distribuées en UMD/classique et exposent leur API via un global
  (`window.X`). C'est une propriété de la bibliothèque elle-même — la même
  bibliothèque produit toujours le même type de balise, quelle que soit la
  capacité qui la consomme via `deps`.

Procédure : déposer les fichiers réels de la bibliothèque, tels que fournis
par l'éditeur tiers, sous `assets/default/libraries/<nom>/` (aucun
build/bundling supplémentaire n'est effectué par ce mécanisme), déclarer
`root` (et `module = false` si nécessaire) dans `theme.toml`, relancer
l'Étape 1 (§7).

### 3.2 Consommer une bibliothèque ESM par import direct

Réservé aux bibliothèques `module = true` (défaut) qui exposent une vraie
syntaxe de module. Depuis n'importe quel `[scripts.components]` ou
`[scripts.capabilities.*]` :

```js
import { IconLayer } from "libraries/deckgl/deckgl.js";
```

Le slash de tête est optionnel et sans effet — `"/libraries/deckgl/deckgl.js"`
résout à l'identique.

- **Convention impérative :** chemin relatif à la **racine du thème**,
  jamais relatif au fichier qui importe. Un import commençant par `./` ou
  `../` désigne toujours un autre module du **même composant/capacité**
  (résolu, haché individuellement, chaîné dans le graphe ESM) — jamais une
  bibliothèque, même si un fichier de même nom existe sous `libraries/`.
- Résolu au build contre le registre déjà peuplé par
  `[libraries.*]`/`[static.verbatim]` — qui s'exécute systématiquement
  avant tout script (§7, Étape 1) — et réécrit vers l'URL hachée réelle
  (`/libraries/deckgl.<hash>.js`).
- Absence de la clé dans le registre (bibliothèque non déclarée, `root`
  incorrect, faute de frappe dans le chemin) : **échec de build immédiat**
  (`AssetNotFound`), jamais un 404 silencieux découvert seulement au
  runtime.
- **Rien ne vérifie ici que la bibliothèque importée est bien `module =
  true`.** Importer littéralement une bibliothèque `module = false` compile
  sans erreur (la résolution ne consulte jamais ce champ pour un `import`
  direct) mais échoue au runtime navigateur — un bundle UMD n'est pas un
  module ES valide. Pour une bibliothèque `module = false`, utiliser `deps`
  (§4), jamais `import`. Voir piège correspondant en §9.

### 3.3 Limite connue — les références internes à une bibliothèque ne sont pas réécrites

`[libraries.*]` copie chaque fichier octet pour octet — c'est son invariant
fondateur (« zéro transformation »), identique à `[static.verbatim]`. Si un
fichier vendoré importe **lui-même** un autre fichier de la même
bibliothèque (`import './deckgl-worker.js'` à l'intérieur de `deckgl.js`),
cette référence n'est **jamais** réécrite : elle traverse le pipeline
inchangée.

Seuls les scripts `[scripts.components]`/`[scripts.capabilities.*]` (le
graphe ESM propre au thème) bénéficient aujourd'hui de la réécriture
d'imports — pas les fichiers verbatim d'une bibliothèque entre eux. Avant
de vendorer une bibliothèque multi-fichiers dont les fichiers se
référencent mutuellement, vérifier qu'elle le fait par un mécanisme qui ne
dépend pas d'un chemin résolu au build de ce projet (bundle mono-fichier
fourni par l'éditeur tiers, résolution interne propre à la bibliothèque à
l'exécution, etc.) — sinon le fichier cassera au chargement malgré un
build entièrement vert.

## 4. `deps` — dépendances de chargement d'une capacité

### 4.1 Le contrat exact

`deps` est une liste de scripts dont le **chargement** doit précéder
l'activation d'une capacité — **jamais** un mécanisme d'import ES6 à
injecter dans le code source de `entry`. Disponible **uniquement** sur
`[scripts.capabilities.*]` (§2 : pas de `deps` sur `[scripts.components]`).

```toml
[scripts.capabilities.map]
entry = "scripts/map.js"
markers = ["map"]
activation = "bootstrap"
deps = ["libraries/deckgl/deckgl.js"]
```

Le script référencé dans `deps` n'est **jamais** importé littéralement dans
`entry` — `map.js` reste totalement ignorant de ce mécanisme, il n'a rien à
écrire de particulier pour en bénéficier. Pour du code UMD/classique (le
cas d'usage d'origine, Deck.gl), qui expose son API via un global
(`window.deck`), c'est la **seule** façon de le charger correctement : un
`import` ESM d'un bundle UMD échouerait à l'exécution — ce n'est pas une
limite de Marius, mais du format UMD lui-même (§3.2). Le script consommateur
accède directement au global (`window.deck.DeckGL`, etc.) sans jamais
écrire d'`import` pour cette dépendance précise.

### 4.2 Résolution AOT

Chaque entrée de `deps` est résolue au build, avec la même rigueur que tout
le reste de ce pipeline : chemin canonique (relatif à la racine du thème,
slash de tête optionnel et sans effet — même convention que §3.2) →
recherche dans `manifest.toml` → URL hachée, ou **échec dur** (le build de
`crates/core/schema` échoue, jamais `marius-assets` — voir §4.5) si la clé
est absente. Jamais un repli silencieux, jamais un 404 découvert seulement
au runtime — même discipline que la résolution d'import direct déjà en
place (§3.2).

### 4.3 `module` — ESM ou classique, décidé par la bibliothèque

Le mode de chargement d'une dépendance suit exactement le `module` déclaré
sur sa bibliothèque d'origine (§3.1) :

- `module = true` (défaut) → `<script type="module" src="...">`.
- `module = false` (explicite) → `<script src="..." defer>`.

Cette information ne transite **jamais** par une relecture de `theme.toml`
côté `crates/core/schema/build.rs` (qui ne lit jamais `[libraries.*]`) —
elle passe par une entrée dédiée et disjointe du manifeste,
`classic_scripts` : une liste **sparse** des seules clés canoniques
explicitement classiques (`module = false`) ; l'absence d'une clé dans
cette liste signifie module, comportement par défaut. `AssetEntry` (la
structure qui décrit un artefact produit — CSS, image, JS, peu importe)
**ne porte jamais** cette information : ce n'est pas une propriété de
l'artefact produit, mais une métadonnée propre au mécanisme de chargement
de `deps` — une version antérieure de ce projet avait fait cette confusion
(`module` porté par `AssetEntry` lui-même), provoquant un échec de
désérialisation sur toute entrée non-JS (`styles/print.css`, `images/
logo.svg`, ...) qui n'avait évidemment aucune raison de porter ce champ ;
corrigé en séparant `classic_scripts` de `assets` au niveau du manifeste.

### 4.4 Ordre de chargement, dédoublonnage, conditionnalité

**Ordre** — garanti, toujours : les dépendances d'abord, dans l'ordre
déclaré, puis le `<script type="module">` de `entry`, qui exécute enfin
`activation` :

```html
<script src="/libraries/deckgl.HASH.js" defer></script>
<script type="module">import{bootstrap as _0}from"/scripts/map.HASH.js";_0();</script>
```

Chaque dépendance est sa propre balise `<script>` de premier niveau —
jamais fusionnée dans le `<script type="module">` de la capacité qui la
consomme (à ne pas confondre avec le regroupement import/appel **entre
plusieurs capacités**, §6.2.4 — ce regroupement-là ne concerne jamais
`deps`).

**Dédoublonnage** — une même dépendance déclarée par plusieurs capacités du
même template ne produit **jamais** plus d'une balise. Le dédoublonnage
porte sur l'**identité canonique** de la dépendance (sa clé, ex.
`"libraries/deckgl/deckgl.js"`), **jamais** sur l'URL hachée finale.

**Conditionnalité** — une dépendance hérite de la conditionnalité agrégée
de **toutes** ses capacités consommatrices sur ce template :
- si l'une d'elles est **statique** (§6.2.1), la dépendance devient
  **inconditionnelle** — même si d'autres capacités dynamiques la
  partagent aussi (domination statique, exactement la même règle que pour
  le `<script type="module">` d'une capacité elle-même) ;
- si **toutes** les capacités consommatrices sont dynamiques, la
  dépendance est chargée sous la condition « OU binaire de tous leurs
  bits » — jamais une balise par bit.

Exemple : `map` (bit 2) et `terrain` (bit 4) déclarent toutes deux
`deps = ["libraries/deckgl/deckgl.js"]` → une seule balise, sous la
condition `if record.js_deps & 6 != 0`.

### 4.5 Ce que `deps` ne fait jamais

- Ne modifie **jamais** le texte source de `entry` — aucune injection,
  aucune réécriture d'`import`. Le script reste exactement ce que son
  auteur a écrit.
- N'est **jamais résolu par `marius-assets`** lui-même. La résolution AOT
  complète (§4.2) est entièrement à la charge de
  `crates/core/schema/build.rs`, via sa propre lecture indépendante de
  `theme.toml` (même principe que `markers`/`activation`, jamais partagés
  entre les deux crates). `marius-assets` n'a besoin de rien connaître de
  `deps` : il lui suffit de produire un `manifest.toml` correct (`assets`
  + `classic_scripts`, §4.3).
- N'existe **pas** pour `[scripts.components]` — seule
  `[scripts.capabilities.*]` porte ce champ à ce jour (§2).
- Ne bénéficie **jamais** à un fichier `[static.verbatim]` qui ne serait
  pas passé par `[libraries.*]` de la façon attendue : `classic_scripts`
  n'est alimentée QUE par les bibliothèques explicitement `module = false`.
  Un `.js` posé directement sous `[static.verbatim].files` sans jamais
  passer par `[libraries.*]` serait techniquement résolvable via `deps` (il
  atterrit dans le même registre) mais toujours comme `module: true` par
  défaut, sans aucun moyen de le déclarer classique — voir la conclusion
  architecturale en §10.1.

## 5. Grammaire des imports reconnue

S'applique **uniquement** à l'import ESM direct (§3.2) — jamais à `deps`
(§4), qui ne touche jamais le texte source d'un script. Concerne tout
script traité comme point d'entrée ou dépendance ESM
(`[scripts.components]`/`[scripts.capabilities.*]`) — **jamais** aux
fichiers d'une bibliothèque verbatim eux-mêmes (§3.3). Le lexer est
volontairement borné (aucun AST JS complet) : il reconnaît une grammaire
fermée, documentée, pas « n'importe quel JavaScript valide ».

Reconnu, y compris multi-lignes (imposé par certains formatters, ex.
Biome, dès qu'une clause `{ ... }` dépasse une longueur seuil) :

```js
import Default from './x.js';
import { A, B } from './x.js';
import Default, { A, B } from './x.js';
import {
	A,
	B,
} from './x.js';
```

**Non reconnu — n'échoue pas, mais le specifier n'est jamais réécrit**
(piège potentiel, cf. §9) :

```js
import('./x.js');       // import() dynamique
import './x.js';        // effet de bord seul, sans `from`
```

Un chemin composé dans un gabarit (`` `${base}/x.js` ``) n'est jamais
détecté non plus — un gabarit est traité comme une région opaque de bout
en bout.

## 6. Capacités frontend conditionnelles (`js_deps`)

### 6.1 Le problème que ça résout

Certains contenus ont besoin d'un module JS spécifique côté client — une
vidéo YouTube embarquée, un slider `range`, une image avec point de focus.
Charger ces modules **sur toutes les pages**, qu'elles en aient besoin ou
non, gaspille de la bande passante. Les charger **au cas par cas côté
client** (détection DOM, `IntersectionObserver` sur des sélecteurs) ajoute
une passe JS avant que quoi que ce soit ne s'exécute.

Le système résout ça en amont, à la compilation et à l'écriture, jamais au
runtime. Mais il y a en réalité **deux besoins bien distincts**, souvent
confondus au premier abord :

1. **Besoin structurel, connu à la compilation** — un `.marius` (layout ou
   composant) porte un marqueur `class` **en dur, dans son propre HTML**
   (`<pre class="add-line-marks">` écrit par le développeur du template).
   Ce besoin est vrai pour **tous** les enregistrements rendus par ce
   template, sans exception — Forge le sait dès le build, avant même
   qu'aucune donnée n'existe.
2. **Besoin éditorial, connu uniquement à l'écriture** — un éditeur écrit
   un article dont le corps contient `class="video-youtube"`. Ce besoin
   **varie d'un enregistrement à l'autre** (l'article A embarque une vidéo,
   l'article B non) — seul PostgreSQL, à l'écriture du corps, peut le
   savoir.

Le compilateur AOT (`db-forge`) traite les deux, mais **jamais par le même
mécanisme** : le premier par un scan statique du template lui-même
(constant folding — zéro test au runtime), le second par un bitset
(`js_deps`) calculé une fois à l'écriture et testé au rendu. Les deux
convergent au même endroit dans le HTML final (`<!-- MARIUS_MODULES -->`),
sans jamais se mélanger en amont.

Contrairement à un component (§2), une capacité est **toujours** un point
d'entrée ESM candidat à un test conditionnel — jamais chargée
inconditionnellement sur toutes les pages qui incluent son layout, sauf
dans le cas particulier où son marqueur est détecté statiquement (§6.2.1).

### 6.2 Suivre la donnée — vue d'ensemble

Deux chemins, indépendants, qui convergent au même endroit du HTML final.

#### 6.2.1 Branche statique — détection AOT dans les templates

```
┌───────────────────────────────┐
│  Un .marius porte un          │   <pre class="add-line-marks">
│  marqueur en dur dans son     │   écrit directement dans le template,
│  propre HTML                  │   jamais dans un {{ champ }}
└──────────┬────────────────────┘
           │ lu au build (cargo build), sur le flux DÉJÀ FUSIONNÉ
           │ parent+enfant (post-lower(), avant splice MARIUS_MODULES)
           ▼
┌───────────────────────────────┐
│ extract_static_class_tokens() │   fragment-forge — scanne UNIQUEMENT les
│ (fragment-forge)              │   FlatPageToken::Static, jamais Field/IfBool
└──────────┬────────────────────┘
           ▼
┌───────────────────────────────┐
│ lower_modules_for_template()  │   Marqueur trouvé dans ce template →
│ (build.rs)                    │   émission INCONDITIONNELLE (constant
│                               │   folding : Forge sait déjà, à la
│                               │   compilation, que CE template en a
│                               │   besoin, quel que soit le record)
└──────────┬────────────────────┘
           ▼
      buf.push_str(...);   ← jamais de test if, jamais de bit consulté

```

#### 6.2.2 Branche dynamique — détection à l'écriture, décision au rendu

```
┌──────────────────────────────┐
│  Éditeur écrit du            │   Le corps HTML contient une classe
│  contenu (content.           │   marqueur, ex. class="figure-image-focus"
│  body.content)               │
└──────────┬───────────────────┘
           │ INSERT/UPDATE (trigger AFTER)
           ▼
┌──────────────────────────────┐
│ content.fn_sync_js_deps()    │   Appelle compute_js_deps(NEW.content),
│ (db/05_content/              │   compare à la valeur existante, UPDATE
│  02_systems.sql)             │   conditionnel (jamais si inchangé)
└──────────┬───────────────────┘
           ▼
┌──────────────────────────────┐
│ content.core.js_deps         │   BIGINT — un bit par capacité,
│ (bitset)                     │   ex. 16 = image-focus actif
└──────────┬───────────────────┘
           │ lu au build (cargo build) POUR GÉNÉRER le test — le bit
           │ lui-même n'est lu qu'au runtime, à chaque rendu
           ▼
┌───────────────────────────────┐
│ lower_modules_for_template()  │   Marqueur ABSENT statiquement de ce
│ (build.rs)                    │   template → test dynamique généré
└──────────┬────────────────────┘
           ▼
      if record.js_deps & 16 != 0 { buf.push_str(...); }
```

#### 6.2.3 Convergence — un seul point d'agrégation, jamais deux émissions

```
┌──────────────────────┐   ┌──────────────────────┐
│ Émission statique    │   │ Émission dynamique   │
│ (6.2.1, si marqueur  │   │ (6.2.2, si marqueur  │
│  détecté dans le     │   │  absent du template  │
│  template)           │   │  ET has_record=true) │
└──────────┬───────────┘   └──────────┬───────────┘
           │                          │
           └──────────────┬───────────┘
                           ▼
              <!-- MARIUS_MODULES -->  (base.marius, position fixe)
                           │
                           ▼
              HTML servi au navigateur — précédé, le cas échéant, des
              balises <script> de deps (§4.4), puis AU PLUS UN <script>
              regroupant tous les imports puis tous les appels des
              capacités elles-mêmes (§6.2.4), présent UNIQUEMENT si au
              moins un besoin (statique OU dynamique) est réel pour CE
              template/CE record
```

**Invariant garanti par construction, jamais une déduplication après
coup** : pour une capacité et un template donnés, il n'y a **au plus une**
émission possible. Si le marqueur est détecté statiquement dans le
template, l'émission est inconditionnelle et le test `if record.js_deps &
BIT != 0` n'est **même pas généré** pour cette capacité sur ce template —
la présence statique domine toujours le besoin dynamique, elle ne s'ajoute
jamais à lui. La même règle de domination s'applique séparément à toute
`deps` consommée par cette capacité (§4.4).

**Point clé à retenir :** la détection statique (scan des `.marius`) se
fait **au build**, côté Rust, sur le HTML du template lui-même — jamais sur
`content.body`. La détection dynamique (scan de `content.body`) se fait
**une fois, à l'écriture**, côté SQL — jamais sur les fichiers `.marius`.
Les deux définitions du marqueur (« un token exact d'un attribut `class` »)
doivent rester identiques entre les deux implémentations, mais ce sont
deux implémentations **indépendantes** — aucune ne dérive de l'autre, aucun
code ni bibliothèque de parsing n'est partagé entre elles.

Quatre fichiers doivent donc rester synchronisés à la main : `theme.toml`,
`scripts_registry.lock`, le corps de `compute_js_deps()` (marqueur
dynamique), et le HTML des `.marius` concernés (marqueur statique). Rien ne
les régénère automatiquement les uns à partir des autres.

#### 6.2.4 Sérialisation finale — un seul `<script>` par page, jamais un par capacité

Ce qui précède (§6.2.1 à §6.2.3) décide **quelles** capacités sont
nécessaires pour une page donnée. Une fois cette liste établie, la
sérialisation finale regroupe **tout** dans une seule balise, plutôt que
d'émettre un `<script type="module">` distinct par capacité — cela ne
concerne que les capacités elles-mêmes, **jamais** leurs `deps` (§4.4, qui
restent des balises individuelles, hors de ce regroupement) :

```html
<!-- Avant regroupement (une balise par capacité) -->
<script type="module">import{boot as _n}from"/scripts/line-mark.b0fd8.js";_n();</script><script type="module">import{initMapsSystem as _n}from"/scripts/map.a6994.js";_n();</script>

<!-- Après regroupement (une seule balise) -->
<script type="module">import{boot as _0}from"/scripts/line-mark.b0fd8.js";import{initMapsSystem as _1}from"/scripts/map.a6994.js";_0();_1();</script>
```

Règles de ce regroupement, purement une affaire de sortie — **le contrat
`js_deps`, le scan statique, et la décision statique/dynamique par
capacité (§6.2.1-§6.2.3) n'y participent pas et n'en sont pas affectés** :

- **Tous les `import` d'abord, tous les appels d'activation ensuite** —
  jamais entrelacés capacité par capacité.
- **Alias séquentiels et locaux** (`_0`, `_1`, `_2`, …) — assignés par
  position dans la liste déjà ordonnée des capacités concernées (le même
  ordre canonique qu'ailleurs dans ce guide, jamais un parcours de
  `HashMap`). Ces alias n'ont aucune signification en dehors du bloc : ils
  peuvent — et vont — changer d'une page à l'autre selon les capacités
  réellement présentes.
- **Présence de la balise elle-même** :
  - si au moins une capacité de la page est détectée **statiquement**
    (§6.2.1), la balise est garantie présente — aucun test enveloppe ;
  - si **toutes** les capacités de la page sont dynamiques (§6.2.2), la
    balise entière est conditionnée à *au moins un* bit actif parmi ceux
    concernés — jamais de `<script></script>` vide ;
  - zéro capacité concernée → rien n'est émis.
- Un import ou un appel individuel reste conditionnel à son propre bit
  (`if record.js_deps & BIT != 0 { ... }`) exactement comme avant — seule
  la balise `<script>` elle-même change de granularité, jamais la logique
  par capacité.

### 6.3 Ajouter une nouvelle capacité

Exemple fil rouge : ajouter une capacité `carousel`, déclenchée soit par la
classe `carousel-embed` posée par un éditeur dans le corps d'un article
(branche dynamique), soit par un `.marius` qui la porte en dur dans son
propre layout (branche statique) — via un module `carousel.js` exportant
une fonction `boot`. Les étapes 6.3.1 à 6.3.3 sont **communes aux deux
branches** ; l'étape 6.3.4 ne concerne que la branche dynamique.

#### 6.3.1 Le module JS

Créer `assets/default/scripts/development/carousel.js`, avec une fonction
exportée nommée (c'est cette fonction qui sera appelée automatiquement) :

```js
export function boot() {
  // ...
}
```

Le nom de la fonction est libre — il sera référencé tel quel dans
`theme.toml` (`activation`). Convention observée dans ce projet : `init` ou
un nom court et explicite (`boot`, `mount`). Si ce module a besoin de code
tiers vendoré : import ESM direct (§3.2) si la bibliothèque est
`module = true`, ou déclaration `deps` (§4) si elle est `module = false`
(UMD/classique) — jamais les deux pour la même bibliothèque.

#### 6.3.2 `theme.toml` — déclarer la capacité

Dans `assets/default/theme.toml`, sous `[scripts.capabilities]` :

```toml
[scripts.capabilities.carousel]
entry = "scripts/development/carousel.js"
markers = ["carousel-embed"]
activation = "boot"
# deps = ["libraries/<nom>/<fichier>.js"]   # optionnel, voir §4
```

- `entry` : chemin du fichier JS, relatif au dossier du thème.
- `markers` : liste des classes HTML qui déclenchent cette capacité. Peut
  contenir plusieurs entrées (cf. `range`/`range-multithumb`, une seule
  capacité, deux marqueurs qui activent le même bit). **Sert aux deux
  branches** (§6.2) : c'est cette même liste que `lower_modules_for_template`
  (build.rs) compare au HTML statique des `.marius` (§6.2.1) — mais elle
  n'alimente **jamais mécaniquement** `compute_js_deps` (§6.2.2, SQL) :
  ajouter un marqueur ici ne le fait pas apparaître comme par magie côté
  SQL, il faut l'étape 6.3.4 séparément. Les deux listes doivent rester
  synchronisées à la main.
- `activation` : nom de la fonction exportée à appeler. **Doit être un
  identifiant valide** (lettres/chiffres/underscore, ne commence pas par un
  chiffre) — il est injecté tel quel dans le code Rust généré, jamais
  échappé comme une chaîne.
- `deps` : optionnel, voir §4 en détail. Absent = comportement strictement
  identique à avant l'introduction de ce champ.

#### 6.3.3 `scripts_registry.lock` — attribuer un bit

Fichier `assets/default/scripts_registry.lock`, à côté de `theme.toml`.
**Manuel, append-only, jamais généré automatiquement.** Ajouter une ligne
avec le **prochain bit libre** (prochaine puissance de deux jamais utilisée
— vérifier les valeurs existantes dans le fichier avant d'écrire) :

```
carousel = 128
```

Règles strictes :
- Un bit retiré n'est **jamais réattribué** à un nom différent. Pour retirer
  une capacité, renommer sa clé en `_retired_<nom>`, ne jamais supprimer la
  ligne.
- Bijection stricte avec `theme.toml [scripts.capabilities]` : toute
  capacité active dans l'un doit exister dans l'autre, sinon le build
  échoue (`cargo:error`, volontaire — voir §7).

#### 6.3.4 `compute_js_deps` — reconnaître le marqueur côté SQL (branche dynamique — §6.2.2)

Nécessaire **uniquement si** la capacité doit pouvoir être déclenchée par du
contenu éditorial (un éditeur pose la classe dans le corps d'un article).
Si `carousel` ne doit jamais être déclenchée que par des `.marius` (§6.3.5),
cette étape peut être sautée — mais alors le bit attribué en 6.3.3 ne sera
jamais réellement testé au runtime : autant ne pas le prévoir du tout, ou
le documenter comme réservé à l'usage statique.

Dans `db/05_content/02_systems.sql`, fonction `content.compute_js_deps` :
ajouter un bloc `IF` avec le même bit que dans `scripts_registry.lock`.

```sql
IF 'carousel-embed' = ANY(v_classes) THEN
  v_deps := v_deps | 128;  -- carousel
END IF;
```

**Contrat strict, jamais dérogé** : comparaison exacte de tokens `class`
uniquement. Jamais de sous-chaîne (`position()`/`LIKE`), jamais d'attribut
`data-*`, jamais un autre motif HTML. La fonction découpe déjà tous les
attributs `class="..."` du corps en tokens individuels (`v_classes`,
`regexp_split_to_table` sur les espaces) — un nouveau marqueur se contente
de tester son appartenance à cet ensemble, rien d'autre à toucher dans le
corps de la fonction.

#### 6.3.5 Déclencher par un `.marius` (branche statique — §6.2.1)

Rien à câbler séparément côté Rust — le scan statique
(`extract_static_class_tokens`) est générique, il compare **automatiquement**
le HTML de **chaque template** aux `markers` de **toutes** les capacités
déclarées en 6.3.2. Il suffit d'écrire le marqueur directement dans le
`.marius` concerné :

```html
<pre class="carousel-embed">
  ...
</pre>
```

Dès que ce `.marius` (ou un layout dont il hérite) contient ce marqueur
littéralement dans son HTML, `carousel.js` est émis **inconditionnellement**
pour ce template — aucun bit testé, aucun `content.body` consulté, y
compris sur les pages `STATIC_PAGES` (§9). Écrire `class="{{ some_field }}"`
ne compte **jamais** comme un marqueur statique — seule une chaîne littérale
dans le HTML du template est détectable ici, par construction (fragment-forge
ne scanne que les tokens `FlatPageToken::Static`, jamais l'intérieur d'un
`{{ champ }}`).

Si le même marqueur apparaît **à la fois** dans un `.marius` et dans le
corps d'un enregistrement rendu par ce template, une seule émission est
produite — la présence statique domine toujours, le test dynamique n'est
même pas généré pour cette capacité sur ce template (§6.2.3).

## 7. Compiler — l'ordre compte, chaque étape peut échouer isolément

L'Étape 1 est commune aux **trois** mécanismes de configuration (§1) : tout
ajout — component, bibliothèque, ou capacité — passe par le même
compilateur d'assets. **`deps` n'est jamais résolu à l'Étape 1** (§4.5) —
sa résolution AOT complète a lieu à l'Étape 3. L'Étape 2 est spécifique à
la branche dynamique des capacités (SQL, `content.core`) ; un component ou
une bibliothèque seuls n'en ont jamais besoin.

**L'Étape 5 est la plus souvent oubliée, et son absence ne produit aucune
erreur.** Voir `runtime-lifecycle-guide.md` pour le détail complet — le
résumé nécessaire ici : `cargo build` recompile `render()` dans le
binaire, mais **ne régénère jamais** un pack HTML déjà servi
(`{table}.bin`). Tant qu'aucun événement runtime (`NOTIFY`) n'a
effectivement déclenché une régénération pour les enregistrements
concernés, le HTML servi reste celui produit par l'**ancien** `render()` —
build vert, serveur relancé, et pourtant rien ne change à l'écran. Seules
les pages `STATIC_PAGES` échappent à cette étape (leur `.html` est écrit
directement par `cargo build` de `core/schema`, sans passer par
PostgreSQL).

Sauter une étape ne produit **pas toujours** une erreur immédiate — parfois
juste un comportement silencieusement obsolète (cas vécu en session :
`cargo build` qui passe sur un manifeste d'assets périmé). Toujours
exécuter les étapes pertinentes dans l'ordre après une modification de
`theme.toml`/`scripts_registry.lock`/`02_systems.sql`.

### Étape 1 — Recompiler les assets (component, bibliothèque, capacité)

```bash
cargo run --release --bin marius-assets -- ./assets/default
```

Régénère `build/default/manifest.toml` — c'est lui qui contient l'URL
hachée (`/scripts/carousel.HASH.js`, `/libraries/deckgl.HASH.js`, ...) que
`validate_capabilities`/`{% asset %}` iront lire, ainsi que
`classic_scripts` (§4.3, la liste sparse des bibliothèques `module =
false`). Sans cette étape, une clé peut être parfaitement déclarée et
pourtant introuvable :
- capacité/component : `cargo build` échouera avec *« clé 'carousel.js'
  absente du manifeste d'assets »* ;
- bibliothèque référencée par un `import` direct (§3.2) : le build de
  `marius-assets` lui-même échoue avec `AssetNotFound` — jamais un 404
  silencieux découvert plus tard.

Une `deps` (§4), elle, n'est **jamais** validée à cette étape — voir Étape 3.

### Étape 2 — Recharger le schéma SQL (capacité, branche dynamique uniquement)

Uniquement si `02_systems.sql` (ou tout autre fichier sous `db/`) a changé —
**inutile pour un component ou une bibliothèque, et inutile si la capacité
n'est déclenchée que statiquement (§6.3.5), sans passer par 6.3.4** :

```bash
psql "$DATABASE_URL" -f db/master_init.sql
psql "$DATABASE_URL" -f db/dml/master_schema_dml.pgsql
psql "$DATABASE_URL" -c "ANALYZE;"
```

`master_init.sql` recrée la base entièrement (`DROP DATABASE` inclus) — pas
de migration incrémentale dans ce projet. Sans cette étape, la nouvelle
capacité reste ajoutée dans `theme.toml`/`scripts_registry.lock` côté Rust,
mais son marqueur `class` correspondant n'est jamais reconnu côté SQL : le
bit ne s'allumera jamais, quel que soit le contenu éditorial écrit.

### Étape 3 — Compiler le schéma Rust (dès qu'un `.marius` référence l'asset, ou qu'une `deps` est déclarée)

```bash
cargo build
```

C'est ici que `validate_capabilities` (crates/core/schema/build.rs) lit
`theme.toml` + `scripts_registry.lock` + `manifest.toml`, valide la
bijection et les bits, **et résout entièrement `deps`** (§4.2 : chaque
entrée, canonicalisée puis recherchée dans `manifest.toml`, croisée avec
`classic_scripts` pour le mode de chargement) — puis
`lower_modules_for_template`, appelée une fois par composant/page, scanne
le HTML de chaque `.marius` (`.marius` fusionné parent+enfant) et génère
soit une émission inconditionnelle (marqueur détecté statiquement), soit un
test `if record.js_deps & BIT != 0 { ... }` (marqueur absent statiquement),
en agrégeant et dédupliquant les `deps` de toutes les capacités du
template (§4.4). Toute résolution `{% asset %}` (component, bibliothèque
référencée directement par un template) est également figée à cette étape.
Échoue fort (jamais silencieusement) sur :
- capacité présente dans un fichier (`theme.toml`/`scripts_registry.lock`),
  absente de l'autre ;
- bit invalide (pas une puissance de deux) ou dupliqué ;
- `activation` qui n'est pas un identifiant valide ;
- `markers` vide ;
- clé `{path}.js` (ou toute clé `{% asset %}`) absente du manifeste d'assets
  (→ retour à l'Étape 1) ;
- **une entrée de `deps` absente du manifeste** (bibliothèque non
  déclarée, `root` incorrect, faute de frappe dans le chemin — même classe
  d'erreur que pour `entry`, jamais un repli silencieux, §4.2).

### Étape 4 — `marius-dump` (transport de données, **sans rapport avec le rendu HTML**)

```bash
cargo run --bin marius-dump
```

`marius-dump` peut produire `{table}_store.bin` (dump brut, transport) et,
séparément, provisionner un pack HTML initial — mais `regenerate_and_swap`
(le chemin qui sert réellement le HTML) **ne lit jamais** `store.bin` ; il
récupère les données depuis PostgreSQL directement
(`runtime-lifecycle-guide.md` §6). Concrètement, l'ajout ou la
modification d'une capacité — qui n'ajoute jamais de colonne à
`content.core`, `js_deps` étant un `BIGINT` déjà existant — **ne rend pas
cette étape nécessaire pour que le bit soit correctement rendu en HTML.**
Cette étape reste utile uniquement si un autre consommateur du dump
(hors périmètre de ce guide) dépend de `store.bin` à jour, ou après un
véritable `ALTER TABLE` (colonne ajoutée/retirée) sur `content.core`.

**Limitation actuelle : `marius-dump` ne couvre que `content_core`.**
Aucune autre table n'a de mécanisme de dump à ce jour (cf.
`docs/guides/../SUIVI-js-deps-points-en-attente.md`, §1).

### Étape 5 — Déclencher la régénération runtime (capacité dynamique, ou tout enregistrement déjà existant)

Ne concerne jamais un ajout pur (nouveau document jamais encore rendu) ni
une page `STATIC_PAGES` — uniquement les enregistrements **déjà rendus au
moins une fois** avant le déploiement du nouveau binaire. `cargo build`
compile un nouveau `render()`, mais ne le fait tourner sur aucune donnée :
tant qu'aucune écriture SQL ne produit de `NOTIFY` sur les lignes
concernées, le pack HTML déjà servi (produit par l'**ancien** `render()`)
reste tel quel.

En développement, forcer la régénération d'une ligne précise :

```sql
UPDATE {schema}.{table} SET {pk} = {pk} WHERE {pk} = {valeur};
```

ou de toute la table (si le trigger `AFTER UPDATE` correspondant émet bien
le `NOTIFY`) :

```sql
UPDATE {schema}.{table} SET {pk} = {pk};
```

S'applique aussi bien à une capacité dynamique nouvellement déclenchée par
un marqueur déjà présent dans un vieux document, qu'à une capacité ou un
component nouvellement rendus **inconditionnels** par un marqueur ajouté
dans un `.marius` (§6.2.1/§6.3.5), qu'à une `deps` nouvellement ajoutée à
une capacité déjà active : dans tous les cas, c'est le pack existant de
l'enregistrement qui doit être régénéré, jamais seulement le binaire.

Prérequis souvent oublié en local : le serveur doit être en écoute
(`PgListener` abonné) **avant** l'écriture SQL — un `NOTIFY` émis pendant
que le serveur est arrêté n'est jamais rejoué à son redémarrage.

## 8. Vérifier que ça fonctionne

Avertissement commun aux vérifications ci-dessous, avant même de regarder
le HTML : **si la page testée existait déjà avant ce déploiement**,
l'Étape 5 (§7) est un préalable — sinon vous inspectez encore le pack
produit par l'ancien `render()`. Pour une page tout juste créée après le
déploiement du nouveau binaire, ce préalable ne se pose pas (son premier
rendu utilise déjà le nouveau code). Seules les pages `STATIC_PAGES`
échappent entièrement à cette question.

### Component ou bibliothèque seule (import direct, §3.2)

1. Vérifier la présence de la clé dans `build/default/manifest.toml` après
   l'Étape 1.
2. Servir la page référençant le `.marius` concerné (après Étape 5 si la
   page existait déjà), inspecter le HTML rendu : l'URL de l'asset doit
   pointer vers le fichier haché (`/scripts/main.<hash>.js`), jamais vers
   le chemin source.
3. Pour une bibliothèque : ouvrir le fichier JS produit et vérifier que
   l'`import` vers la bibliothèque a bien été réécrit vers son URL hachée
   (§3.2) — pas seulement que le script consommateur se charge.

### `deps` (§4)

1. Vérifier que `build/default/manifest.toml` contient bien la clé de la
   bibliothèque sous `[assets]`, et, si `module = false`, que sa clé
   canonique figure dans `classic_scripts` (§4.3) après l'Étape 1.
2. `cargo build` (Étape 3) : aucune erreur `deps '...' absente du
   manifeste d'assets`.
3. Servir la page (après Étape 5 si elle existait déjà) et inspecter le
   HTML : la balise de la dépendance doit apparaître **avant** le
   `<script type="module">` de la capacité, avec la forme attendue
   (`<script src="..." defer>` pour `module = false`, `<script
   type="module" src="...">` pour `module = true`).
4. Si plusieurs capacités du même template partagent la même `deps` :
   vérifier qu'une seule balise apparaît dans le HTML rendu (§4.4).
5. Pour une dépendance partagée par des capacités dynamiques uniquement :
   vérifier que la condition affichée correspond bien au OU binaire des
   bits attendus.

### Branche dynamique (§6.2.2, si l'étape 6.3.4 a été faite)

1. Écrire ou modifier un document dont le corps contient le marqueur
   (`class="carousel-embed"` quelque part dans `content.body.content`) —
   cette écriture SQL déclenche elle-même le `NOTIFY` nécessaire (Étape 5),
   aucune action séparée à faire si le document est réellement modifié à
   cette occasion.
2. Vérifier le bit en base :
   ```sql
   SELECT document_id, js_deps FROM content.core WHERE document_id = <id>;
   ```
   `js_deps & 128` doit être non nul.
3. Servir la page (`cargo run --bin marius`) et inspecter le HTML rendu :
   le `<script type="module">` du module concerné doit apparaître dans
   `<head>`, juste avant `</head>` (position de `<!-- MARIUS_MODULES -->`
   dans `base.marius`) — uniquement sur les documents qui portent le
   marqueur, absent partout ailleurs.

### Branche statique (§6.2.1/6.3.5)

Aucune base de données à interroger pour la **décision** — elle est prise
au build, identique pour tous les enregistrements du template concerné.
Mais l'**artefact** d'un enregistrement donné, lui, suit une règle
différente selon le type de page :

- **`STATIC_PAGES`** : `cargo build` (`core/schema`) écrit directement le
  `.html` final. Aucune étape supplémentaire.
- **Page adossée à un `record`** (Mode Page) : même si le test lui-même
  n'est jamais généré (§6.2.3), le pack déjà servi pour un enregistrement
  existant ne se régénère pas tout seul après `cargo build` — l'Étape 5
  reste nécessaire pour que ce document précis soit re-rendu par le
  nouveau `render()`.

1. Vérifier que le marqueur est bien présent, littéralement, dans le HTML
   du `.marius` (pas dans un `{{ champ }}`).
2. `cargo build`, puis :
   - `STATIC_PAGES` : servir directement la page — `.html` déjà à jour.
   - Mode Page : Étape 5 sur l'enregistrement testé, puis servir la page —
     même un document dont `js_deps` vaut `0` doit alors afficher le
     `<script type="module">` : la présence vient du template, jamais du
     contenu.

## 9. Pièges déjà rencontrés

- **Un `.js` dans `[static.verbatim].files` échoue au build, volontairement**
  (§10.1) : *« JavaScript asset "..." is not allowed here; use
  [scripts.components] or [scripts.capabilities.*]. »* Ce n'est jamais un
  bug — c'est le garde-fou attendu. Déplacer le fichier vers l'une des deux
  sections indiquées. Ne s'applique jamais aux fichiers `.js` d'une
  bibliothèque `[libraries.*]` — la restriction porte uniquement sur
  l'interface déclarative `[static.verbatim]`, jamais sur le mécanisme
  interne de copie qu'elle partage avec `[libraries.*]`.
- **`cargo build` vert ne garantit pas que le bit fonctionne réellement.**
  Il valide la cohérence structurelle (bijection, bits, manifeste, `deps`)
  — pas que la base de données a bien été rechargée avec le dernier
  `compute_js_deps`. Un build vert après une modification de
  `02_systems.sql` sans rechargement SQL (Étape 2) compile un code
  parfaitement correct... qui ne sera jamais déclenché par aucun contenu
  réel.
- **`cargo build` vert + serveur relancé ne régénère aucun pack HTML déjà
  servi** (cf. `runtime-lifecycle-guide.md`, et Étape 5, §7). C'est le
  piège le plus trompeur de ce guide, parce qu'aucune erreur ne le
  signale : la page se sert normalement, juste avec l'ancien contenu. Si
  un changement de template, de component, de capacité ou de `deps`
  n'apparaît toujours pas après un déploiement propre, vérifier en
  priorité si un `NOTIFY` a réellement été déclenché pour la page testée
  avant de chercher une erreur dans le template ou dans `theme.toml`.
- **`store.bin`/`marius-dump` n'a aucune incidence sur le rendu HTML d'une
  capacité.** Une confusion héritée d'une ancienne version de ce guide,
  antérieure à l'introduction du Sweep Merge (`runtime-lifecycle-guide.md`
  §6) : `regenerate_and_swap` lit PostgreSQL directement, jamais
  `store.bin`. Ne pas chercher de ce côté si un bit `js_deps` ne semble
  pas pris en compte.
- **`meta.containment_intent.intent_density_bytes`** (registre de taille
  DOD, `db/10_meta_seed/01_manifest.sql`) doit rester synchronisé
  manuellement avec la taille réelle du `StorageRow` généré à chaque
  `ALTER TABLE` sur une table concernée. `cargo build` échoue explicitement
  si ce n'est pas fait (`layout diverge du registre`) — le message d'erreur
  donne la valeur calculée à copier.
- **Une page statique (`STATIC_PAGES`, ex. `offline`) n'a jamais de
  capacités actives DYNAMIQUEMENT**, par construction (pas de `record`,
  pas de `js_deps` à tester). Mais elle **peut** émettre un module si le
  marqueur est détecté **statiquement** dans son propre `.marius` ou dans
  `base.marius` (§6.2.1/6.3.5) — ce n'est pas une exception, c'est la
  partie statique qui, elle, ne dépend jamais d'un `record`. Ne pas
  confondre « aucun bit à tester » (toujours vrai pour `STATIC_PAGES`) avec
  « ne peut jamais avoir besoin d'un module » (faux).
- **Ne pas confondre : le test dynamique n'est même pas généré pour une
  capacité dont le marqueur est présent statiquement dans le template en
  cours** — pas une histoire de « les deux s'exécutent, l'un des deux est
  redondant ». Si un module semble absent d'un `<head>` alors que
  `js_deps` porte bien le bit correspondant, vérifiez d'abord si ce
  template (ou un layout dont il hérite) porte déjà le marqueur en dur :
  l'émission a probablement déjà eu lieu, inconditionnelle — pas un bug.
- **Les alias `_0`/`_1`/… (§6.2.4) ne sont jamais stables d'une page à
  l'autre.** Ils dépendent de la liste des capacités réellement présentes
  sur CETTE page précise — `map` peut être `_0` sur une page et `_2` sur
  une autre. Ne rien coder côté JS qui suppose un alias fixe pour une
  capacité donnée ; ces alias sont strictement internes au bloc généré,
  jamais une API.
- **Un `import` direct vers une bibliothèque non vendorée, ou avec un
  chemin incorrect, échoue au build de `marius-assets`, jamais au
  runtime.** Si vous cherchez un 404 navigateur pour une bibliothèque, le
  build a probablement été lancé avant l'ajout de `[libraries.*]`/le dépôt
  des fichiers — pas un bug du pipeline de résolution.
- **Importer directement (§3.2) une bibliothèque `module = false` compile
  sans erreur mais casse au runtime.** Rien, à aucune étape du build,
  n'empêche d'écrire `import ... from "libraries/deckgl/deckgl.js"` même
  si `deckgl` est déclarée `module = false` — la résolution d'un `import`
  direct ne consulte jamais ce champ. L'échec (bundle UMD chargé comme
  module ES) n'apparaît qu'au chargement dans le navigateur. Pour une
  bibliothèque `module = false`, toujours `deps` (§4), jamais `import`.
- **Confondre chemin relatif et chemin canonique pour une bibliothèque.**
  Depuis `map.js`, `import "./deckgl.js"` cherche un fichier `deckgl.js`
  **sibling de `map.js`**, jamais la bibliothèque `libraries/deckgl/`. Le
  chemin vers une bibliothèque est toujours relatif à la racine du thème
  (§3.2), jamais préfixé `./`/`../`.
- **Ne pas attendre qu'une bibliothèque vendorée réécrive ses propres
  références internes** (§3.3) — un fichier de bibliothèque qui importe un
  autre fichier de la même bibliothèque garde ce chemin intact après le
  build. Un 404 sur un chemin qui n'apparaît nulle part dans `theme.toml`
  ni dans aucun script du thème pointe généralement vers ce cas.
- **`deps` sur `[scripts.components]` n'existe pas.** `CapabilityConfig`
  seul porte ce champ ; `[scripts.components]` reste une simple table
  `nom → chemin` (§2). Un component qui a besoin d'un chargement garanti
  avant lui n'a, à ce jour, aucun mécanisme équivalent à `deps` — seul
  l'import direct ESM (§3.2) reste disponible pour lui.

## 10. Constats d'architecture pour une prochaine passe (informationnel)

Le point 10.1 ci-dessous a depuis été **tranché et implémenté** — ce n'est
plus un simple constat, contrairement au point 10.2, qui lui reste une
observation ouverte, sans décision prise.

### 10.1 `[static.verbatim]` interdit au JavaScript — décision actée

**Statut : décision architecturale actée.** Le JavaScript ne relève plus
jamais de `[static.verbatim]`, quelle que soit la façon dont il est
consommé. Deux voies légitimes, et seulement deux, pour déclarer du
JavaScript dans ce projet :

- **`[scripts.capabilities.*]`** — un script rattaché à une capacité
  conditionnelle, avec éventuellement des dépendances de chargement
  (`deps`, §4) ;
- **`[scripts.components]`** — un script autonome, consommé explicitement
  comme asset (`{% asset %}`, §2), sans condition.

`[static.verbatim]` reste réservé aux assets dont le pipeline n'a **pas**
besoin de connaître la sémantique d'exécution — images, polices, fichiers
`.map`, tout ce qui est copié tel quel sans jamais être exécuté par un
navigateur en tant que script.

**Justification.** Avec `deps` (§4) en place, les cas d'usage légitimes
pour amener du JS au client sont désormais tous couverts explicitement :

```
[scripts.capabilities.*]  → script applicatif conditionnel
[scripts.components]      → script applicatif autonome/structurel
deps = [...]              → dépendance de chargement d'une capacité
{% script %}...{% endscript %}  → script explicitement écrit dans un template
[service_worker]          → pipeline spécialisé
```

Un `.js` glissé directement sous `[static.verbatim].files` héritait
silencieusement de deux limites qui ne se voyaient qu'à l'usage : aucun
moyen de le déclarer `module = false` (ce champ n'existe que sur
`[libraries.*]`, §3.1 — un tel fichier était donc toujours traité comme
`module: true` par `deps`, même s'il s'agissait en réalité d'un bundle
UMD, sans qu'aucune erreur ne le signale) ; et aucune réécriture de ses
références internes (`[static.verbatim]` partage l'invariant « zéro
transformation » de `[libraries.*]`, §3.3).

**Portée de la règle — important.** Elle concerne strictement
l'**interface déclarative** `[static.verbatim].files` (ce que
l'intégrateur écrit dans `theme.toml`), jamais le mécanisme interne de
copie verbatim lui-même (`run_verbatim_pipeline`), qui reste utilisé tel
quel — sans aucune restriction d'extension — pour les fichiers découverts
via `[libraries.*]` (une bibliothèque vendorée contient légitimement du
`.js`, `.css`, des images, etc., tous copiés verbatim). La garde ne
s'applique qu'au moment où `[static.verbatim].files` est lu, jamais à
l'intérieur de `run_verbatim_pipeline` lui-même.

**Erreur de build attendue**, dès qu'un `.js` apparaît dans
`[static.verbatim].files` :

```
[static.verbatim].files: JavaScript asset "scripts/legacy.js" is not
allowed here; use [scripts.components] or [scripts.capabilities.*].
```

Échec de build immédiat et déterministe — jamais un avertissement, jamais
une tolérance silencieuse, cohérent avec la discipline « échec dur, jamais
un repli silencieux » déjà en place pour `deps` (§4.2) et pour la
résolution d'assets en général (§3.2, §7).

**Implémentation.** Faite dans cette même session — `crates/assets/src/main.rs`,
juste après la lecture de `theme.static_.verbatim.files` et avant toute
fusion avec les fichiers découverts par `[libraries.*]`. Vérifiée
empiriquement sur le message d'erreur exact et les cas limites (extension
homonyme partielle, absence d'extension, position dans une liste mixte) —
pas de test automatisé dans le crate à ce jour (`main.rs` n'a aucune
infrastructure de test existante, cf. §10 de l'audit de session
correspondant).

### 10.2 Asymétrie de convention de clé `{% asset %}`

Observation factuelle, non corrigée : la clé attendue par `{% asset %}` ne
suit pas une convention unique selon le pipeline d'origine de l'asset.

| Pipeline | Forme de la clé | Exemple |
|---|---|---|
| `styles.rs` (CSS) | Chemin canonique physique | `{% asset styles/print.css %}` |
| `verbatim.rs` / `libraries.rs` | Chemin canonique physique | `{% asset favicons/logo.svg %}` |
| `sprites.rs` | Chemin canonique **construit** (`sprites/{nom}.svg`) | `{% asset sprites/utils.svg %}` |
| `webmanifest.rs` | Nom logique **fixe**, unique dans tout le projet | `{% asset manifest.webmanifest %}` |
| `scripts.rs` — **point d'entrée** `[scripts.components]`/`[scripts.capabilities.*]` | Chemin canonique physique | `{% asset scripts/mon-script.js %}` |
| `scripts.rs` — module transitif (importé relativement, jamais nommé dans `theme.toml`) | Chemin canonique physique | `{% asset scripts/navigation.js %}` |

Concrètement, à part `webmanifest.rs`, tous les autres pipelines de ce
crate utilisent le chemin canonique physique : `{% asset styles/print.css %}`,
`{% asset scripts/mon-script.js %}`, etc.

---

_Document révisé le 31 août 2026_
