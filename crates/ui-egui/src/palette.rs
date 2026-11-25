//! Quick switcher (files) and command palette: egui widgets. Ranking and selection live in `palette_model`.
use crate::palette_model::{Nav, RowKind};
use crate::state::AppState;
use egui::{Align2, Color32, Key, Modifiers, Sense, TextFormat, WidgetInfo, pos2, text::LayoutJob, vec2};

pub use crate::palette_model::PaletteMode;

const ROW_H: f32 = 24.0;
const LIST_H: f32 = 336.0;

/// egui side of the palette: focus request and list scrolling. The model (`state.palette`) is in the core.
#[derive(Default)]
pub struct Palette {
    /// `PaletteModel::open_seq` last seen; a change means the palette was (re)opened.
    seen: u32,
    focus: bool,
    scroll_y: f32,
    scroll_to: Option<f32>,
    /// Selected file and since when (prefetch only after it stayed selected for `DWELL`).
    dwell: Option<(std::path::PathBuf, std::time::Instant)>,
}

const DWELL: std::time::Duration = std::time::Duration::from_millis(150);

/// Ellipsizes `dir` so it fits within `max_w` (measured by `measure`), returning empty when too narrow.
pub(crate) fn fit_dir_text(dir: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> String {
    if max_w < 12.0 || dir.is_empty() {
        return String::new();
    }
    if measure(dir) <= max_w {
        return dir.to_string();
    }
    let chars: Vec<char> = dir.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        if measure(&cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        String::new()
    } else {
        chars[..lo].iter().collect::<String>() + "…"
    }
}

fn job(text: &str, pos: &[u32], base: Color32, hi: Color32, font: egui::FontId) -> LayoutJob {
    let mut j = LayoutJob::default();
    j.wrap.max_rows = 1;
    j.wrap.max_width = f32::INFINITY;
    let normal = TextFormat { font_id: font.clone(), color: base, ..Default::default() };
    let strong = TextFormat { font_id: font, color: hi, underline: egui::Stroke::new(1.0, hi), ..Default::default() };
    let mut buf = String::new();
    let mut cur_hi = false;
    let mut p = pos.iter().peekable();
    for (i, c) in text.chars().enumerate() {
        while p.peek().is_some_and(|&&x| (x as usize) < i) {
            p.next();
        }
        let is_hi = p.peek().is_some_and(|&&x| x as usize == i);
        if is_hi != cur_hi && !buf.is_empty() {
            j.append(&buf, 0.0, if cur_hi { strong.clone() } else { normal.clone() });
            buf.clear();
        }
        cur_hi = is_hi;
        buf.push(c);
    }
    if !buf.is_empty() {
        j.append(&buf, 0.0, if cur_hi { strong } else { normal });
    }
    j
}

impl Palette {
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if !state.palette.open {
            return;
        }
        // The model is taken out of the state while it reads the rest of it.
        let mut model = std::mem::take(&mut state.palette);
        self.show_model(ctx, state, &mut model);
        state.palette = model;
    }

    fn show_model(&mut self, ctx: &egui::Context, state: &mut AppState, model: &mut crate::palette_model::PaletteModel) {
        if self.seen != model.open_seq {
            self.seen = model.open_seq;
            self.focus = true;
            self.scroll_to = Some(0.0);
        }
        model.refresh(state);
        if model.throttled() {
            ctx.request_repaint_after(std::time::Duration::from_millis(160));
        }
        // Small windows: the list shrinks so the whole palette stays inside.
        let screen_h = ctx.content_rect().height();
        let top_gap = 70.0_f32.min(screen_h * 0.1);
        let list_h = LIST_H.min((screen_h - top_gap - 80.0).max(ROW_H * 2.0));
        // Keys are consumed before the text field sees them.
        let mut enter: Option<bool> = None;
        let mut moved = false;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::COMMAND, Key::Enter) {
                enter = Some(true);
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                enter = Some(false);
            }
            for (key, nav) in [
                (Key::ArrowDown, Nav::Down),
                (Key::ArrowUp, Nav::Up),
                (Key::PageDown, Nav::PageDown),
                (Key::PageUp, Nav::PageUp),
                (Key::Home, Nav::Home),
                (Key::End, Nav::End),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    moved |= model.nav(nav);
                }
            }
        });
        if moved {
            let top = model.selected() as f32 * ROW_H;
            if top < self.scroll_y {
                self.scroll_to = Some(top);
            } else if top + ROW_H > self.scroll_y + list_h {
                self.scroll_to = Some(top + ROW_H - list_h);
            }
        }
        match model.selected_file() {
            Some(p) if state.docs.get(p).is_none() => match &self.dwell {
                Some((d, since)) if d == p => {
                    if since.elapsed() >= DWELL {
                        state.docs.prefetch.request(p);
                    } else {
                        ctx.request_repaint_after(DWELL.saturating_sub(since.elapsed()));
                    }
                }
                _ => {
                    self.dwell = Some((p.to_path_buf(), std::time::Instant::now()));
                    ctx.request_repaint_after(DWELL);
                }
            },
            _ => self.dwell = None,
        }
        if let Some(new_tab) = enter {
            let s = model.selected();
            model.activate(s, new_tab, state);
            return;
        }

        let area = egui::Modal::default_area(egui::Id::NULL.with("iron_sheets_palette")).anchor(Align2::CENTER_TOP, vec2(0.0, top_gap));
        let mut clicked: Option<(usize, bool)> = None;
        let resp = egui::Modal::new(egui::Id::NULL.with("iron_sheets_palette")).area(area).show(ctx, |ui| {
            ui.set_width(580.0_f32.min((ctx.content_rect().width() - 40.0).max(120.0)));
            let hint = model.placeholder();
            let te = egui::TextEdit::singleline(&mut model.query).hint_text(hint).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(6, 6));
            let r = ui.add(te);
            crate::theme::name_field(&r, "Quick switcher");
            if self.focus {
                r.request_focus();
                self.focus = false;
            }
            if r.changed() {
                model.query_changed();
                self.scroll_to = Some(0.0);
            }
            ui.add_space(4.0);
            let n = model.rows().len();
            if let Some(status) = model.status() {
                ui.weak(status);
            }
            if n == 0 {
                if model.status().is_none() {
                    ui.weak("No matches");
                }
                return;
            }
            let mut area = egui::ScrollArea::vertical().id_salt("palette_list").max_height(list_h).auto_shrink([false, true]);
            if let Some(y) = self.scroll_to.take() {
                area = area.vertical_scroll_offset(y);
            }
            let out = area.show_rows(ui, ROW_H, n, |ui, range| {
                for i in range {
                    let row = &model.rows()[i];
                    let sel = model.selected();
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                    resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, i == sel, &row.name));
                    crate::theme::focus_ring(ui, rect, &resp);
                    if i == sel {
                        ui.painter().rect_filled(rect, 3.0, crate::theme::selected_bg(ui.visuals()));
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                    }
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let is_sel = i == sel;
                    let base = crate::theme::row_text_color(
                        ui.visuals(),
                        if is_sel { crate::theme::RowKind::Active } else { crate::theme::RowKind::Normal },
                        resp.hovered(),
                    );
                    let hi = ui.visuals().warn_fg_color;
                    let weak = if is_sel { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
                    let painter = ui.painter().with_clip_rect(rect);
                    let small = egui::TextStyle::Small.resolve(ui.style());
                    let hint_galley = if row.hint.is_empty() {
                        None
                    } else {
                        Some(painter.layout_no_wrap(row.hint.clone(), small.clone(), weak))
                    };
                    let hint_w = hint_galley.as_ref().map_or(0.0, |g| g.size().x);
                    let right_limit = if hint_galley.is_some() { rect.right() - 8.0 - hint_w - 10.0 } else { rect.right() - 8.0 };
                    let text_clip = egui::Rect::from_min_max(rect.min, pos2(right_limit.max(rect.left()), rect.bottom()));
                    let text_painter = ui.painter().with_clip_rect(text_clip.intersect(rect));
                    let g = text_painter.layout_job(job(&row.name, &row.name_pos, base, hi, font.clone()));
                    let mut x = rect.left() + 8.0;
                    let w = g.size().x;
                    text_painter.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, base);
                    x += w + 10.0;
                    if !row.dir.is_empty() && x < right_limit {
                        let max_dir_w = (right_limit - x).max(0.0);
                        let fitted = fit_dir_text(&row.dir, max_dir_w, |s| {
                            text_painter.layout_no_wrap(s.to_string(), small.clone(), weak).size().x
                        });
                        if !fitted.is_empty() {
                            let kept_chars = fitted.chars().count().saturating_sub(usize::from(fitted.ends_with('…')));
                            let pos: Vec<u32> = if fitted == row.dir {
                                row.dir_pos.clone()
                            } else {
                                row.dir_pos.iter().copied().filter(|&p| (p as usize) < kept_chars).collect()
                            };
                            let g = text_painter.layout_job(job(&fitted, &pos, weak, hi, small));
                            text_painter.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, weak);
                        }
                    }
                    if let Some(hg) = hint_galley {
                        painter.galley(pos2(rect.right() - 8.0 - hg.size().x, rect.center().y - hg.size().y / 2.0), hg, weak);
                    }
                    // Middle-click opens a note in a new tab; it never runs a command (a stray wheel-click on "Quit").
                    let mid = resp.middle_clicked() && matches!(row.kind, RowKind::File(_) | RowKind::Create(_));
                    if resp.clicked() || mid {
                        clicked = Some((i, mid || ui.input(|i| i.modifiers.command)));
                    }
                }
            });
            self.scroll_y = out.state.clamped_offset().y;
        });
        if let Some((i, nt)) = clicked {
            model.activate(i, nt, state);
        } else if resp.should_close() {
            model.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit_dir_text;

    #[test]
    fn fit_dir_text_ellipsizes_to_avoid_hint_overlap() {
        let m = |s: &str| s.chars().count() as f32 * 8.0;
        assert_eq!(fit_dir_text("notes/sub", 200.0, m), "notes/sub");
        let f = fit_dir_text("very/long/nested/folder/path", 96.0, m);
        assert!(f.ends_with('…') && m(&f) <= 96.0, "fitted={f}");
        assert_eq!(fit_dir_text("notes/sub", 8.0, m), "");
    }
}
