//! Settings as an auxiliary window. The kittest harness has no native viewports (`embed_viewports()` is true), so
//! these tests cover the embedded fallback: open/close/reopen, Esc, the close control, staying inside the app window,
//! persisted open state and geometry, and startup with the window open. The native window is checked by hand (see
//! the report).
use crate::common::*;
use egui::{Key, Modifiers, Pos2, Rect, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use iron_sheets_core::config::{AppConfig, AuxWindowGeom};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn settings_window(h: &H) -> Option<Rect> {
    let ppp = h.ctx.pixels_per_point();
    h.root().children_recursive().find_map(|n| {
        let ak = n.accesskit_node();
        if ak.role() != Role::Window || ak.label().as_deref() != Some("Settings") {
            return None;
        }
        let b = ak.bounding_box()?;
        Some(Rect::from_min_max(Pos2::new(b.x0 as f32 / ppp, b.y0 as f32 / ppp), Pos2::new(b.x1 as f32 / ppp, b.y1 as f32 / ppp)))
    })
}

#[test]
fn embedded_mode_is_what_the_harness_gets() {
    let fx = Fx::new();
    let h = harness(fx.opts(vec![fx.notes()]));
    assert!(h.ctx.embed_viewports());
    assert!(!iron_sheets::aux_window::is_native(&h.ctx));
}

#[test]
fn opens_closes_with_esc_and_close_control_and_reopens() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    assert!(settings_window(&h).is_none());
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    settle(&mut h);
    assert!(st(&mut h).settings.open);
    assert!(settings_window(&h).is_some());
    // Esc closes it.
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!st(&mut h).settings.open, "Esc closes");
    assert!(settings_window(&h).is_none());
    // The app keeps working: a shortcut still runs.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    assert!(settings_window(&h).is_some(), "reopens");
    // The window's close control.
    h.get_by_label("Close window").click();
    settle(&mut h);
    assert!(!st(&mut h).settings.open, "close control closes");
    assert!(settings_window(&h).is_none());
    // Opening twice does not duplicate and asks for focus.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    st(&mut h).commands.push(Command::OpenSettings);
    h.step();
    assert!(st(&mut h).settings.open);
    settle(&mut h);
    assert_eq!(h.get_all_by_label("Search settings").count(), 1);
}

#[test]
fn recording_esc_cancels_recording_and_keeps_the_window() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    {
        let s = st(&mut h);
        s.settings.open = true;
        s.settings.section = iron_sheets::settings::Section::Keyboard;
        s.settings.recording = s.keymap.index_of("view.sync");
    }
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).settings.recording.is_none());
    assert!(st(&mut h).settings.open, "Esc only cancelled the recording");
}

#[test]
fn stays_inside_the_app_window_after_resize() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).settings.open = true;
    settle(&mut h);
    for size in [egui::vec2(1100.0, 720.0), egui::vec2(600.0, 400.0), egui::vec2(420.0, 300.0)] {
        h.set_size(size);
        settle(&mut h);
        let r = settings_window(&h).expect("settings window");
        assert!(h.ctx.content_rect().expand(1.0).contains_rect(r), "{size:?}: {r:?}");
    }
}

#[test]
fn open_state_is_written_to_the_config_and_read_back() {
    let fx = Fx::new();
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        assert!(st(&mut h).config.settings_window.is_none());
        st(&mut h).settings.open = true;
        settle(&mut h);
        assert!(st(&mut h).config.settings_window.is_some_and(|g| g.open));
        st(&mut h).save_config_now();
        let on_disk = AppConfig::load(&fx.dirs());
        assert!(on_disk.settings_window.is_some_and(|g| g.open));
        // Quitting with the window open keeps the flag.
        h.state_mut().app().on_quit();
        assert!(AppConfig::load(&fx.dirs()).settings_window.is_some_and(|g| g.open));
    }
    // Restart: the window is open again, then closed by the user and stays closed.
    let mut o = fx.opts(vec![fx.notes()]);
    o.config = AppConfig::load(&fx.dirs());
    let mut h = harness(o);
    assert!(st(&mut h).settings.open, "reopened from the config");
    assert!(settings_window(&h).is_some());
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).config.settings_window.is_some_and(|g| !g.open));
}

#[test]
fn stored_geometry_survives_reset_all() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let g = AuxWindowGeom { width: 800.0, height: 500.0, x: Some(40.0), y: Some(60.0), open: false };
    st(&mut h).config.settings_window = Some(g);
    iron_sheets::settings::reset_all(st(&mut h));
    assert_eq!(st(&mut h).config.settings_window, Some(g));
}
