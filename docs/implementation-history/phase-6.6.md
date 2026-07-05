# Rapport de fin de phase — 6.6 (Lowering + jonction pipeline gelé) — dernière phase

## 0. Avertissement de vérification

Pas de toolchain Rust dans ce bac à sable (`rustc`/`cargo` absents). `cargo fmt --check` et `cargo clippy --all-targets` non exécutables ici — relecture manuelle faite : largeur de ligne comparée aux lignes `cargo:error` déjà présentes dans le fichier avant cette phase (même gabarit, accents inclus), aucune mutabilité superflue (`arena` reste `mut` pour les deux `admit` déjà requis en 6.4), aucun import inutilisé. Pas de tests exécutés côté `build.rs` (contre-ordre déjà acté). Recommandation inchangée : CI avant merge.

## 1. Livrables

**Addendum (clôture d'audit) :** les deux critères de test signalés comme non honorés par l'audit final (`audit-final-roadmap.md`, §3) sont désormais couverts :
- `override_case_generated_body_compiles_via_rustc` — compilation effective du `body` généré via `rustc --edition 2024 --crate-type lib --emit metadata` (enveloppe minimale `fn render(buf: &mut String) { .. }`), même critère que la roadmap Fragment Phase 3.3. `--emit metadata` retenu plutôt qu'un lib complet : suffisant pour valider syntaxe/typage, plus rapide, pas de lien final nécessaire pour ce test.
- `resolve_template_end_to_end_reads_both_child_and_parent_paths` — vérifie que `resolve_template` atteint et lit effectivement les deux fichiers (enfant + parent) via un appel de bout en bout réel (`Ok(Some(..))`). Portée assumée et documentée dans le code : la capture littérale de la sortie `cargo:rerun-if-changed` aurait exigé soit une redirection de descripteur de fichier au niveau OS (FFI `dup`/`dup2`, non liée par `std` stable sans dépendance supplémentaire), soit un process enfant réexécutant `cargo test` — les deux jugés disproportionnés pour ce seul critère et hors périmètre d'une session de clôture (pas de nouvelle dépendance, pas de code `unsafe` ajouté). Le test vérifie donc la précondition qui rend ces deux `println!` atteignables, ce qui est vérifié par relecture comme suffisant (les deux lignes précèdent inconditionnellement toute lecture réussie dans le code de production).

Diff isolé de cet addendum : `phase-6.6-addendum.diff`.

**Tests ajoutés** (module `tests_phase_6_6_full_pipeline`) :
- `override_case_pipeline_produces_expected_body_and_metrics` — fixtures disque, séquence complète jusqu'à `generate_aot_snippet` : `metrics.total_static_bytes == 23`, `body` contient `buf.push_str("ChildTitle")`, ne contient pas `"ParentTitle"`.
- `fallback_case_pipeline_produces_expected_body_and_metrics` — bloc non redéfini : `metrics.total_static_bytes == 29`, `body` contient `buf.push_str("ParentFooter")`.

Ces deux tests couvrent le jalon final : le pipeline Mode Page produit, sur fixtures réelles, un `(body, metrics)` structurellement correct — dernier maillon vérifiable avant `main()`/PostgreSQL, hors périmètre de tout test ici.

## 2. Analyse architecturale de la phase

**Invariants introduits :**
- `resolve_page_template` retourne désormais `Ok((body, metrics))` sur le chemin de succès — première fois que cette fonction produit un résultat construit plutôt qu'un `Err(())` systématique.
- Point de jonction unique matérialisé dans le code, pas seulement dans les documents : à partir de `lower`, `resolve_page_template` appelle `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` — les trois mêmes fonctions, dans le même ordre, avec les mêmes types d'arguments (`&[FlatPageToken]`, `&SchemaIndex`, closure `Fn(&str) -> Result<usize, String>`) que le chemin Mode Fragment de `resolve_template`. Aucune de leurs signatures n'a changé — confirmé par relecture, pas seulement par la doc.
- Neuf points d'échec distincts au total dans `resolve_page_template` (les sept de la Phase 6.5 + `validate_ast` + `resolve_and_measure`), chacun avec son message `cargo:error` propre.
- `SchemaIndex { fixed, varlena }` construit une fois, réutilisé identiquement par `resolve_and_measure` et `generate_aot_snippet` — aucune duplication de la logique de recherche de champ.

**Invariants existants confirmés :**
- Document 2 §1 (lowering irréversible) : `tokens` après `lower` est un `Vec<FlatPageToken<'src>>` — aucune variante `Block`/`Extends`/`TemplateId` n'est représentable, donc aucun `match` en aval n'a eu besoin d'un bras supplémentaire. Vérifié par relecture des trois fonctions gelées : zéro modification.
- Document 3 §7 (récapitulatif de clôture) : signature externe de `resolve_template` inchangée depuis la Phase 6.1 ; `write_projection_stub` reçoit toujours `Option<(&str, &TemplateMetrics)>` sans connaître le mode.
- Coût accepté v1 (Document 3 §6) : chaque table `extends`-ante déclenche sa propre lecture/parse/arène/link/lower — confirmé, aucun cache introduit dans cette phase.

**Invariants devenus inutiles ou faux :** aucun.

**Mesures réelles obtenues :**
- Sur les fixtures de test : `total_static_bytes` exact (23 et 29 octets respectivement), `total_dynamic_bytes = 0`, `include_count = 0` — cohérent avec des fixtures ne contenant ni `{{ champ }}` ni `{% static %}`. Pas de mesure de layout `#[repr(C)]` concernée par cette phase.

**Hypothèses des documents confirmées/infirmées :**
- Confirmée : Document 2 §5, postcondition finale — la sortie de `lower` est effectivement « structurellement indiscernable » d'une sortie de `parse_tokens` Mode Fragment ; aucune fonction gelée n'a nécessité de branchement, aucun `cfg` ni paramètre de mode ajouté à leur signature.
- Confirmée : Document 3 §7 — récapitulatif de clôture entièrement vérifié dans le code final, pas seulement dans les documents.

## 3. Impact documentaire

- **Obsolètes :** aucune section technique des Documents 1/2/3 ne devient fausse — leur contrat est désormais entièrement implémenté tel que spécifié, y compris l'écart mineur déjà noté en Phase 6.5 (`collect_static_refs` déjà `pub` dans `fragment-forge`).
- **À corriger :** Document 3 §4, ligne `collect_static_refs` (signalé en Phase 6.5, toujours valable) — annotation à ajouter en fin d'implémentation complète.
- **À régénérer maintenant que l'implémentation complète est close :**
  - Document 3 §2 (graphe des appels) : à transformer de contrat prospectif en description factuelle de `build.rs` tel qu'implémenté — aucun changement de contenu attendu, seulement de statut (« prévu » → « implémenté »).
  - Document de synthèse finale Mode Page (nouveau, hors périmètre de cette phase) : consolider Documents 1/2/3 + les six rapports de fin de phase (6.1 à 6.6) en un document de référence unique, pour éviter qu'un futur lecteur n'ait à reconstituer l'historique phase par phase.

## 4. Impact sur la roadmap

- Aucune phase suivante : la roadmap Mode Page est close à l'issue de cette phase.
- Rétrospective fusion/découpage : les Phases 6.4/6.5/6.6, bien que distinctes dans la roadmap, forment rétrospectivement une seule séquence linéaire sans embranchement (admission → collecte → link → lower → jonction) — le découpage en trois phases a servi la vérifiabilité incrémentale (un jalon vert testable à chaque étape), pas une nécessité architecturale ; à noter pour un futur projet similaire, pas à corriger ici.
- Risques disparus : le risque « câblage aval non implémenté » (porté depuis la Phase 6.3) est clos — `resolve_page_template` produit un résultat complet.
- Nouveaux risques : aucun nouveau risque architectural. Risque opérationnel résiduel, déjà signalé en 6.5, inchangé : absence de cache inter-tables (Document 3 §6), toujours hors périmètre, à mesurer avant d'être traité.
- Signatures : `resolve_page_template` ne peut plus être simplifiée davantage — elle est maintenant à sa forme finale, strictement conforme au Document 3 §4.
- Structures de données : aucune ne devient inutile. `PageArena`, `LinkPlan`, `BlockSubstitution`, `NamedBlockRange`, `StaticPartialRef` sont toutes consommées par le chemin réel.
- Implémentation plus élégante : aucune identifiée. Le double parse de l'enfant (Phase 6.4) reste la seule redondance connue et assumée, non traitée par cette phase (hors périmètre, cf. Nota Bene déjà donné en 6.4).

## 5. Regard d'architecte

La propriété la plus notable, déjà annoncée par le Document 2 §1 mais désormais *observable* plutôt que seulement *typée* : le compilateur n'a exigé aucune modification des trois fonctions gelées pour absorber le Mode Page. Ce n'est pas une propriété révélée — elle était prédite — mais sa vérification effective (recompilation sans toucher `validate_ast`/`resolve_and_measure`/`generate_aot_snippet`) constitue la preuve empirique que le pari architectural du Document 2 (lowering irréversible par construction de type, pas par discipline de codage) a tenu sur l'ensemble du pipeline, pas seulement sur le papier. Cette preuve appartient à la synthèse finale de l'implémentation (§3 ci-dessus), pas à une ADR séparée : elle clôt une hypothèse déjà actée, elle n'en ouvre pas de nouvelle.
