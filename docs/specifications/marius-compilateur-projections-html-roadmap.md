Fichier lu. Voici la lecture de la situation avant de générer la roadmap.

**Diagnostic du lib.rs actuel :** La Phase 0 implémente le pipeline _schema-introspection → render()_ (FieldSpec + VarlenField → corps Rust généré). C'est validé. La cible de la roadmap est le pipeline **template → render_page()** : parsing du `.marius`, calcul de `PAGE_TOTAL_CAP`, émission du corps. Ces deux pipelines coexistent dans la même crate.

---

# ROADMAP FRAGMENT-FORGE — v0.3 → v1.0

## PHASE 1 — PIPELINE DE PARSING `.marius`

---

### Phase 1.1 — Refactoring AST : élimination des `String`

**Objectif Technique Unique**
Introduire le paramètre de lifetime `'src` sur `FlatPageToken`. Chaque `String` devient `&'src str` lié à la durée de vie de la source brute. Aucune logique de parsing n'est écrite ici.

**Contrat Entrée/Sortie**

```
Entrée : FlatPageToken { Static(String), Field { entity: String, field: String }, … }
Sortie : FlatPageToken<'src> { Static(&'src str), Field { entity: &'src str, field: &'src str }, … }
```

`FieldSpec::name` et `VarlenField::name` restent `String` — ils viennent de `pg_attribute` (owned, build-time). Seul le token AST est impacté.

**Périmètre Algorithmique**

- ✅ Ajouter `<'src>` sur l'enum et tous les variants
- ✅ Ajuster `StaticInclude` : `original_path: &'src str`, `rel_from_manifest: &'src str`, `len: usize` (len reste owned, valeur scalaire)
- ❌ Interdiction d'écrire un scanner, un parser ou toute logique de construction

**Jalon Vert**
`cargo check` passe sans warning. Un test unitaire construit un `FlatPageToken::Static("hello")` en liant `"hello"` à une `&'static str` locale et vérifie que le type compile sans lifetime explicite.

**Clause d'Échappement**

> « Phase 1.1 bloquée : les invariants de lifetime sur `StaticInclude` sont contradictoires avec le contexte de build.rs. Informations manquantes : durée de vie de la source lue depuis le disque. »

---

### Phase 1.2 — Scanner Lexical Isolé

**Objectif Technique Unique**
Découper `src: &'src str` en une séquence de `RawSpan<'src>` — des slices annotées par leur catégorie syntaxique. Aucune sémantique n'est résolue ici.

**Contrat Entrée/Sortie**

```
Entrée  : src: &'src str
Sortie  : impl Iterator<Item = RawSpan<'src>>

RawSpan<'src> = { slice: &'src str, kind: SpanKind }
SpanKind = Literal | ExprOpen | ExprClose | BlockOpen | BlockClose | Ident | Punct
```

**Périmètre Algorithmique**

- ✅ Opérations sur `&str` uniquement : `find`, `split_at`, indexation de bytes
- ✅ Reconnaissance des délimiteurs `{{`, `}}`, `{%`, `%}` par scan de bytes
- ❌ Interdiction de lire le disque
- ❌ Interdiction de construire un `FlatPageToken`
- ❌ Interdiction d'allouer : ni `String`, ni `Vec` dans le corps du scanner

**Jalon Vert**
Test unitaire : `scan("hello {{ user.name }} world")` produit exactement `[Literal("hello "), ExprOpen("{{"), Ident("user"), Punct("."), Ident("name"), ExprClose("}}"), Literal(" world")]`. Asserté span-par-span sur les slices.

**Clause d'Échappement**

> « Phase 1.2 bloquée : la grammaire des délimiteurs `{% %}` versus `{{ }}` n'est pas spécifiée pour les cas de bord (espaces intérieurs, nested). Fournir la spec lexicale exacte. »

---

### Phase 1.3 — Classifieur de Tokens

**Objectif Technique Unique**
Transformer le flux de `RawSpan<'src>` en `Vec<FlatPageToken<'src>>`. Le classifieur est un automate à états finis sur le flux de spans.

**Contrat Entrée/Sortie**

```
Entrée  : impl Iterator<Item = RawSpan<'src>>
Sortie  : Result<Vec<FlatPageToken<'src>>, ParseError>

ParseError = UnexpectedSpan { kind: SpanKind, offset: usize }
           | UnknownBlockKeyword { keyword: Box<str> }
           | MalformedField { raw: Box<str> }
```

**Périmètre Algorithmique**

- ✅ Résolution des patterns : `{{ entity.field }}` → `Field`, `{% if entity.field %}` → `IfBool`, `{% endif %}` → `EndIf`, `{% include path %}` → `StaticInclude`
- ✅ Lecture de `len` pour `StaticInclude` via `std::fs::metadata` (seule I/O disque autorisée dans cette phase, pour connaître la longueur du fichier inclus à la compilation)
- ❌ Interdiction de valider l'existence des entités/champs dans le schéma
- ❌ Interdiction de calculer une capacité

**Jalon Vert**
Test : un template complet avec 1 `Static`, 1 `Field`, 1 `IfBool/EndIf`, 1 `StaticInclude` produit un `Vec` de 5 éléments avec les variants corrects. Asserté par pattern matching exhaustif.

**Clause d'Échappement**

> « Phase 1.3 bloquée : la signature de `StaticInclude` impose un `len` calculé à ce stade, mais la politique de résolution de chemin relatif (relatif à quel manifest ?) n'est pas documentée. Spécifier `rel_from_manifest`. »

---

### Phase 1.4 — Validation Sémantique

**Objectif Technique Unique**
Vérifier la cohérence de l'AST contre le schéma et les invariants structurels. Produit une liste d'erreurs exhaustive (pas de fail-fast).

**Contrat Entrée/Sortie**

```
Entrée  : tokens: &[FlatPageToken<'src>], schema: &SchemaContext
Sortie  : Result<(), Vec<SemanticError>>

SemanticError = UnknownEntity(&'src str)
              | UnknownField { entity: &'src str, field: &'src str }
              | UnbalancedIfBool { depth: isize }
              | IfBoolOnNonBoolField { entity: &'src str, field: &'src str }
```

**Périmètre Algorithmique**

- ✅ Compteur de profondeur pour IfBool/EndIf (doit atteindre 0 en fin de slice)
- ✅ Lookup `entity.field` dans `SchemaContext`
- ✅ Vérification que `IfBool` pointe sur un champ de type `FieldKind::Bool`
- ❌ Interdiction de modifier l'AST
- ❌ Interdiction de calculer une capacité ou d'émettre du code

**Jalon Vert**
Deux tests : (a) AST valide → `Ok(())`. (b) AST avec `IfBool` non équilibré + champ inconnu → `Err(vec![...])` contient exactement les deux erreurs attendues.

**Clause d'Échappement**

> « Phase 1.4 bloquée : `SchemaContext` n'est pas encore défini comme type public dans la crate. Fournir sa structure minimale (liste d'entités + liste de champs par entité avec leur FieldKind). »

---

## PHASE 2 — CALCUL ANALYTIQUE DE CAPACITÉ

---

### Phase 2.1 — Accumulation `PAGE_STATIC_CAP`

**Objectif Technique Unique**
Calculer la somme exacte des octets HTML statiques depuis l'AST validé. Fonction pure, zéro lookup de schéma.

**Contrat Entrée/Sortie**

```
Entrée  : tokens: &[FlatPageToken<'src>]
Sortie  : usize  (PAGE_STATIC_CAP)
```

**Périmètre Algorithmique**

- ✅ `Static(s)` → `s.len()`
- ✅ `StaticInclude { len, .. }` → `len`
- ✅ `IfBool`, `EndIf`, `Field` → contribution `0` (les balises HTML encadrantes sont absentes dans ce modèle de template libre, contrairement au pipeline schema)
- ❌ Interdiction de consulter `SchemaContext`

**Jalon Vert**
Test : un AST avec `Static("abc")` (3) + `StaticInclude { len: 100 }` + `Field` retourne `103`. Asserté avec `const` Rust (la fonction est `const fn`).

**Clause d'Échappement**

> « Phase 2.1 bloquée : la contribution des balises de contrôle IfBool (éventuel HTML de wrapping) n'est pas spécifiée. Les balises `<if>` sont-elles incluses dans les tokens `Static` adjacents ou comptabilisées séparément ? »

---

### Phase 2.2 — Accumulation `PAGE_DYNAMIC_CAP`

**Objectif Technique Unique**
Pour chaque token `Field` dans l'AST, résoudre son `max_display_width` ou `max_escaped_len` via le schéma.

**Contrat Entrée/Sortie**

```
Entrée  : tokens: &[FlatPageToken<'src>], schema: &SchemaContext
Sortie  : Result<usize, CapacityError>

CapacityError = UnboundedVarlen { entity: &'src str, field: &'src str }
```

**Périmètre Algorithmique**

- ✅ `Field` fixed-length → `FieldKind::max_display_width()`
- ✅ `Field` varlena → `VarlenField::max_escaped_len()`
- ✅ `IfBool` → contribution `0` (le champ booléen contrôle le flux, n'est pas émis dans le buffer)
- ❌ Interdiction de panic sur champ inconnu : retourner `Err(CapacityError::UnboundedVarlen)`

**Jalon Vert**
Test : AST avec 1 `Field { I64 }` + 1 `Field { VarlenField { max_len: 100, is_pre_escaped: false } }` retourne `Ok(20 + 500)`.

**Clause d'Échappement**

> « Phase 2.2 bloquée : le comportement pour un champ TEXT sans contrainte CHECK n'est pas tranché (panic vs fallback 10 000 vs erreur). Décision requise avant implémentation. »

---

### Phase 2.3 — Émission des Constantes de Capacité

**Objectif Technique Unique**
Produire les trois lignes `const` Rust à partir des deux valeurs scalaires.

**Contrat Entrée/Sortie**

```
Entrée  : page_name: &str, static_cap: usize, dynamic_cap: usize
Sortie  : String  (3 lignes de code Rust)
```

**Périmètre Algorithmique**

- ✅ Nommage SCREAMING_SNAKE : `{PAGE_NAME}_STATIC_CAP`, `_DYNAMIC_CAP`, `_TOTAL_CAP`
- ✅ `TOTAL_CAP = static_cap + dynamic_cap` calculé ici, pas en amont
- ❌ Interdiction de toute logique autre que le formatage de chaîne

**Jalon Vert**
`assert_eq!` sur la sortie textuelle exacte pour `("home", 100, 200)`.

**Clause d'Échappement**

> « Phase 2.3 bloquée : la convention de nommage de `page_name` (provient du nom de fichier `.marius` ? d'une annotation dans le template ?) n'est pas fixée. »

---

## PHASE 3 — GÉNÉRATION DE CODE RUST

---

### Phase 3.1 — Prologue `render_page()`

**Objectif Technique Unique**
Émettre la signature de la fonction et l'appel `buf.reserve(TOTAL_CAP)`.

**Contrat Entrée/Sortie**

```
Entrée  : fn_name: &str, total_cap_ident: &str
Sortie  : String  (signature + 1 instruction)
```

**Périmètre Algorithmique**

- ✅ Signature conforme au trait `Projection` (si applicable) ou libre
- ❌ Interdiction d'émettre autre chose que la signature et le reserve

**Jalon Vert**
Output contient exactement `buf.reserve({total_cap_ident});` comme première instruction du corps.

**Clause d'Échappement**

> « Phase 3.1 bloquée : la signature de `render_page()` n'est pas spécifiée (paramètres, trait impl vs fn libre). Fournir la déclaration de trait ou la signature attendue. »

---

### Phase 3.2 — Corps Séquentiel

**Objectif Technique Unique**
Transformer `&[FlatPageToken<'src>]` en une séquence linéaire d'instructions d'émission dans `buf`.

**Contrat Entrée/Sortie**

```
Entrée  : tokens: &[FlatPageToken<'src>], schema: &SchemaContext
Sortie  : String  (corps de la fonction, sans signature ni accolades)
```

**Règles d'émission par variant**

- `Static(s)` → `buf.push_str("{s}");`
- `StaticInclude { rel_from_manifest, .. }` → `buf.push_str(include_str!("{rel_from_manifest}"));`
- `Field` fixed-length → `::std::fmt::Write::write_fmt(buf, format_args!("{{}}", record.{field})).ok();`
- `Field` varlena → `marius_html_escape(payload.{field}, buf);`
- `IfBool` → `if record.{field} {`
- `EndIf` → `}`

**Périmètre Algorithmique**

- ❌ Interdiction d'émettre un `format!()` alloué dans le code généré
- ❌ Interdiction d'émettre toute autre logique conditionnelle que le branchement IfBool

**Jalon Vert**
Test de snapshot : AST à 5 tokens produit un corps dont chaque ligne est assertée verbatim. Compiler le résultat via `rustc --edition 2024 --crate-type lib` passe sans erreur.

**Clause d'Échappement**

> « Phase 3.2 bloquée : l'accès au champ varlena depuis le code généré (`record`, `payload`, ou autre) n'est pas tranché. Spécifier les noms des paramètres de `render_page()`. »

---

### Phase 3.3 — Assemblage du Fichier Final

**Objectif Technique Unique**
Concaténer header + constantes + corps dans l'ordre correct et écrire dans le fichier de sortie de `build.rs`.

**Contrat Entrée/Sortie**

```
Entrée  : out_path: &Path, header: &str, capacity_consts: &str, fn_body: &str
Sortie  : Result<(), std::io::Error>
```

**Périmètre Algorithmique**

- ✅ Seule phase autorisée à faire de l'I/O disque (écriture)
- ✅ Écriture atomique via `BufWriter`
- ❌ Interdiction de reformatter ou modifier le contenu des trois blocs

**Jalon Vert**
`cargo build` dans `crates/core/schema` compile le fichier généré sans erreur. `cargo test` passe le test `test_{page_name}_no_realloc()` : `buf.capacity()` avant et après `render_page()` est identique.

**Clause d'Échappement**

> « Phase 3.3 bloquée : le chemin de sortie (`OUT_DIR`, nom du fichier `.rs`) n'est pas confirmé par `build.rs`. Fournir la variable d'environnement ou le path attendu. »

---

## RÉCAPITULATIF DES DÉPENDANCES DE PHASES

```
1.1 ──→ 1.2 ──→ 1.3 ──→ 1.4
                  │           \
                  ↓            ↓
                2.1 ──→ 2.2 ──→ 2.3
                                  │
                                  ↓
                         3.1 ──→ 3.2 ──→ 3.3
```

Les phases 2.x et 3.x sont bloquantes sur 1.3 validée (AST construit). La Phase 1.4 (validation sémantique) est un prérequis de 2.2 uniquement (évite les lookups sur des champs non vérifiés), mais peut être parallélisée avec 2.1.
