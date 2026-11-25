//! Wave A bug regressions (bugs3).
use crate::common::*;
use egui::{Event, vec2};
use egui_kittest::Harness;
use iron_sheets::state::AppState;

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready() -> (Fx, H) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    h.set_size(vec2(1100.0, 700.0));
    settle(&mut h);
    (fx, h)
}

fn sidebar_edge(h: &H) -> f32 {
    use egui_kittest::kittest::Queryable;
    h.query_all_by_label("New tab").map(|n| n.rect().min.x).fold(f32::INFINITY, f32::min)
}

/// Shrinking the window and growing it again with the pointer held down (a window-edge drag) must bring the pane back
/// to the stored width and leave the stored width alone.
#[test]
fn sidebar_grows_back_after_window_shrink() {
    let (_fx, mut h) = ready();
    let stored = st(&mut h).config.sidebar_width;
    let w0 = sidebar_edge(&h);
    let p = egui::pos2(900.0, 300.0);
    h.event(Event::PointerMoved(p));
    h.step();
    h.event(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    for w in [900.0, 700.0, 500.0, 420.0, 500.0, 700.0, 900.0, 1100.0] {
        h.set_size(vec2(w, 700.0));
        h.step();
        h.step();
    }
    h.event(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    settle(&mut h);
    assert_eq!(st(&mut h).config.sidebar_width, stored, "stored width changed");
    let w1 = sidebar_edge(&h);
    assert!((w1 - w0).abs() < 1.0, "pane did not grow back: {w0} -> {w1}");
}

fn pixel(img: &egui::ColorImage, p: egui::Pos2) -> egui::Color32 {
    img.pixels[(p.y as usize) * img.width() + p.x as usize]
}

/// Hovering a view-mode segment or the Sync chip changes its fill and moves nothing; the selected segment reacts too.
#[test]
fn toolbar_controls_show_hover_without_moving() {
    use egui_kittest::kittest::Queryable;
    for dark in [false, true] {
        let fx = Fx::new();
        let mut o = fx.opts(vec![fx.notes()]);
        o.config.theme = if dark { iron_sheets_core::config::ThemePref::Dark } else { iron_sheets_core::config::ThemePref::Light };
        let mut h = harness(o);
        h.set_size(vec2(1200.0, 700.0));
        st(&mut h).commands.push(iron_sheets::command::Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
        st(&mut h).commands.push(iron_sheets::command::Command::SetViewMode(iron_sheets_core::config::ViewMode::Split));
        settle(&mut h);
        let away = egui::pos2(600.0, 400.0);
        for label in ["Source", "Split", "Preview", "Sync"] {
            h.event(Event::PointerMoved(away));
            settle(&mut h);
            let rect = h.get_by_label(label).rect();
            let rest = h.render().expect("render");
            // Left padding of the button, clear of the text.
            let probe = egui::pos2(rect.min.x + 2.0, rect.center().y);
            let before = pixel(&egui::ColorImage::from_rgba_unmultiplied([rest.width() as usize, rest.height() as usize], rest.as_raw()), probe);
            h.event(Event::PointerMoved(rect.center()));
            settle(&mut h);
            assert_eq!(h.get_by_label(label).rect(), rect, "{label}: moved on hover");
            let hov = h.render().expect("render");
            let after = pixel(&egui::ColorImage::from_rgba_unmultiplied([hov.width() as usize, hov.height() as usize], hov.as_raw()), probe);
            assert_ne!(before, after, "{label} (dark={dark}): no visible hover feedback");
            shot(&mut h, &format!("bugs3-hover-{label}-{}", if dark { "dark" } else { "light" }));
        }
    }
}
