//! Preview pane: dispatches on the document kind.
#[cfg(feature = "excalidraw")]
pub mod excalidraw;
pub mod markdown;
pub mod popup;
pub mod typst;

#[cfg(feature = "excalidraw")]
pub use excalidraw::ExcalidrawPreview;

use crate::state::AppState;
use iron_sheets_core::FileKind;
use std::path::PathBuf;

/// Image preview state (decoding runs on a background thread).
#[derive(Default)]
pub struct ImagePreview {
    path: Option<PathBuf>,
    texture: Option<egui::TextureHandle>,
    rx: Option<crossbeam_channel::Receiver<Result<egui::ColorImage, String>>>,
    error: Option<String>,
}

impl ImagePreview {
    pub fn in_flight(&self) -> usize {
        usize::from(self.rx.as_ref().is_some_and(|rx| rx.is_empty()))
    }

    fn start_load(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        self.path = Some(path.to_path_buf());
        self.texture = None;
        self.error = None;
        let (tx, rx) = crossbeam_channel::unbounded();
        self.rx = Some(rx);
        let (p, ctx) = (path.to_path_buf(), ctx.clone());
        let spawned = std::thread::Builder::new().name("iron-sheets-img".into()).spawn(move || {
            let r = image::open(&p).map_err(|e| e.to_string()).map(|img| {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            self.rx = None;
            self.error = Some(format!("Cannot start image decoder: {e}"));
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok(Ok(img)) => {
                self.texture = Some(ctx.load_texture("image_preview", img, Default::default()));
                self.rx = None;
            }
            Ok(Err(e)) => {
                self.error = Some(e);
                self.rx = None;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.error = Some("Image decoding failed unexpectedly".into());
                self.rx = None;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
    let Some(tab) = state.tabs.tabs.get(tab_index) else { return };
    let path = tab.path.clone();
    let kind = state.docs.get(&path).map_or(FileKind::from_path(&path), |d| d.kind);
    match kind {
        FileKind::Markdown => {
            let mut p = std::mem::take(&mut state.ui.md_preview);
            p.show(ui, state, tab_index);
            state.ui.md_preview = p;
        }
        FileKind::Typst => {
            let mut p = std::mem::take(&mut state.ui.typst_preview);
            p.show(ui, state, tab_index);
            state.ui.typst_preview = p;
        }
        FileKind::Image => {
            let anim = state.config.animations;
            show_image(ui, &mut state.ui.image_preview, &path, anim);
        }
        #[cfg(feature = "excalidraw")]
        FileKind::Excalidraw => {
            let mut p = std::mem::take(&mut state.ui.excalidraw_preview);
            p.show(ui, state, tab_index);
            state.ui.excalidraw_preview = p;
        }
        #[cfg(not(feature = "excalidraw"))]
        FileKind::Excalidraw => {}
        FileKind::Other => {}
    }
}

fn show_image(ui: &mut egui::Ui, st: &mut ImagePreview, path: &std::path::Path, animations: bool) {
    if st.path.as_deref() != Some(path) {
        st.start_load(ui.ctx(), path);
    }
    st.poll(ui.ctx());
    if let Some(e) = &st.error {
        let msg = format!("Cannot show image: {e}");
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.colored_label(ui.visuals().error_fg_color, msg);
            ui.add_space(8.0);
            if ui.button("Retry").clicked() {
                st.start_load(ui.ctx(), path);
            }
        });
    } else if let Some(tex) = &st.texture {
        let avail = ui.available_size();
        let sz = tex.size_vec2();
        let scale = (avail.x / sz.x).min(avail.y / sz.y).clamp(0.01, 1.0);
        ui.centered_and_justified(|ui| {
            ui.image((tex.id(), sz * scale));
        });
    } else {
        let pulse = ui.style().animation_time > 0.0 && animations;
        let fill = crate::skeleton::fill(ui, pulse);
        crate::skeleton::image_box(ui, ui.available_rect_before_wrap(), fill, ui.visuals().widgets.noninteractive.bg_stroke, crate::theme::tokens::RADIUS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_channel_sets_error_and_clears_rx() {
        let ctx = egui::Context::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut st = ImagePreview { path: Some(PathBuf::from("x.png")), rx: Some(rx), ..Default::default() };
        assert_eq!(st.in_flight(), 1);
        drop(tx);
        st.poll(&ctx);
        assert!(st.rx.is_none());
        assert_eq!(st.in_flight(), 0);
        assert!(st.error.as_deref().is_some_and(|e| e.contains("failed unexpectedly")));
    }

    #[test]
    fn ok_and_err_results_clear_rx() {
        let ctx = egui::Context::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut st = ImagePreview { path: Some(PathBuf::from("x.png")), rx: Some(rx), ..Default::default() };
        tx.send(Err("corrupt".into())).unwrap();
        st.poll(&ctx);
        assert!(st.rx.is_none() && st.in_flight() == 0);
        assert_eq!(st.error.as_deref(), Some("corrupt"));

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut st = ImagePreview { path: Some(PathBuf::from("x.png")), rx: Some(rx), ..Default::default() };
        tx.send(Ok(egui::ColorImage::example())).unwrap();
        st.poll(&ctx);
        assert!(st.rx.is_none() && st.in_flight() == 0);
        assert!(st.texture.is_some() && st.error.is_none());
    }
}
