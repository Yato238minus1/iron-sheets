//! Zoom, middle ellipsis in the tab strip, preview font, animations, reduce motion and the dropdown chevron.
use crate::common::*;
use egui::{Event, Key, Modifiers, MouseWheelUnit, Role, vec2};
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

fn wheel_with_ctrl(h: &mut H, notches: f32) {
    h.event(Event::ModifiersChanged(Modifiers::COMMAND));
    h.event(Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, notches),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::COMMAND,
    });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::default()));
    settle(h);
}

/// The compact "Mode" button of the tab strip (the menu bar has a "View" menu too, above it).
fn tab_view_button(h: &H) -> egui_kittest::Node<'_> {
    h.query_all_by_label("Mode").find(|n| n.rect().top() > 30.0).expect("compact Mode button")
}

fn long_note(fx: &Fx) -> PathBuf {
    let mut text = String::new();
    for i in 0..400 {
        text.push_str(&format!("Line {i} of a long note with a little text to fill the row.\n\n"));
    }
    let p = fx.notes().join("Long.md");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn ctrl_wheel_changes_both_fonts_in_whole_points_and_clamps() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    let (e0, p0) = (st(&mut h).config.editor_font_size, st(&mut h).config.preview_font_size);
    wheel_with_ctrl(&mut h, 1.0);
    assert_eq!(st(&mut h).config.editor_font_size, e0 + 1.0);
    assert_eq!(st(&mut h).config.preview_font_size, p0 + 1.0);
    assert_eq!(st(&mut h).config.ui_zoom, 1.0, "the wheel does not touch the UI zoom");
    wheel_with_ctrl(&mut h, -2.0);
    assert_eq!(st(&mut h).config.editor_font_size, e0 - 1.0);
    // Clamped at the range of the settings.
    st(&mut h).config.editor_font_size = 6.0;
    wheel_with_ctrl(&mut h, -1.0);
    assert_eq!(st(&mut h).config.editor_font_size, 6.0);
    let msg = st(&mut h).status.current().map(|m| m.text.clone()).unwrap_or_default();
    assert!(msg.starts_with("Font "), "{msg}");
}

#[test]
fn ctrl_wheel_keeps_the_top_line_of_the_editor() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    let p = long_note(&fx);
    open(&mut h, p);
    let tab = st(&mut h).tabs.active.unwrap();
    st(&mut h).tabs.tabs[tab].view = iron_sheets_core::config::ViewMode::Source;
    settle(&mut h);
    st(&mut h).tabs.tabs[tab].editor_scroll.force = Some(3000.0);
    settle(&mut h);
    let line = |h: &mut H| {
        let s = &st(h).tabs.tabs[tab].editor_scroll;
        (s.offset, s.map.line_for_y(s.offset))
    };
    let (off0, l0) = line(&mut h);
    assert!(off0 > 1000.0, "scrolled: {off0}");
    for _ in 0..3 {
        wheel_with_ctrl(&mut h, 1.0);
    }
    let (off1, l1) = line(&mut h);
    assert!(off1 > off0, "taller lines move the same line further down: {off0} -> {off1}");
    assert!((l1 - l0).abs() < 2.5, "top line stays: {l0} -> {l1}");
}

#[test]
fn ctrl_equals_and_plus_zoom_the_ui_and_the_message_names_it() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    h.key_press_modifiers(Modifiers::COMMAND, Key::Equals);
    settle(&mut h);
    assert!((st(&mut h).config.ui_zoom - 1.1).abs() < 1e-4);
    let msg = st(&mut h).status.current().map(|m| m.text.clone()).unwrap_or_default();
    assert_eq!(msg, "Zoom 110 %");
}

#[test]
fn long_tab_names_are_elided_in_the_middle_and_keep_the_extension() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    let long = fx.notes().join("a-very-long-note-name-that-would-take-half-the-tab-strip-by-itself-2024.md");
    std::fs::write(&long, "# x\n").unwrap();
    open(&mut h, long);
    open(&mut h, fx.notes().join("Welcome.md"));
    let tabs: Vec<_> = h.query_all_by_role(Role::Tab).map(|n| (n.accesskit_node().label().unwrap_or_default().to_string(), n.rect())).collect();
    let (name, r) = tabs.iter().find(|(n, _)| n.starts_with("a-very-long")).expect("long tab");
    assert!(name.ends_with("2024.md"), "accessible name keeps the full title: {name}");
    let font = 13.0;
    assert!(r.width() <= font * 13.0 + 36.0 + 1.0, "tab width {}", r.width());
    let short = tabs.iter().find(|(n, _)| n == "Welcome.md").expect("short tab");
    assert!(short.1.width() < r.width());
    let p = shot(&mut h, "uxpolish_long_tab_names");
    assert!(p.exists());
}

// the default preview font is Libertinus; without Typst it is already Ubuntu Light
#[cfg(feature = "typst")]
#[test]
fn preview_font_is_applied_from_the_view_menu_and_changes_the_preview() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    let tab = st(&mut h).tabs.active.unwrap();
    st(&mut h).tabs.tabs[tab].view = iron_sheets_core::config::ViewMode::Preview;
    settle(&mut h);
    let a = shot(&mut h, "uxpolish_preview_font_default");
    let before = std::fs::read(&a).unwrap();
    st(&mut h).commands.push(Command::SetPreviewFont("sans".into()));
    settle(&mut h);
    assert_eq!(st(&mut h).config.preview_font, "sans");
    let b = shot(&mut h, "uxpolish_preview_font_sans");
    assert_ne!(before, std::fs::read(&b).unwrap(), "the preview is drawn with another font");
    // The View menu offers the choice.
    h.get_by_label("View").click();
    settle(&mut h);
    h.get_by_label("Preview font").click();
    settle(&mut h);
    h.get_by_label("New Computer Modern").click();
    settle(&mut h);
    assert_eq!(st(&mut h).config.preview_font, "newcm");
    let _ = shot(&mut h, "uxpolish_preview_font_newcm");
}

#[test]
fn view_toolbar_collapse_and_chevron_animate_then_go_quiet() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    open(&mut h, fx.notes().join("Welcome.md"));
    h.get_by_label("Split");
    // Narrow window: the toolbar collapses into one "Mode" button over a few frames.
    h.set_size(vec2(620.0, 420.0));
    h.step();
    let mut animating = 0;
    for _ in 0..12 {
        h.step();
        if wants_immediate_repaint(&h) {
            animating += 1;
        }
    }
    eprintln!("UXPOLISH glide frames with an immediate repaint: {animating}");
    assert!(animating <= 6, "the glide takes a few frames, not more: {animating}");
    for _ in 0..3 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "no repaint left after the animation settled");
    }
    tab_view_button(&h);
    assert!(h.query_by_label("Split").is_none(), "compact form");
    let _ = shot(&mut h, "uxpolish_narrow_view_closed");
    // Opening the menu turns the chevron; the expanded state is reported.
    tab_view_button(&h).click();
    for _ in 0..8 {
        h.step();
    }
    for _ in 0..3 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "idle again with the menu open");
    }
    let node = tab_view_button(&h);
    assert_eq!(node.accesskit_node().data().is_expanded(), Some(true));
    h.get_by_label("Split");
    let _ = shot(&mut h, "uxpolish_narrow_view_open");
}

#[test]
fn reduce_motion_makes_every_transition_instant() {
    let fx = Fx::new();
    let mut h = ready(&fx, vec2(1100.0, 720.0));
    assert!(h.ctx.global_style().animation_time > 0.0);
    st(&mut h).config.animations = false;
    iron_sheets::settings_model::apply(st(&mut h), false);
    settle(&mut h);
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
    open(&mut h, fx.notes().join("Welcome.md"));
    h.set_size(vec2(620.0, 420.0));
    h.step();
    let mut repaints = 0;
    for _ in 0..12 {
        h.step();
        repaints += wants_immediate_repaint(&h) as u32;
    }
    eprintln!("UXPOLISH reduced-motion frames with an immediate repaint: {repaints}");
    for _ in 0..3 {
        h.step();
        assert!(!wants_immediate_repaint(&h), "quiet after the resize");
    }
    tab_view_button(&h);
}

/// Screenshot of the file tree with long names in a narrow sidebar. The tree shows the middle ellipsis only with the
/// `sidebar/files.rs` hand-merge applied (see the report); without it the names are clipped at the edge.
#[test]
fn file_tree_with_long_names_screenshot() {
    let fx = Fx::new();
    std::fs::write(fx.notes().join("an-extremely-long-file-name-in-the-tree-for-testing-ellipsis-2024.md"), "# x\n").unwrap();
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    o.config.sidebar_width = 200.0;
    let mut h = harness(o);
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    let _ = shot(&mut h, "uxpolish_tree_long_names");
}
