//! A 5 MB note above the highlight limit shows highlighting in the visible part: top, middle and end.
use crate::common::*;
use egui::{Key, Modifiers};
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

fn big_note(mb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < mb << 20 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum **dolor** sit amet, *consectetur* adipiscing [[Ideas]] and `code` with $x^{i}$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n```rust\nfn main() {{ println!(\"{i}\"); }}\n```\n\n> quote {i} with a little more text\n\n| a | b |\n|---|---|\n| {i} | 2 |\n\n"
        ));
        i += 1;
    }
    s.push_str("THE END\n");
    s
}

/// Share of coloured (saturated) pixels in the editor area.
fn coloured(img: &image::RgbaImage) -> f32 {
    let (w, h) = img.dimensions();
    let (mut n, mut c) = (0usize, 0usize);
    for y in (100..h - 40).step_by(2) {
        for x in (320..w.min(1000)).step_by(2) {
            let p = img.get_pixel(x, y).0;
            n += 1;
            if p[..3].iter().max().copied().unwrap_or(0) as i32 - p[..3].iter().min().copied().unwrap_or(0) as i32 > 60 {
                c += 1;
            }
        }
    }
    c as f32 / n as f32
}

fn render(h: &mut egui_kittest::Harness<'static, Slot>) -> image::RgbaImage {
    let _g = GPU_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    h.render().expect("render")
}

#[test]
fn five_mb_note_is_highlighted_where_it_is_visible() {
    let fx = Fx::new();
    let text = big_note(5);
    let path = fx.notes().join("Big5.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Source));
    }
    settle(&mut h);
    // Top.
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(0..0);
    for _ in 0..8 {
        h.step();
    }
    let top = render(&mut h);
    shot(&mut h, "bignote-1-top");
    assert!(coloured(&top) > 0.002, "highlighted at the top: {}", coloured(&top));
    // Middle: inside a code block, a table, ...
    let mid = text[..text.len() / 2].rfind("```rust").expect("fence") + 8;
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
    for _ in 0..10 {
        h.step();
    }
    let middle = render(&mut h);
    shot(&mut h, "bignote-2-middle");
    assert!(coloured(&middle) > 0.002, "highlighted in the middle: {}", coloured(&middle));
    // End.
    h.key_press_modifiers(Modifiers::CTRL, Key::End);
    for _ in 0..10 {
        h.step();
    }
    let end = render(&mut h);
    shot(&mut h, "bignote-3-end");
    assert!(coloured(&end) > 0.002, "highlighted at the end: {}", coloured(&end));
    let st = &mut h.state_mut().app().state;
    let id = st.tabs.active_tab().expect("tab").id;
    let s = st.ui.editor.stats(id).expect("stats");
    assert_eq!(s.chars, text.chars().count());
    assert!(s.placeholders > 1000, "the rest of the note is still estimated: {s:?}");
}

#[test]
fn idle_frames_lay_out_the_paragraphs_around_the_viewport_and_then_stop() {
    let fx = Fx::new();
    let text = big_note(1);
    let path = fx.notes().join("Big1.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Source));
    }
    let placeholders = |h: &mut egui_kittest::Harness<'static, Slot>| {
        let st = &mut h.state_mut().app().state;
        let id = st.tabs.active_tab().expect("tab").id;
        st.ui.editor.stats(id).expect("stats").placeholders
    };
    // Frame 1 runs the commands; the note loads in the background, then a frame shows it with only the viewport laid out.
    h.step();
    while h.state_mut().app().state.is_active_tab_loading() {
        std::thread::sleep(std::time::Duration::from_millis(2));
        h.step();
    }
    h.step();
    let p0 = placeholders(&mut h);
    for _ in 0..40 {
        h.step();
    }
    let p1 = placeholders(&mut h);
    assert!(p1 < p0, "idle frames replaced placeholders: {p0} -> {p1}");
    for _ in 0..20 {
        h.step();
    }
    let p2 = placeholders(&mut h);
    assert_eq!(p1, p2, "no more idle work once the window around the viewport is exact");
    assert!(p2 > 1000, "far paragraphs stay estimated: {p2}");
}
