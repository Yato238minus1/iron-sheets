//! Per-keystroke cost of big notes: the status bar counts follow the edits without recounting, and paragraphs far from
//! the view are let go of when idle.
use crate::common::*;
use egui::{Event, Key};
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

fn big_note() -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < 700 * 1024 {
        s.push_str(&format!("## Héading {i}\n\nParagraph {i}: lorem ipsum\u{a0}dolor → sit amet,  consectetur\tadipiscing.\n\n- item {i}\n- [ ] task {i}\n\n"));
        i += 1;
    }
    s
}

fn open() -> (Fx, H, std::path::PathBuf, String) {
    let fx = Fx::new();
    let text = big_note();
    let path = fx.notes().join("Big.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);
    (fx, h, path, text)
}

fn frames(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

#[test]
fn counts_follow_typing_in_a_big_note_without_recounting() {
    let (_fx, mut h, path, text) = open();
    let mid = (0..text.len() / 2).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    frames(&mut h, 6);
    let base = h.state_mut().app().state.docs.get(&path).expect("doc").full_count_runs();
    for (i, ev) in ["a", " ", "é", "\u{a0}", "  ", "→→", "\n", "x y"].iter().enumerate() {
        h.event(Event::Text((*ev).into()));
        frames(&mut h, 2);
        if i % 3 == 2 {
            h.key_press(Key::Backspace);
            frames(&mut h, 2);
        }
        let st = &mut h.state_mut().app().state;
        let doc = st.docs.get(&path).expect("doc");
        let want = iron_sheets::docs::count_text(&doc.text);
        assert_eq!(doc.counts(), want, "after {ev:?}");
        assert_eq!(st.stats.counts(), want, "status bar after {ev:?}");
    }
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.docs.get(&path).expect("doc").full_count_runs(), base, "typing never counted the whole note again");
    assert!(st.docs.get(&path).is_some_and(|d| d.text.len() > text.len()), "the edits went into the note");
}

#[test]
fn far_paragraphs_are_let_go_of_after_scrolling_through_the_note() {
    let (_fx, mut h, _path, text) = open();
    let paras = text.matches('\n').count() + 1;
    let id = h.state_mut().app().state.tabs.active_tab().expect("tab").id;
    let wheel = |dy: f32| Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    };
    h.event(Event::PointerMoved(egui::pos2(600.0, 400.0)));
    frames(&mut h, 2);
    let mut most_real = 0;
    for _ in 0..30 {
        h.event(wheel(-16000.0));
        frames(&mut h, 2);
        // Idle frames lay out around the view and let go of the far part.
        std::thread::sleep(Duration::from_millis(5));
        frames(&mut h, 2);
        let s = h.state_mut().app().state.ui.editor.stats(id).expect("stats");
        most_real = most_real.max(paras - s.placeholders);
    }
    frames(&mut h, 30);
    let s = h.state_mut().app().state.ui.editor.stats(id).expect("stats");
    let real = paras - s.placeholders;
    assert!(most_real > real, "a lot was laid out on the way ({most_real}), little stays ({real} of {paras} paragraphs)");
    assert!(real < paras / 4, "{real} of {paras} paragraphs are still laid out at the end of the note");
}

/// Screenshot of a 20 MB note at its middle after typing there (look at it):
/// `IRON_SHEETS_SHOTS_DIR=... cargo test -p iron-sheets --release --test it -- bigcost:: --ignored shot_of_a_20_mb_note`
#[test]
#[ignore]
fn shot_of_a_20_mb_note_after_editing_in_the_middle() {
    let fx = Fx::new();
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 20 << 20 {
        text.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum **dolor** sit amet, *consectetur* adipiscing [[Ideas]] and `code` with $x^{i}$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n```rust\nfn main() {{ println!(\"{i}\"); }}\n```\n\n> quote {i} with a little more text\n\n| a | b |\n|---|---|\n| {i} | 2 |\n\n"
        ));
        i += 1;
    }
    let path = fx.notes().join("Big20.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Source));
    frames(&mut h, 8);
    let mid = text[..text.len() / 2].rfind("```rust").expect("fence") + 8;
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    frames(&mut h, 10);
    for ev in ["EDITED ", "in the ", "middle "] {
        h.event(Event::Text(ev.into()));
        frames(&mut h, 3);
    }
    frames(&mut h, 20);
    shot(&mut h, "bigcost-20m-middle");
    let doc = h.state_mut().app().state.docs.get(&path).expect("doc").text.clone();
    assert!(doc.contains("EDITED in the middle "));
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.docs.get(&path).expect("doc").counts(), iron_sheets::docs::count_text(&doc));
}

#[test]
fn undo_survives_a_paste_that_crosses_a_size_class() {
    let fx = Fx::new();
    // Just under the 640 KB undo size class; the paste takes it over.
    let mut text = big_note();
    text.truncate(600 * 1024);
    let text = text.trim_end_matches(|c: char| !c.is_ascii()).to_string();
    let path = fx.notes().join("Grow.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);
    let mid = (0..text.len() / 2).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    frames(&mut h, 6);
    h.event(Event::Paste("pasted ".repeat(15_000)));
    frames(&mut h, 3);
    let len = |h: &mut H| h.state_mut().app().state.docs.get(&path).expect("doc").text.len();
    assert!(len(&mut h) > 700 * 1024, "the paste crossed the size class");
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::Z);
    frames(&mut h, 3);
    assert_eq!(h.state_mut().app().state.docs.get(&path).expect("doc").text, text, "undo reverted the paste");
}
