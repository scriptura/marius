// crates/forge/fragment-forge/src/page/blocks.rs

//! Phases 5.2–5.4, révisées HANDOFF imbrication `{% block %}` (Option A) —
//! `collect_blocks` : appariement par pile des `BlockOpen`/`BlockEnd` d'un
//! fichier admis en arène, production des `NamedBlockRange` (arène plate,
//! `parent_index` pour l'imbrication — jamais rejetée), fail-slow (mots-clés
//! non supportés mappés vers `PageValidationError`).

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
//      ci-dessous — un bloc imbriqué est rattaché à son parent, jamais
//      rejeté (HANDOFF imbrication `{% block %}`, Option A actée). Plus un
//      point hors périmètre depuis ce diff.
//
// ─── Point ouvert, non tranché par ce diff ─────────────────────────────────
//
//   Un flux structurellement mal formé au sens de l'appariement lui-même —
//   `BlockEnd` sans `BlockOpen` correspondant, ou `BlockOpen` non refermé en
//   fin de flux — n'est PAS un cas couvert par le chemin heureux testé ici,
//   et n'est représenté par aucune variante existante de
//   `PageValidationError` (`NonBoolIfCondition`, `ForLoopDetected`,
//   `RelationalKeyword` : aucune ne nomme un déséquilibre structurel).
//   Introduire une nouvelle variante pour ce cas dépasserait le
//   périmètre de cette phase (« ne préparer aucun comportement relevant des
//   phases ultérieures »). Choix retenu : un `panic!` documenté, nommé,
//   assorti d'un message explicite — jamais un `todo!`/`unimplemented!` muet
//   — sur une entrée que les fixtures testées à ce stade ne produisent
//   jamais. À trancher explicitement dans une session ultérieure, au même
//   titre que le point ouvert déjà signalé au Document 2 §6.1.
//
// =============================================================================
// PHASE 5.3, révisée — `collect_blocks` : rattachement de l'imbrication
// (HANDOFF imbrication `{% block %}`, §3.2, Option A actée)
// =============================================================================
// Historique : cette phase produisait `PageValidationError::NestedBlock`
// dès que `open_stack` était non-vide à l'ouverture d'un nouveau bloc —
// interdiction par choix d'implémentation (aucune contrainte de capacité
// runtime ne l'imposait, cf. HANDOFF §1). Révision : le `BlockOpen` rencontré
// pendant que la pile est non-vide est désormais rattaché au bloc au sommet
// de la pile, via `NamedBlockRange::parent_index` — jamais rejeté.
//
// ─── Mécanisme — l'arène plate se construit au moment de l'ouverture ──────
//
//   Différence structurelle avec 5.2 (historique) : `ranges` n'attend plus
//   la fermeture (`BlockEnd`) pour recevoir une entrée. `BlockOpen` pousse
//   immédiatement une `NamedBlockRange` provisoire (`end` temporairement
//   égal à `start`, corrigé à la fermeture) — c'est cette poussée précoce
//   qui permet à un enfant, ouvert avant que son parent ne soit refermé,
//   de connaître l'indice définitif de son parent dans `ranges` au moment
//   même de sa propre construction. `open_stack` ne porte donc plus
//   `(name, start)` (forme historique) mais l'indice dans `ranges` de
//   chaque bloc actuellement ouvert — le sommet de pile, s'il existe, EST
//   le `parent_index` du prochain `BlockOpen` rencontré.
//
// ─── Profondeur non bornée, comme la pile LIFO le permettait déjà ─────────
//
//   Aucune limite de profondeur n'est introduite ici (aucun cas d'usage
//   connu ne la justifie — HANDOFF §4, point non tranché, laissé tel quel :
//   « à confirmer explicitement plutôt qu'à laisser un oubli », hors
//   périmètre de cette révision). La pile LIFO appariait déjà correctement
//   n'importe quelle profondeur (propriété algorithmique de 5.2 historique,
//   inchangée) ; cette révision exploite cette propriété plutôt que de la
//   contourner par une interdiction.
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
// ─── Fail-slow, orthogonal à l'imbrication ─────────────────────────────────
//
//   Cette branche ne fait pas partie de la pile d'appariement (`open_stack`
//   n'est ni lu ni modifié) : un mot-clé `Unsupported` peut coexister avec un
//   bloc imbriqué dans le même flux, chacun poussant sa propre erreur (ou,
//   pour l'imbrication, sa propre entrée `ranges`) sans interférence — même
//   politique fail-slow que par le passé, sur un axe de validation
//   indépendant.
pub fn collect_blocks<'src>(
    template: TemplateId,
    tokens: &[PageSourceToken<'src>],
) -> Result<Vec<NamedBlockRange<'src>>, Vec<PageValidationError<'src>>> {
    // Pile des indices, dans `ranges`, des blocs actuellement ouverts —
    // sommet de pile = parent direct du prochain `BlockOpen` rencontré.
    // Forme révisée (HANDOFF imbrication `{% block %}`, §3.2) : porte un
    // indice dans `ranges`, pas `(name, start)` (forme historique 5.2) —
    // `ranges` reçoit désormais son entrée dès l'ouverture, pas à la
    // fermeture (voir doc de section ci-dessus).
    let mut open_stack: Vec<usize> = Vec::new();
    let mut ranges: Vec<NamedBlockRange<'src>> = Vec::new();
    let mut errors = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        match token {
            // Ouverture : pousse immédiatement une entrée provisoire dans
            // `ranges` (`end` temporairement égal à `start`, corrigé à la
            // fermeture ci-dessous), rattachée au bloc au sommet de la pile
            // via `parent_index` — `None` si la pile est vide (bloc de
            // premier niveau). `start` pointe juste après le marqueur
            // `BlockOpen` lui-même — la plage couvre le contenu du bloc,
            // jamais ses délimiteurs (convention actée par la doc de
            // `NamedBlockRange`).
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                let parent_index = open_stack.last().copied();
                let range_index = ranges.len();
                ranges.push(NamedBlockRange {
                    name,
                    template,
                    start: index + 1,
                    end: index + 1,
                    parent_index,
                });
                open_stack.push(range_index);
            }
            // Fermeture : dépile l'indice du bloc que cette fermeture
            // referme, et corrige son `end` en place — `index` (position du
            // `BlockEnd`) exclusif, même convention qu'avant.
            PageSourceToken::Block(PageBlockToken::BlockEnd) => {
                let range_index = open_stack.pop().unwrap_or_else(|| {
                    panic!(
                        "collect_blocks : BlockEnd sans BlockOpen correspondant \
                         à l'index {index} — cas mal formé hors périmètre du \
                         chemin heureux, non représenté par PageValidationError \
                         à ce stade (voir doc de tête)"
                    )
                });
                ranges[range_index].end = index;
            }
            // Mot-clé de grammaire non supporté (Phase 5.4, cf. doc de tête) :
            // mapping total vers l'erreur de validation nommée
            // correspondante. N'interagit pas avec `open_stack` — orthogonal
            // à l'appariement de blocs, fail-slow au même titre que
            // l'imbrication ci-dessus.
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
        "collect_blocks : {} bloc(s) BlockOpen non refermé(s) en fin de flux \
         — cas mal formé hors périmètre du chemin heureux, non représenté \
         par PageValidationError à ce stade (voir doc de tête)",
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
                    parent_index: None,
                },
                NamedBlockRange {
                    name: "b",
                    template,
                    start: 4,
                    end: 5,
                    parent_index: None,
                },
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 5.3, révisée (HANDOFF imbrication `{% block %}`, Option A)
// =============================================================================

#[cfg(test)]
mod tests_phase_5_3_nested_block_attachment {
    use super::{
        FlatPageToken, NamedBlockRange, PageBlockToken, PageSourceToken, TemplateId, collect_blocks,
    };

    /// Jalon Vert — un bloc imbriqué à un seul niveau n'est plus rejeté :
    /// il produit une entrée `ranges` distincte, avec `parent_index`
    /// pointant vers l'indice de son parent dans le même `Vec` retourné.
    /// La plage du parent (`outer`) couvre bien tout son contenu, marqueurs
    /// du bloc enfant inclus (`[start, end)` du parent englobe ceux de
    /// `inner`) — c'est `parent_index`, pas les bornes, qui porte la
    /// structure.
    #[test]
    fn single_level_nesting_attaches_child_via_parent_index() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let ranges = collect_blocks(template, &tokens).expect("imbrication admise, chemin heureux");

        assert_eq!(
            ranges,
            vec![
                NamedBlockRange {
                    name: "outer",
                    template,
                    start: 1,
                    end: 4,
                    parent_index: None,
                },
                NamedBlockRange {
                    name: "inner",
                    template,
                    start: 2,
                    end: 3,
                    parent_index: Some(0),
                },
            ]
        );
    }

    /// Jalon Vert — imbrication à trois niveaux : chaque enfant référence
    /// l'indice exact de son parent direct, jamais celui du Root de la
    /// pile ni un indice fixe — `parent_index` suit la position réelle
    /// dans `ranges`, pas la profondeur.
    #[test]
    fn three_level_nesting_each_child_points_to_its_direct_parent() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "main_nav" }),
            PageSourceToken::Block(PageBlockToken::BlockOpen {
                name: "current_tab",
            }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "tab_icon" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let ranges = collect_blocks(template, &tokens).expect("imbrication admise, chemin heureux");

        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].name, "main_nav");
        assert_eq!(ranges[0].parent_index, None);
        assert_eq!(ranges[1].name, "current_tab");
        assert_eq!(ranges[1].parent_index, Some(0));
        assert_eq!(ranges[2].name, "tab_icon");
        assert_eq!(ranges[2].parent_index, Some(1));
    }

    /// Jalon Vert — deux enfants successifs (pas imbriqués l'un dans
    /// l'autre) du même parent partagent le même `parent_index` : la pile
    /// est bien dépilée entre les deux (`BlockEnd` du premier enfant avant
    /// `BlockOpen` du second), aucune confusion entre "frère" et "enfant du
    /// dernier frère".
    #[test]
    fn two_siblings_under_same_parent_share_parent_index() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "first" }),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "second" }),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let ranges = collect_blocks(template, &tokens).expect("imbrication admise, chemin heureux");

        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[1].name, "first");
        assert_eq!(ranges[1].parent_index, Some(0));
        assert_eq!(ranges[2].name, "second");
        assert_eq!(ranges[2].parent_index, Some(0));
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
