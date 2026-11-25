//! Mermaid in the preview: last good render on errors, re-render on save, viewers, selectable text and links.
use crate::common::*;
use egui::{CursorIcon, Event, Modifiers, PointerButton, Pos2, Rect};
use egui_kittest::{Harness, kittest::Queryable};
use iron_sheets::command::Command;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

const GOOD: &str = "flowchart LR\n  A[Start] --> B[Docs]\n  B --> C[Ideas]\n  click B \"https://example.com\"\n  class C internal-link;\n";

fn open(save_mode: bool) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, format!("# D\n\n```mermaid\n{GOOD}```\n\nafter\n")).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Dark;
    if save_mode {
        o.config.mermaid_render = "save".into();
        o.config.autosave = false;
    }
    let mut h = harness_dt(o, 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait(&mut h, "diagram painted", |h| hit(h, "diagram").is_some());
    (fx, h, p)
}

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn hit(h: &mut H, label: &str) -> Option<Rect> {
    prev(h).hits.iter().find(|(_, l)| l == label).map(|(r, _)| *r)
}

/// Rect of the k-th text of the diagram (reading order: Start, Docs, Ideas).
fn text_rect(h: &mut H, k: usize) -> Rect {
    prev(h).hits.iter().filter(|(_, l)| l == "diagram-text").nth(k).map(|(r, _)| *r).unwrap_or_else(|| panic!("no text {k}"))
}

fn wait(h: &mut H, what: &str, cond: impl Fn(&mut H) -> bool) {
    for _ in 0..800 {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timeout: {what}");
}

fn hover(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.step();
    h.step();
}

fn click(h: &mut H, p: Pos2) {
    hover(h, p);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
}

fn edit(h: &mut H, p: &std::path::Path, from: &str, to: &str) {
    let d = h.state_mut().app().state.docs.get_mut(p).expect("doc");
    let at = d.text.find(from).expect("text");
    assert!(d.apply_edit(at..at + from.len(), to));
}

/// Steps for about `ms` milliseconds (debounce and background render).
fn run(h: &mut H, ms: u64) {
    for _ in 0..ms / 20 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn syntax_error_keeps_the_last_render_and_shows_the_message_below() {
    let (_fx, mut h, p) = open(false);
    let before = hit(&mut h, "diagram").expect("rect");
    edit(&mut h, &p, "flowchart LR", "flowchrt LR");
    run(&mut h, 1200);
    let after = hit(&mut h, "diagram").expect("still the picture, not the source");
    assert_eq!(before.size(), after.size());
    let block = prev(&mut h).block_rects.iter().find(|(i, _)| *i == 1).map(|(_, r)| *r).expect("block");
    assert!(block.height() > after.height() + 8.0, "error line below the picture: {block:?} {after:?}");
    shot(&mut h, "mermaid-error-keeps-render");
    // Fixed again: back to normal, no message.
    edit(&mut h, &p, "flowchrt LR", "flowchart LR");
    run(&mut h, 1200);
    let block = prev(&mut h).block_rects.iter().find(|(i, _)| *i == 1).map(|(_, r)| *r).expect("block");
    assert!((block.height() - hit(&mut h, "diagram").expect("rect").height()).abs() < 1.0);
}

#[test]
fn on_save_the_render_waits_for_the_save() {
    let (_fx, mut h, p) = open(true);
    let before = hit(&mut h, "diagram").expect("rect");
    assert_eq!(h.state_mut().app().state.config.mermaid_render, "save");
    edit(&mut h, &p, "B --> C[Ideas]", "B --> C[Ideas]\n  C --> D[Four]\n  D --> E[Five]");
    assert!(h.state_mut().app().state.docs.get(&p).is_some_and(|d| d.is_dirty()));
    run(&mut h, 1200);
    assert_eq!(hit(&mut h, "diagram").expect("rect").size(), before.size(), "unsaved: old picture");
    h.state_mut().app().state.commands.push(Command::Save);
    run(&mut h, 1500);
    assert!(hit(&mut h, "diagram").expect("rect").width() > before.width() + 5.0, "saved: new picture");
}

#[test]
fn buttons_open_fullscreen_and_window_with_zoom() {
    let (_fx, mut h, _) = open(false);
    let pic = hit(&mut h, "diagram").expect("rect");
    hover(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    let win = hit(&mut h, "view-window").expect("button");
    assert!(pic.contains(full.center()) && pic.contains(win.center()));
    shot(&mut h, "mermaid-buttons");
    click(&mut h, full.center());
    assert!(prev(&mut h).diagram_full.is_some(), "fullscreen opened");
    run(&mut h, 600);
    let c = h.ctx.content_rect().center();
    let z0 = prev(&mut h).diagram_full.as_ref().expect("open").view.zoom;
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 300.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    });
    hover(&mut h, c);
    for _ in 0..10 {
        h.step();
    }
    let z1 = prev(&mut h).diagram_full.as_ref().expect("open").view.zoom;
    assert!(z1 > z0 * 1.05, "wheel zooms: {z0} -> {z1}");
    run(&mut h, 1500);
    shot(&mut h, "mermaid-fullscreen-zoomed");
    h.key_press(egui::Key::Escape);
    run(&mut h, 100);
    assert!(prev(&mut h).diagram_full.is_none(), "Esc closes");
    // Window (embedded window in the test harness).
    hover(&mut h, pic.center());
    let win = hit(&mut h, "view-window").expect("button");
    click(&mut h, win.center());
    run(&mut h, 600);
    assert!(prev(&mut h).diagram_win.is_some(), "window opened");
    shot(&mut h, "mermaid-window");
}

#[test]
fn text_is_selectable_and_links_work() {
    let (_fx, mut h, _) = open(false);
    let (a, b) = (text_rect(&mut h, 0), text_rect(&mut h, 2));
    // Drag from the first label to the last: all three labels are selected and copied in reading order.
    h.event(Event::PointerMoved(a.left_center() + egui::vec2(1.0, 0.0)));
    h.step();
    h.event(Event::PointerButton { pos: a.left_center() + egui::vec2(1.0, 0.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=6 {
        h.event(Event::PointerMoved(a.left_center().lerp(b.right_center() - egui::vec2(1.0, 0.0), k as f32 / 6.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: b.right_center(), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    shot(&mut h, "mermaid-text-selected");
    h.event(Event::Copy);
    h.step();
    let t = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    let t = t.expect("copied");
    assert!(t.contains("Start") && t.contains("Docs") && t.contains("Ideas"), "{t:?}");
    assert!(t.find("Start") < t.find("Docs") && t.find("Docs") < t.find("Ideas"));
    // Pointer: hand over a linked node, text cursor over a plain one.
    let (docs, start) = (text_rect(&mut h, 1), text_rect(&mut h, 0));
    hover(&mut h, docs.center());
    assert_eq!(h.output().platform_output.cursor_icon, CursorIcon::PointingHand);
    hover(&mut h, start.center());
    assert_eq!(h.output().platform_output.cursor_icon, CursorIcon::Text);
    // `class C internal-link` opens the note named by the label.
    let ideas = text_rect(&mut h, 2);
    click(&mut h, ideas.center());
    run(&mut h, 400);
    let active = h.state_mut().app().state.tabs.active_path().map(|p| p.to_path_buf());
    assert_eq!(active.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).as_deref(), Some("Ideas.md"));
}

#[test]
fn text_selection_works_in_the_fullscreen_viewer() {
    let (_fx, mut h, _) = open(false);
    let pic = hit(&mut h, "diagram").expect("rect");
    hover(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    click(&mut h, full.center());
    run(&mut h, 300);
    shot(&mut h, "mermaid-fullscreen");
    // The zoom buttons sit at the top-right beside the close button, never over the menu bar text at the left.
    let zin = h.get_by_label("Zoom in").rect();
    assert!(zin.min.x > h.ctx.content_rect().center().x && zin.max.x < h.ctx.content_rect().right() - 48.0, "{zin:?}");
    let z0 = prev(&mut h).diagram_full.as_ref().expect("open").view.zoom;
    h.get_by_label("Zoom in").click();
    run(&mut h, 100);
    assert!(prev(&mut h).diagram_full.as_ref().expect("open").view.zoom > z0, "zoom in button works");
    h.get_by_label("Reset").click();
    run(&mut h, 100);
    assert!(prev(&mut h).diagram_full.as_ref().expect("open").selected().is_empty());
    let first = prev(&mut h).diagram_full.as_ref().expect("open").text_rects()[0];
    click(&mut h, first.center());
    click(&mut h, first.center());
    let sel = prev(&mut h).diagram_full.as_ref().expect("open").selected();
    assert_eq!(sel, "Start");
    shot(&mut h, "mermaid-fullscreen-selection");
    h.event(Event::Copy);
    h.step();
    let t = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(t.as_deref(), Some("Start"));
}

#[test]
fn on_save_holds_through_a_line_shift_above_the_diagram() {
    let (_fx, mut h, p) = open(true);
    let before = hit(&mut h, "diagram").expect("rect");
    edit(&mut h, &p, "# D\n", "# D\n\nnew paragraph\n\nanother one\n");
    edit(&mut h, &p, "B --> C[Ideas]", "B --> C[Ideas]\n  C --> D[Four]\n  D --> E[Five]");
    run(&mut h, 1200);
    assert_eq!(hit(&mut h, "diagram").expect("rect").size(), before.size(), "unsaved source is not rendered");
}

#[test]
fn an_open_viewer_follows_edits_of_its_diagram() {
    let (_fx, mut h, p) = open(false);
    let pic = hit(&mut h, "diagram").expect("rect");
    hover(&mut h, pic.center());
    let win = hit(&mut h, "view-window").expect("button");
    click(&mut h, win.center());
    run(&mut h, 300);
    edit(&mut h, &p, "B --> C[Ideas]", "B --> C[Ideas]\n  C --> D[Four]");
    run(&mut h, 1500);
    assert!(prev(&mut h).diagram_win.is_some(), "still open");
    shot(&mut h, "mermaid-window-after-edit");
    // The preview shows the new picture; the window has no "Loading..." left (its picture is the slot's current one).
    assert!(hit(&mut h, "diagram").expect("rect").width() > pic.width() + 5.0);
}

#[test]
fn a_settled_preview_with_diagrams_requests_no_repaint() {
    let fx = Fx::new();
    let p = fx.notes().join("M2.md");
    // Two diagrams with the same source, one with an error, one edited later: every state must settle.
    std::fs::write(&p, format!("# D\n\n```mermaid\n{GOOD}```\n\n```mermaid\n{GOOD}```\n\n```mermaid\nnot a diagram\n```\n")).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait(&mut h, "diagrams painted", |h| prev(h).hits.iter().filter(|(_, l)| l == "diagram").count() == 2);
    edit(&mut h, &p, "A[Start]", "A[Begin]");
    run(&mut h, 1500);
    // Quiet: no frame wants another one (a delayed request would show up after waiting for it).
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(120));
        h.step();
    }
    assert!(!h.ctx.has_requested_repaint(), "still repainting: {:?}", h.ctx.repaint_causes());
}

#[test]
fn the_whole_node_shape_is_a_link_not_only_its_text() {
    let (_fx, mut h, _) = open(false);
    let nodes: Vec<Rect> = prev(&mut h).hits.iter().filter(|(_, l)| l == "diagram-node").map(|(r, _)| *r).collect();
    assert_eq!(nodes.len(), 2, "the two linked nodes (Docs, Ideas) have a shape: {nodes:?}");
    let (docs, ideas) = (nodes[0], nodes[1]);
    let (dt, it) = (text_rect(&mut h, 1), text_rect(&mut h, 2));
    assert!(docs.contains_rect(dt) && docs.width() > dt.width() + 4.0);
    // Near the node's edge, clear of its text: hand cursor, shown with the tooltip's link; a plain node's edge is not a link.
    let edge = |n: Rect| n.left_center() + egui::vec2(3.0, 0.0);
    assert!(!dt.contains(edge(docs)) && !it.contains(edge(ideas)));
    hover(&mut h, edge(docs));
    assert_eq!(h.output().platform_output.cursor_icon, CursorIcon::PointingHand);
    shot(&mut h, "mermaid-node-hover");
    let start = hit(&mut h, "diagram-text").expect("text");
    assert!(!nodes.iter().any(|n| n.contains(start.center())));
    // Clicking the shape of the `internal-link` node opens its note.
    click(&mut h, edge(ideas));
    run(&mut h, 400);
    let active = h.state_mut().app().state.tabs.active_path().map(|p| p.to_path_buf());
    assert_eq!(active.and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).as_deref(), Some("Ideas.md"));
}

#[test]
fn node_shape_is_a_link_in_the_fullscreen_viewer_too() {
    let (_fx, mut h, _) = open(false);
    let pic = hit(&mut h, "diagram").expect("rect");
    hover(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    click(&mut h, full.center());
    run(&mut h, 300);
    let t = prev(&mut h).diagram_full.as_ref().expect("open").text_rects()[1];
    // Left of the "Docs" text but inside its node (the text rects carry a 2 px margin).
    hover(&mut h, t.left_center() - egui::vec2(40.0, 0.0));
    assert_eq!(h.output().platform_output.cursor_icon, CursorIcon::PointingHand);
    // Far from any node: not a link.
    hover(&mut h, egui::pos2(t.center().x, t.center().y + 90.0));
    assert_ne!(h.output().platform_output.cursor_icon, CursorIcon::PointingHand);
    shot(&mut h, "mermaid-fullscreen-node-hover");
}

#[test]
fn a_diagram_edited_and_reverted_shows_its_picture_again() {
    let (_fx, mut h, p) = open(false);
    // Edited and rendered: the original picture is released.
    edit(&mut h, &p, "C[Ideas]", "C[Idea]");
    run(&mut h, 1500);
    assert!(hit(&mut h, "diagram").is_some() && hit(&mut h, "diagram-blank").is_none());
    // Back to the original text: its layout is still cached, its picture must come back too.
    edit(&mut h, &p, "C[Idea]", "C[Ideas]");
    run(&mut h, 1500);
    assert!(hit(&mut h, "diagram").is_some() && hit(&mut h, "diagram-blank").is_none(), "empty box after revert");
    // Edited and reverted within the debounce: the original picture stays and the edit's render is not kept.
    edit(&mut h, &p, "C[Ideas]", "C[Idea]");
    h.step();
    edit(&mut h, &p, "C[Idea]", "C[Ideas]");
    run(&mut h, 1500);
    assert!(hit(&mut h, "diagram").is_some() && hit(&mut h, "diagram-blank").is_none());
}

#[test]
fn editing_one_of_two_identical_diagrams_never_blanks_the_other() {
    let (_fx, mut h, p) = open(false);
    // A copy of the rendered diagram is pasted below it: its layout comes from the cache.
    edit(&mut h, &p, "after", &format!("```mermaid\n{GOOD}```"));
    run(&mut h, 600);
    assert_eq!(prev(&mut h).hits.iter().filter(|(_, l)| l == "diagram").count(), 2);
    // The first one is edited: the copy keeps its picture throughout.
    edit(&mut h, &p, "A[Start]", "A[Begin]");
    for _ in 0..75 {
        h.step();
        assert!(hit(&mut h, "diagram-blank").is_none(), "a diagram painted as an empty box");
        std::thread::sleep(Duration::from_millis(20));
    }
}
