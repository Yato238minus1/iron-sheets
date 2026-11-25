use crate::common::*;
use egui::{Event, Modifiers, PointerButton};
use egui_kittest::kittest::Queryable;
use iron_sheets_app_core::command::Command;
use iron_sheets_core::tree::{SortKey, TreeSort};

fn right_click(h: &mut egui_kittest::Harness<'static, Slot>, pos: egui::Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

#[test]
fn removed_folder_can_be_undone_from_the_toast() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    st.tree.set_expanded(&fx.journal(), true);
    st.commands.push(Command::RemoveRoot(0));
    settle(&mut h);
    {
        let st = &h.state_mut().app().state;
        assert_eq!(st.workspace.roots, vec![fx.journal()]);
        assert!(st.notice.is_some());
    }
    h.get_by_label_contains("Removed notes from workspace");
    shot(&mut h, "tree_ux_toast");
    h.get_by_label("Undo").click();
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.workspace.roots, vec![fx.notes(), fx.journal()], "back at its old index");
    assert!(st.tree.is_expanded(&fx.journal()));
    assert!(st.notice.is_none());
}

#[test]
fn undo_with_nothing_removed_only_says_so() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::UndoRemoveRoot);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.workspace.roots, vec![fx.notes()]);
    assert!(st.status.current().is_some_and(|m| m.text == "Nothing to undo"));
}

#[test]
fn empty_space_menu_offers_sort_and_collapse() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    h.state_mut().app().state.tree.set_expanded(&fx.notes(), true);
    settle(&mut h);
    let pos = egui::pos2(h.get_by_label("Projects").rect().center().x, 400.0);
    right_click(&mut h, pos);
    for l in ["New folder", "Add folder to workspace...", "Collapse all"] {
        h.get_by_label(l);
    }
    shot(&mut h, "tree_ux_empty_menu");
    h.get_by_label("Sort").click();
    settle(&mut h);
    shot(&mut h, "tree_ux_empty_sort");
    h.get_by_label("Date modified").click();
    settle(&mut h);
    h.get_by_label("Descending").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.workspace.tree_sort, TreeSort { key: SortKey::Modified, desc: true });
    h.get_by_label("Order"); // the submenu is still open after both clicks
    right_click(&mut h, egui::pos2(pos.x, 650.0)); // right-click while the menu is open
    h.get_by_label("Collapse all").click();
    settle(&mut h);
    assert!(!h.state_mut().app().state.tree.is_expanded(&fx.notes()));
}

#[test]
fn header_sort_menu_sets_key_and_order_in_one_opening() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    let sort = |h: &mut egui_kittest::Harness<'static, Slot>| h.state_mut().app().state.workspace.tree_sort;
    h.get_by_label("Name, asc.").click();
    settle(&mut h);
    for l in ["Sort by", "Name", "Date modified", "Date created", "Order", "Ascending", "Descending"] {
        h.get_by_label(l);
    }
    shot(&mut h, "tree_ux_sort_menu_name");
    h.get_by_label("Date modified").click();
    settle(&mut h);
    assert_eq!(sort(&mut h), TreeSort { key: SortKey::Modified, desc: false }, "changing the key keeps the order");
    h.get_by_label("Descending").click();
    settle(&mut h);
    assert_eq!(sort(&mut h), TreeSort { key: SortKey::Modified, desc: true });
    h.get_by_label("Date created"); // still open after both clicks
    h.get_by_label("Name").click();
    settle(&mut h);
    assert_eq!(sort(&mut h), TreeSort { key: SortKey::Name, desc: true }, "the order stays");
    h.get_by_label("Ascending").click();
    settle(&mut h);
    assert_eq!(sort(&mut h), TreeSort::default());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.query_by_label("Sort by").is_none(), "Esc closes the menu");
}
