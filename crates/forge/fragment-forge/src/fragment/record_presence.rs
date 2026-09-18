// crates/forge/fragment-forge/src/fragment/record_presence.rs

//! Élimination AOT des conditions `record.*` pour une unité de compilation
//! sans `Record` — session « shell/représentation ».
//!
//! ## Principe
//!
//! Deux axes distincts (arbitrage de session) :
//!   - `record.*` (`IfBool`/`IfEq`) : donnée de record, résolue au dump,
//!     une fois par ligne — inchangé pour toute unité qui possède
//!     effectivement un `Record`.
//!   - présence/absence de `Record` pour l'unité de compilation courante :
//!     déjà connue de Forge, AVANT tout parsing de template, via le
//!     `SchemaIndex` construit par l'appelant (`fixed`/`varlena` non vides
//!     pour une unité avec record ; `SchemaIndex { fixed: &[], varlena: &[] }`
//!     — TOUJOURS vide, jamais un paramètre — pour `STATIC_PAGES`, cf. doc
//!     de `resolve_static_page`). Aucune nouvelle identité de représentation
//!     n'est introduite ici : ce module se contente d'exploiter un fait déjà
//!     produit par l'appelant.
//!
//! Pour une unité SANS record, toute condition `record.*` est statiquement
//! fausse — pour `IfEq` (`record.field == N`) comme pour `IfNeq`
//! (`record.field != N`), session `!=` : audit dédié, décision non dérivée
//! par négation logique. L'absence de record rend indécidable toute
//! affirmation d'identité, positive ou négative — analogue à la logique
//! ternaire SQL, où `NULL = 1` et `NULL <> 1` sont tous deux « non vrai »,
//! jamais l'un vrai et l'autre faux. Forge élimine donc la branche `if`
//! (qu'elle soit `IfBool`, `IfEq` ou `IfNeq`, traitées identiquement) et ne
//! conserve que la branche `else`, si elle existe — sinon rien. Ceci a lieu AVANT `resolve_and_measure` : un champ
//! comme `record.document_id`, référencé dans une branche éliminée, n'est
//! donc jamais recherché dans le `SchemaIndex` et ne peut jamais produire
//! `ResolverError::UnknownField` pour cette seule raison.
//!
//! ## Point d'insertion dans le pipeline
//!
//! ```text
//! parse (parse_tokens | parse_page_tokens → … → lower)
//!        ↓
//! validate_ast            — structure if/else/endif, indépendant du schéma
//!        ↓
//! eliminate_recordless_conditions(tokens, schema)   ← CE MODULE
//!        ↓
//! resolve_and_measure     — ne voit plus jamais une condition record.*
//!        ↓                  dans une unité qui n'a structurellement aucun
//!        ↓                  champ à résoudre
//! generate_aot_snippet
//! ```
//!
//! `validate_ast` s'exécute AVANT cette passe, pas après : la validité
//! structurelle d'un `if`/`else`/`endif` (équilibrage, absence
//! d'imbrication) ne dépend jamais de la présence d'un record — une erreur
//! de structure dans une unité sans record doit être rapportée comme dans
//! n'importe quelle autre unité, jamais silencieusement avalée par cette
//! élimination.
//!
//! ## Ce que ce module NE fait PAS
//!
//! Ne modifie jamais le sens de `record.*` pour une unité qui possède un
//! `Record` : `has_record()` (ci-dessous) retourne `true` dès que `fixed`
//! ou `varlena` contient au moins une entrée, auquel cas cette passe est un
//! no-op strict (retourne `tokens` sans y toucher) — `resolve_and_measure`
//! continue de voir exactement le même flux qu'avant cette session, y
//! compris son propre rejet `UnknownField` pour un champ réellement
//! inconnu (distinct de « aucun record du tout »).
//!
//! N'introduit aucune nouvelle entité (`nav.*`, `ShellContext`, etc.) : la
//! syntaxe `.marius` reste `{% if record.field %}`/`{% if record.field ==
//! N %}` dans les deux cas, avec ou sans record — seul le comportement de
//! Forge en aval du parsing diffère selon l'unité de compilation.

use crate::fragment::token::FlatPageToken;
use crate::schema::SchemaIndex;

/// Vrai si l'unité de compilation courante possède un `Record` réel —
/// c'est-à-dire si `schema` porte au moins un champ, fixed ou varlena.
///
/// Définition volontairement identique à celle déjà en vigueur dans
/// `resolve_static_page` (« `SchemaIndex` toujours vide, jamais un
/// paramètre », `static_page.rs`) : ce n'est pas une nouvelle notion, c'est
/// le même fait, exposé ici comme fonction nommée plutôt que réécrit à
/// chaque site d'appel.
#[inline]
fn has_record(schema: &SchemaIndex<'_>) -> bool {
    !schema.fixed.is_empty() || !schema.varlena.is_empty()
}

/// Élimine les conditions `record.*` (`IfBool`/`IfEq`) d'un flux déjà
/// validé structurellement (`validate_ast` a retourné `Ok`), pour une
/// unité de compilation qui ne possède structurellement aucun `Record`.
///
/// No-op strict (retourne `tokens` inchangé, sans même les parcourir au-delà
/// d'une vérification triviale) si `schema` porte au moins un champ —
/// `record.*` garde alors exactement son sens actuel, résolu plus loin par
/// `resolve_and_measure`/`generate_aot_snippet` comme avant cette session.
///
/// Pour une unité sans record : chaque bloc `IfBool { .. }`/`IfEq { .. }`
/// … `[Else …]` `EndIf` est remplacé par le contenu de sa branche `Else`
/// s'il en a une, ou par rien sinon — les trois marqueurs eux-mêmes
/// (`IfBool`/`IfEq`, `Else`, `EndIf`) disparaissent toujours, ainsi que
/// l'intégralité du contenu de la branche `if` (y compris tout token
/// imbriqué : `AssetRef`, `ScriptStart`/`ScriptEnd`, etc. — s'ils
/// n'appartiennent jamais à la représentation produite, ils n'ont aucune
/// raison d'atteindre `resolve_and_measure` non plus).
///
/// # Précondition
///
/// `tokens` doit avoir passé `validate_ast` avec succès — cette fonction ne
/// revalide pas l'équilibrage `if`/`else`/`endif` (ce n'est pas sa
/// responsabilité, `validate_ast` s'en charge en amont, cf. doc de module).
/// Sur un flux mal formé (jamais produit par le pipeline réel après
/// `validate_ast`), cette fonction s'arrête simplement à la fin du flux
/// sans paniquer plutôt que d'errer sur une garantie qu'elle ne possède
/// pas — défensif, pas correctif : un flux mal formé qui parviendrait
/// jusqu'ici serait de toute façon déjà un bug ailleurs dans le pipeline.
///
/// # Allocation
///
/// Une nouvelle `Vec` est toujours allouée (même dans le cas no-op — voir
/// note ci-dessous) : contrairement à `resolve_and_measure` (`&mut
/// [FlatPageToken]`, mutation en place sur une tranche de taille fixe),
/// cette passe peut réellement RACCOURCIR le flux, ce qu'une tranche ne
/// permet structurellement pas — d'où une signature `Vec<FlatPageToken>`
/// en entrée/sortie plutôt qu'une tranche.
pub fn eliminate_recordless_conditions<'src>(
    tokens: Vec<FlatPageToken<'src>>,
    schema: &SchemaIndex<'_>,
) -> Vec<FlatPageToken<'src>> {
    if has_record(schema) {
        return tokens;
    }

    let mut output: Vec<FlatPageToken<'src>> = Vec::with_capacity(tokens.len());
    let mut iter = tokens.into_iter();

    while let Some(token) = iter.next() {
        match token {
            FlatPageToken::IfBool { .. }
            | FlatPageToken::IfEq { .. }
            | FlatPageToken::IfNeq { .. } => {
                // Condition statiquement fausse (aucun record) : on
                // saute le contenu de la branche `if` sans le pousser dans
                // `output`, puis on bascule en mode « conservation » dès
                // qu'un `Else` est rencontré — jusqu'à `EndIf`, qui referme
                // le bloc sans jamais lui-même rejoindre `output`.
                let mut keep = false;
                for inner in iter.by_ref() {
                    match inner {
                        FlatPageToken::Else => keep = true,
                        FlatPageToken::EndIf => break,
                        other if keep => output.push(other),
                        _ => {} // contenu de la branche if : ignoré
                    }
                }
            }
            other => output.push(other),
        }
    }

    output
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests_eliminate_recordless_conditions {
    use super::{eliminate_recordless_conditions, has_record};
    use crate::fragment::token::FlatPageToken;
    use crate::schema::{FieldKind, FieldSpec, SchemaIndex};

    fn empty_schema() -> SchemaIndex<'static> {
        SchemaIndex {
            fixed: &[],
            varlena: &[],
        }
    }

    fn record_field() -> FieldSpec {
        FieldSpec {
            name: "document_id".to_string(),
            kind: FieldKind::I32,
            attnum: 1,
        }
    }

    #[test]
    fn has_record_is_false_for_fully_empty_schema() {
        assert!(!has_record(&empty_schema()));
    }

    #[test]
    fn has_record_is_true_with_at_least_one_fixed_field() {
        let fixed = vec![record_field()];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };
        assert!(has_record(&schema));
    }

    /// Unité AVEC record : no-op strict, flux rigoureusement inchangé —
    /// `IfBool` reste résolu normalement plus loin par
    /// `resolve_and_measure`.
    #[test]
    fn with_record_if_bool_and_else_is_untouched() {
        let fixed = vec![FieldSpec {
            name: "is_readable".to_string(),
            kind: FieldKind::Bool,
            attnum: 1,
        }];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens.clone(), &schema);

        assert_eq!(result, tokens, "unité avec record : flux inchangé");
    }

    /// Unité AVEC record : no-op strict également pour `IfEq`.
    #[test]
    fn with_record_if_eq_and_else_is_untouched() {
        let fixed = vec![record_field()];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };
        let tokens = vec![
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

        let result = eliminate_recordless_conditions(tokens.clone(), &schema);

        assert_eq!(result, tokens, "unité avec record : flux inchangé");
    }

    /// Unité AVEC record : no-op strict également pour `IfNeq` (session `!=`).
    #[test]
    fn with_record_if_neq_and_else_is_untouched() {
        let fixed = vec![record_field()];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };
        let tokens = vec![
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

        let result = eliminate_recordless_conditions(tokens.clone(), &schema);

        assert_eq!(result, tokens, "unité avec record : flux inchangé");
    }

    /// Unité SANS record, `IfBool` SANS `else` : branche éliminée, rien
    /// n'est conservé.
    #[test]
    fn without_record_if_bool_without_else_yields_nothing() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::Static("before"),
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("CURRENT"),
            FlatPageToken::EndIf,
            FlatPageToken::Static("after"),
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("before"),
                FlatPageToken::Static("after"),
            ]
        );
    }

    /// Unité SANS record, `IfBool` AVEC `else` : seule la branche `else`
    /// est conservée.
    #[test]
    fn without_record_if_bool_with_else_keeps_else_branch_only() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(result, vec![FlatPageToken::Static("B")]);
    }

    /// Unité SANS record, `IfEq` SANS `else` : branche éliminée, rien
    /// n'est conservé — même traitement qu'`IfBool`.
    #[test]
    fn without_record_if_eq_without_else_yields_nothing() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("CURRENT"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(result, Vec::<FlatPageToken<'_>>::new());
    }

    /// Unité SANS record, `IfEq` AVEC `else` : seule la branche `else` est
    /// conservée — reproduit exactement le critère d'acceptation de
    /// session (`navigation.marius` dans `offline.offline`).
    #[test]
    fn without_record_if_eq_with_else_keeps_else_branch_only() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("<div class=\"current\">Content 1</div>"),
            FlatPageToken::Else,
            FlatPageToken::Static("<a href=\"/content/1\">Content 1</a>"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(
            result,
            vec![FlatPageToken::Static(
                "<a href=\"/content/1\">Content 1</a>"
            )]
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Session `!=` — IfNeq doit être éliminé à FAUX, exactement comme
    // IfEq, jamais à vrai (audit dédié, décision non dérivée par négation
    // logique — cf. doc de tête du module).
    // ─────────────────────────────────────────────────────────────────────

    /// Unité SANS record, `IfNeq` SANS `else` : branche éliminée, rien
    /// n'est conservé — même traitement qu'`IfEq`/`IfBool`, pas l'inverse.
    #[test]
    fn without_record_if_neq_without_else_yields_nothing() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfNeq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("CURRENT"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(
            result,
            Vec::<FlatPageToken<'_>>::new(),
            "IfNeq sans record doit être éliminé à FAUX, pas à vrai"
        );
    }

    /// Unité SANS record, `IfNeq` AVEC `else` : seule la branche `else`
    /// est conservée — même traitement qu'`IfEq`.
    #[test]
    fn without_record_if_neq_with_else_keeps_else_branch_only() {
        let schema = empty_schema();
        let tokens = vec![
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

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(result, vec![FlatPageToken::Static("B")]);
    }

    /// Le contenu de la branche éliminée est intégralement supprimé, même
    /// s'il contient d'autres tokens qu'un simple `Static` (ici un
    /// `AssetRef`) — il ne doit jamais atteindre `resolve_and_measure`.
    #[test]
    fn without_record_eliminated_branch_drops_all_inner_tokens() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("<use href=\""),
            FlatPageToken::AssetRef("sprites/utils.svg"),
            FlatPageToken::Static("\">"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(result, Vec::<FlatPageToken<'_>>::new());
    }

    /// Plusieurs blocs séquentiels sans record : chacun est traité
    /// indépendamment, exactement le cas des trois `<li>` de
    /// `navigation.marius`.
    #[test]
    fn without_record_multiple_sequential_blocks_each_resolved_independently() {
        let schema = empty_schema();
        let tokens = vec![
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 1,
            },
            FlatPageToken::Static("current-1"),
            FlatPageToken::Else,
            FlatPageToken::Static("link-1"),
            FlatPageToken::EndIf,
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id",
                literal: 2,
            },
            FlatPageToken::Static("current-2"),
            FlatPageToken::Else,
            FlatPageToken::Static("link-2"),
            FlatPageToken::EndIf,
        ];

        let result = eliminate_recordless_conditions(tokens, &schema);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("link-1"),
                FlatPageToken::Static("link-2"),
            ]
        );
    }

    /// Un template sans aucune condition, dans une unité sans record,
    /// traverse cette passe inchangé.
    #[test]
    fn without_record_no_conditions_is_unaffected() {
        let schema = empty_schema();
        let tokens = vec![FlatPageToken::Static("<p>plain</p>")];

        let result = eliminate_recordless_conditions(tokens.clone(), &schema);

        assert_eq!(result, tokens);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Exigence #7 — UnknownField reste produit, mais UNIQUEMENT pour une
    // unité qui possède un SchemaIndex non vide (donc après le no-op de
    // cette passe) et un champ réellement absent de CE schéma — jamais
    // confondu avec « aucun record du tout ».
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn unknown_field_still_rejected_when_record_exists_but_field_is_absent() {
        use crate::fragment::resolver::{ResolverError, resolve_and_measure};
        use crate::fragment::validator::validate_ast;

        // Schéma AVEC record, mais qui ne connaît pas "document_id" —
        // distinct du cas "aucun record du tout" (schéma non vide, juste
        // un champ manquant en son sein).
        let fixed = vec![FieldSpec {
            name: "title".to_string(),
            kind: FieldKind::I32,
            attnum: 1,
        }];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };

        let tokens = vec![
            FlatPageToken::IfEq {
                entity: "record",
                field: "document_id", // absent de CE schéma (seul "title" y figure)
                literal: 1,
            },
            FlatPageToken::Static("A"),
            FlatPageToken::Else,
            FlatPageToken::Static("B"),
            FlatPageToken::EndIf,
        ];

        validate_ast(&tokens).expect("structure valide, indépendamment du schéma");

        // No-op : le schéma n'est PAS vide (has_record == true), donc cette
        // passe ne touche rien — le flux atteint resolve_and_measure
        // inchangé, exactement comme avant cette session.
        let mut after_elimination = eliminate_recordless_conditions(tokens.clone(), &schema);
        assert_eq!(
            after_elimination, tokens,
            "unité avec record : cette passe ne doit rien éliminer"
        );

        let result = resolve_and_measure(
            &mut after_elimination,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0,
        );

        assert_eq!(
            result,
            Err(vec![ResolverError::UnknownField {
                entity: "record",
                field: "document_id",
            }]),
            "un champ réellement absent d'un schéma non vide doit toujours \
             produire UnknownField — cette passe ne doit jamais masquer cette \
             erreur pour une unité qui possède un record"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Exigence #8 — test bout-en-bout, aussi proche que possible du
    // pipeline réel `offline.offline` héritant de `base.marius` et
    // important `navigation.marius` : parse (Mode Page) → validate_ast →
    // eliminate_recordless_conditions (schéma vide, comme
    // `resolve_static_page`) → resolve_and_measure → generate_aot_snippet.
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn offline_pipeline_end_to_end_keeps_only_else_branch_and_never_mentions_record() {
        use crate::fragment::codegen::generate_aot_snippet;
        use crate::fragment::validator::validate_ast;
        use crate::page::parser::parse_page_tokens;
        use crate::page::token::PageSourceToken;

        // Fixture calquée sur navigation.marius (un seul <li>, structure
        // exacte du fichier réel — cf. handoff de session).
        let src = "<li>{% if record.document_id == 1 %}\
                   <div class=\"current\">Content 1</div>\
                   {% else %}\
                   <a href=\"/content/1\">Content 1</a>\
                   {% endif %}</li>";

        // 1. Parse — Mode Page, exactement le chemin emprunté par
        //    `discover_imports`/`splice_all_imports` pour un fragment importé.
        let parsed = parse_page_tokens(crate::fragment::lexer::scan(src))
            .expect("navigation.marius doit continuer à parser sans erreur");

        // Dépouille l'enveloppe Runtime — ce fragment ne contient aucun
        // opérateur de composition (block/import/static), seulement du
        // contenu Runtime, cohérent avec un <li> isolé.
        let tokens: Vec<FlatPageToken<'_>> = parsed
            .tokens
            .into_iter()
            .map(|t| match t {
                PageSourceToken::Runtime(flat) => flat,
                other => panic!("fixture attend uniquement Runtime, obtenu {other:?}"),
            })
            .collect();

        // 2. validate_ast — structure if/else/endif, indépendante du schéma.
        validate_ast(&tokens).expect("structure if/else/endif valide");

        // 3. eliminate_recordless_conditions — schéma vide, exactement
        //    celui construit par `resolve_static_page` pour STATIC_PAGES
        //    (`SchemaIndex { fixed: &[], varlena: &[] }`).
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let mut tokens = eliminate_recordless_conditions(tokens, &schema);

        // Preuve structurelle : plus aucune trace de IfEq/Else/EndIf dans
        // le flux — la branche a été éliminée avant même resolve_and_measure.
        assert!(
            !tokens.iter().any(|t| matches!(
                t,
                FlatPageToken::IfEq { .. } | FlatPageToken::Else | FlatPageToken::EndIf
            )),
            "plus aucun marqueur conditionnel ne doit subsister après élimination : {tokens:?}"
        );

        // 4. resolve_and_measure — ne doit JAMAIS échouer avec UnknownField
        //    pour "document_id" : ce champ n'existe plus dans le flux.
        let metrics = crate::fragment::resolver::resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans cette fixture"),
            |_| unreachable!("aucun AssetRef dans cette fixture"),
            0,
        )
        .expect(
            "resolve_and_measure ne doit produire aucune erreur : \
             record.document_id n'existe plus dans le flux à ce stade",
        );
        // Bonus : les métriques elles-mêmes ne comptent que le HTML de la
        // branche else conservée (aucune contribution "morte" de la
        // branche if éliminée).
        let expected_len = "<li><a href=\"/content/1\">Content 1</a></li>".len();
        assert_eq!(metrics.total_static_bytes, expected_len);

        // 5. generate_aot_snippet — le Rust généré ne doit contenir NI
        //    "record" NI le HTML de la branche if éliminée.
        let rust_code =
            generate_aot_snippet(&tokens, &schema, |_| unreachable!("aucun AssetRef ici"), "");

        assert!(
            !rust_code.contains("record"),
            "le code généré ne doit plus jamais mentionner `record` pour une \
             unité sans record :\n{rust_code}"
        );
        assert!(
            !rust_code.contains("current"),
            "la branche if (current) a été éliminée, elle ne doit apparaître \
             nulle part dans le code généré :\n{rust_code}"
        );
        assert!(
            rust_code.contains("/content/1"),
            "la branche else (lien) doit être intégralement conservée :\n{rust_code}"
        );

        println!("=== Rust généré pour offline.offline (sans record) ===\n{rust_code}");
    }
}
