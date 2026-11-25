use crate::common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

/// Step frames until `f` holds (or 10 s pass).
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

fn ready(opts: iron_sheets::AppOptions) -> H {
    let mut h = harness(opts);
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    h
}

fn typ(h: &mut H, text: &str) {
    h.event(Event::Text(text.into()));
    settle(h);
}

fn active_name(h: &mut H) -> Option<String> {
    st(h).tabs.active_path().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())
}

fn view(h: &mut H, v: SidebarView) {
    st(h).commands.push(Command::SetSidebarView(v));
    settle(h);
}

fn open(h: &mut H, p: std::path::PathBuf) {
    st(h).commands.push(Command::OpenPath { path: p, new_tab: false, line: None });
    settle(h);
}

#[test]
fn quick_switcher_opens_note_by_partial_name() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes(), fx.journal()]));
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    assert!(st(&mut h).palette.open);
    shot(&mut h, "panels-01-palette-empty");
    typ(&mut h, "ta");
    typ(&mut h, "sks");
    shot(&mut h, "panels-02-palette-query");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(active_name(&mut h).as_deref(), Some("Tasks.md"));
    assert!(!st(&mut h).palette.open);
}

#[test]
fn quick_switcher_create_row_creates_note() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes()]));
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    typ(&mut h, "Brand New Idea");
    h.get_by_label_contains("Create note 'Brand New Idea'");
    shot(&mut h, "panels-03-palette-create");
    h.key_press(Key::ArrowUp); // wraps to the last row: the create row
    h.key_press(Key::Enter);
    settle(&mut h);
    wait(&mut h, "created note", |s| s.tabs.active_path().is_some_and(|p| p.file_stem().is_some_and(|n| n == "Brand New Idea")));
}

#[test]
fn command_palette_runs_command() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes()]));
    let before = st(&mut h).config.sidebar_visible;
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    settle(&mut h);
    typ(&mut h, "toggle sidebar");
    shot(&mut h, "panels-04-command-palette");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_ne!(st(&mut h).config.sidebar_visible, before);
}

#[test]
fn palette_prefix_switches_to_commands() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes()]));
    h.key_press_modifiers(Modifiers::COMMAND, Key::O);
    settle(&mut h);
    typ(&mut h, ">toggle sidebar");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(!st(&mut h).palette.open);
}

#[test]
fn fuzzy_rank_10k_notes_is_fast() {
    let items: Vec<String> = (0..10_000).map(|i| format!("folder{}/sub{}/Note number {i} about topic{}.md", i % 50, i % 7, i % 13)).collect();
    let mut worst = 0.0f64;
    for q in ["note", "nn7", "fld3 topic1", "zzz"] {
        let t = Instant::now();
        let n = iron_sheets_core::fuzzy::fuzzy_rank(q, items.iter().map(|s| ((), s.as_str())), 100).len();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("fuzzy_rank 10k notes, query {q:?}: {ms:.2} ms ({n} results)");
        worst = worst.max(ms);
    }
    let per = worst;
    assert!(per < 200.0, "ranking far too slow");
}

#[test]
fn search_finds_word_in_several_files_and_click_opens_hit() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes(), fx.journal()]));
    view(&mut h, SidebarView::Search);
    typ(&mut h, "plan");
    wait(&mut h, "search finished", |s| s.ui.search.status_finished());
    settle(&mut h);
    shot(&mut h, "panels-05-search");
    let (hits, files) = st(&mut h).ui.search.counts();
    assert!(hits >= 5 && files >= 4, "hits {hits} files {files}");
    h.get_by_label_contains("hits in");
    // Tasks.md line 6 (1-based): "See [[Plan]]."
    h.get_by_label("6: See [[Plan]].").click();
    settle(&mut h);
    assert_eq!(active_name(&mut h).as_deref(), Some("Tasks.md"));
    wait(&mut h, "cursor", |s| s.tabs.active_tab().is_some_and(|t| t.cursor.is_some()));
    let s = st(&mut h);
    let path = s.tabs.active_path().map(|p| p.to_path_buf()).unwrap_or_default();
    let text = s.docs.get(&path).map(|d| d.text.clone()).unwrap_or_default();
    let start = s.tabs.active_tab().and_then(|t| t.cursor.clone()).map_or(0, |c| c.start);
    assert_eq!(text[..start].matches('\n').count(), 5, "cursor on line index 5");
}

#[test]
fn search_empty_query_clears_and_toggle_works() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::GlobalSearch { query: Some("Older".into()) });
    settle(&mut h);
    wait(&mut h, "search finished", |s| s.ui.search.status_finished());
    assert_eq!(st(&mut h).ui.search.counts(), (2, 1));
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.key_press(Key::Backspace);
    settle(&mut h);
    assert_eq!(st(&mut h).ui.search.counts(), (0, 0));
    assert!(st(&mut h).ui.search.query.is_empty());
}

#[test]
fn outline_lists_headings_and_click_scrolls() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes()]));
    open(&mut h, fx.notes().join("Reading.md"));
    view(&mut h, SidebarView::Outline);
    h.get_by_label("Reading");
    h.get_by_label("Books");
    shot(&mut h, "panels-06-outline");
    h.get_by_label("Papers").click();
    settle(&mut h);
    wait(&mut h, "cursor", |s| s.tabs.active_tab().and_then(|t| t.cursor.clone()).is_some_and(|c| c.start > 0));
    let s = st(&mut h);
    let path = s.tabs.active_path().map(|p| p.to_path_buf()).unwrap_or_default();
    let text = s.docs.get(&path).map(|d| d.text.clone()).unwrap_or_default();
    let start = s.tabs.active_tab().and_then(|t| t.cursor.clone()).map_or(0, |c| c.start);
    assert!(text[start..].starts_with("## Papers"));
    settle(&mut h);
    shot(&mut h, "panels-07-outline-cursor");
}

#[test]
fn links_panel_lists_backlinks_outgoing_and_tags() {
    let fx = Fx::new();
    let mut h = ready(fx.opts(vec![fx.notes(), fx.journal()]));
    open(&mut h, fx.notes().join("Projects").join("Plan.md"));
    view(&mut h, SidebarView::Links);
    let t = Instant::now();
    loop {
        h.step();
        if h.query_by_label_contains("Tasks \u{2014} line").is_some() {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(10), "backlinks never appeared");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.get_by_label("Welcome \u{2014} line 3");
    h.get_by_label("Old \u{2014} line 3");
    h.get_by_label("-> Welcome \u{2014} line 3");
    h.get_by_label_contains("Backlinks (");
    h.get_by_label_contains("Outgoing (1)");
    shot(&mut h, "panels-08-links");
}
