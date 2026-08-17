# Guide `js_deps` — capacités frontend conditionnelles

Référence : `HANDOFF-js-deps-capacites-frontend-v2.md` (implémentation
d'origine). Ce guide en est la version d'usage courant — pas d'historique de
décisions, juste : comprendre le système, l'étendre, le compiler.

## 1. Le problème que ça résout

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

## 2. Suivre la donnée — vue d'ensemble

Deux chemins, indépendants, qui convergent au même endroit du HTML final.

### 2.1 Branche statique — détection AOT dans les templates

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

### 2.2 Branche dynamique — détection à l'écriture, décision au rendu

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

### 2.3 Convergence — un seul point d'agrégation, jamais deux émissions

```
┌──────────────────────┐   ┌──────────────────────┐
│ Émission statique    │   │ Émission dynamique   │
│ (2.1, si marqueur    │   │ (2.2, si marqueur    │
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
              (§2.4), présent UNIQUEMENT si au moins un besoin
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

### 2.4 Sérialisation finale — un seul `<script>` par page, jamais un par capacité

Ce qui précède (§2.1 à §2.3) décide **quelles** capacités sont nécessaires
pour une page donnée. Une fois cette liste établie, la sérialisation finale
regroupe **tout** dans une seule balise, plutôt que d'émettre un
`<script type="module">` distinct par capacité :

```html
<!-- Avant regroupement (une balise par capacité) -->
<script type="module">import{boot as _n}from"/scripts/line-mark.b0fd8.js";_n();</script><script type="module">import{initMapsSystem as _n}from"/scripts/map.a6994.js";_n();</script>

<!-- Après regroupement (une seule balise) -->
<script type="module">import{boot as _0}from"/scripts/line-mark.b0fd8.js";import{initMapsSystem as _1}from"/scripts/map.a6994.js";_0();_1();</script>
```

Règles de ce regroupement, purement une affaire de sortie — **le contrat
`js_deps`, le scan statique, et la décision statique/dynamique par
capacité (§2.1-§2.3) n'y participent pas et n'en sont pas affectés** :

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
    (§2.1), la balise est garantie présente — aucun test enveloppe ;
  - si **toutes** les capacités de la page sont dynamiques (§2.2), la
    balise entière est conditionnée à *au moins un* bit actif parmi ceux
    concernés — jamais de `<script></script>` vide ;
  - zéro capacité concernée → rien n'est émis.
- Un import ou un appel individuel reste conditionnel à son propre bit
  (`if record.js_deps & BIT != 0 { ... }`) exactement comme avant — seule
  la balise `<script>` elle-même change de granularité, jamais la logique
  par capacité.

## 3. Ajouter une nouvelle capacité

Exemple fil rouge : ajouter une capacité `carousel`, déclenchée soit par la
classe `carousel-embed` posée par un éditeur dans le corps d'un article
(branche dynamique), soit par un `.marius` qui la porte en dur dans son
propre layout (branche statique) — via un module `carousel.js` exportant
une fonction `boot`. Les étapes 3.1 à 3.3 sont **communes aux deux
branches** ; l'étape 3.4 ne concerne que la branche dynamique.

### 3.1 Le module JS

Créer `assets/default/scripts/development/carousel.js`, avec une fonction
exportée nommée (c'est cette fonction qui sera appelée automatiquement) :

```js
export function boot() {
  // ...
}
```

Le nom de la fonction est libre — il sera référencé tel quel dans
`theme.toml` (`activation`). Convention observée dans ce projet : `init` ou
un nom court et explicite (`boot`, `mount`).

### 3.2 `theme.toml` — déclarer la capacité

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
  branches** (§2) : c'est cette même liste que `lower_modules_for_template`
  (build.rs) compare au HTML statique des `.marius` (§2.1) — mais elle
  n'alimente **jamais mécaniquement** `compute_js_deps` (§2.2, SQL) :
  ajouter un marqueur ici ne le fait pas apparaître comme par magie côté
  SQL, il faut l'étape 3.4 séparément. Les deux listes doivent rester
  synchronisées à la main.
- `activation` : nom de la fonction exportée à appeler. **Doit être un
  identifiant valide** (lettres/chiffres/underscore, ne commence pas par un
  chiffre) — il est injecté tel quel dans le code Rust généré, jamais
  échappé comme une chaîne.

### 3.3 `scripts_registry.lock` — attribuer un bit

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
  échoue (`cargo:error`, volontaire — voir §4).

### 3.4 `compute_js_deps` — reconnaître le marqueur côté SQL (branche dynamique — §2.2)

Nécessaire **uniquement si** la capacité doit pouvoir être déclenchée par du
contenu éditorial (un éditeur pose la classe dans le corps d'un article).
Si `carousel` ne doit jamais être déclenchée que par des `.marius` (§3.5),
cette étape peut être sautée — mais alors le bit attribué en 3.3 ne sera
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

### 3.5 Déclencher par un `.marius` (branche statique — §2.1)

Rien à câbler séparément côté Rust — le scan statique
(`extract_static_class_tokens`) est générique, il compare **automatiquement**
le HTML de **chaque template** aux `markers` de **toutes** les capacités
déclarées en 3.2. Il suffit d'écrire le marqueur directement dans le
`.marius` concerné :

```html
<pre class="carousel-embed">
  ...
</pre>
```

Dès que ce `.marius` (ou un layout dont il hérite) contient ce marqueur
littéralement dans son HTML, `carousel.js` est émis **inconditionnellement**
pour ce template — aucun bit testé, aucun `content.body` consulté, y
compris sur les pages `STATIC_PAGES` (§6). Écrire `class="{{ some_field }}"`
ne compte **jamais** comme un marqueur statique — seule une chaîne littérale
dans le HTML du template est détectable ici, par construction (fragment-forge
ne scanne que les tokens `FlatPageToken::Static`, jamais l'intérieur d'un
`{{ champ }}`).

Si le même marqueur apparaît **à la fois** dans un `.marius` et dans le
corps d'un enregistrement rendu par ce template, une seule émission est
produite — la présence statique domine toujours, le test dynamique n'est
même pas généré pour cette capacité sur ce template (§2.3).

## 4. Compiler — l'ordre compte, chaque étape peut échouer isolément

Chacune des quatre étapes suivantes lit un état écrit par la précédente.
Sauter une étape ne produit **pas toujours** une erreur immédiate — parfois
juste un comportement silencieusement obsolète (cas vécu cette session :
`cargo build` qui passe sur un manifeste d'assets périmé). Toujours exécuter
les quatre dans l'ordre après une modification de `theme.toml`/
`scripts_registry.lock`/`02_systems.sql`.

### Étape 1 — Recompiler les assets

```bash
cargo run --release --bin marius-assets -- ./assets/default
```

Régénère `build/default/manifest.toml` — c'est lui qui contient l'URL
hachée (`/scripts/carousel.HASH.js`) que `validate_capabilities` ira lire.
Sans cette étape, le bit peut être parfaitement déclaré et pourtant
introuvable : `cargo build` échouera avec *« clé 'carousel.js' absente du
manifeste d'assets »*.

### Étape 2 — Recharger le schéma SQL

Uniquement si `02_systems.sql` (ou tout autre fichier sous `db/`) a changé —
**inutile si la capacité n'est déclenchée que statiquement (§3.5), sans
passer par 3.4** :

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

### Étape 3 — Compiler le schéma Rust

```bash
cargo build
```

C'est ici que `validate_capabilities` (crates/core/schema/build.rs) lit
`theme.toml` + `scripts_registry.lock` + `manifest.toml`, valide la
bijection et les bits — puis `lower_modules_for_template`, appelée une fois
par composant/page, scanne le HTML de chaque `.marius` (`.marius` fusionné
parent+enfant) et génère soit une émission inconditionnelle (marqueur
détecté statiquement), soit un test `if record.js_deps & BIT != 0 { ... }`
(marqueur absent statiquement). Échoue fort (jamais silencieusement) sur :
- capacité présente dans un fichier (`theme.toml`/`scripts_registry.lock`),
  absente de l'autre ;
- bit invalide (pas une puissance de deux) ou dupliqué ;
- `activation` qui n'est pas un identifiant valide ;
- `markers` vide ;
- clé `{nom}.js` absente du manifeste d'assets (→ retour à l'Étape 1).

### Étape 4 — Régénérer le `store.bin` de la table concernée

```bash
cargo run --bin marius-dump
```

Nécessaire dès qu'un `#[repr(C)]` généré change de taille — **c'est le cas
à chaque fois qu'une capacité change**, puisque `js_deps` (ou tout autre
champ du `StorageRow`) reste identique en octets, mais le *contenu* rendu
change. Concrètement : cette étape est surtout critique après un `ALTER
TABLE` (colonne ajoutée/retirée), pas après un simple ajout de capacité —
mais en cas de doute, la relancer ne coûte rien et évite un
`[PackfileReader] stride incohérent` au démarrage du serveur.

**Limitation actuelle : `marius-dump` ne couvre que `content_core`.**
Aucune autre table n'a de mécanisme de dump à ce jour (cf.
`docs/guides/../SUIVI-js-deps-points-en-attente.md`, §1).

## 5. Vérifier que ça fonctionne

### Branche dynamique (§2.2, si l'étape 3.4 a été faite)

1. Écrire ou modifier un document dont le corps contient le marqueur
   (`class="carousel-embed"` quelque part dans `content.body.content`).
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

### Branche statique (§2.1/3.5)

Aucune base de données à interroger — l'émission est décidée au build,
identique pour tous les enregistrements du template concerné :

1. Vérifier que le marqueur est bien présent, littéralement, dans le HTML
   du `.marius` (pas dans un `{{ champ }}`).
2. `cargo build`, puis servir n'importe quelle page rendue par ce
   template — même un document dont `js_deps` vaut `0` doit afficher le
   `<script type="module">` : la présence vient du template, jamais du
   contenu.

## 6. Pièges déjà rencontrés

- **`cargo build` vert ne garantit pas que le bit fonctionne réellement.**
  Il valide la cohérence structurelle (bijection, bits, manifeste) — pas
  que la base de données a bien été rechargée avec le dernier
  `compute_js_deps`. Un build vert après une modification de `02_systems.sql`
  sans rechargement SQL (Étape 2) compile un code parfaitement correct...
  qui ne sera jamais déclenché par aucun contenu réel.
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
  `base.marius` (§2.1/3.5) — ce n'est pas une exception, c'est la partie
  statique qui, elle, ne dépend jamais d'un `record`. Ne pas confondre
  « aucun bit à tester » (toujours vrai pour `STATIC_PAGES`) avec « ne
  peut jamais avoir besoin d'un module » (faux).
- **Ne pas confondre : le test dynamique n'est même pas généré pour une
  capacité dont le marqueur est présent statiquement dans le template en
  cours** — pas une histoire de « les deux s'exécutent, l'un des deux est
  redondant ». Si un module semble absent d'un `<head>` alors que
  `js_deps` porte bien le bit correspondant, vérifiez d'abord si ce
  template (ou un layout dont il hérite) porte déjà le marqueur en dur :
  l'émission a probablement déjà eu lieu, inconditionnelle — pas un bug.
- **Les alias `_0`/`_1`/… (§2.4) ne sont jamais stables d'une page à
  l'autre.** Ils dépendent de la liste des capacités réellement présentes
  sur CETTE page précise — `map` peut être `_0` sur une page et `_2` sur
  une autre. Ne rien coder côté JS qui suppose un alias fixe pour une
  capacité donnée ; ces alias sont strictement internes au bloc généré,
  jamais une API.
