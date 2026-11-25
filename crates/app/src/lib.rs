//! iron sheets: thin crate that selects the frontend by cargo feature (`ui-egui` is the default; `ui-xilem` is a placeholder)
//! and re-exports the application API (`App`, `AppOptions`, the state, commands and modules the UI tests use).
#[cfg(all(feature = "ui-egui", feature = "ui-xilem"))]
compile_error!("features `ui-egui` and `ui-xilem` are mutually exclusive: build with exactly one frontend (e.g. `--no-default-features --features ui-xilem`)");

#[cfg(not(any(feature = "ui-egui", feature = "ui-xilem")))]
compile_error!("no frontend selected: enable exactly one of the cargo features `ui-egui` (default) or `ui-xilem`");

#[cfg(feature = "ui-egui")]
pub use iron_sheets_ui_egui::*;

/// The framework-free core, for code that does not need a frontend.
pub use iron_sheets_app_core as core;
