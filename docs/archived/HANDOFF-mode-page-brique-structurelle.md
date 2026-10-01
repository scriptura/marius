# Handoff — Mode Page, première brique structurelle

Cible : `forge/fragment-forge/src/lib.rs`. Ne pas toucher `scan`,
`parse_tokens`, `validate_ast`, `resolve_and_measure`, `generate_aot_snippet`
dans cette session — voir contrainte de méthode ci-dessous.

## État actuel du compilateur

Mode Fragment seul actif. `FlatPageToken` : 5 variantes
(`Static`, `StaticInclude`, `Field`, `IfBool`, `EndIf`), plates — aucune
imbrication, FSM de validation à un seul niveau d'état
(`current_open_if: Option<(entity, field)>`). Pipeline câblé depuis
`crates/core/schema/build.rs`, un seul fichier de sortie
(`generated_schema.rs`, `include!()`).

`PageParseError` existe déjà (3 variantes : `UnexpectedToken`,
`UnexpectedEof`, `InvalidBlockSequence`), toutes orientées mode fragment.

## Contrainte de méthode

Structure avant logique. Cette session définit des types de données
additifs — publics, documentés, non branchés dans le pipeline existant.
Aucun test existant ne doit changer de comportement. Le parseur qui
produira ces types viendra dans une session ultérieure.

## Préalable bloquant — à trancher avant d'écrire le premier type

`if record.field { }` (bool natif) vs `if record.field != 0 { }` (u8,
contrainte `Pod`). La spécification v1.1 §8 illustre encore la forme bool
native pour le mode page ; le mode fragment a déjà tranché en faveur de
u8-sentinelle pour rester compatible `bytemuck::Pod` sur `StorageRow`.

Si la nouvelle variante de token pour `{% block %}` embarque elle-même des
conditions (cas probable), le choix ci-dessus détermine sa forme. Trancher
en premier, documenter la décision dans le commit qui introduit les
nouveaux types — ne pas la reporter au niveau du codegen.

## Périmètre proposé

Trois ajouts additifs, chacun avec un contrat explicite en doc-comment,
zéro logique de résolution :

1. **Marqueurs de bloc dans `FlatPageToken`.**
   Mirror direct de `IfBool`/`EndIf` (déjà validé par la FSM à un niveau) —
   pas de `Vec<FlatPageToken>` imbriqué dans la nouvelle variante, ce qui
   romprait l'invariant de platitude de l'AST. Deux marqueurs plats
   (ouverture nommée, fermeture), le contenu par défaut du bloc reste une
   plage linéaire de tokens entre les deux marqueurs — cohérent avec le
   balayage séquentiel déjà en place pour `IfBool`/`EndIf`.

2. **Type de template enfant, pré-fusion.**
   Struct pure portant le chemin `extends` et une correspondance
   nom-de-bloc → plage de tokens dans son propre AST. Aucune méthode de
   fusion sur ce type dans cette session — juste la forme des données que
   la fusion consommera.

3. **Variantes additives sur `PageParseError`.**
   `ExtendsNotFirst`, `ExtendsNotFound`, `OrphanBlock`,
   `StaticFileNotFound`, `NonBoolIfCondition`, `ForLoopDetected`,
   `RelationalKeyword`, `NestedBlock` — déclarées, non levées par aucun code
   actif pour l'instant. Code mort assumé jusqu'au câblage du parseur.

## Décision ouverte, à trancher explicitement (pas de défaut silencieux)

`{% static %}` doit-il réutiliser `StaticInclude` (comportement actuel :
`include_str!` direct, sans déduplication) ou introduire une variante
distincte ? Impact direct sur le calcul de capacité : un fichier partagé
entre plusieurs blocs doit être compté une fois (`static_partials::X.len()`)
et non une fois par occurrence. Ne pas résoudre par réutilisation par
défaut de `StaticInclude` sans avoir vérifié cet impact sur
`TemplateMetrics`.

## Definition of done pour cette session

- `cargo build` et `cargo test` passent sans régression sur le mode
  fragment.
- Les nouveaux types sont `pub`, documentés (contrat attendu, pas
  implémentation).
- Zéro appel depuis `scan`/`parse_tokens`/`validate_ast`/
  `resolve_and_measure`/`generate_aot_snippet` vers ces nouveaux types.
- Le choix bool/u8 est documenté explicitement dans le commit, même si la
  variante de bloc de cette session ne l'exploite pas encore directement.

## Hors périmètre (explicitement, pour éviter la dérive)

Fusion parent/enfant, câblage parseur, génération de code Rust pour le mode
page, résolution de l'expression de capacité `{% static %}`,
`crates/core/schema/build.rs`. Rien de tout cela dans cette session.
