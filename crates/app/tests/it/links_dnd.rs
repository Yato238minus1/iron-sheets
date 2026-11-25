//! Dragging files in the tree moves them and rewrites the links to them.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::kittest::Queryable;
use iron_sheets_app_core::command::Command;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

fn setup() -> (Fx, H) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let notes = fx.notes();
    let st = &mut h.state_mut().app().state;
    for d in [notes.clone(), notes.join("Projects"), notes.join("Projects/Archive")] {
        st.tree.set_expanded(&d, true);
    }
    settle(&mut h);
    (fx, h)
}

fn center(h: &H, label: &str) -> Pos2 {
    h.get_by_label(label).rect().center()
}

fn button(h: &mut H, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.step();
}

/// Press on `from`, move over `to` in steps, and return the pointer position there (button still down).
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

fn wait_for(h: &mut H, what: &str, mut done: impl FnMut(&mut H) -> bool) {
    for _ in 0..200 {
        h.step();
        if done(h) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out: {what}");
}

#[test]
fn drag_moves_a_note_and_updates_links_then_undo() {
    let (fx, mut h) = setup();
    let (plan, archive) = (fx.notes().join("Projects/Plan.md"), fx.notes().join("Projects/Archive"));
    let (src, dst) = (center(&h, "Plan.md"), center(&h, "Archive"));
    drag_to(&mut h, src, dst);
    shot(&mut h, "links_dnd_mid_drag");
    button(&mut h, dst, false);
    settle(&mut h);
    // Welcome.md links to it by path: the user is asked before anything moves.
    h.get_by_label("Update links in 1 note?");
    assert!(plan.exists());
    h.get_by_label("Update").click();
    wait_for(&mut h, "notice", |h| h.state_mut().app().state.notice.as_ref().is_some_and(|n| n.text.contains("Updated")));
    assert!(!plan.exists() && archive.join("Plan.md").exists());
    let welcome = std::fs::read_to_string(fx.notes().join("Welcome.md")).unwrap();
    assert!(welcome.contains("[[Projects/Archive/Plan]]") && welcome.contains("[[Ideas]]"), "{welcome}");
    let text = h.state_mut().app().state.notice.as_ref().map(|n| n.text.clone()).unwrap();
    assert_eq!(text, "Moved Plan.md to Archive. Updated 1 link in 1 note");
    shot(&mut h, "links_dnd_after_drop");
    // Undo moves it back and the links with it.
    // The index must have seen the new text of Welcome.md (the watcher reports it a moment later).
    wait_for(&mut h, "index", |h| {
        let ix = h.state_mut().app().state.indexer.index();
        let g = ix.read();
        g.by_path(&fx.notes().join("Welcome.md")).and_then(|id| g.get(id)).is_some_and(|m| m.links.iter().any(|l| l.target == "Projects/Archive/Plan"))
    });
    h.state_mut().app().state.commands.push(Command::Move { from: archive.join("Plan.md"), to: plan.clone(), links: Some(true), dnd: false });
    wait_for(&mut h, "undo", |_| std::fs::read_to_string(fx.notes().join("Welcome.md")).unwrap().contains("[[Projects/Plan]]"));
    assert!(plan.exists());
}

#[test]
fn refused_and_cancelled_drops_do_nothing() {
    let (fx, mut h) = setup();
    let plan = fx.notes().join("Projects/Plan.md");
    // Onto its own folder: no move, no dialog.
    let (src, own) = (center(&h, "Plan.md"), center(&h, "Projects"));
    drag_to(&mut h, src, own);
    button(&mut h, own, false);
    settle(&mut h);
    assert!(plan.exists() && h.query_by_label("Update links in 1 note?").is_none());
    // A folder into its own subtree.
    let (src, sub) = (center(&h, "Projects"), center(&h, "Archive"));
    drag_to(&mut h, src, sub);
    button(&mut h, sub, false);
    settle(&mut h);
    assert!(fx.notes().join("Projects/Archive").is_dir() && !fx.notes().join("Projects/Archive/Projects").exists());
    // Esc while dragging over a valid folder cancels.
    let (src, dst) = (center(&h, "Plan.md"), center(&h, "Archive"));
    drag_to(&mut h, src, dst);
    h.key_press(egui::Key::Escape);
    h.step();
    button(&mut h, dst, false);
    settle(&mut h);
    assert!(plan.exists() && h.query_by_label("Update links in 1 note?").is_none());
}

#[test]
fn click_and_middle_click_on_rows_still_work() {
    let (_fx, mut h) = setup();
    let p = center(&h, "Reading.md");
    h.event(Event::PointerMoved(p));
    h.step();
    button(&mut h, p, true);
    button(&mut h, p, false);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.active_path().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).as_deref(), Some("Reading.md"));
    let p = center(&h, "Ideas.md");
    h.event(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.tabs.len(), 2, "middle click opens a new tab");
}

#[test]
fn drag_that_ends_while_the_tree_is_hidden_is_forgotten() {
    let (fx, mut h) = setup();
    let plan = fx.notes().join("Projects/Plan.md");
    let (src, dst) = (center(&h, "Plan.md"), center(&h, "Archive"));
    drag_to(&mut h, src, dst);
    h.state_mut().app().state.commands.push(Command::ToggleSidebar);
    settle(&mut h);
    button(&mut h, dst, false);
    settle(&mut h);
    h.state_mut().app().state.commands.push(Command::ToggleSidebar);
    settle(&mut h);
    h.event(Event::PointerMoved(dst + egui::vec2(2.0, 0.0)));
    settle(&mut h);
    assert!(plan.exists() && !fx.notes().join("Projects/Archive/Plan.md").exists(), "no drop after the tree comes back");
}

#[test]
fn a_small_movement_with_the_button_down_is_a_click_not_a_drag() {
    let (fx, mut h) = setup();
    let p = center(&h, "Reading.md");
    h.event(Event::PointerMoved(p));
    h.step();
    button(&mut h, p, true);
    for dx in [3.0, 6.5, 7.0] {
        h.event(Event::PointerMoved(p + egui::vec2(dx, 1.0)));
        h.step();
    }
    assert!(!h.state_mut().app().state.ui.files.dragging(), "no drag below the threshold");
    button(&mut h, p + egui::vec2(7.0, 1.0), false);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.active_path(), Some(fx.notes().join("Reading.md")), "the note opened");
}

#[test]
fn drop_on_a_file_row_inside_a_folder_moves_into_that_folder() {
    let (fx, mut h) = setup();
    let plan = fx.notes().join("Projects/Plan.md");
    let (src, row) = (center(&h, "Plan.md"), center(&h, "Older.md"));
    drag_to(&mut h, src, row);
    shot(&mut h, "links_dnd_block_highlight");
    button(&mut h, row, false);
    settle(&mut h);
    h.get_by_label("Update").click();
    wait_for(&mut h, "move", |_| fx.notes().join("Projects/Archive/Plan.md").exists());
    assert!(!plan.exists());
}

fn active_name(h: &mut H) -> Option<String> {
    h.state_mut().app().state.active_path().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
}

#[test]
fn a_release_away_from_the_pressed_row_is_not_a_click() {
    let (fx, mut h) = setup();
    // A long drag on a root row (it cannot be dragged) does not toggle it.
    let label = h.state_mut().app().state.workspace.root_label(0);
    let root = center(&h, &label);
    drag_to(&mut h, root, root + egui::vec2(0.0, 100.0));
    button(&mut h, root + egui::vec2(0.0, 100.0), false);
    settle(&mut h);
    assert!(h.state_mut().app().state.tree.is_expanded(&fx.notes()), "the root stayed open");
    // Press on one note, release on another: neither opens.
    let (a, b) = (center(&h, "Reading.md"), center(&h, "Ideas.md"));
    h.event(Event::PointerMoved(a));
    h.step();
    button(&mut h, a, true);
    h.event(Event::PointerMoved(b));
    h.step();
    button(&mut h, b, false);
    settle(&mut h);
    assert_eq!(active_name(&mut h), None, "nothing opened");
    // A refused drop (onto its own folder) does not open the note either.
    let (src, own) = (center(&h, "Plan.md"), center(&h, "Projects"));
    drag_to(&mut h, src, own);
    button(&mut h, own, false);
    settle(&mut h);
    assert_eq!(active_name(&mut h), None, "a refused drop opens nothing");
}

#[test]
fn the_empty_space_below_the_rows_is_no_drop_target() {
    let (fx, mut h) = setup();
    let src = center(&h, "Plan.md");
    let below = egui::pos2(src.x, 600.0);
    drag_to(&mut h, src, below);
    button(&mut h, below, false);
    settle(&mut h);
    assert!(fx.notes().join("Projects/Plan.md").exists() && h.query_by_label("Update links in 1 note?").is_none());
}
