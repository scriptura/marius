# Architecture du pipeline « Mode Page » — Marius / Fragment-Forge

**Statut :** document d'architecture (pas une spécification fonctionnelle, pas un plan d'implémentation).
**Portée :** `forge/fragment-forge/src/lib.rs`, `crates/core/schema/build.rs`.
**Base factuelle :** lecture directe du code gelé (`lib.rs`), pas de la spécification v1.1 seule. Les écarts entre les deux sont isolés en fin de document.

---

## 0. Principe directeur : le lowering

Le compilateur est une suite de fonctions totales `Entrée → Sortie`, chacune éliminant définitivement une famille de concepts du domaine manipulé. Une phase ne peut pas régresser un concept déjà éliminé par une phase amont — c'est l'invariant qui rend le pipeline auditable dans le temps.

Conséquence directe, déjà actée dans le code : `FlatPageToken` est un `enum` matché de façon exhaustive (sans bras `_`) dans trois fonctions gelées (`validate_ast`, `resolve_and_measure`, `generate_aot_snippet`). Ajouter une variante de composition à cet `enum` casserait l'exhaustivité et forcerait l'édition des trois fonctions — c'est précisément le garde-fou recherché. Les concepts de composition (`extends`, `block`) vivent donc dans des types frères (`PageBlockToken`, `ChildTemplateSpec`), jamais dans `FlatPageToken` lui-même.

Conséquence opérationnelle majeure : **Mode Fragment et Mode Page convergent**. Ils divergent uniquement sur les phases amont (Parser, Linker, Normalizer). À partir de `FlatPageToken`, il n'existe plus qu'un seul pipeline, déjà écrit, déjà gelé, déjà testé. Le travail restant à faire pour le Mode Page se limite aux phases qui _produisent_ du `FlatPageToken` — jamais à celles qui le _consomment_.

```
                    ┌─── MODE FRAGMENT (gelé) ───┐
Source .marius ──▶ Scanner ──▶ AST Front-end ──▶ FlatPageToken ──▶ Resolver/Mesure ──▶ Codegen
                    └─────────────────────────────┘                      │                │
                                                                    (validate_ast)  (generate_aot_snippet)
                    ┌─── MODE PAGE (partiel) ─────────────────────────┐  │                │
Source .marius ──▶ Scanner ──▶ AST Front-end ──▶ Linker ──▶ Normalizer ─┴────────────────┴──▶ (identique)
                    (partagé)   (à écrire)      (à écrire)  (à écrire)
```

---

## 1. Scanner

### 1. Responsabilité

Découper `&'src str` en une séquence plate de `RawSpan<'src>` catégorisés par grammaire lexicale minimale (délimiteurs, identifiants, ponctuation). Aucune sémantique n'est résolue : le Scanner ne sait pas ce qu'est `extends`, `if`, ou `include` — ces mots sont juste des `Ident`.

**Pourquoi une phase séparée, non fusionnable avec l'AST Front-end :** le Scanner opère sur des octets/`char` ASCII avec des invariants de frontière UTF-8 ; l'AST Front-end opère sur une grammaire de mots-clés. Fusionner les deux couplerait la validation de frontière de caractères à la logique de reconnaissance de patterns — deux taux de changement différents (la grammaire du DSL évolue, la tokenisation ASCII non).

### 2. Entrée

`src: &'src str` — emprunt pur, lifetime liée à la `String` lue par `std::fs::read_to_string` dans l'orchestrateur (`build.rs`). Aucune garantie a priori sur le contenu (fichier brut).

### 3. Sortie

`impl Iterator<Item = RawSpan<'src>>`, où `RawSpan { slice: &'src str, kind: SpanKind }`, `SpanKind ∈ {Literal, ExprOpen, ExprClose, BlockOpen, BlockClose, Ident, Punct}`.
Garantie nouvelle : chaque `slice` pointe dans `src`, chaque frontière de span est une frontière `char` valide.

### 4. Invariants

- Entrée : `src` est une chaîne UTF-8 valide (garanti par le type `&str`).
- Sortie : `Scanner::pos` reste toujours sur une frontière `char` valide (prouvé par construction : `Literal` avance via `str::find`, `InExpr`/`InBlock` ne consomment que de l'ASCII single-byte).

### 5. Concepts éliminés

- Recherche manuelle de délimiteurs `{{`, `}}`, `{%`, `%}` dans les phases avales.
- Gestion des frontières UTF-8 dans les phases avales — toute phase en aval du Scanner peut indexer un `RawSpan::slice` sans revalider `char_boundary`.

### 6. Gestion mémoire

Zéro allocation heap. `Scanner<'src>` tient sur la pile (`&str` + `usize` + `Mode`, 24 octets). Aucun `Vec`, `String`, `Box` dans le corps. Justification DOD : le Scanner est traversé une fois par octet de fichier, à build-time uniquement — toute allocation ici serait un coût de compilation, pas d'exécution, mais reste un mauvais signal architectural (un scanner qui alloue invite à des scanners qui allouent).

### 7. Complexité

`O(n)` en octets de `src`. Pure, déterministe, séquentielle (état `Mode` mutable entre appels `next()` — un `Iterator` avec état interne n'est pas parallélisable sans repartitionnement préalable). I/O : aucune. Parallélisable _entre fichiers_, jamais _à l'intérieur_ d'un fichier.

### 8. Erreurs

Le Scanner ne retourne pas de `Result` — un délimiteur non fermé (`{{` sans `}}`) se traduit par `None` prématuré (itérateur épuisé), remonté comme erreur de couche par l'AST Front-end (`PageParseError::UnexpectedEof`), pas par le Scanner lui-même. Erreurs devenues impossibles après cette phase : toute question du type « est-ce que ce caractère commence bien un token ASCII propre ? » n'a plus à être reposée en aval.

### 9. API publique

```rust
pub fn scan(src: &str) -> impl Iterator<Item = RawSpan<'_>>;
```

Existant, gelé, **partagé sans modification entre Mode Fragment et Mode Page**. Le futur parseur Mode Page consomme le même flux de `RawSpan` que `parse_tokens` (Mode Fragment) ; la distinction entre les deux modes n'apparaît qu'au niveau du classifieur (AST Front-end), jamais au niveau lexical.

---

## 2. AST Front-end (Parser)

Deux instanciations coexistent : celle du Mode Fragment (`parse_tokens`, gelée) et celle du Mode Page (à écrire). Le contrat structurel est commun ; seul le vocabulaire de mots-clés reconnu diffère.

### 1. Responsabilité

Transformer le flux de `RawSpan<'src>` en une structure typée reflétant la grammaire d'**un seul fichier `.marius`**, sans connaissance des autres fichiers. C'est une frontière stricte : l'AST Front-end ne sait pas si le chemin déclaré par `{% extends %}` existe — il sait seulement qu'une déclaration `extends` a été rencontrée en position syntaxiquement valide.

**Pourquoi non fusionnable avec le Linker :** résoudre `extends` exige de lire un second fichier — une opération I/O et une dépendance inter-fichiers. Le Parser reste une fonction pure sur une seule source ; le coupler au Linker romprait la testabilité unitaire (chaque test du Parser devient un test d'intégration multi-fichiers) et interdirait le parsing parallèle de N fichiers indépendants avant résolution des dépendances.

### 2. Entrée

`impl Iterator<Item = RawSpan<'src>>` (sortie du Scanner). Garantie déjà acquise : frontières `char` valides, catégorisation lexicale correcte.

### 3. Sortie — Mode Fragment (gelé)

`Result<Vec<FlatPageToken<'src>>, PageParseError>` — directement l'IR canonique, car le Mode Fragment ne connaît pas la composition.

### 3. Sortie — Mode Page (à écrire)

Non tranchée dans le code actuel, et c'est documenté comme tel (commentaire « Phase 3.0 » de `lib.rs`) : le handoff envisage soit une variante additionnelle sur `FlatPageToken` (rejetée — casserait l'exhaustivité gelée), soit un type englobant paramétré sur `FlatPageToken` et `PageBlockToken`. Ce que la sortie **doit** porter, en revanche, est tranché par les types déjà scaffoldés :

- une séquence mêlant `FlatPageToken` (Static/Field/IfBool/EndIf/StaticInclude — réutilisés tels quels) et `PageBlockToken` (BlockOpen/BlockEnd),
- un `ChildTemplateSpec<'src> { extends: &'src str, blocks: Vec<NamedBlockRange<'src>> }` si le fichier est un enfant,
- des `StaticPartialRef<'src>` pour chaque `{% static %}` rencontré.

Je ne propose pas de signature Rust pour cette sortie tant que le choix « union de types » vs « enum englobant » n'est pas arbitré — inventer la signature ici masquerait une décision de câblage encore ouverte.

### 4. Invariants

- Entrée : héritée du Scanner (frontières valides, catégorisation lexicale).
- Sortie nouvelle (Mode Fragment) : l'AST est syntaxiquement complet — chaque `{{ }}` et `{% %}` est bien formé (`entity.field`, `if entity.field`, `endif`, `include path`). Aucune garantie sémantique (l'entité peut ne pas exister dans le schéma — ce n'est pas encore vérifié).
- Sortie nouvelle (Mode Page) : `extends` est syntaxiquement en première position s'il est présent (`PageComposeParseError::ExtendsNotFirst` sinon) ; chaque `block`/`endblock` est syntaxiquement apparié — mais la **validité de forme** (imbrication interdite) est un invariant de la sous-phase suivante, pas de celle-ci (voir plus bas).

### 5. Concepts éliminés

- Après cette phase (Mode Fragment) : texte brut non catégorisé, délimiteurs bruts (`{{`, `%}`…), suite de `RawSpan` non structurée.
- Après cette phase (Mode Page), en plus : position de `extends` dans le fichier (fixée), séquence brute de mots-clés `block`/`endblock`/`static` (remplacée par des tokens typés). **Ce qui persiste encore** après cette phase en Mode Page, et qui ne disparaît que plus tard : l'existence du fichier parent référencé par `extends` (Linker), l'appariement bloc enfant ↔ bloc parent (Linker), la présence physique du bloc dans le flux final (Normalizer).

### 6. Gestion mémoire

`Vec<FlatPageToken<'src>>` (et son pendant Mode Page) est une allocation heap, mais strictement build-time — cette structure ne survit jamais jusqu'au binaire final. Zéro copie de texte : chaque champ texte est un emprunt sur `src`. `Vec::new()` n'alloue pas avant le premier `push` : un template vide ne coûte rien. Justification DOD : la seule donnée qui compte au runtime est le code Rust généré en bout de pipeline (Codegen) — tout ce qui est build-time peut allouer librement sans impact sur le hot path, à condition de ne jamais fuiter de structure vers ce hot path.

### 7. Complexité

`O(n)` en nombre de spans, un seul passage, pas de backtracking (l'automate consomme la tête de séquence et ses helpers consomment les spans suivants selon le pattern attendu — jamais de relecture). Pure, déterministe. I/O : aucune, à une exception near-triviale documentée dans le Mode Fragment — `std::fs::metadata` pour connaître la taille d'un fichier `{% include %}` est en réalité déportée dans l'orchestrateur (`build.rs`), pas dans le Parser lui-même (voir §8, roadmap Phase 1.3 vs implémentation réelle). Séquentielle _par fichier_ ; parallélisable _entre fichiers_ (aucun état partagé entre deux appels).

### 8. Erreurs

`PageParseError` (Mode Fragment, gelé) : `UnexpectedToken`, `UnexpectedEof`, `InvalidBlockSequence` — exclusivement des erreurs de grammaire locale à un fichier.
`PageComposeParseError` (Mode Page) : domaine disjoint et volontairement distinct de `PageParseError` — même s'il s'agit de la même _catégorie_ d'erreur (grammaire, pas sémantique), car le Mode Fragment ne connaît ni `extends` ni `block` : une fonction `Result<_, PageParseError>` ne peut physiquement pas retourner `PageComposeParseError::ExtendsNotFirst`. C'est le typage, pas la documentation, qui garantit l'étanchéité entre les deux modes.
Erreurs devenues impossibles après cette phase (Mode Fragment) : toute question de bien-formation syntaxique locale (`{{` sans `.`, `if` sans champ, etc.) — les phases avales peuvent supposer l'AST syntaxiquement clos.

### 9. API publique

Mode Fragment (existant, gelé) :

```rust
pub fn parse_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<Vec<FlatPageToken<'src>>, PageParseError>;
```

Mode Page : non inventée ici — dépend de l'arbitrage « union de types vs enum englobant » (§3, ci-dessus). Les types déjà tranchés qui composeront cette API : `PageBlockToken<'src>`, `ChildTemplateSpec<'src>`, `NamedBlockRange<'src>`, `TemplateId`, `StaticPartialRef<'src>`, `PageComposeParseError`.

---

## 2bis. Validation de forme mono-fichier (composition)

Sous-phase distincte, positionnée immédiatement après le Parser Mode Page, avant tout accès à un second fichier. Le code la nomme `PageValidationError` et documente explicitement pourquoi elle n'est ni Parser ni Linker : « propriétés vérifiables sur un unique template déjà syntaxiquement valide, sans connaissance des autres templates ».

### 1. Responsabilité

Vérifier des propriétés de forme qui ne nécessitent la connaissance d'aucun autre fichier : condition `if` non booléenne (`NonBoolIfCondition`), présence d'une boucle (`ForLoopDetected` — interdite structurellement, capacité non bornable), mot-clé relationnel (`RelationalKeyword` — logique relationnelle hors périmètre), bloc imbriqué (`NestedBlock`).

### 5. Concepts éliminés

Après cette phase : possibilité qu'un fichier syntaxiquement valide contienne une construction structurellement interdite (boucle, imbrication, mot-clé relationnel, condition sur champ non-bool).

### 6/7. Mémoire / complexité

`O(n)` sur l'AST du fichier, fail-slow (accumulation d'erreurs, à l'image de `validate_ast` gelé pour le Mode Fragment — même style de preuve, cohérence de méthode). Zéro allocation hors le `Vec<PageValidationError>` conditionnel.

### 9. API publique

Non inventée : la fonction elle-même n'existe pas encore, seul le type d'erreur `PageValidationError<'src>` est scaffoldé.

---

## 3. Linker

### 1. Responsabilité

Résoudre les dépendances **entre** fichiers : suivre `extends` jusqu'au parent, apparier chaque `NamedBlockRange` de l'enfant avec un `PageBlockToken::BlockOpen` du parent, vérifier l'existence des fichiers `{% static %}`. C'est la seule phase qui a besoin de voir plus d'un fichier à la fois.

**Pourquoi non fusionnable avec le Normalizer :** le Linker répond à une question binaire par référence (« ce nom de bloc existe-t-il dans le parent ? », « ce chemin existe-t-il sur disque ? ») — il ne mute aucune structure. Le Normalizer, lui, mute/reconstruit le flux de tokens. Séparer les deux permet au Linker de rester `Fn` pure modulo I/O de lecture (testable avec un `get_file_size` injecté, comme `resolve_and_measure` le fait déjà pour le Mode Fragment), pendant que le Normalizer reste une fonction de reconstruction déterministe sans I/O.

### 2. Entrée

Un `ChildTemplateSpec<'src>` (chemin `extends`, plages de blocs de l'enfant) + l'AST du parent (flux de `PageBlockToken`/`FlatPageToken` mêlés, taggé par un `TemplateId`) + un résolveur de chemin injecté (I/O différée, testable). Garantie déjà acquise : chaque fichier impliqué est individuellement bien formé (Parser + validation de forme passées).

### 3. Sortie

Non un nouvel arbre — une preuve de correspondance. Le type `NamedBlockRange<'src> { name, template: TemplateId, start, end }` porte déjà toute l'information nécessaire : il ne reste qu'à confirmer, pour chaque plage de l'enfant, qu'un `BlockOpen` de même nom existe dans le parent référencé. Garantie nouvelle : tout bloc de l'enfant est soit rattaché à un `BlockOpen` du parent, soit signalé orphelin — plus aucune ambiguïté de correspondance ne subsiste après cette phase.

### 4. Invariants

- Entrée : chaque fichier impliqué est syntaxiquement clos et structurellement conforme (2bis passée).
- Sortie : toute référence `extends`/`block`/`static` est soit résolue (chemin existe, nom apparié), soit rejetée avec un `PageLinkError` explicite — aucune référence pendante ne franchit cette phase.

### 5. Concepts éliminés

Après cette phase : existence incertaine d'un fichier référencé par chemin (`extends`, `static`), correspondance incertaine entre un nom de bloc enfant et un `BlockOpen` parent. Ce qui **persiste** encore : le contenu du bloc n'est pas encore substitué dans le flux du parent — c'est le rôle du Normalizer.

### 6. Gestion mémoire

Le `TemplateId` (`u32` nu, `Copy`) est un choix DOD documenté explicitement dans le code : porter directement un `&'ast [FlatPageToken<'src>]` dans `NamedBlockRange` rendrait `ChildTemplateSpec` auto-référentiel (une struct qui emprunte son propre `Vec`), incompatible avec une construction en une passe. Le compromis assumé : la vérification d'appartenance à la bonne arène est faite par assertion runtime au point de déréférencement dans le Linker, pas par le compilateur — acceptable tant qu'une seule passe de linking traite les templates séquentiellement.

### 7. Complexité

`O(b × p)` dans le cas naïf (`b` blocs enfant, `p` blocs parent) ou `O(b + p)` avec une table de hachage nom→plage construite sur le parent en amont — choix d'implémentation, pas d'architecture. I/O : lecture de métadonnées fichier (`{% static %}`) et lecture du fichier parent référencé par `extends`. Fail-slow par cohérence avec le reste du pipeline (`resolve_and_measure` accumule déjà les erreurs I/O de la même façon). Séquentiel par chaîne `extends` (un enfant ne peut être lié qu'après que son parent a été parsé) ; parallélisable entre chaînes `extends` indépendantes.

### 8. Erreurs

`PageLinkError<'src> { ExtendsNotFound, OrphanBlock, StaticFileNotFound }` — domaine disjoint de `ResolverError` (Mode Fragment, même catégorie « fichier introuvable » mais contexte de phase différent, volontairement non mutualisé avant l'écriture des trois call-sites réels). Erreurs devenues impossibles après cette phase : un bloc enfant sans parent connu, un chemin `static`/`extends` non vérifié.

### 9. API publique

Non tranchée. Le type `PageLinkError<'src>` existe ; la fonction n'a pas de signature scaffoldée dans le code actuel. Elle dépendra directement du type retenu en sortie du Parser Mode Page (§2), non encore fixé.

---

## 4. Normalizer

### 1. Responsabilité

Fusionner l'AST du parent et les plages de blocs de l'enfant en une **unique** séquence plate de `FlatPageToken<'src>` — l'opération de substitution textuelle décrite par la grammaire (`{% block name %}` du parent remplacé par le contenu enfant si présent, conservé sinon). Déduplique les `{% static %}` référencés plusieurs fois vers une constante partagée unique.

**Pourquoi non fusionnable avec le Resolver/Mesure :** le Normalizer répond à « quelle est la séquence finale de tokens ? », une question purement structurelle sur des références déjà validées par le Linker. Le Resolver répond à « ces tokens décrivent-ils un accès schéma valide, et quelle est leur capacité mémoire ? », une question qui nécessite `SchemaIndex` — une dépendance que le Normalizer n'a pas besoin de connaître. Séparer les deux, c'est préserver la réutilisation intégrale de `resolve_and_measure` (déjà gelé, déjà testé) sans lui ajouter de connaissance de la composition.

### 2. Entrée

L'AST du parent (taggé par `TemplateId`), les `NamedBlockRange` appariées par le Linker, les `StaticPartialRef` résolues. Garantie déjà acquise : toute référence est valide (Linker passé).

### 3. Sortie

`Vec<FlatPageToken<'src>>` — **exactement le même type** que la sortie du Parser Mode Fragment. C'est le point de convergence du pipeline (voir §0). Garantie nouvelle, la plus importante du document : à partir d'ici, il n'existe plus de différence observable entre un template né en Mode Fragment et un template né en Mode Page.

### 4. Invariants

- Entrée : toute référence de composition est résolue (Linker passé).
- Sortie : la séquence ne contient plus aucune trace de `PageBlockToken`, `ChildTemplateSpec`, `TemplateId`, `NamedBlockRange`, ou `StaticPartialRef` — uniquement les cinq variantes de `FlatPageToken` (Static, Field, IfBool, EndIf, StaticInclude).

### 5. Concepts éliminés

**Tous** les concepts de composition, définitivement : `extends`, `block`/`endblock`, la distinction parent/enfant, la notion de « fichier composé de plusieurs fichiers ». Après cette phase, un template Mode Page et un template Mode Fragment sont structurellement indiscernables. Régresser un `BlockOpen` dans `FlatPageToken` à ce stade (ou plus tard) est la définition même d'une régression architecturale — c'est l'exemple que la mission cite explicitement, et c'est pourquoi le tableau récapitulatif (§7) doit servir d'alarme.

### 6. Gestion mémoire

Reconstruction d'un nouveau `Vec<FlatPageToken<'src>>` — allocation heap build-time, comme toutes les phases amont. Les fragments `Static` réutilisés depuis l'enfant ou le parent restent des emprunts sur leurs sources respectives (`'src` distinctes selon l'arène d'origine — d'où l'importance du `TemplateId` porté en amont par le Linker, qui garantit qu'on ne mélange pas deux arènes par erreur). La déduplication des `{% static %}` introduit un registre séparé (mentionné dans le code comme hors périmètre de la session actuelle) keyé par `static_const_ident` — fonction déjà existante et réutilisable sans modification.

### 7. Complexité

`O(p + Σ tailles des blocs substitués)` — un parcours du flux parent, substitution en place ou par reconstruction linéaire selon l'implémentation retenue (choix d'implémentation, pas d'architecture). Pure, déterministe. I/O : aucune (tout l'I/O a été fait en amont par le Linker). Séquentiel par chaîne `extends` résolue.

### 8. Erreurs

Aucun domaine d'erreur propre attendu à ce stade : toutes les références ont été validées par le Linker en amont — le Normalizer est une fonction de reconstruction totale sur une entrée déjà garantie cohérente. Si un domaine d'erreur apparaissait ici (ex. un bloc « default » du parent mal formé), ce serait le signe qu'une vérification a été omise dans une phase amont, pas une raison d'ouvrir un nouveau type d'erreur à ce niveau.

### 9. API publique

Non tranchée. Dépend directement du type de sortie du Parser Mode Page (§2). Ce qui est acquis : la signature de retour est `Vec<FlatPageToken<'src>>`, sans paramètre de type générique — la fonction ferme définitivement la composition.

---

## 5. FlatPageToken — IR canonique (pivot, pas une fonction)

### 1. Responsabilité

Servir de frontière de type unique entre le front-end (syntaxe, composition) et le back-end (schéma, capacité, codegen). Ce n'est pas une transformation mais un contrat : toute fonction qui consomme `&[FlatPageToken<'src>]` peut ignorer complètement l'origine du template (Fragment ou Page).

### 2/3. Entrée / Sortie

`enum FlatPageToken<'src> { Static(&'src str), Field { entity, field }, IfBool { entity, field }, EndIf, StaticInclude { original_path, rel_from_manifest, len } }` — `Copy`, `Clone`, `PartialEq`, `Eq`. Zéro allocation intrinsèque au type lui-même.

### 4. Invariants

- Fermé par construction : cinq variantes, aucune extension sans casser l'exhaustivité des trois fonctions gelées qui le consomment.
- Chaque champ texte est un emprunt sur la source d'origine — aucune copie, aucune normalisation de casse ou d'espace.

### 5. Concepts éliminés

Tout ce qui a précédé : `RawSpan`, mots-clés bruts, composition, chemins non résolus (`len` de `StaticInclude` est déjà résolu à ce stade — mutation en place par le Resolver, pas de nouvelle allocation).

### 6. Gestion mémoire

`Copy` — 24 à 48 octets selon la variante, jamais indirection supplémentaire. Le choix `Copy` plutôt que `Clone`-only est délibéré : permet aux phases avales de dupliquer des tokens sans souci d'ownership (utile notamment pour `match *token` dans `validate_ast`, qui obtient des bindings directs `entity: &'src str` sans double indirection).

### 9. API publique

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatPageToken<'src> {
    Static(&'src str),
    Field { entity: &'src str, field: &'src str },
    IfBool { entity: &'src str, field: &'src str },
    EndIf,
    StaticInclude { original_path: &'src str, rel_from_manifest: &'src str, len: usize },
}
```

Existant, gelé.

---

## 6. Resolver / Measurement

Composite de deux fonctions gelées appelées en séquence dans `build.rs` : `validate_ast` (gate sémantique — équilibre des `if`, existence des champs référencés) puis `resolve_and_measure` (résolution I/O des inclusions + calcul de capacité). Les deux opèrent sur le même `FlatPageToken`, qu'il vienne du Mode Fragment ou du Mode Page — c'est ici que la convergence décrite en §0 se matérialise concrètement dans le code de `build.rs`.

### 1. Responsabilité

`validate_ast` : vérifier la cohérence structurelle des blocs conditionnels (FSM à un seul niveau d'état — imbrication interdite, fermeture obligatoire). `resolve_and_measure` : résoudre en place la taille des `StaticInclude` (I/O disque injectée) et calculer `TemplateMetrics { total_static_bytes, total_dynamic_bytes, include_count }` en une seule passe — la mesure de capacité et la validation des références de champ sont fusionnées pour éviter deux parcours de cache distincts sur le même flux.

**Pourquoi non fusionnable avec le Codegen :** le Resolver produit une donnée scalaire (`TemplateMetrics`) consommée par l'orchestrateur pour émettre les constantes `_STATIC_CAP`/`_DYNAMIC_CAP`/`_TOTAL_CAP` ; le Codegen produit du texte Rust. Un échec de résolution (champ inconnu, fichier manquant) doit arrêter la compilation _avant_ toute tentative de génération de texte — fusionner les deux retarderait la détection d'erreur jusqu'à un point où du texte partiellement généré existerait déjà, sans bénéfice.

### 2. Entrée

`&mut [FlatPageToken<'src>]` (mutable — `len` de `StaticInclude` passe de sa valeur provisoire à la taille réelle, en place, sans nouvel arbre) + `&SchemaIndex<'_>` + un résolveur de taille de fichier injecté (`impl Fn(&str) -> Result<usize, String>`, testable sans I/O réel).

### 3. Sortie

`Result<TemplateMetrics, Vec<ResolverError<'src>>>`. Garantie nouvelle : toute référence de champ est soit valide et mesurée (Hot), soit rejetée explicitement (`UnknownField`, `UnboundedField`) — aucun champ ne peut contribuer silencieusement à la capacité sans borne connue (disjoncteur Hot/Cold/Erreur, ADR-007).

### 4. Invariants

- Entrée : l'AST est syntaxiquement clos (Parser + éventuellement Linker/Normalizer passés).
- Sortie : `total_static_bytes` et `total_dynamic_bytes` sont des bornes supérieures exactes, atteignables au pire cas — pas de marge arbitraire (tout écart serait une sous-estimation cachée, détectable seulement par le test `no_realloc`, hors périmètre de cette phase).

### 5. Concepts éliminés

Taille de fichier inconnue (`StaticInclude::len` provisoire), existence non vérifiée d'un champ référencé, capacité mémoire non chiffrée.

### 6. Gestion mémoire

Mutation en place de l'AST (`len` seul champ scalaire modifié — les slices `&'src str` sont inchangées). `Vec<ResolverError>` n'alloue qu'au premier `push` : un template entièrement valide traverse cette phase sans allocation. Justification DOD explicite dans le code : deux parcours séparés (un pour la validation, un pour la mesure) gaspilleraient cycles CPU et localité de cache pour un gain de modularité illusoire — les deux opérations consomment le même flux dans le même ordre.

### 7. Complexité

`O(n)` en nombre de tokens, un seul parcours. Fail-slow : toutes les erreurs I/O sont accumulées avant de retourner `Err` (un build référençant N fichiers manquants remonte N erreurs en une passe, pas une passe par fichier). I/O : lecture de métadonnées fichier, injectée donc testable sans FS réel. Séquentiel (mutation en place d'un unique buffer).

### 8. Erreurs

`SemanticError<'src>` (`validate_ast`) : `UnexpectedEndIf`, `NestedIfNotSupported`, `UnclosedIf` — domaine structurel pur (FSM), zéro dépendance schéma.
`ResolverError<'src>` (`resolve_and_measure`) : `IoError`, `UnknownField`, `UnboundedField` — domaine schéma + I/O, disjoint du précédent.
Erreurs devenues impossibles après cette phase : capacité indéterminée, champ référencé sans borne connue, inclusion de fichier non résolue.

### 9. API publique

```rust
pub fn validate_ast<'src>(
    tokens: &[FlatPageToken<'src>],
) -> Result<(), Vec<SemanticError<'src>>>;

pub fn resolve_and_measure<'src>(
    tokens: &mut [FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>>;
```

Existant, gelé, **partagé sans modification entre Mode Fragment et Mode Page**.

---

## 7. Codegen Rust

### 1. Responsabilité

Transpiler `&[FlatPageToken<'src>]` en un `String` de code Rust natif — appels directs `push_str`/`write_fmt`/`marius_html_escape`, zéro runtime de templating, zéro `format!()` alloué. C'est la dernière phase build-time ; sa sortie est ce qui survit jusqu'au binaire (via `include!()` dans `generated_schema.rs`).

**Pourquoi non fusionnable avec le Resolver :** déjà justifié en §6 — détection d'erreur avant génération de texte.

### 2. Entrée

`&[FlatPageToken<'src>]` (AST supposé correct — Parser + validation passés) + `&SchemaIndex<'_>` (pour trancher fixed-length vs varlena au niveau de chaque `Field`).

### 3. Sortie

`String` — corps de fonction Rust syntaxiquement valide, sans signature ni accolades englobantes (celles-ci sont émises par l'orchestrateur). N'émet pas `buf.reserve()` — responsabilité de l'orchestrateur, qui référence `PAGE_TOTAL_CAP` calculé depuis `TemplateMetrics`.

### 4. Invariants

- Entrée : AST syntaxiquement et sémantiquement clos (phases 1 à 6 passées).
- Sortie : le texte généré compile sans dépendre d'aucune information supplémentaire au-delà de `record`, `varlena`, `buf` dans le scope appelant. Indentation plate (2 niveaux max), garantie par l'absence d'imbrication dans `FlatPageToken` lui-même.

### 5. Concepts éliminés

AST, `FlatPageToken`, `SchemaIndex` — après cette phase, il ne reste qu'un texte Rust opaque, indifférencié de tout autre code source du projet.

### 6. Gestion mémoire

Une seule allocation dimensionnée par avance (`String::with_capacity(25 + tokens.len() * 60)`) — heuristique de pré-réservation pour éviter les réallocations du buffer de génération lui-même (build-time, sans rapport avec `PAGE_TOTAL_CAP` qui borne le buffer _runtime_). Déclarations de références varlena dédupliquées et triées (`varlena_seen.sort_unstable(); dedup();`) en tête de fonction — un champ référencé N fois dans le template ne produit qu'une seule déclaration `let {name}_ref`, même si sa contribution à `total_dynamic_bytes` est comptée N fois (aucune déduplication de capacité — seule la déclaration de code l'est).

### 7. Complexité

`O(n)` en nombre de tokens. Pure, déterministe. I/O : aucune. Parallélisable entre templates indépendants (aucun état partagé).

### 8. Erreurs

Aucune — `generate_aot_snippet` ne retourne pas de `Result`. Toute condition d'échec (champ inconnu, capacité non calculable) a déjà été éliminée par les phases amont ; le Codegen suppose un AST correct par construction et ne revalide rien.

### 9. API publique

```rust
pub fn generate_aot_snippet<'src>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
) -> String;
```

Existant, gelé, **partagé sans modification entre Mode Fragment et Mode Page**.

---

## 8. Orchestrateur (`build.rs`) — articulation Fragment / Page

`build.rs` est la seule couche autorisée à faire de l'I/O disque (lecture des `.marius`, `std::fs::metadata`, écriture du fichier généré) — Fragment-Forge reste un générateur pur qui reçoit des résultats déjà calculés. Dans son état actuel :

- `resolve_template()` appelle **uniquement** la chaîne Mode Fragment (`scan → parse_tokens → validate_ast → resolve_and_measure → generate_aot_snippet`) pour chaque table dont un fichier `templates/{schema}/{table}.marius` existe.
- Il n'existe **aucune détection de mode** dans `build.rs` : pas de branchement sur la présence d'un `{% extends %}` en tête de fichier. Tout fichier `.marius` trouvé est traité comme un fragment.
- Il n'existe **aucun appel** à une fonction Mode Page (`detect_extends`, un futur `parse_page_template`, un futur Linker/Normalizer) — cohérent avec le fait que ces fonctions ne sont pas encore écrites (Phase 3.0 : « non câblé »).

Conséquence architecturale directe : le branchement Mode Fragment / Mode Page (une fois le Parser/Linker/Normalizer Mode Page écrits) doit s'insérer **dans `resolve_template()`**, avant l'appel à `scan()` — sur la base d'un `detect_extends(&src)` lisant uniquement la position de la première construction non-whitespace. Les deux branches convergent ensuite vers le même appel `validate_ast` / `resolve_and_measure` / `generate_aot_snippet`, déjà écrit et déjà appelé aujourd'hui pour le Mode Fragment. Aucune modification de `build.rs` en aval de l'obtention d'un `Vec<FlatPageToken<'src>>` n'est nécessaire pour supporter le Mode Page — c'est la preuve opérationnelle de la convergence décrite en §0.

---

## 9. Tableau récapitulatif du pipeline

| Phase                      | Statut               | Entrée                                       | Sortie                                 | Concepts éliminés                                                       |
| -------------------------- | -------------------- | -------------------------------------------- | -------------------------------------- | ----------------------------------------------------------------------- |
| Scanner                    | Gelé, partagé        | `&'src str`                                  | `Iterator<RawSpan<'src>>`              | Recherche de délimiteurs bruts, frontières UTF-8                        |
| AST Front-end (Fragment)   | Gelé                 | `Iterator<RawSpan<'src>>`                    | `Vec<FlatPageToken<'src>>`             | Texte non catégorisé, délimiteurs bruts                                 |
| AST Front-end (Page)       | Scaffoldé, non câblé | `Iterator<RawSpan<'src>>`                    | _(union non tranchée)_                 | Position de `extends`, mots-clés bruts `block`/`static`                 |
| Validation de forme (Page) | Scaffoldé, non câblé | AST mono-fichier                             | `Result<(), Vec<PageValidationError>>` | Boucle, imbrication, condition non-bool, mot-clé relationnel            |
| Linker                     | Non écrit            | `ChildTemplateSpec` + AST parent             | Correspondances validées               | Existence incertaine d'un fichier/bloc référencé                        |
| Normalizer                 | Non écrit            | AST parent + correspondances                 | `Vec<FlatPageToken<'src>>`             | **Composition entière** : `extends`, `block`, distinction parent/enfant |
| **FlatPageToken (IR)**     | Gelé, pivot          | —                                            | —                                      | Tout concept de syntaxe ou de composition                               |
| Resolver / Measurement     | Gelé, partagé        | `&mut [FlatPageToken<'src>]` + `SchemaIndex` | `TemplateMetrics`                      | Taille de fichier inconnue, champ non vérifié, capacité indéterminée    |
| Codegen                    | Gelé, partagé        | `&[FlatPageToken<'src>]` + `SchemaIndex`     | `String` (Rust)                        | AST, schéma — ne reste que du texte Rust opaque                         |

**Usage recommandé de ce tableau :** toute PR qui touche `lib.rs` doit être relue contre la colonne « Concepts éliminés ». Une PR qui fait apparaître une notion déjà éliminée dans une phase antérieure (ex. un `BlockOpen` dans la sortie du Resolver, ou une allocation de `String` owned dans `FlatPageToken`) est une régression architecturale par définition — indépendamment de la question de savoir si le code compile ou si les tests passent.

---

## 10. Dette documentaire identifiée

Ces écarts entre la spécification v1.1, la roadmap et le code réel ne sont **pas corrigés** dans ce document — ils sont signalés pour une remise en cohérence ultérieure de la spécification elle-même.

1. **`FlatPageToken` owned vs emprunté.** La spécification §3.2 décrit `PageToken`/`FlatPageToken` avec des champs `String` (owned). Le code gelé porte `&'src str` partout (refactor de lifetime, roadmap Phase 1.1, postérieure à la date de la spec v1.1). La spec doit être mise à jour pour refléter l'AST emprunté — c'est un changement de représentation mémoire, pas de sémantique.

2. **Algorithme de fusion monolithique vs phases séparées.** La spécification §3.3 décrit la composition comme un algorithme unique en cinq étapes (parse enfant → parse parent → fusion → résolution statique → validation sémantique), toutes mélangées dans une seule description procédurale. Le scaffolding réel (Phase 3.0) sépare explicitement trois domaines d'erreur typés — `PageComposeParseError` (Parser), `PageLinkError` (Linker), `PageValidationError` (validation de forme) — correspondant à trois phases distinctes du pipeline classique désormais adopté. La spec doit être réécrite pour refléter cette décomposition, pas la fusion originelle.

3. **Structure récursive rejetée.** La spécification §3.2 propose `BlockDecl { name: String, default: Vec<PageToken> }` — une structure imbriquée. Le scaffolding réel (`NamedBlockRange`) rejette explicitement cette forme au profit de plages d'indices `[start, end)` dans une arène plate taguée par `TemplateId`, avec un invariant de platitude documenté en commentaire (« aucune variante ne porte de `Vec<Self>` imbriqué »). C'est un renversement de décision architecturale que la spec doit intégrer, pas seulement une reformulation.

4. **Représentation physique du booléen.** La spécification décrit `{% if entity.bool_field %}` en termes de « bit booléen » sans jamais mentionner explicitement la contrainte `bytemuck::Pod` qui interdit `bool` dans `StorageRow` (`#[repr(C)]`) et impose un `u8`-sentinelle testé `!= 0`. Le code documente ce choix (Phase 3.0) comme déjà tranché et non renégociable au niveau du codegen. La spec devrait l'énoncer explicitement en §1.1/§13, pas le laisser implicite.

5. **API Mode Page non implémentée, non détectée par l'orchestrateur.** La spécification §5.1 propose des signatures publiques (`detect_extends`, `parse_page_template`) présentées comme existantes. Dans le code réel, seuls des types de données (Phase 3.0) sont scaffoldés — aucune des cinq fonctions du pipeline Mode Page n'est écrite, et `build.rs` ne contient aucun branchement de détection de mode (§3.1 de la spec, absent du code). Le Mode Page n'existe donc pas de bout en bout aujourd'hui ; c'est l'écart le plus significatif entre spec et réalité.

6. **Scanner partagé, non documenté comme tel.** Aucun document (spec, roadmap) n'énonce explicitement que `scan()`/`RawSpan` sera réutilisé sans modification par le futur parseur Mode Page. C'est une conséquence directe de l'architecture actuelle, mais elle mérite d'être actée par écrit avant l'écriture du Parser Mode Page, pour éviter qu'une future session ne duplique le Scanner par excès de prudence.

7. **Roadmap non étendue au Mode Page.** La roadmap (`roadmap-marius-compilateur-projections-html.md`) documente exclusivement le pipeline Fragment (Phases 1 à 3, elle-même consciente de ce périmètre : « ces deux pipelines coexistent dans la même crate »). Elle ne couvre pas la scaffolding Phase 3.0 ni les phases Linker/Normalizer décrites ici. Une branche de roadmap dédiée (Phase 4.x ou équivalent) devrait être ouverte pour ces trois phases avant toute implémentation.

---

_le 2 juillet 2026_
