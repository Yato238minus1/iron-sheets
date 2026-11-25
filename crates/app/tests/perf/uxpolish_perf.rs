//! Frame times for the UX polish work: a font size change on the 100k-word note and the tab strip with 30 long names.
//! Run with `cargo test --release -p iron-sheets --test perf -- uxpolish_perf:: --nocapture --test-threads 1`. Uses only
//! APIs that exist before the change as well, so the same file runs against the base copy.
use crate::common::*;
use egui::Event;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
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

fn quiet(h: &mut H, n: usize) {
    for _ in 0..n {
        frame(h, vec![]);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn words_note() -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.split_whitespace().count() < 100_000 {
        s.push_str(&format!("## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n> quote {i} with a little more text in it\n\n"));
        i += 1;
    }
    s
}

/// Font size change (what Ctrl+wheel does): the frame that applies the new size and the frames after it.
#[test]
fn uxperf_font_zoom_on_100k_words() {
    for mode in [ViewMode::Source, ViewMode::Split] {
        let fx = Fx::new();
        let p = fx.notes().join("Words100k.md");
        std::fs::write(&p, words_note()).expect("write");
        let mut h = harness(fx.opts(vec![fx.notes()]));
        h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
        h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        h.state_mut().app().state.commands.push(Command::SetViewMode(mode));
        h.step();
        h.step();
        quiet(&mut h, 40);
        let mut idle = Vec::new();
        for _ in 0..30 {
            idle.push(frame(&mut h, vec![]));
        }
        eprintln!("UXPERF font zoom, {mode:?}: idle frame {}", stats(idle));
        let (mut change, mut after) = (Vec::new(), Vec::new());
        for k in 0..16 {
            let up = k % 2 == 0;
            {
                let c = &mut h.state_mut().app().state.config;
                c.editor_font_size += if up { 1.0 } else { -1.0 };
                c.preview_font_size += if up { 1.0 } else { -1.0 };
            }
            change.push(frame(&mut h, vec![]));
            for _ in 0..3 {
                after.push(frame(&mut h, vec![]));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("UXPERF font zoom, {mode:?}: frame applying the size {}", stats(change));
        eprintln!("UXPERF font zoom, {mode:?}: next 3 frames {}", stats(after));
    }
}

/// Idle frame time of the tab strip with 30 tabs of long names, against one tab.
#[test]
fn uxperf_tab_strip_30_long_names() {
    let fx = Fx::new();
    let mut paths = Vec::new();
    for i in 0..30 {
        let p = fx.notes().join(format!("a-rather-long-note-file-name-number-{i:02}-with-a-long-tail-2024.md"));
        std::fs::write(&p, format!("# Note {i}\n\nText.\n")).expect("write");
        paths.push(p);
    }
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: paths[0].clone(), new_tab: true, line: None });
    h.step();
    h.step();
    quiet(&mut h, 40);
    let mut one = Vec::new();
    for _ in 0..60 {
        one.push(frame(&mut h, vec![]));
    }
    eprintln!("UXPERF tab strip, 1 tab: idle frame {}", stats(one));
    for p in &paths[1..] {
        h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
        h.step();
        h.step();
    }
    quiet(&mut h, 40);
    assert_eq!(h.state_mut().app().state.tabs.tabs.len(), 30);
    let mut many = Vec::new();
    for _ in 0..60 {
        many.push(frame(&mut h, vec![]));
    }
    eprintln!("UXPERF tab strip, 30 long tabs: idle frame {}", stats(many));
}

/// Idle frame time with the file tree showing a nested, expanded folder structure (guides, row loop).
#[test]
fn uxperf_file_tree_nested_idle_frame() {
    let fx = Fx::new();
    let mut dirs = Vec::new();
    for a in 0..6 {
        for b in 0..4 {
            let d = fx.notes().join(format!("group-{a}")).join(format!("sub-{b}"));
            std::fs::create_dir_all(&d).expect("dir");
            for f in 0..6 {
                std::fs::write(d.join(format!("note-{f}.md")), "# n\n").expect("write");
            }
            dirs.push(d);
        }
    }
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.step();
    h.step();
    for a in 0..6 {
        h.state_mut().app().state.tree.toggle(&fx.notes().join(format!("group-{a}")));
    }
    h.step();
    for d in &dirs {
        h.state_mut().app().state.tree.toggle(d);
    }
    h.step();
    h.step();
    quiet(&mut h, 40);
    let rows = h.state_mut().app().state.tree.rows().len();
    let mut v = Vec::new();
    for _ in 0..100 {
        v.push(frame(&mut h, vec![]));
    }
    eprintln!("UXPERF file tree, {rows} rows, depth 3: idle frame {}", stats(v));
}
