//! The sidebar header shows labels while they fit, icon-only tabs below that, and a "…" menu for what does not fit
//! even as icons; the current view always has a button.
//! Nothing keeps repainting once the width settled.
use crate::common::*;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::state::{AppState, SidebarView};
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

/// All seven views: Calendar is on by default, Git needs a repository in a root.
fn ready(fx: &Fx, pane: f32, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> H {
    let git = |args: &[&str]| {
        let _ = std::process::Command::new("git").current_dir(fx.notes()).args(args).output();
    };
    git(&["init", "-b", "main"]);
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    o.config.sidebar_width = pane;
    f(&mut o.config);
    let mut h = harness(o);
    h.set_size(vec2(1100.0, 720.0));
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    settle(&mut h);
    h
}

const NAMES: [&str; 7] = ["Files", "Search", "Outline", "Links", "Symbols", "Git", "Calendar"];

/// Names of the views with a button in the header row, left to right.
fn header(h: &H) -> Vec<&'static str> {
    let mut v: Vec<_> = NAMES.iter().filter_map(|n| h.query_all_by_label(n).find(|b| b.rect().top() < 70.0).map(|b| (b.rect().left(), *n))).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.into_iter().map(|(_, n)| n).collect()
}

#[test]
fn header_fits_what_it_can_and_the_rest_is_in_the_more_menu() {
    let fx = Fx::new();
    for pane in [140.0, 200.0, 260.0, 400.0] {
        let mut h = ready(&fx, pane, |_| {});
        let shown = header(&h);
        let more = h.query_by_label("…");
        eprintln!("SIDEBAR {pane}: {shown:?} more={}", more.is_some());
        assert!(shown.contains(&"Files"), "the current view has a button at {pane}");
        for n in h.query_all_by_label("…").chain(NAMES.iter().flat_map(|n| h.query_all_by_label(n))).filter(|n| n.rect().top() < 70.0) {
            assert!(n.rect().right() <= pane + 1.0, "{pane}: inside the pane: {:?}", n.rect());
        }
        assert_eq!(more.is_some(), shown.len() < NAMES.len(), "{pane}: a More button exactly when a view is left out");
        let _ = shot(&mut h, &format!("sidebar_header_{pane}"));
    }
    // 260 px is the default: all seven views as icons, no single-button collapse and no More button.
    let h = ready(&fx, 260.0, |_| {});
    assert_eq!(header(&h), NAMES, "usable at the default width");
    assert!(h.query_by_label("…").is_none());
    assert!(h.get_by_label("Files").rect().width() < 30.0, "icon-only");
    // 140 px has room for some icons and the More menu.
    let h = ready(&fx, 140.0, |_| {});
    assert!((3..NAMES.len()).contains(&header(&h).len()), "{:?}", header(&h));
    // Wide: everything, no More button.
    let h = ready(&fx, 600.0, |_| {});
    assert_eq!(header(&h), NAMES);
    assert!(h.query_by_label("…").is_none());
    assert!(h.get_by_label("Files").rect().width() > 30.0, "labels");
}

#[test]
fn the_current_view_stays_visible_when_it_is_in_the_more_menu() {
    let fx = Fx::new();
    let mut h = ready(&fx, 140.0, |_| {});
    assert!(!header(&h).contains(&"Calendar"));
    h.get_by_label("…").click();
    settle(&mut h);
    let _ = shot(&mut h, "sidebar_more_open");
    // Hover first: a click right after the menu opened lands one row off in the harness.
    h.get_by_label("Calendar").hover();
    h.step();
    h.get_by_label("Calendar").click();
    settle(&mut h);
    assert_eq!(st(&mut h).sidebar_view, SidebarView::Calendar);
    assert!(header(&h).contains(&"Calendar"), "the selection is never hidden: {:?}", header(&h));
    for _ in 0..3 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "quiet once settled");
    }
}

#[test]
fn hidden_views_leave_the_header_but_stay_reachable() {
    let fx = Fx::new();
    let mut h = ready(&fx, 600.0, |c| c.sidebar_views = vec!["search".into(), "-outline".into()]);
    let shown = header(&h);
    assert_eq!(&shown[..2], ["Search", "Files"], "the configured order");
    assert!(!shown.contains(&"Outline"), "hidden");
    assert!(iron_sheets::command::all_commands().iter().any(|c| c.id == "view.outline"), "still a command (palette, View menu)");
    st(&mut h).commands.push(iron_sheets::command::Command::SetSidebarView(SidebarView::Outline));
    settle(&mut h);
    assert!(header(&h).contains(&"Outline"), "the current view is shown even when hidden");
    st(&mut h).commands.push(iron_sheets::command::Command::SetSidebarView(SidebarView::Files));
    settle(&mut h);
    assert!(!header(&h).contains(&"Outline"));
}

#[test]
fn settings_reorder_and_hide_the_views() {
    let fx = Fx::new();
    let mut h = ready(&fx, 600.0, |_| {});
    st(&mut h).settings.open = true;
    st(&mut h).settings.search = "Sidebar views".into();
    settle(&mut h);
    h.get_by_label("Move Links up").click();
    settle(&mut h);
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Outline").click();
    settle(&mut h);
    let _ = shot(&mut h, "sidebar_views_settings");
    assert_eq!(st(&mut h).config.sidebar_views[..4], ["files", "search", "links", "-outline"]);
    assert!(!header(&h).contains(&"Outline"));
    assert_eq!(&header(&h)[..3], ["Files", "Search", "Links"]);
}
