# Rapport de fin de phase — Phase 5.4

**Périmètre :** extension de `collect_blocks` (Document 2 §3) — branche sur `PageSourceToken::Unsupported`, mapping vers `PageValidationError::ForLoopDetected` / `RelationalKeyword`.

---

## 1. Livrables

Tests ajoutés (module `tests_phase_5_4_unsupported_mapping`) :

- `for_keyword_produces_for_loop_detected` — `for` isolé → `Err(vec![ForLoopDetected])`.
- `relational_keywords_produce_relational_keyword_error` — paramétré sur `join`/`where`/`filter`/`group` → `Err(vec![RelationalKeyword { keyword }])` pour chacun.
- `arbitrary_unsupported_keyword_also_produces_relational_keyword_error` — mot-clé hors liste connue (`frobnicate`) → même variante, pas de troisième branche silencieuse.
- `two_unsupported_keywords_in_same_stream_accumulate_both_errors` — `for` + `where` dans un même flux → `Vec` de longueur 2 (fail-slow), pas fail-fast.

Suite complète : 54 tests verts (50 préexistants inchangés + 4 nouveaux).

---

## 2. Analyse architecturale de la phase

**Invariant introduit.** Mapping total et nommé de `PageSourceToken::Unsupported` vers `PageValidationError` : `keyword == "for"` → `ForLoopDetected` (variante sans charge utile) ; tout autre `keyword` → `RelationalKeyword { keyword }`. La totalité tient sur une disjonction booléenne, pas sur une énumération de mots-clés connus — un mot-clé futur, non encore nommé par la grammaire du Parser (catch-all Phase 4.7), retombe automatiquement dans `RelationalKeyword` sans modification de `collect_blocks`.

**Invariants confirmés.** Le fail-slow acté en 5.3 (la boucle ne s'interrompt jamais sur une erreur nommée) s'étend sans modification à cet axe : la nouvelle branche pousse dans le même `Vec<PageValidationError>` que `NestedBlock`, sans lecture ni écriture de `open_stack`. Les deux axes de validation (appariement de blocs, mots-clés non supportés) sont donc orthogonaux — un test le vérifie indirectement en croisant `for`+`where` dans un même flux sans bloc, mais aucune coexistence `NestedBlock`+`Unsupported` n'a été testée : elle découle du même mécanisme fail-slow, non revérifiée séparément (cf. §5).

**Invariants devenus inutiles ou faux.** Le commentaire de tête de la Phase 5.2 (« `Unsupported` traité comme contenu opaque, ignoré, ne produit aucune erreur ») était vrai en 5.2/5.3 et devient faux à partir de cette phase — corrigé dans le diff pour éviter une documentation perimée décrivant un comportement qui n'existe plus.

**Mesures réelles.** Aucune mesure de layout ou de performance : cette phase ajoute une branche de `match` sur un type déjà existant (`PageValidationError`, `Copy`), sans nouveau champ ni nouvelle structure — pas de `size_of` pertinent à re-vérifier ici (le layout de `PageValidationError` est inchangé depuis la Phase 3.0).

**Hypothèses des documents — confirmée.** Document 2 §3 annonçait deux erreurs pour cette responsabilité (`ForLoopDetected`, `RelationalKeyword`) sans préciser la règle de répartition entre les deux. L'implémentation tranche ce point implicite : `for` est le seul cas distingué nommément par la grammaire documentée (Document 1, roadmap 4.7), tout le reste du catch-all Parser est de facto relationnel ou futur-inconnu, donc `RelationalKeyword` en absorbe la totalité. Cette règle n'était pas écrite noir sur blanc dans le Document 2 — elle est maintenant portée par le code et par les tests `arbitrary_unsupported_keyword_*`.

---

## 3. Impact documentaire

- **Corrigée dans ce diff :** le commentaire de tête Phase 5.2 (point 1) faisait une déclaration devenue fausse par cette phase — mis à jour au fil de l'eau plutôt que laissé comme dette.
- **Aucune documentation externe obsolète :** Document 2 §3 reste valide tel quel (il annonçait exactement ce mapping, sans le détailler). Roadmap §5.4 est désormais un jalon clos, aucune correction nécessaire.
- **À régénérer en fin d'implémentation complète :** rien de spécifique à cette phase — la règle de répartition `for` vs. reste (§2, « hypothèse confirmée ») mériterait d'être rendue explicite dans le Document 2 §3 lors de la régénération finale, plutôt que de rester seulement dans le code et ce rapport.

---

## 4. Impact sur la roadmap

- **Pertinence des phases suivantes :** inchangée. 5.5 (`link` sans E/S) ne dépend d'aucune décision prise ici.
- **Fusion/découpage :** aucun candidat — la roadmap annonçait déjà cette phase comme une seule branche de `match`, taille confirmée par le diff réel (une branche, quatre tests).
- **Risques disparus :** le risque de « troisième cas silencieux » évoqué implicitement par « mapping total » (roadmap §5.4) est éliminé par construction (disjonction booléenne exhaustive), pas seulement par convention de code.
- **Risques apparus :** aucun nouveau. Le point ouvert Document 2 §6.1 (héritage multi-niveaux) et le `panic!` documenté sur flux mal formé (5.2) restent inchangés et hors périmètre de cette phase.
- **Signatures simplifiables :** aucune — signature de `collect_blocks` inchangée depuis 5.2.
- **Structures devenues inutiles :** aucune.
- **Implémentation plus élégante que celle des documents :** la règle de répartition retenue (`for` isolé, reste dans `RelationalKeyword`) est plus simple que l'alternative naïve (liste explicite `["join", "where", "filter", "group"]` avec un `else` non spécifié) — elle évite une liste à maintenir en synchronisation avec un futur ajout de mot-clé côté Parser.

---

## 5. Regard d'architecte

**Propriété révélée, non anticipée explicitement par les documents.** Le Document 2 §3 nommait deux erreurs (`ForLoopDetected`, `RelationalKeyword`) sans jamais énoncer que `RelationalKeyword` devait être *le complément total de `for`* plutôt qu'une liste fermée de mots-clés relationnels connus. Cette phase révèle que le catch-all Parser (Phase 4.7 : « tout mot-clé ∉ grammaire connue ») et le mapping Validation (Phase 5.4) ne peuvent rester cohérents dans la durée que si le second est lui aussi un catch-all sur le même axe — sans quoi un mot-clé futur ajouté au catch-all Parser traverserait silencieusement `collect_blocks` sans erreur nommée, rouvrant exactement le trou que la roadmap §5.4 visait à fermer (« `collect_blocks` est désormais total »).

**Portage recommandé :** cette propriété est structurelle, pas un détail d'implémentation — elle contraint toute évolution future de la grammaire `Unsupported`. Elle mérite d'être portée dans le Document 2 §3 (une phrase : *« `RelationalKeyword` est le complément de `for`, pas une liste fermée »*) plutôt que de rester seulement dans le commentaire de code et ce rapport, pour qu'un futur ajout de mot-clé au catch-all Parser (Phase 4.7) ne casse pas silencieusement cette garantie sans qu'un relecteur du Document 2 en soit averti.

---

## Confirmation finale

- `cargo fmt -- --check` : 10 divergences pré-existantes (tri des `use`, imputables à une différence de version de `rustfmt` sur l'environnement de vérification, présentes à l'identique sur la baseline avant cette phase) — **aucune nouvelle divergence introduite par ce diff**.
- `cargo test` : **VERT** — 54 passed; 0 failed.
- `cargo clippy --all-targets` : **VERT** sur le code de cette phase — un seul avertissement pré-existant (`needless_lifetimes` sur `generate_aot_snippet`, fonction gelée hors périmètre), présent à l'identique sur la baseline.
- **Périmètre Phase 5.4 strictement respecté :** une seule branche de `match` ajoutée dans `collect_blocks`, aucune logique de pile modifiée, aucun `todo!`/`unimplemented!`, aucune anticipation de 5.5+ (aucun branchement `static_refs`/`file_exists` introduit).
