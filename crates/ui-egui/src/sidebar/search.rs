//! Workspace search panel: debounced background search, flattened virtualized results.
use super::search_model::{SearchModel, SearchRow, hit_text};
use crate::state::AppState;
use egui::{Align2, Key, Modifiers, Sense, TextFormat, WidgetInfo, pos2, text::LayoutJob, vec2};
use std::ops::{Deref, DerefMut};
use std::time::Instant;

const ROW_H: f32 = 20.0;

#[derive(Default)]
pub struct SearchPanel {
    pub model: SearchModel,
    last_frame: u64,
    scroll_y: f32,
    view_h: f32,
    scroll_to: Option<f32>,
    pending_focus_row: Option<usize>,
    /// Result rows laid out and painted last frame (the viewport range, not the result count).
    pub painted_rows: usize,
}

impl Deref for SearchPanel {
    type Target = SearchModel;
    fn deref(&self) -> &SearchModel {
        &self.model
    }
}

impl DerefMut for SearchPanel {
    fn deref_mut(&mut self) -> &mut SearchModel {
        &mut self.model
    }
}

impl SearchPanel {
    /// Scrolls the list just enough to show `row`.
    fn reveal(&mut self, row: usize) {
        let top = row as f32 * ROW_H;
        let vh = self.view_h.max(ROW_H);
        if top < self.scroll_y {
            self.scroll_to = Some(top);
        } else if top + ROW_H > self.scroll_y + vh {
            self.scroll_to = Some(top + ROW_H - vh);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let ctx = ui.ctx().clone();
        let frame = ctx.cumulative_frame_nr();
        let mut focus = self.last_frame + 1 < frame || std::mem::take(&mut state.focus_sidebar);
        self.last_frame = frame;
        let mut run_now = false;
        if let Some(q) = state.search_request.take() {
            if !q.is_empty() {
                self.model.query = q;
                run_now = true;
            }
            focus = true;
        }
        let mut cmds = Vec::new();
        let mut qchanged = false;
        let mut focus_input_after_clear = false;
        let mut row_step: isize = 0;
        let mut row_enter: Option<bool> = None;
        let mut esc_from_rows = false;
        let input_id = ui.scope_id().with("search_query_input");
        let input_had_focus_before = ui.memory(|m| m.has_focus(input_id));
        if input_had_focus_before {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(input_id, egui::EventFilter { vertical_arrows: true, escape: true, ..Default::default() });
            });
        }
        ui.horizontal(|ui| {
            // Leave exactly the room of the toggles and optional Clear button so the panel never widens.
            let sp = ui.spacing();
            let (pad, gap) = (2.0 * sp.button_padding.x, sp.item_spacing.x);
            let text_w = |t: &str| egui::WidgetText::from(t).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button).size().x;
            let clear_reserve = if self.model.query.is_empty() { 0.0 } else { ui.spacing().interact_size.y + gap };
            let w = ui.available_width() - (text_w("Aa") + text_w("W") + 2.0 * (pad + gap) + clear_reserve);
            let te = egui::TextEdit::singleline(&mut self.model.query).id(input_id).hint_text("Search in workspace").desired_width(w.max(40.0));
            let r = ui.add(te);
            crate::theme::name_field(&r, "Search in workspace");
            if focus {
                r.request_focus();
                if run_now {
                    // Select all so typing replaces the injected query.
                    if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(self.model.query.chars().count()),
                        )));
                        s.store(ui.ctx(), r.id);
                    }
                }
            }
            qchanged |= r.changed();
            // A single-line TextEdit gives up focus on Enter before this code runs: that frame still counts.
            if r.has_focus() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape))) {
                let (mut down, mut up, mut pgdn, mut pgup, mut enter, mut esc) = (false, false, false, false, None, false);
                ui.input_mut(|i| {
                    down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
                    up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
                    pgdn = i.consume_key(Modifiers::NONE, Key::PageDown);
                    pgup = i.consume_key(Modifiers::NONE, Key::PageUp);
                    esc = i.consume_key(Modifiers::NONE, Key::Escape);
                    if i.consume_key(Modifiers::COMMAND, Key::Enter) {
                        enter = Some(true);
                    } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                        enter = Some(false);
                    }
                });
                let page = ((self.view_h / ROW_H).floor() as isize).max(1);
                if down {
                    row_step += 1;
                }
                if up {
                    row_step -= 1;
                }
                if pgdn {
                    row_step += page;
                }
                if pgup {
                    row_step -= page;
                }
                if enter.is_some() {
                    row_enter = enter;
                }
                if esc {
                    if !self.model.query.is_empty() {
                        self.model.query.clear();
                        qchanged = true;
                        focus_input_after_clear = true;
                    } else {
                        state.focus_editor = true;
                    }
                }
            }
            if !self.model.query.is_empty() && crate::icons::button(ui, crate::icons::Icon::Close, "Clear search").clicked() {
                self.model.query.clear();
                qchanged = true;
                focus_input_after_clear = true;
            }
            if focus_input_after_clear {
                r.request_focus();
            }
            qchanged |= ui.selectable_label(self.model.case_sensitive, "Aa").on_hover_text("Match case").clicked() && {
                self.model.case_sensitive = !self.model.case_sensitive;
                true
            };
            qchanged |= ui.selectable_label(self.model.whole_word, "W").on_hover_text("Whole word").clicked() && {
                self.model.whole_word = !self.model.whole_word;
                true
            };
        });
        if qchanged {
            self.model.query_edited(Instant::now());
        }
        // Enter during the debounce runs the search now; otherwise it opens the selected row.
        if let Some(nt) = row_enter {
            if self.model.is_pending() {
                run_now = true;
            } else if let Some(row) = self.model.enter_row() {
                cmds.extend(self.model.open_row(row, nt));
            }
        }
        if let Some(d) = self.model.schedule(state, run_now, Instant::now()) {
            ctx.request_repaint_after(d);
        }
        self.model.poll(&state.workspace.roots);

        ui.weak(self.model.status_text());
        ui.separator();

        let n = self.model.rows().len();
        if row_step != 0 {
            let steps = row_step.unsigned_abs();
            let dir = row_step.signum();
            let mut landed = None;
            for _ in 0..steps {
                landed = self.model.step_sel(dir);
            }
            if let Some(row) = landed {
                self.reveal(row);
            }
        }

        let mut area = egui::ScrollArea::vertical().id_salt("search_results").auto_shrink(false);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let mut clicked: Option<(usize, bool)> = None;
        let mut painted = 0;
        let mut row_had_focus = false;
        let pending_focus_row = self.pending_focus_row.take();
        let out = area.show_rows(ui, ROW_H, n, |ui, range| {
            let model = &self.model;
            let rows = model.rows();
            let sel = model.selected();
            painted = range.len();
            for i in range {
                let row = rows[i];
                let (SearchRow::File(g) | SearchRow::Hit(g, _)) = row;
                let grp = model.group(g);
                let row_id = ui.scope_id().with(("search_row", i));
                let (_, rect) = ui.allocate_space(vec2(ui.available_width(), ROW_H));
                let resp = ui.interact(rect, row_id, Sense::click());
                if pending_focus_row == Some(i) {
                    resp.request_focus();
                }
                if resp.has_focus() {
                    row_had_focus = true;
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(row_id, egui::EventFilter { vertical_arrows: true, escape: true, ..Default::default() });
                    });
                }
                let is_sel = sel == Some(i);
                let weak = if is_sel { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                let base = crate::theme::row_text_color(
                    ui.visuals(),
                    if is_sel { crate::theme::RowKind::Active } else { crate::theme::RowKind::Normal },
                    resp.hovered(),
                );
                crate::theme::focus_ring(ui, rect, &resp);
                if is_sel {
                    ui.painter().rect_filled(rect, 2.0, crate::theme::selected_bg(ui.visuals()));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let painter = ui.painter().with_clip_rect(rect);
                let font = egui::TextStyle::Body.resolve(ui.style());
                let cy = rect.center().y;
                if let SearchRow::File(_) = row {
                    resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, is_sel, format!("{} ({})", grp.name, grp.hits.len())));
                    let r = painter.text(pos2(rect.left() + 2.0, cy), Align2::LEFT_CENTER, &grp.name, font.clone(), base);
                    let r2 = painter.text(pos2(r.right() + 6.0, cy), Align2::LEFT_CENTER, &grp.dir, egui::TextStyle::Small.resolve(ui.style()), weak);
                    painter.text(pos2(rect.right() - 4.0, cy), Align2::RIGHT_CENTER, grp.hits.len().to_string(), font, weak);
                    let _ = r2;
                } else {
                    let SearchRow::Hit(_, h) = row else { continue };
                    let hit = &grp.hits[h];
                    resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, is_sel, format!("{}: {}", hit.line + 1, hit.line_text.trim())));
                    let num = format!("{}", hit.line + 1);
                    painter.text(pos2(rect.left() + 28.0, cy), Align2::RIGHT_CENTER, num, font.clone(), weak);
                    let ht = hit_text(hit);
                    let t = ht.text;
                    let mut j = LayoutJob::default();
                    j.wrap.max_rows = 1;
                    j.wrap.max_width = f32::INFINITY;
                    let n = TextFormat { font_id: font.clone(), color: base, ..Default::default() };
                    let m = TextFormat {
                        font_id: font,
                        color: ui.visuals().strong_text_color(),
                        background: ui.visuals().selection.bg_fill.gamma_multiply(1.5),
                        ..Default::default()
                    };
                    if let Some((s, e)) = ht.matched {
                        j.append(&t[..s], 0.0, n.clone());
                        j.append(&t[s..e], 0.0, m);
                        j.append(&t[e..], 0.0, n);
                    } else {
                        j.append(t, 0.0, n);
                    }
                    let g = painter.layout_job(j);
                    painter.galley(pos2(rect.left() + 34.0, cy - g.size().y / 2.0), g, base);
                }
                // Middle-click acts like Ctrl+click: open in a new tab.
                if resp.clicked() || resp.middle_clicked() {
                    clicked = Some((i, resp.middle_clicked() || ui.input(|i| i.modifiers.command)));
                }
            }
        });
        self.painted_rows = painted;
        self.scroll_y = out.state.clamped_offset().y;
        self.view_h = out.inner_rect.height();
        if row_had_focus {
            let mut rstep: isize = 0;
            let mut rjump: Option<isize> = None;
            let mut renter: Option<bool> = None;
            ui.input_mut(|i| {
                let page = ((self.view_h / ROW_H).floor() as isize).max(1);
                if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                    rstep += 1;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                    rstep -= 1;
                }
                if i.consume_key(Modifiers::NONE, Key::PageDown) {
                    rstep += page;
                }
                if i.consume_key(Modifiers::NONE, Key::PageUp) {
                    rstep -= page;
                }
                if i.consume_key(Modifiers::NONE, Key::Home) {
                    rjump = Some(0);
                }
                if i.consume_key(Modifiers::NONE, Key::End) {
                    rjump = Some(n as isize);
                }
                if i.consume_key(Modifiers::COMMAND, Key::Enter) {
                    renter = Some(true);
                } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                    renter = Some(false);
                }
                if i.consume_key(Modifiers::NONE, Key::Escape) {
                    esc_from_rows = true;
                }
            });
            if let Some(j) = rjump {
                let target = if j == 0 {
                    self.model.set_selected(None);
                    self.model.step_sel(1)
                } else {
                    self.model.set_selected(None);
                    self.model.step_sel(-1)
                };
                if let Some(row) = target {
                    self.reveal(row);
                    self.pending_focus_row = Some(row);
                }
            } else if rstep != 0 {
                let steps = rstep.unsigned_abs();
                let dir = rstep.signum();
                let mut landed = None;
                for _ in 0..steps {
                    landed = self.model.step_sel(dir);
                }
                if let Some(row) = landed {
                    self.reveal(row);
                    self.pending_focus_row = Some(row);
                }
            }
            if let Some(nt) = renter {
                if let Some(row) = self.model.enter_row() {
                    cmds.extend(self.model.open_row(row, nt));
                }
            }
            if esc_from_rows {
                if !self.model.query.is_empty() {
                    ui.memory_mut(|m| m.request_focus(input_id));
                } else {
                    state.focus_editor = true;
                }
            }
        }
        if let Some((row, nt)) = clicked {
            self.model.set_selected(Some(row));
            self.pending_focus_row = Some(row);
            cmds.extend(self.model.open_row(row, nt));
        }
        state.commands.extend(cmds);
    }
}
