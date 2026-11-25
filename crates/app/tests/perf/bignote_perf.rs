//! Big-note measurements (1 MB, 5 MB): open, keystroke, scroll and idle frames.
//! `cargo test -p iron-sheets --release --test perf -- bignote_perf:: --nocapture --test-threads 1`
use crate::common::*;
use egui::Event;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;
/// Durations of frame 1 and frame 2 after an open.
type Frames = (Duration, Duration);

fn unit() -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < 100 * 1024 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, with inline math $x^{i} + y$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n```rust\nfn main() {{ println!(\"{i}\"); }}\n```\n\n> quote {i} with a little more text in it\n\n"
        ));
        i += 1;
    }
    s
}

fn note_of(kb: usize) -> String {
    let base = unit();
    let mut s = String::new();
    while s.len() < kb * 1024 {
        s.push_str(&base);
    }
    s
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    format!("median {:.2} ms, p90 {:.2} ms, max {:.2} ms", ms(v[v.len() / 2]), ms(v[v.len() * 9 / 10]), ms(v[v.len() - 1]))
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

fn quiet(h: &mut H) {
    for _ in 0..30 {
        frame(h, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Resident set size in MB (0 where /proc is missing).
fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = s.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok());
    kb.unwrap_or(0.0) / 1024.0
}

fn wheel(dy: f32) -> Event {
    Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    }
}

fn run(kb: usize) {
    let fx = Fx::new();
    let text = note_of(kb);
    let p = fx.notes().join("Big.md");
    std::fs::write(&p, &text).expect("write");
    let small = fx.notes().join("Small.md");
    std::fs::write(&small, "# Small\n\nA short note.\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: small, new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Source));
    quiet(&mut h);
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    // Source view unless BIGNOTE_SPLIT is set (then the preview and the scroll sync take part).
    let mode = if std::env::var_os("BIGNOTE_SPLIT").is_some() { ViewMode::Split } else { ViewMode::Source };
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    let f1 = frame(&mut h, vec![]);
    let f2 = frame(&mut h, vec![]);
    let f3 = frame(&mut h, vec![]);
    eprintln!("BIGNOTE {kb} KB open: frame1 {:.2} ms, frame2 {:.2} ms, frame3 {:.2} ms, first two {:.2} ms", ms(f1), ms(f2), ms(f3), ms(f1 + f2));
    quiet(&mut h);
    let rss_open = rss_mb();
    let mut idle = Vec::new();
    for _ in 0..30 {
        idle.push(frame(&mut h, vec![]));
    }
    eprintln!("BIGNOTE {kb} KB idle frame: {}", stats(idle));
    // Scroll by wheel.
    let hover = Event::PointerMoved(egui::pos2(600.0, 400.0));
    frame(&mut h, vec![hover]);
    let mut sc = Vec::new();
    for _ in 0..40 {
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -200.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        };
        sc.push(frame(&mut h, vec![wheel]));
        frame(&mut h, vec![]);
    }
    eprintln!("BIGNOTE {kb} KB scroll frame: {}", stats(sc));
    // End to end (bounded: 20 MB is ~200k rows), then back to the top.
    let t = Instant::now();
    let steps = if std::env::var_os("BIGNOTE_QUICK").is_some() { 20 } else { (kb / 4).clamp(100, 1500) };
    for _ in 0..steps {
        frame(&mut h, vec![wheel(-60_000.0)]);
    }
    for _ in 0..30 {
        frame(&mut h, vec![]);
    }
    eprintln!("BIGNOTE {kb} KB scroll end to end: {} steps in {:.0} ms", steps, ms(t.elapsed()));
    quiet(&mut h);
    eprintln!("BIGNOTE {kb} KB RSS after open {rss_open:.0} MB, after scrolling to the end {:.0} MB", rss_mb());
    for _ in 0..steps {
        frame(&mut h, vec![wheel(60_000.0)]);
    }
    quiet(&mut h);
    // Typing in the middle.
    let mid = text.len() / 2;
    let mid = (0..=mid).rev().find(|&i| text.is_char_boundary(i) && text[..i].ends_with(' ')).unwrap_or(0);
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.pending_cursor = Some(mid..mid);
    }
    quiet(&mut h);
    let (mut keys, mut next) = (Vec::new(), Vec::new());
    for k in 0..40 {
        let c = if k % 6 == 5 { " " } else { "a" };
        keys.push(frame(&mut h, vec![Event::Text(c.to_string())]));
        next.push(frame(&mut h, vec![]));
        std::thread::sleep(Duration::from_millis(30));
    }
    eprintln!("BIGNOTE {kb} KB keystroke frame: {}", stats(keys));
    eprintln!("BIGNOTE {kb} KB following frame: {}", stats(next));
    eprintln!("BIGNOTE {kb} KB RSS after typing {:.0} MB", rss_mb());
    let st = &mut h.state_mut().app().state;
    if let Some(s) = st.tabs.active_tab().map(|t| t.id).and_then(|id| st.ui.editor.stats(id)) {
        eprintln!("BIGNOTE {kb} KB after typing: {} rows, {} placeholders, {} galleys patched", s.rows, s.placeholders, s.patched);
    }
}

#[test]
fn bignote_1m() {
    run(1024);
}

#[test]
fn bignote_5m() {
    run(5 * 1024);
}

/// Minutes in a dev build: `cargo test -p iron-sheets --release --test perf -- bignote_perf:: --ignored --nocapture bignote_20m`
/// (`BIGNOTE_QUICK=1` shortens the scroll to the end).
#[test]
#[ignore]
fn bignote_20m() {
    run(20 * 1024);
}

/// Time from the open command to the end of the first painted frame (frame 1 runs the command, frame 2 shows the
/// note), for notes that were read in the background first and for cold ones.
fn open_prefetch(kb: usize, mode: ViewMode) {
    let fx = Fx::new();
    let text = note_of(kb);
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    let mut f12c: Vec<Frames> = Vec::new();
    let mut f12w: Vec<Frames> = Vec::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    quiet(&mut h);
    for i in 0..6 {
        let order = if std::env::var_os("BIGNOTE_WARM_FIRST").is_some() { [true, false] } else { [false, true] };
        for warm_run in order {
            let p = fx.notes().join(format!("N{i}{}.md", warm_run as u8));
            std::fs::write(&p, format!("{text}\n{i}")).expect("write");
            if warm_run {
                let st = &mut h.state_mut().app().state;
                st.docs.prefetch.request(&p);
                st.docs.prefetch.wait_idle(Duration::from_secs(10));
                assert!(st.docs.prefetch.has(&p), "prefetched");
            }
            quiet(&mut h);
            h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
            h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
            let t = Instant::now();
            let a = frame(&mut h, vec![]);
            let b = frame(&mut h, vec![]);
            let d = t.elapsed();
            if warm_run { &mut f12w } else { &mut f12c }.push((a, b));
            if warm_run { &mut warm } else { &mut cold }.push(d);
            h.state_mut().app().state.commands.push(Command::CloseActiveTab);
            quiet(&mut h);
        }
    }
    let st = &mut h.state_mut().app().state;
    let (reads, hits) = st.docs.prefetch.counters();
    let med = |v: &[Frames]| {
        let mut a: Vec<f64> = v.iter().map(|x| ms(x.0)).collect();
        let mut b: Vec<f64> = v.iter().map(|x| ms(x.1)).collect();
        a.sort_by(|x, y| x.total_cmp(y));
        b.sort_by(|x, y| x.total_cmp(y));
        format!("frame1 {:.2} ms, frame2 {:.2} ms", a[a.len() / 2], b[b.len() / 2])
    };
    eprintln!("BIGNOTE open {kb} KB {mode:?} cold frames: {}; prefetched frames: {}", med(&f12c), med(&f12w));
    eprintln!("BIGNOTE open {kb} KB {mode:?} cold: {}", stats(cold));
    eprintln!("BIGNOTE open {kb} KB {mode:?} prefetched: {} (worker reads {reads}, taken {hits})", stats(warm));
}

#[test]
fn bignote_prefetch_open() {
    for mode in [ViewMode::Source, ViewMode::Split] {
        open_prefetch(100, mode);
        open_prefetch(1024, mode);
    }
}
