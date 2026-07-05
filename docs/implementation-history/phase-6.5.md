# Rapport de fin de phase — 6.5 (Câblage collect_blocks + collect_static_refs + link)

## 0. Avertissement de vérification

Contre-ordre reçu : l'exigence « cargo test vert » ne s'appliquait qu'à `lib.rs` (fragment-forge). Pour `build.rs`, aucun test n'est exécuté — cohérent avec la contrainte déjà documentée en tête des modules `tests_phase_6_4_*`/`tests_phase_6_5_*` : un build script n'est pas une cible `cargo test` standard. Ces modules restent dans le fichier à titre documentaire et de vérification manuelle, non comme jalon CI actif.

Par ailleurs, cet environnement ne dispose toujours pas de la toolchain Rust (`rustc`/`cargo` absents), ni du reste du workspace au-delà du `Cargo.toml` fourni : `cargo fmt --check` et `cargo clippy --all-targets` n'ont donc pas pu être exécutés réellement ici non plus. Relecture manuelle effectuée contre `Cargo.toml` (dépendances `marius-db-forge`, `marius-fragment-forge`, `sqlx`, `tokio` en `[build-dependencies]` — cohérentes avec les imports de `build.rs`) et contre le style déjà en place (largeur ~100 col, 4 espaces). Recommandation inchangée : vérification CI avant merge.

## 1. Livrables

**Tests ajoutés** (module `tests_phase_6_5_collect_and_link`, dans `build.rs`) :
- `override_case_plan_retains_child_block` — fixtures disque parent/enfant, l'enfant redéfinit `title` : `LinkPlan.substitutions[0].source.template == child_id`.
- `fallback_case_plan_retains_parent_block_when_not_overridden` — l'enfant n'a aucun bloc : `LinkPlan.substitutions[0].source.template == parent_id`.

2 tests, conforme au minimum de la roadmap §6.5 (« 1 test d'intégration vert par cas, 2 minimum »). Même réserve qu'en Phase 6.4 sur l'exécution automatique par `cargo test` (documentée en tête de module).

## 2. Analyse architecturale de la phase

**Invariants introduits :**
- `resolve_page_template` calcule désormais un `LinkPlan` réel à partir de deux fichiers sur disque : `collect_blocks` est appliqué séparément à l'enfant et au parent (erreurs distinctes, jamais fusionnées dans un message commun), `collect_static_refs` est appliqué aux deux flux et concaténé sans déduplication (Document 2 §6.2, toujours hors périmètre), et `link` reçoit un `file_exists` réel — `Path::new(&relative_path_for_include_str(manifest_dir, path)).exists()` — première closure d'E/S injectée dans le Linker qui touche effectivement le disque.
- Sept points d'échec distincts, chacun avec son propre message `cargo:error` (dénombrés dans la doc de fonction et revérifiés contre le code) : lecture parent, parse parent, garde single-level, re-parse enfant, blocs enfant, blocs parent, linking.

**Invariants existants confirmés :**
- `collect_blocks`/`link` (Document 2 §3/§4, Phase 5.2-5.6) : comportement identique en contexte disque réel à celui déjà vérifié en mémoire (Phase 5.5/5.6/5.9) — override et fallback produisent exactement les mêmes règles de substitution.
- `PageArena` (Phase 6.4) : réutilisée sans modification, `arena.get(id).tokens` suffit à alimenter `collect_blocks`/`collect_static_refs` sans nouvel accesseur.
- Point de convergence unique sur `Vec<FlatPageToken<'src>>` : toujours respecté — cette phase produit un `LinkPlan`, pas un `Vec<FlatPageToken>` ; `lower` n'est pas appelé.

**Invariants devenus inutiles ou faux :** aucun.

**Mesures réelles obtenues :** aucune (pas de `size_of`/benchmark concerné par cette phase — `LinkPlan` ne touche à aucun layout `#[repr(C)]`).

**Hypothèses des documents confirmées/infirmées :**
- Confirmée : Document 3 §4 — la signature de `collect_static_refs` correspond exactement à celle déjà `pub` dans `fragment-forge` (Phase 5.7). Écart de forme relevé : le Document 3 la décrit comme utilitaire *local à build.rs, non pub* ; en pratique elle existe déjà comme fonction `pub` de `fragment-forge` (le Document 2/roadmap §5.7 l'ayant close avant l'écriture de cette phase). Décision prise : réutiliser la fonction existante plutôt que la dupliquer dans `build.rs` — aucune règle de la mission n'impose de dupliquer une fonction déjà `pub` et testée, et une duplication locale aurait introduit deux implémentations divergentes du même contrat. Ceci n'est pas une modification de signature publique de `fragment-forge` (aucun changement dans `lib.rs`), seulement un choix de câblage côté `build.rs`.
- Confirmée : Document 2 §4 — le comportement par défaut (fallback parent silencieux, pas d'erreur) est bien celui observé sur fixtures réelles, pas seulement en mémoire.

## 3. Impact documentaire

- **Obsolètes :** aucune section ne devient fausse.
- **À corriger :** Document 3 §4, tableau des signatures attendues — la ligne `collect_static_refs` gagnerait à être annotée « déjà `pub` dans fragment-forge (Phase 5.7), réutilisée telle quelle » plutôt que listée comme nouvelle fonction `build.rs` non-`pub`, pour éviter qu'un futur lecteur ne tente de la réimplémenter. Correction mineure, non bloquante, à faire en fin d'implémentation complète.
- **À régénérer en fin d'implémentation complète :** le Document 3 §2 (graphe des appels) reste correct en l'état — `collect_static_refs`, `link` y figurent déjà comme étapes prévues avant `lower`.

## 4. Impact sur la roadmap

- La Phase 6.6 reste pertinente et inchangée dans son périmètre (câblage `lower` + jonction).
- Aucune fusion ni découpage supplémentaire identifié.
- Aucun risque disparu. Nouveau risque mineur identifié, non bloquant : le double appel `collect_static_refs` (enfant + parent) sans déduplication signifie que si un même chemin `static` est référencé par les deux fichiers, il apparaîtra deux fois dans `static_refs` transmis à `link` — sans conséquence à ce stade (`link` ne fait qu'une vérification d'existence par référence, idempotente), mais à garder à l'esprit pour la Phase 6.6/résolution finale si un comptage par occurrence devait un jour s'appuyer sur cette liste brute.
- Aucune signature prévue ne peut être simplifiée : `resolve_page_template` reste gelée jusqu'à 6.6.
- Aucune structure de données ne devient inutile.
- Pas d'implémentation plus élégante identifiée.

## 5. Regard d'architecte

Le seul écart notable entre les documents et l'implémentation réelle — `collect_static_refs` déjà `pub` dans `fragment-forge` plutôt qu'utilitaire local à `build.rs` — n'est pas une propriété architecturale nouvelle, c'est un décalage temporel entre la rédaction du Document 3 et la clôture effective de la Phase 5.7 qui l'a précédée. Il ne change rien au contrat (même signature, même comportement), seulement l'endroit où la fonction vit. À porter par une note de correction documentaire (Document 3 §4), pas par une ADR ni un changement de code — consigné ci-dessus (§3), suffisant pour la synthèse finale.
