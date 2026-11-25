//! GUI-free Markdown layer of iron sheets.
//!
//! * [`parse()`] / [`model`]: GitHub-flavored Markdown + Obsidian extras (via comrak) flattened
//!   into a list of [`Block`]s with source line ranges and content hashes.
//! * [`highlight()`]: a fast line scanner that colors Markdown source for the editor.
//! * [`edit`]: pure text-editing helpers (list continuation, indentation, wrapping, ...).

pub mod edit;
#[cfg(feature = "excalidraw")]
pub mod excalidraw;
pub mod highlight;
mod html;
mod html_block;
mod inline;
#[cfg(feature = "kanban")]
pub mod kanban;
pub mod math;
pub mod model;
pub mod parse;

pub use edit::*;
pub use highlight::{HlSpan, IncrementalHighlighter, Tok, WindowHighlighter, diff_texts, highlight};
pub use model::*;
pub use parse::parse;
