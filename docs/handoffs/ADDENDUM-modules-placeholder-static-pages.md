# Addendum — HANDOFF-js-deps-capacites-frontend-v2.md
## `ModulesPlaceholder` rencontré par deux générateurs distincts

Arbitré en session, à annexer au handoff. Ne remplace aucune section
existante — précise un angle mort du lowering AOT de `js_deps` découvert à
l'implémentation du chantier 3.

### Constat

`<!-- MARIUS_MODULES -->` vit dans `base.marius`, layout **unique et
partagé** par deux pipelines de `crates/core/schema/build.rs` :

- `resolve_page_template` (Mode Page, composant réel adossé à
  `fetch_component_list` — un `record: &{Table}Row` existe dans le scope
  généré).
- `resolve_static_page` (`STATIC_PAGES`, ex. `offline` — `SchemaIndex`
  toujours vide par garde-fou explicite, **aucun `record` n'existe**).

Le même token `FlatPageToken::ModulesPlaceholder` est donc atteignable par
les deux générateurs (`generate_aot_snippet`/`generate_segmented_snippet`
d'un côté, `emit_static_html` de l'autre), avec des scopes structurellement
différents.

### Invariant retenu (Option A)

`MARIUS_MODULES` signifie *« projeter les capacités `js_deps` du contexte
de rendu »*. Une page statique ne possède ni `record`, ni état éditorial,
ni `js_deps` — son ensemble de capacités est donc **par définition vide**.
Le lowering produit alors **zéro octet**.

Ce n'est pas un no-op accidentel ni un cas d'erreur : c'est le comportement
normal du lowering de `ModulesPlaceholder` dans le pipeline `STATIC_PAGES`,
au même titre que sa projection vers `record.js_deps & BIT != 0 { ... }`
est le comportement normal dans le pipeline Mode Page.

`base.marius` reste unique et partagé. Pas de second layout, pas d'échec de
`emit_static_html` sur la simple rencontre de `ModulesPlaceholder`.

### Portée de l'invariant

C'est une **propriété du contexte de lowering**, pas une propriété du token
lui-même ni une nouvelle décision d'architecture profonde : le même
`FlatPageToken::ModulesPlaceholder`, injecté par le même mécanisme de
splice textuel (`split_static_at_marker`, symétrique à
`SCRIPTS_PLACEHOLDER`), se résout différemment selon que l'appelant
possède ou non un `record` :

| Appelant | `record` | Résolution de `ModulesPlaceholder` |
|---|---|---|
| `resolve_page_template` | oui | vue calculée par `build_modules_lowering` (bit → URL/activation), injectée en amont |
| `resolve_static_page` | non | `0` / `""` codés en dur, jamais consultée `ModulesLowering` |

En dynamique (`resolve_page_template`), toute résolution d'un bit vers
`(URL, activation)` reste soumise **sans exception** aux validations de
build déjà actées (bijection `theme.toml` ↔ `scripts_registry.lock`,
validité/unicité des bits, existence de l'entrée dans le manifeste
d'assets, `activation` identifiant valide, `markers` non vide) — l'absence
de `record` en `STATIC_PAGES` ne dispense jamais ces validations pour les
pages qui, elles, en possèdent un.

### Implémentation (chantier 3, clos)

- `FlatPageToken::ModulesPlaceholder` : un seul nouveau variant ajouté à
  l'AST gelé de `fragment-forge` — aucune autre extension.
- `resolve_and_measure` reçoit un `modules_static_bytes: usize` ;
  `generate_aot_snippet`/`generate_segmented_snippet` reçoivent un
  `modules_snippet: &str` (code Rust déjà assemblé, inséré verbatim).
  `fragment-forge` ne connaît et ne doit connaître aucune des trois sources
  (`theme.toml`, `scripts_registry.lock`, `AssetManifest`).
- `resolve_page_template` : splice inconditionnel de `MODULES_PLACEHOLDER`,
  valeurs fournies par `ModulesLowering` (calculé une seule fois par
  `build_modules_lowering`, avant toute connexion Postgres).
- `resolve_static_page` : splice inconditionnel identique, mais `0`/`""`
  codés en dur — aucune dépendance à `ModulesLowering`.
- `emit_static_html` : `FlatPageToken::ModulesPlaceholder => {}` — arm
  explicite et intentionnel, pas un `cargo:error` (contrairement à
  `Field`/`IfBool`/`EndIf`, dont la rencontre signale un bug réel en amont).
