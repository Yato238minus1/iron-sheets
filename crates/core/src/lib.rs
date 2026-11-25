//! GUI-free core of iron sheets: app dirs and config, workspaces, text IO, lazy file tree,
//! link scanning, background index with persistent cache, fuzzy matching, content search,
//! file watching and scroll-sync mapping.
//!
//! Line numbers in this crate (`Heading::line`, `OutLink::line`, `SearchHit::line`) are 0-based.

#![allow(clippy::collapsible_if)]

pub mod config;
pub mod fuzzy;
pub mod glob;
pub mod index;
pub mod kind;
pub mod links;
pub mod ops;
pub mod paths;
pub mod relink;
pub mod scrollsync;
pub mod search;
pub mod themes;
pub mod textio;
pub mod tree;
pub mod untitled;
pub mod watch;
pub mod workspace;

pub use kind::FileKind;
