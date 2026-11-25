//! The egui frontend's part of the application state, and the `AppState` alias the widgets use.
use crate::editor::{EditorState, features::EditorFeatures};
use crate::palette::Palette;
#[cfg(feature = "excalidraw")]
use crate::preview::ExcalidrawPreview;
use crate::preview::{ImagePreview, markdown::MarkdownPreview, typst::TypstPreview};
use crate::sidebar::{files::FilesPanel, links::LinksPanel, outline::OutlinePanel, search::SearchPanel, symbols::SymbolsPanel};
use crate::theme::Theme;
pub use iron_sheets_app_core::state::*;

/// Module states of the egui widgets (`AppState::ui`).
#[derive(Default)]
pub struct EguiUi {
    pub theme: Theme,
    pub search: SearchPanel,
    pub outline: OutlinePanel,
    pub links: LinksPanel,
    pub symbols: SymbolsPanel,
    #[cfg(feature = "git")]
    pub git: crate::sidebar::git::GitPanel,
    #[cfg(feature = "git")]
    pub git_dialogs: crate::git_dialogs::GitDialogs,
    #[cfg(feature = "git")]
    pub git_hunk: crate::git_hunk::HunkPopup,
    #[cfg(feature = "calendar")]
    pub calendar: iron_sheets_app_core::calendar::CalendarModel,
    pub palette: Palette,
    pub md_preview: MarkdownPreview,
    pub page_popup: crate::preview::popup::PagePopup,
    pub typst_preview: TypstPreview,
    pub editor_features: EditorFeatures,
    pub editor: EditorState,
    pub files: FilesPanel,
    pub image_preview: ImagePreview,
    pub newtab: crate::newtab::NewTabUi,
    pub eff: iron_sheets_app_core::effective::EffCache,
    #[cfg(feature = "excalidraw")]
    pub excalidraw_preview: ExcalidrawPreview,
}

/// Application state with egui module states. Shadows the generic core type for everything in this crate.
pub type AppState = iron_sheets_app_core::state::AppState<EguiUi>;

/// Builds the state and applies the egui-side settings (nothing beyond what the core applies at the moment).
pub fn new_state(waker: std::sync::Arc<dyn iron_sheets_app_core::platform::Waker>, opts: AppOptions) -> AppState {
    AppState::new(waker, opts, EguiUi::default())
}
