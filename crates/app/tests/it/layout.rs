//! Layout and interaction fixes from the user's test session: zoom shortcuts, empty tab and Ctrl+T, tab drag,
//! small windows, the status bar at small widths, the settings zoom drag and floating windows staying inside.
//! Screenshots go to IRON_SHEETS_SHOTS_DIR (default target/ui-shots) as `layout_*.png`.
use crate::common::*;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Role, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use std::path::PathBuf;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, size: egui::Vec2) -> H {
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    let mut h = harness(o);
    h.set_size(size);
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

fn win(h: &H) -> Rect {
    h.ctx.content_rect()
}

/// (role, label, rect) of every node that has a non-empty rect.
fn nodes(h: &H) -> Vec<(Role, String, Rect)> {
    let ppp = h.ctx.pixels_per_point();
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let ak = n.accesskit_node();
            let b = ak.bounding_box()?;
            let r = Rect::from_min_max(Pos2::new(b.x0 as f32 / ppp, b.y0 as f32 / ppp), Pos2::new(b.x1 as f32 / ppp, b.y1 as f32 / ppp));
            (r.width() > 0.0 && r.height() > 0.0 && r.is_finite()).then(|| (ak.role(), ak.label().or_else(|| ak.value()).unwrap_or_default().to_string(), r))
        })
        .collect()
}

/// Interactive nodes (buttons, tabs, fields, ...) that stick out of the window.
fn outside(h: &H) -> Vec<String> {
    let w = win(h).expand(1.0);
    nodes(h)
        .into_iter()
        .filter(|(role, ..)| {
            matches!(
                role,
                Role::Button
                    | Role::Tab
                    | Role::TextInput
                    | Role::MultilineTextInput
                    | Role::CheckBox
                    | Role::Label
                    | Role::ComboBox
                    | Role::MenuItem
                    | Role::Slider
                    | Role::SpinButton
            )
        })
        // Rows of virtualized lists are clipped by their scroll area, not by the window.
        .filter(|(role, l, _)| !(*role == Role::Button && l.ends_with(".md")))
        .filter(|(_, _, r)| !w.contains_rect(*r))
        .map(|(role, l, r)| format!("{role:?} {l:?} {r:?}"))
        .collect()
}

fn key_with(h: &mut H, m: Modifiers, k: Key) {
    h.key_press_modifiers(m, k);
    settle(h);
}

// ---- 1. zoom shortcuts ----

#[test]
fn ctrl_plus_zooms_in_ctrl_equals_and_zero_reset() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    assert_eq!(st(&mut h).config.ui_zoom, 1.0);
    key_with(&mut h, Modifiers::COMMAND, Key::Plus);
    assert_eq!(st(&mut h).config.ui_zoom, 1.1);
    assert!((h.ctx.zoom_factor() - 1.1).abs() < 1e-4, "egui zoom follows the config, not its own shortcut handling: {}", h.ctx.zoom_factor());
    // US layout: Ctrl+Shift+= produces Plus with Shift held.
    key_with(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Plus);
    assert_eq!(st(&mut h).config.ui_zoom, 1.2);
    key_with(&mut h, Modifiers::COMMAND, Key::Minus);
    assert_eq!(st(&mut h).config.ui_zoom, 1.1);
    // Ctrl+= zooms in as well (US layout without Shift); Ctrl+0 resets zoom and the font sizes.
    key_with(&mut h, Modifiers::COMMAND, Key::Equals);
    assert!((st(&mut h).config.ui_zoom - 1.2).abs() < 1e-4);
    key_with(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Equals);
    assert!((st(&mut h).config.ui_zoom - 1.3).abs() < 1e-4);
    st(&mut h).config.editor_font_size = 20.0;
    key_with(&mut h, Modifiers::COMMAND, Key::Num0);
    assert_eq!(st(&mut h).config.ui_zoom, 1.0);
    assert_eq!(st(&mut h).config.editor_font_size, 14.0);
    assert_eq!(st(&mut h).config.preview_font_size, 15.0);
}

// ---- 2. empty tab (Ctrl+T) ----

#[test]
fn ctrl_t_opens_an_empty_tab_that_the_next_file_overwrites() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    // The empty tab of the fresh start is overwritten by the first file.
    open(&mut h, fx.notes().join("Welcome.md"));
    open(&mut h, fx.notes().join("Ideas.md"));
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    assert_eq!(st(&mut h).tabs.tabs.len(), 3);
    assert!(st(&mut h).tabs.active_is_empty());
    h.get_by_role_and_label(Role::Tab, "New tab");
    // A second Ctrl+T while an empty tab is active only focuses it.
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    assert_eq!(st(&mut h).tabs.tabs.len(), 3, "no second empty tab");
    shot(&mut h, "layout_new_tab");
    // Empty tabs are not stored in the session.
    let sess = st(&mut h).session();
    assert_eq!(sess.tabs.len(), 2);
    assert_eq!(sess.active, Some(2), "active index points past the stored tabs; restore falls back to the first");
    // Opening a file overwrites the active empty tab, even "in a new tab".
    open(&mut h, fx.notes().join("Reading.md"));
    assert_eq!(st(&mut h).tabs.tabs.len(), 3);
    assert_eq!(st(&mut h).tabs.tabs.iter().filter(|t| t.is_empty()).count(), 0);
    assert_eq!(st(&mut h).tabs.active_path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()), Some("Reading.md".into()));
    // The "+" button opens a new empty tab too.
    h.get_by_label("New tab").click();
    settle(&mut h);
    assert!(st(&mut h).tabs.active_is_empty());
    assert_eq!(st(&mut h).tabs.tabs.len(), 4);
    // Closing an empty tab does not put anything into "reopen closed tab".
    let closed_before = st(&mut h).tabs.closed.len();
    key_with(&mut h, Modifiers::COMMAND, Key::W);
    assert_eq!(st(&mut h).tabs.closed.len(), closed_before);
    assert_eq!(st(&mut h).tabs.tabs.len(), 3);
}

#[test]
fn empty_page_lists_recent_files_and_opens_them() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    open(&mut h, fx.notes().join("Ideas.md"));
    st(&mut h).commands.push(Command::CloseActiveTab);
    settle(&mut h);
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    shot(&mut h, "layout_new_tab_recent");
    h.get_by_role_and_label(Role::Link, "Ideas.md").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p.ends_with("Ideas.md")));
}

// ---- 3. tab drag ----

#[test]
fn dragging_a_tab_shows_a_ghost_and_reorders_on_release() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    for n in ["Welcome.md", "Ideas.md", "Reading.md"] {
        open(&mut h, fx.notes().join(n));
    }
    let names =
        |h: &mut H| st(h).tabs.tabs.iter().map(|t| t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect::<Vec<_>>();
    assert_eq!(names(&mut h), ["Welcome.md", "Ideas.md", "Reading.md"]);
    let first = h.get_by_role_and_label(Role::Tab, "Welcome.md").rect();
    let last = h.get_by_role_and_label(Role::Tab, "Reading.md").rect();
    let start = first.center();
    h.event(Event::PointerMoved(start));
    h.step();
    h.event(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let mut p = start;
    while p.x < last.center().x {
        p.x += 25.0;
        p.y += 1.0;
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.step();
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::Grabbing, "a drag is in progress");
    assert_eq!(names(&mut h), ["Welcome.md", "Ideas.md", "Reading.md"], "nothing moves before the release");
    shot(&mut h, "layout_tab_drag");
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert_eq!(names(&mut h), ["Ideas.md", "Reading.md", "Welcome.md"]);
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p.ends_with("Welcome.md")), "the dragged tab stays active");
    shot(&mut h, "layout_tab_drag_done");
}

#[test]
fn tab_drop_target_clamps_to_the_ends() {
    use iron_sheets::tabstrip::drop_index;
    let r = |l: f32, rr: f32| Rect::from_min_max(Pos2::new(l, 0.0), Pos2::new(rr, 26.0));
    let rects = [r(10.0, 90.0), r(92.0, 170.0), r(172.0, 250.0)];
    assert_eq!(drop_index(&rects, -50.0), Some(0));
    assert_eq!(drop_index(&rects, 40.0), Some(0));
    assert_eq!(drop_index(&rects, 91.0), Some(0), "the gap belongs to the tab on its left");
    assert_eq!(drop_index(&rects, 100.0), Some(1));
    assert_eq!(drop_index(&rects, 240.0), Some(2));
    assert_eq!(drop_index(&rects, 900.0), Some(2));
    assert_eq!(drop_index(&[], 5.0), None);
}

// ---- 4. small windows ----

#[test]
fn wide_sidebar_cannot_push_content_out_of_small_windows() {
    for (w, hgt) in [(1100.0, 720.0), (600.0, 400.0), (400.0, 300.0)] {
        let fx = Fx::new();
        let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
        o.config.sidebar_width = 900.0;
        let mut h = harness(o);
        h.set_size(vec2(w, hgt));
        settle(&mut h);
        open(&mut h, fx.notes().join("Welcome.md"));
        open(&mut h, fx.notes().join("Projects/Plan.md"));
        settle(&mut h);
        let bad = outside(&h);
        assert!(bad.is_empty(), "{w}x{hgt}: outside the window: {bad:#?}");
        // The central area keeps its minimum: the tab strip and the "New tab" button are reachable.
        let plus = h.get_by_label("New tab").rect();
        assert!(plus.left() >= win(&h).width() - 330.0 - 1.0 || w > 700.0, "{w}: sidebar leaves 300 px: {plus:?}");
        shot(&mut h, &format!("layout_small_{w}x{hgt}"));
        // Every mode button works at this size.
        for mode in [iron_sheets_core::config::ViewMode::Source, iron_sheets_core::config::ViewMode::Split, iron_sheets_core::config::ViewMode::Preview] {
            st(&mut h).commands.push(Command::SetViewMode(mode));
            settle(&mut h);
            let bad = outside(&h);
            assert!(bad.is_empty(), "{w}x{hgt} {mode:?}: {bad:#?}");
        }
        shot(&mut h, &format!("layout_small_{w}x{hgt}_preview"));
    }
}

#[test]
fn sidebar_is_clamped_when_the_window_shrinks() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    settle(&mut h);
    let side = |h: &H| h.get_by_role_and_label(Role::Tab, "Files").rect().right();
    let _ = side;
    h.set_size(vec2(500.0, 400.0));
    settle(&mut h);
    // The tree rows must lie left of the 300 px reserved for the central area.
    let rows: Vec<_> = nodes(&h).into_iter().filter(|(r, ..)| *r == Role::TreeItem).collect();
    assert!(!rows.is_empty());
    for (_, l, r) in rows {
        assert!(r.left() < 500.0 - 300.0 + 1.0, "{l}: {r:?}");
    }
    assert_eq!(iron_sheets::app::sidebar_width_range(500.0), (140.0, 200.0));
    assert_eq!(iron_sheets::app::sidebar_width_range(400.0), (100.0, 100.0));
    assert_eq!(iron_sheets::app::sidebar_width_range(1600.0), (140.0, 1300.0));
}

// ---- 4/7. status bar ----

fn busy_status(h: &mut H, fx: &Fx) {
    open(h, fx.notes().join("Welcome.md"));
    let name = "A rather long workspace name for a tiny window".to_string();
    st(h).workspace.name = name;
    if let Some(d) = st(h).docs.get_mut(&fx.notes().join("Welcome.md")) {
        d.external_change = true;
    }
    st(h).status.error("Cannot save: something went wrong with a long explanation of why");
    settle(h);
}

fn status_nodes(h: &H) -> Vec<(Role, String, Rect)> {
    let top = win(h).bottom() - 34.0;
    nodes(h)
        .into_iter()
        .filter(|(role, _, r)| {
            r.top() >= top && !matches!(role, Role::GenericContainer | Role::Unknown | Role::Window | Role::Pane | Role::Group | Role::TreeItem | Role::Tree)
        })
        .collect()
}

#[test]
fn status_bar_items_never_overlap_and_follow_reading_order() {
    for w in [1100.0, 700.0, 500.0, 400.0, 300.0] {
        let fx = Fx::new();
        let mut h = ready(&fx, vec2(w, 400.0));
        busy_status(&mut h, &fx);
        let items = status_nodes(&h);
        assert!(!items.is_empty(), "{w}: status bar nodes");
        let win_r = win(&h).expand(1.0);
        for (role, l, r) in &items {
            assert!(win_r.contains_rect(*r), "{w}: {role:?} {l:?} {r:?} outside the window");
        }
        for (i, (_, la, a)) in items.iter().enumerate() {
            for (_, lb, b) in &items[i + 1..] {
                let ov = a.intersect(*b);
                assert!(ov.width() <= 0.5 || ov.height() <= 0.5, "{w}: {la:?} {a:?} overlaps {lb:?} {b:?}");
            }
        }
        // Tree order == visual left-to-right order.
        let lefts: Vec<f32> = items.iter().map(|(_, _, r)| r.left()).collect();
        assert!(
            lefts.windows(2).all(|p| p[0] <= p[1] + 0.5),
            "{w}: reading order differs from visual order: {:?}",
            items.iter().map(|(_, l, r)| (l.clone(), r.left())).collect::<Vec<_>>()
        );
        shot(&mut h, &format!("layout_status_{w}"));
    }
}

#[test]
fn status_bar_drops_low_priority_items_first() {
    let labels = |w: f32| {
        let fx = Fx::new();
        let mut h = ready(&fx, vec2(w, 400.0));
        open(&mut h, fx.notes().join("Welcome.md"));
        status_nodes(&h).into_iter().map(|(_, l, _)| l).collect::<Vec<_>>()
    };
    let wide = labels(1100.0);
    assert!(wide.iter().any(|l| l.contains("words")) && wide.iter().any(|l| l.starts_with("Ln ")), "{wide:?}");
    let narrow = labels(330.0);
    assert!(!narrow.iter().any(|l| l.contains("words")), "word count is the first item to go: {narrow:?}");
    assert!(narrow.iter().any(|l| l == "Saved"), "the save badge stays: {narrow:?}");
}

#[test]
fn status_labels_are_not_selectable() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    let r = h.get_by_label_contains("words").rect();
    h.event(Event::PointerMoved(r.center()));
    h.step();
    // A text cursor over selectable text would show Text; a plain label shows the default arrow.
    assert_ne!(h.output().platform_output.cursor_icon, egui::CursorIcon::Text);
}

// ---- 5. settings: zoom drag ----

fn open_settings_on_zoom(h: &mut H) {
    st(h).settings.open = true;
    st(h).settings.search = "UI zoom".into();
    settle(h);
}

#[test]
fn zoom_drag_value_is_applied_on_release_and_stays_stable_while_dragging() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open_settings_on_zoom(&mut h);
    let spin = h.get_by_label("UI zoom").rect();
    // The drag value is the widget next to the label.
    let target = nodes(&h).into_iter().find(|(role, _, r)| *role == Role::SpinButton && (r.center().y - spin.center().y).abs() < 6.0).expect("drag value").2;
    let start = target.center();
    h.event(Event::PointerMoved(start));
    h.step();
    h.event(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let mut p = start;
    for _ in 0..12 {
        p.x += 10.0;
        h.event(Event::PointerMoved(p));
        h.step();
        assert_eq!(st(&mut h).config.ui_zoom, 1.0, "no live rescaling while the pointer is down");
        assert_eq!(h.ctx.zoom_factor(), 1.0);
    }
    shot(&mut h, "layout_zoom_dragging");
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    let z = st(&mut h).config.ui_zoom;
    assert!(z > 1.3 && z <= 3.0, "applied once on release: {z}");
    assert!((h.ctx.zoom_factor() - z).abs() < 1e-4);
    assert_eq!((z * 100.0).round() / 100.0, z, "two decimals");
    shot(&mut h, "layout_zoom_released");
}

// ---- 6. floating windows stay inside ----

fn find_window(h: &H, title: &str) -> Rect {
    nodes(h)
        .into_iter()
        .find(|(role, l, _)| matches!(*role, Role::Window | Role::Dialog) && l == title)
        .unwrap_or_else(|| panic!("window {title} not found"))
        .2
}

#[test]
fn settings_window_stays_inside_after_resize_and_drag() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    st(&mut h).settings.open = true;
    settle(&mut h);
    let r0 = find_window(&h, "Settings");
    assert!(win(&h).contains_rect(r0), "{r0:?}");
    shot(&mut h, "layout_settings_big");
    for size in [vec2(700.0, 450.0), vec2(500.0, 330.0), vec2(400.0, 300.0)] {
        h.set_size(size);
        settle(&mut h);
        let r = find_window(&h, "Settings");
        assert!(win(&h).expand(1.0).contains_rect(r), "{size:?}: {r:?} not inside {:?}", win(&h));
        shot(&mut h, &format!("layout_settings_{}x{}", size.x, size.y));
    }
    // Drag the title bar out of the window.
    h.set_size(vec2(800.0, 600.0));
    settle(&mut h);
    let r = find_window(&h, "Settings");
    let grab = Pos2::new(r.center().x, r.top() + 10.0);
    h.event(Event::PointerMoved(grab));
    h.step();
    h.event(Event::PointerButton { pos: grab, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let mut p = grab;
    for _ in 0..10 {
        p += vec2(150.0, 100.0);
        h.event(Event::PointerMoved(p));
        h.step();
    }
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    let r = find_window(&h, "Settings");
    assert!(win(&h).expand(1.0).contains_rect(r), "dragged out: {r:?} not inside {:?}", win(&h));
    shot(&mut h, "layout_settings_dragged");
    // Shrink the app window afterwards: the window follows.
    h.set_size(vec2(450.0, 320.0));
    settle(&mut h);
    let r = find_window(&h, "Settings");
    assert!(win(&h).expand(1.0).contains_rect(r), "{r:?}");
}

#[test]
fn dialogs_and_palette_fit_tiny_windows() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(400.0, 300.0));
    st(&mut h).commands.push(Command::NewNote { dir: None });
    settle(&mut h);
    let r = find_window(&h, "New note");
    assert!(win(&h).expand(1.0).contains_rect(r), "{r:?}");
    shot(&mut h, "layout_dialog_400x300");
    h.key_press(Key::Escape);
    settle(&mut h);
    st(&mut h).commands.push(Command::AddRootByPath);
    settle(&mut h);
    let r = find_window(&h, "Select folder");
    assert!(win(&h).expand(1.0).contains_rect(r), "{r:?}");
    shot(&mut h, "layout_folder_dialog_400x300");
    h.key_press(Key::Escape);
    settle(&mut h);
    key_with(&mut h, Modifiers::COMMAND, Key::O);
    let bad = outside(&h);
    assert!(bad.is_empty(), "{bad:#?}");
    shot(&mut h, "layout_palette_400x300");
}

// ---- polish: conflict controls never dropped, narrow split fallback ----

#[test]
fn disk_conflict_controls_survive_the_narrowest_status_bar() {
    for w in [1100.0, 500.0, 360.0, 300.0] {
        let fx = Fx::new();
        let mut h = ready(&fx, vec2(w, 400.0));
        busy_status(&mut h, &fx);
        let items = status_nodes(&h);
        let has = |pred: &dyn Fn(&str) -> bool| items.iter().any(|(role, l, _)| *role == Role::Button && pred(l));
        assert!(has(&|l| l == "Reload"), "{w}: Reload button missing: {items:?}");
        assert!(has(&|l| l == "Keep" || l == "Keep mine"), "{w}: Keep button missing: {items:?}");
        if w <= 360.0 {
            assert!(has(&|l| l == "Keep"), "{w}: compact form expected");
        }
        let win_r = win(&h).expand(1.0);
        for (role, l, r) in &items {
            assert!(win_r.contains_rect(*r), "{w}: {role:?} {l:?} {r:?} outside the window");
        }
        // The controls still work.
        h.get_by_role_and_label(Role::Button, "Reload").click();
        settle(&mut h);
        let p = fx.notes().join("Welcome.md");
        assert!(!st(&mut h).docs.get(&p).is_some_and(|d| d.external_change), "{w}: Reload did not resolve the conflict");
        if w == 300.0 {
            // Screenshot with the controls still showing.
            if let Some(d) = st(&mut h).docs.get_mut(&p) {
                d.external_change = true;
            }
            settle(&mut h);
            shot(&mut h, "layout_status_conflict_300");
        }
    }
}

#[test]
fn split_view_shows_only_the_editor_in_a_very_narrow_window() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 600.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    st(&mut h).commands.push(Command::SetViewMode(iron_sheets_core::config::ViewMode::Split));
    settle(&mut h);
    shot(&mut h, "layout_split_wide");
    let editor_rect = |h: &H| nodes(h).into_iter().find(|(role, ..)| *role == Role::MultilineTextInput).map(|(.., r)| r);
    // The preview is painted (not in the accessibility tree): it counts the frames it was drawn in.
    let preview_visible = |h: &mut H| {
        let before = st(h).ui.md_preview.frames_shown;
        h.step();
        h.step();
        usize::from(st(h).ui.md_preview.frames_shown > before)
    };
    let wide = editor_rect(&h).expect("editor in wide split");
    assert!(wide.width() < 600.0, "wide split: editor shares the area, got {wide:?}");
    assert!(preview_visible(&mut h) > 0, "wide split shows the preview heading");

    // Hide the sidebar and shrink: the central area drops below the threshold.
    st(&mut h).config.sidebar_visible = false;
    h.set_size(vec2(420.0, 500.0));
    settle(&mut h);
    let narrow = editor_rect(&h).expect("editor in narrow split");
    assert!(narrow.width() > 300.0, "narrow split: the editor gets the whole area, got {narrow:?}");
    assert_eq!(preview_visible(&mut h), 0, "narrow split must not show the preview");
    assert_eq!(st(&mut h).tabs.active_tab().map(|t| t.view), Some(iron_sheets_core::config::ViewMode::Split), "the view setting stays Split");
    shot(&mut h, "layout_split_narrow");

    // Wide again: the preview is back.
    h.set_size(vec2(1100.0, 600.0));
    settle(&mut h);
    assert!(preview_visible(&mut h) > 0, "preview returns when there is room");
    assert!(editor_rect(&h).is_some_and(|r| r.width() < 600.0));
}

// ---- IS-36: ultrawide windows and resizes ----

/// Labels of the controls (buttons, tabs, check boxes) of the tab strip, the view switch and the status bar.
fn control_labels(h: &H) -> std::collections::BTreeSet<String> {
    nodes(h)
        .into_iter()
        .filter(|(role, l, _)| matches!(role, Role::Button | Role::Tab | Role::CheckBox) && !l.is_empty() && !l.ends_with(".md") && !l.ends_with(".typ"))
        .map(|(_, l, _)| l)
        .collect()
}

#[test]
fn ultrawide_windows_lose_no_controls() {
    use iron_sheets_core::config::ViewMode;
    for mode in [ViewMode::Split, ViewMode::Preview] {
        let mut base = None;
        for (w, hgt) in [(1100.0, 720.0), (2560.0, 1080.0), (3440.0, 1440.0)] {
            let fx = Fx::new();
            let mut h = ready(&fx, vec2(w, hgt));
            open(&mut h, fx.notes().join("Welcome.md"));
            open(&mut h, fx.notes().join("Projects/Plan.md"));
            st(&mut h).commands.push(Command::SetViewMode(mode));
            settle(&mut h);
            assert!(st(&mut h).config.sidebar_visible, "tree open");
            assert!(h.get_all_by_label("Welcome.md").next().is_some(), "tree row and tab are findable");
            let bad = outside(&h);
            assert!(bad.is_empty(), "{w}x{hgt} {mode:?}: outside the window: {bad:#?}");
            let labels = control_labels(&h);
            let base = base.get_or_insert_with(|| labels.clone());
            let missing: Vec<_> = base.difference(&labels).collect();
            assert!(missing.is_empty(), "{w}x{hgt} {mode:?}: controls found at 1100 px are gone: {missing:?}");
            // Status bar segments sit inside the window, in the bottom strip, and are still found by label.
            let status: Vec<String> = status_nodes(&h).into_iter().map(|(_, l, _)| l).collect();
            assert!(status.iter().any(|l| l == "Saved") && status.iter().any(|l| l.contains("words")) && status.iter().any(|l| l.starts_with("Ln ")), "{w}x{hgt}: {status:?}");
            // The tab strip's controls are within the window and reachable (not zero-sized).
            for l in ["New tab", "Source", "Split", "Preview"] {
                let r = h.get_by_label(l).rect();
                assert!(win(&h).contains_rect(r) && r.width() > 8.0 && r.height() > 8.0, "{w}x{hgt}: {l} {r:?}");
            }
            shot(&mut h, &format!("layout_ultra_{w}x{hgt}_{mode:?}"));
        }
    }
}

fn long_note(fx: &Fx) -> PathBuf {
    let mut t = String::new();
    for i in 0..300 {
        t.push_str(&format!("## Section {i}\n\nParagraph {i}: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\n- item a\n- item b\n\n"));
    }
    let p = fx.notes().join("Long.md");
    std::fs::write(&p, t).expect("write");
    p
}

/// The preview offset the scroll map asks for at the editor's top line, and the preview's actual offset.
fn coupling(h: &mut H) -> (f32, f32, f32) {
    let t = st(h).tabs.active_tab().expect("tab");
    let (e, p) = (&t.editor_scroll, &t.preview_scroll);
    let line = e.map.line_for_y(e.offset);
    (p.follow_offset(iron_sheets_app_core::scrollsync::Follow::Line(line), p.max_offset()), p.offset, line)
}

#[test]
fn sync_scroll_stays_coupled_across_resizes() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1300.0, 800.0));
    open(&mut h, long_note(&fx));
    assert!(st(&mut h).config.sync_scroll);
    // Wheel the editor to about the middle.
    h.event(Event::PointerMoved(Pos2::new(560.0, 400.0)));
    h.step();
    for _ in 0..60 {
        let t = st(&mut h).tabs.active_tab().expect("tab");
        if t.editor_scroll.offset >= t.editor_scroll.max_offset() / 2.0 {
            break;
        }
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -600.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        h.step();
    }
    settle(&mut h);
    let (want, got, line) = coupling(&mut h);
    eprintln!("RESIZE start: editor line {line:.1}, preview offset {got:.0} (map says {want:.0})");
    assert!(line > 100.0 && got > 500.0, "scrolled to the middle: line {line}, preview {got}");
    // The map's own tolerance: half a viewport of drift would already be a visible desync.
    let line0 = line;
    let tol = 120.0;
    for (w, hgt) in [(800.0, 700.0), (1900.0, 900.0), (900.0, 600.0), (2560.0, 1080.0)] {
        h.set_size(vec2(w, hgt));
        settle(&mut h);
        for _ in 0..10 {
            h.step();
        }
        let t = st(&mut h).tabs.active_tab().expect("tab");
        let shown = t.view == iron_sheets_core::config::ViewMode::Split;
        assert!(shown);
        let (want, got, line) = coupling(&mut h);
        eprintln!("RESIZE {w}x{hgt}: editor line {line:.1}, preview offset {got:.0}, map says {want:.0}, diff {:.0}", (want - got).abs());
        assert!((line - line0).abs() < 40.0, "{w}: the editor keeps its place: line {line} vs {line0}");
        assert!(got > 500.0, "{w}: the preview did not jump back to the top ({got})");
        assert!((want - got).abs() <= tol, "{w}x{hgt}: preview {got} vs map {want} (line {line})");
    }
    shot(&mut h, "layout_sync_after_resizes");
}
