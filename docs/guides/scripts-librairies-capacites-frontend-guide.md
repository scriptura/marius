# Guide — Scripts, bibliothèques vendorées et capacités frontend conditionnelles

> Renommé depuis `js-deps-capacites-frontend-guide.md` — l'ancien titre ne
> couvrait que la branche `js_deps`/capacités. Ce guide couvre désormais les
> **trois** mécanismes disponibles pour amener du JS au client, et comment
> ils s'articulent entre eux.

Références :
- `HANDOFF-js-deps-capacites-frontend-v2.md` — implémentation d'origine du
  système de capacités conditionnelles (§5 de ce guide).
- `SPEC-canonical-asset-identity.md` — implémentation d'origine de
  `CanonicalAssetId` et des bibliothèques vendorées (§3 de ce guide).
- `runtime-lifecycle-guide.md` — modèle du cycle build/runtime (AOT vs
  réactif PostgreSQL) dont dépend directement tout ce qui touche à la
  branche dynamique des capacités (§5.2.2, §6, §7 de ce guide). À lire en
  cas de doute sur « pourquoi mon changement n'apparaît pas alors que le
  build est vert ».

Ce guide en est la version d'usage courant — pas d'historique de décisions,
juste : comprendre le système, l'étendre, le compiler.

## 1. Vue d'ensemble — trois mécanismes, un seul pipeline d'assets

Trois besoins distincts, trois sections de `theme.toml`, un seul
compilateur (`marius-assets`) pour les résoudre tous :

| Besoin | Section `theme.toml` | Condition de chargement |
|---|---|---|
| Script chargé sur des pages précises, décidé au moment d'écrire le template | `[scripts.components]` | Aucune — inclusion explicite dans un `.marius` |
| Script chargé selon un marqueur structurel (`.marius`) ou éditorial (`content.body`) | `[scripts.capabilities.*]` | Bit `js_deps`, testé au build (statique) ou au rendu (dynamique) |
| Code tiers vendoré, consommé par un script ci-dessus | `[libraries.*]` | Jamais chargé seul — référencé par un `import` depuis un component ou une capacité |

Une bibliothèque n'est jamais un point d'entrée en elle-même : elle existe
pour être **importée** par un component ou une capacité (§3.2). Un
component et une capacité sont tous deux des points d'entrée ESM,
traités par le même pipeline d'exploration/tri/hachage (`scripts.rs`) — leur
seule différence est la présence ou non d'une condition de chargement.

Si le besoin est conditionné par le contenu ou par le template, allez
directement en §5. Si c'est un script sans condition, §2. Si c'est du code
tiers à vendorer, §3.

## 2. Scripts inconditionnels — `[scripts.components]`

Dans `assets/default/theme.toml` :

```toml
[scripts.components]
main = "scripts/main/index.js"
```

- Clé (`main`) = nom logique = clé de manifeste `main.js`.
- Aucun `markers`, aucune `activation`, aucun bit `scripts_registry.lock` —
  rien à synchroniser côté SQL, rien à câbler dans `compute_js_deps`.
- Résolu par le **même pipeline ESM** que les capacités
  (`build_module_arena` → tri topologique → `patch_and_hash_modules`) :
  les imports relatifs entre fichiers d'un même composant sont suivis et
  chaque module — y compris un module intermédiaire jamais nommé
  directement dans `theme.toml` (ex. `navigation.js` importé par
  `index.js`) — reçoit sa propre entrée de manifeste, sous son chemin
  canonique complet (`scripts/main/navigation.js`), jamais son seul nom de
  fichier.
- Un component peut importer une bibliothèque vendorée exactement comme une
  capacité (§3.2) — le mécanisme de résolution est identique, indépendant
  de la section d'origine du point d'entrée.

Consommation côté template : via `{% asset main.js %}`, comme toute entrée
de manifeste (même convention que `webmanifest`/CSS/sprites). 🟡 La forme
exacte de la balise (`<script type="module" src="...">` posée à la main
dans le `.marius`, vs. un mécanisme d'inclusion différent) n'a pas été
vérifiée sur pièce pour ce guide — contrairement aux capacités, dont
l'émission HTML est entièrement générée par `lower_modules_for_template`
(§5.2), un component n'est probablement pas orchestré automatiquement : à
confirmer sur un `.marius` existant en utilisant déjà `[scripts.components]`
avant de considérer ce point clos.

**Quand l'utiliser :** un script dont le besoin est connu à l'écriture du
template, indépendamment du contenu de la page — navigation globale,
recherche, tout ce qui n'a pas de sens à conditionner.

## 3. Bibliothèques vendorées — `[libraries.*]`

### 3.1 Déclarer et vendorer

```toml
[libraries.deckgl]
root = "libraries/deckgl"
```

- Autorise la découverte récursive de **tout** le sous-arbre
  `assets/default/libraries/deckgl/` — déterministe (chemins triés), aucune
  distinction de type de fichier (JS, CSS, images, `.map`, fonts...).
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

Procédure : déposer les fichiers réels de la bibliothèque, tels que fournis
par l'éditeur tiers, sous `assets/default/libraries/<nom>/` (aucun
build/bundling supplémentaire n'est effectué par ce mécanisme), déclarer
`root` dans `theme.toml`, relancer l'Étape 1 (§6).

### 3.2 Référencer une bibliothèque depuis un script

Depuis n'importe quel `[scripts.components]` ou `[scripts.capabilities.*]` :

```js
import { IconLayer } from "libraries/deckgl/deckgl.js";
```

Le slash de tête est optionnel et sans effet — `"/libraries/deckgl/deckgl.js"`
résout à l'identique.

- **Convention impérative :** chemin relatif à la **racine du thème**,
  jamais relatif au fichier qui importe. Un import commençant par `./` ou
  `../` désigne toujours un autre module du **même composant/capacité**
  (résolu, haché individuellement, chaîné dans le graphe ESM) — jamais une
  bibliothèque, même si un fichier de même nom existe sous
  `libraries/`.
- Résolu au build contre le registre déjà peuplé par
  `[libraries.*]`/`[static.verbatim]` — qui s'exécute systématiquement
  avant tout script (§6, Étape 1) — et réécrit vers l'URL hachée réelle
  (`/libraries/deckgl.<hash>.js`).
- Absence de la clé dans le registre (bibliothèque non déclarée, `root`
  incorrect, faute de frappe dans le chemin) : **échec de build immédiat**
  (`AssetNotFound`), jamais un 404 silencieux découvert seulement au
  runtime.

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

## 4. Grammaire des imports reconnue

S'applique à tout script traité comme point d'entrée ou dépendance ESM
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
(piège potentiel, cf. §8) :

```js
import('./x.js');       // import() dynamique
import './x.js';        // effet de bord seul, sans `from`
```

Un chemin composé dans un gabarit (`` `${base}/x.js` ``) n'est jamais
détecté non plus — un gabarit est traité comme une région opaque de bout
en bout.

## 5. Capacités frontend conditionnelles (`js_deps`)

### 5.1 Le problème que ça résout

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
dans le cas particulier où son marqueur est détecté statiquement (§5.2.1).

### 5.2 Suivre la donnée — vue d'ensemble

Deux chemins, indépendants, qui convergent au même endroit du HTML final.

#### 5.2.1 Branche statique — détection AOT dans les templates

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

#### 5.2.2 Branche dynamique — détection à l'écriture, décision au rendu

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

#### 5.2.3 Convergence — un seul point d'agrégation, jamais deux émissions

```
┌──────────────────────┐   ┌──────────────────────┐
│ Émission statique    │   │ Émission dynamique   │
│ (5.2.1, si marqueur  │   │ (5.2.2, si marqueur  │
│  détecté dans le     │   │  absent du template  │
│  template)           │   │  ET has_record=true) │
└──────────┬───────────┘   └──────────┬───────────┘
           │                          │
           └──────────────┬───────────┘
                           ▼
              <!-- MARIUS_MODULES -->  (base.marius, position fixe)
                           │
                           ▼
              HTML servi au navigateur — AU PLUS UN <script>
              regroupant tous les imports puis tous les appels
              (§5.2.4), présent UNIQUEMENT si au moins un besoin
              (statique OU dynamique) est réel pour CE template/
              CE record
```

**Invariant garanti par construction, jamais une déduplication après
coup** : pour une capacité et un template donnés, il n'y a **au plus une**
émission possible. Si le marqueur est détecté statiquement dans le
template, l'émission est inconditionnelle et le test `if record.js_deps &
BIT != 0` n'est **même pas généré** pour cette capacité sur ce template —
la présence statique domine toujours le besoin dynamique, elle ne s'ajoute
jamais à lui.

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

#### 5.2.4 Sérialisation finale — un seul `<script>` par page, jamais un par capacité

Ce qui précède (§5.2.1 à §5.2.3) décide **quelles** capacités sont
nécessaires pour une page donnée. Une fois cette liste établie, la
sérialisation finale regroupe **tout** dans une seule balise, plutôt que
d'émettre un `<script type="module">` distinct par capacité :

```html
<!-- Avant regroupement (une balise par capacité) -->
<script type="module">import{boot as _n}from"/scripts/line-mark.b0fd8.js";_n();</script><script type="module">import{initMapsSystem as _n}from"/scripts/map.a6994.js";_n();</script>

<!-- Après regroupement (une seule balise) -->
<script type="module">import{boot as _0}from"/scripts/line-mark.b0fd8.js";import{initMapsSystem as _1}from"/scripts/map.a6994.js";_0();_1();</script>
```

Règles de ce regroupement, purement une affaire de sortie — **le contrat
`js_deps`, le scan statique, et la décision statique/dynamique par
capacité (§5.2.1-§5.2.3) n'y participent pas et n'en sont pas affectés** :

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
    (§5.2.1), la balise est garantie présente — aucun test enveloppe ;
  - si **toutes** les capacités de la page sont dynamiques (§5.2.2), la
    balise entière est conditionnée à *au moins un* bit actif parmi ceux
    concernés — jamais de `<script></script>` vide ;
  - zéro capacité concernée → rien n'est émis.
- Un import ou un appel individuel reste conditionnel à son propre bit
  (`if record.js_deps & BIT != 0 { ... }`) exactement comme avant — seule
  la balise `<script>` elle-même change de granularité, jamais la logique
  par capacité.

### 5.3 Ajouter une nouvelle capacité

Exemple fil rouge : ajouter une capacité `carousel`, déclenchée soit par la
classe `carousel-embed` posée par un éditeur dans le corps d'un article
(branche dynamique), soit par un `.marius` qui la porte en dur dans son
propre layout (branche statique) — via un module `carousel.js` exportant
une fonction `boot`. Les étapes 5.3.1 à 5.3.3 sont **communes aux deux
branches** ; l'étape 5.3.4 ne concerne que la branche dynamique.

#### 5.3.1 Le module JS

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
tiers vendoré, l'importer comme décrit en §3.2.

#### 5.3.2 `theme.toml` — déclarer la capacité

Dans `assets/default/theme.toml`, sous `[scripts.capabilities]` :

```toml
[scripts.capabilities.carousel]
entry = "scripts/development/carousel.js"
markers = ["carousel-embed"]
activation = "boot"
```

- `entry` : chemin du fichier JS, relatif au dossier du thème.
- `markers` : liste des classes HTML qui déclenchent cette capacité. Peut
  contenir plusieurs entrées (cf. `range`/`range-multithumb`, une seule
  capacité, deux marqueurs qui activent le même bit). **Sert aux deux
  branches** (§5.2) : c'est cette même liste que `lower_modules_for_template`
  (build.rs) compare au HTML statique des `.marius` (§5.2.1) — mais elle
  n'alimente **jamais mécaniquement** `compute_js_deps` (§5.2.2, SQL) :
  ajouter un marqueur ici ne le fait pas apparaître comme par magie côté
  SQL, il faut l'étape 5.3.4 séparément. Les deux listes doivent rester
  synchronisées à la main.
- `activation` : nom de la fonction exportée à appeler. **Doit être un
  identifiant valide** (lettres/chiffres/underscore, ne commence pas par un
  chiffre) — il est injecté tel quel dans le code Rust généré, jamais
  échappé comme une chaîne.

#### 5.3.3 `scripts_registry.lock` — attribuer un bit

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
  échoue (`cargo:error`, volontaire — voir §6).

#### 5.3.4 `compute_js_deps` — reconnaître le marqueur côté SQL (branche dynamique — §5.2.2)

Nécessaire **uniquement si** la capacité doit pouvoir être déclenchée par du
contenu éditorial (un éditeur pose la classe dans le corps d'un article).
Si `carousel` ne doit jamais être déclenchée que par des `.marius` (§5.3.5),
cette étape peut être sautée — mais alors le bit attribué en 5.3.3 ne sera
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

#### 5.3.5 Déclencher par un `.marius` (branche statique — §5.2.1)

Rien à câbler séparément côté Rust — le scan statique
(`extract_static_class_tokens`) est générique, il compare **automatiquement**
le HTML de **chaque template** aux `markers` de **toutes** les capacités
déclarées en 5.3.2. Il suffit d'écrire le marqueur directement dans le
`.marius` concerné :

```html
<pre class="carousel-embed">
  ...
</pre>
```

Dès que ce `.marius` (ou un layout dont il hérite) contient ce marqueur
littéralement dans son HTML, `carousel.js` est émis **inconditionnellement**
pour ce template — aucun bit testé, aucun `content.body` consulté, y
compris sur les pages `STATIC_PAGES` (§8). Écrire `class="{{ some_field }}"`
ne compte **jamais** comme un marqueur statique — seule une chaîne littérale
dans le HTML du template est détectable ici, par construction (fragment-forge
ne scanne que les tokens `FlatPageToken::Static`, jamais l'intérieur d'un
`{{ champ }}`).

Si le même marqueur apparaît **à la fois** dans un `.marius` et dans le
corps d'un enregistrement rendu par ce template, une seule émission est
produite — la présence statique domine toujours, le test dynamique n'est
même pas généré pour cette capacité sur ce template (§5.2.3).

## 6. Compiler — l'ordre compte, chaque étape peut échouer isolément

L'Étape 1 est commune aux **trois** mécanismes (§1) : tout ajout —
component, bibliothèque, ou capacité — passe par le même compilateur
d'assets. L'Étape 2 est spécifique à la branche dynamique des capacités
(SQL, `content.core`) ; un component ou une bibliothèque seuls n'en ont
jamais besoin. L'Étape 3 est nécessaire dès qu'un `.marius` référence
l'asset ajouté — via `{% asset %}` (component, bibliothèque) ou via
l'émission générée pour une capacité.

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
`validate_capabilities`/`{% asset %}` iront lire. Sans cette étape, une
clé peut être parfaitement déclarée et pourtant introuvable :
- capacité/component : `cargo build` échouera avec *« clé 'carousel.js'
  absente du manifeste d'assets »* ;
- bibliothèque référencée depuis un script : le build de `marius-assets`
  lui-même échoue avec `AssetNotFound` (§3.2) — jamais un 404 silencieux
  découvert plus tard.

### Étape 2 — Recharger le schéma SQL (capacité, branche dynamique uniquement)

Uniquement si `02_systems.sql` (ou tout autre fichier sous `db/`) a changé —
**inutile pour un component ou une bibliothèque, et inutile si la capacité
n'est déclenchée que statiquement (§5.3.5), sans passer par 5.3.4** :

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

### Étape 3 — Compiler le schéma Rust (dès qu'un `.marius` référence l'asset)

```bash
cargo build
```

C'est ici que `validate_capabilities` (crates/core/schema/build.rs) lit
`theme.toml` + `scripts_registry.lock` + `manifest.toml`, valide la
bijection et les bits — puis `lower_modules_for_template`, appelée une fois
par composant/page, scanne le HTML de chaque `.marius` (`.marius` fusionné
parent+enfant) et génère soit une émission inconditionnelle (marqueur
détecté statiquement), soit un test `if record.js_deps & BIT != 0 { ... }`
(marqueur absent statiquement). Toute résolution `{% asset %}` (component,
bibliothèque référencée directement par un template) est également
figée à cette étape. Échoue fort (jamais silencieusement) sur :
- capacité présente dans un fichier (`theme.toml`/`scripts_registry.lock`),
  absente de l'autre ;
- bit invalide (pas une puissance de deux) ou dupliqué ;
- `activation` qui n'est pas un identifiant valide ;
- `markers` vide ;
- clé `{nom}.js` (ou toute clé `{% asset %}`) absente du manifeste d'assets
  (→ retour à l'Étape 1).

### Étape 4 — `marius-dump` (transport de données, **sans rapport avec le rendu HTML**)

```bash
cargo run --bin marius-dump
```

**Correction par rapport à une version antérieure de ce guide** :
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
dans un `.marius` (§5.2.1/§5.3.5) : dans les deux cas, c'est le pack
existant de l'enregistrement qui doit être régénéré, jamais seulement le
binaire.

Prérequis souvent oublié en local : le serveur doit être en écoute
(`PgListener` abonné) **avant** l'écriture SQL — un `NOTIFY` émis pendant
que le serveur est arrêté n'est jamais rejoué à son redémarrage.

## 7. Vérifier que ça fonctionne

Avertissement commun aux trois vérifications ci-dessous, avant même de
regarder le HTML : **si la page testée existait déjà avant ce
déploiement**, l'Étape 5 (§6) est un préalable — sinon vous inspectez
encore le pack produit par l'ancien `render()`. Pour une page tout juste
créée après le déploiement du nouveau binaire, ce préalable ne se pose pas
(son premier rendu utilise déjà le nouveau code). Seules les pages
`STATIC_PAGES` échappent entièrement à cette question.

### Component ou bibliothèque seule

1. Vérifier la présence de la clé dans `build/default/manifest.toml` après
   l'Étape 1.
2. Servir la page référençant le `.marius` concerné (après Étape 5 si la
   page existait déjà), inspecter le HTML rendu : l'URL de l'asset doit
   pointer vers le fichier haché (`/scripts/main.<hash>.js`), jamais vers
   le chemin source.
3. Pour une bibliothèque : ouvrir le fichier JS produit et vérifier que
   l'`import` vers la bibliothèque a bien été réécrit vers son URL hachée
   (§3.2) — pas seulement que le script consommateur se charge.

### Branche dynamique (§5.2.2, si l'étape 5.3.4 a été faite)

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

### Branche statique (§5.2.1/5.3.5)

Aucune base de données à interroger pour la **décision** — elle est prise
au build, identique pour tous les enregistrements du template concerné.
Mais l'**artefact** d'un enregistrement donné, lui, suit une règle
différente selon le type de page :

- **`STATIC_PAGES`** : `cargo build` (`core/schema`) écrit directement le
  `.html` final. Aucune étape supplémentaire.
- **Page adossée à un `record`** (Mode Page) : même si le test lui-même
  n'est jamais généré (§5.2.3), le pack déjà servi pour un enregistrement
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

## 8. Pièges déjà rencontrés

- **`cargo build` vert ne garantit pas que le bit fonctionne réellement.**
  Il valide la cohérence structurelle (bijection, bits, manifeste) — pas
  que la base de données a bien été rechargée avec le dernier
  `compute_js_deps`. Un build vert après une modification de `02_systems.sql`
  sans rechargement SQL (Étape 2) compile un code parfaitement correct...
  qui ne sera jamais déclenché par aucun contenu réel.
- **`cargo build` vert + serveur relancé ne régénère aucun pack HTML déjà
  servi** (cf. `runtime-lifecycle-guide.md`, et Étape 5 ci-dessus). C'est
  le piège le plus trompeur des trois mécanismes de ce guide, parce
  qu'aucune erreur ne le signale : la page se sert normalement, juste avec
  l'ancien contenu. Si un changement de template, de component ou de
  capacité n'apparaît toujours pas après un déploiement propre, vérifier
  en priorité si un `NOTIFY` a réellement été déclenché pour la page
  testée avant de chercher une erreur dans le template ou dans
  `theme.toml`.
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
  `base.marius` (§5.2.1/5.3.5) — ce n'est pas une exception, c'est la
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
- **Les alias `_0`/`_1`/… (§5.2.4) ne sont jamais stables d'une page à
  l'autre.** Ils dépendent de la liste des capacités réellement présentes
  sur CETTE page précise — `map` peut être `_0` sur une page et `_2` sur
  une autre. Ne rien coder côté JS qui suppose un alias fixe pour une
  capacité donnée ; ces alias sont strictement internes au bloc généré,
  jamais une API.
- **Un `import` vers une bibliothèque non vendorée, ou avec un chemin
  incorrect, échoue au build de `marius-assets`, jamais au runtime.** Si
  vous cherchez un 404 navigateur pour une bibliothèque, le build a
  probablement été lancé avant l'ajout de `[libraries.*]`/le dépôt des
  fichiers — pas un bug du pipeline de résolution.
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
  