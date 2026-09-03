//! Phase 1.4 — Validateur sémantique : invariants structurels de la FSM de
//! rendu sur `&[FlatPageToken]`. Lecture seule, accumulation exhaustive des
//! erreurs (pas de fail-fast).

use crate::fragment::token::FlatPageToken;

// =============================================================================
// Phase 1.4 — Validateur Sémantique (Structural Validator)
// =============================================================================
//
// Responsabilité unique : valider les invariants structurels de la FSM de rendu.
//
// Frontières strictes :
//   - Lecture seule sur &[FlatPageToken<'src>] : aucune modification de l'AST.
//   - Accumulation exhaustive des erreurs : pas de fail-fast.
//   - Aucune consultation de SchemaContext : les champs entity/field ne sont
//     pas vérifiés contre le schéma BDD ici (périmètre Phase suivante).
//   - Zéro récursion. FSM linéaire : une seule variable d'état scalaire.
//
// Justification de l'interdiction d'imbrication (invariant DOD) :
//   Un moteur de rendu linéaire sans pile de récursion exige que le graphe
//   de contrôle soit un DAG plat. Un `if` imbriqué introduit un niveau de
//   call stack ou un compteur de profondeur au runtime — incompatible avec
//   l'invariant zéro-allocation du hot path.

/// Erreur sémantique produite par `validate_ast`.
///
/// Les champs `&'src str` pointent directement dans les tokens de l'AST,
/// eux-mêmes pointant dans le buffer source lu par `fs::read_to_string`.
/// Aucune allocation. Le Vec<SemanticError> lui-même est build-time uniquement.
#[derive(Debug, PartialEq, Eq)]
pub enum SemanticError<'src> {
    /// Un `{% endif %}` rencontré alors qu'aucun bloc n'était ouvert.
    UnexpectedEndIf,
    /// Un `{% if %}` rencontré alors qu'un bloc était déjà ouvert.
    /// L'ouverture imbriquée est ignorée (heuristique de récupération) :
    /// l'état courant est préservé, le prochain `EndIf` ferme le bloc externe.
    NestedIfNotSupported {
        nested_entity: &'src str,
        nested_field: &'src str,
    },
    /// Fin de l'AST atteinte alors qu'un bloc `if` était encore ouvert.
    UnclosedIf { entity: &'src str, field: &'src str },

    /// Un `{% endscript %}` rencontré alors qu'aucun bloc n'était ouvert.
    /// Symétrique à `UnexpectedEndIf` — FSM indépendante, voir doc de
    /// `validate_ast`.
    UnexpectedEndScript,
    /// Un `{% script %}` rencontré alors qu'un bloc était déjà ouvert.
    /// Même heuristique de récupération que `NestedIfNotSupported` :
    /// l'ouverture imbriquée est ignorée, l'état courant est préservé.
    /// Pas de champs (contrairement à `NestedIfNotSupported`) : `script`/
    /// `endscript` ne portent aucune donnée propre, à la différence d'`if`.
    NestedScriptNotSupported,
    /// Fin de l'AST atteinte alors qu'un bloc `script` était encore ouvert.
    UnclosedScript,
}

/// Parcourt l'AST et valide la machine à états des blocs conditionnels ET
/// des blocs de capture de scripts.
///
/// # Deux FSM indépendantes, jamais couplées
///
/// `{% if %}`/`{% endif %}` et `{% script %}`/`{% endscript %}` sont
/// structurellement identiques (marqueur de bloc, scalaire unique, pas de
/// pile, imbrication interdite) mais orthogonaux par le fond : l'un gate
/// un rendu RUNTIME (dépend de la ligne affichée), l'autre délimite une
/// région connue intégralement à la COMPILATION. Cette fonction ne les
/// fait jamais interagir — un `{% script %}` ouvert à l'intérieur d'un
/// `{% if %}` ouvert n'est PAS une erreur ICI (les deux FSM sont juste
/// indépendamment satisfaites) ; le rejet de ce cas précis est la
/// responsabilité de `hoist_and_dedupe_scripts` (une préoccupation de
/// hoisting, pas de forme d'AST — `validate_ast` reste borné à UNE seule
/// question par paire de marqueurs : est-elle bien équilibrée ?).
///
/// # FSM (`if`)
/// ```text
/// État : None | Some((entity, field))
///
/// None  + IfBool      → Some(entity, field)         [transition normale]
/// None  + EndIf       → None  + push UnexpectedEndIf [erreur, état inchangé]
/// Some  + IfBool      → Some  + push Nested          [erreur, état inchangé]
/// Some  + EndIf       → None                         [fermeture normale]
/// EOF   + Some(e, f)  → push UnclosedIf(e, f)        [erreur de parité]
/// ```
///
/// # FSM (`script`) — même forme, aucun champ à mémoriser
/// ```text
/// État : false | true
///
/// false + ScriptStart → true  + push UnexpectedEndScript si déjà ouvert
/// false + ScriptEnd   → false + push UnexpectedEndScript [erreur, état inchangé]
/// true  + ScriptStart → true  + push NestedScriptNotSupported [erreur, état inchangé]
/// true  + ScriptEnd   → false                        [fermeture normale]
/// EOF   + true        → push UnclosedScript           [erreur de parité]
/// ```
///
/// `*     + Static/Field/Include/Asset → état inchangé` pour les deux FSM
/// (neutre).
///
/// # Garantie de terminaison
/// Parcours linéaire de longueur `tokens.len()` : O(n), pas de récursion.
///
/// # Allocation
/// `Vec::new()` n'alloue pas avant le premier `push` :
/// un AST valide produit `Ok(())` sans allocation heap.
pub fn validate_ast<'src>(tokens: &[FlatPageToken<'src>]) -> Result<(), Vec<SemanticError<'src>>> {
    let mut errors: Vec<SemanticError<'src>> = Vec::new();

    // `None`              : pas de bloc conditionnel ouvert.
    // `Some((e, f))`      : dans un bloc `{% if e.f %}`, en attente de `{% endif %}`.
    let mut current_open_if: Option<(&'src str, &'src str)> = None;
    // État de la seconde FSM, entièrement indépendant de `current_open_if`
    // — pas de champ à mémoriser, `script`/`endscript` ne portent aucune
    // donnée propre.
    let mut current_open_script = false;

    for token in tokens {
        // `match *token` : FlatPageToken est Copy (Phase 1.1).
        // Donne des bindings `entity: &'src str` directs, sans double indirection.
        match *token {
            FlatPageToken::IfBool { entity, field } => match current_open_if {
                None => {
                    current_open_if = Some((entity, field));
                }
                Some(_) => {
                    // Imbrication interdite.
                    // Heuristique : l'ouverture imbriquée est ignorée.
                    // L'état reste sur le bloc externe : le prochain EndIf
                    // fermera correctement ce bloc plutôt que de le laisser ouvert.
                    errors.push(SemanticError::NestedIfNotSupported {
                        nested_entity: entity,
                        nested_field: field,
                    });
                }
            },

            FlatPageToken::EndIf => match current_open_if {
                Some(_) => {
                    // Fermeture normale.
                    current_open_if = None;
                }
                None => {
                    // EndIf sans bloc ouvert.
                    // L'état reste à None : les tokens suivants sont analysés
                    // comme s'ils étaient au niveau racine.
                    errors.push(SemanticError::UnexpectedEndIf);
                }
            },

            // Symétrique exact de IfBool/EndIf ci-dessus, FSM séparée.
            FlatPageToken::ScriptStart => {
                if current_open_script {
                    errors.push(SemanticError::NestedScriptNotSupported);
                } else {
                    current_open_script = true;
                }
            }

            FlatPageToken::ScriptEnd => {
                if current_open_script {
                    current_open_script = false;
                } else {
                    errors.push(SemanticError::UnexpectedEndScript);
                }
            }

            // Static, Field, StaticInclude, AssetRef : aucun effet sur les FSM.
            // ModulesPlaceholder : jamais produit à ce stade (injecté par
            // build.rs après validate_ast, même ordre que ScriptStart/
            // ScriptEnd hissés par hoist_and_dedupe_scripts) — présent ici
            // uniquement pour l'exhaustivité du match, pas parce que ce
            // point est atteignable en pratique.
            FlatPageToken::Static(_)
            | FlatPageToken::Field { .. }
            | FlatPageToken::StaticInclude { .. }
            | FlatPageToken::AssetRef(_)
            | FlatPageToken::ModulesPlaceholder => {}
        }
    }

    // Contrôle de parité final.
    // Si un bloc est resté ouvert, l'erreur est enregistrée après le parcours,
    // ce qui garantit que toutes les erreurs intra-parcours sont déjà dans `errors`.
    if let Some((entity, field)) = current_open_if {
        errors.push(SemanticError::UnclosedIf { entity, field });
    }
    if current_open_script {
        errors.push(SemanticError::UnclosedScript);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 1.4
// =============================================================================

#[cfg(test)]
mod tests_phase_1_4 {
    use super::{FlatPageToken, SemanticError, validate_ast};

    /// Jalon Vert — séquence valide : deux blocs if séquentiels non imbriqués.
    ///
    /// Vérifie que la FSM revient bien à l'état None après chaque EndIf,
    /// et que le second IfBool ne déclenche pas de NestedIfNotSupported.
    #[test]
    fn test_semantic_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("avant"),
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::EndIf,
            FlatPageToken::Static("entre"),
            FlatPageToken::IfBool {
                entity: "user",
                field: "admin",
            },
            FlatPageToken::Static("accès restreint"),
            FlatPageToken::EndIf,
            FlatPageToken::Static("après"),
        ];

        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// Jalon Vert — séquence invalide : 3 erreurs distinctes accumulées.
    ///
    /// Séquence construite pour produire exactement, dans l'ordre :
    ///   1. `UnexpectedEndIf`              — EndIf avant tout IfBool
    ///   2. `NestedIfNotSupported`         — IfBool dans un IfBool
    ///   3. `UnclosedIf { "user", "premium" }` — EOF avec bloc ouvert
    ///
    /// Trace de la FSM :
    ///   EndIf                           : état=None  → erreur [1], état reste None
    ///   IfBool { user, active }         : état=None  → état = Some(user, active)
    ///   IfBool { user, admin }          : état=Some  → erreur [2], état reste Some(user, active)
    ///   EndIf                           : état=Some  → état = None  [ferme le bloc externe]
    ///   IfBool { user, premium }        : état=None  → état = Some(user, premium)
    ///   EOF                             : état=Some  → erreur [3]
    #[test]
    fn test_semantic_errors() {
        let tokens: &[FlatPageToken<'_>] = &[
            // [1] EndIf orphelin
            FlatPageToken::EndIf,
            // Ouverture d'un bloc externe
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            // [2] Imbrication interdite — l'externe reste actif
            FlatPageToken::IfBool {
                entity: "user",
                field: "admin",
            },
            // Ferme le bloc externe (l'imbriqué a été ignoré)
            FlatPageToken::EndIf,
            // [3] Bloc non fermé à l'EOF
            FlatPageToken::IfBool {
                entity: "user",
                field: "premium",
            },
        ];

        let expected = vec![
            SemanticError::UnexpectedEndIf,
            SemanticError::NestedIfNotSupported {
                nested_entity: "user",
                nested_field: "admin",
            },
            SemanticError::UnclosedIf {
                entity: "user",
                field: "premium",
            },
        ];

        assert_eq!(validate_ast(tokens), Err(expected));
    }

    /// Cas limite : AST vide. Aucun token, aucune erreur.
    #[test]
    fn test_semantic_empty_ast() {
        assert_eq!(validate_ast(&[]), Ok(()));
    }
}
