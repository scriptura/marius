# Rapport de fin de phase — 6.4 (Admission en arène)

## 0. Avertissement de vérification

Cette session ne dispose que des fichiers isolés fournis (`build.rs`, `lib.rs`, documents d'architecture) — ni `Cargo.toml`, ni le reste du workspace (`marius-db-forge` en source, `sqlx`, `tokio`, DB `marius`), ni toolchain Rust installée dans ce bac à sable. `cargo fmt --check`, `cargo test` et `cargo clippy --all-targets` n'ont donc **pas pu être exécutés réellement**. Le diff a été relu ligne à ligne pour cohérence de types et de guillemets, et l'indentation suit le style déjà présent dans le fichier (4 espaces, `rustfmt`-like), mais aucune exécution ne peut le confirmer depuis cet environnement. Recommandation : faire tourner les trois commandes côté CI avant merge.

## 1. Livrables

**Tests ajoutés** (module `tests_phase_6_4_arena_admission`, dans `build.rs`) :
- `admitting_child_and_parent_fixtures_yields_expected_token_counts` : écrit deux fixtures réelles sur disque (`parent.marius`, `child.marius`), les relit via `read_template_file`, les reparse via `parse_page_tokens(scan(..))`, les admet dans une `PageArena`, puis vérifie `arena.get(child_id).tokens.len() == 3` et `arena.get(parent_id).tokens.len() == 3`, ainsi que `parent_ast.extends == None` / `child_ast.extends == Some("parent.marius")`.

Conforme au jalon de la roadmap §6.4 (« test d'intégration avec fixtures réelles sur disque »), avec la réserve suivante, documentée dans le code : un `build.rs` n'est pas une cible exécutée par `cargo test` dans Cargo standard. Le test est donc écrit pour vérification manuelle / migration future vers `tests/`, pas pour une exécution automatique garantie en l'état — ce n'est pas un choix silencieux, c'est noté explicitement en commentaire au-dessus du module.

## 2. Analyse architecturale de la phase

**Invariants introduits :**
- Enfant et parent obtiennent chacun un `TemplateId` distinct via `PageArena::admit`, dans le contexte réel du build (pas seulement en test unitaire isolé comme en Phase 5.1).
- `resolve_page_template` consomme désormais réellement son paramètre `child_src` (renommé, plus de préfixe `_`) : il ré-analyse l'enfant pour obtenir le `ParsedPageTemplate` complet nécessaire à l'admission — le `child_ast` produit dans `resolve_template` (pour extraire `child_extends`) n'est pas transmis, conformément au gel de signature du Document 3 §4 et à la Nota Bene de cadrage (double parse accepté à ce stade).

**Invariants existants confirmés :**
- Garde single-level (Phase 6.3) : inchangée, toujours le premier point de sortie anticipée.
- `PageArena::admit`/`get` (Phase 5.1) : comportement identique dans un contexte réel (I/O disque, deux fichiers distincts) à celui déjà vérifié en mémoire — aucune surprise, `TemplateId` reste `Copy`/`Eq`, assignation strictement croissante par ordre d'admission.
- Point de convergence unique sur `Vec<FlatPageToken<'src>>` : toujours respecté — cette phase ne produit aucun `FlatPageToken`, elle s'arrête avant.

**Invariants devenus inutiles ou faux :** aucun.

**Mesures réelles obtenues :** aucune mesure de layout/capacité nouvelle (hors périmètre de cette phase — pas de `size_of`, pas de benchmark ; l'admission en arène ne touche à aucune structure `#[repr(C)]`).

**Hypothèses des documents confirmées/infirmées :**
- Confirmée : Document 3 §4, note de cadrage — le double parse de l'enfant est effectivement nécessaire et accepté, la signature de `resolve_page_template` n'a pas eu besoin d'être retouchée pour cette phase.
- Confirmée : Document 3 §6 — chaque table qui `extends` un même parent déclenche sa propre lecture/parse/arène ; aucune mutualisation introduite ici, cohérent avec le coût accepté v1.

## 3. Impact documentaire

- **Obsolètes :** aucune section de document devient fausse.
- **À corriger :** aucune — les Documents 1/2/3 restent des contrats d'architecture valides ; seul le code d'orchestration progresse en leur sein.
- **À régénérer en fin d'implémentation complète :** le Document 3 §2 (graphe des appels) reste correct en l'état — `arena.admit ×2` y figure déjà comme étape prévue. Rien à régénérer avant la clôture de la Phase 6.

## 4. Impact sur la roadmap

- Les phases 6.5/6.6 restent pertinentes et inchangées dans leur périmètre.
- Aucune fusion ni découpage supplémentaire identifié à ce stade.
- Aucun risque disparu ni nouveau risque architectural introduit — le risque déjà documenté (Document 3 §6, absence de cache inter-tables) reste stable et non traité, comme prévu.
- Aucune signature prévue ne peut être simplifiée : `resolve_page_template` reste gelée telle quelle jusqu'à 6.6.
- Aucune structure de données ne devient inutile.
- Pas d'implémentation plus élégante identifiée : le double parse reste la solution actée, pas une dette accidentelle.

## 5. Regard d'architecte

Aucune propriété non anticipée par les documents n'a été révélée par cette implémentation. Le seul point notable — la limite pratique de `cargo test` sur un `build.rs` — n'est pas une propriété du domaine Mode Page ni un invariant architectural du pipeline ECS/DOD ; c'est une contrainte d'outillage Cargo, déjà implicitement gérée par la roadmap (les jalons des Phases 6.1–6.3 s'appuient sur des critères de build, pas d'exécution `cargo test`). Elle mérite d'être consignée pour la synthèse finale de l'implémentation (probable ADR ou note d'outillage sur la stratégie de test de `build.rs`), mais ne justifie ni modification du code de cette phase ni changement de contrat.
