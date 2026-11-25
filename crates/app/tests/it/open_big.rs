//! Big notes open with only the viewport laid out exactly; everything else is estimated until it is needed.
//! These tests check that cursor, selection, scrolling, resize and scroll sync stay correct while placeholders exist.
use crate::common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn note(kb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < kb * 1024 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, with inline math $x^{i} + y$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n> quote {i} with a little more text in it\n\n"
        ));
        i += 1;
    }
    s.push_str("THE END\n");
    s
}

struct Opened {
    _fx: Fx,
    h: H,
    path: PathBuf,
    text: String,
}

fn open(mode: ViewMode, kb: usize) -> Opened {
    let fx = Fx::new();
    let text = note(kb);
    let path = fx.notes().join("Big.md");
    std::fs::write(&path, &text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(mode));
    }
    settle(&mut h);
    Opened { _fx: fx, h, path, text }
}

fn frames(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn stats(h: &mut H) -> iron_sheets_ui_egui::editor::LayoutStats {
    let st = &mut h.state_mut().app().state;
    let id = st.tabs.active_tab().expect("tab").id;
    st.ui.editor.stats(id).expect("editor laid out")
}

fn cursor(h: &mut H) -> Option<std::ops::Range<usize>> {
    h.state_mut().app().state.tabs.active_tab().and_then(|t| t.cursor.clone())
}

fn scroll(h: &mut H) -> (f32, f32, f32) {
    let t = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.clone();
    (t.offset, t.viewport_h, t.content_h)
}

fn render(h: &mut H) -> image::RgbaImage {
    let _g = GPU_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    h.render().expect("render")
}

/// Every 80 px band of the editor area must contain text pixels: an estimated (unpainted) row inside the viewport would
/// leave a blank band, because the note has text in at least every third line.
fn assert_no_blank_bands(img: &image::RgbaImage, what: &str) {
    let (w, hgt) = img.dimensions();
    let bg = *img.get_pixel(w - 4, hgt - 60);
    let differs = |p: &image::Rgba<u8>| p.0[..3].iter().zip(&bg.0[..3]).map(|(a, b)| (*a as i32 - *b as i32).abs()).sum::<i32>() > 120;
    let (x0, x1) = (320, w.min(1000));
    let mut y = 120;
    while y + 80 < hgt - 40 {
        let ink = (y..y + 80).step_by(2).flat_map(|yy| (x0..x1).step_by(2).map(move |xx| (xx, yy))).filter(|&(xx, yy)| differs(img.get_pixel(xx, yy))).count();
        assert!(ink > 30, "{what}: band at y={y} is blank ({ink} ink pixels)");
        y += 80;
    }
}

#[test]
fn opens_lazily_and_stays_consistent() {
    let mut o = open(ViewMode::Source, 100);
    let s = stats(&mut o.h);
    assert_eq!(s.chars, o.text.chars().count(), "every char is in some row, placeholder or not");
    assert!(s.placeholders > 1000, "most of a 100 KB note is not laid out at open: {s:?}");
    assert_no_blank_bands(&render(&mut o.h), "top after open");
    shot(&mut o.h, "open-01-top");
}

#[test]
fn caret_at_end_right_after_opening() {
    let mut o = open(ViewMode::Source, 100);
    // The caret starts at the end without scrolling there; go to the top, then jump to the end.
    o.h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(0..0);
    frames(&mut o.h, 6);
    assert_eq!(cursor(&mut o.h), Some(0..0));
    assert!(scroll(&mut o.h).0 < 5.0);
    o.h.key_press_modifiers(Modifiers::CTRL, Key::End);
    frames(&mut o.h, 8);
    assert_eq!(cursor(&mut o.h), Some(o.text.len()..o.text.len()));
    let (off, vh, ch) = scroll(&mut o.h);
    assert!(off + vh >= ch - 40.0, "scrolled to the end: offset {off}, viewport {vh}, content {ch}");
    let s = stats(&mut o.h);
    assert_eq!(s.chars, o.text.chars().count());
    assert_no_blank_bands(&render(&mut o.h), "end after Ctrl+End");
    shot(&mut o.h, "open-02-end");
    // Typing at the end goes to the end.
    o.h.event(Event::Text("Z".into()));
    frames(&mut o.h, 4);
    let doc = o.h.state_mut().app().state.docs.get(&o.path).expect("doc").text.clone();
    assert!(doc.ends_with("THE END\nZ"), "typed at the caret: {:?}", &doc[doc.len() - 12..]);
    assert_eq!(stats(&mut o.h).chars, doc.chars().count());
}

#[test]
fn select_all_then_replace() {
    let mut o = open(ViewMode::Source, 100);
    o.h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    frames(&mut o.h, 4);
    assert_eq!(cursor(&mut o.h), Some(0..o.text.len()));
    assert_no_blank_bands(&render(&mut o.h), "select all");
    shot(&mut o.h, "open-03-select-all");
    o.h.event(Event::Text("x".into()));
    frames(&mut o.h, 4);
    assert_eq!(o.h.state_mut().app().state.docs.get(&o.path).expect("doc").text, "x");
    assert_eq!(stats(&mut o.h).chars, 1);
}

#[test]
fn jump_far_below_like_find_does() {
    let mut o = open(ViewMode::Source, 100);
    let at = o.text.find("Section 300\n").expect("far section");
    o.h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(at..at + 11);
    frames(&mut o.h, 8);
    assert_eq!(cursor(&mut o.h), Some(at..at + 11));
    let (off, vh, ch) = scroll(&mut o.h);
    assert!(off > 0.3 * ch && off + vh < ch, "centered somewhere in the middle: {off} {vh} {ch}");
    assert_no_blank_bands(&render(&mut o.h), "after jump");
    shot(&mut o.h, "open-04-jump");
    // And back to the top.
    o.h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(0..0);
    frames(&mut o.h, 8);
    assert!(scroll(&mut o.h).0 < 5.0);
    assert_no_blank_bands(&render(&mut o.h), "back at the top");
}

#[test]
fn find_far_below_selects_and_scrolls() {
    let mut o = open(ViewMode::Source, 100);
    o.h.state_mut().app().state.find_request = true;
    frames(&mut o.h, 3);
    // Type a query that only matches far below.
    {
        use egui_kittest::kittest::Queryable;
        let q = o.h.get_all_by_role(egui::accesskit::Role::TextInput).next().expect("find field");
        q.type_text("Section 350");
    }
    frames(&mut o.h, 10);
    {
        use egui_kittest::kittest::Queryable;
        o.h.get_by_label("1/1");
    }
    // (The editor does not have focus, so `tab.cursor` is not updated; the scroll position shows the jump.)
    let (off, _, ch) = scroll(&mut o.h);
    assert!(off > 0.3 * ch, "scrolled to the match: {off} of {ch}");
    assert_no_blank_bands(&render(&mut o.h), "after find");
    shot(&mut o.h, "open-05-find");
}

// tolerance tuned to the Libertinus preview font
#[cfg(feature = "typst")]
#[test]
fn split_view_scrolls_to_the_bottom_in_sync_right_after_opening() {
    let mut o = open(ViewMode::Split, 100);
    // Wheel over the editor (left half of the central area).
    o.h.hover_at(egui::pos2(420.0, 400.0));
    o.h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -2.0e6),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    frames(&mut o.h, 40);
    let st = &mut o.h.state_mut().app().state;
    let t = st.tabs.active_tab().expect("tab");
    let (e, p) = (&t.editor_scroll, &t.preview_scroll);
    assert!(e.max_offset() > 1000.0 && p.max_offset() > 1000.0, "both panes have a long range: {} {}", e.max_offset(), p.max_offset());
    // (egui's own scroll limit is about 50 px short of content - viewport for this TextEdit, also without placeholders.)
    assert!(e.offset >= e.max_offset() - 60.0, "editor at the bottom: {} of {}", e.offset, e.max_offset());
    assert!(p.offset >= p.max_offset() - 60.0, "preview follows to the bottom: {} of {}", p.offset, p.max_offset());
    let chars = o.text.chars().count();
    assert_eq!(stats(&mut o.h).chars, chars);
    shot(&mut o.h, "open-06-split-bottom");
}

#[test]
fn resize_keeps_the_top_line() {
    let mut o = open(ViewMode::Source, 100);
    let at = o.text.find("Section 200\n").expect("section");
    o.h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(at..at);
    frames(&mut o.h, 8);
    let line = |h: &mut H| {
        let t = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.clone();
        t.map.line_for_y(t.offset)
    };
    let before = line(&mut o.h);
    o.h.set_size(egui::vec2(900.0, 720.0));
    frames(&mut o.h, 8);
    let after = line(&mut o.h);
    assert!((before - after).abs() < 4.0, "same source line at the top after a resize: {before} -> {after}");
    assert_eq!(stats(&mut o.h).chars, o.text.chars().count());
    assert_no_blank_bands(&render(&mut o.h), "after resize");
    shot(&mut o.h, "open-07-resized");
}

#[test]
fn no_wrap_mode_opens_and_ends() {
    let fx = Fx::new();
    let text = note(100);
    let path = fx.notes().join("Big.md");
    std::fs::write(&path, &text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    {
        let st = &mut h.state_mut().app().state;
        st.config.editor_wrap = false;
        st.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Source));
    }
    settle(&mut h);
    let s = stats(&mut h);
    assert_eq!(s.chars, text.chars().count());
    assert!(s.placeholders > 1000);
    h.key_press_modifiers(Modifiers::CTRL, Key::End);
    frames(&mut h, 8);
    assert_eq!(cursor(&mut h), Some(text.len()..text.len()));
    assert_no_blank_bands(&render(&mut h), "no-wrap end");
}

#[test]
fn scrolling_down_lays_out_what_comes_into_view() {
    let mut o = open(ViewMode::Source, 100);
    let p0 = stats(&mut o.h).placeholders;
    o.h.hover_at(egui::pos2(600.0, 400.0));
    for _ in 0..30 {
        o.h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
        frames(&mut o.h, 2);
        assert_no_blank_bands(&render(&mut o.h), "while scrolling");
    }
    let (off, _, _) = scroll(&mut o.h);
    assert!(off > 1500.0, "scrolled: {off}");
    assert!(stats(&mut o.h).placeholders < p0, "paragraphs that came into view were laid out");
}
