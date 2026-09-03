// crates/forge/fragment-forge/src/page/mod.rs

//! Pipeline Mode Page (héritage `extends`/`block`) :
//! `model` → `token` → `parser` → `blocks` → `linker` → `lowering`.
//! Point de jonction unique avec le pipeline Fragment : `lowering::lower`
//! produit un `Vec<FlatPageToken>` qui réintègre `fragment::resolver` /
//! `fragment::codegen`.

pub mod blocks;
pub mod linker;
pub mod lowering;
pub mod model;
pub mod parser;
pub mod token;

pub use blocks::collect_blocks;
pub use linker::{BlockSubstitution, LinkPlan, collect_static_refs, link};
pub use lowering::lower;
pub use model::{
    ChildTemplateSpec, NamedBlockRange, PageArena, PageBlockToken, PageComposeParseError,
    PageLinkError, PageValidationError, ParsedPageTemplate, StaticPartialRef, TemplateId,
};
pub use parser::{detect_extends, parse_page_tokens};
pub use token::PageSourceToken;
