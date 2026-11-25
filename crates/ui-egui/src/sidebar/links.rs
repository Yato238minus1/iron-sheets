//! Links panel: backlinks (background thread), outgoing links, tags.
use super::links_model::{LinkRow, LinksModel};
use super::outline::Jump;
use crate::state::AppState;
use egui::{Align2, Sense, WidgetInfo, pos2, vec2};

const ROW_H: f32 = 22.0;

#[derive(Default)]
pub struct LinksPanel {
    pub model: LinksModel,
    /// Keyboard selection (row index and the row, so a rebuilt list drops it); preserved across focus loss.
    sel: Option<(usize, LinkRow)>,
    last_path: Option<std::path::PathBuf>,
    scroll_y: f32,
    view_h: f32,
    scroll_to: Option<f32>,
}

impl LinksPanel {
    pub fn selected(&self) -> Option<usize> {
        self.sel.as_ref().map(|(i, _)| *i)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let take_focus = std::mem::take(&mut state.focus_sidebar);
        let path = state.active_path();
        if self.last_path != path {
            self.last_path = path.clone();
            self.sel = None;
        }
        self.model.update(state, path.as_deref());
        if self.model.is_pending() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.model.rows().is_empty() {
            self.sel = None;
            ui.add_space(8.0);
            ui.weak(self.model.empty_message());
            return;
        }
        let mut out = Vec::new();
        let rows = self.model.rows();
        let nt = ui.input(|i| i.modifiers.command);
        let pid = egui::Id::NULL.with("links_keys");
        if take_focus {
            ui.memory_mut(|m| m.request_focus(pid));
        }
        let nav = super::outline::panel_keys(ui, pid);
        let pid_focused = ui.memory(|m| m.has_focus(pid));
        if self.sel.as_ref().is_some_and(|(i, r)| rows.get(*i) != Some(r)) {
            self.sel = None;
        }
        let at = self.sel.as_ref().map(|s| s.0);
        let page = ((self.view_h / ROW_H).floor() as usize).max(1);
        let next = match nav.jump {
            Some(Jump::Home) => self.model.step_sel(None, 1),
            Some(Jump::End) => self.model.step_sel(None, -1),
            Some(Jump::PageUp) => {
                let mut cur = at;
                for _ in 0..page {
                    if let Some(n) = self.model.step_sel(cur, -1) {
                        cur = Some(n);
                    }
                }
                cur
            }
            Some(Jump::PageDown) => {
                let mut cur = at;
                for _ in 0..page {
                    if let Some(n) = self.model.step_sel(cur, 1) {
                        cur = Some(n);
                    }
                }
                cur
            }
            None if nav.step != 0 => self.model.step_sel(at, nav.step),
            None => None,
        };
        if let Some(next) = next {
            self.sel = Some((next, rows[next].clone()));
            self.scroll_to = super::outline::reveal(next, ROW_H, self.scroll_y, self.view_h);
        }
        if nav.enter {
            if let Some((_, row)) = &self.sel {
                out.extend(row.command(path.as_deref(), nt));
            }
        }
        if nav.esc {
            state.focus_editor = true;
        }
        let sel = self.sel.as_ref().map(|s| s.0);
        let mut area = egui::ScrollArea::vertical().id_salt("links").auto_shrink(false);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let mut clicked = None;
        let sel_stroke = ui.visuals().selection.stroke;
        let ring_stroke = egui::Stroke::new((sel_stroke.width + 1.0).max(2.0), sel_stroke.color);
        let res = area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for i in range {
                let row = &rows[i];
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                // Middle-click acts like Ctrl+click: open in a new tab.
                let mid = resp.middle_clicked();
                let (hit, nt) = (resp.clicked() || mid, nt || mid);
                let font = egui::TextStyle::Body.resolve(ui.style());
                let painter = ui.painter().with_clip_rect(rect);
                let pos = pos2(rect.left() + 6.0, rect.center().y);
                if sel == Some(i) {
                    painter.rect_filled(rect, 2.0, crate::theme::selected_bg(ui.visuals()));
                }
                if pid_focused && sel == Some(i) {
                    ui.painter().rect_stroke(rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
                } else {
                    crate::theme::focus_ring(ui, rect, &resp);
                }
                // Headers and notes are not targets: clicking them must not select a row Enter cannot open.
                if hit && row.command(None, false).is_some() {
                    clicked = Some((i, row.clone()));
                }
                match row {
                    LinkRow::Header(t) => {
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, t));
                        painter.text(pos, Align2::LEFT_CENTER, t, font, ui.visuals().strong_text_color());
                    }
                    LinkRow::Note(t) => {
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, t));
                        painter.text(pos, Align2::LEFT_CENTER, t, font, ui.visuals().weak_text_color());
                    }
                    LinkRow::Back { text, .. } => {
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, text));
                        if resp.hovered() {
                            painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        painter.text(pos + vec2(8.0, 0.0), Align2::LEFT_CENTER, text, font, ui.visuals().hyperlink_color);
                        if hit {
                            out.extend(row.command(path.as_deref(), nt));
                        }
                    }
                    LinkRow::Out { text, resolved, .. } => {
                        resp.widget_info(|| {
                            let name = if *resolved { text.clone() } else { format!("{text} (unresolved)") };
                            WidgetInfo::labeled(egui::Role::Button, true, &name)
                        });
                        if resp.hovered() {
                            painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        let c = if *resolved { ui.visuals().hyperlink_color } else { ui.visuals().warn_fg_color };
                        let r = painter.text(pos + vec2(8.0, 0.0), Align2::LEFT_CENTER, text, font.clone(), c);
                        if !*resolved {
                            // Not colour alone: strike-through plus a text marker.
                            painter.line_segment([r.left_center(), r.right_center()], egui::Stroke::new(1.0, c));
                            painter.text(
                                pos2(r.right() + 6.0, rect.center().y),
                                Align2::LEFT_CENTER,
                                "unresolved",
                                egui::TextStyle::Small.resolve(ui.style()),
                                ui.visuals().weak_text_color(),
                            );
                        }
                        if !*resolved && resp.hovered() {
                            resp.clone().on_hover_text("No note with this name in the workspace");
                        }
                        if hit {
                            out.extend(row.command(path.as_deref(), nt));
                        }
                    }
                    LinkRow::Tag(t) => {
                        let label = format!("#{t}");
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, &label));
                        if resp.hovered() {
                            painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        painter.text(pos + vec2(8.0, 0.0), Align2::LEFT_CENTER, &label, font, ui.visuals().hyperlink_color);
                        if hit {
                            out.extend(row.command(path.as_deref(), nt));
                        }
                    }
                }
            }
        });
        if pid_focused && sel.is_none() {
            ui.painter().rect_stroke(res.inner_rect.shrink(1.0), crate::theme::tokens::RADIUS, ring_stroke, egui::StrokeKind::Inside);
        }
        self.scroll_y = res.state.clamped_offset().y;
        self.view_h = res.inner_rect.height();
        if clicked.is_some() {
            self.sel = clicked;
            ui.memory_mut(|m| m.request_focus(pid));
        }
        state.commands.extend(out);
    }
}
