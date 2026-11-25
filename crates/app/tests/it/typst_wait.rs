//! IS-17: before the first Typst snapshot the preview shows a page-shaped placeholder and "compiling...".
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;

#[test]
fn first_render_shows_a_page_placeholder() {
    let fx = Fx::new();
    let p = fx.notes().join("Wait.typ");
    std::fs::write(&p, "= Wait\nSome text.\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    // A few frames: the note is loaded and the preview drawn; the first compile is still running.
    for _ in 0..4 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let waiting = {
        let st = &h.state_mut().app().state;
        st.tabs.active_tab().is_some_and(|t| st.typst.document(t.id).is_none())
    };
    let s = shot(&mut h, "typst_first_render_placeholder");
    eprintln!("waiting for first snapshot: {waiting}; screenshot {}", s.display());
    assert!(waiting, "test needs the first compile to still be running");
    assert!(h.query_by_label("compiling...").is_some(), "initial compile shows compiling... cue");
}
