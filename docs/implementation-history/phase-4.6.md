# Rapport de fin de phase — Phase 4.6

## 1. Livrables

**Tests ajoutés (3, roadmap §4.6) :**

- `tests_phase_4_6_extends_position::extends_at_head_is_captured_and_absent_from_tokens`
- `tests_phase_4_6_extends_position::extends_after_a_static_token_fails_with_extends_not_first`
- `tests_phase_4_6_extends_position::absent_extends_on_parent_file_succeeds_with_none`

**Test corrigé (adaptation de signature, pas extension de portée) :** `tests_phase_4_3::composition_keyword_out_of_scope_fails_explicitly` reposait sur `extends` comme exemple de mot-clé hors grammaire ; devenu faux depuis 4.6. Fixture remplacée par `for`, invariant testé inchangé (catch-all `InvalidBlockSequence` toujours actif jusqu'en 4.7).

**Tests ajustés (accès `.tokens`, aucune assertion de fond modifiée) :** les 5 tests de 4.3/4.4/4.5 qui comparaient directement `parse_page_tokens(...)` à un `Vec<PageSourceToken>`.

## 2. Analyse architecturale de la phase

**Invariants introduits**

- `extends`, s'il existe, occupe nécessairement la position 0 du flux de spans — jamais ailleurs, jamais en double (une seconde occurrence n'est jamais en tête, donc automatiquement rejetée par le même mécanisme).
- `ParsedPageTemplate::tokens` ne contient **jamais** de déclaration `extends` — invariant garanti par construction de type (`PageBlockOutcome::Extends` ne transite jamais par `PageSourceToken`), pas par convention runtime vérifiée après coup.
- Zéro E/S sur `extends` : capture brute du chemin, symétrique à `static` (Phase 4.5).

**Invariants existants confirmés**

- Permissivité délibérée sur l'imbrication (Document 1 §4/§6) : `parse_page_block` ne maintient toujours aucune pile — non affecté par cette phase.
- Fail-fast (pas fail-slow) au niveau du Parser (Document 1 §7) : `ExtendsNotFirst` interrompt immédiatement, cohérent avec `ExtendsNotFirst` déjà présent dans `PageComposeParseError` depuis la Phase 3.0.
- `Copy` de `PageSourceToken` non affecté : `PageBlockOutcome` et `ParsedPageTemplate` sont des types internes/de sortie, pas des variantes de l'enum figé.

**Invariants devenus obsolètes**

- Aucun. `PageComposeParseError::InvalidBlockSequence` reste utile (mots-clés reconnus mais mal formés) — son commentaire a été corrigé pour ne plus citer `extends` parmi les cas qu'il couvre encore.

**Mesures réelles**

- `size_of::<ParsedPageTemplate<'_>>() = 40 octets` (build-time uniquement : `Option<&str>` 16 octets + `Vec<PageSourceToken>` 24 octets, cible 64 bits) — mesuré, pas estimé.
- `size_of::<PageSourceToken<'_>>()` inchangé (verrouillé par le test de la Phase 4.1, non touché ici).

**Hypothèses des documents confirmées/infirmées**

- Confirmée : Document 1 §2.2 anticipait exactement cette forme (`extends: Option<&'src str>`, `tokens` distinct) — aucun écart entre le contrat et l'implémentation.
- Confirmée : la frontière « `parse_page_block` juge la forme, `parse_page_tokens` juge la position » n'était pas explicitement actée dans Document 1 mais découle directement de son architecture (seul le dispatch de tête connaît l'ordre du flux) — voir §5 ci-dessous.

## 3. Impact documentaire

- **Obsolète** : rien — Document 1 §3, §6, §7 restent exacts tels quels.
- **À corriger** : aucune correction externe requise ; les commentaires internes obsolètes (mentions de "`extends` migrera en 4.6") ont été corrigés dans ce même diff.
- **À régénérer en fin d'implémentation complète** : le tableau §4 de Document 1 (domaines d'erreur) pourra être mis à jour une fois `Unsupported` clos (4.7), pas avant.

## 4. Impact sur la roadmap

- Phase 4.7 reste pertinente et inchangée : le pattern établi ici (nouvelle branche dans `parse_page_block`, aucune nouvelle branche `SpanKind` dans `parse_page_tokens`) s'applique directement au catch-all.
- Aucune fusion, aucun découpage nécessaire.
- Risque disparu : le risque de divergence entre deux jugements de position (si la logique avait été dupliquée dans `parse_page_block`) est structurellement éliminé par `PageBlockOutcome`.
- Aucun nouveau risque identifié.
- Aucune signature prévue simplifiable, aucune structure devenue inutile.
- Pas d'implémentation plus élégante identifiée que celle décrite — le mécanisme `PageBlockOutcome` est une extension minimale, pas une réécriture.

## 5. Regard d'architecte

**Propriété révélée, non anticipée explicitement par les documents :** la frontière entre « jugement de forme » et « jugement de position » impose une règle générale de conception pour toute future extension du classifieur — _aucune fonction interne à `parse_page_block` ne doit jamais recevoir ou déduire un état de position dans le flux global_. Cette règle n'était qu'implicite dans Document 1 (qui décrit `parse_page_block` comme pur sur la forme) ; la Phase 4.6 la rend explicite et vérifiable : `PageBlockOutcome` est le seul canal par lequel une information « globale au fichier » peut remonter sans franchir la frontière forme/position.

**Portage recommandé :** conservée pour la synthèse finale de l'implémentation (pas de code, pas d'ADR, pas de mise à jour de spécification dans l'immédiat) — c'est un principe de conception locale à ce module, déjà rendu opérant par le type `PageBlockOutcome`, sans conséquence sur l'API publique ni sur les documents d'architecture existants. S'il se reproduit à l'identique en Phase 4.7 ou au-delà (par exemple si un futur mot-clé de composition a lui aussi une contrainte de position), il justifiera alors une note dédiée dans Document 1.
