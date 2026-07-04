# Rapport de fin de phase — Phase 5.2 (`collect_blocks`, cas non imbriqué)

## 1. Livrables

**Code ajouté** : `pub fn collect_blocks<'src>(TemplateId, &[PageSourceToken<'src>]) -> Result<Vec<NamedBlockRange<'src>>, Vec<PageValidationError<'src>>>`. Appariement `BlockOpen`/`BlockEnd` par pile LIFO (`Vec<(&'src str, usize)>`), une seule passe linéaire. Chemin heureux uniquement — conforme au signature exacte du contrat Document 2 §3 et de la roadmap §5.2.

**Tests ajoutés** (module `tests_phase_5_2_collect_blocks`, 1 test, strictement celui prescrit par la roadmap) :
- `two_top_level_blocks_produce_exact_ranges` — 2 blocs top-level → 2 `NamedBlockRange`, indices `start`/`end` vérifiés exactement (pas seulement le compte de plages), conforme au jalon vert §5.2.

Diff : **144 lignes ajoutées, 0 supprimée, 0 fichier touché en dehors de `lib.rs`** (voir `phase-5.2.diff`, isolé par rapport à l'état de fin de Phase 5.1).

## 2. Analyse architecturale de la phase

**Invariants introduits :**
- Appariement `BlockOpen`/`BlockEnd` correct par pile pour toute profondeur — la pile LIFO referme d'abord le bloc le plus interne, donc traite déjà correctement l'imbrication *sans la rejeter* (rejet différé à 5.3, cf. ci-dessous).
- Convention `[start, end)` de `NamedBlockRange` concrétisée pour la première fois par du code exécutable : `start = index(BlockOpen) + 1`, `end = index(BlockEnd)` — les indices excluent les marqueurs eux-mêmes, conforme à la doc de tête du type (Phase 3.0, gelée).
- Deux choix de périmètre explicitement actés et documentés dans le corps de la fonction (roadmap §5.2 exigeait ce choix explicite plutôt qu'un `todo!` silencieux) :
  1. `PageSourceToken::Unsupported` : traité comme contenu opaque (branche `_`), aucune erreur produite — la variante « retour `Ok` uniquement pour l'instant » a été retenue plutôt que des `unreachable!` sur ces branches, parce qu'un `Unsupported` est un cas *réellement atteignable* par la grammaire figée en Phase 4.7 : marquer une branche atteignable `unreachable!` aurait été un mensonge documentaire, pas une simplification honnête.
  2. Profondeur d'imbrication > 1 : non détectée, silencieusement acceptée (propriété algorithmique de la pile, pas un test manquant) — la Phase 5.3 ajoutera la condition qui la transforme en `PageValidationError::NestedBlock`.

**Invariant existant confirmé :**
- `NamedBlockRange` (Phase 3.0, gelé, `Copy`/`Eq`) ne nécessite aucune extension pour porter le résultat de `collect_blocks` — sa forme était déjà correcte par anticipation.
- `PageSourceToken` (Phase 4.7, gelé) reste inchangé : `collect_blocks` le consomme par référence, sans nouvelle variante.

**Invariant devenu inutile ou faux :** aucun.

**Mesures réelles obtenues :**
- `size_of::<NamedBlockRange<'static>>() == 40` octets (`&str` = 16, `TemplateId` = 4 + padding, `usize` × 2 = 16 — layout confirmé sans surprise, aucune indirection ajoutée par `collect_blocks`).
- Complexité : une seule boucle `O(n)` sur `tokens`, une pile dont la profondeur reste bornée par l'imbrication réelle du fichier (jamais allouée avant le premier `BlockOpen`, cf. Document 2 §3) — vérifiable par lecture directe du corps de la fonction, aucune allocation cachée.

**Hypothèses des documents confirmées/infirmées :**
- Document 2 §3 confirmé à l'identique : « pile de profondeur bornée par l'imbrication réelle, jamais allouée avant le premier `BlockOpen` » — l'implémentation n'alloue la pile qu'au premier `push`.
- Confirmé également que ce sous-contrat ne dépend d'aucun `SchemaIndex` ni de connaissance d'un second fichier — la signature ne porte que `TemplateId` (tag) et le flux du fichier courant.

## 3. Impact documentaire

- **Aucune documentation ne devient obsolète.**
- **Rien à corriger dans l'immédiat** — le contrat Document 2 §3 est implémenté sans écart pour son sous-ensemble « chemin heureux ».
- **Point à surveiller pour la synthèse finale (pas à corriger maintenant)** : cette phase a mis au jour un point réellement non tranché par le Document 2 — un flux structurellement déséquilibré (`BlockEnd` sans `BlockOpen`, ou `BlockOpen` non refermé) n'est couvert par aucune variante de `PageValidationError`. Ce n'est pas une omission de cette session : c'est un vide de spécification préexistant, révélé seulement maintenant qu'un code exécutable tente de le gérer. À consigner pour la synthèse finale de l'implémentation (voir §5).

## 4. Impact sur la roadmap

- **5.3–5.9 restent pertinentes telles que découpées.** 5.3 (détection `NestedBlock`) s'insère exactement comme prévu : une condition à ajouter dans la boucle existante, sans toucher la structure de `collect_blocks` écrite ici.
- **Aucune fusion ni découpage suggéré** par cette implémentation.
- **Risque disparu** : l'algorithme de pile s'est révélé trivialement correct pour l'appariement multi-niveaux (LIFO), ce qui simplifie l'implémentation anticipée de 5.3 — détecter l'imbrication ne demandera qu'un test de non-vacuité de la pile *avant* le `push`, pas une restructuration.
- **Risque nouveau identifié** : le point ouvert §3 ci-dessus (déséquilibre structurel non nommé par `PageValidationError`) devra être tranché avant la clôture du Document 2 (avant 5.9) — actuellement un `panic!` documenté, pas une erreur récupérable. Si le Parser (Document 1, gelé) ne garantit pas l'équilibre par construction, une nouvelle variante d'erreur sera nécessaire dans une phase ultérieure (hors périmètre 5.2/5.3, qui ne couvrent que `NestedBlock`).
- **Signature inchangée** par rapport au contrat documenté — déjà minimale.
- **Aucune structure de données devenue inutile.**
- **Aucune implémentation plus élégante identifiée** que celle décrite par le Document 2 §3.

## 5. Regard d'architecte

Cette implémentation révèle une propriété que les documents n'avaient pas nommée explicitement : **la pile d'appariement ne « détecte » pas l'imbrication au sens strict — elle la résout correctement par construction (LIFO), et 5.3 ne fera qu'ajouter une *interdiction* sur un cas déjà géré correctement à l'exécution.** Autrement dit, la frontière entre « chemin heureux » (5.2) et « détection d'erreur » (5.3) n'est pas une frontière de capacité algorithmique, mais une frontière de *politique* (accepter vs rejeter un cas que la structure de données traite déjà sans ambiguïté).

Cette propriété doit être portée par **la doc de code de `collect_blocks` elle-même** (déjà fait dans ce diff — cf. commentaire de tête, point 2) : elle éclaire directement l'implémentation de 5.3 et évite qu'un futur lecteur ne réintroduise une structure de pile différente en croyant que la version 5.2 était incapable de gérer la profondeur > 1. Pas de matière à ADR séparée : c'est un détail d'implémentation d'une fonction déjà entièrement contractualisée par le Document 2, pas une décision d'architecture nouvelle. Le point ouvert sur le déséquilibre structurel (§3 ci-dessus), en revanche, mérite d'être conservé pour la synthèse finale de l'implémentation complète, car il pourrait nécessiter une extension de `PageValidationError` que ni la roadmap ni le Document 2 n'anticipent aujourd'hui.

---

## Confirmations

- `cargo fmt --check` : **10 écarts, identiques en nombre et en nature à ceux déjà présents à la fin de la Phase 5.1** (réordonnancement d'imports dans des modules de tests gelés, dû à la version de `rustfmt` de cet environnement de vérification). **Le diff de cette phase n'introduit aucun écart supplémentaire** (l'ordre des imports du nouveau module `tests_phase_5_2_collect_blocks` a été aligné manuellement sur la convention locale pour l'éviter).
- `cargo test` : **VERT — 49/49 tests passent** (48 préexistants + 1 nouveau de la Phase 5.2), aucune régression.
- `cargo clippy --all-targets` : **1 avertissement préexistant**, dans `generate_aot_snippet` (Phase 2.2, gelé, hors périmètre), identique avant/après le diff. **Aucun avertissement nouveau.**
- **Périmètre strictement respecté** : seule la fonction `collect_blocks` et son test prescrit ont été ajoutés. Aucune détection `NestedBlock`/`ForLoopDetected`/`RelationalKeyword` anticipée (5.3/5.4) ; aucune logique de `link`/`lower` introduite ; aucun fichier autre que `lib.rs` modifié.
