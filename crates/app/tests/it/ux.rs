//! UI polish and accessibility: active-row colours, accessible names, tree keyboard navigation,
//! contrast, and screenshots of the main views (dark, light and 150 % zoom).
use crate::common::*;
use egui::accesskit::Toggled;
use egui::{Key, Modifiers, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets::theme::{self, contrast_pairs, contrast_ratio};
use iron_sheets_core::config::ThemePref;
use std::path::PathBuf;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, theme: ThemePref, zoom: f32) -> H {
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = theme;
    o.config.ui_zoom = zoom;
    let mut h = harness(o);
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    h
}

fn open(h: &mut H, p: PathBuf) {
    st(h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
}

fn visuals(h: &H) -> egui::Visuals {
    h.ctx.global_style().visuals.clone()
}

fn color_of(rows: &[(String, egui::Color32)], name: &str) -> egui::Color32 {
    rows.iter().find(|(n, _)| n.ends_with(name)).unwrap_or_else(|| panic!("row {name} not drawn: {rows:?}")).1
}

#[test]
fn active_tab_is_strong_inactive_grey() {
    for t in [ThemePref::Dark, ThemePref::Light] {
        let fx = Fx::new();
        let mut h = ready(&fx, t, 1.0);
        open(&mut h, fx.notes().join("Ideas.md"));
        open(&mut h, fx.notes().join("Welcome.md"));
        let v = visuals(&h);
        let rows = theme::recorded_rows(&h.ctx, "tabs");
        assert_eq!(color_of(&rows, "Welcome.md"), v.strong_text_color());
        assert_eq!(color_of(&rows, "Ideas.md"), v.text_color());
        assert_ne!(v.strong_text_color(), v.text_color());
    }
}

#[test]
fn active_file_and_folders_are_strong_in_tree() {
    let fx = Fx::new();
    let mut h = ready(&fx, ThemePref::Dark, 1.0);
    open(&mut h, fx.notes().join("Projects/Plan.md"));
    settle(&mut h);
    let v = visuals(&h);
    let rows = theme::recorded_rows(&h.ctx, "files");
    assert_eq!(color_of(&rows, "Plan.md"), v.strong_text_color(), "active file");
    assert_eq!(color_of(&rows, "Projects"), v.strong_text_color(), "folder containing the active file");
    assert_eq!(color_of(&rows, "Ideas.md"), v.text_color(), "other notes stay grey");
    assert_ne!(color_of(&rows, "picture.png"), v.strong_text_color());
    // Clicking another folder makes that one the active folder.
    h.get_by_label("Archive").click();
    settle(&mut h);
    let rows = theme::recorded_rows(&h.ctx, "files");
    assert_eq!(color_of(&rows, "Archive"), v.strong_text_color());
    assert_eq!(color_of(&rows, "Projects"), v.text_color());
    // Hovering a grey row brightens it.
    h.get_by_label("Ideas.md").hover();
    settle(&mut h);
    let rows = theme::recorded_rows(&h.ctx, "files");
    assert_eq!(color_of(&rows, "Ideas.md"), v.strong_text_color());
}

#[test]
fn custom_rows_expose_names_roles_and_selected_state() {
    let fx = Fx::new();
    let mut h = ready(&fx, ThemePref::Dark, 1.0);
    open(&mut h, fx.notes().join("Reading.md"));
    open(&mut h, fx.notes().join("Welcome.md"));
    // Tabs.
    let tab = h.get_by_role_and_label(Role::Tab, "Welcome.md");
    assert_eq!(tab.accesskit_node().toggled(), Some(Toggled::True));
    let other = h.get_by_role_and_label(Role::Tab, "Reading.md");
    assert_eq!(other.accesskit_node().toggled(), Some(Toggled::False));
    h.get_by_label("Close Welcome.md");
    h.get_by_label("New tab");
    // Tree rows.
    let row = h.get_by_role_and_label(Role::TreeItem, "Welcome.md");
    assert_eq!(row.accesskit_node().toggled(), Some(Toggled::True));
    assert_eq!(h.get_by_role_and_label(Role::TreeItem, "Ideas.md").accesskit_node().toggled(), Some(Toggled::False));
    // Sidebar tabs are toggle buttons with names.
    assert!(h.get_all_by_role_and_label(Role::Button, "Files").count() >= 1);
    // Outline.
    view(&mut h, SidebarView::Outline);
    h.get_by_role_and_label(Role::Button, "Welcome");
    // Links: unresolved targets are announced.
    open(&mut h, fx.notes().join("Ideas.md"));
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Links));
    settle(&mut h);
    assert!(h.get_all_by_label_contains("Welcome").count() >= 1);
    // Search hits.
    st(&mut h).commands.push(Command::GlobalSearch { query: Some("Welcome".into()) });
    settle(&mut h);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.get_all_by_label_contains("Welcome.md (").count() >= 1);
    // Palette rows.
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    h.get_by_label("Quick switcher");
    let first = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().toggled() == Some(Toggled::True));
    assert!(first.is_some(), "palette marks the selected row");
}

fn view(h: &mut H, v: SidebarView) {
    st(h).commands.push(Command::SetSidebarView(v));
    settle(h);
}

#[test]
fn tree_arrow_key_navigation() {
    let fx = Fx::new();
    let mut h = ready(&fx, ThemePref::Dark, 1.0);
    let rows: Vec<(PathBuf, bool)> = st(&mut h).tree.rows().iter().map(|r| (r.entry.path.clone(), r.entry.is_dir)).collect();
    let i = (0..rows.len() - 1).find(|&i| !rows[i].1 && !rows[i + 1].1).expect("two adjacent files");
    let name = |p: &PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
    h.get_by_role_and_label(Role::TreeItem, &name(&rows[i].0)).focus();
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::TreeItem, &name(&rows[i].0)).is_focused());
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::TreeItem, &name(&rows[i + 1].0)).is_focused(), "ArrowDown moves to the next row");
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::TreeItem, &name(&rows[i].0)).is_focused(), "ArrowUp moves back");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(st(&mut h).active_path().as_ref(), Some(&rows[i + 1].0), "Enter opens the focused file");

    // Left collapses an expanded folder, Right expands it again, Left on a file jumps to its folder.
    let dir = fx.notes().join("Projects");
    st(&mut h).reveal_request = Some(dir.join("Plan.md"));
    settle(&mut h);
    assert!(st(&mut h).tree.is_expanded(&dir));
    h.get_by_role_and_label(Role::TreeItem, "Projects").focus();
    settle(&mut h);
    h.key_press(Key::ArrowLeft);
    settle(&mut h);
    assert!(!st(&mut h).tree.is_expanded(&dir), "Left collapses");
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    assert!(st(&mut h).tree.is_expanded(&dir), "Right expands");
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    assert!(
        h.get_by_role_and_label(Role::TreeItem, "Archive").is_focused() || h.get_by_role_and_label(Role::TreeItem, "Plan.md").is_focused(),
        "Right on an open folder enters it"
    );
    h.key_press(Key::ArrowLeft);
    settle(&mut h);
    assert!(h.get_by_role_and_label(Role::TreeItem, "Projects").is_focused(), "Left on a child goes to the parent");
    h.key_press(Key::End);
    settle(&mut h);
    let last = st(&mut h).tree.rows().last().map(|r| name(&r.entry.path)).expect("rows");
    assert!(h.get_by_role_and_label(Role::TreeItem, &last).is_focused(), "End goes to the last row");
}

#[test]
fn contrast_of_theme_pairs_meets_aa() {
    for hc in [false, true] {
        for dark in [false, true] {
            for (name, fg, bg, min) in contrast_pairs(dark, true, hc) {
                let r = contrast_ratio(fg, bg);
                assert!(r >= min, "{name} dark={dark} hc={hc}: {r:.2} < {min}");
            }
        }
    }
}

#[test]
fn screenshots_of_main_views() {
    for (tag, theme, zoom) in [("dark", ThemePref::Dark, 1.0), ("light", ThemePref::Light, 1.0), ("dark150", ThemePref::Dark, 1.5)] {
        let fx = Fx::new();
        let mut h = ready(&fx, theme, zoom);
        shot(&mut h, &format!("ux-{tag}-01-start-tree"));
        open(&mut h, fx.notes().join("Projects/Plan.md"));
        open(&mut h, fx.notes().join("Ideas.md"));
        shot(&mut h, &format!("ux-{tag}-02-split"));
        view(&mut h, SidebarView::Outline);
        shot(&mut h, &format!("ux-{tag}-03-outline"));
        view(&mut h, SidebarView::Links);
        shot(&mut h, &format!("ux-{tag}-04-links"));
        h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
        view(&mut h, SidebarView::Symbols);
        shot(&mut h, &format!("ux-{tag}-05-symbols"));
        h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Panic);
        st(&mut h).commands.push(Command::GlobalSearch { query: Some("Plan".into()) });
        for _ in 0..30 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        shot(&mut h, &format!("ux-{tag}-06-search"));
        view(&mut h, SidebarView::Files);
        st(&mut h).commands.push(Command::FindInNote);
        settle(&mut h);
        shot(&mut h, &format!("ux-{tag}-07-find-empty"));
        h.event(egui::Event::Text("Wel".into()));
        settle(&mut h);
        shot(&mut h, &format!("ux-{tag}-07-find"));
        h.key_press(Key::Escape);
        settle(&mut h);
        h.key_press_modifiers(Modifiers::COMMAND, Key::O);
        settle(&mut h);
        h.event(egui::Event::Text("pl".into()));
        settle(&mut h);
        shot(&mut h, &format!("ux-{tag}-08-palette"));
        h.key_press(Key::Escape);
        settle(&mut h);
        st(&mut h).commands.push(Command::NewNote { dir: None });
        settle(&mut h);
        shot(&mut h, &format!("ux-{tag}-09-dialog"));
        h.key_press(Key::Escape);
        settle(&mut h);
        assert!(st(&mut h).dialogs.current.is_none(), "Escape closes the dialog");
        if tag == "dark" {
            // Empty start screen: close everything.
            st(&mut h).commands.push(Command::CloseOtherTabs(usize::MAX));
            for _ in 0..3 {
                if let Some(c) = st(&mut h).tabs.tabs.len().checked_sub(1) {
                    st(&mut h).commands.push(Command::CloseTab(c));
                    settle(&mut h);
                }
            }
            shot(&mut h, "ux-dark-10-welcome");
        }
    }
}
