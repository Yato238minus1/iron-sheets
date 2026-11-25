//! Typst preview pane. The implementation lives in `typst_ui/preview.rs` (feature `typst`).
#[cfg(not(feature = "pdf"))]
use crate::state::AppState;

#[cfg(feature = "typst")]
#[path = "../typst_ui/preview.rs"]
mod imp;
#[cfg(feature = "typst")]
pub use imp::{TypstPreview, poll};
#[cfg(feature = "pdf")]
pub use imp::export_pdf;

#[cfg(not(feature = "typst"))]
#[derive(Default)]
pub struct TypstPreview {}

#[cfg(not(feature = "typst"))]
impl TypstPreview {
    pub fn show(&mut self, ui: &mut egui::Ui, _state: &mut AppState, _tab_index: usize) {
        ui.label("Typst support is not built in (cargo feature `typst`).");
    }
}

/// Per-frame housekeeping (no-op without Typst).
#[cfg(not(feature = "typst"))]
pub fn poll(_state: &mut AppState, _ctx: &egui::Context) {}

/// `Command::ExportPdf` (only reachable through a keymap entry: the palette entry needs the feature).
#[cfg(not(feature = "pdf"))]
pub fn export_pdf(state: &mut AppState, _waker: &std::sync::Arc<dyn crate::platform::Waker>) {
    state.status.error("PDF export is not built in (cargo feature `pdf`)");
}
