//! Wiki-link autocomplete: candidate list cached per index generation, ranking only when the query changes.
use crate::editor::wiki_model::WikiModel;
use crate::state::AppState;
use egui::{Id, Key, Pos2, Rect, Ui};
use iron_sheets_md::edit::EditOp;
use std::path::Path;

#[derive(Default)]
pub struct Wiki {
    pub model: WikiModel,
    /// Selection moved this frame: scroll it into view.
    moved: bool,
}

impl Wiki {
    pub fn active(&self) -> bool {
        self.model.active()
    }

    /// Recomputes the context and items for the cursor. Cheap when nothing changed.
    pub fn update(&mut self, state: &AppState, text: &str, cursor: usize, from: &Path) {
        self.model.update(state, text, cursor, from);
    }

    /// Consumes the popup keys. Returns an edit when an item was accepted. `escaped` is set when Escape closed the popup.
    pub fn keys(&mut self, ui: &Ui, escaped: &mut bool) -> Option<EditOp> {
        if !self.active() {
            return None;
        }
        let (up, down, enter, tab, esc) = ui.input_mut(|i| {
            let plain = !i.modifiers.command && !i.modifiers.alt;
            if !plain {
                return (false, false, false, false, false);
            }
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Tab),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            self.model.move_sel(-1);
            self.moved = true;
        }
        if down {
            self.model.move_sel(1);
            self.moved = true;
        }
        if esc {
            self.model.dismiss();
            *escaped = true;
        }
        if enter || tab { self.model.accept(self.model.selected()) } else { None }
    }

    /// Paints the popup below the caret; returns an accepted edit when an item was clicked.
    pub fn popup(&mut self, ui: &Ui, caret: Rect) -> Option<EditOp> {
        if !self.active() {
            return None;
        }
        let mut clicked = None;
        let pos = Pos2::new(caret.left(), caret.bottom() + 2.0);
        let moved = std::mem::take(&mut self.moved);
        egui::Area::new(Id::NULL.with("wiki_popup")).order(egui::Order::Foreground).fixed_pos(pos).constrain(true).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(260.0);
                egui::ScrollArea::vertical().max_height(220.0).auto_shrink([true, true]).show(ui, |ui| {
                    let sel = self.model.selected();
                    let (f, txt, weak) = (egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color(), ui.visuals().weak_text_color());
                    for (i, it) in self.model.items().iter().enumerate() {
                        let mut job = egui::text::LayoutJob::default();
                        job.append(&it.label, 0.0, egui::TextFormat::simple(f.clone(), txt));
                        if !it.dim.is_empty() {
                            job.append("   ", 0.0, egui::TextFormat::simple(f.clone(), weak));
                            job.append(&it.dim, 0.0, egui::TextFormat::simple(f.clone(), weak));
                        }
                        let r = ui.selectable_label(i == sel, job);
                        if i == sel && moved {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            });
        });
        clicked.and_then(|i| self.model.accept(i))
    }
}
