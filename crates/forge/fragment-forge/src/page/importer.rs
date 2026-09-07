// crates/forge/fragment-forge/src/page/importer.rs

//! `{% import %}` (Mode Page) — vérification de position, pure (aucune
//! E/S) : un import n'est valide qu'en position top-level, jamais à
//! l'intérieur d'un `{% block %}` ouvert. La résolution du chemin, la
//! lecture du fragment, son parsing récursif, la borne de profondeur et la
//! détection de cycle sont une responsabilité exclusive de l'orchestrateur
//! (`build/template/page.rs`) — même répartition que pour `{% extends %}`
//! (`ExtendsNotFound`, cycle, profondeur : jugés hors de ce crate) : ce
//! module ne fait jamais d'E/S, ici comme partout ailleurs dans
//! `fragment-forge`.

use crate::page::model::{ImportRef, PageBlockToken, PageImportError};
use crate::page::token::PageSourceToken;

// =============================================================================
// `collect_top_level_imports` — contrainte de position
// =============================================================================
//
// Pourquoi une contrainte de position, et pourquoi ici plutôt que dans le
// Parser : `parse_page_block` (Document 1, gelé pour `if`/`block`/`static`/
// `extends`) reste délibérément permissif sur l'imbrication — juger une
// position exige un état de pile que le Parser ne maintient pas (cf. sa
// propre doc de tête : « l'appariement correct et l'absence d'imbrication ne
// sont pas des garanties de sortie du Parser »). Cette fonction joue
// exactement le rôle que `collect_blocks` joue pour `NestedBlock` : une
// passe de validation séparée, sur l'AST déjà construit, avec sa propre pile
// de blocs ouverts.
//
// ─── Pourquoi interdire l'imbrication plutôt que l'autoriser ────────────────
//
//   Un `{% import %}` développé remplace positionnellement son marqueur par
//   le flux de tokens du fragment ciblé — y compris, potentiellement, les
//   propres `{% block %}` top-level de ce fragment. Si le marqueur lui-même
//   était toléré à l'intérieur d'un `{% block %}` déjà ouvert, l'expansion
//   recréerait exactement l'imbrication que `NestedBlock` (`collect_blocks`,
//   Document 2) interdit déjà par ailleurs — mais découverte tardivement,
//   après lecture et parsing du fragment, plutôt qu'immédiatement sur le
//   fichier qui importe. Rejeter la position en amont, sans même avoir
//   besoin de lire le fragment ciblé, donne un diagnostic plus rapide et
//   plus précis (le fichier fautif est celui qui contient le `{% import %}`
//   mal placé, jamais le fragment lui-même).
//
// ─── Fail-slow, comme `collect_blocks` ─────────────────────────────────────
//
//   Toutes les violations de position sont accumulées avant de retourner,
//   plutôt que de s'arrêter à la première rencontrée — même politique que
//   `collect_blocks` (`NestedBlock`) et `link_chain` (`OrphanBlock`).

/// Recense, dans l'ordre d'apparition, tous les `{% import %}` de `tokens`,
/// à condition qu'ils soient tous en position top-level (jamais à
/// l'intérieur d'un `{% block %}` ouvert). Toute violation est accumulée
/// (fail-slow) plutôt que de s'arrêter à la première rencontrée.
///
/// Fonction pure sur le flux de tokens d'un seul fichier déjà parsé : ne
/// fait aucune E/S, ne résout aucun chemin, ne connaît l'existence d'aucun
/// autre fichier.
pub fn collect_top_level_imports<'src>(
    tokens: &[PageSourceToken<'src>],
) -> Result<Vec<ImportRef<'src>>, Vec<PageImportError<'src>>> {
    let mut open_blocks: Vec<&'src str> = Vec::new();
    let mut imports = Vec::new();
    let mut errors = Vec::new();

    for token in tokens {
        match token {
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                open_blocks.push(name);
            }
            PageSourceToken::Block(PageBlockToken::BlockEnd) => {
                // `pop()` silencieux sur pile vide : un appariement
                // incorrect (`BlockEnd` sans `BlockOpen`) est du ressort de
                // `collect_blocks`/`NestedBlock`, pas de cette fonction —
                // elle ne juge que la position des `Import` relativement
                // aux blocs, jamais la bonne formation des blocs eux-mêmes.
                open_blocks.pop();
            }
            PageSourceToken::Import(import_ref) => match open_blocks.last() {
                Some(&block_name) => {
                    errors.push(PageImportError::ImportInsideBlock {
                        path: import_ref.original_path,
                        block_name,
                    });
                }
                None => imports.push(*import_ref),
            },
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(imports)
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests_collect_top_level_imports {
    use super::collect_top_level_imports;
    use crate::fragment::token::FlatPageToken;
    use crate::page::model::{ImportRef, PageBlockToken, PageImportError};
    use crate::page::token::PageSourceToken;

    /// Jalon Vert — imports top-level, entre deux blocs et avant/après :
    /// tous acceptés, dans l'ordre d'apparition.
    #[test]
    fn top_level_imports_are_collected_in_order() {
        let tokens = vec![
            PageSourceToken::Import(ImportRef {
                original_path: "head.marius",
            }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "main_nav" }),
            PageSourceToken::Runtime(FlatPageToken::Static("nav content")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Import(ImportRef {
                original_path: "footer.marius",
            }),
        ];

        let imports = collect_top_level_imports(&tokens).expect("aucun import mal placé");

        assert_eq!(
            imports,
            vec![
                ImportRef {
                    original_path: "head.marius"
                },
                ImportRef {
                    original_path: "footer.marius"
                },
            ]
        );
    }

    /// Jalon Vert — un `{% import %}` à l'intérieur d'un `{% block %}`
    /// ouvert est rejeté, avec le nom du bloc englobant fautif.
    #[test]
    fn import_inside_open_block_is_rejected() {
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "main_head" }),
            PageSourceToken::Import(ImportRef {
                original_path: "head.marius",
            }),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let result = collect_top_level_imports(&tokens);

        assert_eq!(
            result,
            Err(vec![PageImportError::ImportInsideBlock {
                path: "head.marius",
                block_name: "main_head",
            }])
        );
    }

    /// Jalon Vert — fail-slow : deux imports mal placés dans deux blocs
    /// distincts produisent deux erreurs, jamais une seule.
    #[test]
    fn multiple_misplaced_imports_accumulate_all_errors() {
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "main_head" }),
            PageSourceToken::Import(ImportRef {
                original_path: "head.marius",
            }),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "main_footer" }),
            PageSourceToken::Import(ImportRef {
                original_path: "footer.marius",
            }),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let result = collect_top_level_imports(&tokens);

        assert_eq!(
            result,
            Err(vec![
                PageImportError::ImportInsideBlock {
                    path: "head.marius",
                    block_name: "main_head",
                },
                PageImportError::ImportInsideBlock {
                    path: "footer.marius",
                    block_name: "main_footer",
                },
            ])
        );
    }

    /// Jalon Vert — aucun `{% import %}` dans le flux : succès trivial,
    /// liste vide.
    #[test]
    fn no_imports_yields_empty_list() {
        let tokens = vec![PageSourceToken::Runtime(FlatPageToken::Static("plain"))];

        let imports = collect_top_level_imports(&tokens).expect("aucun import à valider");

        assert!(imports.is_empty());
    }
}
