# Graphe de dépendances architecturales — Mode Page

**Nature du document :** graphe de causalité entre invariants, pas un planning. Chaque nœud est un invariant introduit par une phase (roadmap 22 phases) ou déjà garanti par le code gelé (nœuds racine, préfixe `R-`). Une arête `A → B` signifie : *l'invariant A doit être vrai pour que B soit bien défini*, pas *A est codé avant B*.

**Modèle retenu — deux natures d'arête, distinguées partout :**
- **(type)** — dépendance structurelle : la signature de B référence un type introduit par A. Détectable par `cargo check`.
- **(inv)** — dépendance sémantique : le comportement correct de B suppose une garantie établie par A, sans que le type système ne l'impose. Indétectable par le compilateur — seule cause de régression silencieuse si A change.

Les arêtes `(inv, implicite)` sont celles qui n'apparaissent dans aucune signature Rust et que seule une lecture du contrat (Documents 1/2/3) révèle. Ce sont les plus dangereuses au sens architectural : leur violation ne casse jamais la compilation.

---

## 0. Nœuds racine — invariants déjà garantis (code gelé)

| ID | Invariant garanti | Statut |
|---|---|---|
| `R-SCAN` | `scan()`/`RawSpan` — tokenisation lexicale correcte | Gelé |
| `R-FLATPAGETOKEN` | IR canonique, 5 variantes, exhaustivité fermée | Gelé, pivot |
| `R-PAGEBLOCKTOKEN` | `BlockOpen`/`BlockEnd` — type scaffoldé | Gelé (non câblé) |
| `R-STATICPARTIALREF` | Référence `{% static %}` sans `len` par construction | Gelé (non câblé) |
| `R-TEMPLATEID` | Identité d'arène, `Copy`/`Eq` | Gelé (non câblé) |
| `R-NAMEDBLOCKRANGE` | Plage nommée taguée `TemplateId`, pas de `Vec<Self>` imbriqué | Gelé (non câblé) |
| `R-PAGEVALIDATIONERROR` | Domaine d'erreur de forme (4 variantes) | Gelé (non câblé) |
| `R-PAGELINKERROR` | Domaine d'erreur de liaison (3 variantes) | Gelé (non câblé) |
| `R-PAGECOMPOSEPARSEERROR` | Domaine d'erreur de grammaire composition | Gelé, partiel (`ExtendsNotFirst` seul) |
| `R-VALIDATE-AST` | Gate sémantique (bornes if, champs) | Gelé |
| `R-RESOLVE-MEASURE` | Capacité exacte, résolution I/O | Gelé |
| `R-CODEGEN` | Émission Rust | Gelé |

Ces nœuds n'ont aucune arête entrante depuis les phases 4–6 : ce sont des invariants acquis, pas des livrables de cette roadmap.

---

## 1. Registre des phases

Format par ligne : `ID | Invariant | Dépend de | Requis par | Composants | Risque si modifié ultérieurement`.

### Phase 4 — Parser

| ID | Invariant introduit | Dépend de | Requis par | Composants | Risque si modifié |
|---|---|---|---|---|---|
| **4.1** | `PageSourceToken` existe, alphabet fermé | `R-FLATPAGETOKEN` (type), `R-PAGEBLOCKTOKEN` (type), `R-STATICPARTIALREF` (type) | 4.3 | Parser | Toute variante ajoutée après coup invalide l'exhaustivité de tout `match` déjà écrit en 5.x/6.x — coût de régression maximal, à traiter comme `FlatPageToken` lui-même (gel après stabilisation). |
| **4.2** | `detect_extends` décide le mode | — (indépendant, pure fonction sur `&str`) | 6.2 | Parser | Faux positif/négatif change silencieusement quels fichiers empruntent quel pipeline — aucun test de Phase 5/6 ne le détecterait directement (leurs fixtures sont déjà mode-connu). |
| **4.3** | Sous-ensemble `Runtime` correct (parité avec `parse_tokens`) | 4.1 (type), `R-SCAN` (type) | 4.4, 4.5, 4.6, 5.8 (inv) | Parser, Scan | Une divergence avec `parse_tokens` introduit un écart de sémantique entre Mode Fragment et Mode Page pour un même opérateur `{{ }}` — viole directement la convergence du §0 (document précédent). |
| **4.4** | `{% block %}` représentable sans validation | 4.3 (inv), `R-PAGEBLOCKTOKEN` (type) | 4.7 (type), 5.2 (type) | Parser | Si cette phase commence à rejeter l'imbrication au niveau syntaxique, la responsabilité de `NestedBlock` (5.3) devient inatteignable — duplication de logique entre deux phases sur le même invariant. |
| **4.5** | `{% static %}` capturé sans E/S | 4.3 (inv), `R-STATICPARTIALREF` (type) | 4.7 (type), 5.6 (type), 5.7 (type), 5.8 (inv) | Parser | Introduire une résolution de chemin ici romprait la frontière « zéro E/S dans le Parser » — déplace silencieusement une erreur de build-time-tardif à build-time-précoce, invalidant l'ordre de rapport d'erreurs attendu par 5.6/6.5. |
| **4.6** | Position d'`extends` contrainte | 4.3 (inv), `R-PAGECOMPOSEPARSEERROR` (type) | 4.7 (type), 5.1 (type), 6.3 (inv) | Parser | Relâcher cette contrainte (permettre `extends` ailleurs qu'en tête) change la définition même de « fichier enfant » consommée par 5.1 et par la garde single-level de 6.3 — need de revalider ces deux phases. |
| **4.7** | Grammaire totale — catch-all `Unsupported` | 4.4 (inv), 4.5 (inv), 4.6 (inv) | 5.4 (type), 6.3 (type), 6.4 (type) | Parser | Ajouter un mot-clé reconnu après coup (ex. futur `{% partial %}`) réduit silencieusement le domaine `Unsupported` — toute règle de 5.4 qui matchait ce mot-clé par nom devient une branche morte non détectée par le compilateur. |

**Note de divergence par rapport à la roadmap chronologique :** 4.2 n'a aucune dépendance sur 4.1 — c'est une branche causale totalement indépendante du reste de la Phase 4. Sa position « avant 4.3 » dans la roadmap est un choix de confort de revue, pas une nécessité architecturale.

### Phase 5 — Linker & Lowering

| ID | Invariant introduit | Dépend de | Requis par | Composants | Risque si modifié |
|---|---|---|---|---|---|
| **5.1** | `TemplateId` assigné, arène cohérente | 4.6 (type — forme de `ParsedPageTemplate`), `R-TEMPLATEID` (type) | 5.2 (type), 5.8 (type), 5.9 (inv), 6.4 (type) | Linker (Arène) | Changer la stratégie d'assignation (ex. hachage de chemin au lieu d'index séquentiel) invalide toute comparaison d'égalité déjà écrite en 5.9/6.4 — `TemplateId` est consommé par valeur, pas par référence, partout en aval. |
| **5.2** | Plages appariées, cas non imbriqué | 4.4 (type), 5.1 (type), `R-NAMEDBLOCKRANGE` (type) | 5.3 (inv), 5.4 (inv), 5.5 (type), 5.9 (inv, implicite) | Linker (collecte) | Un bug d'indexation ici (`start`/`end` décalés) est invisible à la compilation et se propage silencieusement jusqu'au splice de 5.9 — seule couche de défense : le test d'indices exacts de 5.2 lui-même. |
| **5.3** | Imbrication rejetée, jamais acceptée | 5.2 (inv) | 5.9 (inv, implicite) | Linker (validation) | Si cette garantie disparaît, l'algorithme de substitution de 5.9 — qui suppose des plages non chevauchantes — produit un résultat défini mais faux (splice partiel silencieux), sans qu'aucun type ne le signale. **C'est l'arête implicite la plus critique du graphe.** |
| **5.4** | Mapping total mot-clé → erreur nommée | 5.2 (inv), 4.7 (type) | 5.9 (inv, implicite), 6.5 (type) | Linker (validation) | Sans cette garantie, un token `Unsupported` peut atteindre 5.9, qui ne sait pas le traiter (fonction totale sans `Result` — Document 2 §5 suppose son absence par construction). Une régression ici transforme une garantie de totalité de `lower` en `panic!` ou en sortie silencieusement incorrecte. |
| **5.5** | Substitution par défaut + `OrphanBlock` | 5.2 (type — forme de `NamedBlockRange`) | 5.6 (inv), 5.8 (type), 5.9 (type), 6.5 (type) | Linker (matching) | **N'exige explicitement ni 5.3 ni 5.4** : la correspondance par nom fonctionne indépendamment de la validité de forme. Modifier la politique de fallback (ex. exiger un override obligatoire au lieu d'un défaut silencieux) est un changement de contrat côté auteurs de templates, pas seulement d'implémentation — impact hors du seul code Rust. |
| **5.6** | Existence des fichiers `static` vérifiée | 5.5 (inv), 4.5 (type) | 6.5 (type) | Linker (E/S) | Une E/S ajoutée ici double le coût déjà payé par le Resolver (5.8/§5 Document 2) pour le même fichier — accepté et documenté ; toute tentative future de « factoriser » cette vérification avec celle du Resolver romprait la séparation de phase justifiée en Document 2 §4. |
| **5.7** | Extraction complète des `StaticPartialRef` | 4.5 (type) | 6.5 (type) | Linker (utilitaire) | Fonction feuille, aucune dépendance sur 5.1–5.6 : peut être développée en parallèle strict de tout le reste de la Phase 5. Un changement ici (ex. ajout d'une déduplication) romprait silencieusement l'invariant explicite « pas de dédup à ce niveau » consommé par 5.6 (qui s'attend à recevoir toutes les occurrences, pas un ensemble dédupliqué). |
| **5.8** | Projection `Runtime`/`Static` correcte (sans blocs) | 4.3 (inv), 4.5 (inv), 5.1 (type), 5.5 (type) | 5.9 (inv) | Normalizer (lowering) | **N'exige ni 5.2 ni 5.3 ni 5.4 ni 5.6 ni 5.7** — la signature complète de `lower` est posée dès cette phase, mais son test n'exerce que la projection triviale. C'est l'exemple le plus net de dépendance de type sans dépendance d'invariant : le code compile et un sous-ensemble de comportement est prouvé correct bien avant que les autres briques ne le soient. |
| **5.9** | Substitution effective, clôture du domaine composition | 5.8 (inv), 5.5 (inv), 5.1 (inv), 5.2 (inv, implicite), 5.3 (inv, implicite), 5.4 (inv, implicite) | 6.6 (type) | Normalizer (lowering) | Nœud de plus haute centralité de la Phase 5 : toute violation d'une des trois garanties implicites (5.2/5.3/5.4) produit une IR syntaxiquement valide mais sémantiquement fausse, indétectable par le typage de `FlatPageToken` lui-même (qui ne sait pas d'où vient un token). |

### Phase 6 — Orchestration

| ID | Invariant introduit | Dépend de | Requis par | Composants | Risque si modifié |
|---|---|---|---|---|---|
| **6.1** | Lecture de fichier factorisée, comportement inchangé | — (indépendant, refactor pur) | 6.3 (type) | build.rs | Risque classique de refactor : toute divergence de comportement (encodage, gestion d'erreur E/S) casse silencieusement le chemin Fragment déjà en production — seul garde-fou : le jalon « diff nul sur `generated_schema.rs` ». |
| **6.2** | Point de décision de mode unique | 4.2 (type+inv) | 6.3 (type) | build.rs | Un second point de branchement introduit ailleurs (ex. dans `main()`) romprait l'invariant « un seul point de décision » — non détectable automatiquement, seulement par revue de code ciblée sur ce point précis. |
| **6.3** | Garde single-level, E/S parent isolée | 6.1 (type), 6.2 (inv), 4.6 (inv — via 4.7 en pratique, cf. note) | 6.4 (inv) | build.rs, Parser (appel) | **Dépendance de type réelle sur 4.7** (appelle `parse_page_tokens` tel qu'il existe, donc la grammaire complète), **dépendance d'invariant réelle sur 4.6 seulement** (seule la correction du champ `extends` est exploitée). Si un test de 6.3 échoue après une modification de Phase 4, vérifier d'abord si c'est 4.6 (régression réelle) ou une variante grammaticale non liée (faux positif de couplage). |
| **6.4** | Admission en arène dans le contexte réel du build | 6.3 (inv), 5.1 (type+inv), 4.7 (type) | 6.5 (inv) | build.rs, Arène | Risque principal : divergence entre le comportement testé en isolation (5.1, fixtures en mémoire) et le comportement réel (fichiers sur disque, encodage, chemins relatifs) — seule couverture : le test d'intégration propre à 6.4. |
| **6.5** | `LinkPlan` correct sur fixtures réelles | 6.4 (inv), 5.2+5.3+5.4 (inv), 5.7 (inv), 5.5+5.6 (inv) | 6.6 (inv) | build.rs, Linker | Nœud de plus haute fan-in du graphe (6 dépendances directes) — le plus coûteux à auditer en cas de régression signalée ici : le premier réflexe doit être d'identifier laquelle des cinq garanties amont a été violée avant de modifier ce niveau. |
| **6.6** | Point de jonction unique atteint, pipeline clos | 6.5 (inv), 5.9 (inv), `R-VALIDATE-AST` (inv), `R-RESOLVE-MEASURE` (inv), `R-CODEGEN` (inv) | — (feuille terminale) | build.rs, Resolver, Codegen | Risque inverse de toutes les phases précédentes : ce n'est pas 6.6 qui casse le pipeline gelé, c'est le pipeline gelé qui, s'il est modifié pour accommoder un besoin Mode Page mal anticipé, romprait silencieusement le chemin Mode Fragment. Le jalon « diff nul sur les trois fonctions gelées » est la seule défense. |

---

## 2. Graphe (DAG)

```
                         R-FLATPAGETOKEN ─┐
                         R-PAGEBLOCKTOKEN ─┼──▶ 4.1 ──▶ 4.3 ──┬──▶ 4.4 ──┬──▶ 4.7 ──▶ 5.4 ─────────────┐
                         R-STATICPARTIALREF┘         ▲         │         │      ▲                       │
                         R-SCAN ───────────────────────┘         ├──▶ 4.5 ┘      │                       │
                                                                  └──▶ 4.6 ───────┘                       │
                                                                        │                                 │
             4.2 (indépendant) ──────────────────────────────────▶ 6.2 │                                 │
                                                                        ▼                                 │
                                                                       5.1 ◀── R-TEMPLATEID                │
                                                                    ┌───┴────┐                             │
                                                                    ▼        ▼                             │
                                                                   5.2 ◀── R-NAMEDBLOCKRANGE                │
                                                              ┌─────┴─────┐                                │
                                                              ▼           ▼                                │
                                                             5.3         5.5 ◀── (4.5 pour static_refs)     │
                                                              │           │                                │
                                                        (inv implicite)   ├──▶ 5.6 ──────────────┐          │
                                                              │           ├──▶ 5.8 ◀── 4.3,4.5    │          │
                                                              │           │                        │          │
                                                              └───────────┴──▶ 5.9 ◀───────────────┴──5.4────┘
                                                                                │
                                    4.5 ──▶ 5.7 ─────────────────────────────┐ │
                                                                              ▼ ▼
   6.1 (indépendant) ──▶ 6.3 ◀── 6.2                                        6.5 ◀── 5.6, 5.7, 5.5
                          │                                                   │
                          ▼                                                   │
                         6.4 ◀── 5.1, 4.7                                     │
                          └───────────────────────────────────────────────────┘
                                                                                ▼
                                                                               6.6 ◀── 5.9, R-VALIDATE-AST,
                                                                                        R-RESOLVE-MEASURE, R-CODEGEN
```

Trois branches causalement indépendantes en tête de graphe : `4.1→4.3→…` (alphabet + grammaire), `4.2→6.2` (discriminant de mode), `6.1` (refactor E/S). Aucune des trois n'attend les deux autres pour être entreprise — leur ordre dans la roadmap est un choix de séquencement de revue, pas une contrainte du graphe.

---

## 3. Justification des arêtes non triviales

- **4.2 → 6.2, sans passer par 4.1/4.3–4.7** : `detect_extends` n'inspecte que la position du premier token, jamais sa structure interne. Coupler cette arête au reste de la Phase 4 créerait une dépendance de convenance, pas une nécessité — exactement ce que la mission demande d'éviter.
- **5.5 sans dépendance sur 5.3/5.4** : la correspondance nom-à-nom entre blocs parent/enfant est une opération purement combinatoire sur des chaînes de caractères (`name`). Elle ne lit ni n'a besoin de savoir si la structure d'où proviennent ces noms est elle-même valide — c'est la responsabilité de 5.9 (via 5.2/5.3/5.4) de ne jamais lui présenter une entrée invalide.
- **5.9 → 5.3 et 5.9 → 5.4, en `(inv, implicite)`** : aucune signature Rust ne référence `PageValidationError` dans `lower`. La dépendance existe uniquement parce que `lower` est une fonction totale (pas de `Result`) — un choix de Document 2 §5 justifié par « l'entrée est déjà garantie cohérente ». Cette garantie est *produite* par 5.3/5.4, jamais vérifiée par 5.9 elle-même. C'est le cas d'école de dépendance invisible au typage.
- **5.8 sans dépendance sur 5.2/5.3/5.4/5.6/5.7** : la signature de `lower` est fixée dès 5.8 (elle prend déjà `LinkPlan`/`PageArena` en paramètre) mais son test n'exerce qu'un `LinkPlan` vide. C'est une dépendance de type pure, sans dépendance d'invariant — la preuve que « poser la signature tôt » et « prouver le comportement tôt » sont deux actes découplés dans ce pipeline.
- **6.3 : dépendance de type sur 4.7, dépendance d'invariant sur 4.6 seulement** : appeler `parse_page_tokens` exige la fonction complète (4.7), mais la logique de garde (« le parent ne doit pas avoir d'extends ») n'exploite que le champ `extends`, garanti dès 4.6. Une régression future détectée par un test de 6.3 doit d'abord être triée entre ces deux causes avant correction.
- **6.5 : nœud de fan-in maximal (5 dépendances directes)** : c'est un signal architectural délibéré, pas un défaut — 6.5 est le premier point du graphe où toutes les garanties de collecte et de liaison convergent avant d'être consommées par le lowering réel. Un audit de régression sur le Mode Page doit toujours commencer par vérifier lequel des cinq invariants amont a été rompu avant de toucher au code de 6.5 lui-même.

---

## 4. Vers un modèle général — invariants, composants, tests, ADR

Ce graphe reste, pour l'instant, un document texte. Pour qu'un futur outillage de la Forge puisse répondre à des questions telles que *« quels invariants sont impactés si je modifie `collect_blocks` ? »*, il faut que la structure ci-dessus soit exprimable comme un graphe de données interrogeable, pas seulement lisible. Proposition de schéma minimal, pensé pour être alimenté incrémentalement à partir de ce document sans le réécrire :

**Entités**
- `Invariant { id, description, statut(gelé|actif|proposé) }` — un nœud de ce document, ligne à ligne.
- `Component { id, nom, fichier }` — `Scan`, `Parser`, `Linker`, `Normalizer`, `FlatPageToken`, `Resolver`, `Codegen`, `build.rs`.
- `Test { id, fichier, invariant_couvert }` — un test unitaire ou d'intégration.
- `ADR { id, titre, invariants_concernés[] }` — décision d'architecture (ex. « rejet de `BlockDecl` récursif », déjà identifiée comme dette documentaire).

**Relations**
- `Invariant --DEPENDS_ON(type|inv, implicite?)--> Invariant` — exactement les arêtes de ce document.
- `Invariant --IMPLEMENTED_IN--> Component`
- `Invariant --VALIDATED_BY--> Test`
- `Invariant --DOCUMENTED_BY--> ADR | Spec`

Avec ce schéma, les quatre questions posées en préambule deviennent des requêtes de graphe directes plutôt que des relectures manuelles :
- *Impact d'une modification de `collect_blocks`* → fermeture transitive des `DEPENDS_ON` sortants depuis les invariants `5.2/5.3/5.4` (ce document donne déjà cette fermeture : `5.5, 5.9, 6.5, 6.6`).
- *Plus petite partie du pipeline concernée par un changement de `FlatPageToken`* → fermeture transitive depuis `R-FLATPAGETOKEN` — ici, la totalité du graphe (nœud racine de plus haute centralité), ce qui est en soi une information architecturale utile : `FlatPageToken` ne peut structurellement pas avoir de rayon de changement local.
- *Tests à rejouer* → tous les `Test` reliés par `VALIDATED_BY` aux invariants de la fermeture transitive ci-dessus.
- *ADR/specs liés* → tous les `ADR`/`Spec` reliés par `DOCUMENTED_BY` aux mêmes invariants — recoupe directement la section « Dette documentaire identifiée » déjà produite pour le Mode Page.

Ce document constitue, en l'état, le premier jeu de données valide pour peupler ce graphe : chaque ligne des tableaux du §1 est déjà au format `(Invariant, [DEPENDS_ON], [Component], [Test à créer via la roadmap])`. Le travail de conversion en structure interrogeable (fichier de données séparé du texte, distinct de ce document narratif) est une décision d'outillage delegable à une session ultérieure — hors périmètre ici.
