# Guide `js_deps` — capacités frontend conditionnelles

Référence : `HANDOFF-js-deps-capacites-frontend-v2.md` (implémentation
d'origine). Ce guide en est la version d'usage courant — pas d'historique de
décisions, juste : comprendre le système, l'étendre, le compiler.

## 1. Le problème que ça résout

Certains contenus éditoriaux ont besoin d'un module JS spécifique côté
client — une vidéo YouTube embarquée, un slider `range`, une image avec
point de focus. Charger ces modules **sur toutes les pages**, qu'elles en
aient besoin ou non, gaspille de la bande passante. Les charger **au cas par
cas côté client** (détection DOM, `IntersectionObserver` sur des sélecteurs)
ajoute une passe JS avant que quoi que ce soit ne s'exécute.

`js_deps` résout ça en amont, côté base de données : à chaque écriture du
corps d'un document, un trigger SQL scanne le HTML, détecte quelles
capacités sont réellement utilisées, et stocke le résultat sous forme de
bitset. Le compilateur AOT (`db-forge`) lit ce bitset à la génération et
insère, **dans le code Rust compilé**, un test conditionnel direct
(`if record.js_deps & BIT != 0 { ... }`) qui n'émet le `<script>` que si le
bit correspondant est actif. Zéro détection runtime, zéro JS chargé pour
rien — la décision est prise une fois, à l'écriture, jamais à chaque rendu.

## 2. Suivre la donnée — vue d'ensemble

```
┌─────────────────────┐
│  Éditeur écrit du    │   Le corps HTML contient une classe
│  contenu (content.   │   marqueur, ex. class="figure-image-focus"
│  body.content)       │
└──────────┬───────────┘
           │ INSERT/UPDATE (trigger AFTER)
           ▼
┌─────────────────────────────┐
│ content.fn_sync_js_deps()   │   Appelle compute_js_deps(NEW.content),
│ (db/05_content/             │   compare à la valeur existante, UPDATE
│  02_systems.sql)            │   conditionnel (jamais si inchangé)
└──────────┬───────────────────┘
           ▼
┌─────────────────────────────┐
│ content.core.js_deps         │   BIGINT — un bit par capacité,
│ (bitset)                     │   ex. 16 = image-focus actif
└──────────┬───────────────────┘
           │ lu au build (cargo build), PAS au runtime
           ▼
┌─────────────────────────────┐
│ build_modules_lowering()     │   Lit theme.toml [scripts.capabilities]
│ (crates/core/schema/         │   + scripts_registry.lock + manifest.toml,
│  build.rs)                   │   assemble le code Rust conditionnel
└──────────┬───────────────────┘
           ▼
┌─────────────────────────────┐
│ Code Rust généré             │   if record.js_deps & 16 != 0 {
│ (target/.../generated_       │     buf.push_str(r#"<script type="module">
│  schema.rs)                   │     import{init as _n}from"/scripts/
│                               │     image-focus.HASH.js";_n();</script>"#);
│                               │   }
└──────────┬───────────────────┘
           │ à chaque rendu HTTP (record réel, bit réellement testé)
           ▼
┌─────────────────────────────┐
│ HTML servi au navigateur     │   <script> présent UNIQUEMENT si la
│                               │   capacité est réellement utilisée
└───────────────────────────────┘
```

**Point clé à retenir :** la détection (scan du HTML) se fait **une fois, à
l'écriture**, côté SQL. La décision d'émission (quel `<script>` imprimer) se
fait **à la compilation**, côté Rust — le bit lui-même est juste lu et
testé au runtime, jamais recalculé. Trois fichiers doivent donc rester
synchronisés à la main : `theme.toml`, `scripts_registry.lock`, et le corps
de `compute_js_deps()`. Rien ne les régénère automatiquement les uns à
partir des autres.

## 3. Ajouter une nouvelle capacité

Exemple fil rouge : ajouter une capacité `carousel`, déclenchée par la
classe `carousel-embed`, via un module `carousel.js` exportant une fonction
`boot`.

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
  capacité, deux marqueurs qui activent le même bit).
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

### 3.4 `compute_js_deps` — reconnaître le marqueur côté SQL

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
hachée (`/scripts/carousel.HASH.js`) que `build_modules_lowering` ira lire.
Sans cette étape, le bit peut être parfaitement déclaré et pourtant
introuvable : `cargo build` échouera avec *« clé 'carousel.js' absente du
manifeste d'assets »*.

### Étape 2 — Recharger le schéma SQL

Uniquement si `02_systems.sql` (ou tout autre fichier sous `db/`) a changé :

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

C'est ici que `build_modules_lowering` (crates/core/schema/build.rs) lit
`theme.toml` + `scripts_registry.lock` + `manifest.toml`, valide la
bijection et les bits, et génère le code Rust conditionnel. Échoue fort
(jamais silencieusement) sur :
- capacité présente dans un fichier, absente de l'autre ;
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
  capacités actives**, par construction (pas de `record`, pas de `js_deps`).
  `<!-- MARIUS_MODULES -->` y produit toujours zéro octet — normal, pas un
  bug si un module n'apparaît jamais sur ces pages.
