//! Folder notes: a folder holding a note named like it (Foo/Foo.md) opens that note when clicked in the tree.
use crate::common::*;
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::PathBuf;

type H = Harness<'static, Slot>;

/// notes/Foo/{Foo.md, Other.md}; `Projects` (from the fixture) is a plain folder.
fn setup(hide: bool) -> (Fx, H) {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.notes().join("Foo")).expect("mkdir");
    std::fs::write(fx.notes().join("Foo/Foo.md"), "# Foo note\n").expect("write");
    std::fs::write(fx.notes().join("Foo/Other.md"), "# Other\n").expect("write");
    let mut opts = fx.opts(vec![fx.notes()]);
    opts.config.folder_note_hide = hide;
    let mut h = harness(opts);
    h.state_mut().app().state.indexer.wait_idle(std::time::Duration::from_secs(10));
    // Only a hovered or focused folder is read in the background; tests hover first.
    settle(&mut h);
    settle(&mut h);
    (fx, h)
}

fn active(h: &mut H) -> Option<PathBuf> {
    h.state_mut().app().state.active_path()
}

fn expanded(h: &mut H, dir: PathBuf) -> bool {
    h.state_mut().app().state.tree.is_expanded(&dir)
}

/// The tree has a row with this name (the tab strip may show the same title, so the model is asked).
fn has_row(h: &mut H, name: &str) -> bool {
    h.state_mut().app().state.tree.rows().iter().any(|r| r.entry.name == name)
}

fn row_rect(h: &H, label: &str) -> egui::Rect {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("row {label}")).rect()
}

fn click_at(h: &mut H, pos: Pos2, button: PointerButton) {
    // Resting on a folder reads its listing in the background (that is how its row learns it has a folder note).
    h.event(Event::PointerMoved(pos));
    settle(h);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

/// Chevron and name of a depth-1 row (the tree's indent is 14 px, padding 4 px).
fn chevron(h: &H, label: &str) -> Pos2 {
    let r = row_rect(h, label);
    Pos2::new(r.left() + 4.0 + 14.0 + 4.0, r.center().y)
}

fn name(h: &H, label: &str) -> Pos2 {
    let r = row_rect(h, label);
    Pos2::new(r.left() + 4.0 + 14.0 + 40.0, r.center().y)
}

#[test]
fn clicking_the_name_opens_the_note_and_the_chevron_expands() {
    let (fx, mut h) = setup(true);
    let p = name(&h, "Foo");
    h.event(Event::PointerMoved(p));
    settle(&mut h);
    let node = h.get_all_by_label("Foo").next().expect("Foo row");
    assert_eq!(node.accesskit_node().description().as_deref(), Some("folder note"));
    let plain = h.get_all_by_label("Projects").next().expect("Projects row");
    assert_eq!(plain.accesskit_node().description(), None);

    let p = name(&h, "Foo");
    click_at(&mut h, p, PointerButton::Primary);
    assert_eq!(active(&mut h), Some(fx.notes().join("Foo/Foo.md")));
    assert!(!expanded(&mut h, fx.notes().join("Foo")), "the name does not toggle");
    shot(&mut h, "folder_notes_opened");

    let p = chevron(&h, "Foo");
    click_at(&mut h, p, PointerButton::Primary);
    assert!(expanded(&mut h, fx.notes().join("Foo")));
    assert!(h.query_by_label("Other.md").is_some(), "other children show");
    assert!(!has_row(&mut h, "Foo.md"), "the note row is hidden inside its folder");
    shot(&mut h, "folder_notes_expanded_hidden");
}

#[test]
fn middle_click_opens_the_note_in_a_new_tab() {
    let (fx, mut h) = setup(true);
    h.get_all_by_label("Welcome.md").next().expect("row").click();
    settle(&mut h);
    let p = name(&h, "Foo");
    click_at(&mut h, p, PointerButton::Middle);
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 2);
    assert_eq!(st.active_path(), Some(fx.notes().join("Foo/Foo.md")));
}

#[test]
fn the_note_row_shows_when_hiding_is_off() {
    let (fx, mut h) = setup(false);
    let p = chevron(&h, "Foo");
    click_at(&mut h, p, PointerButton::Primary);
    assert!(expanded(&mut h, fx.notes().join("Foo")));
    assert!(has_row(&mut h, "Foo.md"), "note row shown");
    shot(&mut h, "folder_notes_not_hidden");
}

#[test]
fn keyboard_enter_opens_and_arrow_right_expands() {
    let (fx, mut h) = setup(true);
    h.get_all_by_label("Foo").next().expect("row").focus();
    settle(&mut h); // focusing a folder reads its listing
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(active(&mut h), Some(fx.notes().join("Foo/Foo.md")));
    assert!(!expanded(&mut h, fx.notes().join("Foo")), "Enter does not toggle");
    h.get_all_by_label("Foo").next().expect("row").focus();
    settle(&mut h); // focusing a folder reads its listing
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    assert!(expanded(&mut h, fx.notes().join("Foo")));
}

#[test]
fn a_plain_folder_still_toggles_on_click() {
    let (fx, mut h) = setup(true);
    let before = active(&mut h);
    let p = name(&h, "Projects");
    click_at(&mut h, p, PointerButton::Primary);
    assert!(expanded(&mut h, fx.notes().join("Projects")));
    assert_eq!(active(&mut h), before);
    assert!(h.query_by_label("Plan.md").is_some());
}

#[test]
fn revealing_a_hidden_note_selects_its_folder_row() {
    let (fx, mut h) = setup(true);
    let p = name(&h, "Foo");
    h.event(Event::PointerMoved(p));
    settle(&mut h); // Foo's listing is read: its note is known and hidden
    let note = fx.notes().join("Foo/Foo.md");
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::OpenPath { path: note.clone(), new_tab: false, line: None });
    settle(&mut h);
    shot(&mut h, "folder_notes_reveal");
    let st = &mut h.state_mut().app().state;
    let i = st.tree.reveal(&fx.notes().join("Foo/Foo.md")).expect("reveal finds a row");
    assert!(!st.tree.is_expanded(&fx.notes().join("Foo")), "Foo stays collapsed");
    assert_eq!(st.tree.rows()[i].entry.name, "Foo", "the hidden note is shown by its folder row");
}
