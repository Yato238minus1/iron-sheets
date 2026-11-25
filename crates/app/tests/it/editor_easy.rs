use crate::common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use std::time::Duration;

fn open(name: &str, text: &str) -> (Fx, egui_kittest::Harness<'static, Slot>) {
    let fx = Fx::new();
    std::fs::write(fx.notes().join(name), text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label(name).next().expect("file in tree").click();
    settle(&mut h);
    (fx, h)
}

#[test]
fn ctrl_b_toggles_sidebar_and_f2_renames() {
    let (_fx, mut h) = open("T.md", "x");
    let before = h.state_mut().app().state.config.sidebar_visible;
    h.key_press_modifiers(Modifiers::COMMAND, Key::B);
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.config.sidebar_visible, !before);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::B);
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.config.sidebar_visible, !before, "Ctrl+Shift+B is bold, not the sidebar");
    h.key_press(Key::F2);
    settle(&mut h);
    assert!(h.state_mut().app().state.dialogs.current.is_some(), "F2 opens the rename prompt");
}

fn doc_text(h: &mut egui_kittest::Harness<'static, Slot>, name: &str) -> String {
    let p = h.state_mut().app().state.tabs.active_path().expect("active").to_path_buf();
    assert!(p.ends_with(name));
    h.state_mut().app().state.docs.get(&p).expect("doc").text.clone()
}

#[test]
fn ctrl_h_focuses_replace_enter_replaces_and_ctrl_alt_enter_replaces_all() {
    use egui::accesskit::Role;
    let (_fx, mut h) = open("T.md", "foo foo foo foo\n");
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    // Ctrl+H focuses the replace field at once: put the query into the find field first.
    h.get_all_by_role(Role::TextInput).next().expect("find field").click();
    settle(&mut h);
    h.get_all_by_role(Role::TextInput).next().expect("find field").type_text("foo");
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    h.get_all_by_role(Role::TextInput).nth(1).expect("replace field").type_text("X");
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "T.md"), "foo foo foo foo\n", "typing in the replace field does not edit the note");
    h.key_press(Key::Enter);
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "T.md"), "X X foo foo\n", "each Enter replaces one match and moves on");
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "T.md"), "X X foo foo\n", "Shift+Enter only moves to the previous match");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, Key::Enter);
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "T.md"), "X X X X\n");
}

#[test]
fn mouse_side_buttons_and_toolbar_arrows_navigate_history() {
    use iron_sheets::command::Command;
    let (fx, mut h) = open("A.md", "a");
    let (a, b) = (fx.notes().join("A.md"), fx.notes().join("B.md"));
    std::fs::write(&b, "b").expect("write");
    h.state_mut().app().state.commands.push(Command::OpenPath { path: b.clone(), new_tab: false, line: None });
    settle(&mut h);
    let active = |h: &mut egui_kittest::Harness<'static, Slot>| h.state_mut().app().state.tabs.active_path().map(|p| p.to_path_buf());
    assert_eq!(active(&mut h), Some(b.clone()));
    let press = |h: &mut egui_kittest::Harness<'static, Slot>, button| {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: egui::pos2(300.0, 300.0), button, pressed, modifiers: Modifiers::NONE });
        }
        settle(h);
    };
    press(&mut h, egui::PointerButton::Extra1);
    assert_eq!(active(&mut h), Some(a.clone()), "back button goes back");
    press(&mut h, egui::PointerButton::Extra2);
    assert_eq!(active(&mut h), Some(b.clone()), "forward button goes forward");
    // Toolbar arrow: the back arrow is a labelled button.
    h.get_by_label("Back (Alt+Left)").click();
    settle(&mut h);
    assert_eq!(active(&mut h), Some(a));
    shot(&mut h, "editor-easy-toolbar-arrows");
}

#[test]
fn line_numbers_gutter_screenshots() {
    use egui::accesskit::Role;
    let mut note = String::new();
    for i in 1..=300 {
        note.push_str(&format!("line {i}\n"));
        if i == 2 {
            note.push_str(&"a long wrapped line of words ".repeat(12));
            note.push('\n');
        }
    }
    let (_fx, mut h) = open("N.md", &note);
    let left = |h: &mut egui_kittest::Harness<'static, Slot>| h.get_by_role(Role::MultilineTextInput).rect().left();
    let off = left(&mut h);
    shot(&mut h, "editor-easy-gutter-off");
    h.state_mut().app().state.config.show_line_numbers = true;
    settle(&mut h);
    assert!(left(&mut h) > off + 15.0, "the text moves right by the gutter width");
    shot(&mut h, "editor-easy-gutter-on");
    // Scrolled down, three-digit numbers.
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").editor_scroll.force = Some(2000.0);
    settle(&mut h);
    shot(&mut h, "editor-easy-gutter-scrolled");
}

#[test]
fn line_numbers_on_a_big_note() {
    let note: String = (1..=40_000).map(|i| format!("line {i}\n")).collect();
    let (_fx, mut h) = open("Big.md", &note);
    h.state_mut().app().state.config.show_line_numbers = true;
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").editor_scroll.force = Some(160_000.0);
    settle(&mut h);
    settle(&mut h);
    shot(&mut h, "editor-easy-gutter-big");
}

#[test]
fn typing_brackets_auto_closes_and_backspace_removes_the_pair() {
    use egui::accesskit::Role;
    let (_fx, mut h) = open("C.md", "");
    h.get_by_role(Role::MultilineTextInput).type_text("(");
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "C.md"), "()");
    h.get_by_role(Role::MultilineTextInput).type_text("x");
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).type_text(")");
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "C.md"), "(x)", "the typed closer skips over the inserted one");
    h.key_press(Key::Backspace);
    h.key_press(Key::End);
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).type_text("[");
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "C.md"), "(x[]");
    h.key_press(Key::Backspace);
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "C.md"), "(x", "Backspace between an empty pair deletes both");
    h.state_mut().app().state.config.auto_close_brackets = false;
    h.get_by_role(Role::MultilineTextInput).type_text("[");
    settle(&mut h);
    assert_eq!(doc_text(&mut h, "C.md"), "(x[", "setting off: plain typing");
}

#[test]
fn replace_field_focus_is_one_shot() {
    use egui::accesskit::Role;
    let (_fx, mut h) = open("T.md", "foo foooooo\n");
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    h.get_all_by_role(Role::TextInput).next().expect("find field").click();
    h.get_all_by_role(Role::TextInput).next().expect("find field").type_text("foo");
    settle(&mut h);
    h.get_all_by_role(Role::TextInput).nth(1).expect("replace field").click();
    h.get_all_by_role(Role::TextInput).nth(1).expect("replace field").type_text("X");
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    // Back to the find field: typing there must stay there.
    h.get_all_by_role(Role::TextInput).next().expect("find field").click();
    settle(&mut h);
    // Raw text events, like a keyboard: `type_text` would refocus the node itself.
    for _ in 0..2 {
        h.event(egui::Event::Text("o".into()));
        settle(&mut h);
    }
    let replace = h.get_all_by_role(Role::TextInput).nth(1).expect("replace field").value();
    assert_eq!(replace.as_deref(), Some("X"), "typing in the find field never lands in the replace field");
}

#[test]
fn mouse_side_buttons_are_ignored_while_a_dialog_or_settings_is_open() {
    use iron_sheets::command::Command;
    let (fx, mut h) = open("A.md", "a");
    let b = fx.notes().join("B.md");
    std::fs::write(&b, "b").expect("write");
    h.state_mut().app().state.commands.push(Command::OpenPath { path: b.clone(), new_tab: false, line: None });
    settle(&mut h);
    h.state_mut().app().state.settings.open = true;
    settle(&mut h);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: egui::pos2(300.0, 300.0), button: egui::PointerButton::Extra1, pressed, modifiers: Modifiers::NONE });
    }
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.active_path().map(|p| p.to_path_buf()), Some(b), "no history step behind Settings");
}

#[test]
fn f2_with_a_dialog_open_keeps_the_typed_name() {
    use iron_sheets::dialogs::Dialog;
    let (_fx, mut h) = open("T.md", "x");
    h.key_press(Key::F2);
    settle(&mut h);
    if let Some(Dialog::Prompt { value, .. }) = h.state_mut().app().state.dialogs.current.as_mut() {
        *value = "typed".into();
    } else {
        panic!("rename prompt expected");
    }
    h.key_press(Key::F2);
    settle(&mut h);
    match &h.state_mut().app().state.dialogs.current {
        Some(Dialog::Prompt { value, .. }) => assert_eq!(value, "typed"),
        _ => panic!("prompt expected"),
    }
}
