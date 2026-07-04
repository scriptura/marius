# Rapport de fin de phase — 5.9 : `lower` (substitution effective)

## 1. Livrables

**Extension de fonction** : `lower` (signature inchangée depuis 5.8) — le corps traite désormais `PageSourceToken::Block`, complétant la boucle principale.

**Fonctions privées ajoutées** :
- `lower_leaf_token` — projection `Runtime`/`Static` factorisée, partagée entre le niveau racine et le contenu d'une plage substituée.
- `find_matching_block_end` — recherche du `BlockEnd` apparié à un `BlockOpen`, sans pile (blocs non imbriqués, précondition garantie en amont).

**Test ajouté** :
- `tests_phase_5_9_lower_substitution::overridden_block_uses_child_content_untouched_block_keeps_parent_content` — end-to-end en mémoire : parent à 2 blocs (`title`, `footer`), enfant qui redéfinit `title` seulement. `ParsedPageTemplate` construits à la main, admis en arène, `collect_blocks`/`link`/`lower` enchaînés. Séquence `FlatPageToken` exacte vérifiée par `assert_eq!` sur le `Vec` entier.

Aucun autre test ajouté (roadmap §5.9 n'en prévoit qu'un seul).

## 2. Analyse architecturale de la phase

**Invariant introduit** : le contenu émis pour un bloc dépend *exclusivement* de `LinkPlan` — jamais implicitement du contenu physiquement situé entre les délimiteurs `BlockOpen`/`BlockEnd` de `parent_tokens`. Techniquement : la plage lue est toujours `arena.get(source.template).tokens[start..end]`, jamais une sous-tranche de `parent_tokens`, y compris quand `source.template` est le parent lui-même (cas non redéfini). Ceci clôt le domaine composition (Document 2 §1) : à la sortie de `lower`, `Vec<FlatPageToken<'src>>` ne peut, par construction du système de types, porter aucune trace d'héritage.

**Invariants existants confirmés** :
- « Les blocs ne sont pas imbriqués » (Document 2 §3, `NestedBlock` rejeté en Phase 5.3) : confirmé en le réutilisant comme précondition de sûreté de `find_matching_block_end` (premier `BlockEnd` rencontré = fermeture correcte, aucune pile nécessaire).
- « Le Lowering suppose une entrée déjà validée » (Document 2 §5) : confirmé et étendu — `lower_leaf_token` panique sur `Block`/`Unsupported` non seulement au niveau racine (déjà le cas en 5.8) mais aussi à l'intérieur d'une plage substituée, sans logique supplémentaire dupliquée.
- `substitutions.len() == parent_blocks.len()` (invariant de `link`, Phase 5.5) : confirmé en l'exploitant implicitement — chaque `BlockOpen` du parent trouve nécessairement une entrée dans `plan.substitutions` construite depuis les mêmes `parent_blocks`.

**Invariants devenus inutiles ou faux** : la borne de capacité exacte `Vec::with_capacity(parent_tokens.len())` posée en 5.8 est devenue fausse dès cette phase — remplacée par `Vec::new()` (croissance non bornée a priori, la longueur de sortie n'est plus 1:1 avec l'entrée dès qu'un bloc est substitué). Changement anticipé et documenté dès la doc de tête de la Phase 5.8 elle-même (« capacité à réévaluer à ce moment »).

**Mesures réelles obtenues** :
- `cargo test` : 64/64 tests verts (63 préexistants + 1 nouveau).
- `cargo check --all-targets` : vert — jalon de compilation explicitement requis par la roadmap §5.9 (« aucun match exhaustif sur `FlatPageToken` ailleurs dans le crate n'a besoin d'un nouveau bras »), confirmé : `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` n'ont reçu aucune modification.
- `cargo clippy --all-targets` : un warning `needless_lifetimes` a été introduit par la première version de `find_matching_block_end` (lifetime `'src` explicite sur une fonction dont le type de retour, `usize`, ne la porte pas) — corrigé immédiatement par élision (`&[PageSourceToken<'_>]`). Aucun warning résiduel imputable à cette phase.

**Hypothèses des documents confirmées ou infirmées** :
- Confirmée : Document 2 §5, « Le contenu de la plage retenue … est projeté récursivement par les mêmes règles » — implémenté littéralement via `lower_leaf_token`, sans duplication de la logique `Runtime`/`Static` entre les deux niveaux d'appel.
- Confirmée : Document 2 §1, postcondition finale du domaine composition — `FlatPageToken<'src>` sans variante d'héritage possible, vérifiée à la fois par le typage (aucune variante `Block` dans `FlatPageToken`) et par le jalon de compilation `cargo check` demandé par la roadmap.

## 3. Impact documentaire

- **Aucune documentation devenue obsolète.** Document 2 §5 est désormais entièrement couvert par le code (Arène → Collecte → Linker → Lowering, les quatre sous-contrats du §0 sont clos).
- **À corriger à terme (mineur)** : aucun écart identifié entre le contrat et l'implémentation.
- **À régénérer en fin d'implémentation complète** : le tableau récapitulatif du Document 2 §1 (quatre sous-contrats) pourrait gagner une note indiquant que les quatre sont désormais implémentés (pas seulement spécifiés) — cosmétique, à faire lors de la régénération globale plutôt que maintenant.

## 4. Impact sur la roadmap

- **Le Document 2 est clos** : Arène (5.1), Collecte de blocs (5.2-5.4), Linker (5.5-5.6), Lowering (5.8-5.9) sont tous implémentés et testés. La prochaine phase relève de l'orchestration (Document 3) : câblage E/S réel (lecture de `extends`, admission en arène depuis `build.rs`), qui n'a pas été anticipé ici.
- **Aucune fusion ni découpage supplémentaire identifié** pour les phases déjà closes.
- **Risque disparu** : le risque documenté en 5.8 (« capacité `Vec` à réévaluer ») est résolu, pas simplement déplacé — `Vec::new()` est désormais correct par construction, aucune estimation à recalibrer davantage tant que le contrat de `lower` ne change pas.
- **Aucun nouveau risque identifié.**
- **Signatures/structures inchangées** : rien à simplifier, aucune structure devenue inutile.
- **Implémentation plus élégante que celle décrite ?** Non — correspond au contrat roadmap §5.9 sans écart ; la factorisation `lower_leaf_token` est une conséquence directe et attendue de la règle « projetée récursivement par les mêmes règles » du Document 2, pas une simplification supplémentaire inventée.

## 5. Regard d'architecte

Une propriété mérite d'être nommée, bien qu'elle ne soit pas une découverte au sens strict (le Document 2 §1 l'énonçait déjà comme intention) : cette phase la rend *vérifiable mécaniquement*, pas seulement énoncée en commentaire. Le jalon « `cargo check` confirme qu'aucun `match` exhaustif existant sur `FlatPageToken` n'a besoin d'un nouveau bras » transforme une promesse documentaire (« le Lowering est la dernière étape du domaine composition ») en une garantie du compilateur : toute régression future qui réintroduirait une notion d'héritage dans `FlatPageToken` casserait la compilation des consommateurs gelés (`validate_ast`, `resolve_and_measure`, `generate_aot_snippet`), pas seulement un test. Cette propriété est déjà portée par le code lui-même (le système de types) et par le commentaire de tête de cette phase — elle ne nécessite ni ADR ni modification de la spécification, seulement d'être reprise telle quelle dans la synthèse finale de l'implémentation comme preuve de clôture du Document 2.

---

## Confirmations finales

- `cargo fmt` : **conforme sur le périmètre de la Phase 5.9** — aucun diff introduit par le code ajouté ou modifié ; les 10 diffs préexistants (identiques en nombre et en position à ceux déjà signalés en 5.7/5.8) restent inchangés, hors périmètre.
- `cargo test` : **VERT** — 64/64 tests passent, aucune régression.
- `cargo clippy` : **VERT** — un warning transitoire introduit par cette phase (`needless_lifetimes` sur `find_matching_block_end`) a été corrigé avant livraison ; seul le warning préexistant de la Phase 2.2 subsiste, hors périmètre.
- **Périmètre de la Phase 5.9 strictement respecté** : signature de `lower` inchangée (posée en 5.8), corps étendu exactement au traitement `Block`/substitution prévu par le contrat, aucune préparation de logique d'orchestration (Document 3), aucun `todo!`/`unimplemented!`.
