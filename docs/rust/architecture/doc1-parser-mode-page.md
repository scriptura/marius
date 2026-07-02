# Document 1 — Parser Mode Page

**Contrat d'architecture, pas d'implémentation.** Portée : `forge/fragment-forge/src/lib.rs`, nouveau module.
**Principe :** produire l'AST d'**un seul fichier**, sans lecture d'un second fichier, sans résolution d'héritage. Toute connaissance inter-fichiers est hors périmètre de ce document (Document 2).

---

## 1. Responsabilité

Transformer `impl Iterator<Item = RawSpan<'src>>` (sortie du Scanner, réutilisé sans modification) en une structure représentant fidèlement la grammaire d'un fichier `.marius` en mode composition — opérateurs de projection (`{{ }}`, `{% if %}`) **et** opérateurs de composition (`extends`, `block`/`endblock`, `static`) — sans jamais consulter un second fichier ni un `SchemaIndex`.

Frontière stricte avec le Linker (Document 2) : ce Parser ne sait pas si le chemin déclaré par `extends` existe. Il sait seulement qu'une déclaration `extends` occupe une position syntaxiquement valide.

Frontière stricte avec `parse_tokens` (Mode Fragment, gelé) : domaines d'erreur disjoints par construction typée (`PageComposeParseError` ≠ `PageParseError`) — un appelant ne peut physiquement pas confondre les deux échecs.

---

## 2. Structures manipulées

### 2.1 `PageSourceToken<'src>` — alphabet du fichier composé

Enum englobant, paramétré sur les deux enums existants. Décision actée : **pas** de variante additionnelle sur `FlatPageToken` (casserait l'exhaustivité gelée de `validate_ast` / `resolve_and_measure` / `generate_aot_snippet`), **pas** d'union de types séparés (romprait le modèle « un fichier = un flux plat unique » commun à tout le pipeline depuis le Scanner).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSourceToken<'src> {
    /// Opérateur de projection, identique au Mode Fragment.
    /// N'émet jamais StaticInclude — cette variante reste strictement liée
    /// à {% include %}, absent de la grammaire Mode Page.
    Runtime(FlatPageToken<'src>),
    /// Opérateur de composition : {% block %} / {% endblock %}.
    Block(PageBlockToken<'src>),
    /// Opérateur de composition : {% static "path" %}.
    Static(StaticPartialRef<'src>),
    /// Mot-clé de bloc reconnu syntaxiquement mais non supporté par la
    /// grammaire runtime (for, join, where, filter, group, ou tout mot-clé
    /// inconnu). Capturé plutôt que rejeté ici : le Parser ne décide pas
    /// *pourquoi* c'est interdit — il transmet le fait brut à la Validation
    /// (Document 2), qui produit le message d'erreur nommé et précis
    /// (ForLoopDetected, RelationalKeyword) plutôt qu'un rejet générique.
    Unsupported { keyword: &'src str, tail: &'src str },
}
```

Invariant de platitude : aucune variante ne porte de `Vec<Self>` ni de `Vec<FlatPageToken>` imbriqué — cohérent avec `NamedBlockRange` (Phase 3.0, déjà scaffoldée).

### 2.2 `ParsedPageTemplate<'src>` — sortie du Parser pour un fichier

```rust
#[derive(Debug, Clone)]
pub struct ParsedPageTemplate<'src> {
    /// Chemin déclaré par {% extends %}, si présent en tête de fichier.
    /// None : ce fichier est soit un parent (pas d'extends), soit hors
    /// mode page (cas déjà écarté par detect_extends en amont, cf. §3).
    pub extends: Option<&'src str>,
    pub tokens:  Vec<PageSourceToken<'src>>,
}
```

Ne porte ni `TemplateId`, ni `NamedBlockRange` résolus : l'assignation d'identité d'arène est une responsabilité de l'admission en arène (Document 2), pas du Parser — un fichier parsé isolément n'a pas encore d'arène à laquelle appartenir.

---

## 3. Signatures publiques

```rust
/// Discriminant de mode (§3.1 spécification). Pur, O(1) amorti : s'arrête
/// dès que le premier `{%` est vu et compare son premier Ident à "extends".
/// Ne valide pas la forme complète de la déclaration extends — un extends
/// malformé est détecté par parse_page_tokens, pas ici.
pub fn detect_extends(source: &str) -> bool;

/// Construit l'AST d'un unique fichier. Précondition : appelé uniquement
/// si detect_extends(source) == true (contrat d'appel, pas vérifié en
/// interne — un parent sans extends passe aussi par cette fonction, avec
/// extends == None en sortie).
pub fn parse_page_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<ParsedPageTemplate<'src>, PageComposeParseError>;
```

Point ouvert, à ne pas trancher ici : `PageComposeParseError` ne porte aujourd'hui que `ExtendsNotFirst`. La grammaire de ce Parser a besoin d'équivalents Mode Page de `PageParseError::UnexpectedToken` / `UnexpectedEof` / `InvalidBlockSequence` (`{{ }}` malformé, `{% block %}` sans nom, etc.). Ces variantes ne sont pas inventées ici — l'implémentation devra étendre `PageComposeParseError` de façon symétrique à `PageParseError`, sans dupliquer son nom (cf. justification de nommage déjà actée dans le code).

---

## 4. Domaines d'erreur

| Domaine | Portée | Statut |
|---|---|---|
| `PageComposeParseError` | Grammaire mono-fichier (position d'`extends`, forme des tokens) | `ExtendsNotFirst` acté ; reste à étendre (§3) |
| `PageParseError` | Mode Fragment, disjoint, jamais retourné ici | Gelé, hors périmètre |
| `PageValidationError` | Sémantique de forme (nesting, for, mots relationnels, if non-bool) | **Non produit par ce Parser** — le Parser reste permissif sur ces points (voir §6) ; c'est la Validation (Document 2) qui juge |

Décision de méthode, symétrique à `IfBool`/`EndIf` en Mode Fragment : le Parser ne rejette pas l'imbrication de blocs, ni `{% for %}`, ni les mots-clés relationnels. Il les représente fidèlement (`Block`, `Unsupported`) et laisse la phase de validation — qui a besoin d'un état de pile pour juger l'imbrication — trancher. Un Parser qui validerait la sémantique en plus de la syntaxe recréerait la fusion que le §0 du document précédent proscrit explicitement.

---

## 5. Invariants mémoire

- Zéro allocation de texte : tous les champs `&'src str` sont des emprunts directs sur la source, comme `FlatPageToken` et `PageBlockToken`.
- `Vec<PageSourceToken<'src>>` : allocation heap unique, build-time, conditionnelle au premier `push` (un fichier vide ne coûte rien).
- `PageSourceToken` reste `Copy` (agrégat de types `Copy`) — aucune indirection supplémentaire introduite par l'enum englobant. Coût mémoire par token : celui de la plus grande variante (`Unsupported { &str, &str }`, 32 octets sur une cible 64 bits), légèrement supérieur à `FlatPageToken` seul (24 octets) — accepté, car ce coût reste build-time, jamais propagé au binaire final.
- Aucune I/O dans cette fonction. `{% static "path" %}` produit un `StaticPartialRef` portant le chemin brut, non résolu — cohérent avec la doc déjà actée de ce type (« pas de champ `len` par occurrence »). La résolution (existence, taille) est repoussée aux phases avales (Document 2, Document 3).

---

## 6. Garanties produites en sortie

- Tout `{{ entity.field }}` et `{% if entity.field %}` est syntaxiquement bien formé (identique aux garanties de `parse_tokens` Mode Fragment sur ces deux opérateurs, réutilisées telles quelles).
- `extends`, si présent, occupe la première position non-whitespace du fichier — sinon `PageComposeParseError::ExtendsNotFirst`, échec immédiat (pas fail-slow ici : une position d'`extends` invalide invalide la nature même du fichier, poursuivre n'a pas de sens).
- Chaque `{% block name %}` / `{% endblock %}` est syntaxiquement bien formé (nom présent à l'ouverture, aucun nom exigé à la fermeture — symétrique à `EndIf`) — **sans garantie d'appariement correct ni d'absence d'imbrication**. Ces deux garanties n'existent qu'après la Validation (Document 2).
- Chaque `{% static "path" %}` est syntaxiquement bien formé — **sans garantie d'existence du fichier**.
- Tout mot-clé de bloc hors grammaire connue (`for`, `join`, `where`, `filter`, `group`, ou inconnu) est capturé sous `Unsupported`, jamais silencieusement ignoré ni rejeté à ce stade.

## 7. Préconditions / Postconditions

**Préconditions**
- `spans` provient de `scan()` appliqué à la source complète d'un fichier `.marius`.
- Le fichier a été positivement identifié comme Mode Page par `detect_extends` (ou il s'agit d'un parent, admis sans cette précondition — voir Document 2 pour la distinction d'usage).

**Postconditions (succès)**
- `ParsedPageTemplate` contient une représentation complète et fidèle du fichier — aucun span n'est perdu, aucune information de composition n'est résolue.
- Le fichier peut être admis en arène (Document 2) sans reparcours de `spans`.

**Postconditions (échec)**
- Aucune mutation partielle observable en dehors du `Result` — pas de sortie utilisable en cas d'`Err` (fail-fast sur la grammaire de tête ; pas de contrat d'accumulation fail-slow au niveau du Parser, contrairement au Linker et à la Validation qui, eux, accumulent — cette différence de politique reflète l'existant : `PageParseError` de `parse_tokens` Mode Fragment est déjà fail-fast, pas fail-slow).
