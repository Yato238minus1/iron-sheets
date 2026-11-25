//! Framework-free core of the iron sheets application: state, commands, documents, tabs, scroll contract, keymap and
//! the view-models the frontends draw. No GUI toolkit dependency; a frontend supplies `AppState<U>`'s `ui` field.
#![allow(clippy::collapsible_if)]
#[cfg(feature = "calendar")]
pub mod calendar;
pub mod command;
pub mod dialogs;
pub mod docs;
pub mod effective;
pub mod editor;
#[cfg(feature = "git")]
pub mod git;
#[cfg(feature = "git")]
pub mod git_hunks;
pub mod keymap;
pub mod keys;
pub mod modules;
pub mod newtab;
pub mod palette_model;
pub mod perf;
pub mod platform;
pub mod prefetch;
mod saver;
pub mod scrollsync;
pub mod services;
pub mod settings_model;
pub mod sidebar;
pub mod state;
pub mod statusbar_model;
pub mod tabs;
#[cfg(test)]
pub(crate) mod test_support;
pub mod theme_model;

pub use state::{AppOptions, AppState};
