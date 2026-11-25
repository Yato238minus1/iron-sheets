//! Search, outline and links keyboard + empty states (IS-24/25/34), inline display-math errors (IS-18).
use crate::common::*;
use egui::{Event, Key};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_app_core::sidebar::links_model::LinkRow;
use iron_sheets_app_core::sidebar::search_model::SearchRow;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn wait(h: &mut H, what: &str, f: impl Fn(&mut AppState) -> bool) {
    let t = Instant::now();
    loop {
        h.step();
        if f(st(h)) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ready(fx: &Fx) -> H {
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    h
}

fn view(h: &mut H, v: SidebarView) {
    st(h).commands.push(Command::SetSidebarView(v));
    settle(h);
}

fn open(h: &mut H, p: std::path::PathBuf) {
    st(h).commands.push(Command::OpenPath { path: p, new_tab: false, line: None });
    settle(h);
}

fn typ(h: &mut H, text: &str) {
    h.event(Event::Text(text.into()));
    settle(h);
}

fn cursor_line(h: &mut H) -> usize {
    let s = st(h);
    let path = s.tabs.active_path().map(|p| p.to_path_buf()).unwrap_or_default();
    let text = s.docs.get(&path).map(|d| d.text.clone()).unwrap_or_default();
    let start = s.tabs.active_tab().and_then(|t| t.cursor.clone()).map_or(0, |c| c.start);
    text[..start].matches('\n').count()
}

#[test]
fn search_no_results_and_empty_hint() {
    let fx = Fx::new();
    let mut h = ready(&fx);
    view(&mut h, SidebarView::Search);
    h.get_by_label_contains("Type to search all notes");
    typ(&mut h, "zzzqqq");
    wait(&mut h, "search finished", |s| s.ui.search.status_finished());
    settle(&mut h);
    h.get_by_label("No results for 'zzzqqq'");
    shot(&mut h, "ux-w3-01-search-no-results");
}

#[test]
fn search_preselects_first_hit_and_keyboard_opens_second() {
    let fx = Fx::new();
    let mut h = ready(&fx);
    view(&mut h, SidebarView::Search);
    typ(&mut h, "plan");
    wait(&mut h, "search finished", |s| s.ui.search.status_finished());
    settle(&mut h);
    let (first, second) = {
        let m = &st(&mut h).ui.search;
        let hits: Vec<usize> = m.rows().iter().enumerate().filter(|(_, r)| matches!(r, SearchRow::Hit(..))).map(|(i, _)| i).collect();
        assert!(hits.len() >= 3, "hits {hits:?}");
        assert_eq!(m.selected(), Some(hits[0]), "first hit preselected");
        let path = |row: usize| match m.rows()[row] {
            SearchRow::Hit(g, _) | SearchRow::File(g) => m.group(g).path.clone(),
        };
        (path(hits[0]), path(hits[1]))
    };
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    shot(&mut h, "ux-w3-02-search-keyboard");
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(second.clone()));
    let _ = first;
}

#[test]
fn outline_keyboard_moves_cursor_to_heading() {
    let fx = Fx::new();
    let mut h = ready(&fx);
    open(&mut h, fx.notes().join("Reading.md"));
    view(&mut h, SidebarView::Outline);
    // Focus the list (what Tab does), then Down + Enter jumps to "Books" (line 2) and leaves the text alone.
    h.ctx.memory_mut(|m| m.request_focus(egui::Id::NULL.with("outline_keys")));
    h.step();
    let before = st(&mut h).docs.get(&fx.notes().join("Reading.md")).map(|d| d.text.clone());
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    shot(&mut h, "ux-w3-03-outline-keyboard");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(st(&mut h).docs.get(&fx.notes().join("Reading.md")).map(|d| d.text.clone()), before, "Enter did not edit the note");
    assert_eq!(cursor_line(&mut h), 2, "cursor on the Books heading");
}

#[test]
fn links_keyboard_opens_target() {
    let fx = Fx::new();
    let mut h = ready(&fx);
    open(&mut h, fx.notes().join("Welcome.md"));
    view(&mut h, SidebarView::Links);
    wait(&mut h, "links rows", |s| s.ui.links.model.rows().iter().any(|r| matches!(r, LinkRow::Back { .. })));
    settle(&mut h);
    // Open Ideas through its backlink row; Down + Enter then opens the first selectable row of Ideas' links.
    h.get_by_label("Ideas \u{2014} line 3").click();
    settle(&mut h);
    wait(&mut h, "links of Ideas", |s| {
        s.tabs.active_path().is_some_and(|p| p.ends_with("Ideas.md")) && !s.ui.links.model.is_pending() && s.ui.links.model.rows().iter().any(|r| matches!(r, LinkRow::Back { .. }))
    });
    settle(&mut h);
    let expected = {
        let m = &st(&mut h).ui.links.model;
        match m.step_sel(None, 1).and_then(|i| m.rows()[i].command(None, false)) {
            Some(Command::OpenPath { path, .. }) => path,
            other => panic!("first selectable row: {other:?}"),
        }
    };
    // Opening a note hands focus to its editor; give the list focus again (what Tab does).
    h.ctx.memory_mut(|m| m.request_focus(egui::Id::NULL.with("links_keys")));
    h.step();
    shot(&mut h, "ux-w3-04-links");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(expected));
}

#[cfg(feature = "typst")]
#[test]
fn display_math_error_shows_message_inline() {
    use iron_sheets_core::config::{MathSyntax, ViewMode};
    let fx = Fx::new();
    let p = fx.notes().join("Broken.md");
    std::fs::write(&p, "# Broken\n\nBefore.\n\n$$ #nofunc(1) $$\n\nAfter.\n").expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.math_syntax = MathSyntax::Typst;
    let mut h = harness(o);
    st(&mut h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Preview));
    settle(&mut h);
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(8) {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    settle(&mut h);
    shot(&mut h, "ux-w3-05-math-error");
}
