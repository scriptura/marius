// crates/forge/fragment-forge/src/page/blocks.rs

//! Phases 5.2–5.4 — `collect_blocks` : appariement par pile des
//! `BlockOpen`/`BlockEnd` d'un fichier admis en arène, production des
//! `NamedBlockRange`, fail-slow (imbrication détectée, mots-clés non
//! supportés mappés vers `PageValidationError`).

#[cfg(test)]
use crate::fragment::token::FlatPageToken;
use crate::page::model::{NamedBlockRange, PageBlockToken, PageValidationError, TemplateId};
use crate::page::token::PageSourceToken;

// =============================================================================
// PHASE 5.2 — `collect_blocks` : cas non imbriqué (Document 2 §3)
// =============================================================================
// Responsabilité (roadmap §5.2) : apparier, par pile, les `BlockOpen`/
// `BlockEnd` d'**un** fichier déjà admis en arène, et produire les
// `NamedBlockRange` correspondantes.
//
// ─── Choix explicite sur les catégories de cas hors périmètre 5.2 ─────────
// (roadmap §5.2 : « à choisir explicitement, pas laisser un todo! silencieux »)
//
//   1. `PageSourceToken::Unsupported` (mots-clés `for`/`join`/`where`/…) :
//      à ce stade (5.2/5.3), traité comme du contenu opaque par la branche
//      `_` — ignoré par la boucle d'appariement, ne produit aucune erreur.
//      Retour `Ok` systématique tant qu'aucun bloc n'est mal apparié : c'est
//      la variante « retour Ok uniquement pour l'instant » explicitement
//      choisie parmi les deux proposées par la roadmap. La Phase 5.4
//      ci-dessous remplace cette branche par le mapping nommé vers
//      `PageValidationError::ForLoopDetected`/`RelationalKeyword`.
//
//   2. Profondeur d'imbrication > 1 : couverte depuis la Phase 5.3
//      ci-dessous (`NestedBlock`) — plus un point hors périmètre depuis ce
//      diff.
//
// ─── Point ouvert, non tranché par ce diff ─────────────────────────────────
//
//   Un flux structurellement mal formé au sens de l'appariement lui-même —
//   `BlockEnd` sans `BlockOpen` correspondant, ou `BlockOpen` non refermé en
//   fin de flux — n'est PAS un cas couvert par le chemin heureux testé ici,
//   et n'est représenté par aucune variante existante de
//   `PageValidationError` (`NonBoolIfCondition`, `ForLoopDetected`,
//   `RelationalKeyword`, `NestedBlock` : aucune ne nomme un déséquilibre
//   structurel). Introduire une nouvelle variante pour ce cas dépasserait le
//   périmètre de cette phase (« ne préparer aucun comportement relevant des
//   phases ultérieures »). Choix retenu : un `panic!` documenté, nommé,
//   assorti d'un message explicite — jamais un `todo!`/`unimplemented!` muet
//   — sur une entrée que les fixtures testées à ce stade ne produisent
//   jamais. À trancher explicitement dans une session ultérieure, au même
//   titre que le point ouvert déjà signalé au Document 2 §6.1.
//
// =============================================================================
// PHASE 5.3 — `collect_blocks` : détection `NestedBlock` (Document 2 §3)
// =============================================================================
// Extension de 5.2 (roadmap §5.3) : une seule condition ajoutée dans la
// boucle existante, aucune restructuration de la pile. Invariant introduit :
// l'imbrication est rejetée nommément, jamais acceptée comme plage valide.
//
// ─── Mécanisme ──────────────────────────────────────────────────────────
//
//   La pile LIFO appariait déjà correctement n'importe quelle profondeur
//   (propriété algorithmique de 5.2, documentée dans son commentaire de
//   tête). Cette phase n'ajoute donc aucune capacité d'appariement — elle
//   ajoute une *interdiction* : si `open_stack` est déjà non-vide au moment
//   d'empiler un nouveau `BlockOpen`, ce `BlockOpen` est en position
//   imbriquée, ce qui produit `PageValidationError::NestedBlock { name }`
//   (`name` du bloc imbriqué fautif, pas du bloc englobant — c'est
//   l'occurrence la plus profonde qui viole la contrainte de platitude).
//
// ─── Fail-slow, pas fail-fast ────────────────────────────────────────────
//
//   L'empilement continue malgré l'erreur détectée (`open_stack.push`
//   n'est jamais court-circuité) : la boucle va jusqu'au bout du flux,
//   accumulant une erreur par `BlockOpen` en position imbriquée. Ce choix
//   anticipe la vérification fail-slow prescrite en Phase 5.4 (« 2 erreurs
//   simultanées → `Vec` de longueur 2 ») sans l'implémenter par avance :
//   c'est une conséquence directe et minimale de « ne jamais interrompre la
//   boucle sur une erreur nommée », pas un branchement additionnel préparé
//   pour 5.4.
//
// ─── Pas de sortie mixte succès/erreur ───────────────────────────────────
//
//   `ranges` continue d'être peuplé même en présence d'erreurs (nécessaire
//   pour que chaque `BlockEnd` trouve un `start` à dépiler), mais n'est
//   jamais retourné si `errors` est non vide : la fonction retourne
//   `Err(errors)` ou `Ok(ranges)`, jamais les deux à la fois. Les plages
//   calculées en présence d'imbrication sont donc délibérément jetées, pas
//   exposées comme un résultat partiellement fiable.
//
// =============================================================================
// PHASE 5.4 — `collect_blocks` : `ForLoopDetected` / `RelationalKeyword` (Document 2 §3)
// =============================================================================
// Extension de 5.2/5.3 (roadmap §5.4) : une seule branche de `match` ajoutée,
// aucune logique de pile touchée. Invariant introduit : mapping total et
// nommé entre mot-clé `Unsupported` et erreur de validation — plus aucun
// mot-clé `Unsupported` ne peut traverser `collect_blocks` sans produire une
// erreur nommée (le point 1 de la doc de tête, ci-dessus, est donc clos).
//
// ─── Règle du mapping ──────────────────────────────────────────────────────
//
//   `PageSourceToken::Unsupported { keyword, .. }` :
//     - `keyword == "for"`      → `PageValidationError::ForLoopDetected`
//     - tout autre `keyword`    → `PageValidationError::RelationalKeyword { keyword }`
//
//   Ce n'est pas une énumération explicite des mots-clés relationnels connus
//   (`join`/`where`/`filter`/`group`) suivie d'un troisième cas silencieux :
//   c'est un mapping *total* sur le seul axe qui compte ici — `for` est
//   distingué parce que `PageValidationError` lui réserve une variante sans
//   charge utile, tout le reste (relationnel connu ou mot-clé futur non
//   encore nommé par la grammaire, cf. le catch-all Phase 4.7 déjà total sur
//   `keyword: &str` arbitraire) tombe dans `RelationalKeyword`, qui porte le
//   `keyword` reçu tel quel. Aucun `keyword` ne peut donc rester non
//   catégorisé — propriété vérifiée par construction (deux branches
//   exhaustives sur un `bool`), pas par une liste à maintenir.
//
// ─── Fail-slow, orthogonal à `NestedBlock` ─────────────────────────────────
//
//   Cette branche ne fait pas partie de la pile d'appariement (`open_stack`
//   n'est ni lu ni modifié) : un mot-clé `Unsupported` peut coexister avec un
//   bloc imbriqué dans le même flux, chacun poussant sa propre erreur dans
//   `errors` sans interférence — même politique fail-slow que 5.3, sur un axe
//   de validation indépendant.
pub fn collect_blocks<'src>(
    template: TemplateId,
    tokens: &[PageSourceToken<'src>],
) -> Result<Vec<NamedBlockRange<'src>>, Vec<PageValidationError<'src>>> {
    let mut open_stack: Vec<(&'src str, usize)> = Vec::new();
    let mut ranges = Vec::new();
    let mut errors = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        match token {
            // Ouverture : empile `(name, start)`. `start` pointe juste après
            // le marqueur `BlockOpen` lui-même — la plage couvre le contenu
            // du bloc, jamais ses délimiteurs (convention actée par la doc
            // de `NamedBlockRange`). Une pile déjà non-vide à cet instant
            // signale une imbrication (Phase 5.3) : erreur accumulée,
            // empilement néanmoins poursuivi (fail-slow, cf. doc de tête).
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                if !open_stack.is_empty() {
                    errors.push(PageValidationError::NestedBlock { name });
                }
                open_stack.push((name, index + 1));
            }
            // Fermeture : dépile et matérialise la plage `[start, index)`,
            // `index` (position du `BlockEnd`) exclusif — même convention.
            PageSourceToken::Block(PageBlockToken::BlockEnd) => {
                let (name, start) = open_stack.pop().unwrap_or_else(|| {
                    panic!(
                        "collect_blocks (Phase 5.2) : BlockEnd sans BlockOpen \
                         correspondant à l'index {index} — cas mal formé hors \
                         périmètre du chemin heureux, non représenté par \
                         PageValidationError à ce stade (voir doc de tête)"
                    )
                });
                ranges.push(NamedBlockRange {
                    name,
                    template,
                    start,
                    end: index,
                });
            }
            // Mot-clé de grammaire non supporté (Phase 5.4, cf. doc de tête) :
            // mapping total vers l'erreur de validation nommée
            // correspondante. N'interagit pas avec `open_stack` — orthogonal
            // à l'appariement de blocs, fail-slow au même titre que
            // `NestedBlock` ci-dessus.
            PageSourceToken::Unsupported { keyword, .. } => {
                if *keyword == "for" {
                    errors.push(PageValidationError::ForLoopDetected);
                } else {
                    errors.push(PageValidationError::RelationalKeyword { keyword });
                }
            }
            // Tout le reste (`Runtime`, `Static`) est du contenu opaque du
            // point de vue de l'appariement de blocs — ni poussé ni dépilé.
            _ => {}
        }
    }

    assert!(
        open_stack.is_empty(),
        "collect_blocks (Phase 5.2) : {} bloc(s) BlockOpen non refermé(s) en \
         fin de flux — cas mal formé hors périmètre du chemin heureux, non \
         représenté par PageValidationError à ce stade (voir doc de tête)",
        open_stack.len()
    );

    if errors.is_empty() {
        Ok(ranges)
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 5.2
// =============================================================================

#[cfg(test)]
mod tests_phase_5_2_collect_blocks {
    use super::{
        FlatPageToken, NamedBlockRange, PageBlockToken, PageSourceToken, TemplateId, collect_blocks,
    };

    /// Jalon Vert (roadmap §5.2) — deux blocs top-level (non imbriqués)
    /// produisent exactement deux `NamedBlockRange`, aux indices exacts de
    /// contenu (bornes `[start, end)` excluant les marqueurs `BlockOpen`/
    /// `BlockEnd` eux-mêmes, pas seulement au nombre de plages retournées).
    #[test]
    fn two_top_level_blocks_produce_exact_ranges() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "a" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "b" }),
            PageSourceToken::Runtime(FlatPageToken::Static("y")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let ranges = collect_blocks(template, &tokens).expect("chemin heureux attendu");

        assert_eq!(
            ranges,
            vec![
                NamedBlockRange {
                    name: "a",
                    template,
                    start: 1,
                    end: 2,
                },
                NamedBlockRange {
                    name: "b",
                    template,
                    start: 4,
                    end: 5,
                },
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 5.3
// =============================================================================

#[cfg(test)]
mod tests_phase_5_3_nested_block_detection {
    use super::{
        FlatPageToken, PageBlockToken, PageSourceToken, PageValidationError, TemplateId,
        collect_blocks,
    };

    /// Jalon Vert (roadmap §5.3) — un bloc imbriqué produit
    /// `Err(vec![NestedBlock { name: "inner" }])` : le nom rapporté est celui
    /// du bloc fautif (le plus profond), pas du bloc englobant. Le typage en
    /// `Result` exclut par construction toute sortie mixte : ce test
    /// documente cette absence de mélange succès/erreur en assertant
    /// directement sur la variante `Err`, sans exposer de plage à côté.
    #[test]
    fn nested_block_produces_named_error() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![PageValidationError::NestedBlock { name: "inner" }])
        );
    }
}

// =============================================================================
// Tests — Phase 5.4
// =============================================================================

#[cfg(test)]
mod tests_phase_5_4_unsupported_mapping {
    use super::{PageSourceToken, PageValidationError, TemplateId, collect_blocks};

    /// Jalon Vert (roadmap §5.4) — `for` produit nommément `ForLoopDetected`,
    /// jamais `RelationalKeyword`. Cas distingué du reste par construction
    /// (cf. doc de tête de `collect_blocks`, section Phase 5.4).
    #[test]
    fn for_keyword_produces_for_loop_detected() {
        let template = TemplateId(0);
        let tokens = vec![PageSourceToken::Unsupported {
            keyword: "for",
            tail: " item in items",
        }];

        let result = collect_blocks(template, &tokens);

        assert_eq!(result, Err(vec![PageValidationError::ForLoopDetected]));
    }

    /// Jalon Vert (roadmap §5.4) — chacun des mots-clés relationnels connus
    /// (`join`/`where`/`filter`/`group`) produit nommément
    /// `RelationalKeyword { keyword }`, avec le `keyword` reçu tel quel.
    /// Paramétré, comme le catch-all Parser (Phase 4.7) dont cette
    /// validation est le pendant côté `collect_blocks`.
    #[test]
    fn relational_keywords_produce_relational_keyword_error() {
        let template = TemplateId(0);

        for keyword in ["join", "where", "filter", "group"] {
            let tokens = vec![PageSourceToken::Unsupported { keyword, tail: "" }];

            let result = collect_blocks(template, &tokens);

            assert_eq!(
                result,
                Err(vec![PageValidationError::RelationalKeyword { keyword }]),
                "mot-clé {keyword:?} : erreur RelationalKeyword attendue"
            );
        }
    }

    /// Jalon Vert (roadmap §5.4) — le mapping est *total*, pas une liste
    /// fermée sur les quatre mots-clés relationnels connus : un mot-clé
    /// arbitraire non listé (mais déjà capturé par le catch-all Phase 4.7,
    /// cf. `unsupported_catch_all_captures_arbitrary_keywords`) tombe aussi
    /// dans `RelationalKeyword`, jamais silencieusement ignoré.
    #[test]
    fn arbitrary_unsupported_keyword_also_produces_relational_keyword_error() {
        let template = TemplateId(0);
        let tokens = vec![PageSourceToken::Unsupported {
            keyword: "frobnicate",
            tail: " arg",
        }];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![PageValidationError::RelationalKeyword {
                keyword: "frobnicate"
            }])
        );
    }

    /// Jalon Vert (roadmap §5.4) — fail-slow vérifié : deux mots-clés
    /// `Unsupported` dans le même flux produisent un `Vec` de longueur 2,
    /// pas une sortie fail-fast qui s'arrêterait à la première erreur.
    #[test]
    fn two_unsupported_keywords_in_same_stream_accumulate_both_errors() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Unsupported {
                keyword: "for",
                tail: "",
            },
            PageSourceToken::Unsupported {
                keyword: "where",
                tail: "",
            },
        ];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![
                PageValidationError::ForLoopDetected,
                PageValidationError::RelationalKeyword { keyword: "where" },
            ])
        );
    }
}
