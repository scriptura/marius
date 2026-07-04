# Rapport de fin de phase — 5.8 : `lower` (projection sans substitution)

## 1. Livrables

**Fonction ajoutée** : `pub fn lower<'src>(parent_tokens: &[PageSourceToken<'src>], plan: &LinkPlan<'src>, arena: &PageArena<'src>) -> Vec<FlatPageToken<'src>>`. Signature finale posée dès cette phase (conforme roadmap §5.8), corps implémentant uniquement le sous-ensemble `Runtime`/`Static`.

**Test ajouté** :
- `tests_phase_5_8_lower_no_substitution::runtime_tokens_pass_through_and_static_becomes_static_include_with_len_zero` — `LinkPlan` vide, `PageArena` par défaut, flux sans `Block` mêlant `Static`, `Field`, `IfBool`, `EndIf` → égalité valeur à valeur sur la sortie complète ; `StaticInclude { len: 0, .. }` vérifié explicitement.

Aucun autre test ajouté (roadmap §5.8 n'en prévoit qu'un seul, sur `LinkPlan` vide).

## 2. Analyse architecturale de la phase

**Invariants introduits** :
- Projection `PageSourceToken::Runtime(t) → t` : identité stricte, aucune mutation de la charge utile `FlatPageToken`.
- Projection `PageSourceToken::Static → FlatPageToken::StaticInclude { len: 0, rel_from_manifest: original_path, .. }` : couple de valeurs provisoires, résolution différée au Resolver (`len`) et à l'orchestrateur (`rel_from_manifest`) — non ce module.
- Capacité de sortie exacte (`Vec::with_capacity(parent_tokens.len())`) tant qu'aucun `Block` n'est présent : correspondance 1:1 entrée/sortie, invariant local à cette phase, appelé à disparaître en 5.9.
- Invariante de précondition documentée : tout `PageSourceToken::Block` ou `PageSourceToken::Unsupported` rencontré ici signale soit une extension non encore câblée (5.9), soit un bug de la phase amont — jamais un cas silencieusement absorbé (`unreachable!` documenté, pas de branchement deviné).

**Invariants existants confirmés** :
- Document 2 §5 : « le Lowering suppose une entrée déjà validée » — confirmé par construction, `Unsupported` ne peut structurellement pas être produit par un flux ayant traversé `collect_blocks` (Phase 5.4, clos).
- Symétrie Mode Fragment / Mode Page sur le pattern `include`/`static` (`len = 0`, `rel_from_manifest = original_path` comme valeurs provisoires) : réutilisée à l'identique, aucune divergence de convention introduite.

**Invariants devenus inutiles ou faux** : aucun.

**Mesures réelles obtenues** :
- `cargo test` : 63/63 tests verts (62 préexistants + 1 nouveau).
- `cargo clippy --all-targets` : aucun nouveau warning ; le seul warning présent (`needless_lifetimes`, `generate_aot_snippet`, Phase 2.2) est préexistant et hors périmètre.
- `cargo fmt --check` : 10 diffs, strictement identiques en nombre et en contenu à ceux déjà observés en fin de Phase 5.7 (tri d'imports dans des blocs `use` des phases 1.x–4.x, artefact de version de `rustfmt` de cet environnement) — aucun diff supplémentaire introduit par le code de la Phase 5.8.

**Hypothèses des documents confirmées ou infirmées** :
- Confirmée : Document 2 §5 anticipait « `len` provisoire, résolu par le Resolver exactement comme `{% include %}` (Mode Fragment) » — le test vérifie `len == 0` explicitement, conforme.
- Confirmée : la roadmap anticipait que poser la signature complète dès 5.8 évite une re-signature en 5.9 — `plan`/`arena` sont déjà typés et nommés, 5.9 n'aura qu'à lire leur contenu.

## 3. Impact documentaire

- **Aucune documentation devenue obsolète.**
- **À corriger à terme (mineur)** : aucun — le comportement implémenté est un sous-ensemble strict et documenté du contrat Document 2 §5, sans écart à corriger.
- **À régénérer en fin d'implémentation complète** : la doc de tête de cette phase deviendra caduque dès que 5.9 remplacera les deux `unreachable!` par la logique de splice réelle — attendu, signalé explicitement dans le commentaire de code lui-même (« capacité à réévaluer à ce moment, pas anticipée ici »).

## 4. Impact sur la roadmap

- **Phase 5.9 reste pertinente et nécessaire**, sans changement de signature : elle remplacera le bras `PageSourceToken::Block(_) => unreachable!(...)` par la logique de splice (`LinkPlan`/`PageArena` désormais lus), et ajustera l'estimation de capacité (`with_capacity` n'est plus une borne exacte dès qu'un bloc est substitué).
- **Aucune fusion ni découpage supplémentaire identifié.**
- **Aucun risque disparu ou nouveau** : le risque déjà connu (capacité `Vec` à réévaluer en 5.9) est documenté, pas nouveau.
- **Signatures/structures inchangées** : rien à simplifier.
- **Implémentation plus élégante que celle décrite ?** Non — correspond au contrat roadmap §5.8 sans écart.

## 5. Regard d'architecte

Aucune propriété non anticipée n'a été révélée. Un point mérite d'être noté pour la synthèse finale (pas une ADR, pas une correction de spec) : l'invariant de capacité exacte (`Vec::with_capacity(parent_tokens.len())`) n'est vrai que parce que cette phase interdit structurellement les `Block` en entrée — c'est une propriété *transitoire*, contrainte par le périmètre de test de 5.8, pas une propriété du Lowering en général. Le code le documente explicitement pour qu'elle ne soit pas reconduite par erreur en 5.9 sans réexamen.

---

## Confirmations finales

- `cargo fmt` : **conforme sur le périmètre de la Phase 5.8** — le code ajouté ne produit aucun diff ; les 10 diffs observés sont préexistants (déjà présents avant cette phase, cf. rapport 5.7) et hors périmètre, non corrigés conformément à la contrainte « ne modifier aucune fonctionnalité en dehors du périmètre ».
- `cargo test` : **VERT** — 63/63 tests passent, aucune régression.
- `cargo clippy` : **VERT sur le périmètre de la phase** — aucun nouveau warning ; seul le warning préexistant de la Phase 2.2 subsiste, hors périmètre.
- **Périmètre de la Phase 5.8 strictement respecté** : une seule fonction ajoutée (`lower`), signature complète posée conformément à la roadmap mais corps limité au chemin `Runtime`/`Static` ; aucun `todo!`/`unimplemented!` ; le cas `Block` (substitution, Phase 5.9) est explicitement non exercé et documenté comme tel via `unreachable!`, sans logique de splice anticipée.
