// crates/forge/fragment-forge/src/lib.rs

//! # Marius Fragment Forge
//!
//! Génération AOT (`build.rs`) du corps de `render()` pour les tables surveillées.
//!
//! Produit une séquence déterministe d'appels (`push_str`, `write_fmt`, `marius_html_escape`)
//! s'exécutant sur une capacité statiquement bornée (`STATIC_CAP + DYNAMIC_CAP`),
//! garantissant l'**absence totale de réallocation** sur le chemin critique.
//!
//! ## Taxonomie des Structures Générées
//!
//! | Structure | Layout | Rôle Mémoire & Invariants | Durée de vie |
//! | :--- | :--- | :--- | :--- |
//! | `{Name}Row` | Non-`repr(C)` | Transport `sqlx` (Base $\rightarrow$ Site de projection). Varlenas portées via `Option<String>` (allocations heap). | Éphémère (détruite après `render()`) |
//! | `{Name}StorageRow` | `#[repr(C)]` | Stockage contigu en mémoire. Types à taille fixe uniquement (alignés sur DDL). Exclut les varlenas (incompatibles : *fat pointer* de 16 B). | Persistante (cache CPU-friendly) |
//! | `{Name}RenderPayload` | Non-`repr(C)` | Struct de rendu éphémère. Emprunte les varlenas (`&'a str`) depuis la `Row` sans copie ni allocation. | Limitée à `render()` (`'a`) |
//!
//! ## Chemin Critique & Invariants (`no_std` attitude)
//!
//! ```text
//! StorageRow (repr(C)) + RenderPayload (&'a str)  ==>  render()  ==>  buf: &mut String
//! ```
//!
//! - **Garantie de capacité :** `buf.capacity()` doit rester strictement identique avant et après `render()`.
//! - **Zéro logique dynamique :** Aucun branchement (`if`/`match`) dans le template généré.
//! - **Borne exacte :** `STATIC_CAP` (octets fixes) + `DYNAMIC_CAP` (largeurs pires cas).
//!
//! ## Politique d'Échappement HTML (`EscapePolicy`)
//!
//! - `FieldPolicy::Normal` : Taille $\times 5$ (pire cas : `&` $\rightarrow$ `&amp;`).
//! - `FieldPolicy::PreEscaped` : Taille $\times 1$ (Tag DDL `marius:pre_escaped`).
//! - `FieldPolicy::Raw` : Taille $\times 1$, injecté sans passage par l'échappeur runtime (Tag DDL `marius:raw`).
//!
//! *Référence : ADR-002 (`no_std-attitude-within-marius.md`)*

pub mod fragment;
pub mod naming;
pub mod page;
pub mod schema;

pub use fragment::{
    AssetLookup, FlatPageToken, HoistError, PageParseError, RawSpan, ResolverError, SemanticError,
    SpanKind, StaticMarkerFacts, TemplateMetrics, extract_static_class_tokens,
    extract_static_data_attribute_tokens, extract_static_element_tokens, extract_static_id_tokens,
    extract_static_marker_facts, generate_aot_snippet, generate_segmented_snippet,
    generated_file_header, hoist_and_dedupe_scripts, parse_tokens, resolve_and_measure, scan,
    splice_hoisted_scripts, validate_ast,
};
pub use naming::{relative_path_for_include_str, static_capacity, static_const_ident};
pub use page::{
    BlockSubstitution, ChildTemplateSpec, LinkPlan, NamedBlockRange, PageArena, PageBlockToken,
    PageComposeParseError, PageLinkError, PageSourceToken, PageValidationError, ParsedPageTemplate,
    StaticPartialRef, TemplateId, collect_blocks, collect_static_refs, detect_extends, link, lower,
    parse_page_tokens,
};
pub use schema::{EscapePolicy, FieldKind, FieldSpec, SchemaIndex, VarlenField};
