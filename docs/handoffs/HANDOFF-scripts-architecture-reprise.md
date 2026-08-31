# HANDOFF — Architecture scripts/capacités : état actuel et questions ouvertes

> Note de reprise, lecture seule. Rédigée à partir de l'état réel du dépôt
> (fichiers effectivement inspectés cette session : `config.rs`, `main.rs`,
> `manifest.rs`, `scripts.rs`, `verbatim.rs`, `build.rs`,
> `lib.rs`/fragment-forge), jamais de mémoire de conversation seule.
> Aucune modification apportée au dépôt en produisant ce document.
>
> Légende (reprise de `HANDOFF-js-deps-capacites-frontend-v2.md`) :
> 🟢 vérifié sur pièce cette session — 🟡 vérifié mais avec une limite
> explicite (fichier non disponible, comportement non testé) — 🔴 question
> ouverte, non tranchée.

## 1. État architectural actuel

### Pipeline global

```
theme.toml
   │
   ▼
marius-assets (crates/assets)  ─────────────────────────────► manifest.toml
   │  lit : [scripts.components], [scripts.capabilities.*].entry,       │
   │        [libraries.*], [static.verbatim]                             │
   │  ignore : markers, activation, deps (jamais lus, jamais résolus)    │
   ▼                                                                     ▼
                                                    crates/core/schema/build.rs
                                                       lit : manifest.toml +
                                                       theme.toml (SA PROPRE
                                                       désérialisation, indépendante)
                                                       + scripts_registry.lock
                                                             │
                                                             ▼
                                                  validate_capabilities()
                                                  résolution AOT entry + deps
                                                             │
                                                             ▼
                                                  lower_modules_for_template()
                                                  (par template, scan statique
                                                  .marius + record.js_deps)
                                                             │
                                                             ▼
                                              render_modules_as_rust() /
                                              render_modules_as_static_html()
                                              → snippet injecté dans render()
                                              généré, ou HTML final littéral
                                              (STATIC_PAGES)
```

**Séparation stricte confirmée sur pièce** : `crates/assets` (marius-assets)
et `crates/core/schema` (build.rs) ont chacun leur **propre**
désérialisation de `theme.toml`, aucun type Rust partagé entre les deux
crates (`config.rs` local à chacun). Le seul canal de communication entre
les deux est `manifest.toml`.

### Ce que `marius-assets` consomme (🟢)

- `[scripts.components]` — `HashMap<String, String>` (nom → chemin). Aucun
  autre champ possible (`config.rs:109-113`).
- `[scripts.capabilities.*].entry` — seul champ réellement utilisé de
  `CapabilityConfig` (`main.rs:230-244`, fusionné avec `components` dans
  `script_targets` avant `run_scripts_pipeline`).
- `markers`, `activation`, `deps` de `CapabilityConfig` — désérialisés
  (nécessaire pour que le TOML parse), mais **jamais lus** par ce binaire.
  Annotés `#[allow(dead_code)]` avec commentaire explicite
  (`config.rs:143-172`) : leur consommation est exclusivement le fait de
  `build.rs`, via une désérialisation indépendante.
- `[libraries.*]` (`root`, `module`) et `[static.verbatim].files` —
  fusionnés dans une liste unique avant `run_verbatim_pipeline`
  (`main.rs`), à l'exception du garde-fou `.js` (voir plus bas).

### Ce que `build.rs` consomme (🟢)

- `manifest.toml` intégral (`assets: HashMap<String, AssetEntry>` +
  `classic_scripts: Vec<String>`).
- `theme.toml` — mais **seulement** `[scripts.capabilities.*]`
  (`entry`, `markers`, `activation`, `deps`) via `ThemeTomlScriptsOnly`
  (`build.rs:97-99`). Ne lit jamais `[scripts.components]`,
  `[libraries.*]`, `[static.verbatim]` — ces sections ne concernent que la
  production du manifeste, jamais sa consommation côté schéma.
- `scripts_registry.lock` (voir §2).

### Identité canonique d'un asset (🟢, décision de cette session)

Invariant désormais uniforme : **la clé manifeste d'un asset est son
chemin relatif à la racine du thème, jamais un nom de configuration**.

- `[scripts.components]`/`[scripts.capabilities.*]` : la clé est
  `components[*target_name]` (la chaîne `entry` telle qu'écrite dans
  `theme.toml`, ex. `"scripts/map.js"`) — **jamais**
  `format!("{name}.js")` (ancienne convention, corrigée cette session,
  `scripts.rs:640-648`).
- Modules dépendance transitifs (imports ESM relatifs, jamais déclarés
  dans `theme.toml`) : la clé est construite depuis `arena[idx].path`
  (chemin filesystem **canonicalisé, donc absolu**), ramené au format
  relatif via `strip_prefix(&theme_dir_canonical)` — `theme_dir_canonical`
  calculé une fois (`theme_dir.canonicalize()`), jamais `theme_dir` brut
  (`scripts.rs:610-621, 694-704`). Correctif de cette session : un
  `theme_dir` relatif (cas réel de `main.rs`, jamais canonicalisé côté
  CLI) produisait auparavant une clé absolue préfixée `//`.
- `[libraries.*]`/`[static.verbatim]`, `styles.rs`, `sprites.rs` :
  construisent déjà leur clé depuis une chaîne relative connue (jamais
  depuis un chemin filesystem résolu) — jamais concernés par la classe de
  bug ci-dessus.
- Type porteur : `CanonicalAssetId` (`manifest.rs`), newtype sur `String`,
  unique par construction (SPEC-canonical-asset-identity.md).

### `[libraries.*]`, `module`, `classic_scripts` (🟢)

- `LibraryConfig.module: bool`, défaut `true` (`config.rs:80-97`) —
  ESM-first, `module = false` concession explicite (UMD/classique).
- `main.rs` construit `classic_scripts: Vec<String>` — liste **sparse**
  des clés canoniques des seules bibliothèques `module = false`
  (`main.rs`, boucle `for name in library_names`). Absence de clé = ESM.
- Sérialisé comme champ **sœur** de `assets` dans `AssetManifest`, jamais
  un champ d'`AssetEntry` (`manifest.rs`) — décision explicite de cette
  session après un incident de désérialisation (`AssetEntry` doit rester
  un pur descripteur d'artefact, jamais porteur d'une sémantique de
  chargement).
- `build.rs` reconvertit `classic_scripts` en `HashSet<String>` à la
  lecture (`LoadedAssets`, `build.rs`), consulté uniquement lors de la
  résolution de `deps`.

### `deps` (🟢)

- `CapabilityConfig.deps: Vec<String>`, défaut vide — **uniquement** sur
  `[scripts.capabilities.*]`, aucun champ équivalent sur
  `[scripts.components]`.
- Résolution AOT entièrement dans `validate_capabilities` (`build.rs`) :
  chemin canonique → `assets.get()` → URL, échec dur si absent. Jamais un
  import injecté dans le texte source de `entry`.
- Émission : `aggregate_deps` déduplique par identité canonique (jamais
  par URL), domination statique si au moins un consommateur est statique,
  sinon OU binaire des bits. Balises émises **avant** le
  `<script type="module">` de la capacité, dans
  `render_modules_as_rust`/`render_modules_as_static_html`.

### Garde-fou `[static.verbatim]` (🟢)

`main.rs`, juste après lecture de `theme.static_.verbatim.files`, avant
toute fusion avec `[libraries.*]` : tout `.js` dans cette liste échoue le
build avec un message explicite renvoyant vers
`[scripts.components]`/`[scripts.capabilities.*]`. Ne s'applique jamais au
mécanisme interne `run_verbatim_pipeline`, partagé avec `[libraries.*]`.

## 2. `scripts_registry.lock` — cartographie du problème (🔴 non résolu)

### Rôle actuel exact

Fichier TOML plat, `HashMap<String, i64>` (nom de capacité → bit),
sibling de `theme.toml` (`build.rs:422-427`). **Lu exclusivement par
`build.rs`**, à l'intérieur de `validate_capabilities`, avant toute
génération de code — 🟡 je n'ai pas le source de `marius-dump` cette
session, je ne peux donc pas confirmer ou infirmer qu'il le lit aussi ;
l'énoncé du problème mentionne un échec `marius-dump`, mais le seul échec
que j'ai pu vérifier sur pièce est celui de `cargo build`
(`crates/core/schema`).

### Pourquoi son absence provoque un échec — mécanisme exact

`validate_capabilities` effectue une **bijection stricte et
inconditionnelle** (`build.rs:508-525`) :
- toute capacité de `[scripts.capabilities]` doit avoir une entrée active
  (ne commençant pas par `_retired_`) dans le registre ;
- toute entrée active du registre doit correspondre à une capacité
  déclarée.

Cette vérification s'exécute **avant** `lower_modules_for_template` — elle
ne sait donc rien, à ce stade, de savoir si la capacité sera un jour
réellement testée dynamiquement (`if record.js_deps & BIT != 0`) ou si
elle ne sera jamais déclenchée que statiquement (§6.2.1 du guide, cas où
le bit n'est **jamais** émis dans aucun code généré). **Aucune
distinction n'est faite dans le code entre ces deux cas** — le bit est
exigé dans les deux cas, avant même de savoir lequel s'applique.

### Ce qu'apporte le fichier séparé, par rapport à `theme.toml` lui-même

Le registre est **append-only**, un bit retiré n'est jamais réattribué
(`_retired_` préfixe, `build.rs:498-501`) — garantit qu'un bit déjà écrit
dans `content.core.js_deps` pour du contenu existant ne change jamais de
signification. 🔴 Rien dans le code n'empêche *techniquement* de porter
cette même information (un champ `bit = 128`) directement dans
`[scripts.capabilities.X]` de `theme.toml` avec la même discipline
append-only — le fichier séparé est une convention/discipline actuelle,
pas une nécessité technique démontrée sur pièce.

### Distinction conceptuelle entre les trois cas cités — état réel du code

- **(1) Capacité liée à un template `.marius` uniquement** (déclenchement
  statique exclusif) : le code la traite **identiquement** à (2) jusqu'à
  `lower_modules_for_template` — même bijection exigée, même `bit`
  attribué et jamais utilisé.
- **(2) Capacité liée à du contenu DB** (déclenchement dynamique) : seul
  cas où le bit est réellement consulté au runtime généré.
- **(3) Script frontend global** : ce cas est déjà servi, **entièrement
  en dehors** de ce mécanisme, par `[scripts.components]` — aucun bit,
  aucun registre, aucun `markers`/`activation`. Le couplage à
  `scripts_registry.lock` ne concerne donc **jamais** ce troisième cas
  dans l'état actuel du code ; la friction rapportée ne peut concerner
  que (1) vs (2).

**Constat central pour la future session** : le code actuel ne distingue
pas (1) de (2) au moment où le registre est exigé — la bijection est
totale et précède toute connaissance de l'usage réel de la capacité.

## 3. `markers` — modèle actuellement implémenté (🟢)

- **Type** : `Vec<String>`, côté config (`config.rs:147`) et côté
  `CapabilityInfo` (`build.rs`, champ `markers`).
- **Lu par** : `lower_modules_for_template` (`build.rs:693`, comparaison
  directe à un `HashSet<String>`) et, indépendamment, `compute_js_deps`
  côté SQL (non relu cette session, décrit dans HANDOFF v2 comme
  `IF v_tokens && ARRAY[...]`).
- **Prédicat réellement évalué** : `cap.markers.iter().any(|m|
  static_classes.contains(m))` — un simple test d'appartenance
  ensembliste, aucune autre logique. `static_classes` est produit par
  `extract_static_class_tokens` (`lib.rs:2703-2735`), qui ne reconnaît
  **que** l'attribut `class="..."`/`class='...'` (regex ancrée sur
  `class=` littéralement, jamais un autre nom d'attribut) et éclate sa
  valeur sur les espaces.
- **Abstraction actuelle** : **aucune**. `markers` est intrinsèquement lié
  à la notion de classe HTML — pas une couche neutre au-dessus d'un
  concept plus général de "prédicat de présence dans le HTML". Le nom du
  champ (`markers`) est déjà plus générique que son implémentation
  (`class` uniquement).
- **Structures dépendantes de cette représentation** : `CapabilityInfo`
  (Rust), `extract_static_class_tokens` (fragment-forge), la fonction SQL
  `compute_js_deps`. Les trois supposent un token de classe exact,
  jamais un `id`, un `data-*`, ou la présence d'un élément.

### 🔴 Écart avec l'intention potentielle (id/data-*/présence d'élément)

Aucune trace dans le code d'une abstraction en ce sens. Passer à un modèle
plus général toucherait au minimum trois points indépendants qui devront
rester synchronisés manuellement (déjà le cas aujourd'hui pour `class`) :
la regex `lib.rs`, le prédicat `lower_modules_for_template`, et la
fonction SQL — aucun partage de logique entre eux actuellement, donc
aucune généralisation ne serait automatique.

## 4. `activation` — cardinalité actuellement implémentée (🟢)

- **Type** : `String` unique, aucune structure `Vec` ni `Option` nulle
  part (`config.rs:151`, `CapabilityInfo.activation: String`,
  `ModuleEmission.activation: String`).
- **Propagation** : `cap.activation.clone()` → `ModuleEmission.activation`
  → un seul `import{X as _i}from"URL";` + un seul `_i();` par capacité
  émise (`build.rs`, `render_modules_as_rust`/
  `render_modules_as_static_html`).
- **Contrainte de validité** : doit être un identifiant JS valide
  (vérifié par `is_valid_identifier`, `build.rs:435`) — injecté tel quel
  dans le code généré, jamais échappé comme chaîne.
- **Invariant actuel** : une capacité = un import = un appel. Aucune
  notion de plusieurs points d'activation pour une même capacité.

### 🔴 Implication structurelle d'un passage à `activation = [...]`

Toucherait au minimum : `CapabilityConfig.activation` (type),
`CapabilityInfo.activation` (type), `ModuleEmission.activation` (type),
et la boucle de génération dans les deux fonctions de rendu — qui devrait
produire N imports + N appels par capacité au lieu d'un seul, avec un
schéma de nommage d'alias à étendre (actuellement un compteur global `_i`
par émission de capacité, jamais imbriqué). Le mécanisme de
dédoublonnage/agrégation de `deps` (`aggregate_deps`) n'est en revanche
pas concerné — il opère sur les dépendances, jamais sur les activations.

## 5. Invariants à préserver

- **AOT** : aucune section de `theme.toml`/`scripts_registry.lock` n'est
  jamais relue au runtime — tout est figé à `cargo build`/
  `marius-assets`.
- **Aucune interprétation runtime de `theme.toml`** — seul
  `record.js_deps` (déjà un entier) est testé au runtime.
- **Séparation crates/assets ↔ crates/core/schema** : aucun type Rust
  partagé, seul `manifest.toml` fait le pont.
- **Identité canonique = chemin relatif au thème**, jamais un nom de
  configuration ni un chemin absolu (§1) — vérifié cette session pour
  `scripts.rs`, déjà vrai ailleurs.
- **Dédoublonnage déterministe** : `aggregate_deps` par identité
  canonique, jamais par URL ; ordre de première apparition, jamais un
  ordre d'itération de `HashMap`.
- **ESM-first** : `module` défaut `true`, `false` concession explicite
  uniquement sur `[libraries.*]`.
- **`AssetEntry` = pur descripteur d'artefact** — jamais de champ propre à
  un mécanisme de consommation (`module`/`deps` vivent ailleurs,
  `classic_scripts` en particulier).
- **`[static.verbatim]` interdit au `.js`** — garde-fou actif dans
  `main.rs`.
- **Capacité sans `deps` = comportement strictement antérieur** — chemin
  de code vide, jamais une branche spéciale.
- **Domination statique sur dépendance partagée** : si un seul
  consommateur d'une `deps` est statique, elle devient inconditionnelle,
  quel que soit le nombre de consommateurs dynamiques.
- **Bijection stricte `theme.toml` ↔ `scripts_registry.lock`** (invariant
  actuel, remis en question au §2 — à préserver **tant que** la question
  ouverte n'est pas tranchée, pas à contourner silencieusement).
- **`_retired_` jamais réattribué** — un bit retiré reste dans le fichier
  pour mémoire, jamais recyclé pour un nom différent.

## Questions ouvertes — NE PAS RÉSOUDRE

1. `scripts_registry.lock` est-il réellement nécessaire pour **toutes**
   les capacités, y compris celles dont l'unique déclenchement est
   statique (§2) ?
2. Faut-il généraliser le modèle de `markers` au-delà des classes HTML
   (§3) ?
3. `activation` doit-il accepter plusieurs activations (§4) ?
