## Layout réel — préalable factuel à toute l'analyse

Mesure directe (`size_of`), pas l'estimation du Document 1 :

| Type                       | Taille mesurée |
| -------------------------- | -------------- |
| `FlatPageToken<'src>` seul | **48 octets**  |
| `PageBlockToken<'src>`     | 16 octets      |
| `StaticPartialRef<'src>`   | 16 octets      |
| `PageSourceToken<'src>`    | **48 octets**  |

Le Document 1 §5 affirme _« FlatPageToken seul (24 octets) »_ et présente `Unsupported` (32 octets) comme la variante dominante, avec un surcoût _« légèrement supérieur »_ dû à l'enum englobant. **Les deux affirmations sont fausses** : `FlatPageToken` seul pèse déjà 48 octets (probablement `StaticInclude { &str, &str, usize }` = 3 mots = 40, plus tag aligné sur 8), et `PageSourceToken` a exactement la même taille que `FlatPageToken` seul — **zéro octet de surcoût**, pas un surcoût léger.

Cause : niche-filling. Le compilateur loge le discriminant de `PageSourceToken` (4 variantes) dans un bit-pattern inutilisé du tag interne de `FlatPageToken`, plutôt que d'ajouter un tag externe + padding. L'enum englobant est donc **gratuit** dans ce cas précis — pas « bon marché », gratuit.

Conséquence directe sur votre dernière question : c'est la propriété non anticipée par les documents.

---

## Réponses point par point

**Pertinence des phases restantes** — Oui, la chaîne de dépendances tient. Deux nuances :

- 4.3 va matérialiser `Vec<PageSourceToken>` en volume. Stride uniforme à 48 octets quel que soit le contenu réel du token (un `Field` simple coûte autant qu'un `StaticInclude`). C'est un coût _build-time_ accepté par le Document 1 — toujours vrai, mais désormais quantifié, pas supposé.
- 5.4 route `Unsupported { keyword: &str, .. }` vers des `PageValidationError` nommées. Ce n'est pas un `match` exhaustif compilateur — c'est une comparaison de chaînes. L'exhaustivité n'est garantie que par les 6 tests paramétrés de 4.7, pas par le système de types. Risque latent : un mot-clé futur non testé retombe silencieusement dans un bras par défaut.

**Fusions possibles** — 4.4 et 4.5 sont deux ajouts de bras de `match` strictement indépendants (aucun état partagé, aucune dépendance d'ordre). Fusion à faible risque, gain marginal. Je ne la recommande pas : la règle de découpage du Document (« un diff mono-responsabilité ») a une valeur diagnostique — en cas de régression, l'isolement par commit identifie immédiatement l'opérateur fautif.

**Découpages nécessaires** — Aucun. 5.2 combine appariement par pile et calcul d'indices, mais les deux sont indissociables (l'indice n'existe qu'une fois la pile validée) — pas un découpage artificiel à faire.

**Risques disparus** — Un risque catégorique, pas seulement local à 4.1 : casser l'exhaustivité de `validate_ast`/`resolve_and_measure`/`generate_aot_snippet` en ajoutant une variante à `FlatPageToken`. Le choix d'enum englobant retire ce risque pour **toute** la roadmap restante (4.2 à 6.6), pas uniquement pour cette phase — aucune de ces trois fonctions gelées ne sera jamais retouchée par construction du type.

**Nouveaux risques** — Deux :

1. Le budget de niche n'est pas infini. Rien ne garantit qu'une 5ᵉ variante future sur `PageSourceToken` reste gratuite — à mesurer, pas à supposer, au moment où elle apparaîtrait (aucune n'est prévue dans la roadmap actuelle : Document 1 ferme l'enum à 4 variantes).
2. Dispatch par chaîne en 5.4 (ci-dessus) — absence d'exhaustivité compilateur sur le mapping mot-clé → erreur.

**Signatures simplifiables** — Aucune. `detect_extends : &str -> bool` reste bien découplé de `PageSourceToken` (vérifié — zéro dépendance croisée dans le type ajouté aujourd'hui).

**Structures devenues inutiles** — Aucune. `PageSourceToken` est utilisé par toute la Phase 5.

**Implémentation plus élégante que les documents** — Non, sous la contrainte « `FlatPageToken` gelé ». L'encodage actuel est déjà au plancher de coût possible (démontré, pas supposé) : 0 octet ajouté par l'enum englobant.

**Propriété non anticipée** — Oui, formulée ci-dessus : l'estimation mémoire du Document 1 était erronée d'un facteur 2× sur `FlatPageToken` seul, et le raisonnement « surcoût léger accepté » n'a plus lieu d'être — il n'y a pas de surcoût du tout. Le Document 1 avait la bonne conclusion (accepter le coût) pour une mauvaise raison (coût mal mesuré). À corriger dans le document avant Phase 4.2, pour que le §5 ne serve pas de référence erronée aux phases suivantes.

_2 juillet 2026_
