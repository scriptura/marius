# ROADMAP D'IMPLÉMENTATION — MODE PAGE

**Base :** Document 1 (Parser), Document 2 (Linker & Lowering), Document 3 (Orchestration).
**Règle de découpage :** une phase = un invariant majeur, un diff mono-responsabilité, un jalon vérifiable objectivement (test ou assertion de type/layout), zéro régression sur l'existant gelé.
**Numérotation :** poursuite de la roadmap existante (PHASE 1–3 = pipeline schéma, déjà implémenté). PHASE 4–6 ci-dessous.

---

## PHASE 4 — PARSER MODE PAGE

### 4.1 — Type `PageSourceToken<'src>`

**Invariant introduit :** l'alphabet Mode Page existe comme type fermé, disjoint de `FlatPageToken`.
**Entrée → Sortie :** aucune fonction — définition de type seule.
**Périmètre :** nouveau fichier ou section isolée. `FlatPageToken` : diff nul (garanti par revue, pas seulement par convention).
**Tests ajoutés :** construction des 4 variantes ; `match` exhaustif sans `_` compile ; `assert_eq!(size_of::<PageSourceToken>(), N)` — layout figé, toute variation future doit être une décision explicite, pas un effet de bord.
**Jalon vert :** `cargo check` passe. Aucun test existant touché.

### 4.2 — `detect_extends`

**Invariant introduit :** le mode est décidable sans parsing complet.
**Entrée → Sortie :** `&str → bool`.
**Périmètre :** une fonction, zéro dépendance à `PageSourceToken`.
**Tests ajoutés :** fichier sans `{%` → `false` ; `{% extends %}` en tête → `true` ; `{% if %}` en tête → `false` ; `extends` après du texte → `false`.
**Jalon vert :** 4 tests verts, aucune E/S dans le corps de la fonction (vérifiable par lecture — pas de `std::fs`).

### 4.3 — Classifieur : sous-ensemble `Runtime`

**Invariant introduit :** un template Mode Page sans opérateur de composition produit un flux structurellement équivalent à `parse_tokens` (Mode Fragment).
**Entrée → Sortie :** `Iterator<RawSpan<'src>> → Result<Vec<PageSourceToken<'src>>, PageComposeParseError>`, reconnaissant uniquement `Static`/`Field`/`IfBool`/`EndIf`.
**Périmètre :** ne touche pas `parse_tokens` (gelé). N'implémente pas encore `extends`/`block`/`static`/`Unsupported` — tout fichier les contenant peut échouer ou être ignoré à ce stade (explicitement hors scope de ce diff).
**Tests ajoutés :** test de non-régression — même template sans composition passé à `parse_tokens` et au nouveau classifieur ; comparaison token à token après dépouillement de l'enveloppe `Runtime`.
**Jalon vert :** égalité stricte vérifiée sur au moins 3 fixtures (Static seul, Field seul, IfBool/EndIf).

### 4.4 — Reconnaissance `{% block %}` / `{% endblock %}`

**Invariant introduit :** les marqueurs de composition sont représentables sans être résolus ni validés (permissivité délibérée — cf. Document 1 §4).
**Entrée → Sortie :** extension du classifieur 4.3, ajout de la branche `Block`.
**Périmètre :** un seul fichier modifié, une seule branche de l'automate ajoutée.
**Tests ajoutés :** template à 1 bloc top-level → 1 `BlockOpen` + 1 `BlockEnd` ; template à blocs imbriqués → parse réussit quand même (preuve explicite de non-rejet à ce stade).
**Jalon vert :** les deux tests passent ; aucune régression sur 4.3.

### 4.5 — Reconnaissance `{% static "path" %}`

**Invariant introduit :** un chemin de composition est capturé sans vérification d'existence — zéro E/S dans le Parser.
**Entrée → Sortie :** extension du classifieur, ajout de la branche `Static(StaticPartialRef)`.
**Périmètre :** une branche ajoutée, aucune dépendance à `std::fs` introduite.
**Tests ajoutés :** chemin inexistant sur disque → parse réussit (test exécuté sans fixture réelle sur disque, la chaîne de chemin est arbitraire).
**Jalon vert :** test vert sans toucher le système de fichiers — vérifiable par absence d'appel `std::fs` dans le diff.

### 4.6 — Position d'`extends` + `ExtendsNotFirst`

**Invariant introduit :** `extends`, s'il existe, occupe nécessairement la première position non-whitespace.
**Entrée → Sortie :** extension du classifieur, calcul de `ParsedPageTemplate::extends`.
**Périmètre :** logique de position uniquement, pas de résolution du chemin.
**Tests ajoutés :** extends en tête → `Some(path)` ; extends après un `Static` → `Err(ExtendsNotFirst)` ; absence d'extends (fichier parent) → `None`, succès.
**Jalon vert :** 3 tests verts, aucun autre chemin d'erreur affecté.

### 4.7 — Catch-all `Unsupported`

**Invariant introduit :** aucun mot-clé de bloc ne peut échouer silencieusement ni provoquer un rejet générique non informatif.
**Entrée → Sortie :** extension du classifieur, branche par défaut pour tout mot-clé de tête ∉ {if, endif, include, extends, block, endblock, static}.
**Périmètre :** une seule branche par-défaut ajoutée — ferme `parse_page_tokens` pour ce lot (grammaire complète atteinte).
**Tests ajoutés :** paramétré sur `for`, `join`, `where`, `filter`, `group`, mot-clé arbitraire → chacun produit `Unsupported { keyword, .. }` avec le bon `keyword`.
**Jalon vert :** 6 tests verts. `parse_page_tokens` est désormais total sur la grammaire documentée (Document 1 clos).

---

## PHASE 5 — LINKER & LOWERING

### 5.1 — `PageArena` (squelette)

**Invariant introduit :** identité de fichier stable et vérifiable par égalité de valeur.
**Entrée → Sortie :** `admit(ParsedPageTemplate) -> TemplateId`, `get(TemplateId) -> &ParsedPageTemplate`.
**Périmètre :** struct + 2 méthodes, zéro logique de blocs/liens.
**Tests ajoutés :** admit ×2 → `TemplateId` distincts ; `get` après `admit` retourne le contenu inchangé (égalité de `tokens`).
**Jalon vert :** 2 tests verts, `TemplateId` reste `Copy`/`Eq` (vérifié par usage direct dans les assertions, pas de `Clone` manuel nécessaire).

### 5.2 — `collect_blocks` : cas non imbriqué

**Invariant introduit :** appariement `BlockOpen`/`BlockEnd` correct par pile, pour une profondeur ≤ 1.
**Entrée → Sortie :** `(TemplateId, &[PageSourceToken]) -> Result<Vec<NamedBlockRange>, Vec<PageValidationError>>` — seul le chemin heureux est implémenté.
**Périmètre :** pas de détection `NestedBlock`/`ForLoopDetected`/`RelationalKeyword` à ce stade (retour `Ok` uniquement pour l'instant, ou les autres variantes restent `unreachable!` documenté temporairement — à choisir explicitement, pas laisser un `todo!` silencieux).
**Tests ajoutés :** template à 2 blocs top-level → 2 `NamedBlockRange`, indices `start`/`end` vérifiés exactement.
**Jalon vert :** 1 test vert sur indices exacts (pas seulement sur le nombre de plages).

### 5.3 — `collect_blocks` : détection `NestedBlock`

**Invariant introduit :** l'imbrication est rejetée nommément, jamais acceptée comme plage valide.
**Entrée → Sortie :** extension de 5.2, profondeur de pile > 1 → erreur.
**Périmètre :** une condition ajoutée dans la boucle existante.
**Tests ajoutés :** template à bloc imbriqué → `Err(vec![NestedBlock])` ; vérification qu'aucune `NamedBlockRange` n'est retournée en parallèle (pas de sortie mixte succès/erreur).
**Jalon vert :** test vert + non-régression sur 5.2 (2 blocs top-level toujours corrects).

### 5.4 — `collect_blocks` : `ForLoopDetected` / `RelationalKeyword`

**Invariant introduit :** mapping total et nommé entre mot-clé `Unsupported` et erreur de validation.
**Entrée → Sortie :** extension de 5.2/5.3, branche sur `PageSourceToken::Unsupported`.
**Périmètre :** une branche de `match`, aucune logique de pile touchée.
**Tests ajoutés :** paramétré `for`→`ForLoopDetected`, `join`/`where`/`filter`/`group`→`RelationalKeyword`. `collect_blocks` est désormais total sur `PageValidationError` (Document 2 §3 clos).
**Jalon vert :** 5 tests verts, fail-slow vérifié (2 erreurs simultanées dans un même fichier → `Vec` de longueur 2, pas fail-fast).

### 5.5 — `link` : appariement sans E/S

**Invariant introduit :** substitution par défaut (parent conservé si non redéfini) + détection `OrphanBlock`.
**Entrée → Sortie :** `(&[NamedBlockRange], &[NamedBlockRange]) -> Result<LinkPlan, Vec<PageLinkError>>` — paramètres `static_refs`/`file_exists` non branchés (appelés avec entrée vide ou absents de la signature à ce stade, à choisir : préférer signature complète avec `static_refs: &[]` en test plutôt que signature incomplète, pour ne pas re-signer la fonction en 5.6).
**Périmètre :** logique de correspondance par nom uniquement.
**Tests ajoutés :** override simple ; pas d'override (fallback parent) ; bloc enfant orphelin → `OrphanBlock`.
**Jalon vert :** 3 tests verts, `LinkPlan.substitutions.len() == parent_blocks.len()` toujours vérifié (invariant de complétude : une entrée de plan par bloc parent, jamais moins).

### 5.6 — `link` : vérification `static`

**Invariant introduit :** existence de fichier vérifiée via E/S injectée, testable sans FS réel.
**Entrée → Sortie :** branchement effectif de `static_refs`/`file_exists` dans 5.5.
**Périmètre :** une boucle ajoutée, aucune modification de la logique de blocs.
**Tests ajoutés :** `file_exists` mock retournant `false` → `StaticFileNotFound` ; mock retournant `true` → pas d'erreur. `link` clos (Document 2 §4 terminé).
**Jalon vert :** fail-slow vérifié : un bloc orphelin ET un static manquant simultanément → `Vec` de 2 erreurs.

### 5.7 — `collect_static_refs`

**Invariant introduit :** extraction complète, sans omission, des références `static` d'un flux — fonction séparée, une seule responsabilité.
**Entrée → Sortie :** `&[PageSourceToken] -> Vec<StaticPartialRef>`.
**Périmètre :** filtre pur, aucune déduplication (explicitement hors scope — cf. Document 2 §6.2).
**Tests ajoutés :** flux à 2 `Static` dont 1 chemin dupliqué → 2 entrées retournées (pas 1).
**Jalon vert :** 1 test vert, complexité `O(n)` vérifiable par lecture (une seule boucle).

### 5.8 — `lower` : projection sans substitution

**Invariant introduit :** la projection `Runtime`/`Static` vers `FlatPageToken` est correcte indépendamment de toute logique de composition.
**Entrée → Sortie :** `(&[PageSourceToken], &LinkPlan, &PageArena) -> Vec<FlatPageToken>` — testée uniquement sur un `LinkPlan` vide (aucun bloc).
**Périmètre :** cas `Block`/substitution non exercé à ce stade (couvert en 5.9), mais la signature finale est posée dès maintenant pour éviter une re-signature.
**Tests ajoutés :** template sans blocs, 1 `Static` → sortie contient exactement 1 `FlatPageToken::StaticInclude { len: 0, .. }` ; les `Runtime` traversent inchangés (égalité valeur à valeur).
**Jalon vert :** test vert, `len == 0` vérifié explicitement (pas encore résolu — résolution différée au Resolver, Document 2 §5).

### 5.9 — `lower` : substitution effective

**Invariant introduit :** le contenu émis dépend exclusivement du `LinkPlan`, jamais implicitement du fichier physiquement parcouru — clôture du domaine composition.
**Entrée → Sortie :** extension de 5.8, splice des plages substituées via `PageArena`.
**Périmètre :** la boucle principale de `lower`, complète.
**Tests ajoutés :** test end-to-end en mémoire (2 `ParsedPageTemplate` construits à la main, un bloc redéfini, un bloc non redéfini) → séquence `FlatPageToken` exacte, vérifiée élément par élément. Assertion de type : la fonction retourne `Vec<FlatPageToken>` — l'absence de `Block`/`StaticPartialRef`/`Unsupported` dans la sortie est garantie par le système de types, pas par un test de valeur (jalon de compilation, pas seulement d'exécution).
**Jalon vert :** test vert + `cargo check` confirmant qu'aucun `match` exhaustif sur `FlatPageToken` ailleurs dans le crate n'a besoin d'un nouveau bras — preuve que `validate_ast`/`resolve_and_measure`/`generate_aot_snippet` restent inchangés (Document 2 clos).

---

## PHASE 6 — ORCHESTRATION (`build.rs`)

### 6.1 — `read_template_file` (extraction pure)

**Invariant introduit :** lecture de fichier isolée, réutilisable pour enfant et parent.
**Entrée → Sortie :** factorisation du code de lecture déjà présent dans `resolve_template`, sans changement de comportement.
**Périmètre :** refactor pur, zéro nouvelle logique.
**Tests ajoutés :** aucun nouveau test unitaire nécessaire — le jalon est un test de non-régression au niveau build.
**Jalon vert :** `cargo build` du crate `schema` produit un `generated_schema.rs` **identique octet à octet** avant/après ce commit (diff de sortie nul — vérifiable par `diff` sur le fichier généré, capturé avant le refactor).

### 6.2 — Branchement `detect_extends` (sans câblage aval)

**Invariant introduit :** le point de décision de mode existe et est unique dans tout le fichier.
**Entrée → Sortie :** `resolve_template` appelle `detect_extends(&src)` ; branche `true` retourne une erreur contrôlée explicite (`cargo:error` + `exit(1)`, message « Mode Page non câblé »).
**Périmètre :** une condition ajoutée, aucun appel à `parse_page_tokens`/`link`/`lower`.
**Tests ajoutés :** fixture `.marius` commençant par `{% extends %}` → build échoue avec le message attendu (test de build, pas seulement unitaire) ; toutes les fixtures existantes (sans extends) → build toujours vert, chemin Fragment inchangé.
**Jalon vert :** build complet du projet reste vert sur toutes les tables existantes ; une seule fixture négative ajoutée échoue avec le message exact attendu.

### 6.3 — `resolve_page_template` : lecture parent + garde single-level

**Invariant introduit :** l'E/S du parent est isolée et testée indépendamment du reste du pipeline Mode Page.
**Entrée → Sortie :** nouvelle fonction, lit le fichier parent via `read_template_file` (6.1), vérifie `parent.extends == None` après un appel minimal à `parse_page_tokens` (Phase 4 déjà close) — retourne une erreur contrôlée après la garde, sans appeler `collect_blocks`/`link`/`lower`.
**Périmètre :** une fonction, une garde, un point de retour anticipé explicite.
**Tests ajoutés :** parent déclarant lui-même `extends` → erreur nommée distincte de 6.2 ; chemin parent inexistant → erreur nommée distincte (E/S échouée, message différent d'un extends multi-niveaux).
**Jalon vert :** 2 tests de build (fixtures dédiées) verts sur leurs messages d'erreur respectifs ; chemin Mode Fragment et branche 6.2 toujours inchangés.

### 6.4 — Admission en arène (Documents 1+2 §2)

**Invariant introduit :** enfant et parent obtiennent un `TemplateId` distinct et vérifiable dans le contexte réel du build (pas seulement en test unitaire isolé).
**Entrée → Sortie :** extension de 6.3 — `arena.admit` ×2 après la garde, retour anticipé après admission.
**Périmètre :** 4 lignes ajoutées après la garde de 6.3, aucune logique de blocs.
**Tests ajoutés :** test d'intégration avec fixtures réelles sur disque (répertoire de test dédié) — vérifie `arena.get(child_id).tokens.len()` et `arena.get(parent_id).tokens.len()` cohérents avec le contenu attendu des fixtures.
**Jalon vert :** 1 test d'intégration vert, build toujours vert sur les branches 6.2/Fragment.

### 6.5 — Câblage `collect_blocks` + `collect_static_refs` + `link`

**Invariant introduit :** le `LinkPlan` est calculé correctement dans le contexte réel du build (fixtures sur disque, pas seulement construction en mémoire comme en 5.5/5.6).
**Entrée → Sortie :** extension de 6.4 — appel des trois fonctions, retour anticipé après obtention du `LinkPlan` (erreur contrôlée si le plan échoue, succès non encore transmis à `lower`).
**Périmètre :** câblage séquentiel, aucune nouvelle logique métier (toute la logique vient de la Phase 5, déjà testée unitairement).
**Tests ajoutés :** fixtures « override » et « fallback parent » sur disque → `LinkPlan` vérifié par introspection (test d'intégration, pas de test de sortie Rust générée à ce stade).
**Jalon vert :** 1 test d'intégration vert par cas (2 minimum).

### 6.6 — Câblage `lower` + jonction avec le pipeline gelé

**Invariant introduit :** le point de jonction unique est atteint — aucune fonction gelée modifiée, chemin Mode Page complet de bout en bout.
**Entrée → Sortie :** extension de 6.5 — `lower` puis `validate_ast`/`resolve_and_measure`/`generate_aot_snippet`, identiques à l'appel existant du chemin Fragment.
**Périmètre :** dernière extension de `resolve_page_template` ; `resolve_template` (point d'entrée) ne change plus après ce commit.
**Tests ajoutés :** test end-to-end avec un vrai couple `(base.marius, table.marius)` → `String` Rust généré, compilé via `rustc --edition 2024 --crate-type lib` (même critère que Phase 3.3 de la roadmap Fragment) ; capture de `cargo:rerun-if-changed` confirmant les deux chemins (enfant + parent) sont émis.
**Jalon vert :** build complet du projet vert, y compris au moins une table réelle migrée en Mode Page ; **diff nul vérifié par revue** sur `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` dans ce commit — la preuve de non-modification est une condition de fusion, pas une simple affirmation.

---

## RÉCAPITULATIF DES DÉPENDANCES

```
4.1 → 4.2 → 4.3 → 4.4 → 4.5 → 4.6 → 4.7
                                      │
                                      ▼
5.1 → 5.2 → 5.3 → 5.4                │
              │                      │
              ▼                      ▼
             5.5 → 5.6 → 5.7 → 5.8 → 5.9
                                      │
                                      ▼
6.1 → 6.2 → 6.3 → 6.4 → 6.5 → 6.6
```

`4.7` clôt le Document 1 — aucune phase de la Phase 5 ne peut commencer avant (dépendance de type : `collect_blocks`/`link`/`lower` consomment `PageSourceToken`). `5.9` clôt le Document 2 — condition d'entrée de `6.4`. `6.1`/`6.2` sont indépendantes de la Phase 5 (refactor pur + branchement sans câblage aval) et peuvent être menées en parallèle des Phases 4/5 si nécessaire — seule `6.3` dépend de `4.7` (appel à `parse_page_tokens` pour la garde single-level), et seule `6.4` dépend de `5.1`.

**22 phases.** Chacune : un seul invariant, un diff mono-fichier ou mono-fonction, un jalon vérifiable sans ambiguïté (test, assertion de layout, ou diff de sortie nul). Aucune phase ne modifie simultanément la Phase 4 et la Phase 5, ni n'anticipe le câblage de la Phase 6 avant la clôture de sa dépendance.
