//! Dragging a tree file into the source editor inserts a link; onto the tab strip opens it in a new tab.
use crate::common::*;
use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

/// Welcome.md open in a window with both roots; the tree shows Reading.md, Projects and journal/picture.png.
fn setup() -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    settle(&mut h);
    h.get_all_by_label("Welcome.md").next().expect("row").click();
    settle(&mut h);
    let path = fx.notes().join("Welcome.md");
    (fx, h, path)
}

fn text(h: &mut H, p: &std::path::Path) -> String {
    h.state_mut().app().state.docs.get(p).expect("doc").text.clone()
}

fn row(h: &H, label: &str) -> Pos2 {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("{label}")).rect().center()
}

fn editor_rect(h: &H) -> egui::Rect {
    h.get_by_role(Role::MultilineTextInput).rect()
}

fn button(h: &mut H, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.step();
}

/// Press on `from`, move to `to` in steps (button still down).
fn drag_to(h: &mut H, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    button(h, from, true);
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    h.step();
}

fn drop_at(h: &mut H, from: Pos2, to: Pos2) {
    drag_to(h, from, to);
    button(h, to, false);
    settle(h);
}

#[test]
fn dropping_a_note_on_the_editor_inserts_a_link_at_the_pointer_then_undo() {
    let (_fx, mut h, path) = setup();
    let before = text(&mut h, &path);
    let (src, r) = (row(&h, "Reading.md"), editor_rect(&h));
    // Hover first: caret marker and an accepting ghost.
    drag_to(&mut h, src, r.min + egui::vec2(2.0, 8.0));
    button(&mut h, r.min + egui::vec2(2.0, 8.0), false);
    settle(&mut h);
    assert_eq!(text(&mut h, &path), format!("[[Reading]]{before}"));
    let caret = h.state_mut().app().state.tabs.active_tab().and_then(|t| t.cursor.clone());
    assert_eq!(caret, Some(11..11));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    settle(&mut h);
    assert_eq!(text(&mut h, &path), before);
    // Below the text: at its end.
    let (src, r) = (row(&h, "Reading.md"), editor_rect(&h));
    drop_at(&mut h, src, egui::pos2(r.center().x, r.bottom() - 4.0));
    assert_eq!(text(&mut h, &path), format!("{before}[[Reading]]"));
}

#[test]
fn dropping_an_image_inserts_an_embed() {
    let (_fx, mut h, path) = setup();
    let (src, r) = (row(&h, "picture.png"), editor_rect(&h));
    drop_at(&mut h, src, egui::pos2(r.center().x, r.bottom() - 4.0));
    assert!(text(&mut h, &path).ends_with("![[picture.png]]"), "{}", text(&mut h, &path));
}

#[test]
fn folders_and_cancelled_drags_do_not_touch_the_note() {
    let (_fx, mut h, path) = setup();
    let before = text(&mut h, &path);
    let r = editor_rect(&h);
    let to = egui::pos2(r.center().x, r.bottom() - 4.0);
    let src = row(&h, "Projects");
    drop_at(&mut h, src, to);
    assert_eq!(text(&mut h, &path), before, "folder");
    let src = row(&h, "Reading.md");
    drag_to(&mut h, src, to);
    h.key_press(egui::Key::Escape);
    h.step();
    button(&mut h, to, false);
    settle(&mut h);
    assert_eq!(text(&mut h, &path), before, "Esc");
}

#[test]
fn dropping_a_note_on_the_tab_strip_opens_it_at_that_slot() {
    let (_fx, mut h, path) = setup();
    let tab = h.get_by_role_and_label(Role::Tab, "Welcome.md").rect();
    let (src, to) = (row(&h, "Reading.md"), egui::pos2(tab.left() + 4.0, tab.center().y));
    drag_to(&mut h, src, to);
    button(&mut h, to, false);
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    let names: Vec<_> = st.tabs.tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Reading.md", "Welcome.md"]);
    assert_eq!(st.tabs.active_path(), Some(st.tabs.tabs[0].path.as_path()));
    assert_eq!(st.docs.get(&path).map(|d| d.text.starts_with("# Welcome")), Some(true));
}

/// Mid-drag pictures (need the GPU, so apart from the logic tests): caret marker over the editor, insertion marker on the tab strip.
#[test]
fn screenshots_of_the_two_drop_targets() {
    let (_fx, mut h, _path) = setup();
    let (src, r) = (row(&h, "Reading.md"), editor_rect(&h));
    drag_to(&mut h, src, r.min + egui::vec2(60.0, 40.0));
    shot(&mut h, "tree_dnd_over_editor");
    button(&mut h, r.min + egui::vec2(60.0, 40.0), false);
    settle(&mut h);
    let tab = h.get_all_by_role(Role::Tab).next().expect("tab").rect();
    let src = row(&h, "Ideas.md");
    drag_to(&mut h, src, egui::pos2(tab.right() - 4.0, tab.center().y));
    shot(&mut h, "tree_dnd_over_tabs");
}

#[test]
fn dropping_on_the_view_toolbar_in_source_view_inserts_nothing() {
    let (_fx, mut h, path) = setup();
    h.get_by_label("Source").click();
    settle(&mut h);
    let before = text(&mut h, &path);
    let (src, to) = (row(&h, "Reading.md"), h.get_by_label("Split").rect().center());
    drop_at(&mut h, src, to);
    assert_eq!(text(&mut h, &path), before);
    assert_eq!(h.state_mut().app().state.tabs.tabs.len(), 1);
}

#[test]
fn dropping_an_open_note_moves_its_tab_to_the_slot() {
    let (fx, mut h, _path) = setup();
    for n in ["Ideas.md", "Reading.md"] {
        h.state_mut().app().state.commands.push(iron_sheets_app_core::command::Command::OpenPath { path: fx.notes().join(n), new_tab: true, line: None });
        settle(&mut h);
    }
    let names = |h: &mut H| -> Vec<String> {
        h.state_mut().app().state.tabs.tabs.iter().map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(names(&mut h), ["Welcome.md", "Ideas.md", "Reading.md"]);
    // The tree row is the leftmost match; the tab strip is to its right.
    let src = h.get_all_by_label("Welcome.md").map(|n| n.rect().center()).min_by(|a, b| a.x.total_cmp(&b.x)).expect("row");
    let ideas = h.get_all_by_role(Role::Tab).nth(1).expect("tab").rect();
    drop_at(&mut h, src, egui::pos2(ideas.right() - 2.0, ideas.center().y));
    assert_eq!(names(&mut h), ["Ideas.md", "Welcome.md", "Reading.md"]);
}
