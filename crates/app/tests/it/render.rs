//! Rendering regressions: theme switches must not leave stale text in the editor or the preview, and the
//! synchronized scroll mapping must be smooth.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::path::PathBuf;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, theme: ThemePref, zoom: f32) -> H {
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = theme;
    o.config.ui_zoom = zoom;
    let mut h = harness(o);
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    h
}

fn open_split(h: &mut H, p: PathBuf) {
    open_split_wait(h, p, 20)
}

fn open_split_wait(h: &mut H, p: PathBuf, wait: usize) {
    st(h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
    st(h).commands.push(Command::SetViewMode(ViewMode::Split));
    settle(h);
    // Let background work (math, images) land.
    for _ in 0..wait {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

const NOTE: &str = "# Theme check\n\nSome **bold** and *italic* text with `code` and a [[Ideas]] link.\n\n- item one\n- [ ] task\n\n> a quote\n\n```rust\nfn main() {}\n```\n\nInline math $a^2 + b^2 = c^2$ here.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";

/// Fraction of pixels (inside the central area, below the tab strip) that differ by more than a small amount.
fn diff_fraction(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions());
    let (w, hgt) = a.dimensions();
    let mut n = 0u64;
    let mut total = 0u64;
    // Editor and preview area only (the file tree highlights the open note in one run and not in the other).
    for y in 76..hgt.min(684) {
        for x in 270..w {
            total += 1;
            let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
            if p.iter().zip(q.iter()).any(|(u, v)| (*u as i32 - *v as i32).abs() > 24) {
                n += 1;
            }
        }
    }
    n as f64 / total as f64
}

fn note_path(fx: &Fx) -> PathBuf {
    let p = fx.notes().join("ThemeCheck.md");
    std::fs::write(&p, NOTE).expect("write");
    p
}

fn render(h: &mut H, name: &str) -> image::RgbaImage {
    for _ in 0..3 {
        h.step();
    }
    shot(h, name);
    h.render().expect("render")
}

/// A theme switch must give the same picture as starting in that theme.
#[test]
fn theme_switch_matches_fresh_start() {
    for (from, to, tag) in [(ThemePref::Dark, ThemePref::Light, "dark-to-light"), (ThemePref::Light, ThemePref::Dark, "light-to-dark")] {
        let fx = Fx::new();
        let p = note_path(&fx);
        // Reference: start directly in the target theme.
        let mut fresh = ready(&fx, to, 1.0);
        open_split(&mut fresh, p.clone());
        let reference = render(&mut fresh, &format!("render-theme-{tag}-fresh"));
        drop(fresh);

        let fx2 = Fx::new();
        let p2 = note_path(&fx2);
        let mut h = ready(&fx2, from, 1.0);
        open_split(&mut h, p2);
        render(&mut h, &format!("render-theme-{tag}-before"));
        st(&mut h).config.theme = to;
        for _ in 0..20 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        let switched = render(&mut h, &format!("render-theme-{tag}-switched"));
        let d = diff_fraction(&reference, &switched);
        eprintln!("{tag}: differing pixels vs fresh start: {:.4}%", d * 100.0);
        assert!(d < 0.001, "{tag}: switched theme differs from a fresh start in {:.3}% of pixels", d * 100.0);
        // And back again.
        st(&mut h).config.theme = from;
        for _ in 0..20 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        let back = render(&mut h, &format!("render-theme-{tag}-back"));
        let mut fresh_from = ready(&fx2, from, 1.0);
        open_split(&mut fresh_from, p.clone());
        let reference_from = render(&mut fresh_from, &format!("render-theme-{tag}-fresh-from"));
        let d = diff_fraction(&reference_from, &back);
        eprintln!("{tag} back: differing pixels vs fresh start: {:.4}%", d * 100.0);
        assert!(d < 0.001, "{tag} back: differs from a fresh start in {:.3}% of pixels", d * 100.0);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Synchronized scrolling
// ---------------------------------------------------------------------------------------------------------

/// A long note with mixed blocks: headings, long wrapping paragraphs, code, lists, tables, images, math.
fn mixed_note(sections: usize) -> String {
    let mut s = String::new();
    for i in 0..sections {
        s.push_str(&format!("## Section {i}\n\n"));
        s.push_str(&format!("Paragraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat, with `code` and a [link](https://example.org).\n\n"));
        match i % 5 {
            0 => s.push_str("```rust\nfn main() {\n    println!(\"hello\");\n    let x = 1;\n    let y = 2;\n}\n```\n\n"),
            1 => s.push_str("- item one\n- item two with some more words in it\n  - nested\n- [ ] task\n\n"),
            2 => s.push_str("![pic](pic.png)\n\n"),
            3 => s.push_str("$$\\int_0^1 x^2 dx = \\frac{1}{3}$$\n\n| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n\n"),
            _ => s.push_str("> a quote that goes on\n> over two lines\n\n1. first\n2. second\n\n---\n\n"),
        }
    }
    s
}

struct Trace {
    /// (editor offset, preview offset) after each frame.
    frames: Vec<(f32, f32)>,
    /// Frames that belong to the scrolling phase (the rest is the smooth-scroll tail).
    driven: usize,
    editor_map: iron_sheets::scrollsync::ScrollMap,
    preview_map: iron_sheets::scrollsync::ScrollMap,
}

fn tab_scrolls(h: &mut H) -> (f32, f32) {
    let t = st(h).tabs.active_tab().expect("tab");
    (t.editor_scroll.offset, t.preview_scroll.offset)
}

/// Wheel-scroll one pane in small steps and record both offsets after every frame.
/// A note whose preview has one-line blocks that are much taller than a source line: a 300 px image and display formulas.
fn tall_note(sections: usize) -> String {
    let mut s = String::new();
    for i in 0..sections {
        s.push_str(&format!("## Part {i}\n\nA short paragraph {i} before the picture, long enough to wrap onto a second line in the narrow preview column of this test.\n\n![tall](tall.png)\n\nText after the picture.\n\n$$\\int_0^1 x^{i} dx = \\frac{{1}}{{{i}+1}}$$\n\nAnd more text.\n\n"));
    }
    s
}

fn scroll_trace_note(over_editor: bool, step: f32, frames: usize, from_top: bool, warm: bool, note: String) -> Trace {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy image");
    let mut tall = image::RgbaImage::new(120, 300);
    for (x, y, px) in tall.enumerate_pixels_mut() {
        *px = image::Rgba([(x * 2) as u8, (y % 256) as u8, 90, 255]);
    }
    tall.save(fx.notes().join("tall.png")).expect("tall png");
    let p = fx.notes().join("Mixed.md");
    std::fs::write(&p, note).expect("write");
    let mut h = ready(&fx, ThemePref::Light, 1.0);
    open_split_wait(&mut h, p, if warm { 20 } else { 2 });
    // Let exact heights settle (or not: a cold preview still has estimated heights for blocks it never showed).
    for _ in 0..if warm { 60 } else { 0 } {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    let pos = if over_editor { egui::pos2(450.0, 300.0) } else { egui::pos2(900.0, 300.0) };
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    if !from_top {
        // Start in the middle of the note.
        let mid = st(&mut h).tabs.active_tab().expect("tab").editor_scroll.max_offset() / 2.0;
        st(&mut h).tabs.active_tab_mut().expect("tab").editor_scroll.force = Some(mid);
        for _ in 0..30 {
            h.step();
        }
    }
    let mut frames_v = Vec::new();
    for _ in 0..frames {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -step),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        frames_v.push(tab_scrolls(&mut h));
    }
    // Drain the smooth-scroll animation.
    for _ in 0..30 {
        h.step();
        frames_v.push(tab_scrolls(&mut h));
    }
    let (em, pm) = {
        let t = st(&mut h).tabs.active_tab().expect("tab");
        (t.editor_scroll.map.clone(), t.preview_scroll.map.clone())
    };
    Trace { frames: frames_v, driven: frames, editor_map: em, preview_map: pm }
}

/// (max step ratio, max step in px, max |second difference|) of the follower pane relative to the driver's step.
fn analyse(t: &Trace, driver_is_editor: bool) -> (f32, f32, f32) {
    let off = |f: &(f32, f32)| if driver_is_editor { (f.0, f.1) } else { (f.1, f.0) };
    let mut max_ratio = 0.0f32;
    let mut max_step = 0.0f32;
    let mut max_d2 = 0.0f32;
    let mut prev_follow_step = 0.0f32;
    let mut d2s = Vec::new();
    for w in t.frames[3.min(t.frames.len())..].windows(2) {
        let (d0, f0) = off(&w[0]);
        let (d1, f1) = off(&w[1]);
        let (dd, df) = (d1 - d0, f1 - f0);
        max_step = max_step.max(df.abs());
        if dd.abs() > 0.5 {
            max_ratio = max_ratio.max((df / dd).abs());
        }
        max_d2 = max_d2.max((df - prev_follow_step).abs());
        d2s.push((df - prev_follow_step).abs());
        prev_follow_step = df;
    }
    d2s.sort_by(f32::total_cmp);
    eprintln!("   step change p50 {:.2}, p90 {:.2}, p99 {:.2}", d2s[d2s.len() / 2], d2s[d2s.len() * 9 / 10], d2s[d2s.len() * 99 / 100]);
    (max_ratio, max_step, max_d2)
}

/// Position error of the follower in the driver's pixels: where the driver would have to be for the follower to be
/// exactly in sync, minus where it is (final maps; positive = follower lags). Returns (max |err|, mean |err|).
fn lag(t: &Trace, driver_is_editor: bool) -> (f32, f32) {
    let (dm, fm) = if driver_is_editor { (&t.editor_map, &t.preview_map) } else { (&t.preview_map, &t.editor_map) };
    let mut max = 0.0f32;
    let mut sum = 0.0f32;
    let mut n = 0;
    for f in &t.frames[3..t.driven] {
        let (d, fo) = if driver_is_editor { (f.0, f.1) } else { (f.1, f.0) };
        let want = dm.y_for_line(fm.line_for_y(fo));
        let e = (d - want).abs();
        if e > 1.0 && std::env::var_os("TRACE").is_some() {
            eprintln!("   error {e:.1} px at driver offset {d:.1}");
        }
        max = max.max(e);
        sum += e;
        n += 1;
    }
    (max, sum / n.max(1) as f32)
}

/// Where the follower rests after the scrolling stopped, compared with where the final maps say it belongs.
fn rest_error(t: &Trace, driver_is_editor: bool) -> f32 {
    let (dm, fm) = if driver_is_editor { (&t.editor_map, &t.preview_map) } else { (&t.preview_map, &t.editor_map) };
    let f = t.frames.last().expect("frames");
    let (d, fo) = if driver_is_editor { (f.0, f.1) } else { (f.1, f.0) };
    (dm.y_for_line(fm.line_for_y(fo)) - d).abs()
}

#[test]
fn scroll_sync_smoothness_report() {
    // (name, editor drives, warm, tall-block note, sections)
    for (name, over_editor, warm, tall, sections) in [
        ("editor drives preview (warm)", true, true, false, 60),
        ("editor drives preview (cold, 800 sections)", true, false, false, 800),
        ("preview drives editor (warm)", false, true, false, 60),
        ("tall blocks: editor drives preview", true, true, true, 40),
        ("tall blocks: preview drives editor", false, true, true, 40),
    ] {
        let t = scroll_trace_note(over_editor, 6.0, 500, false, warm, if tall { tall_note(sections) } else { mixed_note(sections) });
        let (ratio, step, d2) = analyse(&t, over_editor);
        let (e, p) = *t.frames.last().expect("frames");
        let moved = (t.frames[0].0 - e).abs().max((t.frames[0].1 - p).abs());
        eprintln!("{name}: max follower/driver step ratio {ratio:.2}, max follower step {step:.1} px, max step change {d2:.1} px (moved {moved:.0} px)");
        {
            // Where is the worst follower step (source line, in the editor's map)?
            let off = |f: &(f32, f32)| if over_editor { (f.0, f.1) } else { (f.1, f.0) };
            let mut worst = (0.0f32, 0usize);
            for (i, w) in t.frames.windows(2).enumerate().skip(3) {
                let d = (off(&w[1]).1 - off(&w[0]).1).abs();
                if d > worst.0 {
                    worst = (d, i);
                }
            }
            let f = off(&t.frames[worst.1]);
            let line = if over_editor { t.editor_map.line_for_y(f.0) } else { t.preview_map.line_for_y(f.0) };
            eprintln!("{name}: worst follower step {:.1} px at frame {} (source line {line:.1})", worst.0, worst.1);
        }
        let (lmax, lmean) = lag(&t, over_editor);
        eprintln!("{name}: follower position error vs driver: max {lmax:.1} px, mean {lmean:.1} px, at rest {:.1} px", rest_error(&t, over_editor));
        if std::env::var_os("SOFT").is_some() {
            continue; // report only (used to record numbers of an older build)
        }
        if over_editor {
            // Same-frame sync: the preview is where the map says it belongs, not one frame behind.
            let allowed = if warm { 1.0 } else { 100.0 };
            assert!(lmax <= allowed, "{name}: preview position error {lmax} px");
            assert!(rest_error(&t, over_editor) < 1.0, "{name}: preview rests off its target");
            assert!(ratio < 12.0, "{name}: preview jumped {ratio}x the editor step");
        } else {
            assert!(ratio < 4.0, "{name}: editor jumped {ratio}x the preview step");
            // Same-frame sync in this direction too (one frame of lag at 6 px per frame was a constant 6 px error; what is
            // left are a few frames where the preview corrects its own offset after block heights settled).
            assert!(lmax <= 3.0 && lmean < 0.5, "{name}: editor position error max {lmax} px, mean {lmean} px");
        }
        if std::env::var_os("TRACE").is_some() {
            for (i, f) in t.frames.iter().enumerate() {
                eprintln!("{i} {:.1} {:.1}", f.0, f.1);
            }
        }
    }
}
