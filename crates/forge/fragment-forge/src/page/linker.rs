// crates/forge/fragment-forge/src/page/linker.rs

//! Phases 5.5–5.10 — `link_chain` : appariement N-aire feuille→Root sans
//! E/S (`LinkPlan`/`BlockSubstitution`), vérification `static`, et
//! `collect_static_refs` (alimentation du paramètre `static_refs`).
//! `link` (Phases 5.5/5.6, signature historique à 2 maillons) est
//! réimplémentée comme cas particulier de `link_chain` (Phase 5.10,
//! généralisation multi-niveaux) — aucune logique dupliquée.

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
// ─── `link` clos (Document 2 §4 terminé), généralisée en 5.10 ─────────────
//
//   Les trois erreurs de `PageLinkError` (`ExtendsNotFound`, `OrphanBlock`,
//   `StaticFileNotFound`) ont chacune leur point d'émission : `OrphanBlock`
//   et `StaticFileNotFound` dans `link`/`link_chain` (ce module),
//   `ExtendsNotFound` dans l'orchestrateur (Document 3, hors périmètre —
//   résolution du chemin `extends` lui-même, pas une correspondance de
//   blocs ; construite avec l'identité du fichier déclarant connue de
//   l'orchestrateur au moment de l'échec, jamais par ce module qui ne fait
//   aucune E/S).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSubstitution<'src> {
    /// Nom du bloc, identique à `NamedBlockRange::name` du Root
    /// correspondant. Dupliqué depuis `source` pour que le Lowering
    /// (Phase 5.8+) puisse itérer sur `substitutions` sans redériver le nom
    /// depuis une plage dont l'origine (Root ou n'importe quel maillon de
    /// la chaîne) varie.
    pub name: &'src str,
    /// Plage de contenu retenue : plage du maillon le plus proche de la
    /// feuille qui redéfinit ce nom, ou plage du Root lui-même si aucun
    /// maillon ne le redéfinit. Porte son propre `TemplateId`
    /// (`NamedBlockRange::template`) — c'est ce champ, pas `name`, qui
    /// indique dans quel AST le Lowering devra lire le contenu substitué.
    pub source: NamedBlockRange<'src>,
}

/// Plan de fusion produit par `link`/`link_chain` : une substitution par
/// bloc du Root, dans l'ordre du Root. Type de données pur — aucune méthode
/// de fusion ici, c'est le rôle du Lowering (Document 2 §5, Phase 5.8+).
///
/// `substitutions.len() == root_blocks.len()` est un invariant de ce type
/// produit par `link`/`link_chain` (voir doc de tête ci-dessus) — pas
/// revérifié à la construction (pas de constructeur dédié : le champ est
/// public, produit uniquement par ces deux fonctions dans ce module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPlan<'src> {
    pub substitutions: Vec<BlockSubstitution<'src>>,
}

// =============================================================================
// PHASE 5.10 — `link_chain` : généralisation N-aire de `link` (héritage
// multi-niveaux, `{% extends %}` chaîné)
// =============================================================================
// Contexte (tranché en session ultérieure à 5.5–5.7) : `link` plafonnait la
// chaîne d'héritage à 2 fichiers (parent direct + enfant), garde imposée par
// l'orchestrateur (`build/template/page.rs`), pas par ce module — voir
// commentaire de `PageArena` (Document 2 §6.1, point ouvert désormais
// tranché). `link_chain` lève cette limite ici, à la seule couche qui portait
// réellement une hypothèse binaire dans sa logique (`link` comparait
// exactement 2 ensembles de plages) ; `PageArena`, `NamedBlockRange`,
// `lower` n'ont jamais eu besoin de cette hypothèse et restent inchangés.
//
// ─── Ordre et rôle des niveaux ─────────────────────────────────────────────
//
//   `chain_blocks[0]` : le maillon le plus dérivé (feuille — le fichier
//   directement lié à la table). `chain_blocks[chain_blocks.len() - 1]` :
//   le Root — seul maillon sans `extends`, seul porteur des positions
//   physiques `BlockOpen`/`BlockEnd` effectivement traversées par `lower`.
//   Tout indice intermédiaire est un ancêtre, dans l'ordre feuille → Root.
//
// ─── Règle de résolution : « le plus proche de la feuille gagne » ─────────
//
//   Pour chaque plage du Root : on cherche, dans l'ordre feuille → Root
//   parmi tous les maillons non-Root, le premier qui redéfinit ce nom.
//   Trouvé : sa plage est la substitution. Sinon : plage par défaut du Root
//   lui-même (comportement identique à `link`, étendu à N niveaux au lieu
//   de 1). C'est une généralisation directe de la règle binaire ; à 2
//   niveaux (`chain_blocks = [enfant, parent]`), le comportement est
//   bit-à-bit identique à l'ancien `link`.
//
// ─── OrphanBlock : contre le Root, jamais contre un niveau intermédiaire ──
//
//   Un bloc déclaré à *n'importe quel* niveau non-Root (feuille ou
//   intermédiaire) et absent des `BlockOpen` du Root est orphelin — décision
//   actée explicitement : seul le Root porte des positions physiques
//   (`lower` ne traverse jamais que `root_tokens`), donc un bloc qui ne
//   correspond à aucun slot du Root est du code mort par construction, quel
//   que soit le niveau de profondeur où il est déclaré. `template` (ajouté à
//   `PageLinkError::OrphanBlock`) porte l'identité exacte du maillon fautif
//   — pas seulement son nom, qui ne suffirait pas à désigner le fichier
//   quand la chaîne compte plus de 2 maillons.
//
// ─── `link` comme cas particulier, pas comme duplication ──────────────────
//
//   `link(parent_blocks, child_blocks, ...)` = `link_chain(&[child_blocks,
//   parent_blocks], ...)`. Signature historique conservée pour compatibilité
//   des call-sites et des tests déjà écrits (Phases 5.5–5.9) — aucun de ces
//   tests n'a eu besoin d'être réécrit, seuls ceux qui inspectaient
//   `PageLinkError::OrphanBlock` par valeur ont dû suivre l'ajout du champ
//   `template` (changement mécanique, pas de logique).

/// Calcule le plan de fusion pour une chaîne `{% extends %}` de longueur
/// arbitraire. Voir doc de section ci-dessus pour l'ordre des niveaux, la
/// règle de résolution et la règle `OrphanBlock`.
///
/// Panique si `chain_blocks` est vide : précondition d'appel — une chaîne
/// d'héritage compte toujours au moins le Root (un Root sans aucun ancêtre
/// est un cas valide, représenté par une chaîne à une seule entrée, jamais
/// par une chaîne vide).
pub fn link_chain<'src>(
    chain_blocks: &[&[NamedBlockRange<'src>]],
    static_refs: &[StaticPartialRef<'src>],
    file_exists: impl Fn(&str) -> bool,
) -> Result<LinkPlan<'src>, Vec<PageLinkError<'src>>> {
    let root_blocks = chain_blocks
        .last()
        .expect("chain_blocks non vide : au moins le Root, précondition d'appel");
    let ancestors = &chain_blocks[..chain_blocks.len() - 1];

    let mut substitutions = Vec::with_capacity(root_blocks.len());
    for root_range in *root_blocks {
        let source = ancestors
            .iter()
            .find_map(|level| level.iter().find(|range| range.name == root_range.name))
            .copied()
            .unwrap_or(*root_range);
        substitutions.push(BlockSubstitution {
            name: root_range.name,
            source,
        });
    }

    let mut errors = Vec::new();
    for level in ancestors {
        for range in *level {
            let has_matching_root_slot = root_blocks
                .iter()
                .any(|root_range| root_range.name == range.name);
            if !has_matching_root_slot {
                errors.push(PageLinkError::OrphanBlock {
                    name: range.name,
                    template: range.template,
                });
            }
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

/// Cas particulier à 2 maillons de `link_chain` (Phase 5.10) — signature
/// historique (Phases 5.5/5.6) conservée pour compatibilité des appelants et
/// des tests existants. Toute la logique vit dans `link_chain` ; ce wrapper
/// ne fait que réordonner les deux tranches reçues (`[enfant, parent]`, Root
/// en dernier — le parent direct EST le Root dans le cas à 2 niveaux).
pub fn link<'src>(
    parent_blocks: &[NamedBlockRange<'src>],
    child_blocks: &[NamedBlockRange<'src>],
    static_refs: &[StaticPartialRef<'src>],
    file_exists: impl Fn(&str) -> bool,
) -> Result<LinkPlan<'src>, Vec<PageLinkError<'src>>> {
    link_chain(&[child_blocks, parent_blocks], static_refs, file_exists)
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
    /// `template` porte l'identité de l'enfant, seul maillon fautif possible
    /// dans une chaîne à 2 niveaux.
    #[test]
    fn child_block_without_parent_match_is_orphan() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("sidebar", child_template, 0, 3)];

        let result = link(&parent_blocks, &child_blocks, &[], |_| true);

        assert_eq!(
            result,
            Err(vec![PageLinkError::OrphanBlock {
                name: "sidebar",
                template: child_template,
            }])
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
                PageLinkError::OrphanBlock {
                    name: "sidebar",
                    template: child_template,
                },
                PageLinkError::StaticFileNotFound {
                    path: "missing.css"
                },
            ])
        );
    }
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
// Tests — Phase 5.10 (généralisation multi-niveaux)
// =============================================================================

#[cfg(test)]
mod tests_phase_5_10_link_chain {
    use super::{LinkPlan, NamedBlockRange, PageLinkError, TemplateId, link_chain};

    fn range(name: &str, template: TemplateId, start: usize, end: usize) -> NamedBlockRange<'_> {
        NamedBlockRange {
            name,
            template,
            start,
            end,
        }
    }

    /// Jalon Vert — chaîne à 3 niveaux (feuille, intermédiaire, Root) : la
    /// feuille redéfinit `title`, l'intermédiaire redéfinit `sidebar` (le
    /// Root ne redéfinit rien, il définit). Chaque substitution pointe vers
    /// le maillon le plus proche de la feuille qui redéfinit le nom — pas
    /// nécessairement la feuille elle-même.
    #[test]
    fn three_level_chain_nearest_override_wins_per_block() {
        let leaf = TemplateId(0);
        let mid = TemplateId(1);
        let root = TemplateId(2);

        let leaf_blocks = vec![range("title", leaf, 0, 5)];
        let mid_blocks = vec![range("sidebar", mid, 0, 5)];
        let root_blocks = vec![
            range("title", root, 0, 3),
            range("sidebar", root, 4, 6),
            range("footer", root, 7, 9),
        ];

        let plan = link_chain(&[&leaf_blocks, &mid_blocks, &root_blocks], &[], |_| true)
            .expect("aucun orphelin : title et sidebar existent tous deux dans le Root");

        assert_eq!(plan.substitutions.len(), 3);
        assert_eq!(plan.substitutions[0].name, "title");
        assert_eq!(plan.substitutions[0].source.template, leaf);
        assert_eq!(plan.substitutions[1].name, "sidebar");
        assert_eq!(plan.substitutions[1].source.template, mid);
        assert_eq!(plan.substitutions[2].name, "footer");
        assert_eq!(
            plan.substitutions[2].source.template, root,
            "footer non redéfini par personne : fallback sur le Root lui-même"
        );
    }

    /// Jalon Vert — un bloc déclaré par le maillon intermédiaire (ni la
    /// feuille, ni le Root) sans slot correspondant dans le Root est
    /// orphelin, avec `template` désignant précisément l'intermédiaire —
    /// jamais la feuille, jamais le Root, quel que soit le niveau réel de
    /// la déclaration fautive.
    #[test]
    fn orphan_declared_by_intermediate_level_names_that_level() {
        let leaf = TemplateId(0);
        let mid = TemplateId(1);
        let root = TemplateId(2);

        let leaf_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let mid_blocks = vec![range("extra_unused", mid, 0, 5)];
        let root_blocks = vec![range("title", root, 0, 3)];

        let result = link_chain(&[&leaf_blocks, &mid_blocks, &root_blocks], &[], |_| true);

        assert_eq!(
            result,
            Err(vec![PageLinkError::OrphanBlock {
                name: "extra_unused",
                template: mid,
            }])
        );
    }

    /// Jalon Vert — chaîne à 3 niveaux, cas particulier `link` (2 niveaux) :
    /// `link_chain(&[child, parent], ...)` produit exactement le même
    /// résultat que `link(parent, child, ...)` — non-régression explicite
    /// de la généralisation.
    #[test]
    fn two_level_chain_matches_historical_link_behavior() {
        let parent = TemplateId(0);
        let child = TemplateId(1);
        let parent_blocks = vec![range("title", parent, 0, 3)];
        let child_blocks = vec![range("title", child, 10, 20)];

        let via_link_chain =
            link_chain(&[&child_blocks, &parent_blocks], &[], |_| true).expect("pas d'orphelin");
        let via_link =
            super::link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(via_link_chain, via_link);
        assert_eq!(
            via_link_chain,
            LinkPlan {
                substitutions: vec![super::BlockSubstitution {
                    name: "title",
                    source: range("title", child, 10, 20),
                }],
            }
        );
    }
}
