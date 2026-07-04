# Rapport de fin de phase — Phase 5.6

**Périmètre :** branchement effectif de `static_refs`/`file_exists` dans `link` (Document 2 §4). `link` clos.

---

## 1. Livrables

Tests ajoutés (module `tests_phase_5_6_link_static_check`) :

- `missing_static_file_produces_static_file_not_found` — `file_exists` renvoyant `false` → `Err(vec![StaticFileNotFound { path }])`, chemin porté tel quel.
- `existing_static_file_produces_no_error` — `file_exists` renvoyant `true` → `Ok`, aucune erreur.
- `orphan_block_and_missing_static_file_accumulate_both_errors` — un bloc enfant orphelin ET un fichier `static` manquant simultanément → `Vec` de 2 erreurs (`OrphanBlock`, puis `StaticFileNotFound`, dans l'ordre des boucles).

Suite complète : 61 tests verts (58 préexistants inchangés + 3 nouveaux).

---

## 2. Analyse architecturale de la phase

**Invariant introduit.** Existence de fichier vérifiée exclusivement via `file_exists: impl Fn(&str) -> bool` injecté — aucun `std::fs` dans ce module. Trois boucles indépendantes dans `link` (substitution, orphelin, static), chacune alimentant le même `Vec<PageLinkError>`, aucune n'interrompt les deux autres.

**Invariants confirmés.** Le fail-slow multi-axe se généralise sans friction : ajouter un troisième axe de validation n'a nécessité aucune modification des deux boucles existantes, seulement une boucle supplémentaire poussant dans le même accumulateur. Confirme la décision de signature de 5.5 (paramètres présents dès le départ) : zéro changement de signature, zéro rappel des sites d'appel existants (aucun site d'appel réel n'existe encore — hors périmètre, orchestrateur Document 3).

**Invariants devenus inutiles ou faux.** Les préfixes `_` sur `static_refs`/`file_exists` (introduits en 5.5 pour documenter l'absence de branchement) disparaissent — comportement anticipé et déjà noté dans le rapport 5.5 (§3 : « fera disparaître ce préfixe dans le même diff qui ajoutera la boucle »).

**Mesures réelles.** Aucune nouvelle structure de données, aucun changement de layout — `PageLinkError::StaticFileNotFound` était déjà défini et figé depuis la Phase 3.0 (`path: &'src str`). Pas de `size_of` à revérifier.

**Hypothèses des documents — confirmée.** Document 2 §4 : « `file_exists` : vérification d'existence, distincte de la lecture de taille faite plus tard par le Resolver ». Aucune tentative de mutualisation introduite ici — conforme.

---

## 3. Impact documentaire

- **Aucune documentation obsolète.** Document 2 §4 décrivait déjà ce mécanisme dans son intégralité ; aucun écart contrat/implémentation.
- **Rien à corriger.** Roadmap §5.6 close sans ambiguïté restante — « `link` clos (Document 2 §4 terminé) » repris tel quel dans la doc de tête du code.
- **Rien à régénérer.** Contrairement à 5.5, aucune note transitoire ne subsiste (les préfixes `_` étaient l'unique trace temporaire, désormais retirée).

---

## 4. Impact sur la roadmap

- **Pertinence des phases suivantes :** inchangée. Le Linker (Document 2 §4) est intégralement implémenté ; la suite (5.7+, Lowering) opère sur un `LinkPlan` désormais complet dans ses trois axes d'erreur.
- **Fusion/découpage :** le découpage 5.5/5.6 était pertinent a posteriori — deux diffs de taille comparable (une fonction pure, puis une boucle d'E/S injectée), chacun testable isolément sans dépendre de l'autre pour la couverture de tests.
- **Risques disparus :** le risque de troisième cas silencieux sur `PageLinkError` (un axe de validation du Linker qui resterait non branché) est éliminé — les trois variantes ont chacune leur point d'émission identifié (`OrphanBlock`/`StaticFileNotFound` ici, `ExtendsNotFound` en Document 3).
- **Risques apparus :** aucun.
- **Signatures simplifiables :** aucune.
- **Structures devenues inutiles :** aucune.
- **Implémentation plus élégante :** aucun écart identifié — la troisième boucle est la traduction directe et minimale du contrat, cohérente en style avec les deux boucles de 5.5.

---

## 5. Regard d'architecte

**Propriété révélée.** L'accumulation dans un unique `Vec<PageLinkError>` partagé entre trois boucles indépendantes (substitution — pas de push, orphelin, static) confirme que `PageLinkError` fonctionne comme un journal d'erreurs *non ordonné par gravité* mais *ordonné par ordre de détection* : `OrphanBlock` précède toujours `StaticFileNotFound` dans le `Vec` retourné, non pas parce qu'un bloc orphelin serait "plus grave" qu'un fichier manquant, mais uniquement parce que la boucle correspondante s'exécute en premier dans le corps de `link`. Les documents ne spécifient aucun ordre attendu pour `Vec<PageLinkError>` — le test `orphan_block_and_missing_static_file_accumulate_both_errors` fige cet ordre par construction (assertion sur un `Vec` exact, pas un `HashSet` ou un tri), ce qui en fait un couplage implicite entre l'ordre du code source de `link` et le contrat de test.

**Portage recommandé :** à conserver pour la synthèse finale plutôt qu'à figer maintenant en ADR. Si une réorganisation future de `link` change l'ordre des boucles (ex. static avant orphelin, pour des raisons de coût), le test `orphan_block_and_missing_static_file_accumulate_both_errors` casserait sans que cela traduise une régression réelle — signal à surveiller plutôt que contrainte à documenter dans le contrat d'architecture, l'ordre n'ayant aucune signification fonctionnelle en aval (le Lowering ne consomme pas `Vec<PageLinkError>`, seulement `LinkPlan` en cas de succès).

---

## Confirmation finale

- `cargo fmt -- --check` : 10 divergences pré-existantes (identiques à la baseline, version `rustfmt` de l'environnement) — **aucune nouvelle divergence introduite par ce diff**.
- `cargo test` : **VERT** — 61 passed; 0 failed.
- `cargo clippy --all-targets` : **VERT** sur le code de cette phase — un seul avertissement pré-existant (`needless_lifetimes` sur `generate_aot_snippet`, fonction gelée hors périmètre), identique à la baseline.
- **Périmètre Phase 5.6 strictement respecté :** une boucle ajoutée dans `link` (vérification `static_refs`/`file_exists`), zéro modification des deux boucles 5.5 (substitution, orphelin), zéro `todo!`/`unimplemented!`, zéro anticipation de logique Lowering (5.7+).
