//! IS-31: Clicking an unresolved wiki-link prompts before creating a file on disk.
use crate::common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_core::config::ViewMode;
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

fn click_pos(h: &mut H, pos: egui::Pos2, modifiers: Modifiers) {
    if modifiers != Modifiers::NONE {
        h.event(Event::ModifiersChanged(modifiers));
    }
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers });
    h.step();
    h.event(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers });
    if modifiers != Modifiers::NONE {
        h.event(Event::ModifiersChanged(Modifiers::NONE));
    }
    settle(h);
}

fn preview_hit(h: &mut H, prefix: &str) -> egui::Rect {
    h.state_mut()
        .app()
        .state
        .ui
        .md_preview
        .hits
        .iter()
        .find(|(_, l)| l.starts_with(prefix))
        .map(|(r, _)| *r)
        .unwrap_or_else(|| panic!("no hit starting with {prefix}"))
}

#[test]
fn preview_dead_link_click_confirms_then_cancel_and_create() {
    let fx = Fx::new();
    let host = fx.notes().join("Host.md");
    std::fs::write(&host, "# Host\n\nSee [[Missing]].\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenPath { path: host.clone(), new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Preview));
    settle(&mut h);
    settle(&mut h);

    let r = preview_hit(&mut h, "link:Missing");
    click_pos(&mut h, r.center(), Modifiers::NONE);

    let missing = fx.notes().join("Missing.md");
    assert!(!missing.exists(), "dead link click must not silently create file");
    assert!(h.query_by_label("Note 'Missing' does not exist.").is_some());
    assert!(h.query_by_label("Create").is_some());
    assert!(h.query_by_label("Cancel").is_some());
    shot(&mut h, "dead-link-confirm");

    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert!(!missing.exists(), "Cancel must not create file");
    assert!(st(&mut h).dialogs.current.is_none());
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(host.clone()));

    let r = preview_hit(&mut h, "link:Missing");
    click_pos(&mut h, r.center(), Modifiers::NONE);
    assert!(h.query_by_label("Note 'Missing' does not exist.").is_some());
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(missing.exists(), "Enter on focused Create button must create note");
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(missing));
}

#[test]
fn links_panel_dead_link_click_and_enter_creates() {
    let fx = Fx::new();
    let host = fx.notes().join("HostLinks.md");
    std::fs::write(&host, "# HostLinks\n\nGo to [[UnresolvedLink]].\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    wait(&mut h, "index", |s| s.indexer.index().read().len() >= 8);
    st(&mut h).commands.push(Command::OpenPath { path: host, new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Links));
    settle(&mut h);
    wait(&mut h, "outgoing link row", |s| !s.ui.links.model.is_pending() && !s.ui.links.model.rows().is_empty());
    settle(&mut h);

    h.get_by_label_contains("UnresolvedLink").click();
    settle(&mut h);

    let target = fx.notes().join("UnresolvedLink.md");
    assert!(!target.exists(), "Links panel click must confirm before creating");
    assert!(h.query_by_label("Note 'UnresolvedLink' does not exist.").is_some());

    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(target.exists(), "Enter must confirm and create UnresolvedLink.md");
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(target));
}

#[test]
fn editor_open_link_dead_note_prompts_cancel_and_create() {
    let fx = Fx::new();
    let host = fx.notes().join("HostEd.md");
    std::fs::write(&host, "[[FromEditor]] line\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenPath { path: host.clone(), new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);

    let ed_rect = h.get_by_label("Note editor").rect();
    click_pos(&mut h, ed_rect.min + egui::vec2(42.0, 10.0), Modifiers::COMMAND);

    let target = fx.notes().join("FromEditor.md");
    assert!(!target.exists(), "Ctrl+click on dead wiki-link must not create silently");
    assert!(h.query_by_label("Note 'FromEditor' does not exist.").is_some());

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!target.exists(), "Esc cancels without creating");
    assert!(st(&mut h).dialogs.current.is_none());

    click_pos(&mut h, ed_rect.min + egui::vec2(42.0, 10.0), Modifiers::COMMAND);
    h.get_by_label("Create").click();
    settle(&mut h);
    assert!(target.exists(), "Create button creates note");
    assert_eq!(st(&mut h).tabs.active_path().map(|p| p.to_path_buf()), Some(target));
}

#[test]
fn escape_over_page_popup_closes_confirm_dialog_without_closing_popup() {
    let fx = Fx::new();
    let target = fx.notes().join("TargetPop.md");
    let host = fx.notes().join("HostPop.md");
    std::fs::write(&target, "# TargetPop\n\nPopup body.\n").expect("write");
    std::fs::write(&host, "# HostPop\n\n[[TargetPop]]\n").expect("write");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    st(&mut h).commands.push(Command::OpenPath { path: host.clone(), new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Preview));
    settle(&mut h);
    settle(&mut h);

    let r = preview_hit(&mut h, "link:TargetPop");
    h.event(Event::ModifiersChanged(Modifiers::COMMAND));
    h.event(Event::PointerMoved(r.center()));
    let t0 = Instant::now();
    while !st(&mut h).ui.page_popup.is_open() {
        h.event(Event::PointerMoved(r.center()));
        h.step();
        assert!(t0.elapsed() < Duration::from_secs(5), "popup did not open");
        std::thread::sleep(Duration::from_millis(5));
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));

    st(&mut h).commands.push(Command::OpenLink { note: "DeadOverPopup".into(), heading: None, from: Some(host), new_tab: false });
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_some());
    assert!(st(&mut h).ui.page_popup.is_open());

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_none(), "Esc must close the Confirm dialog");
    assert!(st(&mut h).ui.page_popup.is_open(), "PagePopup must remain open after Esc closed the dialog");

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!st(&mut h).ui.page_popup.is_open(), "second Esc closes PagePopup");
}
