// crates/forge/fragment-forge/src/fragment/parser.rs

//! Phase 1.3 — Classifieur de tokens : `RawSpan` → `Vec<FlatPageToken>`.
//! Syntaxe uniquement — pas de lookup de schéma (Phase 1.4), pas
//! d'équilibrage IfBool/EndIf (Phase 1.4), fail-fast sur la première
//! erreur syntaxique.

#[cfg(test)]
use crate::fragment::lexer::scan;
use crate::fragment::lexer::{RawSpan, SpanKind};
use crate::fragment::token::FlatPageToken;

// =============================================================================
// Phase 1.3 — Classifieur de Tokens (Parseur Syntaxique)
// =============================================================================
//
// Responsabilité unique : traduire un flux de RawSpan en Vec<FlatPageToken>.
//
// Frontières strictes :
//   - Syntaxe uniquement. Pas de lookup dans SchemaContext (Phase 1.4).
//   - Pas d'équilibrage IfBool/EndIf (Phase 1.4).
//   - Pas d'I/O disque : StaticInclude::len = 0 (hack provisoire documenté).
//   - Fail-fast : première erreur syntaxique = retour immédiat.

/// Erreur syntaxique produite par `parse_tokens`.
///
/// Couvre uniquement les erreurs de structure token-niveau.
/// La validation sémantique (champ inconnu, déséquilibre if/endif)
/// est déléguée à Phase 1.4.
#[derive(Debug, PartialEq, Eq)]
pub enum PageParseError {
    /// Token reçu ≠ token attendu à cette position de l'automate.
    UnexpectedToken {
        expected: &'static str,
        got: SpanKind,
    },
    /// Itérateur épuisé alors qu'un token était requis pour compléter un pattern.
    UnexpectedEof,
    /// Séquence de bloc non reconnue :
    ///   keyword inconnu, ou `if entity.field` sans `.` dans l'ident bloc.
    InvalidBlockSequence,
}

/// Transforme un flux de `RawSpan<'src>` en AST `Vec<FlatPageToken<'src>>`.
///
/// Automate à états implicites : chaque appel à `next()` sur l'itérateur
/// consomme la tête de séquence, et les helpers consomment les spans suivants
/// selon le pattern du token courant.
///
/// `.peekable()` est créé ici pour que Phase 1.4 puisse étendre ce parseur
/// avec du lookahead sans changer la signature de `parse_tokens`.
///
/// Allocation : le `Vec` de sortie est build-time uniquement.
/// Il est consommé par les phases 2 et 3 et n'existe pas au runtime.
pub fn parse_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<Vec<FlatPageToken<'src>>, PageParseError> {
    let mut iter = spans.peekable();
    let mut ast = Vec::new();

    while let Some(span) = iter.next() {
        let token = match span.kind {
            // Texte HTML verbatim → Static directement.
            SpanKind::Literal => FlatPageToken::Static(span.slice),

            // `{{ entity.field }}` → Field.
            SpanKind::ExprOpen => parse_expr(&mut iter)?,

            // `{% keyword … %}` → IfBool | EndIf | StaticInclude.
            SpanKind::BlockOpen => parse_block(&mut iter)?,

            // Tout autre span en position initiale est une erreur structurelle.
            // ExprClose, BlockClose, Ident, Punct ne peuvent pas ouvrir un token.
            got => {
                return Err(PageParseError::UnexpectedToken {
                    expected: "Literal | ExprOpen | BlockOpen",
                    got,
                });
            }
        };
        ast.push(token);
    }

    Ok(ast)
}

// ─── Parseurs de sous-séquences ──────────────────────────────────────────────

/// Consomme `Ident(entity) Punct(.) Ident(field) ExprClose` et produit `Field`.
///
/// Précondition : `ExprOpen` vient d'être consommé par `parse_tokens`.
fn parse_expr<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let entity = expect_ident(iter, "Ident(entity)")?;
    expect_kind(iter, SpanKind::Punct, "Punct('.')")?;
    let field = expect_ident(iter, "Ident(field)")?;
    expect_kind(iter, SpanKind::ExprClose, "ExprClose('}}')")?;
    Ok(FlatPageToken::Field { entity, field })
}

/// Consomme `Ident(keyword) … BlockClose` et produit le token de bloc.
///
/// Précondition : `BlockOpen` vient d'être consommé par `parse_tokens`.
///
/// Pattern `if` : `Ident("entity.field")` est découpé sur `.` ici,
/// car le scanner InBlock le produit comme un seul Ident (contrairement
/// à InExpr qui émet `Ident Punct Ident`). Voir décision Phase 1.2.
///
/// Pattern `include` : `len = 0` et `rel_from_manifest = original_path`
/// sont des valeurs provisoires. L'orchestrateur (build.rs) injectera
/// la longueur réelle via `std::fs::metadata` après le parsing.
fn parse_block<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let keyword = expect_ident(
        iter,
        "keyword (if | endif | include | asset | script | endscript)",
    )?;

    match keyword {
        "if" => {
            let raw = expect_ident(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted(raw)?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::IfBool { entity, field })
        }
        "endif" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::EndIf)
        }
        // `{% asset key %}` (spec §9) : capture brute de la clé logique,
        // zéro E/S, zéro résolution — même discipline que `include` :
        // la résolution (ici vers une URL, jamais un contenu) est différée
        // à `resolve_and_measure`/`generate_aot_snippet`.
        "asset" => {
            let key = expect_ident(iter, "Ident(key)")?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::AssetRef(key))
        }
        // `{% script %}` / `{% endscript %}` (session dédiée au hoisting) :
        // valides dans les deux modes, comme `asset` — un Fragment inclus
        // peut légitimement porter son propre `<script>`, hissé si la
        // cible finale est une Page, laissé inline (No-Op) si la cible est
        // le Fragment lui-même résolu isolément (voir doc de la variante).
        "script" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::ScriptStart)
        }
        "endscript" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::ScriptEnd)
        }
        "include" => {
            let path = expect_ident(iter, "Ident(path)")?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::StaticInclude {
                original_path: path,
                rel_from_manifest: path, // provisoire : sera résolu par l'orchestrateur
                len: 0,                  // provisoire : idem
            })
        }
        _ => Err(PageParseError::InvalidBlockSequence),
    }
}

// ─── Primitives de consommation ───────────────────────────────────────────────

/// Consomme le span suivant et retourne sa slice si c'est un `Ident`.
/// Retourne une erreur décrivant ce qui était attendu sinon.
#[inline]
fn expect_ident<'src, I>(iter: &mut I, expected: &'static str) -> Result<&'src str, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == SpanKind::Ident => Ok(span.slice),
        Some(span) => Err(PageParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageParseError::UnexpectedEof),
    }
}

/// Consomme le span suivant et vérifie qu'il a le `kind` attendu.
/// La slice n'est pas retournée (les délimiteurs ne portent pas de sémantique).
#[inline]
fn expect_kind<'src, I>(
    iter: &mut I,
    kind: SpanKind,
    expected: &'static str,
) -> Result<(), PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == kind => Ok(()),
        Some(span) => Err(PageParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageParseError::UnexpectedEof),
    }
}

/// Coupe `"entity.field"` sur le premier `.` et retourne `("entity", "field")`.
///
/// Les sous-slices partagent le lifetime `'src` de `raw` :
/// elles pointent directement dans la source du template, sans allocation.
///
/// `.` est ASCII single-byte : `i` et `i+1` sont des frontières char valides.
#[inline]
fn split_dotted(raw: &str) -> Result<(&str, &str), PageParseError> {
    raw.find('.')
        .map(|i| (&raw[..i], &raw[i + 1..]))
        .ok_or(PageParseError::InvalidBlockSequence)
}

// =============================================================================
// Tests — Phase 1.3
// =============================================================================

#[cfg(test)]
mod tests_phase_1_3 {
    use super::{FlatPageToken, PageParseError, SpanKind, parse_tokens, scan};

    /// Jalon Vert Phase 1.3.
    ///
    /// Pipeline complet : scan() → parse_tokens() sur la chaîne de référence.
    ///
    /// Décompte : 8 tokens (et non 7 comme indiqué dans le prompt).
    /// Les 3 espaces inter-blocs (" ") produisent 3 Static("·") distincts
    /// car le scanner est en mode Literal entre chaque `%}` et le `{%` suivant.
    /// Supprimer ces espaces serait une décision sémantique qui appartient
    /// à l'orchestrateur ou à un éventuel pass de compression — pas au parseur.
    #[test]
    fn parse_full_template() {
        let src =
            "hello {{ user.name }} {% if user.active %} {% include fragment.html %} {% endif %}";
        let got = parse_tokens(scan(src)).expect("parsing doit réussir sur un template valide");

        // Note : FlatPageToken doit dériver PartialEq, Eq (ajout non-cassant sur Phase 1.1).
        let expected: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("hello "),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::Static(" "),
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::Static(" "),
            FlatPageToken::StaticInclude {
                original_path: "fragment.html",
                rel_from_manifest: "fragment.html",
                len: 0,
            },
            FlatPageToken::Static(" "),
            FlatPageToken::EndIf,
        ];

        assert_eq!(
            got.len(),
            expected.len(),
            "nombre de tokens incorrect : got {}, expected {}",
            got.len(),
            expected.len()
        );

        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "token[{i}] incorrect");
        }
    }

    /// Erreur sur token inattendu en position initiale (ExprClose seul, sans ExprOpen).
    #[test]
    fn error_on_unexpected_top_level_span() {
        // Scanner ne peut pas produire un ExprClose seul en position initiale,
        // mais ce test vérifie le chemin d'erreur de parse_tokens directement.
        use super::RawSpan;
        let orphan = [RawSpan {
            slice: "}}",
            kind: SpanKind::ExprClose,
        }];
        let err = parse_tokens(orphan.into_iter()).unwrap_err();
        assert_eq!(
            err,
            PageParseError::UnexpectedToken {
                expected: "Literal | ExprOpen | BlockOpen",
                got: SpanKind::ExprClose,
            }
        );
    }

    /// Erreur sur `{% if active %}` sans préfixe `entity.` (pas de `.` dans l'ident).
    #[test]
    fn error_on_if_without_dot() {
        let src = "{% if active %}";
        let err = parse_tokens(scan(src)).unwrap_err();
        assert_eq!(err, PageParseError::InvalidBlockSequence);
    }

    /// Erreur sur `{% %}` (keyword manquant → BlockClose immédiat après BlockOpen).
    #[test]
    fn error_on_empty_block() {
        let src = "{% %}";
        let err = parse_tokens(scan(src)).unwrap_err();
        // Le scanner InBlock voit "%}" immédiatement → Ident attendu, BlockClose reçu.
        assert_eq!(
            err,
            PageParseError::UnexpectedToken {
                expected: "keyword (if | endif | include | asset | script | endscript)",
                got: SpanKind::BlockClose,
            }
        );
    }
}
