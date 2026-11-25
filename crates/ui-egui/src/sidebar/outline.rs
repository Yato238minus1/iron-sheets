//! Outline: headings of the active document, cursor heading highlighted.
use super::outline_model::{OutlineModel, Refresh};
use crate::command::Command;
use crate::state::AppState;
use crate::theme::{RowKind, focus_ring, row_text_color, selected_bg};
use egui::{Align2, Key, Modifiers, Sense, WidgetInfo, pos2, vec2};
use std::time::Instant;

const ROW_H: f32 = 22.0;

#[derive(Default)]
pub struct OutlinePanel {
    pub model: OutlineModel,
    /// Keyboard selection; kept across focus loss and cleared only when the active note changes.
    sel: Option<usize>,
    last_path: Option<std::path::PathBuf>,
    scroll_y: f32,
    view_h: f32,
    scroll_to: Option<f32>,
}

impl OutlinePanel {
    pub fn selected(&self) -> Option<usize> {
        self.sel
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Jump {
    Home,
    End,
    PageUp,
    PageDown,
}

pub(super) struct PanelNav {
    pub step: isize,
    pub jump: Option<Jump>,
    pub enter: bool,
    pub esc: bool,
}

/// One focus stop for a whole virtualized list (rows come and go): keys pressed while it has focus.
pub(super) fn panel_keys(ui: &mut egui::Ui, id: egui::Id) -> PanelNav {
    let r = ui.interact(ui.available_rect_before_wrap(), id, Sense::focusable_noninteractive());
    if !r.has_focus() {
        return PanelNav { step: 0, jump: None, enter: false, esc: false };
    }
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(id, egui::EventFilter { vertical_arrows: true, escape: true, ..Default::default() });
    });
    ui.input_mut(|i| {
        let step = i.consume_key(Modifiers::NONE, Key::ArrowDown) as isize - i.consume_key(Modifiers::NONE, Key::ArrowUp) as isize;
        let jump = if i.consume_key(Modifiers::NONE, Key::Home) {
            Some(Jump::Home)
        } else if i.consume_key(Modifiers::NONE, Key::End) {
            Some(Jump::End)
        } else if i.consume_key(Modifiers::NONE, Key::PageUp) {
            Some(Jump::PageUp)
        } else if i.consume_key(Modifiers::NONE, Key::PageDown) {
            Some(Jump::PageDown)
        } else {
            None
        };
        let enter = i.consume_key(Modifiers::NONE, Key::Enter);
        let esc = i.consume_key(Modifiers::NONE, Key::Escape);
        PanelNav { step, jump, enter, esc }
    })
}

/// Scroll offset that shows row `row` of a list scrolled to `scroll_y` in a view `view_h` high, if it changes.
pub(super) fn reveal(row: usize, row_h: f32, scroll_y: f32, view_h: f32) -> Option<f32> {
    let top = row as f32 * row_h;
    let vh = view_h.max(row_h);
    if top < scroll_y {
        Some(top)
    } else if top + row_h > scroll_y + vh {
        Some(top + row_h - vh)
    } else {
        None
    }
}

impl OutlinePanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let take_focus = std::mem::take(&mut state.focus_sidebar);
        let Some(path) = state.active_path() else {
            self.sel = None;
            self.last_path = None;
            ui.add_space(8.0);
            ui.weak("No document open.");
            self.model.clear();
            return;
        };
        if self.last_path.as_ref() != Some(&path) {
            self.last_path = Some(path.clone());
            self.sel = None;
        }
        let Some(doc) = state.docs.get(&path) else {
            ui.add_space(8.0);
            ui.weak("Document not loaded.");
            return;
        };
        if let Refresh::Wait(d) = self.model.update(&path, doc.version, doc.kind, &doc.text, Instant::now()) {
            ui.ctx().request_repaint_after(d);
        }
        let headings = self.model.headings();
        if headings.is_empty() {
            self.sel = None;
            ui.add_space(8.0);
            ui.weak("No headings.");
            return;
        }
        let cur_head = self.model.current(state.tabs.active_tab().and_then(|t| t.cursor.as_ref()).map(|c| c.start));
        let mut out = Vec::new();
        let pid = egui::Id::NULL.with("outline_keys");
        if take_focus {
            ui.memory_mut(|m| m.request_focus(pid));
        }
        let nav = panel_keys(ui, pid);
        let pid_focused = ui.memory(|m| m.has_focus(pid));
        let n = headings.len();
        if let Some(s) = self.sel.as_mut() {
            *s = (*s).min(n - 1);
        }
        let page = ((self.view_h / ROW_H).floor() as usize).max(1);
        let next_sel = match (nav.jump, nav.step) {
            (Some(Jump::Home), _) => Some(0),
            (Some(Jump::End), _) => Some(n - 1),
            (Some(Jump::PageUp), _) => Some(self.sel.or(cur_head).unwrap_or(0).min(n - 1).saturating_sub(page)),
            (Some(Jump::PageDown), _) => Some((self.sel.or(cur_head).unwrap_or(0).min(n - 1) + page).min(n - 1)),
            (None, step) if step != 0 => {
                let at = self.sel.or(cur_head).map(|a| a.min(n.saturating_sub(1)));
                Some(match at {
                    None => if step > 0 { 0 } else { n - 1 },
                    Some(a) if step > 0 => (a + 1).min(n - 1),
                    Some(a) => a.saturating_sub(1),
                })
            }
            _ => None,
        };
        if let Some(next) = next_sel {
            self.sel = Some(next);
            self.scroll_to = reveal(next, ROW_H, self.scroll_y, self.view_h);
        }
        if nav.enter {
            if let Some(h) = self.sel.or(cur_head).and_then(|i| headings.get(i)) {
                out.push(Command::ScrollToLine(h.line));
            }
        }
        if nav.esc {
            state.focus_editor = true;
        }
        let shown = self.sel.or(cur_head);
        let mut area = egui::ScrollArea::vertical().id_salt("outline").auto_shrink(false);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let mut clicked = None;
        let sel_stroke = ui.visuals().selection.stroke;
        let ring_stroke = egui::Stroke::new((sel_stroke.width + 1.0).max(2.0), sel_stroke.color);
        let res = area.show_rows(ui, ROW_H, n, |ui, range| {
            for i in range {
                let h = &headings[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let current = shown == Some(i);
                resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, current, &h.text));
                if current {
                    ui.painter().rect_filled(rect, 2.0, selected_bg(ui.visuals()));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                if pid_focused && current {
                    ui.painter().rect_stroke(rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
                } else {
                    focus_ring(ui, rect, &resp);
                }
                let color = row_text_color(ui.visuals(), if current { RowKind::Active } else { RowKind::Normal }, resp.hovered());
                let x = rect.left() + 6.0 + (h.level.saturating_sub(1)) as f32 * 12.0;
                let font = egui::TextStyle::Body.resolve(ui.style());
                ui.painter().with_clip_rect(rect).text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &h.text, font, color);
                if resp.clicked() {
                    out.push(Command::ScrollToLine(h.line));
                    clicked = Some(i);
                }
            }
        });
        if pid_focused && shown.is_none() {
            ui.painter().rect_stroke(res.inner_rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
        }
        self.scroll_y = res.state.clamped_offset().y;
        self.view_h = res.inner_rect.height();
        if let Some(i) = clicked {
            self.sel = Some(i);
            ui.memory_mut(|m| m.request_focus(pid));
        }
        state.commands.extend(out);
    }
}
