// crates/forge/fragment-forge/src/page/linker.rs

//! Phases 5.5–5.7 — `link` : appariement parent/enfant sans E/S
//! (`LinkPlan`/`BlockSubstitution`), vérification `static`, et
//! `collect_static_refs` (alimentation du paramètre `static_refs` de
//! `link`, fonction séparée par catégorie de concept — voir doc de tête).

#[cfg(test)]
use crate::fragment::token::FlatPageToken;
#[cfg(test)]
use crate::page::model::TemplateId;
use crate::page::model::{NamedBlockRange, PageLinkError, StaticPartialRef};
use crate::page::token::PageSourceToken;

// =============================================================================
// PHASE 5.5 — `link` : appariement sans E/S (Document 2 §4)
// =============================================================================
// Responsabilité (roadmap §5.5) : répondre à des questions de correspondance
// *par référence*, sans muter aucune structure — pour chaque plage du
// parent, quelle est la substitution retenue (plage enfant si redéfinition
// de même nom, plage parent sinon) ? Toute plage enfant sans correspondance
// côté parent est un bloc orphelin. Fonction pure modulo E/S injectée (la
// vérification `static`, ci-dessous, Phase 5.6, ne branche que la fonction
// `file_exists` reçue — aucun `std::fs` direct dans ce module).
//
// ─── Décision de signature (roadmap §5.5, point explicitement laissé ouvert) ─
//
//   La roadmap propose deux options : signature réduite à
//   `(parent_blocks, child_blocks)` avec re-signature en 5.6, ou signature
//   complète dès 5.5 avec `static_refs`/`file_exists` présents mais non
//   utilisés. La roadmap recommande explicitement la seconde (« pour ne pas
//   re-signer la fonction en 5.6 ») — retenue en 5.5. Confirmé par 5.6
//   ci-dessous : la signature n'a pas bougé, seul le corps a gagné une
//   boucle.
//
// ─── Règle de construction du plan ──────────────────────────────────────────
//
//   Pour chaque plage du parent (ordre de parcours = ordre du parent) : la
//   substitution retenue est celle de l'enfant si un nom identique existe
//   côté enfant, sinon celle du parent lui-même (comportement par défaut —
//   Document 2 §4). Conséquence directe : `substitutions.len() ==
//   parent_blocks.len()` est un invariant de complétude, vérifié par
//   construction (une itération, une poussée, jamais de `continue` qui
//   sauterait une plage parent) — pas seulement par les tests.
//
//   Toute plage de l'enfant qui ne correspond à aucun nom du parent est un
//   `PageLinkError::OrphanBlock` — jamais silencieusement ignorée. Boucle
//   séparée de la construction du plan (deux responsabilités disjointes du
//   même contrat : « quelle substitution » vs. « quel enfant est
//   orphelin »), fail-slow comme `collect_blocks` : les deux boucles vont
//   jusqu'au bout, `substitutions` est entièrement construit même si
//   `errors` est non vide, mais seul l'un des deux est retourné.
// =============================================================================
// PHASE 5.6 — `link` : vérification `static` (Document 2 §4)
// =============================================================================
// Extension de 5.5 (roadmap §5.6) : une boucle ajoutée, aucune modification
// de la logique de blocs (construction du plan, détection `OrphanBlock`
// inchangées ligne à ligne). Invariant introduit : existence de fichier
// vérifiée via E/S injectée (`file_exists: impl Fn(&str) -> bool`), jamais
// via `std::fs` direct — la fonction reste testable sans FS réel, seule la
// fermeture passée par l'appelant décide de ce qu'« exister » signifie.
//
// ─── Mécanisme ──────────────────────────────────────────────────────────
//
//   Une troisième boucle, sur `static_refs` : pour chaque
//   `StaticPartialRef { original_path }`, `file_exists(original_path)` est
//   interrogé. `false` → `PageLinkError::StaticFileNotFound { path:
//   original_path }` poussée dans le même `errors` que `OrphanBlock` — un
//   seul `Vec` d'erreurs pour les deux axes de validation du Linker, fidèle
//   au fail-slow déjà en place : aucune des trois boucles (substitution,
//   orphelin, static) n'interrompt les autres.
//
// ─── Duplication d'E/S assumée (Document 2 §4) ─────────────────────────────
//
//   `file_exists` ici est distinct de la lecture de taille que fera plus
//   tard le Resolver (Document 3 §3, `get_file_size`) : deux fonctions
//   injectées, deux contextes de phase, pas de mutualisation prématurée —
//   décision déjà actée par le document d'architecture, appliquée sans
//   écart.
//
// ─── `link` clos (Document 2 §4 terminé) ───────────────────────────────────
//
//   Les trois erreurs de `PageLinkError` (`ExtendsNotFound`, `OrphanBlock`,
//   `StaticFileNotFound`) ont chacune leur point d'émission : `OrphanBlock`
//   et `StaticFileNotFound` dans `link` (ce module), `ExtendsNotFound` dans
//   l'orchestrateur (Document 3, hors périmètre — résolution du chemin
//   `extends` lui-même, pas une correspondance de blocs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSubstitution<'src> {
    /// Nom du bloc, identique à `NamedBlockRange::name` du parent
    /// correspondant. Dupliqué depuis `source` pour que le Lowering
    /// (Phase 5.8+) puisse itérer sur `substitutions` sans redériver le nom
    /// depuis une plage dont l'origine (enfant ou parent) varie.
    pub name: &'src str,
    /// Plage de contenu retenue : plage enfant si override, plage parent
    /// sinon. Porte son propre `TemplateId` (`NamedBlockRange::template`) —
    /// c'est ce champ, pas `name`, qui indique dans quel AST le Lowering
    /// devra lire le contenu substitué.
    pub source: NamedBlockRange<'src>,
}

/// Plan de fusion produit par `link` : une substitution par bloc du parent,
/// dans l'ordre du parent. Type de données pur — aucune méthode de fusion
/// ici, c'est le rôle du Lowering (Document 2 §5, Phase 5.8+).
///
/// `substitutions.len() == parent_blocks.len()` est un invariant de ce type
/// produit par `link` (voir doc de tête ci-dessus) — pas revérifié à la
/// construction (pas de constructeur dédié : le champ est public, produit
/// uniquement par `link` dans ce module à ce stade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPlan<'src> {
    pub substitutions: Vec<BlockSubstitution<'src>>,
}

/// Calcule le plan de fusion entre les blocs d'un parent et ceux d'un
/// enfant (correspondance par nom), et vérifie l'existence de chaque
/// fichier `{% static %}` référencé via `file_exists` — aucune E/S directe
/// dans cette fonction, aucune mutation des tranches reçues. Voir doc de
/// tête (Phases 5.5/5.6) pour la règle de construction du plan et le
/// mécanisme de vérification `static`.
pub fn link<'src>(
    parent_blocks: &[NamedBlockRange<'src>],
    child_blocks: &[NamedBlockRange<'src>],
    static_refs: &[StaticPartialRef<'src>],
    file_exists: impl Fn(&str) -> bool,
) -> Result<LinkPlan<'src>, Vec<PageLinkError<'src>>> {
    let mut substitutions = Vec::with_capacity(parent_blocks.len());
    for parent_range in parent_blocks {
        let source = child_blocks
            .iter()
            .find(|child_range| child_range.name == parent_range.name)
            .copied()
            .unwrap_or(*parent_range);
        substitutions.push(BlockSubstitution {
            name: parent_range.name,
            source,
        });
    }

    let mut errors = Vec::new();
    for child_range in child_blocks {
        let has_matching_parent = parent_blocks
            .iter()
            .any(|parent_range| parent_range.name == child_range.name);
        if !has_matching_parent {
            errors.push(PageLinkError::OrphanBlock {
                name: child_range.name,
            });
        }
    }

    for static_ref in static_refs {
        if !file_exists(static_ref.original_path) {
            errors.push(PageLinkError::StaticFileNotFound {
                path: static_ref.original_path,
            });
        }
    }

    if errors.is_empty() {
        Ok(LinkPlan { substitutions })
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 5.5
// =============================================================================

#[cfg(test)]
mod tests_phase_5_5_link {
    use super::{LinkPlan, NamedBlockRange, PageLinkError, TemplateId, link};

    fn range(name: &str, template: TemplateId, start: usize, end: usize) -> NamedBlockRange<'_> {
        NamedBlockRange {
            name,
            template,
            start,
            end,
        }
    }

    /// Jalon Vert (roadmap §5.5) — un bloc enfant de même nom qu'un bloc
    /// parent est retenu comme substitution (override), la plage source
    /// pointant vers l'enfant, pas vers le parent.
    #[test]
    fn child_override_replaces_parent_range() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("title", child_template, 10, 20)];

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(
            plan,
            LinkPlan {
                substitutions: vec![super::BlockSubstitution {
                    name: "title",
                    source: range("title", child_template, 10, 20),
                }],
            }
        );
    }

    /// Jalon Vert (roadmap §5.5) — un bloc parent sans redéfinition côté
    /// enfant conserve son propre contenu (fallback par défaut, Document 2
    /// §4).
    #[test]
    fn parent_range_kept_when_no_override() {
        let parent_template = TemplateId(0);
        let parent_blocks = vec![range("footer", parent_template, 5, 8)];
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(
            plan,
            LinkPlan {
                substitutions: vec![super::BlockSubstitution {
                    name: "footer",
                    source: range("footer", parent_template, 5, 8),
                }],
            }
        );
    }

    /// Jalon Vert (roadmap §5.5) — un bloc enfant sans correspondance côté
    /// parent produit `OrphanBlock`, jamais une substitution silencieuse.
    #[test]
    fn child_block_without_parent_match_is_orphan() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("sidebar", child_template, 0, 3)];

        let result = link(&parent_blocks, &child_blocks, &[], |_| true);

        assert_eq!(
            result,
            Err(vec![PageLinkError::OrphanBlock { name: "sidebar" }])
        );
    }

    /// Jalon Vert (roadmap §5.5) — invariant de complétude : une entrée de
    /// plan par bloc parent, jamais moins, quel que soit le nombre de blocs
    /// enfant (redéfinis ou non).
    #[test]
    fn substitutions_len_always_equals_parent_blocks_len() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![
            range("a", parent_template, 0, 1),
            range("b", parent_template, 2, 3),
            range("c", parent_template, 4, 5),
        ];
        let child_blocks = vec![range("b", child_template, 10, 11)];

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(plan.substitutions.len(), parent_blocks.len());
    }
}

// =============================================================================
// Tests — Phase 5.6
// =============================================================================

#[cfg(test)]
mod tests_phase_5_6_link_static_check {
    use super::{NamedBlockRange, PageLinkError, StaticPartialRef, TemplateId, link};

    fn range(name: &str, template: TemplateId, start: usize, end: usize) -> NamedBlockRange<'_> {
        NamedBlockRange {
            name,
            template,
            start,
            end,
        }
    }

    /// Jalon Vert (roadmap §5.6) — `file_exists` renvoyant `false` produit
    /// `StaticFileNotFound`, le chemin porté étant celui reçu tel quel
    /// (aucune normalisation dans `link`).
    #[test]
    fn missing_static_file_produces_static_file_not_found() {
        let parent_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let static_refs = vec![StaticPartialRef {
            original_path: "nav.html",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| false);

        assert_eq!(
            result,
            Err(vec![PageLinkError::StaticFileNotFound { path: "nav.html" }])
        );
    }

    /// Jalon Vert (roadmap §5.6) — `file_exists` renvoyant `true` ne
    /// produit aucune erreur : le plan est calculé normalement.
    #[test]
    fn existing_static_file_produces_no_error() {
        let parent_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let static_refs = vec![StaticPartialRef {
            original_path: "nav.html",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| true);

        assert!(result.is_ok());
        assert!(result.unwrap().substitutions.is_empty());
    }

    /// Jalon Vert (roadmap §5.6) — fail-slow croisé sur les deux axes du
    /// Linker : un bloc enfant orphelin ET un fichier `static` manquant
    /// dans le même appel produisent un `Vec` de 2 erreurs, jamais une
    /// seule (pas d'interruption au premier axe en défaut).
    #[test]
    fn orphan_block_and_missing_static_file_accumulate_both_errors() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("sidebar", child_template, 0, 3)];
        let static_refs = vec![StaticPartialRef {
            original_path: "missing.css",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| false);

        assert_eq!(
            result,
            Err(vec![
                PageLinkError::OrphanBlock { name: "sidebar" },
                PageLinkError::StaticFileNotFound {
                    path: "missing.css"
                },
            ])
        );
    }
}

// =============================================================================
// PHASE 5.7 — `collect_static_refs` (Document 2 §4, alimentation de `link`)
// =============================================================================
// Responsabilité (roadmap §5.7) : extraire, sans omission, toutes les
// références `{% static %}` d'un flux `PageSourceToken` — fonction séparée,
// une seule responsabilité, pour alimenter le paramètre `static_refs` de
// `link` (Phase 5.6, déjà clos). Le câblage réel (quel flux — enfant, parent,
// ou les deux — est passé à `link` par l'orchestrateur) est hors périmètre
// de cette phase : Document 3.
//
// ─── Pourquoi une fonction séparée, pas une extension de `collect_blocks` ──
//
//   `collect_blocks` (Phase 5.2-5.4, Document 2 §3) a une seule
//   responsabilité déjà remplie : position des blocs et validation de forme
//   sans second fichier. Y ajouter la collecte `static` mélangerait deux
//   catégories de concept distinctes dans une même fonction (§0 : une
//   fonction, une catégorie de concept éliminée) — ici, « où sont les
//   blocs » et « où sont les références static » n'ont aucune donnée ni
//   aucun invariant en commun (pas de pile, pas d'appariement, pas d'erreur
//   de forme). Contrairement à la fusion actée pour `collect_blocks`
//   lui-même (construction de plage + validation de forme : même flux, même
//   ordre, même pile), aucune économie de parcours ne justifierait ici de
//   coupler les deux : un filtre `Static` et un appariement `BlockOpen`/
//   `BlockEnd` restent deux boucles indépendantes même fusionnées en une
//   seule passe physique, sans partage d'état — la séparation en deux
//   fonctions ne coûte donc aucune localité de cache supplémentaire.
//
// ─── Pas de déduplication (Document 2 §6.2, point ouvert non tranché ici) ──
//
//   Chaque occurrence de `{% static %}` dans le flux produit une entrée,
//   y compris si `original_path` est identique à une entrée déjà retournée.
//   Comportement identique à `{% include %}` en Mode Fragment (gelé) :
//   compter les occurrences réelles, pas les chemins distincts. La
//   déduplication cross-page évoquée par le scaffolding de `StaticPartialRef`
//   (partager un unique `static_partials::{IDENT}` entre plusieurs pages)
//   resterait hors de portée même avec une déduplication *intra*-flux ici —
//   c'est un problème d'orchestrateur sur plusieurs fichiers, pas un problème
//   de cette fonction sur un seul flux. Introduire un filtre de doublons
//   maintenant serait un comportement spéculatif non demandé par cette phase.
//
// ─── Complexité et mémoire ──────────────────────────────────────────────────
//
//   Une seule boucle sur `tokens`, `O(n)`. Aucun étage de recherche
//   (`HashSet`, tri) : la fonction ne compare jamais deux entrées entre
//   elles, elle projette uniquement. `Vec<StaticPartialRef<'src>>` alloué au
//   premier `push`, croissance linéaire — pas de capacité pré-allouée sur la
//   taille de `tokens` (le nombre de `Static` est généralement une faible
//   fraction du flux ; `Vec::with_capacity(tokens.len())` sur-allouerait dans
//   le cas courant sans bénéfice mesuré). `StaticPartialRef` est `Copy`,
//   copié depuis la slice sans indirection nouvelle.

/// Extrait, dans l'ordre du flux et sans déduplication, toutes les
/// références `{% static %}` de `tokens`. Filtre pur : ne consulte ni
/// `PageArena`, ni `LinkPlan`, ne fait aucune E/S. Voir doc de tête
/// (Phase 5.7) pour la justification de la séparation d'avec
/// `collect_blocks` et l'absence de déduplication.
pub fn collect_static_refs<'src>(tokens: &[PageSourceToken<'src>]) -> Vec<StaticPartialRef<'src>> {
    let mut refs = Vec::new();
    for token in tokens {
        if let PageSourceToken::Static(static_ref) = token {
            refs.push(*static_ref);
        }
    }
    refs
}

// =============================================================================
// Tests — Phase 5.7
// =============================================================================

#[cfg(test)]
mod tests_phase_5_7_collect_static_refs {
    use super::{FlatPageToken, PageSourceToken, StaticPartialRef, collect_static_refs};

    /// Jalon Vert (roadmap §5.7) — un flux portant 2 `Static`, dont 1 chemin
    /// dupliqué (`nav.html` apparaît deux fois), produit 2 entrées : la
    /// fonction compte les occurrences réelles, elle ne déduplique pas par
    /// valeur de `original_path` (Document 2 §6.2, comportement dégradé
    /// retenu comme contrat v1).
    #[test]
    fn duplicated_static_path_yields_two_entries_not_one() {
        let tokens = vec![
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
            PageSourceToken::Runtime(FlatPageToken::Static("between")),
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
        ];

        let refs = collect_static_refs(&tokens);

        assert_eq!(
            refs,
            vec![
                StaticPartialRef {
                    original_path: "nav.html"
                },
                StaticPartialRef {
                    original_path: "nav.html"
                },
            ]
        );
    }
}
