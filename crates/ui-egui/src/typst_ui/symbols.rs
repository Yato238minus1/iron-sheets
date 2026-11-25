//! Symbol panel: hand-drawn symbol lookup (background recognizer) and a virtualized, searchable symbol grid.
use crate::command::Command;
use crate::state::AppState;
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Event, FontId, RichText, Sense, Stroke as PaintStroke, pos2, vec2};
use iron_sheets_core::FileKind;
use iron_sheets_core::config::MathSyntax;
use iron_sheets_symrec::{Recognizer, Stroke, find_math_font, handwriting_aliases};
use iron_sheets_typst::{SymbolInfo, symbols};

const CELL: f32 = 34.0;
const CANDIDATES: usize = 24;

pub use iron_sheets_app_core::settings_model::InsertMode;

const INSERT_MODES: [(InsertMode, &str); 4] =
    [(InsertMode::Auto, "Auto"), (InsertMode::Name, "Name"), (InsertMode::SymName, "#sym.name"), (InsertMode::Character, "Char")];

/// Is `cursor` inside Typst math? Heuristic: count the unescaped `$` before the cursor; odd = inside.
/// It does not know about raw blocks, strings or comments containing `$`.
pub fn typst_in_math(text: &str, cursor: usize) -> bool {
    let b = text.as_bytes();
    let end = cursor.min(b.len());
    let (mut i, mut inside) = (0, false);
    while i < end {
        match b[i] {
            b'\\' => i += 1,
            b'$' => inside = !inside,
            _ => {}
        }
        i += 1;
    }
    inside
}

/// The text inserted for `sym` at `cursor` in a document of `kind`. With LaTeX math syntax a Markdown note never
/// gets Typst syntax: the LaTeX command when the converter maps it back to this very symbol, else the character.
/// Auto means LaTeX when the formula around the cursor looks like LaTeX, else Typst.
pub fn insertion_text(sym: &SymbolInfo, mode: InsertMode, kind: FileKind, text: &str, cursor: usize, syntax: MathSyntax) -> String {
    let latex = match syntax {
        MathSyntax::Auto => iron_sheets_md::math_at(text, cursor).is_some_and(iron_sheets_md::looks_like_latex),
        s => s == MathSyntax::Latex,
    };
    let chr = || sym.ch.map_or_else(|| sym.value.clone(), |c| c.to_string());
    let (name, sym_name) = if latex && kind != FileKind::Typst {
        // Lazy and per click: no table is built, the converter's own command table is asked.
        let cmd = || {
            let cmd = format!("\\{}", sym.name);
            let known = sym.name.bytes().all(|b| b.is_ascii_alphabetic()) && iron_sheets_typst::latex_to_typst(&cmd).is_ok_and(|t| t == sym.name);
            if known { cmd } else { chr() }
        };
        let c = cmd();
        (c.clone(), c)
    } else {
        (sym.name.clone(), format!("#sym.{}", sym.name))
    };
    let (name, sym_name) = (|| name.clone(), || sym_name.clone());
    match mode {
        InsertMode::Name => name(),
        InsertMode::SymName => sym_name(),
        InsertMode::Character => chr(),
        InsertMode::Auto => match kind {
            FileKind::Typst => {
                if typst_in_math(text, cursor) {
                    name()
                } else {
                    sym_name()
                }
            }
            _ => {
                if iron_sheets_md::edit::in_math(text, cursor) {
                    name()
                } else {
                    chr()
                }
            }
        },
    }
}

enum Out {
    Ready,
    Failed(String),
    Result(u64, Vec<u32>),
}

#[derive(Default, PartialEq)]
enum Rec {
    #[default]
    Idle,
    Building,
    Ready,
    Failed(String),
}

#[derive(Default)]
pub struct SymbolsPanel {
    rec: Rec,
    req_tx: Option<Sender<(u64, Vec<Stroke>)>>,
    out_rx: Option<Receiver<Out>>,
    seq: u64,
    strokes: Vec<Stroke>,
    current: Option<Stroke>,
    candidates: Vec<u32>,
    /// Highlighted candidate (Left/Right moves, Enter inserts) while the canvas has focus.
    cand_sel: usize,
    /// The recognizer has answered the current stroke set (`seq`).
    answered: bool,
    pub query: String,
    /// Canvas rectangle of the last frame (screen coordinates); for tests.
    pub canvas_rect: Option<egui::Rect>,
    canvas_id: Option<egui::Id>,
    filtered_query: Option<String>,
    filtered: Vec<u32>,
    /// Indices of all non-deprecated symbols, in table order.
    all: Vec<u32>,
    grid_sel: usize,
    grid_scroll_y: f32,
    grid_view_h: f32,
    grid_scroll_to: Option<f32>,
}

fn glyph_text(s: &SymbolInfo) -> String {
    s.ch.map_or_else(|| s.value.clone(), |c| c.to_string())
}

impl SymbolsPanel {
    /// True once the recognizer is built.
    pub fn recognizer_ready(&self) -> bool {
        self.rec == Rec::Ready
    }

    /// The recognizer has answered the current drawing.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Test hook: the recognizer answered the current drawing with no candidates.
    #[doc(hidden)]
    pub fn force_empty_answer(&mut self) {
        self.strokes = vec![vec![[10.0, 10.0], [60.0, 60.0]]];
        self.candidates.clear();
        self.answered = true;
    }

    /// Names of the current handwriting candidates, best first.
    pub fn candidate_names(&self) -> Vec<String> {
        self.candidates.iter().filter_map(|&i| symbols().get(i as usize)).map(|s| s.name.clone()).collect()
    }

    /// Names listed by the search/all grid for the current query.
    pub fn filtered_names(&self) -> Vec<String> {
        self.filtered.iter().map(|&i| symbols()[i as usize].name.clone()).collect()
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.rec = Rec::Building;
        let (req_tx, req_rx) = unbounded::<(u64, Vec<Stroke>)>();
        let (out_tx, out_rx) = unbounded::<Out>();
        self.req_tx = Some(req_tx);
        self.out_rx = Some(out_rx);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("iron-sheets-symrec".into()).spawn(move || {
            let fonts: Vec<&[u8]> = iron_sheets_assets_fonts();
            let Some(math) = find_math_font(&fonts) else {
                let _ = out_tx.send(Out::Failed("no math font available".into()));
                ctx.request_repaint();
                return;
            };
            let mut pairs: Vec<(u32, char)> = Vec::new();
            for (i, s) in symbols().iter().enumerate() {
                if s.deprecated {
                    continue;
                }
                if let Some(c) = s.ch {
                    pairs.push((i as u32, c));
                    pairs.extend(handwriting_aliases(c).map(|a| (i as u32, a)));
                }
            }
            let rec = Recognizer::build(&[math], pairs);
            let _ = out_tx.send(Out::Ready);
            ctx.request_repaint();
            // Latest request wins.
            while let Ok(mut req) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let ids = rec.recognize(&req.1, CANDIDATES).into_iter().map(|m| m.id).collect();
                if out_tx.send(Out::Result(req.0, ids)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        if spawned.is_err() {
            self.rec = Rec::Failed("cannot start the recognizer thread".into());
        }
    }

    fn request(&mut self) {
        if self.strokes.is_empty() {
            self.seq += 1;
            self.candidates.clear();
            self.cand_sel = 0;
            self.answered = false;
            return;
        }
        if self.rec == Rec::Ready {
            self.seq += 1;
            self.answered = false;
            if let Some(tx) = &self.req_tx {
                let _ = tx.send((self.seq, self.strokes.clone()));
            }
        }
    }

    fn poll(&mut self) {
        let mut ready = false;
        if let Some(rx) = &self.out_rx {
            while let Ok(o) = rx.try_recv() {
                match o {
                    Out::Ready => {
                        self.rec = Rec::Ready;
                        ready = true;
                    }
                    Out::Failed(e) => self.rec = Rec::Failed(e),
                    Out::Result(seq, ids) => {
                        if seq == self.seq && !self.strokes.is_empty() {
                            self.candidates = ids;
                            self.cand_sel = 0;
                            self.answered = true;
                        }
                    }
                }
            }
        }
        if ready {
            self.request();
        }
    }

    fn refilter(&mut self) {
        if self.filtered_query.as_deref() == Some(self.query.as_str()) {
            return;
        }
        self.filtered_query = Some(self.query.clone());
        self.grid_sel = 0;
        self.grid_scroll_to = Some(0.0);
        let table = symbols();
        let q = self.query.trim().to_lowercase();
        self.filtered.clear();
        if q.is_empty() {
            self.filtered.extend_from_slice(&self.all);
            return;
        }
        let mut rest: Vec<(u32, &str)> = Vec::new();
        for &i in &self.all {
            let name = table[i as usize].name.as_str();
            if name.to_lowercase().contains(&q) {
                self.filtered.push(i);
            } else {
                rest.push((i, name));
            }
        }
        let ranked = iron_sheets_core::fuzzy::fuzzy_rank(&q, rest, 200);
        self.filtered.extend(ranked.into_iter().map(|(i, _)| i));
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        if self.all.is_empty() {
            self.all = symbols().iter().enumerate().filter(|(_, s)| !s.deprecated).map(|(i, _)| i as u32).collect();
        }
        let take_focus = std::mem::take(&mut state.focus_sidebar);
        if self.rec == Rec::Idle {
            self.start(ui.ctx());
        }
        self.poll();
        let mut picked: Option<u32> = None;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for (m, label) in INSERT_MODES {
                if ui.selectable_label(state.symbol_mode == m, RichText::new(label).small()).clicked() {
                    state.symbol_mode = m;
                }
            }
        });
        let focused = self.canvas(ui);
        if focused && !self.strokes.is_empty() {
            if let Some(id) = self.canvas_id {
                ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() }));
            }
            let n = self.candidates.len();
            let (step, enter, esc) = ui.input_mut(|i| {
                let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                // `|`, not `||`: every arrow key is consumed even when two are pressed in one frame.
                let step = (k(egui::Key::ArrowRight) | k(egui::Key::ArrowDown)) as isize - (k(egui::Key::ArrowLeft) | k(egui::Key::ArrowUp)) as isize;
                (step, k(egui::Key::Enter), k(egui::Key::Escape))
            });
            // The candidates are only listed while the name search is empty: Enter must not insert a hidden one.
            if n > 0 && self.query.trim().is_empty() {
                self.cand_sel = self.cand_sel.saturating_add_signed(step).min(n - 1);
                if enter {
                    picked = Some(self.candidates[self.cand_sel]);
                }
            }
            if esc {
                self.strokes.clear();
                self.request();
            }
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.strokes.is_empty(), egui::Button::new("Clear")).clicked() {
                self.strokes.clear();
                self.request();
            }
            if ui.add_enabled(!self.strokes.is_empty(), egui::Button::new("Undo stroke")).clicked() {
                self.strokes.pop();
                self.request();
            }
            match &self.rec {
                Rec::Building | Rec::Idle => {
                    let pulse = ui.style().animation_time > 0.0 && state.config.animations;
                    let fill = crate::skeleton::fill(ui, pulse);
                    let (bar, _) = ui.allocate_exact_size(vec2(36.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(bar, 3.0, fill);
                    ui.weak("Preparing...");
                }
                Rec::Failed(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, format!("recognizer: {e}"));
                }
                Rec::Ready => {}
            }
        });
        ui.separator();
        let spacing = 3.0;
        let cols = (((ui.available_width() + spacing) / (CELL + spacing)).floor() as usize).max(1);
        let search_id = egui::Id::NULL.with("symbol_search_input");
        let grid_id = egui::Id::NULL.with("symbol_grid_keys");
        let showing_grid = !(self.query.trim().is_empty() && !self.strokes.is_empty());
        let search_focused_before = ui.memory(|m| m.has_focus(search_id));
        if showing_grid && search_focused_before {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(search_id, egui::EventFilter { vertical_arrows: true, escape: true, ..Default::default() });
            });
            let list_len = self.filtered.len();
            let (down, enter, esc) = ui.input_mut(|i| {
                let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                (k(egui::Key::ArrowDown), k(egui::Key::Enter), k(egui::Key::Escape))
            });
            if enter && list_len > 0 {
                let idx = self.grid_sel.min(list_len - 1);
                picked = Some(self.filtered[idx]);
            } else if down && list_len > 0 {
                self.grid_sel = if cols < list_len { (self.grid_sel + cols).min(list_len - 1) } else { 0 };
                ui.memory_mut(|m| m.request_focus(grid_id));
                self.grid_scroll_to = crate::sidebar::outline::reveal(self.grid_sel / cols, CELL + spacing, self.grid_scroll_y, self.grid_view_h);
            }
            if esc {
                if !self.query.is_empty() {
                    self.query.clear();
                } else {
                    state.focus_editor = true;
                }
            }
        }
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .id(search_id)
                .hint_text("Search symbols by name")
                .desired_width(f32::INFINITY),
        );
        crate::theme::name_field(&resp, "Search symbols by name");
        if take_focus {
            resp.request_focus();
        }
        self.refilter();

        let table = symbols();
        let math = crate::theme::families().math;
        if !showing_grid {
            ui.add_space(4.0);
            if self.candidates.is_empty() {
                ui.weak(match (self.rec == Rec::Ready, self.answered) {
                    (false, _) => "Candidates appear when the recognizer is ready.",
                    (true, false) => "Thinking...",
                    (true, true) => "No match. Try drawing larger or adding strokes.",
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                for (k, &i) in self.candidates.iter().enumerate() {
                    if symbol_button(ui, &table[i as usize], &math, focused && k == self.cand_sel, false) {
                        picked = Some(i);
                    }
                }
            });
        } else {
            let list_len = self.filtered.len();
            if list_len > 0 && self.grid_sel >= list_len {
                self.grid_sel = list_len - 1;
            }
            let grid_resp = ui.interact(ui.available_rect_before_wrap(), grid_id, Sense::focusable_noninteractive());
            let grid_focused = grid_resp.has_focus();
            if grid_focused {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        grid_id,
                        egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() },
                    );
                });
                let (left, right, up, down, home, end, enter, esc) = ui.input_mut(|i| {
                    let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                    (
                        k(egui::Key::ArrowLeft),
                        k(egui::Key::ArrowRight),
                        k(egui::Key::ArrowUp),
                        k(egui::Key::ArrowDown),
                        k(egui::Key::Home),
                        k(egui::Key::End),
                        k(egui::Key::Enter),
                        k(egui::Key::Escape),
                    )
                });
                if list_len > 0 {
                    let mut moved = false;
                    if left {
                        self.grid_sel = self.grid_sel.saturating_sub(1);
                        moved = true;
                    }
                    if right {
                        self.grid_sel = (self.grid_sel + 1).min(list_len - 1);
                        moved = true;
                    }
                    if up {
                        if self.grid_sel < cols {
                            ui.memory_mut(|m| m.request_focus(search_id));
                        } else {
                            self.grid_sel -= cols;
                            moved = true;
                        }
                    }
                    if down {
                        self.grid_sel = (self.grid_sel + cols).min(list_len - 1);
                        moved = true;
                    }
                    if home {
                        self.grid_sel = 0;
                        moved = true;
                    }
                    if end {
                        self.grid_sel = list_len - 1;
                        moved = true;
                    }
                    if moved {
                        self.grid_scroll_to = crate::sidebar::outline::reveal(self.grid_sel / cols, CELL + spacing, self.grid_scroll_y, self.grid_view_h);
                    }
                    if enter {
                        picked = Some(self.filtered[self.grid_sel]);
                    }
                }
                if esc {
                    if !self.query.is_empty() {
                        self.query.clear();
                        self.refilter();
                        ui.memory_mut(|m| m.request_focus(search_id));
                    } else {
                        state.focus_editor = true;
                    }
                }
            }
            let list = &self.filtered;
            if list.is_empty() {
                ui.weak("No symbols match.");
            }
            ui.spacing_mut().item_spacing = vec2(spacing, spacing);
            let rows = list.len().div_ceil(cols);
            let search_focused = resp.has_focus();
            let sel_idx = (search_focused || grid_focused).then_some(self.grid_sel);
            let mut area = egui::ScrollArea::vertical().id_salt("symbol_grid").auto_shrink(false);
            if let Some(y) = self.grid_scroll_to.take() {
                area = area.vertical_scroll_offset(y);
            }
            let out = area.show_rows(ui, CELL, rows, |ui, range| {
                for r in range {
                    ui.horizontal(|ui| {
                        for (c, &i) in list.iter().skip(r * cols).take(cols).enumerate() {
                            let idx = r * cols + c;
                            let is_sel = sel_idx == Some(idx);
                            if symbol_button(ui, &table[i as usize], &math, is_sel, grid_focused && is_sel) {
                                self.grid_sel = idx;
                                picked = Some(i);
                            }
                        }
                    });
                }
            });
            self.grid_scroll_y = out.state.clamped_offset().y;
            self.grid_view_h = out.inner_rect.height();
        }
        if let Some(i) = picked {
            self.insert(state, &table[i as usize]);
        }
    }

    fn insert(&mut self, state: &mut AppState, sym: &SymbolInfo) {
        let Some(tab) = state.tabs.active_tab() else {
            state.status.info("Open a note to insert a symbol");
            return;
        };
        let Some(doc) = state.docs.get(&tab.path) else { return };
        let cursor = tab.cursor.as_ref().map_or(doc.text.len(), |c| c.start).min(doc.text.len());
        let text = insertion_text(sym, state.symbol_mode, doc.kind, &doc.text, cursor, state.config.math_syntax);
        state.commands.push(Command::InsertText(text));
    }

    /// Draws the canvas; true while it has keyboard focus.
    fn canvas(&mut self, ui: &mut egui::Ui) -> bool {
        let side = ui.available_width().clamp(120.0, 300.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
        self.canvas_rect = Some(rect);
        self.canvas_id = Some(resp.id);
        let rel = |p: egui::Pos2| [p.x - rect.min.x, p.y - rect.min.y];
        if resp.drag_started() || resp.clicked() {
            resp.request_focus();
        }
        if resp.drag_started() {
            let mut s = Stroke::new();
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                s.push(rel(p));
            }
            self.current = Some(s);
        }
        if resp.dragged() {
            let moves: Vec<egui::Pos2> = ui.input(|i| i.events.iter().filter_map(|e| if let Event::PointerMoved(p) = e { Some(*p) } else { None }).collect());
            if let Some(cur) = &mut self.current {
                for p in moves {
                    let q = rel(p);
                    if cur.last().is_none_or(|l| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 0.5) {
                        cur.push(q);
                    }
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(s) = self.current.take() {
                if s.len() >= 2 {
                    self.strokes.push(s);
                    self.request();
                }
            }
        }
        let v = ui.visuals();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, v.extreme_bg_color);
        painter.rect_stroke(rect, 4.0, v.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
        let guide = PaintStroke::new(1.0, v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.5));
        painter.line_segment([rect.center_top(), rect.center_bottom()], guide);
        painter.line_segment([rect.left_center(), rect.right_center()], guide);
        if self.strokes.is_empty() && self.current.is_none() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Draw a symbol here", FontId::proportional(13.0), v.weak_text_color());
        }
        let ink = PaintStroke::new(2.5, v.text_color());
        for s in self.strokes.iter().chain(self.current.iter()) {
            let pts: Vec<egui::Pos2> = s.iter().map(|p| pos2(rect.min.x + p[0], rect.min.y + p[1])).collect();
            if pts.len() >= 2 {
                painter.add(egui::Shape::line(pts, ink));
            }
        }
        if self.current.is_some() {
            ui.ctx().request_repaint();
        }
        resp.has_focus()
    }
}

/// The only caller of `typst_assets::fonts()`: each inlined copy of it duplicated the embedded font bytes in the binary.
#[inline(never)]
pub fn iron_sheets_assets_fonts() -> Vec<&'static [u8]> {
    typst_assets::fonts().collect()
}

/// One glyph button; returns true when clicked. Tooltip: large glyph, name, code points.
fn symbol_button(ui: &mut egui::Ui, s: &SymbolInfo, math: &egui::FontFamily, selected: bool, ring: bool) -> bool {
    let text = RichText::new(glyph_text(s)).family(math.clone()).size(20.0);
    let resp = ui.add_sized([CELL, CELL], egui::Button::new(text).selected(selected));
    if ring {
        let sel = ui.visuals().selection.stroke;
        ui.painter().rect_stroke(resp.rect.shrink(1.0), crate::theme::tokens::RADIUS, egui::Stroke::new((sel.width + 1.0).max(2.0), sel.color), egui::StrokeKind::Inside);
    }
    let clicked = resp.clicked();
    resp.on_hover_ui(|ui| {
        ui.label(RichText::new(glyph_text(s)).family(math.clone()).size(44.0));
        ui.label(RichText::new(&s.name).strong());
        let cps: Vec<String> = s.value.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
        ui.weak(cps.join(" "));
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, ch: char) -> SymbolInfo {
        SymbolInfo { name: name.into(), value: ch.to_string(), ch: Some(ch), deprecated: false }
    }

    #[test]
    fn math_heuristic() {
        let t = "a $x + \\$ y$ b $";
        assert!(!typst_in_math(t, 1));
        assert!(typst_in_math(t, 5));
        assert!(typst_in_math(t, 11)); // after the escaped dollar, still inside
        assert!(!typst_in_math(t, 14));
        assert!(typst_in_math(t, t.len()));
    }

    #[test]
    fn auto_modes() {
        let s = sym("arrow.r", '\u{2192}');
        let t = "x $ $ y";
        assert_eq!(insertion_text(&s, InsertMode::Auto, FileKind::Typst, t, 3, MathSyntax::Typst), "arrow.r");
        assert_eq!(insertion_text(&s, InsertMode::Auto, FileKind::Typst, t, 0, MathSyntax::Typst), "#sym.arrow.r");
        assert_eq!(insertion_text(&s, InsertMode::Auto, FileKind::Markdown, "a $x$ b", 3, MathSyntax::Typst), "arrow.r");
        assert_eq!(insertion_text(&s, InsertMode::Auto, FileKind::Markdown, "a $x$ b", 7, MathSyntax::Typst), "\u{2192}");
        assert_eq!(insertion_text(&s, InsertMode::Character, FileKind::Typst, t, 0, MathSyntax::Typst), "\u{2192}");
        assert_eq!(insertion_text(&s, InsertMode::Name, FileKind::Markdown, t, 0, MathSyntax::Typst), "arrow.r");
    }

    #[test]
    fn latex_mode_never_inserts_typst_syntax_in_markdown() {
        let ins = |s: &SymbolInfo, m, k, t, c| insertion_text(s, m, k, t, c, MathSyntax::Latex);
        let (alpha, arrow) = (sym("alpha", '\u{3b1}'), sym("arrow.r", '\u{2192}'));
        for mode in [InsertMode::Auto, InsertMode::Name, InsertMode::SymName] {
            assert_eq!(ins(&alpha, mode, FileKind::Markdown, "a $x$ b", 3), "\\alpha");
            assert_eq!(ins(&arrow, mode, FileKind::Markdown, "a $x$ b", 3), "\u{2192}");
        }
        assert_eq!(ins(&alpha, InsertMode::Auto, FileKind::Markdown, "a $x$ b", 7), "\u{3b1}");
        // .typ notes stay Typst.
        assert_eq!(ins(&arrow, InsertMode::Auto, FileKind::Typst, "x $ $ y", 3), "arrow.r");
    }

    #[test]
    fn auto_mode_follows_the_formula_under_the_cursor() {
        let ins = |s: &SymbolInfo, t, c| insertion_text(s, InsertMode::Auto, FileKind::Markdown, t, c, MathSyntax::Auto);
        let alpha = sym("alpha", '\u{3b1}');
        let t = "a $\\frac{1}{2}$ b $x$ $$ $$ c";
        assert_eq!(ins(&alpha, t, 6), "\\alpha"); // inside a LaTeX formula
        assert_eq!(ins(&alpha, t, t.find("$x$").unwrap() + 1), "alpha"); // inside a Typst formula
        assert_eq!(ins(&alpha, t, 0), "\u{3b1}"); // outside any formula
    }
}
