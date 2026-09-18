// crates/forge/fragment-forge/src/fragment/validator.rs

//! Phase 1.4 — Validateur sémantique : invariants structurels de la FSM de
//! rendu sur `&[FlatPageToken]`. Lecture seule, accumulation exhaustive des
//! erreurs (pas de fail-fast).
//!
//! Session IfEq/Else : la FSM `if`/`endif` passe de 2 à 3 états
//! (`Closed → Open → InElse → Closed`), partagée par `IfBool`, `IfEq` ET `IfNeq`
//! (les deux ouvrent un bloc conditionnel de façon identique du point de
//! vue de cette FSM — seule leur résolution/émission diffère, ailleurs).
//! `Else` reste strictement optionnel : `if`/`endif` sans `else` continue
//! de suivre exactement le chemin `Closed → Open → Closed` d'avant cette
//! session, zéro régression.

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
    /// Un `{% if %}` (ou `{% if ... == N %}`) rencontré alors qu'un bloc
    /// était déjà ouvert (branche `if` ou branche `else`). L'ouverture
    /// imbriquée est ignorée (heuristique de récupération) : l'état
    /// courant est préservé, le prochain `EndIf` ferme le bloc externe.
    NestedIfNotSupported {
        nested_entity: &'src str,
        nested_field: &'src str,
    },
    /// Fin de l'AST atteinte alors qu'un bloc `if` était encore ouvert
    /// (branche `if` ou branche `else`).
    UnclosedIf { entity: &'src str, field: &'src str },

    /// Un `{% else %}` rencontré alors qu'aucun bloc `if` n'était ouvert —
    /// soit qu'aucun `if` n'ait jamais été ouvert, soit que le bloc `if`
    /// courant ait déjà été refermé par un `{% endif %}` précédent. Les
    /// deux cas partagent la même erreur : dans les deux cas, l'état de la
    /// FSM au moment de `Else` est `Closed`, symétrique à `UnexpectedEndIf`.
    UnexpectedElse,
    /// Un second `{% else %}` rencontré alors que le bloc `if` courant est
    /// déjà dans sa branche `else` (`InElse`). L'état reste inchangé
    /// (heuristique de récupération, même politique que
    /// `NestedIfNotSupported`) : le prochain `EndIf` ferme normalement le
    /// bloc.
    DuplicateElse,

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

/// État de la FSM `if`/`else`/`endif` — 3 états (session IfEq/Else).
///
/// `Open` et `InElse` portent tous deux `(entity, field)` : nécessaire pour
/// que `UnclosedIf`/`NestedIfNotSupported` continuent de nommer le bloc
/// fautif quelle que soit la branche dans laquelle l'AST se termine ou dans
/// laquelle une imbrication est détectée — aucune perte d'information par
/// rapport à la FSM à 2 états qui précédait cette session.
#[derive(Clone, Copy)]
enum IfState<'src> {
    Closed,
    Open { entity: &'src str, field: &'src str },
    InElse { entity: &'src str, field: &'src str },
}

/// Parcourt l'AST et valide la machine à états des blocs conditionnels ET
/// des blocs de capture de scripts.
///
/// # Deux FSM indépendantes, jamais couplées
///
/// `{% if %}`/`{% else %}`/`{% endif %}` et `{% script %}`/`{% endscript %}`
/// sont structurellement voisines (marqueur de bloc, pas de pile,
/// imbrication interdite) mais orthogonales par le fond : l'une gate un
/// rendu RUNTIME (dépend de la ligne affichée), l'autre délimite une région
/// connue intégralement à la COMPILATION. Cette fonction ne les fait jamais
/// interagir — un `{% script %}` ouvert à l'intérieur d'un `{% if %}`
/// ouvert n'est PAS une erreur ICI (les deux FSM sont juste indépendamment
/// satisfaites) ; le rejet de ce cas précis est la responsabilité de
/// `hoist_and_dedupe_scripts` (une préoccupation de hoisting, pas de forme
/// d'AST — `validate_ast` reste borné à UNE seule question par paire de
/// marqueurs : est-elle bien équilibrée ?).
///
/// # FSM (`if`/`else`/`endif`) — 3 états (session IfEq/Else)
/// ```text
/// État : Closed | Open(entity, field) | InElse(entity, field)
///
/// Closed  + IfBool/IfEq/IfNeq → Open(entity, field)     [transition normale]
/// Closed  + Else          → Closed + push UnexpectedElse [erreur, état inchangé]
/// Closed  + EndIf         → Closed + push UnexpectedEndIf [erreur, état inchangé]
/// Open    + IfBool/IfEq/IfNeq → Open   + push Nested    [erreur, état inchangé]
/// Open    + Else          → InElse(entity, field)         [transition normale]
/// Open    + EndIf         → Closed                        [fermeture normale]
/// InElse  + IfBool/IfEq/IfNeq → InElse + push Nested    [erreur, état inchangé]
/// InElse  + Else          → InElse + push DuplicateElse    [erreur, état inchangé]
/// InElse  + EndIf         → Closed                         [fermeture normale]
/// EOF     + Open(e, f)    → push UnclosedIf(e, f)          [erreur de parité]
/// EOF     + InElse(e, f)  → push UnclosedIf(e, f)          [erreur de parité]
/// ```
///
/// `IfBool`, `IfEq` et `IfNeq` sont traités de façon strictement identique par cette
/// FSM : seule la présence d'un bloc conditionnel ouvert compte, jamais sa
/// nature (troncature vs égalité) — cohérent avec le principe « `Else` est
/// générique au niveau de `if` », qui vaut symétriquement pour l'ouverture.
///
/// # FSM (`script`) — même forme qu'avant cette session, aucun champ à mémoriser
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

    let mut if_state: IfState<'src> = IfState::Closed;
    // État de la seconde FSM, entièrement indépendant de `if_state`
    // — pas de champ à mémoriser, `script`/`endscript` ne portent aucune
    // donnée propre.
    let mut current_open_script = false;

    for token in tokens {
        // `match *token` : FlatPageToken est Copy (Phase 1.1).
        // Donne des bindings `entity: &'src str` directs, sans double indirection.
        match *token {
            FlatPageToken::IfBool { entity, field }
            | FlatPageToken::IfEq { entity, field, .. }
            | FlatPageToken::IfNeq { entity, field, .. } => match if_state {
                IfState::Closed => {
                    if_state = IfState::Open { entity, field };
                }
                IfState::Open { .. } | IfState::InElse { .. } => {
                    // Imbrication interdite, quelle que soit la branche
                    // courante (if ou else). Heuristique : l'ouverture
                    // imbriquée est ignorée, l'état reste sur le bloc
                    // externe — le prochain EndIf le fermera correctement.
                    errors.push(SemanticError::NestedIfNotSupported {
                        nested_entity: entity,
                        nested_field: field,
                    });
                }
            },

            FlatPageToken::Else => match if_state {
                IfState::Open { entity, field } => {
                    if_state = IfState::InElse { entity, field };
                }
                IfState::InElse { .. } => {
                    errors.push(SemanticError::DuplicateElse);
                }
                IfState::Closed => {
                    errors.push(SemanticError::UnexpectedElse);
                }
            },

            FlatPageToken::EndIf => match if_state {
                IfState::Open { .. } | IfState::InElse { .. } => {
                    if_state = IfState::Closed;
                }
                IfState::Closed => {
                    // EndIf sans bloc ouvert.
                    // L'état reste à Closed : les tokens suivants sont
                    // analysés comme s'ils étaient au niveau racine.
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
    // Si un bloc est resté ouvert (branche if ou branche else), l'erreur
    // est enregistrée après le parcours, ce qui garantit que toutes les
    // erreurs intra-parcours sont déjà dans `errors`.
    match if_state {
        IfState::Open { entity, field } | IfState::InElse { entity, field } => {
            errors.push(SemanticError::UnclosedIf { entity, field });
        }
        IfState::Closed => {}
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
    /// Vérifie que la FSM revient bien à l'état Closed après chaque EndIf,
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
    ///   EndIf                           : état=Closed → erreur [1], état reste Closed
    ///   IfBool { user, active }         : état=Closed → état = Open(user, active)
    ///   IfBool { user, admin }          : état=Open   → erreur [2], état reste Open(user, active)
    ///   EndIf                           : état=Open   → état = Closed  [ferme le bloc externe]
    ///   IfBool { user, premium }        : état=Closed → état = Open(user, premium)
    ///   EOF                             : état=Open   → erreur [3]
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

    // ─────────────────────────────────────────────────────────────────────
    // Session IfEq / Else
    // ─────────────────────────────────────────────────────────────────────

    /// `if` sans `else` : chemin d'avant cette session, zéro régression.
    #[test]
    fn if_without_else_is_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::EndIf,
        ];
        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// `if` + `else` + `endif` — IfBool, forme complète.
    #[test]
    fn if_with_else_is_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];
        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// `IfEq` + `else` + `endif` — même FSM que IfBool, mêmes garanties.
    #[test]
    fn if_eq_with_else_is_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];
        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// `IfNeq` + `else` + `endif` — même FSM également, mêmes garanties.
    #[test]
    fn if_neq_with_else_is_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfNeq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];
        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// `else` sans `if` jamais ouvert → UnexpectedElse.
    #[test]
    fn else_without_if_is_rejected() {
        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::Else];
        assert_eq!(
            validate_ast(tokens),
            Err(vec![SemanticError::UnexpectedElse])
        );
    }

    /// `else` après un `if` déjà refermé par `endif` → UnexpectedElse (même
    /// erreur que « else sans if » : l'état au moment de `Else` est Closed
    /// dans les deux cas).
    #[test]
    fn else_after_endif_is_rejected() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::EndIf,
            FlatPageToken::Else,
        ];
        assert_eq!(
            validate_ast(tokens),
            Err(vec![SemanticError::UnexpectedElse])
        );
    }

    /// Second `else` pour le même bloc → DuplicateElse, état préservé
    /// (le `endif` suivant referme normalement le bloc).
    #[test]
    fn double_else_is_rejected() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::Else,
            FlatPageToken::Static("C"),
            FlatPageToken::EndIf,
        ];
        assert_eq!(
            validate_ast(tokens),
            Err(vec![SemanticError::DuplicateElse])
        );
    }

    /// `endif` sans `if` jamais ouvert → UnexpectedEndIf (comportement
    /// préexistant, revérifié explicitement dans le contexte de la FSM à
    /// 3 états).
    #[test]
    fn endif_without_if_is_rejected() {
        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::EndIf];
        assert_eq!(
            validate_ast(tokens),
            Err(vec![SemanticError::UnexpectedEndIf])
        );
    }

    /// `if` sans `endif`, avec un `else` entre-temps — EOF alors que l'état
    /// est `InElse` : doit produire `UnclosedIf`, exactement comme un `if`
    /// sans `else` non refermé.
    #[test]
    fn unclosed_if_in_else_branch_is_rejected() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
        ];
        assert_eq!(
            validate_ast(tokens),
            Err(vec![SemanticError::UnclosedIf {
                entity: "record",
                field: "document_id",
            }])
        );
    }
}
