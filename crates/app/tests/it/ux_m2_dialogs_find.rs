//! FolderBrowser 2k virtualization/nav, Dialog::Modal role/focus/Enter/Esc, Settings search/arrows, Find compact/expanded (Items #12).
use crate::common::*;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::dialogs::Dialog;
use iron_sheets::state::AppState;
use iron_sheets_core::config::ViewMode;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
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
fn folder_browser_virtualizes_2000_dirs_and_supports_keyboard_nav() {
    let fx = Fx::new();
    let big = fx.dir.path().join("big_dirs");
    std::fs::create_dir_all(&big).expect("mkdir");
    for i in 0..2000 {
        std::fs::create_dir(big.join(format!("d{i:04}"))).expect("subdir");
    }

    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).dialogs.current = Some(Dialog::FolderBrowser {
        path_text: big.display().to_string(),
        dir: big.clone(),
        error: None,
        listing: None,
    });
    let t0 = Instant::now();
    loop {
        h.step();
        let done = matches!(
            &st(&mut h).dialogs.current,
            Some(Dialog::FolderBrowser { listing: Some((d, v)), .. }) if d == &big && v.len() == 2000
        );
        if done {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "timeout listing 2000 dirs");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);

    let painted = h.ctx.data(|d| d.get_temp::<usize>(egui::Id::NULL.with("fb_painted_rows"))).expect("fb_painted_rows");
    assert!(painted > 0 && painted <= 30, "expected virtualized rows <= 30, got {painted}");
    shot(&mut h, "m2-folder-browser-2k");

    assert!(h.ctx.memory(|m| m.has_focus(egui::Id::NULL.with("fb_path_input"))), "initial focus in Folder path");
    // ArrowDown from path_text moves to list row 0 (`..`), ArrowDown moves to d0000.
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::End);
    settle(&mut h);
    h.key_press(Key::Home);
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    let cur_dir = match &st(&mut h).dialogs.current {
        Some(Dialog::FolderBrowser { dir, .. }) => dir.clone(),
        _ => panic!("expected FolderBrowser"),
    };
    assert_eq!(cur_dir, big.join("d0000"), "Enter on selected subdir navigates into d0000");
}

#[test]
fn confirm_and_unsaved_modals_have_dialog_role_and_enter_esc() {
    let fx = Fx::new();
    let host = fx.notes().join("ModalHost.md");
    std::fs::write(&host, "[[ModalMissing]]\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenPath { path: host.clone(), new_tab: true, line: None });
    settle(&mut h);

    st(&mut h).commands.push(Command::OpenLink { note: "ModalMissing".into(), heading: None, from: Some(host), new_tab: false });
    settle(&mut h);
    shot(&mut h, "m2-confirm-modal");

    let has_dialog_role = h
        .output()
        .platform_output
        .accesskit_update
        .as_ref()
        .is_some_and(|u| u.nodes.iter().any(|(_, n)| n.role() == egui::accesskit::Role::Dialog))
        || h.get_all_by_role(egui::accesskit::Role::Dialog).next().is_some();
    assert!(has_dialog_role, "modal must expose Role::Dialog");

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_none());
    assert!(!fx.notes().join("ModalMissing.md").exists());

    // Unsaved dialog: disable autosave so CloseTab prompts, Esc cancels.
    let idea = fx.notes().join("Ideas.md");
    st(&mut h).config.autosave = false;
    st(&mut h).commands.push(Command::OpenPath { path: idea.clone(), new_tab: true, line: None });
    settle(&mut h);
    st(&mut h).docs.get_mut(&idea).expect("doc").apply_edit(0..0, "dirty ");
    let idx = st(&mut h).tabs.active.expect("active");
    st(&mut h).commands.push(Command::CloseTab(idx));
    settle(&mut h);
    assert!(h.query_by_label("Save").is_some());
    assert!(h.query_by_label("Discard").is_some());
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_none());
    assert!(st(&mut h).tabs.tabs.iter().any(|t| t.path == idea), "tab stays open on Esc");
}

/// A click on the backdrop must not throw away what was typed into a prompt.
#[test]
fn backdrop_click_keeps_a_prompt_open() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    st(&mut h).dialogs.prompt("New note", "Name", "", iron_sheets::dialogs::PromptAction::NewNote { dir: fx.notes() });
    settle(&mut h);
    h.event(egui::Event::Text("draft".into()));
    settle(&mut h);
    let corner = egui::pos2(3.0, 3.0);
    h.event(egui::Event::PointerMoved(corner));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: corner, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    settle(&mut h);
    assert!(matches!(&st(&mut h).dialogs.current, Some(Dialog::Prompt { value, .. }) if value == "draft"));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_none(), "Esc still cancels");
}

#[test]
fn settings_escape_clears_search_and_section_arrows_navigate() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    assert!(st(&mut h).settings.open);

    h.get_by_label("Search settings").type_text("font");
    settle(&mut h);
    assert_eq!(st(&mut h).settings.search, "font");

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(st(&mut h).settings.open, "first Esc must clear non-empty Settings search without closing");
    assert!(st(&mut h).settings.search.is_empty());

    // Section keyboard navigation: click General section button to focus section rail, then ArrowDown/End/Home.
    h.get_by_label("General").click();
    settle(&mut h);
    let initial = st(&mut h).settings.section;
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_ne!(st(&mut h).settings.section, initial, "ArrowDown moves to next section");
    h.key_press(Key::End);
    settle(&mut h);
    let at_end = st(&mut h).settings.section;
    h.key_press(Key::Home);
    settle(&mut h);
    assert_eq!(st(&mut h).settings.section, initial);
    assert_ne!(at_end, initial);

    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!st(&mut h).settings.open, "Esc with empty search closes Settings");
}

#[test]
fn find_bar_compact_vs_expanded_and_escape_from_controls() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);

    // Ctrl+F opens compact Find-only bar.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    settle(&mut h);
    assert!(h.query_by_label("Find in note").is_some());
    assert!(h.query_by_label("Replace…").is_some());
    assert!(h.query_by_label("Replace with").is_none());
    assert!(h.query_by_label("Replace all").is_none());
    shot(&mut h, "m2-find-compact");

    // Clicking "Replace…" expands Replace controls.
    h.get_by_label("Replace…").click();
    settle(&mut h);
    assert!(h.query_by_label("Replace with").is_some());
    assert!(h.query_by_label("Replace all").is_some());
    shot(&mut h, "m2-find-expanded");

    // Esc from any focused toggle button inside Find closes Find and returns focus to editor.
    h.get_by_label("Aa").click();
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(h.query_by_label("Find in note").is_none(), "Esc closes Find bar even when toggle is focused");
    assert!(editor_focused(&mut h), "Esc returns focus to Note editor");

    // Ctrl+H opens Find with Replace expanded immediately.
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    assert!(h.query_by_label("Replace with").is_some());
}
