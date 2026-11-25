//! The View menu is grouped into submenus; the current sidebar view, mode and theme are shown as selected.
use crate::common::*;
use egui::vec2;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::state::AppState;

type H = egui_kittest::Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

#[test]
fn view_menu_has_submenus_and_marks_the_current_choice() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.set_size(vec2(1100.0, 720.0));
    settle(&mut h);
    h.get_by_label("View").click();
    settle(&mut h);
    for g in ["Sidebar", "Editor mode", "Theme", "Zoom", "Preview font"] {
        h.get_by_label(g);
    }
    assert!(h.query_by_label("Zoom in").is_none(), "grouped entries are inside their submenu");
    h.get_by_label_contains("Toggle line wrap");
    let _ = shot(&mut h, "viewmenu_open");
    h.get_by_label("Sidebar").click();
    settle(&mut h);
    let files = h.get_by_label_contains("Show files");
    assert_eq!(files.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "Files is the current view");
    assert_ne!(h.get_by_label_contains("Show search").accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
    let _ = shot(&mut h, "viewmenu_sidebar");
    h.get_by_label_contains("Show search").click();
    settle(&mut h);
    assert_eq!(st(&mut h).sidebar_view, iron_sheets::state::SidebarView::Search);
}

/// The compact tab-strip button is "Mode", so "View" names the menu bar menu only.
#[test]
fn compact_toolbar_button_does_not_clash_with_the_view_menu() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(iron_sheets::command::Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    h.set_size(vec2(500.0, 400.0));
    for _ in 0..30 {
        h.step();
    }
    settle(&mut h);
    assert_eq!(h.query_all_by_label("View").count(), 1, "the menu bar menu only");
    h.get_by_label("Mode");
}
