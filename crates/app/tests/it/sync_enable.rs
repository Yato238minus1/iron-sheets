//! Switching synchronized scrolling on brings the preview to the editor's position.
use crate::common::*;
use egui::vec2;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn steps(h: &mut H, ms: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn enabling_sync_moves_the_preview_to_the_editor() {
    let fx = Fx::new();
    let mut text = String::new();
    for i in 0..400 {
        text.push_str(&format!("## Heading {i}\n\nParagraph {i} with a little text to fill the row.\n\n"));
    }
    let note = fx.notes().join("Long.md");
    std::fs::write(&note, text).unwrap();
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.sync_scroll = false;
    let mut h = harness(o);
    h.set_size(vec2(1100.0, 700.0));
    st(&mut h).commands.push(Command::OpenPath { path: note, new_tab: true, line: None });
    steps(&mut h, 800);
    let tab = st(&mut h).tabs.active.unwrap();
    // Sync off: scroll only the editor far down.
    st(&mut h).tabs.tabs[tab].editor_scroll.force = Some(6000.0);
    steps(&mut h, 300);
    let (eo, po) = (st(&mut h).tabs.tabs[tab].editor_scroll.offset, st(&mut h).tabs.tabs[tab].preview_scroll.offset);
    assert!(eo > 5000.0 && po < 100.0, "precondition: editor {eo}, preview {po}");
    st(&mut h).commands.push(Command::ToggleSyncScroll);
    steps(&mut h, 600);
    let t = &st(&mut h).tabs.tabs[tab];
    let (el, pl) = (t.editor_scroll.map.line_for_y(t.editor_scroll.offset), t.preview_scroll.map.line_for_y(t.preview_scroll.offset));
    assert!((el - pl).abs() < 8.0, "preview at source line {pl}, editor at {el}");
}
