//! Right-clicking while a context menu is open must open the new menu and keep it open.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton};
use egui_kittest::kittest::Queryable;
use iron_sheets_app_core::command::Command;

type H = egui_kittest::Harness<'static, Slot>;

fn right_click(h: &mut H, pos: egui::Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

fn setup() -> (Fx, H) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    let notes = fx.notes();
    let st = &mut h.state_mut().app().state;
    st.tree.set_expanded(&notes, true);
    st.commands.push(Command::OpenPath { path: notes.join("Welcome.md"), new_tab: false, line: None });
    settle(&mut h);
    (fx, h)
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label_contains(label).is_some()
}

#[test]
fn second_right_click_keeps_the_new_menu_open() {
    let (_fx, mut h) = setup();
    let empty = |y| egui::pos2(60.0, y);
    let row = egui::pos2(h.get_by_label("Projects").rect().center().x, h.get_by_label("Projects").rect().center().y);
    let editor = egui::pos2(500.0, 200.0);

    right_click(&mut h, empty(420.0));
    assert!(has(&h, "Collapse all"), "tree menu opens");
    // another spot of the same empty space
    right_click(&mut h, empty(650.0));
    assert!(has(&h, "Collapse all"), "tree menu stays open after a second right-click on empty space");
    // a tree row
    right_click(&mut h, row);
    assert!(has(&h, "Copy path"), "row menu opens over the empty-space menu");
    assert!(!has(&h, "Collapse all"));
    // empty space again, from the row menu
    right_click(&mut h, empty(650.0));
    assert!(has(&h, "Collapse all"), "tree menu opens over the row menu");
    // the editor
    right_click(&mut h, editor);
    assert!(has(&h, "Select all"), "editor menu opens over the tree menu");
    right_click(&mut h, editor - egui::vec2(100.0, 60.0));
    assert!(has(&h, "Select all"), "editor menu stays open after a second right-click");
    right_click(&mut h, empty(650.0));
    assert!(has(&h, "Collapse all"), "tree menu opens over the editor menu");
    assert!(!has(&h, "Select all"));
}
