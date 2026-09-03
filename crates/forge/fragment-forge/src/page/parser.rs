//! Phases 4.2–4.7 — Parser Mode Page : détection d'`extends`
//! (`detect_extends`), classification du sous-ensemble `Runtime` (symétrique
//! de `parse_tokens` Mode Fragment), reconnaissance `block`/`endblock`,
//! `static`, `extends`, et catch-all `Unsupported` fermant la grammaire.

use crate::fragment::lexer::{RawSpan, SpanKind, scan};
#[cfg(test)]
use crate::fragment::parser::parse_tokens;
use crate::fragment::token::FlatPageToken;
use crate::page::model::{
    PageBlockToken, PageComposeParseError, ParsedPageTemplate, StaticPartialRef,
};
use crate::page::token::PageSourceToken;

// =============================================================================
// Phase 4.2 — `detect_extends`
// =============================================================================
// Responsabilité unique (Document 1 §3) : décider si un fichier source est en
// Mode Page, sans parsing complet et sans dépendance à `PageSourceToken`.
//
// Invariant introduit : le mode est décidable sans parsing complet — un
// fichier est en Mode Page ssi sa toute première unité syntaxique, avant
// tout texte HTML verbatim et avant tout autre délimiteur, est un `{%`
// dont le premier `Ident` vaut `"extends"`.

/// Détermine si `source` relève du Mode Page (présence d'un `{% extends %}`
/// en tête de fichier), sans effectuer le parsing complet de la grammaire.
///
/// ─── Algorithme ────────────────────────────────────────────────────────────
///
/// Réutilise `scan` (Phase 1.2, gelé) et consomme au plus deux `RawSpan` :
/// le premier span de l'itérateur, puis, seulement s'il s'agit d'un
/// `BlockOpen` (`{%`), le second. Aucun troisième appel à `next()` n'est
/// effectué : la fonction s'arrête dès que le verdict est connu — c'est le
/// sens de « O(1) amorti » (Document 1 §3) : le coût est borné par la
/// position du premier délimiteur, jamais par la longueur totale du fichier
/// au-delà de ce point.
///
/// - Si le premier span n'est pas `BlockOpen` (fichier sans délimiteur →
///   `None` ; premier délimiteur `{{` → `ExprOpen` ; texte HTML précédant
///   `{%` → `Literal`), le fichier n'est pas en Mode Page : `false`.
/// - Si le premier span est `BlockOpen`, le second span est examiné : `true`
///   ssi c'est un `Ident` de contenu exactement `"extends"`.
///
/// ─── Ce que cette fonction NE valide PAS ───────────────────────────────────
///
/// Ne valide pas la forme complète de la déclaration `extends` (présence
/// d'un chemin, guillemets bien formés, `%}` de fermeture) : un `extends`
/// syntaxiquement malformé en tête de fichier est tout de même détecté ici
/// (`true`) et échoue plus tard dans `parse_page_tokens` (§3), pas dans
/// cette fonction.
///
/// Un fichier où du texte précède `{% extends %}` retourne `false` : la
/// première unité syntaxique n'est alors pas `BlockOpen` mais `Literal`.
/// Ce même fichier, s'il atteint `parse_page_tokens` par un autre chemin
/// d'appel, échoue avec `PageComposeParseError::ExtendsNotFirst` (Phase 4.6)
/// — produire cette erreur nommée n'est pas la responsabilité de cette
/// fonction, qui ne retourne qu'un `bool` (contrat d'appel, cf. Document 1
/// §3 : `parse_page_tokens` n'est appelée qu'après `detect_extends ==
/// true`, sauf cas du parent, admis sans cette précondition).
///
/// ─── Invariants mémoire ─────────────────────────────────────────────────────
///
/// Aucune allocation heap : `scan` n'alloue rien (Phase 1.2), et cette
/// fonction ne construit aucune structure intermédiaire. Aucune E/S : pas
/// d'appel à `std::fs`, la fonction opère exclusivement sur `source: &str`
/// déjà en mémoire.
pub fn detect_extends(source: &str) -> bool {
    let mut spans = scan(source);
    match spans.next() {
        Some(RawSpan {
            kind: SpanKind::BlockOpen,
            ..
        }) => matches!(
            spans.next(),
            Some(RawSpan {
                kind: SpanKind::Ident,
                slice: "extends"
            })
        ),
        _ => false,
    }
}

// =============================================================================
// Tests — Phase 4.2
// =============================================================================

#[cfg(test)]
mod tests_phase_4_2_detect_extends {
    use super::detect_extends;

    /// Jalon Vert — fichier sans `{%` (aucun délimiteur de bloc) → `false`.
    #[test]
    fn no_block_delimiter_returns_false() {
        assert!(!detect_extends("<div>hello {{ entity.field }}</div>"));
    }

    /// Jalon Vert — `{% extends %}` en toute première position → `true`.
    #[test]
    fn extends_at_head_returns_true() {
        assert!(detect_extends(r#"{% extends "base.marius" %}"#));
    }

    /// Jalon Vert — un autre mot-clé de bloc en tête (`{% if %}`) → `false`.
    #[test]
    fn if_at_head_returns_false() {
        assert!(!detect_extends("{% if entity.active %}yes{% endif %}"));
    }

    /// Jalon Vert — `extends` précédé de texte HTML → `false` : la première
    /// unité syntaxique est alors `Literal`, pas `BlockOpen`. Preuve directe
    /// que la fonction juge la *position*, pas la simple *présence* du
    /// mot-clé dans le fichier.
    #[test]
    fn extends_after_leading_text_returns_false() {
        assert!(!detect_extends(
            r#"<p>intro</p>{% extends "base.marius" %}"#
        ));
    }

    /// Fichier vide → `false` (premier `next()` retourne `None`, aucune E/S,
    /// aucun panic).
    #[test]
    fn empty_source_returns_false() {
        assert!(!detect_extends(""));
    }
}

// =============================================================================
// Phase 4.3 — Classifieur : sous-ensemble `Runtime`
// =============================================================================
// Responsabilité unique (roadmap §4.3) : un template Mode Page sans opérateur
// de composition produit un flux `PageSourceToken` structurellement
// équivalent à `parse_tokens` (Mode Fragment, Phase 1.3, gelé).
//
// Périmètre :
//   - Reconnaît `Static` (Literal), `Field` (`{{ }}`), `IfBool`/`EndIf`
//     (`{% if %}` / `{% endif %}`) — les quatre productions de la grammaire
//     runtime, chacune enveloppée sous `PageSourceToken::Runtime`.
//   - Ne touche pas `parse_tokens` (gelé) : aucune fonction existante
//     modifiée, aucun automate partagé — deux implémentations disjointes,
//     conformément à la frontière de domaine d'erreur actée Document 1 §0.
//   - N'implémentait pas, à la clôture de 4.3, `block`/`endblock`, `static`,
//     `extends`, ni le catch-all `Unsupported` : tout mot-clé de bloc autre
//     que `if`/`endif` — y compris `include` (absent de la grammaire Mode
//     Page par construction du type `PageSourceToken::Runtime`, cf. Phase
//     4.1) — échouait avec `PageComposeParseError::InvalidBlockSequence`.
//     Depuis, `block`/`endblock` (Phase 4.4), `static` (Phase 4.5),
//     `extends` (Phase 4.6) et le catch-all `Unsupported` avec l'exclusion
//     explicite d'`include` (Phase 4.7) sont sortis de ce catch-all — voir
//     sections dédiées ci-dessous. La grammaire des mots-clés de bloc est
//     désormais close (Document 1 clos sur ce point).
//   - `{% block %}` / `{% endblock %}` (Phase 4.4), `{% static %}` (Phase
//     4.5), `{% extends %}` (Phase 4.6) et le catch-all `Unsupported` /
//     `{% include %}` (Phase 4.7) : voir sections dédiées ci-dessous, qui
//     étendent `parse_page_block` (seule fonction modifiée à chaque fois)
//     sans toucher à ce dispatch de tête.

// =============================================================================
// Phase 4.6 — Position d'`extends` + `ExtendsNotFirst`
// =============================================================================
// Invariant introduit (roadmap §4.6) : `extends`, s'il existe, occupe
// nécessairement la première position non-whitespace du fichier — jamais
// ailleurs, jamais en double.
//
// Périmètre :
//   - `ParsedPageTemplate<'src>` (Document 1 §2.2) devient le type de sortie
//     de `parse_page_tokens` — `extends: Option<&'src str>` et
//     `tokens: Vec<PageSourceToken<'src>>`, ce dernier ne portant jamais de
//     déclaration `extends` (cf. doc du type).
//   - `parse_page_block` reconnaît désormais `extends` (branche dédiée,
//     forme jugée localement) et retourne un `PageBlockOutcome` pour laisser
//     `parse_page_tokens`, seule à connaître la position d'un span dans le
//     flux, juger la légalité de cette position.
//   - Logique de position uniquement : aucune résolution du chemin déclaré
//     (résolution d'existence : Linker, Document 2, `PageLinkError::
//     ExtendsNotFound`, hors périmètre ici).
//   - Ce diff modifie la signature publique de `parse_page_tokens`
//     (`Vec<PageSourceToken>` → `ParsedPageTemplate`) : les tests des
//     Phases 4.3/4.4/4.5 sont ajustés en conséquence (accès via `.tokens`),
//     sans changement de leurs assertions de fond — pure adaptation de
//     signature, pas une extension de portée de ce diff.

/// Construit l'AST complet (`ParsedPageTemplate<'src>`) d'un unique fichier
/// — grammaire Mode Page hors catch-all `Unsupported` (Phase 4.7).
///
/// ─── Automate ──────────────────────────────────────────────────────────────
///
/// Structurellement identique à `parse_tokens` (Phase 1.3, gelé) : même
/// dispatch sur `SpanKind` en position de tête (`Literal` → `Static`,
/// `ExprOpen` → `Field`, `BlockOpen` → sous-automate de bloc), même primitives
/// de consommation (`expect_ident`/`expect_kind`, réimplémentées ici sous
/// domaine d'erreur `PageComposeParseError` pour ne pas coupler ce
/// classifieur au type d'erreur gelé `PageParseError` — Document 1 §0).
/// Chaque token de contenu est enveloppé sous `PageSourceToken::Runtime`
/// avant d'être poussé dans l'AST — c'est la seule différence structurelle
/// avec `parse_tokens`.
///
/// ─── Position d'`extends` (Phase 4.6) ──────────────────────────────────────
///
/// `extends` n'est jamais poussé dans `tokens` : c'est une propriété du
/// fichier, portée par le champ séparé `ParsedPageTemplate::extends`
/// (Document 1 §2.2, cf. doc du type). La position de tête est vérifiée ici,
/// et seulement ici — `parse_page_block` reconnaît la forme syntaxique
/// d'`extends` mais ne sait pas, et ne doit pas savoir, à quelle position du
/// flux il a été rencontré (cf. doc de `PageBlockOutcome`). Concrètement :
/// `is_head` est vrai uniquement à la toute première itération de la boucle,
/// quel que soit le type de span rencontré ensuite ; toute déclaration
/// `extends` obtenue alors que `is_head` est faux — qu'elle apparaisse après
/// un autre token ou qu'elle soit une seconde occurrence — échoue avec
/// `PageComposeParseError::ExtendsNotFirst` (Document 1 §6, §7 : fail-fast,
/// pas d'accumulation). Un fichier sans aucun `extends` (parent) laisse ce
/// champ à `None` sans qu'aucune erreur ne soit levée — Document 1 §3.
///
/// ─── Grammaire close (Phase 4.7) ───────────────────────────────────────────
///
/// Reconnaît désormais tout mot-clé de bloc : `if`/`endif`/`block`/
/// `endblock`/`static`/`extends` chacun sous sa forme dédiée, `include`
/// explicitement exclu (`PageComposeParseError::InvalidBlockSequence`), et
/// tout le reste sous `PageSourceToken::Unsupported` (catch-all, voir doc de
/// `parse_page_block`). Aucun mot-clé de tête ne peut plus atteindre un
/// chemin d'erreur générique non informatif — Document 1 clos sur ce point.
///
/// ─── Invariants mémoire ─────────────────────────────────────────────────────
///
/// Zéro allocation de texte : chaque `&'src str` porté par un
/// `PageSourceToken` (ou par `ParsedPageTemplate::extends`) est un emprunt
/// direct sur `spans`, jamais une copie — identique au contrat de
/// `parse_tokens`. Le seul `Vec` alloué est celui de `tokens`, build-time,
/// conditionnel au premier `push` (cf. Document 1 §5) — une déclaration
/// `extends` n'y contribue jamais.
pub fn parse_page_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<ParsedPageTemplate<'src>, PageComposeParseError> {
    let mut iter = spans.peekable();
    let mut tokens = Vec::new();
    let mut extends: Option<&'src str> = None;
    let mut is_head = true;

    while let Some(span) = iter.next() {
        let head = is_head;
        is_head = false;

        match span.kind {
            // Texte HTML verbatim → Static directement, enveloppé Runtime.
            SpanKind::Literal => {
                tokens.push(PageSourceToken::Runtime(FlatPageToken::Static(span.slice)));
            }

            // `{{ entity.field }}` → Field, enveloppé Runtime.
            SpanKind::ExprOpen => {
                tokens.push(PageSourceToken::Runtime(parse_page_expr(&mut iter)?));
            }

            // `{% keyword … %}` → IfBool | EndIf | BlockOpen | BlockEnd |
            // Static(..) | Extends(path). `parse_page_block` décide de la
            // forme (`PageBlockOutcome`) ; seule cette fonction sait si le
            // span de tête `{%` consommé était le tout premier du fichier,
            // donc seule elle peut juger la position d'un `Extends` (Phase
            // 4.6 : voir doc ci-dessus).
            SpanKind::BlockOpen => match parse_page_block(&mut iter)? {
                PageBlockOutcome::Extends(path) => {
                    if !head {
                        return Err(PageComposeParseError::ExtendsNotFirst);
                    }
                    extends = Some(path);
                }
                PageBlockOutcome::Token(token) => tokens.push(token),
            },

            // Tout autre span en position initiale est une erreur
            // structurelle : ExprClose, BlockClose, Ident, Punct ne peuvent
            // pas ouvrir un token, au même titre que dans `parse_tokens`.
            got => {
                return Err(PageComposeParseError::UnexpectedToken {
                    expected: "Literal | ExprOpen | BlockOpen",
                    got,
                });
            }
        }
    }

    Ok(ParsedPageTemplate { extends, tokens })
}

// =============================================================================
// Phase 4.4 — Reconnaissance `{% block %}` / `{% endblock %}`
// =============================================================================
// Responsabilité unique (roadmap §4.4) : les marqueurs de composition
// `{% block name %}` / `{% endblock %}` sont représentables dans l'AST Mode
// Page sans être résolus (correspondance parent/enfant, Document 2) ni
// validés (appariement, absence d'imbrication, Document 2) — permissivité
// délibérée déjà actée Document 1 §4/§6.
//
// Invariant introduit : un `{% block name %}` produit toujours
// `PageSourceToken::Block(PageBlockToken::BlockOpen { name })`, un
// `{% endblock %}` toujours `PageSourceToken::Block(PageBlockToken::BlockEnd)`
// — sans vérification d'appariement ni de nom à la fermeture, y compris pour
// des blocs imbriqués. Un fichier à blocs mal appariés ou imbriqués n'est
// donc PAS rejeté par cette phase : c'est une propriété positive de ce
// diff, prouvée par le test `nested_blocks_parse_succeeds` ci-dessous, pas
// une lacune à corriger ici.
//
// Périmètre : une seule fonction modifiée (`parse_page_block`), une seule
// branche de l'automate ajoutée (`block` | `endblock` dans son `match`).
// Le dispatch de tête de `parse_page_tokens` (Phase 4.3) est ajusté en
// conséquence (propagation directe du `PageSourceToken` déjà enveloppé),
// sans ajout de nouvelle branche de `match` sur `SpanKind` — `BlockOpen`
// reste l'unique point d'entrée vers `parse_page_block`, comme en 4.3.
// `extends` (4.6) et le catch-all `Unsupported` (4.7) restent hors
// périmètre de ce diff 4.4 : ils continuaient, à l'époque, d'échouer via
// `PageComposeParseError::InvalidBlockSequence`. `{% static %}` est sorti de
// ce même catch-all en Phase 4.5 (section dédiée plus bas) — seul `extends`
// y échoue encore à ce stade.

// ─── Parseurs de sous-séquences (domaine `PageComposeParseError`) ────────────
//
// Symétriques de `parse_expr`/`parse_block` (Phase 1.3, gelées) : même
// pattern de consommation, domaine d'erreur `PageComposeParseError` au lieu
// de `PageParseError` — duplication délibérée plutôt que généricité sur le
// type d'erreur, pour ne pas coupler le classifieur Mode Page au type
// d'erreur gelé du Parser Mode Fragment (Document 1 §0).

/// Consomme `Ident(entity) Punct(.) Ident(field) ExprClose` et produit
/// `FlatPageToken::Field`. Précondition : `ExprOpen` vient d'être consommé
/// par `parse_page_tokens`.
fn parse_page_expr<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let entity = expect_ident_page(iter, "Ident(entity)")?;
    expect_kind_page(iter, SpanKind::Punct, "Punct('.')")?;
    let field = expect_ident_page(iter, "Ident(field)")?;
    expect_kind_page(iter, SpanKind::ExprClose, "ExprClose('}}')")?;
    Ok(FlatPageToken::Field { entity, field })
}

/// Résultat de `parse_page_block` (Phase 4.6). Distingue un token de contenu
/// ordinaire, prêt à être poussé dans `ParsedPageTemplate::tokens`, d'une
/// déclaration `{% extends "path" %}`, qui n'est **jamais** poussée dans
/// `tokens` (cf. doc de `ParsedPageTemplate`).
///
/// Cette distinction est nécessaire parce que la position d'`extends`
/// (tête de fichier ou non) ne peut être jugée que par `parse_page_tokens`
/// — seule fonction qui observe l'ordre des spans de tête au fil de son
/// itération. `parse_page_block` reconnaît la forme syntaxique d'`extends`
/// (grammaire), mais ne connaît pas, et ne doit pas se voir déléguer, sa
/// position (une question de grammaire mono-fichier distincte, cf. doc de
/// `PageComposeParseError::ExtendsNotFirst`) : lui faire porter ce jugement
/// dupliquerait, à l'échelle d'une seule fonction, l'état que
/// `parse_page_tokens` maintient déjà (`is_head`) — deux sources de vérité
/// pour une même position, un candidat naturel à la divergence.
enum PageBlockOutcome<'src> {
    /// Token de contenu ordinaire — `if`/`endif`/`block`/`endblock`/`static`.
    Token(PageSourceToken<'src>),
    /// Chemin brut d'une déclaration `{% extends path %}`, syntaxiquement
    /// bien formée. La légalité de sa position est jugée par l'appelant.
    Extends(&'src str),
}

/// Consomme `Ident(keyword) … BlockClose` et produit le `PageBlockOutcome`
/// correspondant. Précondition : `BlockOpen` vient d'être consommé par
/// `parse_page_tokens`.
///
/// Portée Phase 4.7 : reconnaît `if`/`endif` (Phase 4.3, logique inchangée),
/// `block`/`endblock` (Phase 4.4, logique inchangée), `static` (Phase 4.5,
/// logique inchangée), `extends` (Phase 4.6, logique inchangée), `include`
/// (exclusion explicite, introduite ici) et le catch-all `Unsupported`
/// (introduit ici) pour tout le reste. Cette fonction est désormais totale
/// sur la grammaire lexicale des mots-clés de bloc : aucun `Ident` de tête
/// ne peut plus atteindre un chemin d'erreur générique non informatif —
/// Document 1 clos sur ce point (roadmap §4.7).
///
/// ─── Pourquoi le type de retour change : `PageBlockOutcome`, plus
///     `PageSourceToken` directement ─────────────────────────────────────────
///
/// `if`/`endif`/`block`/`endblock`/`static` restent enveloppés exactement
/// comme en Phase 4.5 (`PageSourceToken`, lui-même sous `Runtime` ou
/// `Block`/`Static` selon le cas). `extends` seul n'a pas d'enveloppe
/// `PageSourceToken` : ce n'est pas un token de contenu, c'est un champ de
/// `ParsedPageTemplate` (cf. doc du type) — `PageBlockOutcome::Extends` le
/// fait remonter à l'appelant sans le faire transiter par `PageSourceToken`,
/// ce qui rendrait par construction impossible de le pousser par erreur
/// dans `tokens`.
///
/// ─── Invariant introduit en Phase 4.6 : zéro E/S sur `extends`,
///     forme jugée ici, position jugée par l'appelant ─────────────────────────
///
/// Comme `static` (Phase 4.5), la branche `extends` capture `path` tel quel
/// — aucun appel `std::fs`, aucune vérification d'existence. Elle vérifie en
/// revanche la forme (`Ident(path) BlockClose`, sinon `UnexpectedToken`/
/// `UnexpectedEof`) : c'est un jugement de grammaire mono-fichier, dans le
/// domaine de cette fonction. Ce que cette fonction ne vérifie jamais, y
/// compris pour `extends` : la position dans le fichier — jugée exclusivement
/// par `parse_page_tokens` via `PageComposeParseError::ExtendsNotFirst`
/// (cf. doc de `PageBlockOutcome`).
///
/// ─── Invariant introduit en Phase 4.5 : zéro E/S sur `static` ────────────
///
/// La branche `static` capture `original_path` tel quel — aucun appel
/// `std::fs`, aucune vérification d'existence, aucune résolution de chemin
/// relatif. Un chemin syntaxiquement bien formé mais inexistant sur disque
/// produit un `Ok` identique à un chemin existant : l'existence est une
/// propriété du Linker (`PageLinkError::StaticFileNotFound`, Document 2),
/// pas du Parser (Document 1 §5/§6). Cf. `static_path_parses_without_touching_filesystem`.
///
/// ─── Permissivité délibérée sur l'imbrication (Document 1 §4, §6) ─────────
///
/// Cette fonction ne maintient aucune pile de blocs ouverts : un
/// `{% block %}` rencontré alors qu'un autre est déjà ouvert est accepté
/// sans distinction — l'appariement correct et l'absence d'imbrication ne
/// sont pas des garanties de sortie du Parser (cf. Document 1 §6). Juger
/// l'imbrication exige un état de pile que seule la Validation (Document 2,
/// `PageValidationError::NestedBlock`) construit ; le dupliquer ici
/// recréerait la fusion syntaxe/sémantique que le Parser doit éviter par
/// construction.
fn parse_page_block<'src, I>(iter: &mut I) -> Result<PageBlockOutcome<'src>, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let keyword = expect_ident_page(
        iter,
        "keyword (if | endif | block | endblock | static | extends | asset | script | endscript)",
    )?;

    match keyword {
        "if" => {
            let raw = expect_ident_page(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted_page(raw)?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::IfBool { entity, field },
            )))
        }
        "endif" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::EndIf,
            )))
        }
        "block" => {
            let name = expect_ident_page(iter, "Ident(name)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Block(
                PageBlockToken::BlockOpen { name },
            )))
        }
        "endblock" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Block(
                PageBlockToken::BlockEnd,
            )))
        }
        // `{% static path %}` (Phase 4.5) : capture brute, zéro E/S. `path`
        // est l'`Ident` de bloc nu retourné par le scanner — pas de
        // dépouillement de guillemets (cf. doc `PageSourceToken::Static`).
        // Aucune vérification d'existence : c'est le rôle du Linker
        // (`PageLinkError::StaticFileNotFound`, Document 2), pas celui de
        // cette fonction.
        "static" => {
            let original_path = expect_ident_page(iter, "Ident(path)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Static(
                StaticPartialRef { original_path },
            )))
        }
        // `{% extends path %}` (Phase 4.6) : capture brute, zéro E/S, même
        // convention non-quotée que `static` (symétrie délibérée — cf. doc
        // `StaticPartialRef::original_path`). Forme jugée ici ; position
        // jugée par `parse_page_tokens` (cf. doc de `PageBlockOutcome`).
        "extends" => {
            let path = expect_ident_page(iter, "Ident(path)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Extends(path))
        }
        // `{% include path %}` (Phase 4.7) : exclusion explicite du catch-all
        // `Unsupported` ci-dessous — roadmap §4.7 exige `∉ {if, endif,
        // include, extends, block, endblock, static}` pour la branche
        // par défaut. `include` n'est pas « non supporté » au sens
        // d'`Unsupported` (un mot-clé dont la grammaire est inconnue de ce
        // Parser) : sa grammaire *est* connue (Mode Fragment, `parse_block`,
        // gelé) — il est structurellement absent de la grammaire Mode Page
        // par construction du type (`PageSourceToken::Runtime` n'émet
        // jamais `FlatPageToken::StaticInclude`, cf. doc de cette variante).
        // Le confondre avec `Unsupported` ferait porter à la Validation
        // (Document 2) la charge de distinguer, au sein d'un même verdict
        // « non supporté », un mot-clé simplement pas encore implémenté
        // (`for`) d'un mot-clé délibérément interdit dans ce mode
        // (`include`, qui a un équivalent : `static`) — une confusion que
        // Document 1 §0 proscrit explicitement (fusion syntaxe/sémantique).
        // Bras explicite plutôt que laissé retomber dans le catch-all : sans
        // lui, `include` migrerait silencieusement vers `Unsupported` dès
        // que le catch-all serait ajouté — un effet de bord de ce diff, pas
        // une décision prise consciemment.
        "include" => Err(PageComposeParseError::InvalidBlockSequence),
        // `{% asset key %}` (spec `marius-assets-specification.md` §9) :
        // à la différence d'`include` (Mode Fragment exclusif, cf. bras
        // ci-dessus), `asset` est valide dans les deux modes — l'exemple de
        // référence de la spec §9 (balises `<link>` dans un layout) est
        // typiquement du Mode Page. Enveloppé sous `Runtime` comme `if`/
        // `endif` : c'est un token de contenu ordinaire pour ce Parser,
        // résolu plus tard par `resolve_and_measure`/`generate_aot_snippet`.
        "asset" => {
            let key = expect_ident_page(iter, "Ident(key)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::AssetRef(key),
            )))
        }
        // `{% script %}` / `{% endscript %}` (session dédiée au hoisting) :
        // valides dans les deux modes, comme `asset` juste au-dessus —
        // enveloppés sous `Runtime` comme `if`/`endif`, ce sont des tokens
        // de contenu ordinaires pour ce Parser. La distinction Page/Fragment
        // isolé (hisser ou laisser en No-Op) ne se joue jamais ici, ni même
        // dans ce crate — elle vit exclusivement dans `build.rs`.
        "script" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::ScriptStart,
            )))
        }
        "endscript" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::ScriptEnd,
            )))
        }
        // Catch-all (Phase 4.7, roadmap §4.7, Document 1 §2.1) : tout
        // mot-clé de bloc hors grammaire déjà reconnue (`for`, `join`,
        // `where`, `filter`, `group`, ou tout mot-clé inconnu) est capturé
        // sous `Unsupported`, jamais rejeté à ce stade — Document 1 §6 :
        // « jamais silencieusement ignoré ni rejeté ». Cette branche clôt la
        // grammaire de `parse_page_block` : plus aucun mot-clé ne peut
        // atteindre un chemin d'erreur générique non informatif.
        //
        // ─── Arité non contrainte (0, 1 ou N tokens avant `%}`) ────────────
        //
        // Ce Parser ne connaît pas la grammaire de ces mots-clés — il ignore
        // si `for` attend `item in items` (3 tokens), `filter` un unique
        // prédicat, ou si un mot-clé inconnu n'attend rien du tout. Il ne
        // doit donc jamais échouer sur l'arité : tous les tokens jusqu'à
        // `BlockClose` sont consommés sans jugement de forme, en une seule
        // passe, sans retour arrière — cohérent avec l'automate `O(n)` sans
        // backtracking de ce module (cf. doc du fichier d'architecture).
        //
        // ─── `tail` : premier token suivant le mot-clé, ou vide ───────────
        //
        // `tail` capture le premier `Ident` rencontré après le mot-clé
        // (`""` si `BlockClose` suit immédiatement) — un indice minimal,
        // suffisant pour que la Validation (Document 2) nomme le rejet
        // (`ForLoopDetected`, `RelationalKeyword`, etc.) sans que ce Parser
        // ne tente de reconstituer la totalité du contenu du bloc. Tout
        // token additionnel au-delà du premier est consommé mais non
        // conservé : cette consommation ne vise qu'à resynchroniser
        // l'automate sur `BlockClose`, pas à préserver le contenu — zéro
        // allocation (`tail` reste un emprunt direct sur `spans`, jamais une
        // concaténation).
        keyword => {
            let mut tail = "";
            let mut seen_first = false;
            loop {
                match iter.next() {
                    Some(span) if span.kind == SpanKind::BlockClose => break,
                    Some(span) => {
                        if !seen_first {
                            tail = span.slice;
                            seen_first = true;
                        }
                    }
                    None => return Err(PageComposeParseError::UnexpectedEof),
                }
            }
            Ok(PageBlockOutcome::Token(PageSourceToken::Unsupported {
                keyword,
                tail,
            }))
        }
    }
}

// ─── Primitives de consommation (domaine `PageComposeParseError`) ────────────

/// Consomme le span suivant et retourne sa slice si c'est un `Ident`.
#[inline]
fn expect_ident_page<'src, I>(
    iter: &mut I,
    expected: &'static str,
) -> Result<&'src str, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == SpanKind::Ident => Ok(span.slice),
        Some(span) => Err(PageComposeParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageComposeParseError::UnexpectedEof),
    }
}

/// Consomme le span suivant et vérifie qu'il a le `kind` attendu.
#[inline]
fn expect_kind_page<'src, I>(
    iter: &mut I,
    kind: SpanKind,
    expected: &'static str,
) -> Result<(), PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == kind => Ok(()),
        Some(span) => Err(PageComposeParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageComposeParseError::UnexpectedEof),
    }
}

/// Coupe `"entity.field"` sur le premier `.` et retourne `("entity",
/// "field")`. Symétrique de `split_dotted` (Phase 1.3, gelée).
#[inline]
fn split_dotted_page(raw: &str) -> Result<(&str, &str), PageComposeParseError> {
    raw.find('.')
        .map(|i| (&raw[..i], &raw[i + 1..]))
        .ok_or(PageComposeParseError::InvalidBlockSequence)
}

// =============================================================================
// Tests — Phase 4.3
// =============================================================================

#[cfg(test)]
mod tests_phase_4_3_parse_page_tokens_runtime_subset {
    use super::{
        FlatPageToken, PageComposeParseError, PageSourceToken, parse_page_tokens, parse_tokens,
        scan,
    };

    /// Dépouille l'enveloppe `Runtime` d'un AST Mode Page pour comparaison
    /// directe avec la sortie de `parse_tokens` (Mode Fragment). Panique si
    /// une variante non-`Runtime` apparaît : les fixtures de ce module sont
    /// construites pour ne jamais en émettre (aucun opérateur de composition
    /// n'y figure).
    fn strip_runtime_envelope(tokens: Vec<PageSourceToken<'_>>) -> Vec<FlatPageToken<'_>> {
        tokens
            .into_iter()
            .map(|t| match t {
                PageSourceToken::Runtime(inner) => inner,
                other => panic!(
                    "fixture de non-régression 4.3 attend uniquement Runtime, obtenu {other:?}"
                ),
            })
            .collect()
    }

    /// Jalon Vert — fixture `Static` seul : un template sans aucun opérateur
    /// produit, dépouillé de son enveloppe `Runtime`, exactement le même AST
    /// que `parse_tokens` sur la même source.
    #[test]
    fn runtime_subset_matches_parse_tokens_static_only() {
        let src = "<div>plain html</div>";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — fixture `Field` seul : `{{ entity.field }}` produit la
    /// même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_field_only() {
        let src = "{{ user.name }}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — fixture `IfBool`/`EndIf` : un bloc conditionnel complet
    /// produit la même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_if_endif() {
        let src = "{% if user.active %}yes{% endif %}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — un mot-clé structurellement exclu de la grammaire Mode
    /// Page (`include`) échoue explicitement plutôt que d'être
    /// silencieusement accepté ou ignoré — comportement documenté, pas un
    /// effet de bord. Ni `extends` (sorti en Phase 4.6, position jugée par
    /// `ExtendsNotFirst`) ni `for` (capturé sous `Unsupported` depuis le
    /// catch-all de la Phase 4.7, cf. `tests_phase_4_7_unsupported_catch_all`)
    /// n'illustrent plus cet invariant : `include` est désormais le seul
    /// mot-clé qui échoue encore ici, de façon définitive — cf. doc de
    /// `parse_page_block` (Phase 4.7, exclusion explicite du catch-all).
    #[test]
    fn composition_keyword_out_of_scope_fails_explicitly() {
        let src = r#"{% include fragment.html %}"#;
        let result = parse_page_tokens(scan(src));
        assert_eq!(result, Err(PageComposeParseError::InvalidBlockSequence));
    }
}

// =============================================================================
// Tests — Phase 4.4
// =============================================================================

#[cfg(test)]
mod tests_phase_4_4_block_endblock {
    use super::{FlatPageToken, PageBlockToken, PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert — template à 1 bloc top-level : `{% block name %}` produit
    /// exactement `BlockOpen { name }`, `{% endblock %}` produit exactement
    /// `BlockEnd`, le contenu intermédiaire reste `Runtime` inchangé.
    #[test]
    fn single_top_level_block_produces_block_open_and_block_end() {
        let src = "{% block header %}content{% endblock %}";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sur un bloc bien formé");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "header" }),
                PageSourceToken::Runtime(FlatPageToken::Static("content")),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
            ]
        );
    }

    /// Jalon Vert — blocs imbriqués : preuve explicite de non-rejet à ce
    /// stade (Document 1 §4/§6 — l'appariement et l'absence d'imbrication
    /// ne sont pas des garanties du Parser). Le classifieur n'inspecte
    /// aucune pile d'état ; il reproduit fidèlement chaque marqueur
    /// rencontré, y compris quand un `{% block %}` s'ouvre alors qu'un autre
    /// est déjà ouvert.
    #[test]
    fn nested_blocks_parse_succeeds() {
        let src = "{% block outer %}{% block inner %}x{% endblock %}{% endblock %}";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sur des blocs imbriqués");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
                PageSourceToken::Runtime(FlatPageToken::Static("x")),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 4.5
// =============================================================================

#[cfg(test)]
mod tests_phase_4_5_static {
    use super::{FlatPageToken, PageSourceToken, StaticPartialRef, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.5) — un chemin syntaxiquement valide mais
    /// absent du disque est accepté : cette fonction ne fait aucune E/S,
    /// donc l'existence réelle du fichier n'a aucune incidence sur le
    /// résultat. Aucune fixture sur disque n'est créée pour ce test — la
    /// chaîne de chemin est arbitraire, exactement comme le prescrit la
    /// roadmap ; c'est la preuve positive de l'absence d'E/S, pas seulement
    /// une absence de panique.
    #[test]
    fn static_path_parses_without_touching_filesystem() {
        let src = "before{% static this/path/does/not/exist.html %}after";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sans vérifier l'existence du fichier");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Runtime(FlatPageToken::Static("before")),
                PageSourceToken::Static(StaticPartialRef {
                    original_path: "this/path/does/not/exist.html",
                }),
                PageSourceToken::Runtime(FlatPageToken::Static("after")),
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 4.6
// =============================================================================

#[cfg(test)]
mod tests_phase_4_6_extends_position {
    use super::{FlatPageToken, PageComposeParseError, PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.6) — `extends` en tête de fichier est capturé
    /// dans `ParsedPageTemplate::extends`, et n'apparaît jamais dans
    /// `tokens` (cf. doc du type : `extends` est un champ séparé, pas une
    /// variante de `PageSourceToken`).
    #[test]
    fn extends_at_head_is_captured_and_absent_from_tokens() {
        let src = "{% extends base.marius %}content";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir avec extends en tête");

        assert_eq!(actual.extends, Some("base.marius"));
        assert_eq!(
            actual.tokens,
            vec![PageSourceToken::Runtime(FlatPageToken::Static("content"))]
        );
    }

    /// Jalon Vert (roadmap §4.6) — `extends` rencontré après un autre token
    /// (ici un `Static` de type HTML verbatim) échoue avec
    /// `ExtendsNotFirst` : la position de tête est une propriété du fichier
    /// entier, pas seulement du premier bloc `{% %}` rencontré.
    #[test]
    fn extends_after_a_static_token_fails_with_extends_not_first() {
        let src = "leading text{% extends base.marius %}";

        let result = parse_page_tokens(scan(src));

        assert_eq!(result, Err(PageComposeParseError::ExtendsNotFirst));
    }

    /// Jalon Vert (roadmap §4.6) — un fichier parent, sans aucun `extends`,
    /// réussit avec `extends == None` : l'absence de la déclaration n'est
    /// pas une erreur, Document 1 §3 l'admet explicitement comme cas normal.
    #[test]
    fn absent_extends_on_parent_file_succeeds_with_none() {
        let src = "{% block header %}content{% endblock %}";

        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens doit réussir sans extends");

        assert_eq!(actual.extends, None);
    }
}

// =============================================================================
// Tests — Phase 4.7
// =============================================================================

#[cfg(test)]
mod tests_phase_4_7_unsupported_catch_all {
    use super::{PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.7) — paramétré sur `for`, `join`, `where`,
    /// `filter`, `group`, et un mot-clé arbitraire inconnu : chacun produit
    /// `Unsupported { keyword, .. }` avec le bon `keyword`, jamais un rejet
    /// générique (`InvalidBlockSequence`) ni un rejet silencieux.
    #[test]
    fn unsupported_catch_all_captures_arbitrary_keywords() {
        let keywords = ["for", "join", "where", "filter", "group", "frobnicate"];

        for keyword in keywords {
            let src = format!("{{% {keyword} arg %}}");
            let actual = parse_page_tokens(scan(&src)).unwrap_or_else(|e| {
                panic!("mot-clé {keyword:?} doit être capturé, pas rejeté (erreur : {e:?})")
            });

            assert_eq!(
                actual.tokens.len(),
                1,
                "mot-clé {keyword:?} : un seul token attendu dans le flux"
            );
            match actual.tokens[0] {
                PageSourceToken::Unsupported {
                    keyword: got_keyword,
                    ..
                } => assert_eq!(got_keyword, keyword, "keyword capturé incorrect"),
                other => panic!("mot-clé {keyword:?} : attendu Unsupported, obtenu {other:?}"),
            }
        }
    }
}
