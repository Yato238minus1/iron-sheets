//! Excalidraw preview (`.excalidraw` and `.excalidraw.md`): background SVG generation + `resvg` rasterization,
//! pan/zoom viewer, toolbar, and M2 UX loading/empty/error+Retry states.

use crate::preview::markdown::images::DEBOUNCE;
use crate::state::AppState;
use crate::viewer::{self, View};
use crossbeam_channel::{Receiver, TryRecvError, unbounded};
use egui::{ColorImage, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use iron_sheets_app_core::command::Command;
use iron_sheets_core::config::ViewMode;
use iron_sheets_core::index::Index;
use iron_sheets_md::excalidraw::{ExcalidrawError, RenderOptions, render_to_svg};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const MAX_SIDE: f32 = 2048.0;
const ZOOM_SIDE: f32 = 8192.0;
const ZOOM_PIXELS: f32 = 32_000_000.0;
const MAX_EMBED_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const STACK: usize = 16 * 1024 * 1024;
const ZOOM_DEBOUNCE: Duration = Duration::from_millis(250);
const MAX_STUCK: isize = 2;

const RUNNING: u8 = 0;
const ENDED: u8 = 1;
const TIMED_OUT: u8 = 2;

static STUCK: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ScaleMode {
    #[default]
    Fit,
    Actual,
}

#[derive(Clone)]
pub(crate) struct RasterJob {
    pub path: PathBuf,
    pub source: Option<String>,
    pub dark: bool,
    pub w_pts: f32,
    pub ppp: f32,
    pub px_w: Option<f32>,
    pub index: Arc<RwLock<Index>>,
}

pub(crate) struct Rasterized {
    pub dims: [u32; 2],
    pub image: ColorImage,
}

pub(crate) enum RenderError {
    Empty,
    Failed { message: String, transient: bool },
}

enum EmbedMime {
    Png,
    Jpeg,
    Webp,
    Gif,
}

type EmbedMap = FxHashMap<String, (EmbedMime, Arc<Vec<u8>>)>;

fn sniff_mime(path: &Path, bytes: &[u8]) -> Option<EmbedMime> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some(EmbedMime::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(EmbedMime::Jpeg);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(EmbedMime::Webp);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(EmbedMime::Gif);
    }
    match path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("png") => Some(EmbedMime::Png),
        Some("jpg" | "jpeg") => Some(EmbedMime::Jpeg),
        Some("webp") => Some(EmbedMime::Webp),
        Some("gif") => Some(EmbedMime::Gif),
        _ => None,
    }
}

fn resolve_vault_embeds(index: &Arc<RwLock<Index>>, drawing_path: &Path, refs: &[iron_sheets_md::excalidraw::EmbeddedFileRef]) -> EmbedMap {
    let mut resolved_paths: Vec<(String, PathBuf)> = Vec::new();
    if !refs.is_empty() {
        let ix = index.read();
        for r in refs {
            if let Some(p) = ix.resolve_file(&r.target, Some(drawing_path)) {
                resolved_paths.push((r.file_id.clone(), p));
            }
        }
    }
    let mut out = FxHashMap::default();
    for (id, p) in resolved_paths {
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        if !meta.is_file() || meta.len() > MAX_EMBED_IMAGE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        if let Some(mime) = sniff_mime(&p, &bytes) {
            out.insert(id, (mime, Arc::new(bytes)));
        }
    }
    out
}

fn usvg_options(fonts: Arc<resvg::usvg::fontdb::Database>, embeds: Arc<EmbedMap>) -> resvg::usvg::Options<'static> {
    let embeds_for_str = embeds.clone();
    let href = resvg::usvg::ImageHrefResolver {
        resolve_data: Box::new(|mime, data, _| match mime.to_ascii_lowercase().as_str() {
            "image/png" => Some(resvg::usvg::ImageKind::PNG(data)),
            "image/jpeg" | "image/jpg" => Some(resvg::usvg::ImageKind::JPEG(data)),
            "image/webp" => Some(resvg::usvg::ImageKind::WEBP(data)),
            "image/gif" => Some(resvg::usvg::ImageKind::GIF(data)),
            _ => None,
        }),
        resolve_string: Box::new(move |href, _| {
            let id = href.strip_prefix("vault-embed://")?;
            let (mime, bytes) = embeds_for_str.get(id)?;
            Some(match mime {
                EmbedMime::Png => resvg::usvg::ImageKind::PNG(bytes.clone()),
                EmbedMime::Jpeg => resvg::usvg::ImageKind::JPEG(bytes.clone()),
                EmbedMime::Webp => resvg::usvg::ImageKind::WEBP(bytes.clone()),
                EmbedMime::Gif => resvg::usvg::ImageKind::GIF(bytes.clone()),
            })
        }),
    };
    resvg::usvg::Options { fontdb: fonts, image_href_resolver: href, ..Default::default() }
}

fn rasterise_now(job: &RasterJob, fonts: Arc<resvg::usvg::fontdb::Database>) -> Result<Rasterized, RenderError> {
    let owned_source;
    let source = match job.source.as_deref() {
        Some(s) => s,
        None => {
            let loaded = iron_sheets_core::textio::load_text_bounded(&job.path, iron_sheets_md::excalidraw::MAX_INPUT_BYTES as u64)
                .map_err(|e| RenderError::Failed { message: e.to_string(), transient: false })?;
            owned_source = loaded.text;
            &owned_source
        }
    };

    let rendered = render_to_svg(source, &RenderOptions { dark: job.dark, roughness: true }).map_err(|e| match e {
        ExcalidrawError::EmptyScene => RenderError::Empty,
        other => RenderError::Failed { message: other.to_string(), transient: false },
    })?;

    let embeds = Arc::new(resolve_vault_embeds(&job.index, &job.path, &rendered.embedded_files));
    let opts = usvg_options(fonts, embeds);
    let tree = resvg::usvg::Tree::from_str(&rendered.svg, &opts).map_err(|e| RenderError::Failed { message: e.to_string(), transient: false })?;

    let nw = tree.size().width().max(1.0);
    let nh = tree.size().height().max(1.0);
    let scale = match job.px_w {
        Some(px) => (px / nw).min(ZOOM_SIDE / nw).min(ZOOM_SIDE / nh).min((ZOOM_PIXELS / (nw * nh)).sqrt()),
        None if job.w_pts > 0.0 => ((job.w_pts / nw).min(1.0) * job.ppp).min(MAX_SIDE / nw).min(MAX_SIDE / nh),
        None => job.ppp.min(MAX_SIDE / nw).min(MAX_SIDE / nh),
    };
    let (pw, ph) = ((nw * scale).ceil().max(1.0) as u32, (nh * scale).ceil().max(1.0) as u32);
    let mut pm =
        resvg::tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| RenderError::Failed { message: "drawing too large to rasterize".into(), transient: false })?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    let image = ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
    Ok(Rasterized { dims: [nw.ceil() as u32, nh.ceil() as u32], image })
}

pub(crate) fn rasterise_job(job: &RasterJob) -> Result<Rasterized, RenderError> {
    let fonts = crate::preview::markdown::svgfonts::fonts();
    if STUCK.load(Ordering::SeqCst) >= MAX_STUCK {
        return Err(RenderError::Failed { message: "too many drawings still rendering".into(), transient: true });
    }
    let (tx, rx) = mpsc::channel();
    let state = Arc::new(AtomicU8::new(RUNNING));
    let (job_c, st) = (job.clone(), state.clone());
    let spawned = std::thread::Builder::new().name("iron-sheets-excalidraw-resvg".into()).stack_size(STACK).spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rasterise_now(&job_c, fonts)))
            .unwrap_or_else(|_| Err(RenderError::Failed { message: "Excalidraw renderer crashed".into(), transient: false }));
        let _ = tx.send(r);
        if st.swap(ENDED, Ordering::SeqCst) == TIMED_OUT {
            STUCK.fetch_sub(1, Ordering::SeqCst);
        }
    });
    if spawned.is_err() {
        return Err(RenderError::Failed { message: "could not start Excalidraw render thread".into(), transient: true });
    }
    match rx.recv_timeout(TIMEOUT) {
        Ok(r) => r,
        Err(_) => {
            if state.compare_exchange(RUNNING, TIMED_OUT, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                STUCK.fetch_add(1, Ordering::SeqCst);
            }
            Err(RenderError::Failed { message: "Excalidraw render timed out".into(), transient: false })
        }
    }
}

#[derive(Default)]
pub struct ExcalidrawPreview {
    path: Option<PathBuf>,
    doc_version: u64,
    dark: bool,
    ppp: f32,
    mode: ScaleMode,
    view: View,
    natural_size: [f32; 2],
    base_tex: Option<TextureHandle>,
    zoom_tex: Option<(u32, TextureHandle)>,
    pending_zoom: Option<(u32, Instant)>,
    rx: Option<Receiver<Result<Rasterized, RenderError>>>,
    zoom_rx: Option<(u32, Receiver<Result<Rasterized, RenderError>>)>,
    empty: bool,
    error: Option<String>,
}

impl ExcalidrawPreview {
    pub fn in_flight(&self) -> usize {
        usize::from(self.rx.as_ref().is_some_and(|r| r.is_empty())) + usize::from(self.zoom_rx.as_ref().is_some_and(|(_, r)| r.is_empty()))
    }

    #[allow(clippy::too_many_arguments)]
    fn start_render_with_index(&mut self, ctx: &egui::Context, path: &Path, source: String, version: u64, dark: bool, ppp: f32, index: Arc<RwLock<Index>>) {
        let same_path = self.path.as_deref() == Some(path);
        self.path = Some(path.to_path_buf());
        self.doc_version = version;
        self.dark = dark;
        self.ppp = ppp;
        self.zoom_tex = None;
        self.zoom_rx = None;
        self.pending_zoom = None;
        self.error = None;
        self.empty = false;
        if !same_path {
            self.base_tex = None;
            self.view = View::default();
            self.mode = ScaleMode::Fit;
            self.natural_size = [0.0, 0.0];
        }
        crate::preview::markdown::svgfonts::warm_fonts();
        let (tx, rx) = unbounded();
        self.rx = Some(rx);
        let ctx_c = ctx.clone();
        let p_c = path.to_path_buf();
        let spawned = std::thread::Builder::new().name("iron-sheets-excalidraw".into()).spawn(move || {
            let job = RasterJob { path: p_c, source: Some(source), dark, w_pts: 0.0, ppp, px_w: None, index };
            let res = rasterise_job(&job);
            let _ = tx.send(res);
            ctx_c.request_repaint();
        });
        if let Err(e) = spawned {
            self.rx = None;
            self.error = Some(format!("Cannot start Excalidraw renderer: {e}"));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_zoom_render(&mut self, ctx: &egui::Context, path: &Path, source: String, dark: bool, ppp: f32, bucket: u32, index: Arc<RwLock<Index>>) {
        let (tx, rx) = unbounded();
        self.zoom_rx = Some((bucket, rx));
        let ctx_c = ctx.clone();
        let p_c = path.to_path_buf();
        let spawned = std::thread::Builder::new().name("iron-sheets-excalidraw-zoom".into()).spawn(move || {
            let job = RasterJob { path: p_c, source: Some(source), dark, w_pts: 0.0, ppp, px_w: Some(bucket as f32), index };
            let res = rasterise_job(&job);
            let _ = tx.send(res);
            ctx_c.request_repaint();
        });
        if spawned.is_err() {
            self.zoom_rx = None;
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(Ok(r)) => {
                    self.natural_size = [r.dims[0] as f32, r.dims[1] as f32];
                    self.base_tex = Some(ctx.load_texture("excalidraw_preview", r.image, TextureOptions::LINEAR));
                    self.empty = false;
                    self.error = None;
                    self.rx = None;
                }
                Ok(Err(RenderError::Empty)) => {
                    self.base_tex = None;
                    self.zoom_tex = None;
                    self.empty = true;
                    self.error = None;
                    self.rx = None;
                }
                Ok(Err(RenderError::Failed { message, .. })) => {
                    self.error = Some(message);
                    self.rx = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.error = Some("Excalidraw rendering failed unexpectedly".into());
                    self.rx = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some((bucket, rx)) = &self.zoom_rx {
            let bucket = *bucket;
            match rx.try_recv() {
                Ok(Ok(r)) => {
                    self.zoom_tex = Some((bucket, ctx.load_texture("excalidraw_zoom", r.image, TextureOptions::LINEAR)));
                    self.zoom_rx = None;
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                    self.zoom_rx = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
        let Some((path, _is_split)) = state.tabs.tabs.get(tab_index).map(|t| (t.path.clone(), t.view == ViewMode::Split)) else {
            return;
        };
        if let Some(tab) = state.tabs.tabs.get_mut(tab_index) {
            // Excalidraw pans/zooms freely; clear any residual scrollsync force so idle repaint stays 0.
            tab.preview_scroll.force = None;
            tab.preview_scroll.content_h = 0.0;
            tab.preview_scroll.viewport_h = 0.0;
        }
        let Some((version, last_edit)) = state.docs.get(&path).map(|d| (d.version, d.last_edit)) else {
            return;
        };
        // Cloned only when a render starts, never per frame.
        let source = |state: &AppState| state.docs.get(&path).map(|d| d.text.clone()).unwrap_or_default();
        let dark = state.ui.theme.dark;
        let ppp = ui.ctx().pixels_per_point();
        let index = state.indexer.index();

        let restyle = self.path.as_deref() != Some(path.as_path()) || self.dark != dark || (self.ppp - ppp).abs() > 0.01;
        if restyle || self.doc_version != version {
            // Edits render after the Mermaid debounce; the previous render stays on screen meanwhile.
            let since = last_edit.elapsed();
            if restyle || since >= DEBOUNCE {
                self.start_render_with_index(ui.ctx(), &path, source(state), version, dark, ppp, index.clone());
            } else {
                ui.ctx().request_repaint_after(DEBOUNCE - since);
            }
        }
        self.poll(ui.ctx());

        let mut cmds: Vec<Command> = Vec::new();
        let size = Vec2::from(self.natural_size).max(Vec2::splat(1.0));
        let mut retry_clicked = false;
        let mut fit_clicked = false;
        let mut actual_clicked = false;
        let mut zoom_step: Option<f32> = None;

        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if ui.selectable_label(self.mode == ScaleMode::Fit && (self.view.zoom - 1.0).abs() < 0.02, "Fit").clicked() {
                fit_clicked = true;
            }
            if ui.selectable_label(self.mode == ScaleMode::Actual, "100%").clicked() {
                actual_clicked = true;
            }
            if ui.button("-").on_hover_text("Zoom out").clicked() {
                zoom_step = Some(0.8);
            }
            let pct = (self.view.zoom * 100.0).round() as u32;
            ui.label(egui::RichText::new(format!("{pct}%")).weak().monospace());
            if ui.button("+").on_hover_text("Zoom in").clicked() {
                zoom_step = Some(1.25);
            }
            ui.separator();
            if ui.button("Source").on_hover_text("Edit drawing source (Ctrl+E)").clicked() {
                cmds.push(Command::SetViewMode(ViewMode::Source));
            }
            // Editing stays external (plan section 6): the system handler or an Excalidraw plugin takes over; the watcher reloads.
            if ui.button("Edit externally").on_hover_text("Open in the default app").clicked() {
                cmds.push(Command::OpenExternal(path.clone()));
            }
        });
        ui.separator();

        if let Some(err) = &self.error
            && self.base_tex.is_some()
        {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().error_fg_color, format!("Render error (showing previous): {err}"));
                if ui.button("Retry").clicked() {
                    retry_clicked = true;
                }
                if ui.button("View source").clicked() {
                    cmds.push(Command::SetViewMode(ViewMode::Source));
                }
            });
            ui.separator();
        }

        let area = ui.available_rect_before_wrap();
        if area.width() <= 0.0 || area.height() <= 0.0 {
            state.commands.extend(cmds);
            return;
        }

        if fit_clicked {
            self.mode = ScaleMode::Fit;
            self.view = View::default();
        }
        if actual_clicked {
            self.mode = ScaleMode::Actual;
            let avail = (area.size() - Vec2::splat(48.0)).max(Vec2::splat(1.0));
            let fit_scale = (avail.x / size.x).min(avail.y / size.y).clamp(0.01, 1.0);
            self.view = View { zoom: (1.0 / fit_scale).clamp(0.1, 64.0), pan: Vec2::ZERO };
        }
        if let Some(factor) = zoom_step {
            self.mode = ScaleMode::Fit;
            self.view.zoom_at(area.center(), factor, area);
        }

        if self.empty {
            ui.vertical_centered(|ui| {
                ui.add_space((area.height() * 0.25).min(80.0));
                ui.label(egui::RichText::new("Empty Excalidraw drawing").heading());
                ui.add_space(6.0);
                ui.label(egui::RichText::new("This drawing has no visible elements.").weak());
                ui.add_space(10.0);
                if ui.button("View source").clicked() {
                    cmds.push(Command::SetViewMode(ViewMode::Source));
                }
            });
        } else if let Some(err) = &self.error
            && self.base_tex.is_none()
        {
            let msg = format!("Cannot show drawing: {err}");
            ui.vertical_centered(|ui| {
                ui.add_space((area.height() * 0.25).min(80.0));
                ui.colored_label(ui.visuals().error_fg_color, msg);
                ui.add_space(10.0);
                ui.allocate_ui_with_layout(egui::vec2(160.0, 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if ui.button("Retry").clicked() {
                        retry_clicked = true;
                    }
                    if ui.button("View source").clicked() {
                        cmds.push(Command::SetViewMode(ViewMode::Source));
                    }
                });
            });
        } else if let Some(base_tex_w) = self.base_tex.as_ref().map(|t| t.size()[0] as f32) {
            let frame_nr = ui.ctx().cumulative_frame_nr();
            ui.ctx().data_mut(|d| {
                d.insert_temp(crate::app::wheel_owner_id(), (area, frame_nr));
            });
            let bg = ui.interact(area, ui.scope_id().with(("exc-canvas", tab_index)), Sense::click_and_drag());
            let prev_view = self.view;
            viewer::pan_zoom(ui, &bg, &mut self.view, area, &|_| true);
            if self.view != prev_view && (self.view.zoom - 1.0).abs() < 0.01 && self.view.pan == Vec2::ZERO {
                self.mode = ScaleMode::Fit;
            }
            let r = self.view.rect(area, size, 1.0);
            let need_px = (r.width() * ppp).ceil();
            if need_px > base_tex_w * 1.15 {
                let bucket = (need_px as u32).next_power_of_two().min(8192);
                let have_tex = self.zoom_tex.as_ref().is_some_and(|(b, _)| *b == bucket);
                let have_job = self.zoom_rx.as_ref().is_some_and(|(b, _)| *b == bucket);
                if !have_tex && !have_job {
                    if self.pending_zoom.as_ref().is_none_or(|(b, _)| *b != bucket) {
                        self.pending_zoom = Some((bucket, Instant::now()));
                    }
                    if let Some((_, since)) = self.pending_zoom {
                        let el = since.elapsed();
                        if el >= ZOOM_DEBOUNCE {
                            self.pending_zoom = None;
                            self.start_zoom_render(ui.ctx(), &path, source(state), dark, ppp, bucket, index);
                        } else {
                            ui.ctx().request_repaint_after(ZOOM_DEBOUNCE - el);
                        }
                    }
                } else {
                    self.pending_zoom = None;
                }
            } else {
                self.pending_zoom = None;
            }

            if let Some(draw_tex) = self.zoom_tex.as_ref().map(|(_, t)| t).or(self.base_tex.as_ref()) {
                let clipped = ui.painter().with_clip_rect(area);
                clipped.image(draw_tex.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
        } else {
            let pulse = ui.style().animation_time > 0.0 && state.config.animations;
            let fill = crate::skeleton::fill(ui, pulse);
            crate::skeleton::image_box(ui, area.shrink(12.0), fill, ui.visuals().widgets.noninteractive.bg_stroke, crate::theme::tokens::RADIUS);
        }

        if retry_clicked {
            let idx = state.indexer.index();
            self.start_render_with_index(ui.ctx(), &path, source(state), version, dark, ppp, idx);
        }

        state.commands.extend(cmds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_channel_sets_error_and_clears_rx() {
        let ctx = egui::Context::default();
        let (tx, rx) = unbounded();
        let mut st = ExcalidrawPreview { path: Some(PathBuf::from("a.excalidraw")), rx: Some(rx), ..Default::default() };
        assert_eq!(st.in_flight(), 1);
        drop(tx);
        st.poll(&ctx);
        assert!(st.rx.is_none());
        assert_eq!(st.in_flight(), 0);
        assert!(st.error.as_deref().is_some_and(|e| e.contains("failed unexpectedly")));
    }

    #[test]
    fn empty_and_ready_results_clear_rx() {
        let ctx = egui::Context::default();
        let (tx, rx) = unbounded();
        let mut st = ExcalidrawPreview { path: Some(PathBuf::from("a.excalidraw")), rx: Some(rx), ..Default::default() };
        tx.send(Err(RenderError::Empty)).unwrap();
        st.poll(&ctx);
        assert!(st.empty && st.error.is_none() && st.rx.is_none() && st.in_flight() == 0);

        let (tx, rx) = unbounded();
        st.rx = Some(rx);
        tx.send(Ok(Rasterized { dims: [100, 80], image: ColorImage::example() })).unwrap();
        st.poll(&ctx);
        assert!(!st.empty && st.base_tex.is_some() && st.rx.is_none() && st.in_flight() == 0);
    }

    #[test]
    fn rasterise_job_renders_non_empty_drawing_and_rejects_empty() {
        let job = RasterJob {
            path: PathBuf::from("test.excalidraw"),
            source: Some(
                r##"{"type":"excalidraw","elements":[{"id":"r1","type":"rectangle","x":10,"y":10,"width":100,"height":60,"strokeColor":"#1e1e1e"}]}"##.into(),
            ),
            dark: false,
            w_pts: 0.0,
            ppp: 1.0,
            px_w: None,
            index: Arc::new(RwLock::new(Index::new())),
        };
        let out = rasterise_job(&job).unwrap_or_else(|_| panic!("expected valid raster"));
        assert!(out.dims[0] >= 100 && out.dims[1] >= 60);
        assert!(out.image.size[0] >= 100);

        let empty_job = RasterJob { source: Some(r#"{"type":"excalidraw","elements":[]}"#.into()), ..job };
        assert!(matches!(rasterise_job(&empty_job), Err(RenderError::Empty)));
    }

    #[test]
    fn huge_extent_drawing_stays_within_the_side_caps() {
        let far = r##"{"type":"excalidraw","elements":[
            {"id":"a","type":"rectangle","x":-1000000,"y":-1000000,"width":10,"height":10,"strokeColor":"#1e1e1e"},
            {"id":"b","type":"rectangle","x":1000000,"y":1000000,"width":10,"height":10,"strokeColor":"#1e1e1e"}]}"##;
        let job = RasterJob {
            path: PathBuf::from("far.excalidraw"),
            source: Some(far.into()),
            dark: false,
            w_pts: 0.0,
            ppp: 1.0,
            px_w: None,
            index: Arc::new(RwLock::new(Index::new())),
        };
        let out = rasterise_job(&job).unwrap_or_else(|_| panic!("expected a raster"));
        assert!(out.image.size[0] as f32 <= MAX_SIDE && out.image.size[1] as f32 <= MAX_SIDE, "{:?}", out.image.size);
        let zoom = rasterise_job(&RasterJob { px_w: Some(8192.0), ..job }).unwrap_or_else(|_| panic!("expected a raster"));
        assert!(zoom.image.size[0] * zoom.image.size[1] <= ZOOM_PIXELS as usize + 2 * ZOOM_SIDE as usize);
    }
}
