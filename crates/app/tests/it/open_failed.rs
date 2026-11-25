//! IS-33: a file that cannot be opened keeps its tab, showing why, with Retry / Reveal / Close.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;

fn open_missing() -> (Fx, egui_kittest::Harness<'static, Slot>, std::path::PathBuf) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let missing = fx.notes().join("Nope.md");
    h.state_mut().app().state.commands.push(Command::OpenPath { path: missing.clone(), new_tab: true, line: None });
    settle(&mut h);
    (fx, h, missing)
}

#[test]
fn failed_open_shows_panel_and_enter_retries() {
    let (_fx, mut h, missing) = open_missing();
    h.step();
    assert!(h.state_mut().app().state.tabs.find(&missing).is_some(), "tab stays");
    assert!(!h.state_mut().app().state.is_active_tab_loading(), "failed is not loading");
    h.get_by_label("Can't open Nope.md");
    h.get_by_label("Reveal in file manager");
    h.get_by_label("Close");
    let p = shot(&mut h, "open_failed");
    eprintln!("screenshot {}", p.display());
    // The file appears; Enter (Retry has focus) loads it.
    std::fs::write(&missing, "# Now here\n").expect("write");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    assert!(st.docs.get(&missing).is_some_and(|d| d.text.contains("Now here")), "retry loaded the note");
    assert!(st.tabs.find(&missing).is_some_and(|i| st.tabs.tabs[i].load_error.is_none()));
}

#[test]
fn failed_open_closes_with_button_and_escape() {
    let (_fx, mut h, missing) = open_missing();
    h.step();
    h.get_by_label("Close").click();
    settle(&mut h);
    assert!(h.state_mut().app().state.tabs.find(&missing).is_none(), "Close button closes");
    let (_fx, mut h, missing) = open_missing();
    h.step();
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.state_mut().app().state.tabs.find(&missing).is_none(), "Esc closes");
}
