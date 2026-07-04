# Rapport de fin de phase — Phase 5.3 (`collect_blocks`, détection `NestedBlock`)

## 1. Livrables

**Code modifié** : extension de `collect_blocks` (pas de nouvelle fonction). Une condition ajoutée dans la boucle existante (`if !open_stack.is_empty()` avant chaque `push`), un accumulateur `errors: Vec<PageValidationError<'src>>`, et un branchement de sortie `Ok(ranges)` / `Err(errors)` en fin de fonction. Aucune restructuration de la pile.

**Test ajouté** (module `tests_phase_5_3_nested_block_detection`, 1 test, strictement celui prescrit par la roadmap) :
- `nested_block_produces_named_error` — un bloc imbriqué (`outer` contenant `inner`) produit `Err(vec![NestedBlock { name: "inner" }])`, aucune sortie mixte.

Non-régression vérifiée : le test 5.2 (`two_top_level_blocks_produce_exact_ranges`) reste vert sans modification.

Diff : **92 lignes ajoutées, 13 supprimées (mise à jour de la doc de tête, plus périmée)** (voir `phase-5.3.diff`, isolé par rapport à l'état de fin de Phase 5.2).

## 2. Analyse architecturale de la phase

**Invariant introduit :**
- Toute profondeur d'imbrication > 1 est désormais rejetée nommément (`PageValidationError::NestedBlock { name }`, `name` = bloc le plus profond, fautif) — jamais acceptée silencieusement comme plage valide.
- Fail-slow : la boucle ne s'interrompt pas sur la première imbrication détectée ; l'empilement se poursuit (`open_stack.push` non court-circuité), ce qui permet d'accumuler une erreur par occurrence sur tout le flux, sans changement structurel supplémentaire.
- Absence de sortie mixte : `ranges` reste peuplé en interne (nécessaire pour que chaque `BlockEnd` trouve un `start`), mais n'est jamais exposé si `errors` est non vide — le type `Result` porte cette garantie par construction, pas par une vérification a posteriori.

**Invariant existant confirmé :**
- La propriété algorithmique de la pile LIFO (5.2) — appariement correct à profondeur arbitraire — est confirmée telle quelle : cette phase n'ajoute aucune capacité d'appariement nouvelle, uniquement une politique de rejet sur une capacité déjà correcte. C'est la propriété d'architecture notable de cette session (cf. §5).

**Invariant devenu inutile ou faux :**
- La phrase de doc 5.2 « la Phase 5.3 ajoutera la condition... aujourd'hui silencieusement accepté » est désormais fausse en l'état — mise à jour dans le diff (point 2 de la doc de tête).

**Mesures réelles obtenues :**
- `size_of::<PageValidationError<'static>>() == 40` octets — inchangé par cette phase (aucune nouvelle variante ajoutée à l'enum, `NestedBlock { name: &'src str }` existait déjà, gelé depuis une phase antérieure).
- Complexité inchangée : toujours `O(n)` sur `tokens`, une seule passe, une allocation additionnelle (`errors`) seulement au premier `push` (`Vec` vide tant qu'aucune imbrication n'est rencontrée).

**Hypothèses des documents confirmées/infirmées :**
- Document 2 §3 confirmé : « le Lowering... l'erreur est signalée, la pile reste sur le bloc externe, le parcours continue » (pattern déjà acté pour `NestedIfNotSupported` en Mode Fragment) — le comportement fail-slow implémenté ici en est l'équivalent exact pour `NestedBlock`.

## 3. Impact documentaire

- **Aucune documentation externe (Document 2) ne devient obsolète** — le contrat qu'il décrit était déjà celui implémenté.
- **Corrigé dans ce diff** : le commentaire de tête interne de `collect_blocks` (obsolète depuis cette session même, corrigé immédiatement, pas différé).
- **Rien à régénérer en fin d'implémentation complète** pour cette portion précise.

## 4. Impact sur la roadmap

- **5.4–5.9 restent pertinentes telles que découpées.** 5.4 (détection `ForLoopDetected`/`RelationalKeyword` sur `Unsupported`, fail-slow vérifié sur 2 erreurs simultanées) s'insère directement dans la même boucle, sur le même modèle d'accumulateur `errors` déjà en place — aucune restructuration à prévoir.
- **Aucune fusion ni découpage suggéré.**
- **Risque disparu** : l'accumulateur fail-slow étant déjà écrit et testé (bien que par un seul type d'erreur ici), 5.4 n'aura qu'à ajouter une branche supplémentaire dans le même `match`, pas à introduire le mécanisme d'accumulation lui-même.
- **Aucun risque nouveau** introduit par cette phase.
- **Signature inchangée** — `collect_blocks` garde exactement la signature du Document 2 §3.
- **Aucune structure de données devenue inutile.**
- **Aucune implémentation plus élégante identifiée.**

## 5. Regard d'architecte

Propriété confirmée, pas révélée par surprise, mais rendue explicite pour la première fois par du code exécutable : **la frontière entre 5.2 et 5.3 est une frontière de politique de rejet, pas de capacité d'appariement.** La pile LIFO gérait déjà correctement toute profondeur dès 5.2 ; 5.3 n'a fait qu'interdire un cas qu'elle savait déjà traiter. Cette distinction était déjà anticipée dans le rapport de la Phase 5.2 (§5) — cette session la confirme par l'implémentation réelle plutôt que de la découvrir.

Rien à porter vers une ADR : c'est un détail d'implémentation d'un contrat déjà entièrement spécifié par le Document 2. Le point ouvert sur le déséquilibre structurel (signalé en 5.2, toujours non représenté par `PageValidationError`) reste entier et inchangé par cette phase — à reporter, comme précédemment indiqué, vers la synthèse finale de l'implémentation complète.

---

## Confirmations

- `cargo fmt --check` : **10 écarts, identiques en nombre, en nature et en localisation à ceux déjà présents à la fin de la Phase 5.2** (imports de modules de tests gelés, Phases 1–4 ; aucune ligne du diff 5.3 concernée). **Aucun écart supplémentaire.**
- `cargo test` : **VERT — 50/50 tests passent** (49 préexistants + 1 nouveau de la Phase 5.3), aucune régression, y compris le test de non-imbrication de la Phase 5.2.
- `cargo clippy --all-targets` : **1 avertissement préexistant**, dans `generate_aot_snippet` (Phase 2.2, gelé, hors périmètre), identique avant/après le diff. **Aucun avertissement nouveau.**
- **Périmètre strictement respecté** : seule la détection `NestedBlock` a été ajoutée à `collect_blocks`, avec son unique test prescrit. Aucune détection `ForLoopDetected`/`RelationalKeyword` anticipée (5.4) ; aucune logique de `link`/`lower` introduite ; aucun fichier autre que `lib.rs` modifié.
