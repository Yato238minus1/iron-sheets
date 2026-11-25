//! Mermaid fences in the Markdown preview: rendered image, error fallback, setting off.
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::time::Duration;

type H = Harness<'static, Slot>;

const NOTE: &str = "# Diagrams\n\n```mermaid\nflowchart LR\n  A[Start] --> B{Ok?}\n  B -->|yes| C[Done]\n  B -->|no| A\n```\n\n```mermaid\nsequenceDiagram\n  Alice->>Bob: Hello\n  Bob-->>Alice: Hi\n```\n\n```mermaid\nthis is not a diagram\n```\n";

fn open(on: bool) -> (Fx, H) {
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, NOTE).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Dark;
    o.config.render_mermaid = on;
    let mut h = harness_dt(o, 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    // The worker renders in the background: give it time, then let the layout pick the result up.
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    (fx, h)
}

fn heights(h: &mut H) -> Vec<f32> {
    let mut v: Vec<_> = h.state_mut().app().state.ui.md_preview.block_rects.iter().map(|(i, r)| (*i, r.height())).collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, h)| h).collect()
}

#[test]
fn diagrams_render_and_errors_fall_back() {
    let (_fx, mut h) = open(true);
    let hs = heights(&mut h);
    assert_eq!(hs.len(), 4, "{hs:?}");
    // Rendered flowchart and sequence diagram: neither the 80 pt placeholder nor a few lines of code.
    assert!(hs[1] > 90.0 && hs[2] > 90.0, "{hs:?}");
    shot(&mut h, "mermaid-dark");
    let on_err = hs[3];
    let (_fx2, mut off) = open(false);
    let hs_off = heights(&mut off);
    shot(&mut off, "mermaid-off");
    // The failed diagram is code plus a one-line note, so taller than the plain code block.
    assert!(on_err > hs_off[3] + 8.0, "{on_err} vs {hs_off:?}");
}
