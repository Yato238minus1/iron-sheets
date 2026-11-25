//! Middle-click opens in a new tab on search hits, link rows and editor wiki-links; tab menu and free strip space.
use crate::common::*;
use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn setup() -> (Fx, H) {
    // Short frames: a double-click needs both clicks within egui's time window.
    let fx = Fx::new();
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.02);
    let t = Instant::now();
    while st(&mut h).indexer.index().read().len() < 8 {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "index");
        std::thread::sleep(Duration::from_millis(5));
    }
    (fx, h)
}

fn open(h: &mut H, path: std::path::PathBuf) {
    st(h).commands.push(Command::OpenPath { path, new_tab: false, line: None });
    settle(h);
}

fn middle_click(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(h);
}

fn tabs(h: &mut H) -> Vec<String> {
    st(h).tabs.tabs.iter().map(|t| t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect()
}

#[test]
fn middle_click_on_a_search_hit_opens_a_new_tab() {
    let (fx, mut h) = setup();
    open(&mut h, fx.notes().join("Reading.md"));
    st(&mut h).commands.push(Command::GlobalSearch { query: Some("Welcome".into()) });
    let t = Instant::now();
    while h.query_by_label_contains("Welcome.md (").is_none() {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "search rows");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    let p = h.get_by_label_contains("Welcome.md (").rect().center();
    middle_click(&mut h, p);
    assert_eq!(tabs(&mut h), ["Reading.md", "Welcome.md"], "old tab stays, hit opens beside it");
}

#[test]
fn middle_click_on_a_backlink_opens_a_new_tab() {
    let (fx, mut h) = setup();
    open(&mut h, fx.notes().join("Projects").join("Plan.md"));
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Links));
    let t = Instant::now();
    while h.query_by_label("Welcome \u{2014} line 3").is_none() {
        h.step();
        assert!(t.elapsed() < Duration::from_secs(10), "backlinks");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    let p = h.get_by_label("Welcome \u{2014} line 3").rect().center();
    middle_click(&mut h, p);
    assert_eq!(tabs(&mut h), ["Plan.md", "Welcome.md"]);
}

#[test]
fn middle_click_on_an_editor_wiki_link_opens_a_new_tab() {
    let (fx, mut h) = setup();
    let note = fx.notes().join("Linker.md");
    std::fs::write(&note, "[[Ideas]] and more\n").unwrap();
    open(&mut h, note);
    let r = h.get_by_role(Role::MultilineTextInput).rect();
    let p = r.min + egui::vec2(40.0, 10.0);
    // egui-winit sends the PRIMARY selection paste with a middle press on Linux: it must not land in the note.
    h.event(Event::PointerMoved(p));
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Middle, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::MiddleClickPaste { pos: p, text: "PASTED".into() });
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Middle, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(&mut h);
    let t = tabs(&mut h);
    assert_eq!(t, ["Linker.md", "Ideas.md"], "{t:?}");
    let text = st(&mut h).docs.get(&fx.notes().join("Linker.md")).expect("doc").text.clone();
    assert_eq!(text, "[[Ideas]] and more\n", "no paste on a link");
}

#[test]
fn tab_menu_closes_to_the_right_and_free_strip_space_opens_a_tab() {
    let (fx, mut h) = setup();
    for f in ["Welcome.md", "Ideas.md", "Reading.md"] {
        st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join(f), new_tab: true, line: None });
        settle(&mut h);
    }
    assert_eq!(tabs(&mut h).len(), 3);
    // Free space right of the last tab: double-click opens one more tab.
    let last = h.get_by_role_and_label(Role::Tab, "Reading.md").rect();
    let free = egui::pos2(last.right() + 80.0, last.center().y);
    h.event(Event::PointerMoved(free));
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(Event::PointerButton { pos: free, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
            h.step();
        }
    }
    settle(&mut h);
    assert_eq!(tabs(&mut h).len(), 4, "double-click on free space opens a tab");
    // Context menu on the first tab.
    h.get_by_role_and_label(Role::Tab, "Welcome.md").click_secondary();
    settle(&mut h);
    shot(&mut h, "middle_click_tab_menu");
    h.get_by_label("Close tabs to the right").click();
    settle(&mut h);
    assert_eq!(tabs(&mut h), ["Welcome.md"]);
}
