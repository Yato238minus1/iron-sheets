use crate::common::*;
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::keys::parse_shortcut;
use iron_sheets::settings::Section;
use iron_sheets_core::tree::{SortKey, TreeSort};

type H = Harness<'static, Slot>;

fn open_keyboard(h: &mut H) {
    let st = &mut h.state_mut().app().state;
    st.settings.open = true;
    st.settings.section = Section::Keyboard;
    settle(h);
}

fn middle_click(h: &mut H, pos: egui::Pos2) {
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

#[test]
fn keyboard_table_columns_line_up() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_keyboard(&mut h);
    // One row records, one is rebound (shows "Reset"): neither may move the other columns.
    {
        let st = &mut h.state_mut().app().state;
        st.settings.recording = Some(0); // a visible row
    }
    settle(&mut h);
    let xs: Vec<f32> = h.get_all_by_label("Record").map(|n| n.rect().min.x).collect();
    assert!(xs.len() > 3, "several rows visible");
    assert!(xs.iter().all(|x| (x - xs[0]).abs() < 0.5), "Record buttons share one column: {xs:?}");
    let rec = h.get_by_label_contains("Press keys").rect();
    assert!((rec.min.x - xs[0]).abs() < 0.5, "recording row keeps the column");
    shot(&mut h, "settings_tree_keyboard_table");
}

#[test]
fn find_by_shortcut_lists_bound_commands_and_swallows_the_press() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_keyboard(&mut h);
    let sidebar_before = h.state_mut().app().state.config.sidebar_visible;
    h.get_by_label("Find by shortcut").click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::B);
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.config.sidebar_visible, sidebar_before, "the press is search text, not a command");
    let sc = parse_shortcut("Ctrl+B").expect("shortcut");
    let want = st.keymap.others_with(&sc, usize::MAX);
    assert!(!want.is_empty());
    let title = st.keymap.entries[want[0]].title;
    h.get_by_label("Commands bound to Ctrl+B");
    h.get_by_label(title);
    shot(&mut h, "settings_tree_shortcut_search");
    // Esc while waiting cancels without searching.
    h.get_by_label("Clear").click();
    settle(&mut h);
    h.get_by_label("Find by shortcut").click();
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    h.get_by_label("Find by shortcut");
    assert!(h.query_by_label_contains("Commands bound to").is_none());
}

#[test]
fn tree_sort_is_per_workspace_and_folder_view_has_breadcrumb() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    {
        let st = &mut h.state_mut().app().state;
        let s = TreeSort { key: SortKey::Name, desc: true };
        st.workspace.tree_sort = s;
        st.tree.set_sort(s);
    }
    settle(&mut h);
    shot(&mut h, "settings_tree_sorted_desc");
    // Middle-click the Projects folder: it becomes the root and a breadcrumb appears.
    let pos = h.get_by_label("Projects").rect().center();
    middle_click(&mut h, pos);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tree.focus(), Some(fx.notes().join("Projects").as_path()));
    shot(&mut h, "settings_tree_go_into");
    h.get_by_label("notes");
    // The root crumb leaves the folder view; so does Esc on a focused row.
    h.get_by_label("notes").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tree.focus(), None);
    // Persisted with the workspace.
    h.state_mut().app().state.flush_session();
    let ws = iron_sheets_core::workspace::WorkspaceStore::new(&fx.dirs()).load("test").expect("saved workspace");
    assert!(ws.tree_sort.desc);
}
