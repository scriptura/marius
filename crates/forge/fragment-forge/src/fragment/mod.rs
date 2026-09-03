// crates/forge/fragment-forge/src/fragment/mod.rs

//! Pipeline Mode Fragment (template unique, sans héritage) :
//! `token` → `lexer` → `parser` → `validator` → `resolver` → `codegen`,
//! plus deux passes de post-traitement sur `FlatPageToken` déjà résolu
//! (`static_markers`, `script_hoisting`).

pub mod codegen;
pub mod lexer;
pub mod parser;
pub mod resolver;
pub mod script_hoisting;
pub mod static_markers;
pub mod token;
pub mod validator;

pub use codegen::{generate_aot_snippet, generate_segmented_snippet, generated_file_header};
pub use lexer::{RawSpan, SpanKind, scan};
pub use parser::{PageParseError, parse_tokens};
pub use resolver::{AssetLookup, ResolverError, TemplateMetrics, resolve_and_measure};
pub use script_hoisting::{HoistError, hoist_and_dedupe_scripts, splice_hoisted_scripts};
pub use static_markers::{
    StaticMarkerFacts, extract_static_class_tokens, extract_static_data_attribute_tokens,
    extract_static_element_tokens, extract_static_id_tokens, extract_static_marker_facts,
};
pub use token::FlatPageToken;
pub use validator::{SemanticError, validate_ast};
