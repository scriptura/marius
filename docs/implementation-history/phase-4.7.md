# Rapport de fin de phase — Phase 4.7

## 1. Livrables

**Test ajouté :** `tests_phase_4_7_unsupported_catch_all::unsupported_catch_all_captures_arbitrary_keywords` (paramétré, 6 cas : `for`, `join`, `where`, `filter`, `group`, `frobnicate`).

**Test corrigé (adaptation, pas extension) :** `tests_phase_4_3::composition_keyword_out_of_scope_fails_explicitly`, fixture `for` → `include`.

## 2. Analyse architecturale de la phase

**Invariants introduits**

- Grammaire de `parse_page_block` désormais **totale** : tout `Ident` de tête produit soit un token reconnu, soit `Unsupported`, soit un rejet explicite et nommé (`include`) — plus aucun chemin de rejet générique non informatif (Document 1 §6, clos).
- Arité non contrainte pour le catch-all : 0, 1 ou N tokens avant `BlockClose`, jamais de rejet fondé sur le nombre d'arguments — ce Parser ne connaît pas la grammaire des mots-clés non supportés.
- `include` : exclusion structurelle explicite, distincte sémantiquement d'« unsupported » — sa grammaire est connue (Mode Fragment gelé) mais interdite ici par construction de type.

**Invariants confirmés**

- Zéro allocation, zéro E/S, zéro backtracking — le catch-all ne consomme chaque span qu'une fois, `tail` reste un emprunt direct.
- Permissivité délibérée (Document 1 §4/§6) inchangée sur l'imbrication des blocs.

**Invariants devenus obsolètes**

- Aucun. `PageComposeParseError::InvalidBlockSequence` change de rôle exclusif (de « catch-all temporaire » à « domaine définitif : grammaire malformée des mots-clés reconnus + `include` ») mais n'est pas retiré.

**Mesures réelles**

- `size_of::<PageSourceToken<'_>>()` inchangé (test `page_source_token_layout_is_frozen`, non touché — `Unsupported` était déjà la variante la plus large depuis la Phase 4.1, aucune extension de layout ici).

**Hypothèses confirmées/infirmées**

- Confirmée : Document 1 §2.1 anticipait exactement cette forme de catch-all.
- Précisée (non contredite) : le document ne spécifiait pas la sémantique exacte de `tail` — décision d'implémentation documentée ci-dessus, dans l'esprit du contrat (« le Parser ne décide pas pourquoi »).

## 3. Impact documentaire

- **Obsolète** : rien.
- **Corrigé dans ce diff** : commentaires internes (`PageComposeParseError::InvalidBlockSequence`, bandeau section 4.3, doc de `parse_page_block`/`parse_page_tokens`) mis à jour pour refléter la clôture de la grammaire.
- **À régénérer en fin d'implémentation complète** : le tableau §4 de Document 1 (domaines d'erreur) peut désormais être figé — Document 1 est clos côté Parser.

## 4. Impact sur la roadmap

- Document 1 est **clos** : Phase 5 (Document 2, Linker/Validation) peut démarrer — sa dépendance de type sur `PageSourceToken`/`ParsedPageTemplate` est désormais stable et totale.
- Aucune fusion, aucun découpage.
- Risque disparu : le risque qu'un mot-clé futur (ajouté côté grammaire runtime, ex. `for` un jour supporté) doive migrer _hors_ d'un catch-all fourre-tout est éliminé — le pattern d'extraction (branche dédiée dans `parse_page_block`, exclusion du catch-all) est maintenant établi et répété quatre fois (4.4 à 4.7).
- Aucun nouveau risque identifié.
- Aucune signature simplifiable, aucune structure devenue inutile.

## 5. Regard d'architecte

**Propriété révélée :** le traitement d'`include` cristallise une distinction générale que Document 1 n'articulait qu'implicitement — **« non supporté » (grammaire inconnue du Parser, jugement différé à la Validation) et « interdit » (grammaire connue, exclusion actée par construction de type) sont deux catégories sémantiquement disjointes**, et les mélanger sous un même mécanisme (`Unsupported`) aurait fait porter à Document 2 une distinction qui appartient structurellement au Parser. Cette catégorisation n'est pas spécifique à `include` : elle s'appliquera à tout futur mot-clé dont la grammaire est connue mais le mode refusé (aucun candidat actuel, mais le pattern est désormais réutilisable tel quel).

**Portage recommandé :** conservée pour la synthèse finale — c'est un principe de classification locale à `parse_page_block`, déjà rendu opérant par le bras explicite `"include"`, sans conséquence sur l'API publique. Si un second cas de ce type apparaît en Phase 5+, il justifiera une note dédiée dans Document 1 (actuellement clos, donc pas de modification a posteriori sans nécessité avérée).
