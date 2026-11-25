//! Arrow ligatures (Lilex): display only. The text, the caret steps and the file stay as typed; inline code and code
//! blocks stay literal unless asked; "Ligatures" off draws no ligature anywhere. Screenshots: `lig_on`, `lig_code`, `lig_off`.
use crate::common::*;
use egui::accesskit::Role;
use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::settings;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

type H = Harness<'static, Slot>;

const NOTE: &str = "a -> b => c != d <=> e\n\nspan `x -> y` end\n\n```\nf -> g => h\n```\n";

fn setup() -> (Fx, H, std::path::PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join("Lig.md");
    std::fs::write(&path, NOTE).expect("write note");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.editor_font = "lilex".into();
    o.config.editor_font_size = 28.0;
    let mut h = harness(o);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label("Lig.md").next().expect("file in tree").click();
    settle(&mut h);
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.view = ViewMode::Source;
    }
    settle(&mut h);
    (fx, h, path)
}

fn text(h: &mut H, p: &std::path::Path) -> String {
    h.state_mut().app().state.docs.get(p).expect("doc").text.clone()
}

fn cursor(h: &mut H) -> usize {
    h.state_mut().app().state.tabs.active_tab().and_then(|t| t.cursor.clone()).expect("cursor").start
}

fn set_cursor(h: &mut H, at: usize) {
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.pending_cursor = Some(at..at);
    }
    settle(h);
}

fn flip(h: &mut H, ligatures: bool, in_code: bool) {
    let st = &mut h.state_mut().app().state;
    st.config.module_ligatures = ligatures;
    st.config.ligatures_in_code = in_code;
    settings::apply(st, false);
    settle(h);
}

#[test]
fn ligatures_keep_text_and_caret_steps_and_toggle() {
    let (_fx, mut h, p) = setup();
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    // Typing goes through as typed.
    set_cursor(&mut h, NOTE.len());
    h.get_by_role(Role::MultilineTextInput).type_text("z -> y");
    settle(&mut h);
    assert_eq!(text(&mut h, &p), format!("{NOTE}z -> y"));
    // The caret steps over `->` one character at a time.
    set_cursor(&mut h, 2);
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(cursor(&mut h));
        h.key_press(Key::ArrowRight);
        settle(&mut h);
    }
    seen.push(cursor(&mut h));
    assert_eq!(seen, [2, 3, 4, 5, 6], "a, space, -, > each one step: {seen:?}");
    shot(&mut h, "lig_on");
    flip(&mut h, true, true);
    shot(&mut h, "lig_code");
    flip(&mut h, false, false);
    shot(&mut h, "lig_off");
    assert_eq!(text(&mut h, &p), format!("{NOTE}z -> y"), "toggling never edits the text");
    flip(&mut h, true, false);
}
