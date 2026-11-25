//! Typst preview: feeds the engine, shows rendered pages (virtualized), diagnostics, click-to-source, PDF export.
use crate::scrollsync::ScrollMap;
use crate::scrollsync_egui::scroll_pane;
use crate::state::AppState;
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Color32, Rect, Sense, TextureHandle, TextureOptions, pos2, vec2};
use iron_sheets_typst::{DocSnapshot, Engine, PageState};
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MARGIN: f32 = 12.0;
const GAP: f32 = 10.0;
const DEBOUNCE: Duration = Duration::from_millis(60);
/// egui points per Typst pt at "100 %" (96 dpi).
const ONE_HUNDRED: f32 = 96.0 / 72.0;
const MAX_UPLOADS_PER_FRAME: usize = 2;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Zoom {
    Fit,
    /// egui points per pt.
    Scale(f32),
}

struct PageTex {
    handle: TextureHandle,
    version: u64,
    ppp_bits: u32,
}

/// Per-tab preview state.
struct TabView {
    path: PathBuf,
    /// Document version last handed to the engine.
    fed_version: Option<u64>,
    zoom: Zoom,
    last_scale: f32,
    /// One current texture per page; the old one is drawn until the new one is ready.
    tex: FxHashMap<u32, PageTex>,
    diag_open: bool,
    jump_waiting: Option<Instant>,
    /// (snapshot version, scale bits, total height bits) the cached scroll map was built for.
    map_key: Option<(u64, u32, u32)>,
}

impl TabView {
    fn new(path: PathBuf) -> TabView {
        TabView {
            path,
            fed_version: None,
            zoom: Zoom::Fit,
            last_scale: ONE_HUNDRED,
            tex: FxHashMap::default(),
            diag_open: true,
            jump_waiting: None,
            map_key: None,
        }
    }
}

struct Export {
    // shortcut: the export dialog code stays compiled without `pdf` (it needs no extra crate), only its entry points go.
    #[cfg_attr(not(feature = "pdf"), allow(dead_code))]
    tx: Sender<(u64, Option<PathBuf>)>,
    rx: Receiver<(u64, Option<PathBuf>)>,
    dialog_open: bool,
    /// Destination picked, waiting for the first compile.
    waiting: Vec<(u64, PathBuf)>,
    /// Handed to the engine, waiting for its result.
    sent: Vec<u64>,
}

impl Default for Export {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Export { tx, rx, dialog_open: false, waiting: Vec::new(), sent: Vec::new() }
    }
}

#[derive(Default)]
pub struct TypstPreview {
    views: FxHashMap<u64, TabView>,
    export: Export,
}

/// Byte offset of 0-based `line`, `column` (in chars) in `text`; clamped to the line.
pub fn line_col_to_byte(text: &str, line: u32, column: u32) -> usize {
    let mut start = 0usize;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let rest = &text[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let line_text = &rest[..end];
    start + line_text.char_indices().nth(column as usize).map_or(line_text.len(), |(i, _)| i)
}

fn root_for(workspace: &iron_sheets_core::workspace::Workspace, path: &Path) -> Option<PathBuf> {
    workspace.root_of(path).and_then(|i| workspace.roots.get(i).cloned()).or_else(|| path.parent().map(Path::to_path_buf))
}

fn feed(engine: &Engine, view: &mut TabView, tab_id: u64, doc: &crate::docs::Doc, root: Option<PathBuf>) {
    engine.set_document(tab_id, Some(doc.path.clone()), root, doc.text.clone(), doc.version);
    view.fed_version = Some(doc.version);
}

impl TypstPreview {
    /// Number of page textures currently held (all tabs); for tests and diagnostics.
    pub fn texture_count(&self) -> usize {
        self.views.values().map(|v| v.tex.len()).sum()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
        let ctx = ui.ctx().clone();
        let state_animations = state.config.animations;
        let AppState { tabs, docs, typst, workspace, status, .. } = state;
        let engine: &Arc<Engine> = typst;
        let Some(tab) = tabs.tabs.get_mut(tab_index) else { return };
        let Some(doc) = docs.get(&tab.path) else {
            let pulse = ui.style().animation_time > 0.0 && state_animations;
            let fill = crate::skeleton::fill(ui, pulse);
            let area = ui.available_rect_before_wrap();
            crate::skeleton::page(ui, area, MARGIN, fill);
            return;
        };
        let tab_id = tab.id;
        // A tab that now shows another file starts from scratch (and the engine forgets the old document).
        if self.views.get(&tab_id).is_some_and(|v| v.path != tab.path) {
            self.views.remove(&tab_id);
            engine.close_document(tab_id);
        }
        let view = self.views.entry(tab_id).or_insert_with(|| TabView::new(tab.path.clone()));

        // 1. Feed the engine, debounced while typing.
        if view.fed_version != Some(doc.version) {
            let since = doc.last_edit.elapsed();
            if view.fed_version.is_none() || since >= DEBOUNCE {
                feed(engine, view, tab_id, doc, root_for(workspace, &tab.path));
            } else {
                ctx.request_repaint_after(DEBOUNCE - since);
            }
        }
        let fed = view.fed_version;

        let mut new_cursor: Option<Range<usize>> = None;

        // 5. Result of a click-to-source request.
        if let Some(t0) = view.jump_waiting {
            if let Some((line, col)) = engine.take_jump(tab_id) {
                let off = line_col_to_byte(&doc.text, line, col);
                new_cursor = Some(off..off);
                view.jump_waiting = None;
            } else if t0.elapsed() > Duration::from_secs(3) {
                view.jump_waiting = None;
                status.info("No source location at that position");
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }

        let snap = engine.document(tab_id);

        // Toolbar.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.selectable_label(view.zoom == Zoom::Fit, "Fit width").clicked() {
                view.zoom = Zoom::Fit;
            }
            if ui.selectable_label(view.zoom == Zoom::Scale(ONE_HUNDRED), "100%").clicked() {
                view.zoom = Zoom::Scale(ONE_HUNDRED);
            }
            if crate::icons::button(ui, crate::icons::Icon::ZoomOut, "Zoom out").clicked() {
                view.zoom = Zoom::Scale((view.last_scale / 1.25).clamp(0.1, 8.0));
            }
            if crate::icons::button(ui, crate::icons::Icon::ZoomIn, "Zoom in").clicked() {
                view.zoom = Zoom::Scale((view.last_scale * 1.25).clamp(0.1, 8.0));
            }
            ui.weak(format!("{:.0}%", view.last_scale / ONE_HUNDRED * 100.0));
            if let Some(s) = &snap {
                let errors = s.diagnostics.iter().filter(|d| d.error).count();
                ui.weak(format!("{} page{}", s.pages.len(), if s.pages.len() == 1 { "" } else { "s" }));
                if !s.ok {
                    ui.colored_label(ui.visuals().error_fg_color, format!("compile failed: {errors} error{}", if errors == 1 { "" } else { "s" }));
                } else {
                    ui.weak(format!("{:.0} ms", s.compile_ms));
                }
            }
            // Only while a fed version is not shown yet: steady during typing (the debounce holds the feed back),
            // on from the pause until the compile lands. Right-aligned so it never shifts the items on its left; the
            // debounce above and the engine's wake-up on a finished compile already repaint both transitions.
            if fed.is_some_and(|f| snap.as_ref().is_none_or(|s| s.version != f)) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("compiling...");
                });
            }
        });

        // The page view keeps its own Ctrl+wheel zoom; the font zoom in app.rs yields while the pointer is over it.
        // Read the frame number first: calling the context inside `data_mut` would lock it twice.
        let frame = ui.ctx().cumulative_frame_nr();
        let rect = ui.max_rect();
        ui.ctx().data_mut(|d| d.insert_temp(crate::app::wheel_owner_id(), (rect, frame)));
        // Ctrl+wheel zoom.
        if ui.rect_contains_pointer(ui.max_rect()) {
            let z = ui.input(|i| i.zoom_delta());
            if (z - 1.0).abs() > 1e-3 {
                view.zoom = Zoom::Scale((view.last_scale * z).clamp(0.1, 8.0));
            }
        }

        // 4. Diagnostics strip.
        if let Some(s) = &snap {
            if !s.diagnostics.is_empty() {
                diagnostics_strip(ui, view, s, tab_id, &doc.text, &mut new_cursor);
            }
        }

        // 2. + 3. Pages and scroll map.
        let Some(snap) = snap else {
            let pulse = ui.style().animation_time > 0.0 && state_animations;
            let fill = crate::skeleton::fill(ui, pulse);
            let area = ui.available_rect_before_wrap();
            crate::skeleton::page(ui, area, MARGIN, fill);
            if let Some(c) = new_cursor {
                tab.pending_cursor = Some(c);
            }
            return;
        };
        let mut click: Option<(u32, f32, f32)> = None;
        let mut map: Option<ScrollMap> = None;
        scroll_pane(ui, (tab_id, "typ_preview"), &mut tab.preview_scroll, |ui| {
            map = draw_pages(ui, &ctx, engine, view, &snap, tab_id, &doc.text, &mut click);
        });
        if let Some(m) = map {
            tab.preview_scroll.map = m;
        }
        if let Some((page, x, y)) = click {
            engine.jump_from_click(tab_id, page, x, y);
            view.jump_waiting = Some(Instant::now());
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if let Some(c) = new_cursor {
            tab.pending_cursor = Some(c);
        }
    }
}

fn diagnostics_strip(ui: &mut egui::Ui, view: &mut TabView, s: &DocSnapshot, tab_id: u64, text: &str, new_cursor: &mut Option<Range<usize>>) {
    let errors = s.diagnostics.iter().filter(|d| d.error).count();
    let warnings = s.diagnostics.len() - errors;
    egui::Panel::bottom(egui::Id::NULL.with(("typ_diag", tab_id))).resizable(false).show_separator_line(true).show(ui, |ui| {
        ui.horizontal(|ui| {
            let arrow = if view.diag_open { "-" } else { "+" };
            let mut title = format!("{arrow} {errors} error{}, {warnings} warning{}", if errors == 1 { "" } else { "s" }, if warnings == 1 { "" } else { "s" });
            if !s.ok {
                title.push_str(" (showing last good pages)");
            }
            if ui.add(egui::Label::new(egui::RichText::new(title).strong()).sense(Sense::click())).clicked() {
                view.diag_open = !view.diag_open;
            }
        });
        if view.diag_open {
            egui::ScrollArea::vertical().id_salt(("typ_diag_scroll", tab_id)).max_height(130.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, d) in s.diagnostics.iter().enumerate() {
                    let color = if d.error { ui.visuals().error_fg_color } else { ui.visuals().warn_fg_color };
                    let place = match &d.file {
                        None => format!("{}:{}", d.line + 1, d.column + 1),
                        Some(f) => format!("{}:{}:{}", f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), d.line + 1, d.column + 1),
                    };
                    let mut job = egui::text::LayoutJob::default();
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    job.append(if d.error { "error " } else { "warning " }, 0.0, egui::TextFormat::simple(font.clone(), color));
                    job.append(&format!("{place}  "), 0.0, egui::TextFormat::simple(font.clone(), ui.visuals().weak_text_color()));
                    job.append(&d.message, 0.0, egui::TextFormat::simple(font.clone(), ui.visuals().text_color()));
                    let resp = ui.push_id(i, |ui| ui.add(egui::Label::new(job).sense(Sense::click()).wrap())).inner;
                    for h in &d.hints {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.weak(format!("hint: {h}"));
                        });
                    }
                    if resp.clicked() && d.file.is_none() {
                        let off = line_col_to_byte(text, d.line, d.column);
                        *new_cursor = Some(off..off);
                    }
                    crate::anim::context_menu(&resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                        if ui.button("Copy message").clicked() {
                            let mut m = d.message.clone();
                            for h in &d.hints {
                                m.push_str("\nhint: ");
                                m.push_str(h);
                            }
                            ui.ctx().copy_text(m);
                            ui.close();
                        }
                    });
                }
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_pages(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    engine: &Arc<Engine>,
    view: &mut TabView,
    snap: &DocSnapshot,
    tab_id: u64,
    text: &str,
    click: &mut Option<(u32, f32, f32)>,
) -> Option<ScrollMap> {
    let avail_w = ui.available_width();
    let max_w = snap.pages.iter().map(|p| p[0]).fold(1.0f32, f32::max);
    let max_h = snap.pages.iter().map(|p| p[1]).fold(1.0f32, f32::max);
    let scale = match view.zoom {
        Zoom::Fit => ((avail_w - 2.0 * MARGIN) / max_w).clamp(0.05, 8.0),
        Zoom::Scale(s) => s,
    };
    view.last_scale = scale;
    let mut tops = Vec::with_capacity(snap.pages.len());
    let mut y = MARGIN;
    for p in &snap.pages {
        tops.push(y);
        y += p[1] * scale + GAP;
    }
    let total_h = (y - GAP + MARGIN).max(MARGIN * 2.0);
    let content_w = (max_w * scale + 2.0 * MARGIN).max(avail_w);

    // Texture resolution: quantised so resizing does not re-render constantly, capped by the GPU limit.
    let side = ctx.input(|i| i.max_texture_side) as f32;
    let ppp = ((scale * ctx.pixels_per_point()).min(side / max_w.max(max_h)) * 8.0).round().max(2.0) / 8.0;

    let mut paint = |ui: &mut egui::Ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(content_w, total_h), Sense::hover());
        let visible = ui.clip_rect();
        let dark = ui.visuals().dark_mode;
        ui.painter().rect_filled(visible.intersect(rect), 0.0, Color32::from_gray(if dark { 36 } else { 205 }));
        let margin = max_h * scale;
        let wanted = visible.expand2(vec2(0.0, margin));
        let mut first_vis = None;
        let mut uploads = 0;
        let mut pending = false;
        let mut keep: Vec<u32> = Vec::new();
        for (i, size) in snap.pages.iter().enumerate() {
            let (w, h) = (size[0] * scale, size[1] * scale);
            let page_rect = Rect::from_min_size(pos2(rect.min.x + (content_w - w) * 0.5, rect.min.y + tops[i]), vec2(w, h));
            if !page_rect.intersects(wanted) {
                continue;
            }
            let p = i as u32;
            keep.push(p);
            // Request and upload.
            match engine.page(tab_id, p, ppp) {
                PageState::Ready(img) => {
                    let cur = view.tex.get(&p).map(|t| (t.version, t.ppp_bits));
                    let new = (img.version, img.pixels_per_pt.to_bits());
                    if cur != Some(new) {
                        if uploads < MAX_UPLOADS_PER_FRAME {
                            uploads += 1;
                            let ci = egui::ColorImage::from_rgba_premultiplied([img.width as usize, img.height as usize], &img.rgba);
                            let handle = ctx.load_texture(format!("typ-{tab_id}-{p}"), ci, TextureOptions::LINEAR);
                            view.tex.insert(p, PageTex { handle, version: img.version, ppp_bits: img.pixels_per_pt.to_bits() });
                        } else {
                            pending = true;
                        }
                    }
                    // Rendering of a newer version / scale still in flight.
                    if snap.ok && img.version < snap.version {
                        pending = true;
                    }
                }
                PageState::Pending => pending = true,
            }
            if page_rect.intersects(visible) && first_vis.is_none() {
                first_vis = Some(i);
            }
            // Draw: shadow, white page, texture.
            let painter = ui.painter();
            painter.rect_filled(page_rect.translate(vec2(1.5, 2.0)), 1.0, Color32::from_black_alpha(if dark { 90 } else { 40 }));
            painter.rect_filled(page_rect, 0.0, Color32::WHITE);
            if let Some(t) = view.tex.get(&p) {
                painter.image(t.handle.id(), page_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            let resp = ui.interact(page_rect.intersect(visible), ui.scope_id().with(("typ_page", tab_id, p)), Sense::click());
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    *click = Some((p, (pos.x - page_rect.min.x) / scale, (pos.y - page_rect.min.y) / scale));
                }
            }
        }
        // Free textures of pages far outside the viewport.
        view.tex.retain(|p, _| keep.contains(p));
        if pending {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
        let _ = first_vis;
    };

    if content_w > avail_w + 0.5 {
        egui::ScrollArea::horizontal().id_salt(("typ_hz", tab_id)).auto_shrink([false, true]).show(ui, |ui| paint(ui));
    } else {
        paint(ui);
    }

    // Scroll map: source line -> y in this pane (content coordinates).
    let key = (snap.version, scale.to_bits(), total_h.to_bits());
    if view.map_key == Some(key) {
        return None;
    }
    view.map_key = Some(key);
    let n_lines = text.bytes().filter(|&b| b == b'\n').count() + 1;
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(snap.anchors.len() + 2);
    for a in &snap.anchors {
        let Some(top) = tops.get(a.page as usize) else { continue };
        if pts.last().is_some_and(|l| l.0 >= a.line as f32) {
            continue;
        }
        pts.push((a.line as f32, top + a.y_pt * scale));
    }
    if pts.first().is_none_or(|p| p.0 > 0.0) {
        pts.insert(0, (0.0, 0.0));
    }
    let last_line = pts.last().map_or(0.0, |p| p.0);
    pts.push((((n_lines as f32).max(last_line + 1.0)), total_h));
    Some(ScrollMap::from_points(pts))
}

/// Per-frame housekeeping: forget documents of tabs that are gone, drive PDF export.
pub fn poll(state: &mut AppState, ctx: &egui::Context) {
    let mut me = std::mem::take(&mut state.ui.typst_preview);
    let engine = state.typst.clone();
    me.views.retain(|id, v| {
        let keep = state.tabs.tabs.iter().any(|t| t.id == *id && t.path == v.path);
        if !keep {
            engine.close_document(*id);
        }
        keep
    });
    while let Ok((id, path)) = me.export.rx.try_recv() {
        me.export.dialog_open = false;
        if let Some(p) = path {
            me.export.waiting.push((id, p));
        }
    }
    let mut waiting = std::mem::take(&mut me.export.waiting);
    waiting.retain(|(id, path)| {
        if engine.document(*id).is_some() {
            engine.export_pdf(*id, path.clone());
            me.export.sent.push(*id);
            false
        } else {
            true
        }
    });
    me.export.waiting = waiting;
    let mut sent = std::mem::take(&mut me.export.sent);
    sent.retain(|id| match engine.take_export_result(*id) {
        Some(Ok(p)) => {
            state.status.info(format!("Exported PDF: {}", p.display()));
            false
        }
        Some(Err(e)) => {
            state.status.error(format!("PDF export failed: {e}"));
            false
        }
        None => true,
    });
    me.export.sent = sent;
    if !me.export.waiting.is_empty() || !me.export.sent.is_empty() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    state.ui.typst_preview = me;
}

/// `Command::ExportPdf`: native save dialog on a thread, then the engine writes the PDF.
#[cfg_attr(not(feature = "pdf"), allow(dead_code))]
pub fn export_pdf(state: &mut AppState, waker: &std::sync::Arc<dyn crate::platform::Waker>) {
    let Some(i) = state.tabs.active else { return };
    let (tab_id, path) = (state.tabs.tabs[i].id, state.tabs.tabs[i].path.clone());
    if !matches!(iron_sheets_core::FileKind::from_path(&path), iron_sheets_core::FileKind::Typst) {
        state.status.error("Export PDF works on Typst notes (.typ)");
        return;
    }
    let Some(doc) = state.docs.get(&path) else { return };
    let mut me = std::mem::take(&mut state.ui.typst_preview);
    if me.export.dialog_open {
        state.ui.typst_preview = me;
        return;
    }
    // Make sure the engine has the current text even when the preview pane is hidden.
    let view = me.views.entry(tab_id).or_insert_with(|| TabView::new(path.clone()));
    if view.fed_version != Some(doc.version) {
        feed(&state.typst, view, tab_id, doc, root_for(&state.workspace, &path));
    }
    me.export.dialog_open = true;
    let (tx, ctx2) = (me.export.tx.clone(), waker.clone());
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let dir = path.parent().map(Path::to_path_buf);
    let fallback = path.with_extension("pdf");
    let spawned = std::thread::Builder::new().name("iron-sheets-pdf-pick".into()).spawn(move || {
        let started = Instant::now();
        let mut d = rfd::FileDialog::new().set_title("Export PDF").set_file_name(format!("{stem}.pdf")).add_filter("PDF", &["pdf"]);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        let picked = match d.save_file() {
            Some(p) => Some(p),
            // No dialog could be shown (no portal): write next to the note.
            None if started.elapsed().as_millis() < 150 => Some(fallback),
            None => None,
        };
        let _ = tx.send((tab_id, picked));
        ctx2.wake();
    });
    if spawned.is_err() {
        me.export.dialog_open = false;
        state.status.error("Cannot open the save dialog");
    }
    state.ui.typst_preview = me;
}

#[cfg(test)]
mod tests {
    use super::line_col_to_byte;

    #[test]
    fn line_col() {
        let t = "ab\nc\u{e9}d\n\nlast";
        assert_eq!(line_col_to_byte(t, 0, 0), 0);
        assert_eq!(line_col_to_byte(t, 0, 2), 2);
        assert_eq!(line_col_to_byte(t, 0, 99), 2);
        assert_eq!(line_col_to_byte(t, 1, 2), 3 + 1 + 2);
        assert_eq!(line_col_to_byte(t, 2, 0), 8);
        assert_eq!(line_col_to_byte(t, 3, 2), 11);
        assert_eq!(line_col_to_byte(t, 9, 0), t.len());
    }
}
