//! Editor frame timings on big notes. Run with
//! `CARGO_TARGET_DIR=... cargo test -p iron-sheets --release --test perf -- editor_perf:: --nocapture --test-threads=1`.
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

fn frame(h: &mut H, events: Vec<Event>) -> Duration {
    let ctx = h.ctx.clone();
    ctx.disable_accesskit();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 720.0))),
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

fn wheel() -> Event {
    Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
        source: egui::MouseWheelSource::Unknown,
    }
}

fn open(mb: usize, mode: ViewMode) -> (Fx, H, std::path::PathBuf, String) {
    let fx = Fx::new();
    let text = big_note(mb);
    let p = fx.notes().join("Big.md");
    std::fs::write(&p, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    for _ in 0..10 {
        frame(&mut h, vec![]);
    }
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    for _ in 0..60 {
        frame(&mut h, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    (fx, h, p, text)
}

fn measure(mb: usize, mode: ViewMode) {
    let (_fx, mut h, _p, text) = open(mb, mode);
    let tag = format!("[{mb} MB {mode:?}]");
    // Settle idle prefetch so steady frames are really steady.
    for _ in 0..200 {
        frame(&mut h, vec![]);
    }
    let steady: Vec<_> = (0..30).map(|_| frame(&mut h, vec![])).collect();
    frame(&mut h, vec![Event::PointerMoved(egui::pos2(500.0, 400.0))]);
    let wh: Vec<_> = (0..60).map(|_| frame(&mut h, vec![wheel()])).collect();
    let mut jumps = Vec::new();
    for frac in [0.5f32, 0.999] {
        let tab = h.state_mut().app().state.tabs.active_tab_mut().expect("tab");
        let total = tab.editor_scroll.content_h;
        tab.editor_scroll.force = Some(total * frac);
        let v: Vec<_> = (0..4)
            .map(|_| {
                let d = frame(&mut h, vec![]);
                std::thread::sleep(Duration::from_millis(5));
                d
            })
            .collect();
        jumps.push(format!("{frac}: {}", v.iter().map(|d| format!("{:.1}", ms(*d))).collect::<Vec<_>>().join("/")));
    }
    // Typing in the middle: put the caret there, then type.
    let mid = text.len() / 2;
    let mid = (0..=mid).rev().find(|&i| text.is_char_boundary(i) && text[..i].ends_with(' ')).unwrap_or(0);
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    for _ in 0..10 {
        frame(&mut h, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (mut typed, mut after) = (Vec::new(), Vec::new());
    for _ in 0..20 {
        typed.push(frame(&mut h, vec![Event::Text("a".into())]));
        after.push(frame(&mut h, vec![]));
        std::thread::sleep(Duration::from_millis(20));
    }
    let paste = "pasted line of text with some words in it\n".repeat(250);
    let mut pasted = Vec::new();
    let mut pafter = Vec::new();
    for _ in 0..5 {
        pasted.push(frame(&mut h, vec![Event::Paste(paste.clone())]));
        pafter.push(frame(&mut h, vec![]));
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("{tag} steady {}", med(steady));
    eprintln!("{tag} wheel {}", med(wh));
    eprintln!("{tag} jumps {}", jumps.join(" | "));
    eprintln!("{tag} key frame {}; next frame {}", med(typed), med(after));
    eprintln!("{tag} paste10k frame {}; next frame {}", med(pasted), med(pafter));
}

#[test]
fn editor_1mb_source() {
    measure(1, ViewMode::Source);
}
#[test]
fn editor_1mb_split() {
    measure(1, ViewMode::Split);
}
#[test]
fn editor_5mb_source() {
    measure(5, ViewMode::Source);
}
#[test]
fn editor_5mb_split() {
    measure(5, ViewMode::Split);
}

/// Long typing loop to attach a sampler to (`EDITOR_PERF_LOOP=secs`).
#[test]
fn typing_loop() {
    let Some(secs) = std::env::var("EDITOR_PERF_LOOP").ok().and_then(|s| s.parse::<u64>().ok()) else { return };
    let (_fx, mut h, _p, text) = open(5, ViewMode::Source);
    let mid = text.len() / 2;
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    let t = Instant::now();
    while t.elapsed().as_secs() < secs {
        frame(&mut h, vec![Event::Text("a".into())]);
        frame(&mut h, vec![]);
        std::thread::sleep(Duration::from_millis(20));
    }
}
