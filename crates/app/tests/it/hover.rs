//! Preview pointer shapes, the code-block Copy button and the page preview popup.
use crate::common::*;
use egui::{CursorIcon, Event, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::PathBuf;

type H = Harness<'static, Slot>;

fn open(fx: &Fx, file: &str, text: &str) -> (H, PathBuf) {
    let p = fx.notes().join(file);
    std::fs::write(&p, text).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    settle(&mut h);
    (h, p)
}

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn hit(h: &mut H, prefix: &str) -> egui::Rect {
    prev(h).hits.iter().find(|(_, l)| l.starts_with(prefix)).map(|(r, _)| *r).unwrap_or_else(|| panic!("no hit {prefix}: {:?}", prev(h).hits))
}

fn block_rect(h: &mut H, i: usize) -> egui::Rect {
    prev(h).block_rects.iter().find(|(k, _)| *k == i).map(|(_, r)| *r).unwrap_or_else(|| panic!("block {i} not painted"))
}

fn hover(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.step();
    h.step();
}

fn cursor(h: &H) -> CursorIcon {
    h.output().platform_output.cursor_icon
}

#[test]
fn pointer_shape_follows_content() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Ptr.md", "# T\n\nplain words and a [[Ideas]] link\n\n- [ ] task\n\n```rust\nlet x = 1;\n```\n");
    let b = block_rect(&mut h, 1);
    hover(&mut h, egui::pos2(b.min.x + 3.0, b.center().y));
    assert_eq!(cursor(&h), CursorIcon::Text);
    let l = hit(&mut h, "link:Ideas");
    hover(&mut h, l.center());
    assert_eq!(cursor(&h), CursorIcon::PointingHand);
    let t = hit(&mut h, "task:");
    hover(&mut h, t.center());
    assert_eq!(cursor(&h), CursorIcon::PointingHand);
    hover(&mut h, egui::pos2(b.max.x - 2.0, b.max.y + 400.0));
    assert_eq!(cursor(&h), CursorIcon::Default);
}

#[test]
fn code_block_copy_button() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Code.md", "# T\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n");
    let c = block_rect(&mut h, 1);
    hover(&mut h, c.center());
    shot(&mut h, "hover-code-copy");
    let btn = hit(&mut h, "code-copy");
    hover(&mut h, btn.center());
    assert_eq!(cursor(&h), CursorIcon::PointingHand);
    h.event(Event::PointerButton { pos: btn.center(), button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: btn.center(), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    let text = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(text.as_deref(), Some("let x = 1;\nlet y = 2;"));
    h.step();
    shot(&mut h, "hover-code-copied");
}

// ---- page preview popup ----

fn write(fx: &Fx, name: &str, text: &str) {
    std::fs::write(fx.notes().join(name), text).expect("write");
}

/// Steps with the given modifiers held until `cond` holds (the note is read in the background), at most about 4 s.
fn step_until(h: &mut H, mods: egui::Modifiers, what: &str, cond: impl Fn(&mut H) -> bool) {
    h.event(Event::ModifiersChanged(mods));
    for _ in 0..800 {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timeout: {what}");
}

fn popup_open(h: &mut H) -> bool {
    h.state_mut().app().state.ui.page_popup.is_open()
}

const TARGET: &str = "# Target\n\nIntro of the target note.\n\n```\nline\n\n```\n\n## Filler\n\n- a\n- b\n- c\n- d\n- e\n- f\n- g\n- h\n- i\n- j\n- k\n- l\n- m\n- n\n- o\n- p\n\n# Second\n\nText below the second heading.\n\n- z1\n- z2\n- z3\n- z4\n- z5\n- z6\n- z7\n- z8\n- z9\n- z10\n- z11\n- z12\n- z13\n- z14\n- z15\n- z16\n- z17\n- z18\n- z19\n- z20\n- z21\n- z22\n- z23\n- z24\n- z25\n";

#[test]
fn ctrl_hover_shows_page_preview() {
    let fx = Fx::new();
    write(&fx, "Target.md", TARGET);
    let (mut h, _) = open(&fx, "Host.md", "# Host\n\nGo to [[Target#Second]] or [[Nowhere]] now.\n");
    let l = hit(&mut h, "link:");
    // Without Ctrl nothing opens.
    hover(&mut h, l.center());
    for _ in 0..5 {
        h.step();
    }
    assert!(!popup_open(&mut h));
    // With Ctrl it opens and shows the note, scrolled to the heading.
    h.event(Event::PointerMoved(l.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup", popup_open);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    shot(&mut h, "hover-page-preview");
    // The pointer in the popup keeps it open (Ctrl released); leaving closes it.
    let sc = h.state_mut().app().state.ui.page_popup.scroll_offset();
    assert!(sc > 0.0, "scrolled to the heading: {sc}");
    h.event(Event::PointerMoved(egui::pos2(l.min.x + 20.0, l.max.y + 120.0)));
    for _ in 0..10 {
        h.step();
    }
    assert!(popup_open(&mut h));
    h.event(Event::PointerMoved(egui::pos2(900.0, 650.0)));
    step_until(&mut h, egui::Modifiers::NONE, "close", |h| !popup_open(h));
    assert_eq!(h.state_mut().app().state.ui.page_popup.scroll_offset(), 0.0, "next note starts at the top");
}

#[test]
fn missing_target_and_off_setting() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Host2.md", "# Host\n\nGo to [[Nowhere]] now.\n");
    let l = hit(&mut h, "link:");
    h.event(Event::PointerMoved(l.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup", popup_open);
    for _ in 0..5 {
        h.step();
    }
    shot(&mut h, "hover-page-preview-missing");
    h.get_by_label_contains("Note not found");
    // Esc closes it.
    h.event(Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
    h.step();
    assert!(!popup_open(&mut h));
    // Off: never.
    h.state_mut().app().state.config.page_preview = "off".into();
    h.event(Event::PointerMoved(egui::pos2(900.0, 650.0)));
    h.step();
    h.event(Event::PointerMoved(l.center()));
    for _ in 0..10 {
        h.step();
    }
    assert!(!popup_open(&mut h));
}

#[test]
fn editor_wiki_link_shows_page_preview() {
    let fx = Fx::new();
    write(&fx, "Target.md", TARGET);
    let (mut h, _) = open(&fx, "Src.md", "[[Target]] first line\n");
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);
    // First character of the first line: the editor text starts at the top-left of the pane.
    let p = egui::pos2(300.0, 92.0);
    h.event(Event::PointerMoved(p));
    step_until(&mut h, egui::Modifiers::COMMAND, "editor popup", popup_open);
    shot(&mut h, "hover-page-preview-editor");
}

#[test]
fn heading_jump_lands_in_a_long_note() {
    let fx = Fx::new();
    let mut t = String::from("# Long\n\n");
    for i in 0..400 {
        t.push_str(&format!("Paragraph {i} {}\n\n", "words ".repeat(1 + (i * 7) % 60)));
    }
    t.push_str("# Deep\n\n");
    for i in 0..150 {
        t.push_str(&format!("After {i} {}\n\n", "tail ".repeat(1 + (i * 5) % 40)));
    }
    write(&fx, "Long.md", &t);
    let (mut h, _) = open(&fx, "HostL.md", "# Host\n\nGo to [[Long#Deep]] now.\n");
    let l = hit(&mut h, "link:");
    h.event(Event::PointerMoved(l.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup", popup_open);
    for _ in 0..400 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    shot(&mut h, "hover-page-preview-long");
    let pop = &h.state_mut().app().state.ui.page_popup;
    let (pv, rect) = pop.preview_rect();
    let rect = rect.expect("open");
    let deep = pv.block_rects.iter().find(|(i, _)| *i == 401).map(|(_, r)| *r).expect("heading painted");
    assert!(deep.min.y - rect.min.y < 60.0 && deep.min.y >= rect.min.y, "heading at the top: {deep:?} in {rect:?}");
}

// ---- nested page previews and links inside the popup ----

fn nested_fixture(fx: &Fx) -> (H, egui::Rect) {
    write(fx, "A.md", "# A\n\nNote A links to [[B]] here.\n");
    write(fx, "B.md", "# B\n\nBody of note B, see [[C]].\n");
    write(fx, "C.md", "# C\n\nBody of note C.\n");
    let (mut h, _) = open(fx, "Host3.md", "# Host\n\nGo to [[A]] now.\n");
    let l = hit(&mut h, "link:");
    h.event(Event::PointerMoved(l.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup A", popup_open);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (h, l)
}

fn child_open(h: &mut H) -> bool {
    h.state_mut().app().state.ui.page_popup.child().is_some_and(|c| c.is_open())
}

/// Screen rect of the first link inside the top popup.
fn popup_link(h: &mut H) -> egui::Rect {
    let p = &h.state_mut().app().state.ui.page_popup;
    p.preview_rect().0.hits.iter().find(|(_, l)| l.starts_with("link:B")).map(|(r, _)| *r).expect("link B in popup A")
}

#[test]
fn nested_popup_stacks_and_esc_closes_top_only() {
    let fx = Fx::new();
    let (mut h, _) = nested_fixture(&fx);
    let lb = popup_link(&mut h);
    h.event(Event::PointerMoved(lb.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup B", child_open);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(popup_open(&mut h), "parent stays open");
    shot(&mut h, "hover-page-preview-nested");
    // Pointer into the child keeps both.
    let r = h.state_mut().app().state.ui.page_popup.child().and_then(|c| c.preview_rect().1).expect("child rect");
    h.event(Event::PointerMoved(r.center()));
    for _ in 0..40 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(popup_open(&mut h) && child_open(&mut h));
    // Esc closes only the top one.
    h.event(Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    // Back onto A (where the child was may be outside A).
    let a = h.state_mut().app().state.ui.page_popup.preview_rect().1.expect("parent rect");
    h.event(Event::PointerMoved(a.min + egui::vec2(20.0, 20.0)));
    h.step();
    assert!(!child_open(&mut h));
    assert!(popup_open(&mut h));
    // Leaving everything closes all.
    h.event(Event::PointerMoved(egui::pos2(1000.0, 700.0)));
    step_until(&mut h, egui::Modifiers::NONE, "close", |h| !popup_open(h));
}

fn click(h: &mut H, p: Pos2, mods: egui::Modifiers) {
    h.event(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: mods });
        h.step();
    }
    settle(h);
}

fn active_is(h: &mut H, name: &str) -> bool {
    h.state_mut().app().state.tabs.active_path().is_some_and(|p| p.file_name().is_some_and(|n| n == name))
}

#[test]
fn click_link_in_popup_follows_it() {
    for mods in [egui::Modifiers::NONE, egui::Modifiers::COMMAND] {
        let fx = Fx::new();
        let (mut h, _) = nested_fixture(&fx);
        let lb = popup_link(&mut h);
        click(&mut h, lb.center(), mods);
        assert!(active_is(&mut h, "B.md"), "link followed with {mods:?}");
        assert!(!popup_open(&mut h), "popup closed after following");
    }
}

#[test]
fn click_link_in_nested_popup_closes_the_stack() {
    let fx = Fx::new();
    let (mut h, _) = nested_fixture(&fx);
    let lb = popup_link(&mut h);
    h.event(Event::PointerMoved(lb.center()));
    step_until(&mut h, egui::Modifiers::COMMAND, "popup B", child_open);
    for _ in 0..30 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let lc = {
        let c = h.state_mut().app().state.ui.page_popup.child().expect("child");
        c.preview_rect().0.hits.iter().find(|(_, l)| l.starts_with("link:C")).map(|(r, _)| *r).expect("link C in popup B")
    };
    click(&mut h, lc.center(), egui::Modifiers::NONE);
    assert!(active_is(&mut h, "C.md"), "link in the child followed");
    assert!(!popup_open(&mut h), "whole stack closed");
}
