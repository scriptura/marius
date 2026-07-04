# Rapport de fin de phase — Phase 5.5

**Périmètre :** `link` (Document 2 §4) — appariement parent/enfant par nom, sans E/S. Types `BlockSubstitution`, `LinkPlan` introduits.

---

## 1. Livrables

Tests ajoutés (module `tests_phase_5_5_link`) :

- `child_override_replaces_parent_range` — override simple : nom identique côté enfant → substitution pointant vers la plage enfant.
- `parent_range_kept_when_no_override` — pas d'override → fallback sur la plage parent.
- `child_block_without_parent_match_is_orphan` — bloc enfant sans nom correspondant côté parent → `Err(vec![OrphanBlock])`.
- `substitutions_len_always_equals_parent_blocks_len` — invariant de complétude vérifié directement (3 blocs parent, 1 override → `len() == 3`).

Suite complète : 58 tests verts (54 préexistants inchangés + 4 nouveaux).

---

## 2. Analyse architecturale de la phase

**Invariants introduits.**

1. `substitutions.len() == parent_blocks.len()` toujours vrai pour tout `LinkPlan` retourné en `Ok` — garanti par construction (une itération sur `parent_blocks`, une poussée par itération, aucun `continue`), pas seulement par test.
2. Séparation stricte entre construction du plan (boucle sur `parent_blocks`) et détection d'orphelins (boucle sur `child_blocks`) : deux axes indépendants du même contrat, chacun sur son propre `Vec` source — aucune mutation croisée.
3. `link` reçoit `static_refs`/`file_exists` dans sa signature sans les utiliser (paramètres préfixés `_`) : la signature est verrouillée dès cette phase pour que 5.6 n'ait pas à rompre l'API publique — décision explicitement recommandée par la roadmap, pas une anticipation de comportement.

**Invariants confirmés.** Le style fail-slow acté en 5.3/5.4 se retrouve à l'identique : les deux boucles de `link` vont jusqu'au bout indépendamment du contenu de `errors`, la décision `Ok`/`Err` se prend uniquement à la fin — même politique que `collect_blocks`, appliquée à un nouveau contrat (correspondance de blocs plutôt qu'appariement de pile).

**Invariants devenus inutiles ou faux.** Aucun. Cette phase n'a pas touché aux types gelés (`NamedBlockRange`, `PageLinkError`, `TemplateId`) — extension pure par ajout de deux nouveaux types et d'une fonction.

**Mesures réelles.** `BlockSubstitution` est `Copy` (agrégat de `&'src str` et `NamedBlockRange`, déjà `Copy`) — pas de `size_of` explicitement requis par la roadmap à ce stade, aucune mesure de layout demandée pour cette phase (contrairement à 4.1). `LinkPlan` n'est pas `Copy` (porte un `Vec`), cohérent avec son rôle de résultat alloué, jamais de structure runtime.

**Hypothèses des documents — confirmée.** Document 2 §4 annonçait la signature complète (`parent_blocks`, `child_blocks`, `static_refs`, `file_exists`) comme le contrat final du Linker, sans trancher si `static_refs`/`file_exists` devaient apparaître dès le premier diff d'implémentation ou seulement au diff qui les consomme. La roadmap §5.5 tranche explicitement pour la signature complète immédiate — confirmé et appliqué ici sans écart.

---

## 3. Impact documentaire

- **Aucune documentation existante obsolète.** Document 2 §4 décrivait déjà exactement cette signature et cette règle de construction — aucun écart entre le contrat écrit et l'implémentation.
- **Rien à corriger.** Roadmap §5.5 est un jalon clos sans ambiguïté restante.
- **À régénérer en fin d'implémentation complète :** la note de tête sur `_static_refs`/`_file_exists` (paramètres présents mais inertes) devrait disparaître de la documentation de tête de `link` une fois 5.6 les aura branchés — attendu, pas un défaut de cette phase.

---

## 4. Impact sur la roadmap

- **Pertinence des phases suivantes :** inchangée. 5.6 branchera `static_refs`/`file_exists` sur la signature déjà posée ici — aucune re-signature nécessaire, exactement l'objectif visé par le choix de signature complète.
- **Fusion/découpage :** aucun candidat. Le découpage 5.5/5.6 (appariement par nom, puis vérification `static`) reste pertinent : deux invariants distincts, deux diffs distincts.
- **Risques disparus :** le risque de re-signature de `link` en 5.6 (mentionné explicitement par la roadmap comme motivation du choix) est éliminé — la signature ne bougera plus.
- **Risques apparus :** aucun.
- **Signatures simplifiables :** aucune — signature figée dès cette phase par décision de roadmap.
- **Structures devenues inutiles :** aucune.
- **Implémentation plus élégante que celle des documents :** aucun écart — l'algorithme à deux boucles indépendantes est la traduction directe et minimale du contrat Document 2 §4, sans simplification supplémentaire identifiée.

---

## 5. Regard d'architecte

**Propriété révélée, non anticipée explicitement par les documents.** Le contrat Document 2 §4 énonce deux garanties (règle de substitution par défaut, détection d'orphelin) comme si elles procédaient d'un seul parcours. L'implémentation révèle qu'elles portent sur deux ensembles distincts (`parent_blocks` pour la première, `child_blocks` pour la seconde) et qu'aucun parcours unique ne peut les produire toutes les deux sans stocker une structure intermédiaire (une table de recherche par nom, hors périmètre ici — non introduite, cf. absence de `HashMap` dans ce diff). La complexité résultante (`O(P×C)`, deux boucles à recherche linéaire) est acceptée telle quelle : le contrat Document 2 §4 ne fixe aucune borne de complexité pour `link` (contrairement à `collect_blocks`, où le Document 2 §3 est explicite sur le coût `O(n)` par la contrainte de pile). Une table de correspondance par nom réduirait `O(P×C)` à `O(P+C)`, mais l'introduire maintenant serait une optimisation non demandée par cette phase sur des tailles de collections (blocs par template) typiquement à un chiffre.

**Portage recommandé :** à conserver pour la synthèse finale de l'implémentation plutôt que porter dans le code ou une ADR immédiatement — la trace utile ici est « si les volumes de blocs par page croissent significativement, remplacer les deux boucles linéaires par une table de recherche par nom sur `parent_blocks` », une décision de performance conditionnelle, pas un invariant structurel à figer maintenant.

---

## Confirmation finale

- `cargo fmt -- --check` : 10 divergences pré-existantes (identiques à la baseline, imputables à la version de `rustfmt` de l'environnement de vérification) — **aucune nouvelle divergence introduite par ce diff**.
- `cargo test` : **VERT** — 58 passed; 0 failed.
- `cargo clippy --all-targets` : **VERT** sur le code de cette phase — un seul avertissement pré-existant (`needless_lifetimes` sur `generate_aot_snippet`, fonction gelée hors périmètre), identique à la baseline.
- **Périmètre Phase 5.5 strictement respecté :** deux types de données purs ajoutés (`BlockSubstitution`, `LinkPlan`), une fonction `link` implémentant uniquement la correspondance par nom, `static_refs`/`file_exists` présents dans la signature mais explicitement non branchés (`_`-préfixés, aucune boucle de vérification de fichier écrite) — conforme au découpage 5.5/5.6 de la roadmap. Aucun `todo!`/`unimplemented!`, aucune anticipation de logique 5.6+.
