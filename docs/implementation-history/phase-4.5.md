# Rapport de fin de phase — 4.5

## 1. Livrables

- `tests_phase_4_5_static::static_path_parses_without_touching_filesystem` — seul test prévu par la roadmap §4.5.

## 2. Analyse architecturale

**Invariants introduits**

- `{% static path %}` capturé sans E/S — `original_path` est une slice brute du scanner, jamais résolue.
- Forme lexicale de `path` : `Ident` de bloc nu, non quoté (décision actée ci-dessus, absente des documents).

**Invariants confirmés**

- `PageSourceToken` reste `Copy`, 48 octets — non affecté, `Static` préexistait dans l'enum (4.1).
- `parse_page_block` reste le site unique d'enveloppe (`PageSourceToken`, pas `FlatPageToken`) pour tout mot-clé de composition — `static` suit exactement le pattern de `block`/`endblock`.
- Politique fail-fast du Parser (Document 1 §7) inchangée : `static` ne modifie pas la politique d'erreur, un seul point d'échec par token.

**Invariants devenus faux**

- Toute doc antérieure affirmant que `static` échoue via `InvalidBlockSequence` est fausse depuis ce diff — corrigée à 4 endroits (commentaires de scope 4.3/4.4, doc `PageComposeParseError`, doc `PageSourceToken::Static`).

**Mesures réelles**

- `size_of::<PageSourceToken>() == 48` — inchangé, vérifié par test existant, pas de nouvelle mesure requise (pas de nouveau champ, pas de nouvelle variante).

**Hypothèses confirmées/infirmées**

- Confirmée : Document 1 §5 (« zéro I/O, `StaticPartialRef` porte le chemin brut ») — le test le prouve positivement, pas seulement par absence d'échec.
- Infirmée : la notation à guillemets de Document 1 §2.1/§6 ne correspond pas à la grammaire lexicale réellement scannable sans extension du scanner gelé. Document 1 devra être corrigé sur ce point (§3).

## 3. Impact documentaire

- **À corriger** : Document 1 §2.1 et §6 — remplacer `{% static "path" %}` par `{% static path %}` partout, et ajouter une note sur l'absence de support de littéraux de chaîne dans le scanner.
- **Obsolète** : aucune section entière — seuls des exemples syntaxiques ponctuels.
- **À régénérer en fin d'implémentation complète** : rien à ce stade, la divergence est mineure et déjà corrigée dans les commentaires de code (source de vérité en attendant).

## 4. Impact sur la roadmap

- Phases suivantes (4.6, 4.7, Phase 5) restent pertinentes telles quelles — aucune fusion ni découpage justifié par ce diff.
- Aucun risque disparu ni nouveau.
- Aucune signature simplifiable, aucune structure devenue inutile.
- Pas d'implémentation plus élégante identifiée pour cette phase — le pattern (branche `match` + enveloppe unique) est déjà optimal pour ce grain de diff.

## 5. Regard d'architecte

**Propriété révélée, non anticipée par les documents** : le scanner `InBlock` (Phase 1.2, gelé) impose une contrainte lexicale transverse à tout futur mot-clé de bloc portant un argument textuel libre — aucun littéral quoté n'est représentable sans modifier le scanner. Document 1 a été écrit avec une notation à guillemets purement illustrative, sans vérifier sa compatibilité avec le scanner déjà gelé en amont.

**Portée** : cette propriété concerne aussi `extends` (Phase 4.6, même scanner, même contrainte) — un chemin `extends "base.marius"` échouerait au même titre. Je recommande une **ADR courte** (« Mode Page : chemins de composition non quotés, contrainte du scanner gelé ») plutôt qu'une simple correction de Document 1, car la contrainte est structurelle (scanner gelé) et engage aussi 4.6 — une correction locale de Document 1 §2.1 seule laisserait la même incohérence resurgir à 4.6 sans qu'on sache qu'elle a déjà été tranchée.
