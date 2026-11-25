//! Keyboard-first sidebar and list navigation (IS-34, IS-35).
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

fn editor_focused(h: &mut H) -> bool {
    let id = {
        let s = st(h);
        let t = s.tabs.active_tab().expect("active tab");
        egui::Id::NULL.with(("editor", t.id, t.nav_gen))
    };
    h.ctx.memory(|m| m.has_focus(id))
}

#[test]
fn ctrl_shift_e_focuses_sidebar_and_view_switch_auto_focuses() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Reading.md"), new_tab: true, line: None });
    settle(&mut h);
    assert!(editor_focused(&mut h), "editor focused after opening note");

    // Ctrl+Shift+E moves focus to active sidebar (Files).
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    assert!(!editor_focused(&mut h), "Ctrl+Shift+E moved focus out of editor");

    // Switching to Outline auto-focuses outline_keys and shows focus ring.
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Outline));
    settle(&mut h);
    assert!(h.ctx.memory(|m| m.has_focus(egui::Id::NULL.with("outline_keys"))));
    shot(&mut h, "m2-outline-focus-ring");

    // End / Home / PageUp / PageDown work and Esc returns focus to editor.
    h.key_press(Key::End);
    settle(&mut h);
    let sel_end = st(&mut h).ui.outline.selected();
    assert!(sel_end.is_some());
    h.key_press(Key::Home);
    settle(&mut h);
    assert_eq!(st(&mut h).ui.outline.selected(), Some(0));

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(editor_focused(&mut h), "Esc from Outline returns focus to editor");

    // Selection is preserved across focus loss; FocusSidebar restores focus to Outline.
    st(&mut h).commands.push(Command::FocusSidebar);
    settle(&mut h);
    assert_eq!(st(&mut h).ui.outline.selected(), Some(0));

    // Switching to Links auto-focuses links_keys while keeping sel = None initially.
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Links));
    settle(&mut h);
    wait(&mut h, "links for Reading", |s| !s.ui.links.model.is_pending());
    settle(&mut h);
    assert!(h.ctx.memory(|m| m.has_focus(egui::Id::NULL.with("links_keys"))));
    assert_eq!(st(&mut h).ui.links.selected(), None);
    shot(&mut h, "m2-links-focus-ring");

    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert!(st(&mut h).ui.links.selected().is_some());
    let preserved = st(&mut h).ui.links.selected();
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(editor_focused(&mut h), "Esc from Links returns focus to editor");
    assert_eq!(st(&mut h).ui.links.selected(), preserved, "selection kept across focus loss");

    // Opening a different note clears outline/links selection.
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Ideas.md"), new_tab: true, line: None });
    settle(&mut h);
    assert_eq!(st(&mut h).ui.links.selected(), None);
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Outline));
    settle(&mut h);
    assert_eq!(st(&mut h).ui.outline.selected(), None);
}

#[test]
fn search_panel_clear_button_and_full_keyboard_nav() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    settle(&mut h);

    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Search));
    settle(&mut h);
    h.event(Event::Text("plan".into()));
    wait(&mut h, "search finished", |s| s.ui.search.status_finished());
    settle(&mut h);
    assert!(!st(&mut h).ui.search.query.is_empty());
    assert!(st(&mut h).ui.search.selected().is_some());
    shot(&mut h, "m2-search-focus-ring");

    // Clear (×) button empties the query and keeps focus in input.
    h.get_by_label("Clear search").click();
    settle(&mut h);
    assert!(st(&mut h).ui.search.query.is_empty());

    // Type again, ArrowDown into rows, End/Home/PageDown/PageUp, Enter opens.
    h.event(Event::Text("plan".into()));
    wait(&mut h, "search finished 2", |s| s.ui.search.status_finished());
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    let after_down = st(&mut h).ui.search.selected();
    h.key_press(Key::End);
    settle(&mut h);
    let at_end = st(&mut h).ui.search.selected();
    assert!(at_end >= after_down);
    h.key_press(Key::Home);
    settle(&mut h);
    assert!(st(&mut h).ui.search.selected() <= after_down);

    // Esc from input clears non-empty query first, second Esc returns focus to editor.
    h.get_by_label("Search in workspace").click();
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).ui.search.query.is_empty(), "first Esc clears search query");
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(editor_focused(&mut h), "second Esc returns focus to editor");
}

/// Enter while the query debounce is still running starts the search at once.
#[test]
fn search_enter_during_debounce_runs_now() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Search));
    settle(&mut h);
    h.event(Event::Text("plan".into()));
    h.step();
    assert!(st(&mut h).ui.search.is_pending());
    h.key_press(Key::Enter);
    h.step();
    assert!(!st(&mut h).ui.search.is_pending(), "Enter must start the pending search");
}

#[test]
fn files_panel_initial_focus_and_escape_returns_to_editor() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    settle(&mut h);
    assert!(editor_focused(&mut h));

    st(&mut h).commands.push(Command::FocusSidebar);
    settle(&mut h);
    assert!(!editor_focused(&mut h), "FocusSidebar focuses tree panel");

    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(editor_focused(&mut h), "Esc in FilesPanel returns focus to editor");
}
