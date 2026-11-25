use crate::common::*;
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn setup(name: &str, text: &str) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join(name);
    std::fs::write(&path, text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label(name).next().expect("file in tree").click();
    settle(&mut h);
    (fx, h, path)
}

fn text(h: &mut H, p: &std::path::Path) -> String {
    h.state_mut().app().state.docs.get(p).expect("doc").text.clone()
}

fn caret(h: &mut H) -> Option<std::ops::Range<usize>> {
    h.state_mut().app().state.tabs.active_tab().and_then(|t| t.cursor.clone())
}

fn select(h: &mut H, range: std::ops::Range<usize>) {
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.pending_cursor = Some(range);
    }
    settle(h);
}

fn long_note() -> String {
    (0..400).map(|i| format!("line {i} of a long note\n")).collect()
}

#[test]
fn first_open_puts_the_caret_at_the_top_and_typing_does_not_jump() {
    let (_fx, mut h, p) = setup("Long.md", &long_note());
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    settle(&mut h);
    assert!(text(&mut h, &p).starts_with("Xline 0"), "typed at the top");
    let off = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.offset;
    assert!(off < 5.0, "view stayed at the top, offset {off}");
}

#[test]
fn undo_and_redo_leave_the_caret_where_the_change_was() {
    let (_fx, mut h, p) = setup("U.md", "alpha beta gamma delta\n");
    select(&mut h, 6..6);
    h.get_by_role(Role::MultilineTextInput).type_text("XY");
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "alpha XYbeta gamma delta\n");
    select(&mut h, 22..22);
    // The caret move is its own history step; one press must still reach the text change.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "alpha beta gamma delta\n");
    assert_eq!(caret(&mut h), Some(6..6), "undo of an insertion: caret at the start of the removed text");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "alpha XYbeta gamma delta\n");
    assert_eq!(caret(&mut h), Some(8..8), "redo: caret after the re-inserted text");
}

#[test]
fn alt_z_toggles_wrap() {
    let (_fx, mut h, _p) = setup("W.md", "x\n");
    assert!(h.state_mut().app().state.config.editor_wrap);
    h.key_press_modifiers(Modifiers::ALT, Key::Z);
    settle(&mut h);
    assert!(!h.state_mut().app().state.config.editor_wrap);
    h.key_press_modifiers(Modifiers::ALT, Key::Z);
    settle(&mut h);
    assert!(h.state_mut().app().state.config.editor_wrap);
}

#[test]
fn scroll_speed_setting_reaches_egui() {
    let (_fx, mut h, _p) = setup("S.md", "x\n");
    h.state_mut().app().state.config.scroll_speed = 2.0;
    settle(&mut h);
    assert_eq!(h.ctx.options(|o| o.input_options.line_scroll_speed), 80.0);
}

#[test]
fn animation_speed_setting_reaches_the_style() {
    let (_fx, mut h, _p) = setup("S.md", "x\n");
    let default = iron_sheets_ui_egui::anim::DURATION;
    settle(&mut h);
    assert_eq!(h.ctx.global_style().animation_time, default, "speed 1 is the default pace");
    h.state_mut().app().state.config.anim_speed = 2.0;
    // The settings dialog runs this after every change; the theme rebuilds its styles on the bump.
    iron_sheets_app_core::settings_model::apply(&mut h.state_mut().app().state, false);
    settle(&mut h);
    assert_eq!(h.ctx.global_style().animation_time, default / 2.0, "2 makes every transition twice as fast");
}

#[test]
fn context_menu_in_text_and_below_it() {
    let (_fx, mut h, p) = setup("M.md", "see [[Other]] here\nsecond line\n");
    let ed = h.get_by_role(Role::MultilineTextInput);
    ed.click_secondary();
    settle(&mut h);
    shot(&mut h, "editor_ux_menu_text");
    h.get_by_label_contains("Select all").click();
    settle(&mut h);
    assert_eq!(caret(&mut h), Some(0..31), "select all");
    // A right-click inside the selection keeps it (egui collapses it on the press; the menu puts it back).
    let r = h.get_by_role(Role::MultilineTextInput).rect();
    let pos = r.min + egui::vec2(40.0, 8.0);
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
    }
    settle(&mut h);
    assert_eq!(caret(&mut h), Some(0..31), "selection kept by a right-click inside it");
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    // Empty space below the text: the menu opens, the caret goes to the text end without selecting.
    let r = h.get_by_role(Role::MultilineTextInput).rect();
    h.hover_at(egui::pos2(r.center().x, r.max.y - 20.0));
    h.get_by_role(Role::MultilineTextInput).click_secondary();
    settle(&mut h);
    shot(&mut h, "editor_ux_menu_empty");
    assert_eq!(caret(&mut h), Some(31..31), "caret at the text end, nothing selected");
    let _ = p;
}
