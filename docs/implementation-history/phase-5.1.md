# Rapport de fin de phase — Phase 5.1 (`PageArena`, squelette)

## 1. Livrables

**Code ajouté** : `struct PageArena<'src>` (champ privé `templates: Vec<ParsedPageTemplate<'src>>`) + `impl` avec `admit(&mut self, ParsedPageTemplate<'src>) -> TemplateId` et `get(&self, TemplateId) -> &ParsedPageTemplate<'src>`. `#[derive(Debug, Default)]` sur la struct (pas de `new()` manuel — `Default` suffit et ne constitue pas une méthode de logique métier).

**Tests ajoutés** (module `tests_phase_5_1_page_arena`, 2 tests, conformes exactement au jalon de la roadmap §5.1) :

- `admit_twice_yields_distinct_template_ids` — deux `admit` successifs produisent deux `TemplateId` distincts (`assert_ne!`).
- `get_after_admit_returns_unchanged_content` — `get` après `admit` retourne une valeur strictement égale (`PartialEq`) au `ParsedPageTemplate` inséré.

Diff : **114 lignes ajoutées, 0 supprimée, 0 fichier touché en dehors de `lib.rs`** (voir `phase_5_1.diff`).

## 2. Analyse architecturale de la phase

**Invariants introduits :**

- Identité de fichier stable et vérifiable par égalité de valeur : un `TemplateId` obtenu par `admit` désigne un unique `ParsedPageTemplate` pour toute la durée de vie de l'arène (pas de retrait, pas de réassignation).
- Assignation par index de poussée : le `TemplateId` retourné est strictement croissant à chaque appel — propriété dont dépendra implicitement le futur Linker (5.5+) pour distinguer enfant/parent sans champ de rôle explicite, mais qui n'est pas exploitée ici.

**Invariants existants confirmés :**

- `TemplateId(pub u32)` reste `Copy`/`Eq` sans modification — l'arène l'utilise tel quel, aucune extension de layout n'a été nécessaire.
- `ParsedPageTemplate<'src>` reste inchangé (`Debug, Clone, PartialEq, Eq`) — l'arène en prend possession sans le copier ni le muter, confirmant la note de sa doc (« Document 2, Phase 5.1 » est désormais concrétisée exactement comme annoncée).

**Invariants devenus inutiles ou faux :** aucun. Cette phase n'entre en collision avec aucune décision antérieure.

**Mesures réelles obtenues :**

- `size_of::<TemplateId>() == 4` (inchangé, `u32` nu).
- `size_of::<PageArena<'static>>() == 24` (un seul champ `Vec<T>` : pointeur + capacité + longueur, 3×8 octets sur cible 64 bits — aucune surcharge de layout introduite par le wrapper).

**Hypothèses des documents confirmées/infirmées :**

- Document 2 §2 confirmé à l'identique : la signature `admit`/`get` implémentée est un copier-coller conforme du contrat documenté, sans écart.
- Confirmé également : « `admit` ne fait aucune E/S » et « aucune donnée n'est copiée » — vérifiable par lecture directe du corps des deux fonctions (une ligne chacune, sans allocation autre que la croissance du `Vec` interne).

## 3. Impact documentaire

- **Aucune documentation ne devient obsolète.** La doc de tête de `TemplateId` (« Assignation : responsabilité du Linker, session ultérieure ») reste correcte : l'assignation vit bien dans `PageArena::admit`, conforme à l'anticipation déjà écrite.
- **Rien à corriger dans l'immédiat** — le contrat du Document 2 §2 est implémenté sans écart, aucune clarification requise.
- **À régénérer en fin d'implémentation complète (Document 2/3)** : la doc de tête de `ParsedPageTemplate` référence « Document 2, Phase 5.1 » comme futur — cette mention pourra être mise au passé une fois l'ensemble du pipeline de composition clos (5.2 à 5.9), pas isolément maintenant.

## 4. Impact sur la roadmap

- **5.2–5.9 restent pertinentes telles que découpées** — rien dans cette implémentation ne suggère de fusion ou de re-découpage. `PageArena` est un squelette minimal, sans logique susceptible d'anticiper ou de complexifier 5.2 (`collect_blocks`).
- **Aucun risque disparu, aucun risque nouveau.** Le point ouvert du Document 2 §6.1 (héritage multi-niveaux) reste entier — `PageArena` l'autorise structurellement (`Vec` de croissance illimitée) sans le garder ni le rejeter ; la décision reste différée comme prévu.
- **Signature inchangée par rapport au contrat documenté** — aucune simplification identifiée ; la signature `admit`/`get` était déjà minimale.
- **Aucune structure de données devenue inutile.**
- **Pas d'implémentation plus élégante identifiée** que celle décrite dans le Document 2 — le squelette est déjà à la limite basse de complexité possible (un `Vec`, deux fonctions à une ligne).

## 5. Regard d'architecte

Aucune propriété non anticipée n'a été révélée par cette implémentation. Le contrat documenté (Document 2 §2) et le contrat gelé de `TemplateId` (Phase 3.0) étaient déjà en accord exact avant l'écriture du code — la phase 5.1 est une confirmation par construction plutôt qu'une découverte architecturale. Rien à porter vers une ADR ou une révision de spécification à ce stade.
