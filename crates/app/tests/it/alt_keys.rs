//! VS Code / Zed style Alt keys in the editor: Alt+Up/Down move lines, Alt+Shift+Up/Down copy lines, Alt+X/C cut/copy.
use crate::common::*;
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn setup(name: &str, body: &str) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join(name);
    std::fs::write(&path, body).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label(name).next().expect("file in tree").click();
    settle(&mut h);
    // Focus the editor.
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    (fx, h, path)
}

fn text(h: &mut H, p: &std::path::Path) -> String {
    h.state_mut().app().state.docs.get(p).expect("doc").text.clone()
}

fn select(h: &mut H, range: std::ops::Range<usize>) {
    if let Some(t) = h.state_mut().app().state.tabs.active_tab_mut() {
        t.pending_cursor = Some(range);
    }
    settle(h);
}

fn cursor(h: &mut H) -> Option<std::ops::Range<usize>> {
    h.state_mut().app().state.tabs.active_tab().and_then(|t| t.cursor.clone())
}

/// One frame with what egui-winit sends for Alt+key on Linux/Windows: the key, and the typed character as well
/// (`egui-winit-0.37.0/src/lib.rs:1177` only suppresses text for Ctrl/Cmd). kittest's `h.event` gives every event its
/// own frame, so the events go straight into the next frame's input. Returns that frame's clipboard text.
fn alt_press(h: &mut H, m: Modifiers, key: Key, typed: Option<&str>) -> Option<String> {
    let ev = &mut h.input_mut().events;
    ev.push(Event::ModifiersChanged(m));
    ev.push(Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: m });
    if let Some(t) = typed {
        ev.push(Event::Text(t.into()));
    }
    ev.push(Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: m });
    h.step();
    let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    h.event(Event::ModifiersChanged(Modifiers::default()));
    settle(h);
    copied
}

#[test]
fn alt_up_down_move_lines() {
    let (_fx, mut h, p) = setup("M.md", "one\ntwo\nthree\n");
    select(&mut h, 5..5);
    alt_press(&mut h, Modifiers::ALT, Key::ArrowDown, None);
    assert_eq!(text(&mut h, &p), "one\nthree\ntwo\n");
    alt_press(&mut h, Modifiers::ALT, Key::ArrowUp, None);
    assert_eq!(text(&mut h, &p), "one\ntwo\nthree\n");
    // Ctrl+Shift+Down still works.
    select(&mut h, 5..5);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "one\nthree\ntwo\n");
}

#[test]
fn alt_shift_up_down_duplicate_lines_and_do_not_move() {
    let (_fx, mut h, p) = setup("D.md", "one\ntwo\nthree\n");
    select(&mut h, 5..5);
    alt_press(&mut h, Modifiers::ALT | Modifiers::SHIFT, Key::ArrowDown, None);
    assert_eq!(text(&mut h, &p), "one\ntwo\ntwo\nthree\n");
    assert_eq!(cursor(&mut h), Some(9..9), "caret on the lower copy");
    alt_press(&mut h, Modifiers::ALT | Modifiers::SHIFT, Key::ArrowUp, None);
    assert_eq!(text(&mut h, &p), "one\ntwo\ntwo\ntwo\nthree\n");
    assert_eq!(cursor(&mut h), Some(9..9), "caret stays on the upper copy");
    shot(&mut h, "alt-keys-duplicate");
}

#[test]
fn alt_x_cuts_alt_c_copies_and_no_character_leaks() {
    let (_fx, mut h, p) = setup("C.md", "alpha beta gamma\n");
    select(&mut h, 6..10);
    let copied = alt_press(&mut h, Modifiers::ALT, Key::C, Some("c"));
    assert_eq!(copied.as_deref(), Some("beta"));
    assert_eq!(text(&mut h, &p), "alpha beta gamma\n", "Alt+C neither edits nor types");
    let cut = alt_press(&mut h, Modifiers::ALT, Key::X, Some("x"));
    assert_eq!(cut.as_deref(), Some("beta"));
    assert_eq!(text(&mut h, &p), "alpha  gamma\n", "selection removed, no x typed");
    // Empty selection: like Ctrl+X, nothing happens.
    select(&mut h, 0..0);
    assert_eq!(alt_press(&mut h, Modifiers::ALT, Key::X, Some("x")), None);
    assert_eq!(text(&mut h, &p), "alpha  gamma\n");
    // AltGr (Ctrl+Alt) is not Alt: nothing is cut.
    select(&mut h, 0..5);
    let altgr = Modifiers { alt: true, ctrl: true, command: true, ..Default::default() };
    alt_press(&mut h, altgr, Key::X, None);
    assert_eq!(text(&mut h, &p), "alpha  gamma\n");
}

#[test]
fn alt_left_still_navigates_back() {
    let fx = Fx::new();
    for n in ["A.md", "B.md"] {
        std::fs::write(fx.notes().join(n), "x\n").expect("write");
    }
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let active = |h: &mut H| h.state_mut().app().state.tabs.active_path().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    h.get_all_by_label("A.md").next().expect("A").click();
    settle(&mut h);
    h.get_all_by_label("B.md").next().expect("B").click();
    settle(&mut h);
    assert_eq!(active(&mut h).as_deref(), Some("B.md"));
    alt_press(&mut h, Modifiers::ALT, Key::ArrowLeft, None);
    assert_eq!(active(&mut h).as_deref(), Some("A.md"));
}

#[test]
fn alt_keys_work_in_typst_notes() {
    let (_fx, mut h, p) = setup("T.typ", "one\ntwo\n");
    select(&mut h, 0..3);
    let copied = alt_press(&mut h, Modifiers::ALT, Key::C, Some("c"));
    assert_eq!(copied.as_deref(), Some("one"));
    assert_eq!(text(&mut h, &p), "one\ntwo\n", "no c typed");
    select(&mut h, 0..0);
    alt_press(&mut h, Modifiers::ALT, Key::ArrowDown, None);
    assert_eq!(text(&mut h, &p), "two\none\n");
}
