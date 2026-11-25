//! Zooming (UI zoom or editor font size) must never leave rows of the source editor overlapping, not even for a frame.
use crate::common::*;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

fn open(kb: usize, mode: ViewMode) -> (Fx, H) {
    open_dt(kb, mode, 0.25)
}

fn open_dt(kb: usize, mode: ViewMode, dt: f32) -> (Fx, H) {
    let fx = Fx::new();
    let mut text = String::new();
    let mut i = 0;
    while text.len() < kb * 1024 {
        text.push_str(&format!("## Heading {i}\n\nParagraph {i}: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\n- item {i}\n- [ ] task {i}\n\n"));
        i += 1;
    }
    let path = fx.notes().join("Big.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), dt);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(mode));
    settle(&mut h);
    (fx, h)
}

fn overlapping(h: &mut H) -> usize {
    let st = &mut h.state_mut().app().state;
    let id = st.tabs.active_tab().expect("tab").id;
    st.ui.editor.stats(id).expect("stats").overlapping
}

fn wheel(dy: f32) -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    }
}

fn zoom_steps(kb: usize, mode: ViewMode, font: bool) {
    let (_fx, mut h) = open(kb, mode);
    h.event(egui::Event::PointerMoved(egui::pos2(300.0, 400.0)));
    for _ in 0..6 {
        h.event(wheel(-2000.0));
        h.step();
    }
    for step in 0..12 {
        let up = (step / 3) % 2 == 0;
        let cfg = &mut h.state_mut().app().state.config;
        if font {
            cfg.editor_font_size = (cfg.editor_font_size + if up { 2.0 } else { -2.0 }).clamp(8.0, 40.0);
        } else {
            cfg.ui_zoom = (cfg.ui_zoom + if up { 0.1 } else { -0.1 }).clamp(0.5, 3.0);
        }
        for k in 0..6 {
            h.step();
            assert_eq!(overlapping(&mut h), 0, "{kb} kB font={font} step {step} frame {k}");
        }
    }
}

#[test]
fn ui_zoom_big() {
    zoom_steps(300, ViewMode::Source, false);
    zoom_steps(300, ViewMode::Split, false);
}

#[test]
fn font_size_big() {
    zoom_steps(300, ViewMode::Source, true);
    zoom_steps(300, ViewMode::Split, true);
}

/// The zoom eases over a few frames at 60 fps (never overlapping rows on the way) and is instant with the group off.
/// Prints what one animation step costs with a 100 KB note in split view.
#[test]
fn zoom_animates_and_group_off_is_instant() {
    zoom_run(2, true);
    zoom_run(20, true);
    // Big notes skip the easing (an 18 ms step with 100 KB in split view, 9 ms with 20 KB).
    zoom_run(100, false);
}

fn zoom_run(kb: usize, eased: bool) {
    let (_fx, mut h) = open_dt(kb, ViewMode::Split, 1.0 / 60.0);
    let t = std::time::Instant::now();
    for _ in 0..10 {
        h.step();
    }
    println!("{kb} kB idle step {:.2} ms", t.elapsed().as_secs_f64() * 100.0);
    h.event(egui::Event::PointerMoved(egui::pos2(300.0, 400.0)));
    for _ in 0..6 {
        h.event(wheel(-2000.0));
        h.step();
    }
    settle(&mut h);
    h.state_mut().app().state.config.ui_zoom = 1.3;
    let mut seen = Vec::new();
    let mut times = Vec::new();
    for k in 0..30 {
        let t = std::time::Instant::now();
        h.step();
        if eased && k == 3 {
            shot(&mut h, &format!("zoom_mid_{kb}kb"));
        }
        times.push(t.elapsed().as_secs_f64() * 1e3);
        seen.push(h.ctx.zoom_factor());
        assert_eq!(overlapping(&mut h), 0, "zoom {}", h.ctx.zoom_factor());
    }
    assert_eq!(seen.iter().any(|z| *z > 1.01 && *z < 1.29), eased, "intermediate zoom factors: {seen:?}");
    assert!((seen[29] - 1.3).abs() < 1e-4, "ends at the target");
    assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
    if eased {
        shot(&mut h, &format!("zoom_done_{kb}kb"));
    }
    let moving: Vec<f64> = times.iter().zip(&seen).filter(|(_, z)| **z < 1.2999).map(|(t, _)| *t).collect();
    println!("{kb} kB zoom animation: {} steps, step ms {:?}", moving.len(), moving.iter().map(|t| (t * 10.0).round() / 10.0).collect::<Vec<_>>());

    let cfg = &mut h.state_mut().app().state.config;
    cfg.anim_zoom = false;
    cfg.ui_zoom = 1.0;
    h.state_mut().app().state.settings_gen += 1;
    // egui applies a new zoom factor at the start of the next pass.
    for _ in 0..3 {
        h.step();
    }
    assert!((h.ctx.zoom_factor() - 1.0).abs() < 1e-4, "instant with the group off: {}", h.ctx.zoom_factor());
}

/// A preview that paints math must not ease the zoom (every intermediate scale would rasterise the formulas again).
#[cfg(feature = "typst")]
#[test]
fn zoom_jumps_when_the_preview_shows_math() {
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, "# Math\n\nTypst $a^2 + b^2 = c^2$ and $alpha + beta$.\n\n$$ integral_0^1 x dif x $$\n").expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 1.0 / 60.0);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Split));
    settle(&mut h);
    let t = std::time::Instant::now();
    while t.elapsed() < Duration::from_secs(40) && h.state_mut().app().state.ui.md_preview.textures_painted == 0 {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(h.state_mut().app().state.ui.md_preview.textures_painted > 0, "math was painted");
    h.state_mut().app().state.config.ui_zoom = 1.3;
    let seen: Vec<f32> = (0..12)
        .map(|_| {
            h.step();
            h.ctx.zoom_factor()
        })
        .collect();
    assert!(seen.iter().all(|z| *z < 1.01 || (z - 1.3).abs() < 1e-4), "jumps, no intermediate factors: {seen:?}");
    assert!((seen[11] - 1.3).abs() < 1e-4);
}
