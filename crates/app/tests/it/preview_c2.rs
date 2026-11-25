//! Wave C2: selectable preview text and `![[embed]]` rendering.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton, Pos2, pos2};
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::PathBuf;

type H = Harness<'static, Slot>;

fn open(fx: &Fx, file: &str, text: &str) -> (H, PathBuf) {
    let p = fx.notes().join(file);
    std::fs::write(&p, text).expect("write");
    // Short frames: egui's double-click delay is 0.3 s.
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

fn block_rect(h: &mut H, i: usize) -> egui::Rect {
    prev(h).block_rects.iter().find(|(k, _)| *k == i).map(|(_, r)| *r).unwrap_or_else(|| panic!("block {i} not painted"))
}

fn press(h: &mut H, pos: Pos2, pressed: bool, mods: Modifiers) {
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: mods });
    h.step();
}

fn drag(h: &mut H, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    press(h, from, true, Modifiers::NONE);
    for k in 1..=4 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
        h.step();
    }
    press(h, to, false, Modifiers::NONE);
}

/// Presses Ctrl+C and returns what the app put on the clipboard.
fn copied(h: &mut H) -> Option<String> {
    h.event(Event::Copy);
    h.step();
    h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    })
}

const NOTE: &str = "# Title\n\nFirst paragraph with a [link](https://example.org) and more words.\n\nSecond paragraph, short.\n\nThird one.\n";

#[test]
fn drag_across_blocks_copies_text() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Sel.md", NOTE);
    let (a, b) = (block_rect(&mut h, 1), block_rect(&mut h, 2));
    drag(&mut h, pos2(a.min.x + 2.0, a.center().y), pos2(b.min.x + 60.0, b.center().y));
    shot(&mut h, "c2-selection-drag");
    let t = copied(&mut h).expect("copy output");
    assert!(t.starts_with("First paragraph with a link and more words."), "{t:?}");
    assert!(t.contains("\n\nSecond"), "{t:?}");
    assert!(!t.contains("Third"), "{t:?}");
}

#[test]
fn double_click_selects_a_word_and_click_clears() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Word.md", "# T\n\nalpha beta gamma\n");
    let a = block_rect(&mut h, 1);
    let p = pos2(a.min.x + 2.0, a.center().y);
    h.event(Event::PointerMoved(p));
    h.step();
    for _ in 0..2 {
        press(&mut h, p, true, Modifiers::NONE);
        press(&mut h, p, false, Modifiers::NONE);
    }
    assert_eq!(copied(&mut h).as_deref(), Some("alpha"));
    // A click in the empty area clears it.
    let far = pos2(a.max.x - 2.0, a.max.y + 300.0);
    h.event(Event::PointerMoved(far));
    h.step();
    press(&mut h, far, true, Modifiers::NONE);
    press(&mut h, far, false, Modifiers::NONE);
    assert_eq!(copied(&mut h), None);
}

#[test]
fn edit_clears_selection() {
    let fx = Fx::new();
    let (mut h, p) = open(&fx, "Edit.md", NOTE);
    let (a, b) = (block_rect(&mut h, 1), block_rect(&mut h, 2));
    drag(&mut h, pos2(a.min.x + 2.0, a.center().y), pos2(b.min.x + 60.0, b.center().y));
    assert!(copied(&mut h).is_some());
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p, range: 0..0, insert: "x".into(), select: None });
    settle(&mut h);
    assert_eq!(copied(&mut h), None);
}

#[test]
fn link_click_without_drag_still_opens() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Lnk.md", "# T\n\nSee [[Ideas]] now.\n");
    let r = prev(&mut h).hits.iter().find(|(_, l)| l.starts_with("link:Ideas")).map(|(r, _)| *r).expect("link");
    h.event(Event::PointerMoved(r.center()));
    h.step();
    press(&mut h, r.center(), true, Modifiers::NONE);
    press(&mut h, r.center(), false, Modifiers::NONE);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.active_path(), Some(fx.notes().join("Ideas.md").as_path()));
}

// ---- embeds ----

fn write(fx: &Fx, name: &str, text: &str) {
    std::fs::write(fx.notes().join(name), text).expect("write");
}

/// Steps until `cond` holds (the index scan and image decoding finish in the background), at most about 4 s.
fn wait(h: &mut H, what: &str, cond: impl Fn(&mut H) -> bool) {
    for _ in 0..800 {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timeout: {what}: {:?}", prev(h).hits);
}

fn has_hit(h: &mut H, p: &str) -> bool {
    prev(h).hits.iter().any(|(_, l)| l.starts_with(p))
}

fn setup(fx: &Fx, host: &str) -> H {
    let p = fx.notes().join("Host.md");
    std::fs::write(&p, host).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    settle(&mut h);
    h
}

#[test]
fn image_embed_width() {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy");
    let mut h = setup(&fx, "# T\n\n![[pic.png|32]]\n\n![[pic.png|32x10]]\n\n![[pic.png]]\n");
    wait(&mut h, "image sized", |h| (block_rect(h, 1).height() - 24.0).abs() < 0.5);
    assert!((block_rect(&mut h, 2).height() - 10.0).abs() < 0.5, "{:?}", block_rect(&mut h, 2));
    assert!((block_rect(&mut h, 3).height() - 48.0).abs() < 0.5);
    shot(&mut h, "c2-embed-image");
}

#[test]
fn note_heading_and_block_embeds() {
    let fx = Fx::new();
    write(
        &fx,
        "Src.md",
        "# Src\n\nintro [i](https://intro.test)\n\n## Part\n\nin part [p](https://part.test)\n\n### Sub\n\nin sub [s](https://sub.test)\n\n## Next\n\nafter [n](https://next.test)\n\nlast one ^blk1\n",
    );
    let mut h = setup(&fx, "# T\n\n![[Src#Part]]\n");
    wait(&mut h, "heading embed", |h| has_hit(h, "link:https://part.test"));
    assert!(has_hit(&mut h, "link:https://sub.test"), "subsection is part of the heading's section");
    assert!(!has_hit(&mut h, "link:https://next.test") && !has_hit(&mut h, "link:https://intro.test"));
    shot(&mut h, "c2-embed-heading");

    let fx = Fx::new();
    write(&fx, "Src.md", "# Src\n\nintro [i](https://intro.test)\n\nlast one [b](https://blk.test) ^blk1\n");
    let mut h = setup(&fx, "# T\n\n![[Src#^blk1]]\n\n![[Src]]\n");
    wait(&mut h, "whole note", |h| has_hit(h, "link:https://intro.test"));
    assert!(has_hit(&mut h, "link:https://blk.test"));
}

#[test]
fn embed_cycle_and_depth() {
    let fx = Fx::new();
    write(&fx, "A.md", "# A\n\n![[B]]\n");
    write(&fx, "B.md", "# B\n\n[b](https://b.test)\n\n![[C]]\n");
    write(&fx, "C.md", "# C\n\n[c](https://c.test)\n\n![[D]]\n\n![[A]]\n");
    write(&fx, "D.md", "# D\n\n[d](https://d.test)\n");
    write(&fx, "Self.md", "# S\n\n![[Self]]\n");
    let mut h = setup(&fx, "# T\n\n![[A]]\n\n![[Self]]\n");
    // Host -> A (level 1) -> B (level 2); C would be level 3.
    wait(&mut h, "depth 2", |h| has_hit(h, "link:https://b.test"));
    assert!(!has_hit(&mut h, "link:https://c.test"), "C is the third level");
    shot(&mut h, "c2-embed-depth");
    h.state_mut().app().state.config.embed_max_depth = 3;
    wait(&mut h, "depth 3", |h| has_hit(h, "link:https://c.test"));
    assert!(!has_hit(&mut h, "link:https://d.test"));
    h.state_mut().app().state.config.embed_max_depth = 4;
    wait(&mut h, "depth 4", |h| has_hit(h, "link:https://d.test"));
    // A is embedded in itself through C: drawn as a cycle row, so the page stays finite.
    h.state_mut().app().state.config.embed_max_depth = 8;
    settle(&mut h);
    assert!(prev(&mut h).block_rects.len() < 10);
    // Self.md embeds itself: its heading and one cycle row.
    let r = block_rect(&mut h, 2);
    assert!(r.height() < 160.0, "{r:?}");
    shot(&mut h, "c2-embed-cycle");
}

#[test]
fn embed_modes() {
    let fx = Fx::new();
    write(&fx, "E.md", "# E\n\n[e](https://e.test)\n");
    let mut h = setup(&fx, "# T\n\n![[E]]\n");
    wait(&mut h, "inline", |h| has_hit(h, "link:https://e.test"));
    h.state_mut().app().state.config.embed_mode = "link".into();
    wait(&mut h, "link only", |h| !has_hit(h, "link:https://e.test"));
    assert!(has_hit(&mut h, "link:E"));
    h.state_mut().app().state.config.embed_mode = "collapsed".into();
    wait(&mut h, "collapsed row", |h| has_hit(h, "link:E"));
    assert!(!has_hit(&mut h, "link:https://e.test"));
    shot(&mut h, "c2-embed-collapsed");
    let r = block_rect(&mut h, 1);
    let arrow = pos2(r.min.x + EMBED_ARROW, r.min.y + 14.0);
    h.event(Event::PointerMoved(arrow));
    h.step();
    press(&mut h, arrow, true, Modifiers::NONE);
    press(&mut h, arrow, false, Modifiers::NONE);
    wait(&mut h, "expanded", |h| has_hit(h, "link:https://e.test"));
    shot(&mut h, "c2-embed-expanded");
}

const EMBED_ARROW: f32 = 12.0;

#[test]
fn embed_follows_changes_of_the_target() {
    let fx = Fx::new();
    write(&fx, "E.md", "# E\n\n[old](https://old.test)\n");
    let mut h = setup(&fx, "# T\n\n![[E]]\n");
    wait(&mut h, "old", |h| has_hit(h, "link:https://old.test"));
    write(&fx, "E.md", "# E\n\n[new](https://new.test)\n");
    // The watcher or the stat check of the next layout notices the new file.
    wait(&mut h, "new", |h| has_hit(h, "link:https://new.test"));
}

fn open_more(h: &mut H, fx: &Fx, name: &str) {
    let p = fx.notes().join(name);
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
}

fn click_at(h: &mut H, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    press(h, pos, true, Modifiers::NONE);
    press(h, pos, false, Modifiers::NONE);
    settle(h);
}

#[test]
fn embedded_task_box_never_edits_the_host() {
    let fx = Fx::new();
    write(&fx, "Src.md", "# Src\n\n- [ ] embedded task\n");
    let mut h = setup(&fx, "# T\n\n- [ ] host task\n\n![[Src]]\n");
    wait(&mut h, "tasks", |h| prev(h).hits.iter().filter(|(_, l)| l.starts_with("task:")).count() == 2);
    let tasks: Vec<_> = prev(&mut h).hits.iter().filter(|(_, l)| l.starts_with("task:")).map(|(r, _)| *r).collect();
    let host = fx.notes().join("Host.md");
    let before = h.state_mut().app().state.docs.get(&host).expect("doc").text.clone();
    // The lower box is the embedded one.
    let lower = tasks.iter().max_by(|a, b| a.min.y.total_cmp(&b.min.y)).copied().expect("task");
    click_at(&mut h, lower.center());
    let after = h.state_mut().app().state.docs.get(&host).expect("doc").text.clone();
    assert_eq!(before, after, "a task box in an embed must not edit the host");
    assert!(std::fs::read_to_string(fx.notes().join("Src.md")).expect("read").contains("- [ ] embedded task"));
}

#[test]
fn links_in_an_embed_belong_to_the_embedded_note() {
    let fx = Fx::new();
    write(&fx, "Src.md", "# Src\n\nsee [here](#Part)\n\n## Part\n\nbody\n");
    let mut h = setup(&fx, "# T\n\n![[Src]]\n");
    wait(&mut h, "link", |h| has_hit(h, "link:#Part"));
    let r = prev(&mut h).hits.iter().find(|(_, l)| l.starts_with("link:#Part")).map(|(r, _)| *r).expect("link");
    click_at(&mut h, r.center());
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.active_path(), Some(fx.notes().join("Src.md").as_path()));
}

#[test]
fn embed_follows_an_open_target_note() {
    let fx = Fx::new();
    write(&fx, "E.md", "# E\n\n[old](https://old.test)\n");
    let mut h = setup(&fx, "# T\n\n![[E]]\n");
    wait(&mut h, "old", |h| has_hit(h, "link:https://old.test"));
    // Open E, edit it (unsaved), come back: the embed shows the open text.
    open_more(&mut h, &fx, "E.md");
    let e = fx.notes().join("E.md");
    let st = &mut h.state_mut().app().state;
    let len = st.docs.get(&e).expect("doc").text.len();
    st.commands.push(Command::ApplyEdit { path: e, range: 0..len, insert: "# E\n\n[new](https://new.test)\n".into(), select: None });
    settle(&mut h);
    let host = fx.notes().join("Host.md");
    h.state_mut().app().state.commands.push(Command::OpenPath { path: host, new_tab: false, line: None });
    wait(&mut h, "new", |h| has_hit(h, "link:https://new.test"));
}

#[test]
fn embed_layouts_do_not_leak_between_notes() {
    let fx = Fx::new();
    write(&fx, "X.md", "# X\n\n![[H1]]\n");
    write(&fx, "H1.md", "# H1\n\n[h](https://h1.test)\n\n![[X]]\n");
    write(&fx, "H2.md", "# H2\n\n![[X]]\n");
    let mut h = setup(&fx, "# T\n");
    open_more(&mut h, &fx, "H1.md");
    wait(&mut h, "H1 shows X", |h| has_hit(h, "link:X"));
    // Only H1's own link: H1 -> X -> H1 is a cycle.
    assert_eq!(prev(&mut h).hits.iter().filter(|(_, l)| l.starts_with("link:https://h1.test")).count(), 1);
    open_more(&mut h, &fx, "H2.md");
    wait(&mut h, "H2 shows X and H1", |h| has_hit(h, "link:https://h1.test"));
}

#[test]
fn empty_note_survives_mouse_input() {
    let fx = Fx::new();
    let (mut h, _) = open(&fx, "Empty.md", "");
    let p = pos2(500.0, 200.0);
    click_at(&mut h, p);
    drag(&mut h, p, pos2(600.0, 400.0));
    drag(&mut h, p, pos2(600.0, 900.0));
    for _ in 0..2 {
        press(&mut h, p, true, Modifiers::NONE);
        press(&mut h, p, false, Modifiers::NONE);
    }
    press(&mut h, p, true, Modifiers::SHIFT);
    press(&mut h, p, false, Modifiers::SHIFT);
    assert_eq!(copied(&mut h), None);
}

/// Pressing on a link and dragging selects text from the press point (the link only reacts to a click) and does
/// not follow the link.
// drag target tuned to the Libertinus preview font
#[cfg(feature = "typst")]
#[test]
fn dragging_from_a_link_is_harmless() {
    let fx = Fx::new();
    let (mut h, p) = open(&fx, "Lk.md", "# T\n\nSee [[Ideas]] now.\n\nSecond paragraph.\n");
    let r = prev(&mut h).hits.iter().find(|(_, l)| l.starts_with("link:Ideas")).map(|(r, _)| *r).expect("link");
    let b = block_rect(&mut h, 2);
    drag(&mut h, r.center(), pos2(b.min.x + 40.0, b.center().y));
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.active_path(), Some(p.as_path()), "a drag from a link does not open it");
    let t = copied(&mut h).expect("a selection");
    assert!(t.starts_with("Ideas") && t.contains("Second"), "{t:?}");
}

#[test]
fn drag_autoscroll_counts_as_a_user_scroll() {
    let fx = Fx::new();
    let note: String = (0..120).map(|i| format!("Paragraph {i} of a long note.\n\n")).collect();
    let (mut h, _) = open(&fx, "Long.md", &note);
    let a = block_rect(&mut h, 0);
    let start = pos2(a.min.x + 2.0, a.center().y);
    h.event(Event::PointerMoved(start));
    h.step();
    press(&mut h, start, true, Modifiers::NONE);
    let low = pos2(start.x + 40.0, 712.0);
    let mut flagged = false;
    for k in 0..12 {
        h.event(Event::PointerMoved(low + egui::vec2((k % 2) as f32, 0.0)));
        h.step();
        let t = h.state_mut().app().state.tabs.active_tab().expect("tab");
        flagged |= t.preview_scroll.user_scrolled;
    }
    let off = h.state_mut().app().state.tabs.active_tab().expect("tab").preview_scroll.offset;
    press(&mut h, low, false, Modifiers::NONE);
    assert!(off > 10.0, "the preview scrolled while dragging below it: {off}");
    assert!(flagged, "the autoscroll is reported as a user scroll");
}

fn frame_ms(h: &mut H, n: usize) -> f64 {
    let t = std::time::Instant::now();
    for _ in 0..n {
        h.step();
    }
    t.elapsed().as_secs_f64() * 1000.0 / n as f64
}

/// Prints the steady frame time without and with a selection, and for a note with 20 embeds of a big note.
#[test]
fn perf_numbers() {
    let fx = Fx::new();
    let mut note = String::new();
    for i in 0..1500 {
        note.push_str(&format!("Paragraph {i} with **bold** and some more words to wrap around.\n\n"));
    }
    let mut h = setup(&fx, &note);
    wait(&mut h, "laid out", |h| prev(h).unlaid == 0 && !prev(h).block_rects.is_empty());
    settle(&mut h);
    let plain = frame_ms(&mut h, 60);
    let (a, b) = (block_rect(&mut h, 0), block_rect(&mut h, 3));
    drag(&mut h, pos2(a.min.x + 2.0, a.center().y), pos2(b.min.x + 90.0, b.center().y));
    let with_sel = frame_ms(&mut h, 60);
    eprintln!("C2 perf: steady preview frame, 1500 blocks: no selection {plain:.2} ms, selection over 4 blocks {with_sel:.2} ms");

    let big: String = (0..600).map(|i| format!("Embedded paragraph {i} with words and a [l](https://x.test/{i}).\n\n")).collect();
    write(&fx, "Big.md", &big);
    let host: String = (0..20).map(|i| format!("![[Big]]\n\ntext {i}\n\n")).collect();
    let t = std::time::Instant::now();
    let mut h = setup(&fx, &host);
    wait(&mut h, "embeds", |h| has_hit(h, "link:https://x.test/0"));
    eprintln!("C2 perf: 20 embeds of a 600-block note: first painted after {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    settle(&mut h);
    eprintln!("C2 perf: steady frame with 20 embeds: {:.2} ms", frame_ms(&mut h, 60));
}

const EMBED_PAD: f32 = 8.0;

#[test]
fn embed_text_is_selectable_with_the_host_text() {
    let fx = Fx::new();
    write(&fx, "Src.md", "# Src\n\nembedded alpha beta\n\nsecond embedded para\n");
    let mut h = setup(&fx, "# T\n\nHost words here.\n\n![[Src]]\n\nTail words.\n");
    wait(&mut h, "embed laid out", |h| prev(h).block_rects.iter().any(|(k, r)| *k == 2 && r.height() > 60.0));
    let (host, emb, tail) = (block_rect(&mut h, 1), block_rect(&mut h, 2), block_rect(&mut h, 3));
    // From the host paragraph into the embed's last paragraph: host and embed text are copied together, not the tail.
    drag(&mut h, pos2(host.min.x + 2.0, host.center().y), pos2(emb.max.x - 6.0, emb.max.y - 6.0));
    shot(&mut h, "polish2-embed-selection");
    let t = copied(&mut h).expect("copy output");
    assert!(t.contains("Host words here."), "{t:?}");
    assert!(t.contains("embedded alpha beta") && t.contains("second embedded para"), "{t:?}");
    assert!(!t.contains("Tail"), "{t:?}");
    // From inside the embed to the host's tail.
    drag(&mut h, pos2(emb.min.x + EMBED_PAD, emb.max.y - 10.0), pos2(tail.max.x - 2.0, tail.center().y));
    shot(&mut h, "polish2-embed-selection-down");
    let t = copied(&mut h).expect("copy output");
    assert!(t.contains("Tail words."), "{t:?}");
    assert!(t.contains("second embedded para") && !t.contains("Host words"), "{t:?}");
}
