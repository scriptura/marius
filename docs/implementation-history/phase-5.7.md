# Rapport de fin de phase — 5.7 : `collect_static_refs`

## 1. Livrables

**Fonction ajoutée** : `pub fn collect_static_refs<'src>(tokens: &[PageSourceToken<'src>]) -> Vec<StaticPartialRef<'src>>`.

**Test ajouté** :
- `tests_phase_5_7_collect_static_refs::duplicated_static_path_yields_two_entries_not_one` — flux de 3 tokens (`Static("nav.html")`, `Runtime` intercalé, `Static("nav.html")` dupliqué) → 2 entrées retournées, pas 1. Jalon vert conforme à la roadmap §5.7 (extraction complète, aucune déduplication).

Aucun autre test ajouté (roadmap §5.7 n'en prévoit qu'un seul).

## 2. Analyse architecturale de la phase

**Invariant introduit** : extraction totale et sans omission des `PageSourceToken::Static` d'un flux, par une fonction à responsabilité unique, distincte de `collect_blocks`. Chaque occurrence produit une entrée — pas de déduplication par valeur de `original_path`.

**Invariants existants confirmés** :
- `PageSourceToken` reste `Copy` sans indirection nouvelle : la fonction ne fait que projeter (`*static_ref`), aucun `Box`/`Rc`/clone de contenu.
- Séparation stricte data/logic déjà en place dans `collect_blocks`/`link` : `collect_static_refs` est un filtre pur, zéro E/S, cohérent avec le principe déjà appliqué (Document 2 §4).
- Le principe de lowering irréversible (Document 2 §1) n'est pas affecté : cette fonction opère en amont du Lowering, sur un flux qui contient encore `Block`/`Static`.

**Invariants devenus inutiles ou faux** : aucun. La phase n'a touché aucune signature ni comportement existant.

**Mesures réelles obtenues** :
- `size_of::<StaticPartialRef>() == 16` octets (mesuré par `std::mem::size_of`, 64-bit) — cohérent avec la documentation du type (`&'src str` = pointeur + longueur, aucun champ additionnel).
- Complexité : une seule boucle `for` sur `tokens`, aucune structure de recherche annexe — `O(n)` vérifiable par lecture, conforme au jalon vert de la roadmap.
- `cargo test` : 62/62 tests verts (61 préexistants + 1 nouveau).
- `cargo clippy --all-targets` : aucun nouveau warning introduit par cette phase (1 warning préexistant, hors périmètre, sur `generate_aot_snippet::'src`, phase 2.2 — non touché ici).

**Hypothèses des documents confirmées ou infirmées** :
- Confirmée : Document 2 §6.2 anticipait exactement ce comportement dégradé (« chaque occurrence traitée indépendamment, comme `{% include %}` ») — le test le vérifie littéralement.
- Confirmée : la roadmap anticipait une fonction « séparée » de `collect_blocks` — aucune tentative de fusion n'aurait apporté de bénéfice de localité de cache, la doc de tête en donne la justification explicite (deux boucles indépendantes, pas d'état partagé, contrairement au cas `collect_blocks` lui-même qui fusionne construction de plage et validation de forme parce qu'elles partagent la même pile).

## 3. Impact documentaire

- **Aucune documentation devenue obsolète.** Document 2 §4 (Linker) mentionne déjà `static_refs: &[StaticPartialRef<'src>]` comme paramètre d'entrée de `link` sans préciser sa provenance — `collect_static_refs` comble ce trou sans contredire le contrat existant.
- **À corriger à terme (mineur, non bloquant)** : le Document 2 ne nomme pas explicitement `collect_static_refs` dans son plan de sous-contrats (§0 : Arène, Collecte de blocs, Linker, Lowering — quatre sous-contrats). Cette fonction est un cinquième point d'extraction, orthogonal aux quatre déjà nommés. À la régénération finale du Document 2, il faudrait soit l'intégrer comme annexe du sous-contrat Linker (elle n'existe que pour l'alimenter), soit la nommer explicitement dans le tableau du §1.
- **À régénérer en fin d'implémentation complète** : le tableau du §1 (Document 2) et le schéma d'orchestration du Document 3, une fois l'appelant réel (Phase 6, `build.rs`) écrit — impossible de documenter le câblage réel (quel flux, enfant/parent/les deux, est passé à `collect_static_refs` puis à `link`) avant l'orchestrateur.

## 4. Impact sur la roadmap

- **Phases suivantes toujours pertinentes** : 5.8 (`lower` sans substitution) et 5.9 (`lower` avec substitution) ne dépendent pas de `collect_static_refs` pour leur logique interne — celle-ci ne concerne que l'alimentation de `link`, déjà clos. Aucune fusion ni découpage supplémentaire identifié.
- **Aucun risque disparu ou nouveau** : la phase est isolée et sans effet de bord sur le reste du pipeline.
- **Signature/structures inchangées** : rien à simplifier — `collect_static_refs` n'introduit aucun nouveau type, seulement une fonction consommant/produisant des types déjà gelés (`PageSourceToken`, `StaticPartialRef`).
- **Implémentation plus élégante que celle décrite ?** Non — la roadmap §5.7 décrivait exactement une fonction filtre à une boucle ; l'implémentation correspond au contrat sans écart.

## 5. Regard d'architecte

Aucune propriété nouvelle et non anticipée n'a été révélée par cette phase. Le seul point notable — déjà documenté dans le code ajouté plutôt que découvert en cours de route — est que la séparation entre `collect_blocks` et `collect_static_refs` ne coûte rigoureusement rien en localité de cache, contrairement à la fusion actée pour `collect_blocks` lui-même (construction de plage + validation de forme). C'est une confirmation, pas une découverte : le principe déjà énoncé au Document 2 §0 (« une fonction, une catégorie de concept éliminée ») s'applique ici sans tension, parce qu'aucun état n'est partagé entre les deux parcours. Rien à porter dans une ADR ; l'explication reste dans le commentaire de tête de la Phase 5.7, suffisant pour la synthèse finale.

---

## Confirmations finales

- `cargo fmt` : **non entièrement vert** — un différentiel préexistant (tri des imports dans plusieurs blocs `use super::{...}` des phases 1.x à 4.x) apparaît avec la version de `rustfmt` disponible dans cet environnement (1.75, tri alphabétique strict incluant les fonctions, alors que le fichier semble avoir été formaté avec une politique de tri différente — fonctions avant types). **Aucun de ces différentiels ne touche le code ajouté par la Phase 5.7** (`collect_static_refs` et son module de test n'apparaissent dans aucun diff `cargo fmt --check`) — le code livré est fmt-compliant par construction. Le différentiel préexistant est hors périmètre de cette phase (contrainte : « ne modifier aucune fonctionnalité en dehors du périmètre de la Phase 5.7 ») et n'a donc pas été corrigé.
- `cargo test` : **VERT** — 62/62 tests passent (61 préexistants + 1 nouveau), aucune régression.
- `cargo clippy` : **VERT sur le périmètre de la phase** — aucun nouveau warning introduit ; le seul warning présent (`needless_lifetimes` sur `generate_aot_snippet`, Phase 2.2) est préexistant et hors périmètre.
- **Périmètre de la Phase 5.7 strictement respecté** : une seule fonction ajoutée (`collect_static_refs`), aucune signature existante modifiée, aucun `todo!`/`unimplemented!`, aucune préparation de comportement des phases 5.8/5.9/6 (le câblage vers `link`/l'orchestrateur n'a pas été anticipé).
