# Rapport de fin de phase — 4.2 `detect_extends`

## 1. Livrables

5 tests, module `tests_phase_4_2_detect_extends` :

| Test                                       | Entrée                                      | Attendu                                                                                                      |
| ------------------------------------------ | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `no_block_delimiter_returns_false`         | Aucun `{%`                                  | `false`                                                                                                      |
| `extends_at_head_returns_true`             | `{% extends "base.marius" %}` en position 0 | `true`                                                                                                       |
| `if_at_head_returns_false`                 | `{% if %}` en position 0                    | `false`                                                                                                      |
| `extends_after_leading_text_returns_false` | Texte HTML précédant `{% extends %}`        | `false`                                                                                                      |
| `empty_source_returns_false`               | `""`                                        | `false` (cas limite ajouté hors prescription roadmap, nécessaire pour prouver l'absence de panic sur `None`) |

---

## 2. Analyse architecturale

**Invariants introduits**

- Décidabilité de mode en O(1) amorti — borné par la position du premier délimiteur, jamais par la taille du fichier au-delà.
- Consommation paresseuse plafonnée à 2 items d'un `Iterator<RawSpan>` — aucun buffer intermédiaire, aucun `Vec` construit pour la décision.
- Fonction totale (aucun `panic`, aucun `Result`) : `None`/`Literal`/`ExprOpen` convergent tous vers `false` sans distinction — c'est un choix délibéré, pas une simplification par omission.

**Invariants existants confirmés**

- `scan()` reste sans allocation même en consommation partielle (le `Scanner` ne matérialise jamais l'intégralité du flux — confirmé par lecture, pas par mesure, puisque la structure `Mode::Literal` ne produit qu'un `RawSpan` à la fois).
- `SpanKind`/`RawSpan` sont `Copy` + `PartialEq` : le pattern matching structurel sur littéral (`slice: "extends"`) fonctionne sans coût d'indirection, confirmant que ces types restent exploitables en contexte de décision rapide, pas seulement en parsing complet.

**Invariant devenu ambigu — point d'attention**
Document 1 §3 formule l'algorithme comme « s'arrête dès que le premier `{%` est vu ». L'implémentation réalisée est plus stricte : elle s'arrête au premier `RawSpan`, quel que soit son type — un `Literal` en tête (texte, ou simple espace avant `{%`) invalide immédiatement, sans chercher plus loin le `{%` suivant. La formulation du document laisse entendre une recherche du premier `{%` où qu'il soit ; le comportement réel — et celui qu'exigent les 4 tests prescrits par la roadmap — est « le tout premier octet du fichier doit ouvrir un bloc ». Ce n'est pas une divergence de comportement (les tests passent), c'est une imprécision de formulation à corriger dans le document source.

**Mesures réelles**
Aucune pertinente pour cette phase : signature `&str -> bool`, pas de structure à figer en layout (`size_of` sans objet ici). Le seul « budget » vérifiable est le nombre d'appels à `next()`, garanti ≤ 2 par construction du code (pas de test runtime pour ça — c'est une propriété de lecture, comme le veut le jalon de la roadmap).

**Hypothèses confirmées/infirmées**
Confirmée : l'absence totale de dépendance à `PageSourceToken` (roadmap 4.2, "zéro dépendance"). Infirmée/précisée : la tolérance implicite à un espace ou saut de ligne avant `extends` n'existe pas — un fichier avec une ligne vide en tête avant `{% extends %}` est **hors Mode Page** selon cette fonction. Ce point n'était pas explicite dans le Document 1 et mérite d'être noté comme contrat verrouillé, pas comme détail d'implémentation.

---

## 3. Impact documentaire

- **À corriger maintenant** : Document 1 §3, reformulation de l'algorithme de `detect_extends` pour refléter « premier span du flux, pas premier `{%` du fichier ». Correction ciblée, faible risque de cascade — à faire avant que la Phase 4.6 (position d'`extends`) ne s'appuie sur une lecture erronée du contrat.
- **Inchangés** : Architecture pipeline (aucune divergence structurelle), roadmap (le jalon 4.2 tel qu'écrit est satisfait à la lettre).
- **À régénérer en fin d'implémentation** : toute doc de référence API globale (si elle existe en aval) — pas de action requise phase par phase.

---

## 4. Impact sur la roadmap

- **4.3–4.7 restent pertinentes**, aucune fusion ni découpage justifié par cette phase — le pattern de consommation paresseuse de `scan()` établi ici n'entre pas en collision avec le classifieur complet (4.3), qui consomme nécessairement tout le flux.
- **Risque levé** : IO accidentelle dans `detect_extends` — vérifié par lecture, zéro `std::fs`.
- **Risque nouveau, à porter en Phase 6** : un fichier avec espace/saut de ligne avant `{% extends %}` ne sera jamais routé vers `parse_page_tokens` par `resolve_template` (6.2) — il tombera silencieusement dans le chemin Mode Fragment existant, qui échouera probablement avec une erreur `PageParseError` non liée à la vraie cause (position d'extends). Ce n'est pas un bug de cette phase, mais un point à surveiller à l'intégration `build.rs` : le message d'erreur utilisateur final pourrait être trompeur.
- **Signature** : `&str -> bool` déjà minimale, rien à simplifier.
- **Structures devenues inutiles** : aucune.
- **Implémentation plus élégante que celle décrite** : oui, dans le sens où la description textuelle du document peut être remplacée par une spécification opérationnelle directe (« premier `RawSpan` = `BlockOpen`, second = `Ident("extends")` »), plus courte et non ambiguë que la prose actuelle.

---

## 5. Regard d'architecte

**Propriété non anticipée par les documents** : `detect_extends` établit un pattern générique — _peek borné sur `Iterator<RawSpan>`, sans matérialisation, comme primitive de décision_ — que ni le Document 1 ni la roadmap ne nomment explicitement comme réutilisable. Ce n'est pas seulement « une fonction de détection », c'est la preuve que le Scanner (Phase 1.2, gelé) est utilisable en mode partiel sans coût supplémentaire : toute décision future nécessitant de regarder les N premiers tokens d'un fichier (pas seulement `extends`) peut réutiliser exactement ce schéma, à coût nul, sans dupliquer de logique de tokenisation.

**Portée à donner à cette observation** : ne justifie pas une ADR à ce stade — c'est une propriété d'implémentation, pas une décision de design engageante. À conserver pour la synthèse finale de l'implémentation (Document 1 clos, Phase 4.7), où elle pourra être formalisée comme principe transversal si d'autres phases (5 ou 6) l'exploitent effectivement. Si aucune autre phase ne le réutilise, elle reste une note d'implémentation, pas un invariant d'architecture à documenter formellement.

_3 juillet 2026_
