//! Calendar module: month grid with marked days, click creates and opens a daily note, keyboard moves the selection.
use crate::common::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_app_core::calendar::Ymd;

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn open_nov_2026(fx: &Fx) -> H {
    let notes = fx.notes();
    std::fs::create_dir_all(notes.join("Daily")).unwrap();
    for d in ["2026-11-03", "2026-11-10", "2026-11-18"] {
        std::fs::write(notes.join("Daily").join(format!("{d}.md")), format!("# {d}\n")).unwrap();
    }
    let mut h = harness(fx.opts(vec![notes]));
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Calendar));
    st(&mut h).ui.calendar.select(Ymd { y: 2026, m: 11, d: 18 });
    settle(&mut h);
    settle(&mut h);
    h
}

#[test]
fn month_grid_marks_days_with_a_daily_note() {
    let fx = Fx::new();
    let mut h = open_nov_2026(&fx);
    for d in [3, 10, 18] {
        assert!(h.query_by_label(&format!("{d} November 2026, has a note")).is_some(), "day {d} marked");
    }
    assert!(h.query_by_label("4 November 2026").is_some() && h.query_by_label("4 November 2026, has a note").is_none());
    assert!(h.query_by_label("November 2026").is_some());
    shot(&mut h, "calendar_month");
}

#[test]
fn clicking_a_day_creates_and_opens_its_note() {
    let fx = Fx::new();
    let mut h = open_nov_2026(&fx);
    h.get_by_label("24 November 2026").click();
    settle(&mut h);
    let p = fx.notes().join("Daily").join("2026-11-24.md");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "# 2026-11-24\n");
    assert_eq!(st(&mut h).tabs.active_path(), Some(p.as_path()));
    // The index picks the new file up and the day gets its mark.
    assert!(st(&mut h).indexer.wait_idle(std::time::Duration::from_secs(10)));
    settle(&mut h);
    assert!(h.query_by_label("24 November 2026, has a note").is_some());
    shot(&mut h, "calendar_created");
}

#[test]
fn keyboard_moves_the_selection_and_enter_opens() {
    let fx = Fx::new();
    let mut h = open_nov_2026(&fx);
    st(&mut h).focus_sidebar = true;
    settle(&mut h);
    h.key_press(egui::Key::ArrowRight);
    h.key_press(egui::Key::ArrowDown);
    settle(&mut h);
    assert_eq!(st(&mut h).ui.calendar.sel, Ymd { y: 2026, m: 11, d: 26 });
    h.key_press(egui::Key::PageDown);
    settle(&mut h);
    assert_eq!(st(&mut h).ui.calendar.sel, Ymd { y: 2026, m: 12, d: 26 });
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert!(fx.notes().join("Daily").join("2026-12-26.md").is_file());
}

#[test]
fn today_command_creates_todays_note_and_the_toggle_hides_the_tab() {
    let fx = Fx::new();
    let mut h = open_nov_2026(&fx);
    st(&mut h).commands.push(Command::OpenDaily(None));
    settle(&mut h);
    let t = iron_sheets_app_core::calendar::Ymd::today();
    let p = fx.notes().join("Daily").join(format!("{:04}-{:02}-{:02}.md", t.y, t.m, t.d));
    assert!(p.is_file());
    st(&mut h).config.module_calendar = false;
    st(&mut h).settings_gen += 1;
    // Settings apply on change of the config; the same path as the Settings toggle.
    iron_sheets_app_core::settings_model::apply(st(&mut h), true);
    settle(&mut h);
    assert!(h.query_by_label("Calendar is off (Settings, Modules).").is_some());
}
