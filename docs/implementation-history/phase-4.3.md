=== cargo fmt --check ===
103 écarts détectés, tous situés dans le code antérieur à la Phase 4.3
(ligne maximale concernée : 2327 sur 2912). Il s'agit du style d'alignement
manuel déjà présent dans le fichier original fourni (ex. alignement de
`=>` ou de `:` sur plusieurs colonnes) : ce fichier n'a jamais été passé
au formateur, indépendamment de cette session.
Zéro écart dans la section Phase 4.3 (lignes 2648 à 2912) : le code
nouvellement écrit est conforme à `cargo fmt` sans modification.
Aucune ligne pré-existante n'a été reformatée par cette session
(contrainte « ne modifier aucune fonctionnalité en dehors du périmètre
de la Phase 4.3 » respectée y compris sur le plan du formatage).

=== cargo test ===
running 39 tests
test tests_phase_1_1::all_variants_are_copy ... ok
test tests_phase_1_1::static_variant_infers_lifetime ... ok
test tests_phase_1_2::scan_block_if_endif ... ok
test tests_phase_1_2::scan_delimiter_at_start ... ok
test tests_phase_1_2::scan_empty_and_literal_only ... ok
test tests_phase_1_2::scan_expr_interpolation ... ok
test tests_phase_1_3::error_on_empty_block ... ok
test tests_phase_1_3::error_on_if_without_dot ... ok
test tests_phase_1_3::error_on_unexpected_top_level_span ... ok
test tests_phase_1_3::parse_full_template ... ok
test tests_phase_1_4::test_semantic_empty_ast ... ok
test tests_phase_1_4::test_semantic_errors ... ok
test tests_phase_1_4::test_semantic_valid ... ok
test tests_phase_2_1::bounded_field_referenced_contributes_normally ... ok
test tests_phase_2_1::test_resolve_no_includes ... ok
test tests_phase_2_1::test_resolve_partial_error ... ok
test tests_phase_2_1::test_resolve_success ... ok
test tests_phase_2_1::unbounded_field_not_referenced_is_cold ... ok
test tests_phase_2_1::unbounded_field_referenced_fails_resolution ... ok
test tests_phase_2_2::test_generate_aot_snippet_no_varlena ... ok
test tests_phase_2_2::test_generate_aot_snippet_typed ... ok
test tests_phase_3_0_page_mode_types::child_template_spec_shape ... ok
test tests_phase_3_0_page_mode_types::named_block_range_is_copy_half_open_and_arena_tagged ... ok
test tests_phase_3_0_page_mode_types::page_block_token_is_copy ... ok
test tests_phase_3_0_page_mode_types::phase_errors_are_distinct_types ... ok
test tests_phase_3_0_page_mode_types::static_partial_ref_has_no_len_field ... ok
test tests_phase_4_1_page_source_token::all_variants_are_copy ... ok
test tests_phase_4_1_page_source_token::constructs_all_four_variants ... ok
test tests_phase_4_1_page_source_token::match_is_exhaustive_without_wildcard ... ok
test tests_phase_4_1_page_source_token::page_source_token_layout_is_frozen ... ok
test tests_phase_4_2_detect_extends::empty_source_returns_false ... ok
test tests_phase_4_2_detect_extends::extends_after_leading_text_returns_false ... ok
test tests_phase_4_2_detect_extends::extends_at_head_returns_true ... ok
test tests_phase_4_2_detect_extends::if_at_head_returns_false ... ok
test tests_phase_4_2_detect_extends::no_block_delimiter_returns_false ... ok
test tests_phase_4_3_parse_page_tokens_runtime_subset::composition_keyword_out_of_scope_fails_explicitly ... ok
test tests_phase_4_3_parse_page_tokens_runtime_subset::runtime_subset_matches_parse_tokens_field_only ... ok
test tests_phase_4_3_parse_page_tokens_runtime_subset::runtime_subset_matches_parse_tokens_if_endif ... ok
test tests_phase_4_3_parse_page_tokens_runtime_subset::runtime_subset_matches_parse_tokens_static_only ... ok

test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

Doc-tests fragment-forge
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

(Baseline avant Phase 4.3 : 35/35 tests verts. Après Phase 4.3 : 39/39 —
4 tests ajoutés, conformément au jalon vert §4.3 de la roadmap : « égalité
stricte vérifiée sur au moins 3 fixtures », plus 1 test complémentaire sur
le comportement hors-scope. Zéro régression : les 35 tests pré-existants
restent verts, textuellement inchangés.)

=== cargo clippy --all-targets ===
warning: the following explicit lifetimes could be elided: 'src
--> src/lib.rs:1740:29
|
1740 | pub fn generate*aot_snippet<'src>(
| ^^^^
1741 | tokens: &[FlatPageToken<'src>],
| ^^^^
|
= help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#needless_lifetimes
= note: `#[warn(clippy::needless_lifetimes)]` on by default
help: elide the lifetimes
|
1740 ~ pub fn generate_aot_snippet(
1741 ~ tokens: &[FlatPageToken<'*>],
|

warning: `fragment-forge` (lib) generated 1 warning
Finished dev [unoptimized + debuginfo] target(s) in ...

Ce warning est antérieur à la Phase 4.3 : `generate_aot_snippet` est une
fonction gelée (Phase 2.2), non touchée par ce diff. Vérifié par
comparaison directe : même warning, même ligne, sur le fichier original
avant toute modification. Zéro nouveau warning clippy introduit par le
code de la Phase 4.3 (confirmé par `cargo clippy --all-targets -- -D
warnings`, qui échoue uniquement sur cette ligne pré-existante, avant et
après le diff).
