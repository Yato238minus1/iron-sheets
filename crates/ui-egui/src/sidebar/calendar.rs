//! Calendar panel: month grid, days with a daily note are marked, click or Enter opens (creating) the day's note.
//! The marks come from the index inside `CalendarModel::update`, so a frame reads no files.
use crate::command::Command;
use crate::state::AppState;
use egui::{Align2, EventFilter, Key, Modifiers, Sense, WidgetInfo, pos2, vec2};
use iron_sheets_app_core::calendar::{Ymd, days_in_month};

const ROW_H: f32 = 26.0;
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let take_focus = std::mem::take(&mut state.focus_sidebar);
    if !state.modules.enabled(iron_sheets_app_core::modules::CALENDAR) {
        ui.add_space(8.0);
        ui.weak("Calendar is off (Settings, Modules).");
        return;
    }
    let Some(root) = state.workspace.roots.first().cloned() else {
        ui.add_space(8.0);
        ui.weak("Add a folder to the workspace to use daily notes.");
        return;
    };
    let today = Ymd::today();
    ui.ctx().request_repaint_after(std::time::Duration::from_secs(Ymd::secs_to_midnight()));
    let model = &mut state.ui.calendar;
    if !model.update(&state.indexer.index(), &state.config, state.config_gen, Some(&root)) {
        // A scan holds the index: keep the old marks and look again shortly.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut open = None;
    let pid = egui::Id::NULL.with("calendar_keys");
    ui.horizontal(|ui| {
        if ui.small_button("<").on_hover_text("Previous month (Page Up)").clicked() {
            model.move_months(-1);
        }
        let (y, m) = model.month;
        ui.strong(format!("{} {y}", MONTHS[m as usize - 1]));
        if ui.small_button(">").on_hover_text("Next month (Page Down)").clicked() {
            model.move_months(1);
        }
        if ui.small_button("Today").on_hover_text("Go to today").clicked() {
            model.select(today);
        }
    });
    ui.add_space(4.0);
    let (y, m) = model.month;
    let first = Ymd { y, m, d: 1 };
    let lead = first.weekday();
    let dim = days_in_month(y, m);
    let rows = (lead + dim).div_ceil(7);
    // The sidebar clips its views to the pane: size the grid to what is visible, not to the (wider) layout rect.
    let w = ui.available_width().min(ui.clip_rect().right() - ui.cursor().left());
    let (grid, _) = ui.allocate_exact_size(vec2(w, ROW_H * (rows + 1) as f32), Sense::hover());
    let focus = ui.interact(grid, pid, Sense::focusable_noninteractive());
    if take_focus {
        ui.memory_mut(|mem| mem.request_focus(pid));
    }
    if focus.has_focus() {
        ui.memory_mut(|mem| {
            mem.set_focus_lock_filter(pid, EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() })
        });
        let (days, months, enter, esc) = ui.input_mut(|i| {
            let k = |i: &mut egui::InputState, key| i.consume_key(Modifiers::NONE, key) as i64;
            (
                k(i, Key::ArrowRight) - k(i, Key::ArrowLeft) + 7 * (k(i, Key::ArrowDown) - k(i, Key::ArrowUp)),
                k(i, Key::PageDown) - k(i, Key::PageUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if days != 0 {
            model.move_days(days);
        }
        if months != 0 {
            model.move_months(months as i32);
        }
        if enter {
            open = Some(model.sel);
        }
        if esc {
            state.focus_editor = true;
        }
    }
    let (m_y, m_m) = model.month;
    let cell_w = w / 7.0;
    let painter = ui.painter_at(grid);
    let v = ui.visuals();
    let (weak, text, link, sel_bg) = (v.weak_text_color(), v.text_color(), v.hyperlink_color, v.selection.bg_fill);
    let font = egui::TextStyle::Body.resolve(ui.style());
    for (c, name) in WEEKDAYS.iter().enumerate() {
        painter.text(pos2(grid.left() + (c as f32 + 0.5) * cell_w, grid.top() + ROW_H / 2.0), Align2::CENTER_CENTER, name, font.clone(), weak);
    }
    for day in 1..=dim {
        let i = lead + day - 1;
        let rect = egui::Rect::from_min_size(pos2(grid.left() + (i % 7) as f32 * cell_w, grid.top() + (1 + i / 7) as f32 * ROW_H), vec2(cell_w, ROW_H));
        let d = Ymd { y: m_y, m: m_m, d: day };
        let resp = ui.interact(rect, egui::Id::NULL.with(("calendar_day", m_y, m_m, day)), Sense::click());
        let has = model.has_note(day);
        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("{day} {} {m_y}{}", MONTHS[m_m as usize - 1], if has { ", has a note" } else { "" })));
        if resp.clicked() {
            model.select(d);
            ui.memory_mut(|mem| mem.request_focus(pid));
            open = Some(d);
        }
        if d == model.sel {
            painter.rect_filled(rect.shrink(1.0), 3.0, sel_bg);
            if focus.has_focus() {
                crate::theme::focus_ring(ui, rect, &focus);
                painter.rect_stroke(rect.shrink(1.0), 3.0, v.selection.stroke, egui::StrokeKind::Inside);
            }
        } else if resp.hovered() {
            painter.rect_filled(rect.shrink(1.0), 3.0, v.widgets.hovered.weak_bg_fill);
        }
        if d == today {
            painter.rect_stroke(rect.shrink(2.0), 3.0, egui::Stroke::new(1.5, link), egui::StrokeKind::Inside);
        }
        painter.text(rect.center() - vec2(0.0, 2.0), Align2::CENTER_CENTER, day, font.clone(), text);
        if has {
            painter.circle_filled(pos2(rect.center().x, rect.bottom() - 4.0), 2.0, link);
        }
    }
    if let Some(d) = open {
        state.commands.push(Command::OpenDaily(Some(d)));
    }
}
