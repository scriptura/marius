//! Phases 5.8–5.9 — `lower` : projection finale
//! `(&[PageSourceToken], &LinkPlan, &PageArena) -> Vec<FlatPageToken>`.
//! `Runtime` → identité, `Static` → `StaticInclude` provisoire, `Block` →
//! splice de la plage substituée (parent ou enfant selon `LinkPlan`).

use crate::fragment::token::FlatPageToken;

#[cfg(test)]
use crate::page::blocks::collect_blocks;
use crate::page::linker::LinkPlan;
#[cfg(test)]
use crate::page::linker::link;
#[cfg(test)]
use crate::page::model::ParsedPageTemplate;
use crate::page::model::{PageArena, PageBlockToken, StaticPartialRef};
use crate::page::token::PageSourceToken;

// =============================================================================
// PHASE 5.8 — `lower` : projection sans substitution (Document 2 §5)
// =============================================================================
// Responsabilité (roadmap §5.8) : poser la signature finale du Lowering —
// `(&[PageSourceToken], &LinkPlan, &PageArena) -> Vec<FlatPageToken>` — et en
// implémenter le sous-ensemble exercé sans aucun bloc en entrée : projection
// `Runtime` → identité, `Static` → `StaticInclude` provisoire. La splice des
// plages substituées (`PageSourceToken::Block`, via `LinkPlan`/`PageArena`)
// est explicitement hors périmètre de cette phase — couverte en 5.9.
//
// ─── Pourquoi la signature complète dès maintenant ─────────────────────────
//
//   Même arbitrage que pour `link` en 5.5/5.6 (roadmap §5.5, doc de tête
//   ci-dessus) : la roadmap demande explicitement de poser `plan` et `arena`
//   dans la signature dès 5.8 pour que 5.9 étende le corps sans re-signer la
//   fonction. `plan`/`arena` ne sont pas encore lus par cette phase — voir
//   ci-dessous pour la raison pour laquelle ceci n'est pas un paramètre
//   spéculatif au sens interdit par la contrainte de phase : la signature
//   est un engagement de contrat déjà acté par Document 2 §5 et la roadmap,
//   pas une anticipation de logique non spécifiée.
//
// ─── Chemin heureux uniquement : `Block` hors périmètre, documenté ─────────
//
//   Cette phase ne reçoit, par contrat de test (roadmap §5.8 : « testée
//   uniquement sur un LinkPlan vide »), aucun `PageSourceToken::Block` en
//   entrée. Suivant le précédent déjà établi en 5.2 (`collect_blocks`,
//   `BlockEnd` sans `BlockOpen` correspondant : panique documentée plutôt que
//   `todo!` silencieux ou comportement inventé), le cas `Block` panique ici
//   avec un message explicite renvoyant à la Phase 5.9 : ni `todo!` ni
//   `unimplemented!` littéral, mais une invariante non couverte nommée sans
//   ambiguïté, plutôt qu'un branchement de substitution deviné par avance.
//
//   `PageSourceToken::Unsupported` ne peut pas non plus atteindre cette
//   fonction — précondition déjà actée par Document 2 §5 : ce cas est rejeté
//   en amont par `collect_blocks` (Phase 5.4, clos). Une occurrence ici
//   serait un bug de la phase amont, pas un cas à absorber dans le Lowering
//   (citation directe du contrat : « le Lowering suppose une entrée déjà
//   validée »). Panique documentée, même style que ci-dessus.
//
// ─── Projection `Static` → `StaticInclude` (provisoire) ───────────────────
//
//   `len = 0` et `rel_from_manifest = original_path` : exactement le même
//   couple de valeurs provisoires que le pattern `include` du Mode Fragment
//   (gelé, `parse_block`, ligne ~1033) — `len` sera résolu par le Resolver
//   (Document 2 §5, symétrie explicitement actée par le contrat), et
//   `rel_from_manifest` par l'orchestrateur (Document 3, hors périmètre).
//   Aucune divergence de convention entre les deux modes sur ce point.
//
// ─── Mémoire : capacité exacte pour ce sous-ensemble ───────────────────────
//
//   `Vec::with_capacity(parent_tokens.len())` est une borne exacte, pas une
//   estimation, tant qu'aucun `Block` n'est présent : chaque `Runtime` et
//   chaque `Static` produit exactement un `FlatPageToken` en sortie, la
//   correspondance est 1:1. Cette égalité cesse d'être vraie dès que la
//   Phase 5.9 introduira la splice de plages (les délimiteurs `BlockOpen`/
//   `BlockEnd` disparaissent, le contenu spliced peut différer en longueur
//   du contenu parent d'origine) — capacité à réévaluer à ce moment, pas
//   anticipée ici.
// =============================================================================
// PHASE 5.9 — `lower` : substitution effective (Document 2 §5)
// =============================================================================
// Extension de 5.8 (roadmap §5.9) : le corps de `lower` gagne le traitement
// de `PageSourceToken::Block` — aucune modification de signature (confirmée
// dès 5.8), aucune modification des branches `Runtime`/`Static` déjà closes.
// Invariant introduit (clôture du domaine composition, Document 2 §1) : le
// contenu émis pour un bloc dépend *exclusivement* de `LinkPlan` — jamais
// implicitement du contenu situé entre `BlockOpen`/`BlockEnd` dans
// `parent_tokens`. Concrètement : la plage `[start, end)` lue est toujours
// `arena.get(substitution.source.template).tokens[start..end]`, jamais une
// sous-tranche de `parent_tokens` elle-même — même quand `substitution.source
// .template` se trouve être le parent (cas « non redéfini », Document 2 §4 :
// « comportement par défaut »). Un consommateur ne peut pas, en lisant ce
// code, confondre « ce qui est physiquement entre les délimiteurs du parent »
// et « ce qui est effectivement émis » : ce sont deux sources distinctes, et
// seule la seconde compte.
//
// ─── Mécanisme de correspondance et de saut ────────────────────────────────
//
//   Boucle à index explicite (pas de `for`/`enumerate` : l'avancée n'est pas
//   uniforme — un `Block(BlockOpen)` consomme plusieurs positions de
//   `parent_tokens` d'un coup, jusqu'au `BlockEnd` apparié). Sur
//   `Block(BlockOpen { name })` : recherche dans `plan.substitutions` de
//   l'entrée de même `name` (linéaire — `substitutions` est court, borné par
//   le nombre de blocs d'un fichier, jamais un ensemble justifiant un index
//   de recherche). La plage retenue (`substitution.source`) est lue depuis
//   `arena`, jamais depuis `parent_tokens` (voir ci-dessus), puis chaque
//   token de cette plage est projeté par la même règle `Runtime`/`Static`
//   que le niveau supérieur (factorisée dans `lower_leaf_token`, privée à ce
//   module — aucune duplication de la logique de projection entre le niveau
//   racine et le contenu splicé). L'index `i` saute ensuite directement
//   après le `BlockEnd` apparié dans `parent_tokens` (recherche du premier
//   `BlockEnd` suivant `i` — sûre par précondition : les blocs ne sont pas
//   imbriqués, invariant déjà garanti en amont par `collect_blocks`,
//   `NestedBlock` rejeté avant que `lower` ne soit jamais appelé).
//
// ─── Absence de correspondance ou de fermeture : précondition violée ───────
//
//   Deux cas restent des paniques documentées, jamais des `Result` : (1) un
//   `name` de `BlockOpen` absent de `plan.substitutions` — ne peut se
//   produire si `plan` provient de `link` appelé avec les blocs du *même*
//   parent que `parent_tokens` représente (précondition d'appel, comme pour
//   `TemplateId` au Document 2 §2 : détectable par assertion, jamais un
//   contenu halluciné) ; (2) un `BlockOpen` sans `BlockEnd` apparié dans
//   `parent_tokens` — rejeté en amont par `collect_blocks`
//   (`Vec<PageValidationError>`, Phase 5.2-5.4), ne peut structurellement
//   pas atteindre `lower` sur une entrée déjà validée. Un `Block(BlockEnd)`
//   rencontré par la boucle principale sans avoir été consommé comme
//   fermeture d'un `BlockOpen` est le même bug de précondition, symétrique.
//
// ─── Contenu splicé : pas de `Block` imbriqué, panique symétrique ──────────
//
//   `lower_leaf_token` (utilisée à la fois au niveau racine et pour projeter
//   le contenu d'une plage substituée) panique sur `Block(_)` : l'imbrication
//   de blocs est rejetée en amont (`NestedBlock`, Phase 5.3), donc un
//   `Block` ne peut structurellement pas apparaître à l'intérieur d'une
//   plage `NamedBlockRange` déjà validée — même raisonnement que pour
//   `Unsupported` (Document 2 §5 : « bug de la phase amont, pas un cas à
//   gérer ici »), étendu ici au niveau splicé plutôt qu'au seul niveau
//   racine.
//
// ─── Domaine composition clos (Document 2 §1) ──────────────────────────────
//
//   À partir d'ici, `FlatPageToken<'src>` — sans variante `Block`,
//   `Extends`, ou `TemplateId` — est la seule sortie possible de ce pipeline :
//   aucun type intermédiaire d'héritage ne peut franchir cette fonction par
//   construction du système de types (Document 2 §1, postcondition finale).
//   `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` (Mode
//   Fragment, gelés) s'appliquent sans modification ni branchement de mode —
//   vérifié par `cargo check` : aucun de leurs `match` exhaustifs sur
//   `FlatPageToken` n'a exigé de nouveau bras pour cette phase (jalon de
//   compilation, pas seulement d'exécution, roadmap §5.9).
pub fn lower<'src>(
    parent_tokens: &[PageSourceToken<'src>],
    plan: &LinkPlan<'src>,
    arena: &PageArena<'src>,
) -> Vec<FlatPageToken<'src>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < parent_tokens.len() {
        match &parent_tokens[i] {
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                let substitution = plan
                    .substitutions
                    .iter()
                    .find(|substitution| substitution.name == *name)
                    .unwrap_or_else(|| {
                        panic!(
                            "lower (Phase 5.9) : aucune substitution pour le bloc \
                             {name:?} dans LinkPlan — précondition violée : `plan` \
                             doit provenir de `link` appelé avec les blocs du même \
                             parent que `parent_tokens` représente."
                        )
                    });
                let source = substitution.source;
                let source_tokens = &arena.get(source.template).tokens[source.start..source.end];
                out.extend(source_tokens.iter().map(lower_leaf_token));

                i = find_matching_block_end(parent_tokens, i) + 1;
            }
            PageSourceToken::Block(PageBlockToken::BlockEnd) => unreachable!(
                "lower (Phase 5.9) : PageSourceToken::Block(BlockEnd) rencontré \
                 par la boucle principale sans BlockOpen apparié — précondition \
                 violée (entrée déjà validée par collect_blocks, Phase 5.2-5.4) : \
                 toute fermeture doit avoir été consommée par le traitement de \
                 son ouverture."
            ),
            other => {
                out.push(lower_leaf_token(other));
                i += 1;
            }
        }
    }
    out
}

/// Projette un token n'introduisant pas de composition (`Runtime`, `Static`)
/// vers `FlatPageToken` — factorisation partagée entre le niveau racine de
/// `lower` et le contenu d'une plage substituée (Phase 5.9, voir doc de
/// tête : « aucune duplication de la logique de projection »).
///
/// Panique sur `Block(_)` et `Unsupported { .. }` : aucun des deux ne peut
/// structurellement atteindre ce point sur une entrée déjà validée — voir
/// doc de tête, Phase 5.9, pour la justification de chaque cas.
fn lower_leaf_token<'src>(token: &PageSourceToken<'src>) -> FlatPageToken<'src> {
    match token {
        PageSourceToken::Runtime(flat) => *flat,
        PageSourceToken::Static(StaticPartialRef { original_path }) => {
            FlatPageToken::StaticInclude {
                original_path,
                rel_from_manifest: original_path,
                len: 0,
            }
        }
        PageSourceToken::Block(_) => unreachable!(
            "lower_leaf_token (Phase 5.9) : PageSourceToken::Block rencontré à \
             l'intérieur d'une plage projetée — précondition violée : \
             l'imbrication de blocs est rejetée en amont par collect_blocks \
             (NestedBlock, Phase 5.3), une plage NamedBlockRange déjà validée \
             ne peut structurellement pas en contenir."
        ),
        PageSourceToken::Unsupported { .. } => unreachable!(
            "lower_leaf_token (Phase 5.9) : PageSourceToken::Unsupported \
             rencontré — précondition violée (Document 2 §5) : ce cas doit \
             être rejeté en amont par collect_blocks (Phase 5.4), jamais \
             atteindre le Lowering. Bug de la phase amont, pas un cas géré ici."
        ),
    }
}

/// Cherche, à partir de `open_index + 1`, l'index du premier
/// `PageSourceToken::Block(PageBlockToken::BlockEnd)` de `tokens` — la
/// fermeture appariée au `BlockOpen` situé à `open_index`. Sûr sans pile
/// d'appariement : les blocs ne sont pas imbriqués (précondition, `lower`
/// n'est jamais appelé sur une entrée où `NestedBlock` aurait dû être
/// détecté par `collect_blocks`), donc le premier `BlockEnd` rencontré est
/// nécessairement celui qui ferme ce `BlockOpen`, jamais un autre.
///
/// Panique si aucun `BlockEnd` n'est trouvé : précondition violée, même
/// famille que l'assertion `open_stack.is_empty()` de `collect_blocks`
/// (Phase 5.2) — un `BlockOpen` sans fermeture est rejeté en amont, jamais
/// une entrée que `lower` doit absorber.
fn find_matching_block_end(tokens: &[PageSourceToken<'_>], open_index: usize) -> usize {
    tokens[open_index + 1..]
        .iter()
        .position(|token| matches!(token, PageSourceToken::Block(PageBlockToken::BlockEnd)))
        .map(|relative| open_index + 1 + relative)
        .unwrap_or_else(|| {
            panic!(
                "find_matching_block_end (Phase 5.9) : BlockOpen à l'index \
                 {open_index} sans BlockEnd apparié — précondition violée : \
                 rejeté en amont par collect_blocks (Phase 5.2-5.4), ne peut \
                 structurellement pas atteindre lower."
            )
        })
}

// =============================================================================
// Tests — Phase 5.8
// =============================================================================

#[cfg(test)]
mod tests_phase_5_8_lower_no_substitution {
    use super::{FlatPageToken, LinkPlan, PageArena, PageSourceToken, StaticPartialRef, lower};

    /// Jalon Vert (roadmap §5.8) — template sans blocs (`LinkPlan` vide,
    /// aucun `PageSourceToken::Block` en entrée) : le `Static` unique produit
    /// exactement un `FlatPageToken::StaticInclude { len: 0, .. }`, et
    /// chaque `Runtime` traverse inchangé (égalité valeur à valeur, testée
    /// sur plusieurs variantes de `FlatPageToken` pour couvrir la
    /// projection identité au-delà du seul cas `Static`).
    #[test]
    fn runtime_tokens_pass_through_and_static_becomes_static_include_with_len_zero() {
        let plan = LinkPlan {
            substitutions: Vec::new(),
        };
        let arena = PageArena::default();

        let tokens = vec![
            PageSourceToken::Runtime(FlatPageToken::Static("before")),
            PageSourceToken::Runtime(FlatPageToken::Field {
                entity: "user",
                field: "name",
            }),
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
            PageSourceToken::Runtime(FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            }),
            PageSourceToken::Runtime(FlatPageToken::EndIf),
        ];

        let result = lower(&tokens, &plan, &arena);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("before"),
                FlatPageToken::Field {
                    entity: "user",
                    field: "name",
                },
                FlatPageToken::StaticInclude {
                    original_path: "nav.html",
                    rel_from_manifest: "nav.html",
                    len: 0,
                },
                FlatPageToken::IfBool {
                    entity: "user",
                    field: "active",
                },
                FlatPageToken::EndIf,
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 5.9
// =============================================================================

#[cfg(test)]
mod tests_phase_5_9_lower_substitution {
    use super::{
        FlatPageToken, PageArena, PageBlockToken, PageSourceToken, ParsedPageTemplate,
        collect_blocks, link, lower,
    };

    /// Jalon Vert (roadmap §5.9) — bout en bout en mémoire : un parent à
    /// deux blocs (`title`, `footer`), un enfant qui redéfinit `title` mais
    /// pas `footer`. Vérifie dans le même test le bloc redéfini (contenu
    /// enfant émis, contenu parent d'origine absent) et le bloc non
    /// redéfini (contenu parent conservé) — séquence `FlatPageToken` exacte,
    /// élément par élément, `Vec` entier comparé par égalité de valeur.
    ///
    /// Assertion de type, pas seulement de valeur : le type de retour de
    /// `lower` est `Vec<FlatPageToken<'src>>` — sans variante `Block`,
    /// `Extends`, ni `TemplateId` possible en sortie, garanti par le système
    /// de types (Document 2 §1), pas reconfirmé ici par une inspection de
    /// valeur supplémentaire.
    #[test]
    fn overridden_block_uses_child_content_untouched_block_keeps_parent_content() {
        let parent_tokens = vec![
            PageSourceToken::Runtime(FlatPageToken::Static("<html>")),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "title" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Default Title")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Runtime(FlatPageToken::Static("<body>")),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "footer" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Default Footer")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Runtime(FlatPageToken::Static("</body></html>")),
        ];

        let child_tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "title" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Child Title")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let mut arena = PageArena::default();
        let parent_id = arena.admit(ParsedPageTemplate {
            extends: None,
            tokens: parent_tokens.clone(),
        });
        let child_id = arena.admit(ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: child_tokens.clone(),
        });

        let parent_blocks =
            collect_blocks(parent_id, &parent_tokens).expect("parent blocks bien formés");
        let child_blocks =
            collect_blocks(child_id, &child_tokens).expect("child blocks bien formés");

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true)
            .expect("linking réussit : aucun bloc orphelin, aucune référence static");

        let result = lower(&parent_tokens, &plan, &arena);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<html>"),
                FlatPageToken::Static("Child Title"),
                FlatPageToken::Static("<body>"),
                FlatPageToken::Static("Default Footer"),
                FlatPageToken::Static("</body></html>"),
            ]
        );
    }
}
