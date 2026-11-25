//! Regression: the sidebar must not grow by itself on the Search and Symbols views.
use crate::common::*;
use egui::{Event, vec2};
use egui_kittest::Harness;
use iron_sheets::state::AppState;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready_with(fx: &Fx, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> H {
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    f(&mut o.config);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    let mut h = harness(o);
    // The symbol grid shows symbols the bundled fonts lack; the app draws tofu for them (egui's default), kittest panics.
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    h.set_size(vec2(1100.0, 700.0));
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    h
}

/// Right edge of the sidebar: the left edge of the history arrows, the first thing in the strip next to it.
fn sidebar_edge(h: &H) -> f32 {
    use egui_kittest::kittest::Queryable;
    h.query_all_by_label_contains("Back").map(|n| n.rect().min.x).fold(f32::INFINITY, f32::min)
}

/// Switch the sidebar view by its label. When the pane is too narrow for the five tabs the views are in a menu behind a
/// button named after the current view: open it first.
fn pick_view(h: &mut H, view: &str) {
    use egui_kittest::kittest::Queryable;
    if h.query_by_label(view).is_none() {
        let cur = format!("{:?}", st(h).sidebar_view);
        h.get_by_label(&cur).click();
        settle(h);
        h.get_all_by_label(view).last().expect("menu item").click();
    } else {
        h.get_by_label(view).click();
    }
}

fn grows_while_moving(h: &mut H, view: &str) -> (f32, f32) {
    pick_view(h, view);
    // The symbol table and fonts load in the background: let them arrive first.
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(1500) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(h);
    let w0 = sidebar_edge(h);
    for i in 0..60 {
        h.event(Event::PointerMoved(egui::pos2(600.0 + (i % 7) as f32, 300.0 + (i % 5) as f32)));
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let w1 = sidebar_edge(h);
    (w0, w1)
}

#[test]
fn sidebar_does_not_grow_on_search_and_symbols() {
    // Bigger UI text and zoom make the controls wider than any fixed allowance.
    for (font, zoom) in [(13.0, 1.0), (18.0, 1.0), (13.0, 1.5), (24.0, 1.3)] {
        let fx = Fx::new();
        let mut h = ready_with(&fx, |c| {
            c.ui_font_size = font;
            c.ui_zoom = zoom;
        });
        for view in ["Search", "Symbols"] {
            let (w0, w1) = grows_while_moving(&mut h, view);
            assert!(w0.is_finite() && w0 > 50.0, "{view}: tab strip found ({w0})");
            assert!((w1 - w0).abs() < 1.0, "{view} at {font} pt, zoom {zoom}: sidebar grew from {w0} to {w1}");
        }
    }
}

fn switch(h: &mut H, view: &str) {
    pick_view(h, view);
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(600) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(h);
}

/// The pane never changes size on its own: switching views at any font size and zoom keeps its width, and the
/// stored width stays what it was.
#[test]
fn sidebar_keeps_its_width_when_switching_views() {
    for (font, zoom) in [(13.0, 1.0), (18.0, 1.0), (13.0, 1.5), (24.0, 1.3), (24.0, 1.5)] {
        let fx = Fx::new();
        let mut h = ready_with(&fx, |c| {
            c.ui_font_size = font;
            c.ui_zoom = zoom;
        });
        let (w0, stored) = (sidebar_edge(&h), st(&mut h).config.sidebar_width);
        for view in ["Symbols", "Files", "Search", "Symbols", "Files", "Outline", "Links", "Files"] {
            switch(&mut h, view);
            for i in 0..30 {
                h.event(Event::PointerMoved(egui::pos2(600.0 + (i % 7) as f32, 300.0)));
                h.step();
            }
            let w = sidebar_edge(&h);
            assert!((w - w0).abs() < 1.0, "-> {view} at {font} pt, zoom {zoom}: sidebar went from {w0} to {w}");
            assert_eq!(st(&mut h).config.sidebar_width, stored, "-> {view}: stored width changed");
        }
        if (font, zoom) == (24.0, 1.5) {
            for view in ["Files", "Search", "Symbols"] {
                switch(&mut h, view);
                shot(&mut h, &format!("sidebar-{}-24pt-zoom1.5", view.to_lowercase()));
            }
        }
    }
}

/// A real drag of the edge still resizes the pane, and the width is remembered.
#[test]
fn sidebar_edge_drag_resizes_and_is_remembered() {
    let fx = Fx::new();
    let mut h = ready_with(&fx, |_| {});
    let w0 = st(&mut h).config.sidebar_width;
    let edge = sidebar_edge(&h) - 8.0;
    let (from, to) = (egui::pos2(edge, 300.0), egui::pos2(edge + 120.0, 300.0));
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for k in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 6.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    settle(&mut h);
    let w1 = st(&mut h).config.sidebar_width;
    assert!((w1 - (w0 + 120.0)).abs() < 6.0, "dragged from {w0} to {w1} (edge {edge})");
    // Not undone by later frames or view switches.
    switch(&mut h, "Symbols");
    switch(&mut h, "Files");
    assert!((st(&mut h).config.sidebar_width - w1).abs() < 1.0);
}
