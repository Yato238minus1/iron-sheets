//! Extras of the empty-tab page: a dimmed background image and a random quote at the bottom. Both are skipped in
//! professional mode (see `iron_sheets_app_core::effective`).
use crate::preview::markdown::images::{ImageCache, ImgInfo};
use crate::state::AppState;
use egui::{Align, Color32, Layout, Rect, UiBuilder, pos2};
use iron_sheets_app_core::newtab::{Quote, pick, seed};
use std::path::PathBuf;

#[derive(Default)]
pub struct NewTabUi {
    /// The quote of the tab shown last: (tab id, quotes file it came from, quote). Picked once per tab, not per frame.
    quote: Option<(u64, String, Quote)>,
    images: ImageCache,
    image_path: PathBuf,
    /// Image path whose failure was already reported.
    image_failed: PathBuf,
}

/// Paints under the page's widgets: call before them with the tab's id.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, tab_id: u64) {
    let (image, quotes, file) = {
        let e = state.ui.eff.get(&state.config, state.settings.theme.as_ref(), (state.config_gen, state.settings_gen));
        // A relative path is relative to the config folder.
        let abs = |p: &str| if p.trim().is_empty() { String::new() } else { state.dirs.config.join(p.trim()).display().to_string() };
        (abs(&e.newtab_image), e.newtab_quotes, abs(&e.newtab_quotes_file))
    };
    let full = ui.max_rect();
    let n = &mut state.ui.newtab;
    // Another path (or none): drop the old cache and its texture.
    if n.image_path.as_os_str() != image.as_str() {
        n.images = ImageCache::default();
        n.image_path = PathBuf::from(&image);
    }
    if !image.is_empty() {
        n.images.poll(ui.ctx());
        let path = n.image_path.clone();
        // Nothing is drawn until the decode (a worker thread) is done; a failure is reported once.
        if let ImgInfo::Failed(e) = n.images.info(&path) {
            if n.image_failed != path {
                n.image_failed = path.clone();
                state.status.error(format!("New-tab background image {image}: {e}"));
            }
        } else if let Some(tex) = state.ui.newtab.images.texture(&path) {
            let [w, h] = tex.size();
            let k = (full.width() / w as f32).max(full.height() / h as f32);
            let (fu, fv) = (full.width() / (w as f32 * k), full.height() / (h as f32 * k));
            let uv = Rect::from_min_max(pos2((1.0 - fu) / 2.0, (1.0 - fv) / 2.0), pos2((1.0 + fu) / 2.0, (1.0 + fv) / 2.0));
            // Faded into the page colour so the text on top stays readable.
            ui.painter_at(full).image(tex.id(), full, uv, Color32::from_white_alpha(60));
        }
    }
    if !quotes {
        return;
    }
    let stale = state.ui.newtab.quote.as_ref().is_none_or(|(id, f, _)| *id != tab_id || *f != file);
    if stale {
        // shortcut: the quotes file is read on the UI thread once per new tab; a huge or network file would stall it.
        let (q, err) = pick(&file, seed());
        if let Some(e) = err {
            state.status.error(e);
        }
        state.ui.newtab.quote = q.map(|q| (tab_id, file, q));
    }
    let Some((_, _, q)) = &state.ui.newtab.quote else { return };
    let w = (full.width() - 40.0).clamp(0.0, 640.0);
    let area = Rect::from_min_max(pos2(full.center().x - w / 2.0, full.bottom() - 120.0), pos2(full.center().x + w / 2.0, full.bottom() - 14.0));
    // A child ui, so the page's own layout does not move.
    let mut ui = ui.new_child(UiBuilder::new().max_rect(area).layout(Layout::bottom_up(Align::Center)));
    if let Some(a) = &q.author {
        ui.label(egui::RichText::new(format!("— {a}")).weak());
    }
    ui.label(egui::RichText::new(&q.text).italics());
}
