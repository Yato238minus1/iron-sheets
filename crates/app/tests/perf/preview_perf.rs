//! Preview render timings on big notes. Run with
//! `cargo test -p iron-sheets --release --test perf -- preview_perf:: --nocapture --test-threads=1`.
use crate::common::*;
use egui::Event;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn big_note(mb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < mb << 20 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum **dolor** sit amet, *consectetur* adipiscing [[Ideas]] and `code` with $x^{i}$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n```rust\nfn main() {{ println!(\"{i}\"); }}\n```\n\n> quote {i} with a little more text\n\n| a | b |\n|---|---|\n| {i} | 2 |\n\n"
        ));
        i += 1;
    }
    s.push_str("THE END\n");
    s
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn frame(h: &mut H, events: Vec<Event>, width: f32) -> Duration {
    let ctx = h.ctx.clone();
    ctx.disable_accesskit();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 720.0))),
        events,
        predicted_dt: 1.0 / 60.0,
        ..Default::default()
    };
    let slot = h.state_mut();
    let t = Instant::now();
    let mut out = ctx.run_ui(input, |ui| slot.app().frame(ui));
    let _ = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
    let d = t.elapsed();
    out.textures_delta.clear();
    ctx.enable_accesskit();
    d
}

fn med(mut v: Vec<Duration>) -> String {
    v.sort();
    format!("med {:.2} p90 {:.2} max {:.2}", ms(v[v.len() / 2]), ms(v[v.len() * 9 / 10]), ms(v[v.len() - 1]))
}

fn settle_laid(h: &mut H) -> (usize, Duration) {
    let t = Instant::now();
    let mut n = 0;
    for _ in 0..3000 {
        frame(h, vec![], 1100.0);
        n += 1;
        let a = &mut h.state_mut().app().state;
        if a.ui.md_preview.unlaid == 0 && a.ui.md_preview.parse_count > 0 {
            break;
        }
    }
    (n, t.elapsed())
}

fn measure(mb: usize, mode: ViewMode) {
    let fx = Fx::new();
    let text = big_note(mb);
    let p = fx.notes().join("Big.md");
    std::fs::write(&p, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    for _ in 0..10 {
        frame(&mut h, vec![], 1100.0);
    }
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    let t = Instant::now();
    let mut first = Duration::ZERO;
    for k in 0..400 {
        let d = frame(&mut h, vec![], 1100.0);
        if k < 3 {
            first += d;
        }
        if k >= 3 && h.state_mut().app().state.ui.md_preview.frames_shown > 0 && h.state_mut().app().state.ui.md_preview.parse_count > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let first_view = t.elapsed();
    let (n, laid) = settle_laid(&mut h);
    let mut steady = Vec::new();
    for _ in 0..30 {
        steady.push(frame(&mut h, vec![], 1100.0));
    }
    frame(&mut h, vec![Event::PointerMoved(egui::pos2(900.0, 400.0))], 1100.0);
    let mut wheel = Vec::new();
    for _ in 0..60 {
        wheel.push(frame(
            &mut h,
            vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -120.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
                source: egui::MouseWheelSource::Unknown,
            }],
            1100.0,
        ));
    }
    // Jump to middle / end via the scroll contract.
    let mut jumps = Vec::new();
    for frac in [0.5f32, 0.999] {
        let tab = h.state_mut().app().state.tabs.active_tab_mut().expect("tab");
        let total = tab.preview_scroll.content_h;
        tab.preview_scroll.force = Some(total * frac);
        let mut v = Vec::new();
        for _ in 0..4 {
            v.push(frame(&mut h, vec![], 1100.0));
            std::thread::sleep(Duration::from_millis(5));
        }
        jumps.push(format!("{frac}: {}", v.iter().map(|d| format!("{:.1}", ms(*d))).collect::<Vec<_>>().join("/")));
    }
    // Edit in the middle.
    let mid = text.len() / 2;
    let mid = (0..=mid).rev().find(|&i| text.is_char_boundary(i) && text[..i].ends_with(' ')).unwrap_or(0);
    let mut edit = Vec::new();
    let mut after = Vec::new();
    for k in 0..10 {
        h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p.clone(), range: mid + k..mid + k, insert: "a".into(), select: None });
        edit.push(frame(&mut h, vec![], 1100.0));
        std::thread::sleep(Duration::from_millis(250));
        after.push(frame(&mut h, vec![], 1100.0));
        after.push(frame(&mut h, vec![], 1100.0));
    }
    let md = &h.state_mut().app().state.ui.md_preview;
    eprintln!(
        "[{mb} MB {mode:?}] first3frames {:.1} ms, to-first-parse {:.1} ms, full-lay {} frames/{:.0} ms (unlaid left {})",
        ms(first),
        ms(first_view),
        n,
        ms(laid),
        md.unlaid
    );
    eprintln!("[{mb} MB {mode:?}] steady {}", med(steady));
    eprintln!("[{mb} MB {mode:?}] wheel {}", med(wheel));
    eprintln!("[{mb} MB {mode:?}] jumps {}", jumps.join(" | "));
    eprintln!("[{mb} MB {mode:?}] edit frame {}; after-edit frames {}", med(edit), med(after));
}

#[test]
fn preview_1mb_preview() {
    measure(1, ViewMode::Preview);
}
#[test]
fn preview_1mb_split() {
    measure(1, ViewMode::Split);
}
#[test]
fn preview_5mb_preview() {
    measure(5, ViewMode::Preview);
}
#[test]
fn preview_5mb_split() {
    measure(5, ViewMode::Split);
}

#[cfg(feature = "typst")]
fn para_note() -> String {
    let mut s = String::from("# Full width\n\n");
    for i in 0..6 {
        s.push_str(&format!("Paragraph {i}: Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat, duis aute irure dolor in reprehenderit.\n\n"));
    }
    s.push_str("| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode block\n```\n");
    s
}

fn quiet(h: &mut H) -> bool {
    for _ in 0..80 {
        h.step();
        if !h.output().viewport_output.values().any(|v| v.repaint_delay == Duration::ZERO) {
            return true;
        }
    }
    false
}

/// Horizontal extent of the ink in the note area (below the tab strip), measured on the screenshot.
#[cfg(feature = "typst")]
fn ink_span(img: &image::RgbaImage, x0: u32) -> (u32, u32) {
    let (w, hgt) = img.dimensions();
    let bg = *img.get_pixel(w / 2, 600);
    let (mut lo, mut hi) = (w, 0);
    for y in (140..520.min(hgt - 40)).step_by(2) {
        for x in x0.max(24)..w - 24 {
            if img.get_pixel(x, y) != &bg {
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
    }
    (lo, hi)
}

// The ink-span measure is tuned to the Libertinus preview font (without Typst the chrome dominates the span).
#[cfg(feature = "typst")]
#[test]
fn preview_full_width_option() {
    let fx = Fx::new();
    let p = fx.notes().join("Wide.md");
    std::fs::write(&p, para_note()).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.set_size(egui::vec2(1600.0, 720.0));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    h.state_mut().app().state.config.sidebar_visible = false;
    assert!(quiet(&mut h), "quiet after open");
    let narrow = shot(&mut h, "preview_capped_1600");
    let a = ink_span(&image::open(&narrow).expect("png").to_rgba8(), 0);
    h.state_mut().app().state.config.preview_full_width = true;
    let laid0 = h.state_mut().app().state.ui.md_preview.laid_total;
    assert!(quiet(&mut h), "quiet after the option changed");
    let laid1 = h.state_mut().app().state.ui.md_preview.laid_total;
    let wide = shot(&mut h, "preview_full_width_1600");
    let b = ink_span(&image::open(&wide).expect("png").to_rgba8(), 0);
    eprintln!("ink span capped {a:?}, full width {b:?}; blocks laid by the switch: {}", laid1 - laid0);
    assert!(b.1 - b.0 > a.1 - a.0 + 300, "full width must be much wider: {a:?} vs {b:?}");
    assert!(laid1 - laid0 < 20, "switching re-lays out once, not per frame");
    // Settled: no frame lays anything out and nothing asks for an immediate repaint.
    let l = h.state_mut().app().state.ui.md_preview.laid_total;
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(h.state_mut().app().state.ui.md_preview.laid_total, l);
    assert!(quiet(&mut h));
}

/// A big note's idle layout must end (no immediate repaints afterwards) and an edit must leave the preview quiet.
#[test]
fn preview_goes_quiet_after_big_note_and_edit() {
    let fx = Fx::new();
    let text = big_note(1);
    let p = fx.notes().join("Big.md");
    std::fs::write(&p, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(60) {
        h.step();
        let m = &h.state_mut().app().state.ui.md_preview;
        if m.parse_count > 0 && m.unlaid == 0 {
            break;
        }
    }
    assert!(quiet(&mut h), "quiet after the idle layout");
    let mid = text.len() / 2;
    let mid = (0..=mid).rev().find(|&i| text.is_char_boundary(i) && text[..i].ends_with(' ')).unwrap_or(0);
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p, range: mid..mid, insert: "zz".into(), select: None });
    let before = h.state_mut().app().state.ui.md_preview.parse_count;
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) && h.state_mut().app().state.ui.md_preview.parse_count == before {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(quiet(&mut h), "quiet after the edit");
    let m = &h.state_mut().app().state.ui.md_preview;
    assert_eq!(m.unlaid, 0, "edit left nothing unlaid except what the idle pass finished");
}
