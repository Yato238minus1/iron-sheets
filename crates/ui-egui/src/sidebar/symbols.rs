//! Symbols sidebar panel: draw a symbol to find it, or search by name. Implementation in `typst_ui/symbols.rs`.
#[cfg(not(feature = "typst"))]
use crate::state::AppState;

#[cfg(feature = "typst")]
#[path = "../typst_ui/symbols.rs"]
mod imp;
#[cfg(feature = "typst")]
pub use imp::{InsertMode, SymbolsPanel, insertion_text, iron_sheets_assets_fonts, typst_in_math};

#[cfg(not(feature = "typst"))]
#[derive(Default)]
pub struct SymbolsPanel {}

#[cfg(not(feature = "typst"))]
impl SymbolsPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, _state: &mut AppState) {
        ui.label("Typst support is not built in (cargo feature `typst`), so the symbol panel is unavailable.");
    }
}
