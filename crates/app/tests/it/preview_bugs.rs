//! Regression tests for preview bugs reported by the user (sticky bullet spacing, scroll drift).
use crate::common::*;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

fn run(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn mixed_note(kb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < kb << 10 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum **dolor** sit amet, *consectetur* adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua {}.\n\n- same item\n- same item\n- same item\n\n```rust\nfn main() {{}}\n```\n\n| a | b |\n|---|---|\n| {i} | 2 |\n\n",
            "word ".repeat(i % 30)
        ));
        i += 1;
    }
    s
}

fn settle_unlaid(h: &mut H) {
    let t = std::time::Instant::now();
    for _ in 0..3000 {
        h.step();
        let m = &h.state_mut().app().state.ui.md_preview;
        if t.elapsed() > Duration::from_millis(600) && m.unlaid == 0 && m.parse_count > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    run(h, 5);
}

/// A block with the same content as an earlier one (a repeated list item) took the cached layout without taking
/// its height, so it kept the estimate for good and the list looked spread out.
#[test]
fn repeated_blocks_get_their_exact_height() {
    let fx = Fx::new();
    let p = fx.notes().join("Same.md");
    std::fs::write(&p, "# T\n\n".to_string() + &"- same item\n".repeat(60)).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Split));
    settle_unlaid(&mut h);
    let t = h.state_mut().app().state.ui.md_preview.tops_of(&p).map(<[f32]>::to_vec).unwrap_or_default();
    assert_eq!(t.len(), 61);
    let d: Vec<f32> = t[1..].windows(2).map(|w| w[1] - w[0]).collect();
    assert!(d.iter().all(|x| (x - d[0]).abs() < 0.01), "item pitch differs: {d:?}");
}

/// Wheel-scroll through a long note in split view: the preview's top line stays near the editor's.
#[test]
fn preview_follows_editor_while_scrolling_a_long_note() {
    let fx = Fx::new();
    let p = fx.notes().join("Long.md");
    std::fs::write(&p, mixed_note(300)).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Split));
    settle_unlaid(&mut h);
    h.event(egui::Event::PointerMoved(egui::pos2(450.0, 400.0)));
    let mut worst = 0.0f32;
    for k in 0..120 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -150.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Unknown,
        });
        h.step();
        if k % 10 == 9 {
            run(&mut h, 3);
            let tab = h.state_mut().app().state.tabs.active_tab_mut().expect("tab");
            let (e, p) = (&tab.editor_scroll, &tab.preview_scroll);
            let (le, lp) = (e.map.line_for_y(e.offset), p.map.line_for_y(p.offset));
            eprintln!("step {k}: editor line {le:.1} preview line {lp:.1} offsets {:.0}/{:.0}", e.offset, p.offset);
            worst = worst.max((le - lp).abs());
        }
    }
    assert!(worst < 12.0, "preview top line drifted {worst} lines from the editor's");
}
