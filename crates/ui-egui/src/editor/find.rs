//! Find / replace bar for the source editor.
use crate::editor::find_model::FindModel;
use crate::state::AppState;
use egui::text::CCursor;
use egui::{Color32, Galley, Id, Key, Pos2, Rect, Ui};

#[derive(Default)]
pub struct Find {
    pub model: FindModel,
    focus_field: bool,
    focus_replace: bool,
    /// Field to focus once after the editor grabbed focus for a selection (the replace field after Enter there).
    pub refocus: Option<Id>,
}

fn field_id() -> Id {
    Id::NULL.with("find_query")
}

fn replace_id() -> Id {
    Id::NULL.with("find_replace")
}

impl Find {
    pub fn open_for(&mut self, tab_id: u64, selection: Option<&str>) {
        self.model.open_for(tab_id, selection);
        self.focus_field = true;
    }

    /// Ctrl+H: open the bar with the replace controls expanded and the replace field focused.
    pub fn open_replace(&mut self, tab_id: u64, selection: Option<&str>) {
        self.model.open_replace_for(tab_id, selection);
        self.focus_field = false;
        self.focus_replace = true;
    }

    pub fn shown_for(&self, tab_id: u64) -> bool {
        self.model.shown_for(tab_id)
    }

    fn select_current(&mut self, state: &mut AppState, ti: usize) {
        if self.model.select_current(state, ti) {
            self.refocus.get_or_insert(field_id());
        }
    }

    /// The bar itself; call before the TextEdit so it sits above the scroll area.
    pub fn bar(&mut self, ui: &mut Ui, state: &mut AppState, ti: usize, editor_id: Id) {
        let Some(tab) = state.tabs.tabs.get(ti) else { return };
        let from = tab.cursor.as_ref().map_or(0, |r| r.start);
        let Some(doc) = state.docs.get(&tab.path) else { return };
        self.model.refresh(&doc.text, doc.version, from);
        let can_edit = !doc.read_only;
        let mut nav: isize = 0;
        let (mut do_replace, mut do_all, mut close) = (false, false, false);
        let mut lost_enter = false;
        let mut from_replace = false;
        let mut bar_had_focus = false;
        let shift = ui.input(|i| i.modifiers.shift);
        ui.horizontal_wrapped(|ui| {
            let r = egui::TextEdit::singleline(&mut self.model.query).id(field_id()).hint_text("Find").desired_width(160.0).show(ui);
            crate::theme::name_field(&r.response, "Find in note");
            bar_had_focus |= r.response.has_focus() || r.response.lost_focus();
            if self.focus_field {
                self.focus_field = false;
                r.response.request_focus();
                if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), field_id()) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(CCursor::new(0), CCursor::new(self.model.query.chars().count()))));
                    st.store(ui.ctx(), field_id());
                }
            }
            if r.response.changed() {
                self.model.invalidate();
                let from = state.tabs.tabs.get(ti).and_then(|t| t.cursor.as_ref().map(|c| c.start)).unwrap_or(0);
                if let Some(d) = state.docs.get(&state.tabs.tabs[ti].path) {
                    self.model.refresh(&d.text, d.version, from);
                }
                self.select_current(state, ti);
            }
            if r.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                lost_enter = true;
                nav = if shift { -1 } else { 1 };
            }
            let aa = ui.selectable_label(self.model.case, "Aa").on_hover_text("Match case");
            bar_had_focus |= aa.has_focus() || aa.lost_focus();
            if aa.clicked() {
                self.model.toggle_case();
                aa.request_focus();
                ui.ctx().request_repaint();
            }
            let rx = ui.selectable_label(self.model.regex, ".*").on_hover_text("Regular expression");
            crate::theme::name_field(&rx, "Regular expression");
            bar_had_focus |= rx.has_focus() || rx.lost_focus();
            if rx.clicked() {
                self.model.toggle_regex();
                rx.request_focus();
                ui.ctx().request_repaint();
            }
            if let Some(short) = self.model.error() {
                let col = ui.visuals().error_fg_color;
                ui.painter().rect_stroke(r.response.rect, 2.0, egui::Stroke::new(1.5, col), egui::StrokeKind::Inside);
                ui.label(egui::RichText::new(self.model.label()).color(col)).on_hover_text(self.model.error_detail().unwrap_or(short));
            } else {
                ui.label(self.model.label());
            }
            let prev = crate::icons::button(ui, crate::icons::Icon::Up, "Previous");
            bar_had_focus |= prev.has_focus() || prev.lost_focus();
            if prev.clicked() {
                nav = -1;
                prev.request_focus();
            }
            let next = crate::icons::button(ui, crate::icons::Icon::Down, "Next");
            bar_had_focus |= next.has_focus() || next.lost_focus();
            if next.clicked() {
                nav = 1;
                next.request_focus();
            }
            let rep_toggle = ui.selectable_label(self.model.show_replace, "Replace…").on_hover_text("Show/hide replace");
            bar_had_focus |= rep_toggle.has_focus() || rep_toggle.lost_focus();
            if rep_toggle.clicked() {
                self.model.show_replace = !self.model.show_replace;
                if self.model.show_replace {
                    self.focus_replace = true;
                } else {
                    rep_toggle.request_focus();
                }
            }
            if self.model.show_replace {
                let rr =
                    egui::TextEdit::singleline(&mut self.model.replace).id(replace_id()).hint_text("Replace").desired_width(140.0).interactive(can_edit).show(ui);
                crate::theme::name_field(&rr.response, "Replace with");
                bar_had_focus |= rr.response.has_focus() || rr.response.lost_focus();
                if self.focus_replace {
                    self.focus_replace = false;
                    rr.response.request_focus();
                }
                if rr.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if shift {
                        nav = -1;
                    } else {
                        do_replace = true;
                    }
                    from_replace = nav != 0 || do_replace;
                }
                let rep_btn = ui.add_enabled(can_edit, egui::Button::new("Replace"));
                bar_had_focus |= rep_btn.has_focus() || rep_btn.lost_focus();
                if rep_btn.clicked() {
                    do_replace = true;
                }
                let rep_all_btn = ui.add_enabled(can_edit, egui::Button::new("Replace all"));
                bar_had_focus |= rep_all_btn.has_focus() || rep_all_btn.lost_focus();
                if rep_all_btn.clicked() {
                    do_all = true;
                }
            } else {
                self.focus_replace = false;
            }
            let close_btn = crate::icons::button(ui, crate::icons::Icon::Close, "Close");
            bar_had_focus |= close_btn.has_focus() || close_btn.lost_focus();
            if close_btn.clicked() {
                close = true;
            }
        });
        let esc_pressed = bar_had_focus && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        ui.separator();
        // F3 / Shift+F3 work from anywhere in the editor.
        let km = &state.keymap;
        ui.input_mut(|i| {
            do_all |= crate::keys_egui::editor_pressed(km, i, "editor.replace_all") && can_edit;
            if crate::keys_egui::editor_pressed(km, i, "editor.find_prev") {
                nav = -1;
            } else if crate::keys_egui::editor_pressed(km, i, "editor.find_next") {
                nav = 1;
            }
        });
        if nav != 0 {
            if self.model.go(nav).is_some() {
                self.select_current(state, ti);
            }
            if lost_enter {
                self.refocus.get_or_insert(field_id());
            }
        }
        if do_replace {
            if self.model.replace_and_select_next(state, ti) {
                self.refocus.get_or_insert(field_id());
            }
        }
        if do_all {
            if self.model.replace_all(state, ti) {
                self.refocus.get_or_insert(field_id());
            }
        }
        if from_replace {
            self.refocus = Some(replace_id());
        }
        if close || esc_pressed {
            self.model.close();
            self.refocus = None;
            ui.memory_mut(|m| m.request_focus(editor_id));
        }
    }

    /// Highlights matches in the visible rows and, if requested, keeps focus in the find field.
    pub fn paint(&mut self, ui: &Ui, galley: &Galley, galley_pos: Pos2) {
        if let Some(id) = self.refocus.take().filter(|_| self.model.open) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let matches = self.model.matches();
        if !self.model.open || matches.is_empty() {
            return;
        }
        let cur = self.model.current_index();
        let clip = ui.clip_rect();
        let (top, bottom) = (clip.top() - galley_pos.y, clip.bottom() - galley_pos.y);
        // Char range of the rows that intersect the clip rect.
        let (mut c0, mut c1, mut pos) = (None, 0usize, 0usize);
        for row in &galley.rows {
            if row.min_y() > bottom {
                break; // rows are ordered by y: nothing below the viewport can extend the range
            }
            let len = row.char_count_including_newline().0;
            if row.max_y() >= top {
                c0.get_or_insert(pos);
                c1 = pos + len;
            }
            pos += len;
        }
        let Some(c0) = c0 else { return };
        let first = matches.partition_point(|m| m.c.end <= c0);
        let painter = ui.painter_at(clip);
        for (i, m) in matches.iter().enumerate().skip(first) {
            if m.c.start >= c1 {
                break;
            }
            let a = galley.pos_from_cursor(CCursor::new(m.c.start)).translate(galley_pos.to_vec2());
            let b = galley.pos_from_cursor(CCursor::new(m.c.end)).translate(galley_pos.to_vec2());
            let right = if (a.top() - b.top()).abs() < 1.0 { b.left() } else { clip.right() };
            let col = if i == cur { Color32::from_rgba_unmultiplied(255, 140, 0, 110) } else { Color32::from_rgba_unmultiplied(255, 210, 0, 80) };
            painter.rect_filled(Rect::from_min_max(egui::pos2(a.left(), a.top()), egui::pos2(right.max(a.left() + 2.0), a.bottom())), 2.0, col);
        }
    }
}
