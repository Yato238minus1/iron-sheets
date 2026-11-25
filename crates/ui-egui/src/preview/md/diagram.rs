//! Mermaid diagram text and viewers. The picture is a raster; its text is laid out as invisible galleys over it
//! (`label_box`), so the preview's own selection, hit testing, links and copy work on it. The fullscreen viewer and
//! the separate window show one diagram with zoom, pan, a sharper raster at high zoom and the same text overlay
//! (`DiagramView`).
use super::images::ImageCache;
use super::layout::{LinkAction, LinkSpan, TextBox, layout_job};
use super::mermaid::{DiagramText, Label, Link};
use super::select::{Pos, Sel, seg_range, sel_rects, slice_text, word_range};
use crate::aux_window::AuxSpec;
use crate::command::Command;
use crate::viewer::{self, View};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, Context, FontId, Id, PointerButton, Pos2, Rect, Response, Ui, Vec2, pos2, vec2};
use iron_sheets_core::config::AuxWindowGeom;
use iron_sheets_md::model::LinkTarget;
use std::path::PathBuf;

/// An invisible text box for `l` at `scale` (SVG units to points): its position relative to the picture's top-left
/// and the galley, scaled so its width matches the drawn text.
pub fn label_box(ctx: &Context, l: &Label, scale: f32) -> (Pos2, TextBox) {
    let family = crate::theme::families().preview;
    let layout = |size: f32| {
        let mut job = LayoutJob::single_section(
            l.text.clone(),
            TextFormat { font_id: FontId::new(size.max(1.0), family.clone()), color: Color32::TRANSPARENT, ..Default::default() },
        );
        job.wrap.max_width = f32::INFINITY;
        layout_job(ctx, job)
    };
    let target = l.rect.width() * scale;
    let mut g = layout(l.size * scale);
    let w = g.size().x;
    if w > 1.0 && target > 1.0 && ((target / w) - 1.0).abs() > 0.03 {
        g = layout(l.size * scale * (target / w).clamp(0.5, 2.0));
    }
    let pos = l.rect.center() * scale - g.size() / 2.0;
    let links = l.link.iter().map(|k| {
        let (action, tip) = match k {
            Link::Url(u) => (LinkAction::Target(LinkTarget::Url(u.clone())), u.clone()),
            Link::Note(n) => (LinkAction::Target(LinkTarget::Wiki { note: n.clone(), heading: None, block: None }), n.clone()),
        };
        LinkSpan { chars: 0..l.text.chars().count(), action, tip }
    });
    (pos, TextBox { galley: g, links: links.collect(), slots: vec![], kbds: vec![] })
}

/// Text boxes of a whole diagram at `scale`.
pub fn label_boxes(ctx: &Context, t: &DiagramText, scale: f32) -> Vec<(Pos2, TextBox)> {
    t.labels.iter().map(|l| label_box(ctx, l, scale)).collect()
}

/// Boxes of the linked nodes at `scale`, each with the index of its label: the whole node shape opens the link.
pub fn label_shapes(t: &DiagramText, scale: f32) -> Vec<(Rect, usize)> {
    let r = |r: Rect| Rect::from_min_max(r.min * scale, r.max * scale);
    t.labels.iter().enumerate().filter_map(|(k, l)| Some((r(l.shape?), k))).collect()
}

/// A diagram open in the fullscreen viewer or the separate window.
pub struct DiagramView {
    /// The block's slot in the image cache; the viewer follows it through edits.
    slot: u64,
    /// Cache key of the picture shown now (the slot's current one).
    path: PathBuf,
    /// The note it was opened from.
    pub note: PathBuf,
    pub view: View,
    sel: Option<Sel>,
    /// Text boxes at scale 1 (SVG units), built when the diagram's text is known.
    boxes: Vec<(Pos2, TextBox)>,
    /// Node boxes (SVG units) of the linked labels, with the label's index in `boxes`.
    shapes: Vec<(Rect, usize)>,
    /// Screen rects of the boxes of the last frame (a press there selects instead of panning).
    hot: Vec<Rect>,
    /// Whether the press that started the current drag was on text; fixed at the press, because the text moves under a
    /// still pointer while the view pans (asking `hot` with the press origin each frame would flip it mid-drag).
    press_on_text: Option<bool>,
    pub geom: AuxWindowGeom,
    focus: bool,
}

impl DiagramView {
    pub fn new(slot: u64, note: PathBuf) -> Self {
        DiagramView {
            slot,
            path: PathBuf::new(),
            note,
            view: View::default(),
            sel: None,
            boxes: Vec::new(),
            shapes: Vec::new(),
            hot: Vec::new(),
            press_on_text: None,
            geom: AuxWindowGeom::default(),
            focus: true,
        }
    }

    /// Screen rects of the text boxes in the last frame.
    pub fn text_rects(&self) -> &[Rect] {
        &self.hot
    }

    /// Follows the slot to its current picture; false when the diagram is gone.
    fn sync(&mut self, images: &ImageCache) -> bool {
        let Some(p) = images.slot_path(self.slot) else { return false };
        if p != self.path {
            (self.path, self.sel) = (p, None);
            self.boxes.clear();
            self.shapes.clear();
            self.hot.clear();
        }
        true
    }

    /// The selected text (labels in order, joined with newlines).
    pub fn selected(&self) -> String {
        let Some((lo, hi)) = self.sel.map(|s| s.ordered()) else { return String::new() };
        let parts: Vec<String> =
            self.boxes.iter().enumerate().filter_map(|(k, (_, tb))| Some(slice_text(tb.galley.text(), &[], &seg_range(lo, hi, 0, k)?))).collect();
        parts.join("\n")
    }

    /// Nearest box to `p` (SVG units) and the char under it.
    fn hit(&self, p: Pos2) -> Option<Pos> {
        let dist = |(pos, tb): &(Pos2, TextBox)| {
            let r = Rect::from_min_size(*pos, tb.galley.size());
            let (dx, dy) = ((r.min.x - p.x).max(p.x - r.max.x).max(0.0), (r.min.y - p.y).max(p.y - r.max.y).max(0.0));
            dx * dx + dy * dy
        };
        let (k, (pos, tb)) = self.boxes.iter().enumerate().min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)))?;
        Some(Pos { blk: 0, seg: k, ch: tb.galley.cursor_from_pos(p - *pos).index.0 })
    }

    /// Picture, selection and links in `r`; `bg` covers the viewer (drag, wheel and double-click are
    /// done by the caller through `viewer::pan_zoom`).
    #[allow(clippy::too_many_arguments)]
    fn paint(&mut self, ui: &mut Ui, r: Rect, bg: &Response, size: Vec2, zoom_slot: usize, images: &mut ImageCache, cmds: &mut Vec<Command>) {
        let ppp = ui.ctx().pixels_per_point();
        let base = images.texture(&self.path);
        match images.zoom_texture(&self.path, r.width() * ppp, zoom_slot).or(base) {
            Some(t) => {
                ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "Loading...", FontId::proportional(16.0), Color32::WHITE);
            }
        }
        if self.boxes.is_empty() {
            if let Some(t) = images.texts(&self.path) {
                self.boxes = label_boxes(ui.ctx(), &t, 1.0);
                self.shapes = label_shapes(&t, 1.0);
            }
        }
        let s = r.width() / size.x.max(1.0);
        let to_screen = |p: Pos2| r.min + p.to_vec2() * s;
        let to_svg = |p: Pos2| ((p - r.min) / s).to_pos2();
        let over = |p: Pos2| self.hot.iter().position(|h| h.contains(p));
        // The linked label under `p`, by its text or else by its node's shape.
        let link_at = |p: Pos2| {
            over(p).filter(|&k| !self.boxes[k].1.links.is_empty()).or_else(|| self.shapes.iter().find(|(s, _)| s.contains(to_svg(p))).map(|(_, k)| *k))
        };
        // Selection.
        let (shift, origin) = ui.input(|i| (i.modifiers.shift, i.pointer.press_origin()));
        if bg.drag_started() {
            self.press_on_text = Some(origin.is_some_and(|o| over(o).is_some()));
        } else if !bg.dragged() {
            self.press_on_text = None;
        }
        let on_text = self.press_on_text == Some(true);
        let panning = self.press_on_text == Some(false);
        if bg.drag_started_by(PointerButton::Primary) {
            if let Some(h) = origin.filter(|_| on_text).and_then(|o| self.hit(to_svg(o))) {
                match &mut self.sel {
                    Some(sl) if shift => sl.cursor = h,
                    _ => self.sel = Some(Sel { anchor: h, cursor: h }),
                }
            }
        } else if bg.dragged_by(PointerButton::Primary) && self.sel.is_some() && on_text {
            if let Some(h) = bg.interact_pointer_pos().and_then(|p| self.hit(to_svg(p))) {
                if let Some(sl) = &mut self.sel {
                    sl.cursor = h;
                }
            }
        } else if bg.triple_clicked() || bg.double_clicked() {
            if let Some(h) = bg.interact_pointer_pos().filter(|p| over(*p).is_some()).and_then(|p| self.hit(to_svg(p))) {
                let text = self.boxes[h.seg].1.galley.text();
                let rg = if bg.triple_clicked() { 0..text.chars().count() } else { word_range(text, h.ch) };
                self.sel = Some(Sel { anchor: Pos { ch: rg.start, ..h }, cursor: Pos { ch: rg.end, ..h } });
            }
        } else if bg.clicked() {
            let link = bg.interact_pointer_pos().and_then(link_at).and_then(|k| self.boxes[k].1.links.first());
            match link.map(|l| &l.action) {
                Some(LinkAction::Target(t)) => {
                    let new_tab = ui.input(|i| i.modifiers.command);
                    cmds.extend(link_command(t, &self.note, new_tab));
                }
                _ => self.sel = None,
            }
        }
        if let Some((lo, hi)) = self.sel.map(|s| s.ordered()) {
            // Translucent: the text under the highlight is part of the picture.
            let col = ui.visuals().selection.bg_fill.gamma_multiply(0.4);
            for (k, (pos, tb)) in self.boxes.iter().enumerate() {
                if let Some(rg) = seg_range(lo, hi, 0, k) {
                    for rc in sel_rects(&tb.galley, &rg) {
                        let rc = rc.translate(pos.to_vec2());
                        ui.painter().rect_filled(Rect::from_min_max(to_screen(rc.min), to_screen(rc.max)), 0.0, col);
                    }
                }
            }
        }
        if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) && self.sel.is_some_and(|s| s.anchor != s.cursor) {
            ui.ctx().copy_text(self.selected());
        }
        let hot = self.boxes.iter().map(|(p, tb)| Rect::from_min_max(to_screen(*p), to_screen(*p + tb.galley.size())).expand(2.0)).collect();
        if let Some(hp) = ui.input(|i| i.pointer.hover_pos()).filter(|p| r.contains(*p) && bg.hovered() && !panning) {
            if let Some(k) = link_at(hp).or_else(|| over(hp)) {
                let linked = !self.boxes[k].1.links.is_empty();
                ui.ctx().set_cursor_icon(if linked { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Text });
                if let Some(l) = self.boxes[k].1.links.first().filter(|_| linked) {
                    let tip = l.tip.clone();
                    egui::Tooltip::for_widget(bg).at_pointer().show(|ui| {
                        ui.label(tip);
                    });
                }
            }
        }
        self.hot = hot;
    }

    /// Whether a press belongs to the background (pans): decided by the latched press while a drag runs, else by the text rects.
    fn pan_pred(&self) -> impl Fn(Pos2) -> bool + use<> {
        let (hot, latched) = (self.hot.clone(), self.press_on_text);
        move |p| latched.map_or_else(|| !hot.iter().any(|h| h.contains(p)), |t| !t)
    }

    /// Natural size of the diagram in points (placeholder while it loads).
    fn size(&self, images: &ImageCache) -> Vec2 {
        images.dims(&self.path).map_or(vec2(120.0, 40.0), |[w, h]| vec2(w as f32, h as f32))
    }

    /// Fullscreen: a dark overlay over the app window. Returns true when closed.
    pub fn show_fullscreen(&mut self, ctx: &Context, images: &mut ImageCache, cmds: &mut Vec<Command>) -> bool {
        if !self.sync(images) {
            return true;
        }
        let size = self.size(images);
        let pan = self.pan_pred();
        let mut view = self.view;
        let id = Id::NULL.with("md-diagram-viewer");
        let close = viewer::show_with(ctx, id, &mut view, size, 3.0, &pan, |ui, r, bg, _| {
            self.paint(ui, r, bg, size, 0, images, cmds);
        });
        self.view = view;
        close
    }

    /// A separate OS window (an embedded `egui::Window` where there are no viewports). Returns true when closed.
    pub fn show_window(&mut self, ctx: &Context, images: &mut ImageCache, cmds: &mut Vec<Command>) -> bool {
        if !self.sync(images) {
            return true;
        }
        let spec = AuxSpec { id: "mermaid-diagram", title: "Diagram", default_size: vec2(900.0, 650.0), min_size: vec2(320.0, 240.0) };
        let (size, pan) = (self.size(images), self.pan_pred());
        let (mut geom, mut view, focus) = (self.geom, self.view, std::mem::take(&mut self.focus));
        let close = viewer::window(ctx, &spec, &mut geom, focus, &mut view, size, 3.0, &pan, |ui, r, bg| {
            self.paint(ui, r, bg, size, 1, images, cmds);
        });
        (self.geom, self.view) = (geom, view);
        close
    }
}

/// Same targets as links in the preview text; `#heading` opens the host note at the heading.
fn link_command(t: &LinkTarget, note: &std::path::Path, new_tab: bool) -> Option<Command> {
    match t {
        LinkTarget::Wiki { note: n, heading, .. } => {
            Some(Command::OpenLink { note: n.clone(), heading: heading.clone(), from: Some(note.to_path_buf()), new_tab })
        }
        LinkTarget::Url(u) => match u.strip_prefix('#') {
            Some(h) => {
                let stem = note.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                Some(Command::OpenLink { note: stem, heading: Some(super::percent_decode(h)), from: Some(note.to_path_buf()), new_tab })
            }
            None => super::url_command(note, u, new_tab),
        },
        _ => None,
    }
}
