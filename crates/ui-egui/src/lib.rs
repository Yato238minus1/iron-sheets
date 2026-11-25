//! egui/eframe frontend of iron sheets: all widget code, layout caches and the frame loop.
//! The framework-free state and commands are in `iron-sheets-app-core`; modules that exist there too are re-exported
//! (and extended) here, so `crate::docs`, `crate::keys` and so on resolve the same way as before the split.
#![allow(clippy::collapsible_if)]
pub mod anim;
pub mod caret;
pub mod app;
pub mod aux_window;
pub mod command;
pub mod dialogs;
#[cfg(feature = "git")]
pub mod git_dialogs;
#[cfg(feature = "git")]
pub mod git_hunk;
pub mod editor;
pub mod elide;
pub mod icons;
pub mod ime_filter;
#[cfg(feature = "kanban")]
pub mod kanban;
pub mod keys_egui;
pub mod menu;
pub mod newtab;
pub mod palette;
pub mod perf;
pub mod platform_egui;
pub mod preview;
pub mod scrollsync_egui;
pub mod settings;
pub(crate) mod skeleton;
pub mod sidebar;
pub mod state;
pub mod statusbar;
pub mod sysfont;
pub mod tabstrip;
pub mod theme;
pub mod titlebar;
pub mod viewer;

pub use iron_sheets_app_core::{docs, keymap, keys, palette_model, platform, scrollsync, services, settings_model, statusbar_model, tabs, theme_model};

pub use app::{App, RunArgs, RunResult, run};
pub use state::{AppOptions, AppState};
