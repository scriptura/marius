# ADR-004 — Normalisation des Index de Configuration et Décalages Mémoire

**Statut** : Accepté  
**Date** : 2026  
**Composants** : `crates/forge/guard-forge`, `crates/forge/bridge-forge`, `crates/core/collector`

---

## Contexte

Les tables de présence (_Collector_, bit-vectors atomiques) et les filtres associés sont dimensionnés et configurés via des fichiers YAML éditables par des opérateurs humains. Ces fichiers spécifient des index de colonnes, des numéros d'attributs PostgreSQL (`attnum`), et des positions dans les vecteurs de bits.

Deux référentiels d'indexation coexistent dans le système :

- **Référentiel humain (1-based)** : `attnum` PostgreSQL commence à 1. Les opérateurs humains numérotent les colonnes à partir de 1 dans les configurations et la documentation.
- **Référentiel machine (0-based)** : les offsets mémoire, les indices de mots dans un bit-vector, les instructions CPU (`TZCNT`, `LZCNT`, BSF/BSR) opèrent sur des positions à base 0.

---

## Problématique

Un index 1-based non normalisé qui atteint le Core produit une **erreur de décalage d'un** (_off-by-one_). Dans un bit-vector, lire ou écrire le bit `n` au lieu du bit `n-1` :

- **Faux négatif** : une entité présente n'est pas détectée par le scan `TZCNT` → projection manquée, artefact HTML périmé sans déclenchement de regénération.
- **Faux positif** : une entité absente est projetée → requête SQLx sur un ID inexistant, résultat vide, artefact écrasé par un contenu vide.
- **Dépassement silencieux** : pour le dernier ID du domaine, `bit n` pointe hors du mot courant, dans le mot suivant → corruption du Collector sans panique.

Ces erreurs sont non-déterministes en production (dépendent de la valeur de l'ID) et difficiles à reproduire en test unitaire sans jeu de données couvrant les bornes.

---

## Décision

### Règle unique : normalisation à la frontière d'entrée

**L'index passe de 1-based à 0-based une seule fois, au moment du parsing de la configuration**, dans le code de résolution de Guard-Forge ou Bridge-Forge, avant toute transmission au Core.

```
YAML (1-based) → Parser/Forge → décrémentation → Core (0-based)
```

Aucune autre couche n'effectue de conversion. Le Core reçoit exclusivement des index 0-based.

### Implémentation dans Guard-Forge / Bridge-Forge

Le parser de configuration expose une fonction de normalisation explicite et nommée :

```rust
/// Convertit un index de colonne issu du YAML (1-based, convention opérateur)
/// en offset 0-based pour les calculs de bit-vector dans le Core.
///
/// Panics si `one_based == 0` : index YAML invalide (aucune colonne n'est à 0).
/// La panique au parsing est préférable à une corruption silencieuse au runtime.
#[inline]
pub fn col_index_to_offset(one_based: u16) -> u16 {
    assert!(one_based > 0, "ADR-004 : index YAML invalide (reçu 0, attendu >= 1)");
    one_based - 1
}
```

La fonction est appelée exhaustivement sur tous les index lors du parsing, jamais en différé.

### Invariant Core

**Le Core ne contient aucune soustraction ou addition compensatoire d'index.** Tout calcul d'offset dans `Collector`, `Dispatcher`, et les routines SIMD opère directement sur la valeur reçue, sans ajustement.

Cet invariant est vérifiable statiquement : une revue de code ou un lint personnalisé peut interdire les expressions `attnum - 1` ou `index + 1` hors du module de parsing.

### Traçabilité dans la documentation DDL

Les commentaires SQL sur les colonnes surveillées rappellent l'index 1-based visible dans `pg_attribute.attnum` :

```sql
-- attnum=3 dans pg_attribute → offset=2 dans le Collector (ADR-004)
COMMENT ON COLUMN content.core.status IS 'marius:tracked:attnum=3';
```

Guard-Forge lit ce commentaire, extrait `attnum=3`, applique `col_index_to_offset(3) = 2`, émet la constante `CONTENT_CORE_STATUS_OFFSET: usize = 2` dans le fichier généré.

### Cas particulier : `attnum` PostgreSQL

`pg_attribute.attnum` est intrinsèquement 1-based (les colonnes système ont `attnum <= 0`, les colonnes utilisateur commencent à `attnum = 1`). DB-Forge normalise `attnum` vers 0-based immédiatement après la requête `pg_attribute`, avant toute transmission à Fragment-Forge ou aux structures internes :

```rust
// Dans fetch_columns() — DB-Forge
let offset = col_index_to_offset(attnum as u16) as i16;
```

---

## Conséquences

| Propriété                              | Garantie                                                                             |
| -------------------------------------- | ------------------------------------------------------------------------------------ |
| Core exempt de logique de compensation | Aucune soustraction d'index dans `collector.rs`, `dispatcher.rs`                     |
| Panique au parsing, pas au runtime     | `assert!(one_based > 0)` dans `col_index_to_offset`                                  |
| Traçabilité opérateur → machine        | Commentaire SQL `marius:tracked:attnum=N` + constante générée                        |
| Lint statique possible                 | Une seule expression `n - 1` autorisée dans Guard-Forge/Bridge-Forge                 |
| Compatibilité Phase 2                  | Les offsets mmap sont 0-based par construction ; la règle s'applique sans changement |

**Contrat entre couches** :

```
Guard-Forge / Bridge-Forge    →    Core
  index 1-based (YAML, DDL)        offset 0-based
  col_index_to_offset() ici        jamais ici
```

Toute violation de ce contrat constitue un bug architectural, pas une erreur de logique applicative.
