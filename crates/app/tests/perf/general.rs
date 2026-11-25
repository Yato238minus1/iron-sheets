//! Frame-time measurements for the README. Run with
//! `cargo test -p iron-sheets --release --test perf -- --nocapture`.
//! Assertions are loose so the test stays stable on a busy machine; the printed numbers matter.
use crate::common::*;
use egui::Event;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::Path;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn note_100k() -> String {
    let nomath = std::env::var("PERF_NOMATH").is_ok();
    let mut s = String::new();
    let mut i = 0;
    while s.len() < 100 * 1024 {
        let math = if nomath { "no math".to_string() } else { format!("with inline math $x^{i} + y$") };
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, {math} and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n> quote {i} with a little more text in it\n\n"
        ));
        i += 1;
    }
    s
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    format!("median {:.2} ms, p90 {:.2} ms, max {:.2} ms", ms(v[v.len() / 2]), ms(v[v.len() * 9 / 10]), ms(v[v.len() - 1]))
}

fn quiet(h: &mut H) {
    for _ in 0..40 {
        frame(h, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One timed frame run by hand (UI + tessellation). kittest keeps AccessKit enabled, which builds an
/// O(rows) accessibility tree for a big `TextEdit`; the real app has none unless a screen reader asks.
fn frame(h: &mut H, events: Vec<Event>) -> Duration {
    frame_w(h, events, 1100.0)
}

fn frame_w(h: &mut H, events: Vec<Event>, width: f32) -> Duration {
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

fn open(h: &mut H, p: &Path) -> Duration {
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.to_path_buf(), new_tab: true, line: None });
    // Frame 1 runs the command, frame 2 shows the note.
    let t = Instant::now();
    h.step();
    h.step();
    t.elapsed()
}

#[test]
fn perf_typing_100k_split_view() {
    run(ViewMode::Split);
}

#[test]
fn perf_typing_100k_source_only() {
    run(ViewMode::Source);
}

#[test]
fn perf_typing_100k_preview_only() {
    run(ViewMode::Preview);
}

/// About 100 000 words (~560 KB, above the default highlight limit, so the editor shows plain text).
#[test]
fn perf_typing_100k_words_split_view() {
    let base = note_100k();
    let mut text = String::new();
    while text.split_whitespace().count() < 100_000 {
        text.push_str(&base);
    }
    eprintln!("100k-words note: {} words, {} KB", text.split_whitespace().count(), text.len() / 1024);
    run_with(ViewMode::Split, text, "100k-words");
}

/// 100 KB made of Rust and Python fences, the caret in a fence body: typing re-highlights that fence only.
/// A/B with `IRON_MODULES_OFF=1` (code highlighting off).
#[test]
fn perf_typing_100k_fences_source_only() {
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 100 * 1024 {
        text.push_str(&format!("Paragraph {i}.\n\n```rust\nfn f{i}(x: i32) -> i32 {{ let s = \"a b\"; x + 1 }} // note {i}\n```\n\n```python\ndef g{i}(x):\n    return x + 1  # note {i}\n```\n\n"));
        i += 1;
    }
    run_with(ViewMode::Source, text, "100 KB fences");
}

/// The usual 100 KB note with 20 Rust fences spread through it.
#[test]
fn perf_typing_100k_20_fences_source_only() {
    let mut s = note_100k();
    for i in 0..20 {
        let at = s.len() / 21 * (i + 1);
        let at = s[at..].find("\n\n").map_or(s.len(), |k| at + k + 2);
        s.insert_str(at, "```rust\nfn main() {\n    let s = \"hi\"; // c\n    println!(\"{}\", 42);\n}\n```\n\n");
    }
    run_with(ViewMode::Source, s, "100 KB 20 fences");
}

fn run(mode: ViewMode) {
    run_with(mode, note_100k(), "100 KB");
}

fn run_with(mode: ViewMode, text: String, label: &str) {
    let fx = Fx::new();
    let p = fx.notes().join("Big100k.md");
    std::fs::write(&p, &text).expect("write");
    let small = fx.notes().join("Small.md");
    std::fs::write(&small, "# Small\n\nA short note with [[Ideas]].\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let t_small = open(&mut h, &small);
    quiet(&mut h);
    let t_big = open(&mut h, &p);
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    eprintln!("open note (2 frames): small {:.2} ms, {label} {:.2} ms", t_small.as_secs_f64() * 1e3, t_big.as_secs_f64() * 1e3);
    quiet(&mut h);

    // Put the cursor in the middle of the note and type.
    let mid = text.len() / 2;
    let mid = (0..=mid).rev().find(|&i| text.is_char_boundary(i) && text[..i].ends_with(' ')).unwrap_or(0);
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.pending_cursor = Some(mid..mid);
    }
    quiet(&mut h);
    let v0 = h.state_mut().app().state.docs.get(&p).map(|d| d.version).unwrap_or(0);
    let mut key_frames = Vec::new();
    let mut next_frames = Vec::new();
    for k in 0..40 {
        let c = if k % 6 == 5 { " " } else { "a" };
        if mode == ViewMode::Preview {
            let at = mid + k;
            h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p.clone(), range: at..at, insert: c.to_string(), select: None });
            key_frames.push(frame(&mut h, vec![]));
        } else {
            key_frames.push(frame(&mut h, vec![Event::Text(c.to_string())]));
        }
        next_frames.push(frame(&mut h, vec![]));
        std::thread::sleep(Duration::from_millis(30));
    }
    let v1 = h.state_mut().app().state.docs.get(&p).map(|d| d.version).unwrap_or(0);
    assert!(v1 > v0, "typing reached the document");
    eprintln!("typing in {label} note, {mode:?}: keystroke frame {}", stats(key_frames.clone()));
    eprintln!("typing in {label} note, {mode:?}: following frame {}", stats(next_frames));
    let mut idle = Vec::new();
    quiet(&mut h);
    for _ in 0..40 {
        idle.push(frame(&mut h, vec![]));
    }
    eprintln!("idle frame, {label} note, {mode:?}: {}", stats(idle));
    key_frames.sort();
    assert!(key_frames[key_frames.len() / 2] < Duration::from_millis(500), "typing far too slow");
}

fn note_of(kb: usize) -> String {
    let base = note_100k();
    let mut s = String::new();
    while s.len() < kb * 1024 {
        s.push_str(&base);
    }
    s
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Opens a note with a given view mode and reports frame 1 (runs the command), frame 2 (first paint of the note),
/// frames 3 and 4, and for split view the cost of a window-resize frame (width -40 px) and of the next frame.
fn open_breakdown(mode: ViewMode, kb: usize, resize: bool) {
    let fx = Fx::new();
    let text = if kb == 100 { note_100k() } else { note_of(kb) };
    let p = fx.notes().join("Big.md");
    std::fs::write(&p, &text).expect("write");
    let small = fx.notes().join("Small.md");
    std::fs::write(&small, "# Small\n\nA short note with [[Ideas]].\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    open(&mut h, &small);
    h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
    quiet(&mut h);
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    let f1 = frame(&mut h, vec![]);
    let f2 = frame(&mut h, vec![]);
    let f3 = frame(&mut h, vec![]);
    let f4 = frame(&mut h, vec![]);
    eprintln!(
        "open {kb} KB {mode:?}: frame1 {:.2} ms, frame2 {:.2} ms, frame3 {:.2} ms, frame4 {:.2} ms, first two {:.2} ms",
        ms(f1),
        ms(f2),
        ms(f3),
        ms(f4),
        ms(f1 + f2)
    );
    if resize {
        quiet(&mut h);
        let r1 = frame_w(&mut h, vec![], 1060.0);
        let r2 = frame_w(&mut h, vec![], 1060.0);
        let r3 = frame_w(&mut h, vec![], 1060.0);
        eprintln!("resize (-40 px) with {kb} KB open, {mode:?}: resize frame {:.2} ms, next {:.2} ms, then {:.2} ms", ms(r1), ms(r2), ms(r3));
    }
}

#[test]
fn perf_open_breakdown_100k() {
    open_breakdown(ViewMode::Source, 100, false);
    open_breakdown(ViewMode::Split, 100, true);
    open_breakdown(ViewMode::Preview, 100, false);
}

#[test]
fn perf_open_breakdown_1m() {
    open_breakdown(ViewMode::Source, 1024, true);
    open_breakdown(ViewMode::Split, 1024, false);
}
