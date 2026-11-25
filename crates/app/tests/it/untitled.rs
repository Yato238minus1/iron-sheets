//! Untitled notes: started by typing on the empty tab, Ctrl+T, autosave into the app data folder, replacing, save as,
//! closing, quitting and session restore. Screenshots go to IRON_SHEETS_SHOTS_DIR as `untitled_*.png`.
use crate::common::*;
use egui::{Event, Key, Modifiers, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::dialogs::{Dialog, PromptAction};
use iron_sheets::state::AppState;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn key_with(h: &mut H, m: Modifiers, k: Key) {
    h.key_press_modifiers(m, k);
    settle(h);
}

/// Type into whatever has keyboard focus (no click: proves the editor is focused).
fn type_text(h: &mut H, text: &str) {
    h.event(Event::Text(text.into()));
    settle(h);
}

fn start(fx: &Fx, roots: Vec<PathBuf>, autosave_ms: u32) -> H {
    let mut o = fx.opts(roots);
    o.config.autosave_delay_ms = autosave_ms;
    harness(o)
}

/// Waits `ms` for the autosave delay, then until the background save worker has written every dirty note.
fn autosave(h: &mut H, ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
    settle(h);
    for _ in 0..400 {
        let s = st(h);
        if s.docs.dirty_paths().is_empty() && !s.active_path().is_some_and(|p| s.docs.is_saving(&p)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
    panic!("autosave did not finish");
}

fn untitled_dir(fx: &Fx) -> PathBuf {
    fx.dirs().untitled_dir()
}

fn untitled_files(fx: &Fx) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(untitled_dir(fx))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn active_text(h: &mut H) -> String {
    let s = st(h);
    s.active_path().and_then(|p| s.docs.get(&p).map(|d| d.text.clone())).unwrap_or_default()
}

fn dialog_is_save_prompt(h: &mut H) -> bool {
    matches!(st(h).dialogs.current, Some(Dialog::Prompt { action: PromptAction::SaveUntitled { .. }, .. }))
}

#[test]
fn typing_on_the_fresh_empty_tab_starts_a_focused_untitled_note_and_writes_nothing_before() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 400);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
    h.get_by_role_and_label(Role::Tab, "New tab");
    assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "an empty tab has no editor");
    assert!(!untitled_dir(&fx).exists(), "no folder and no file before there is text");
    // The empty page offers the actions of the old welcome page.
    h.get_by_label("Start typing to begin an untitled note.");
    h.get_by_role_and_label(Role::Button, "New untitled note");
    shot(&mut h, "untitled_fresh_start");
    // Typing needs no click: the text starts an untitled note in this tab and the editor has focus.
    type_text(&mut h, "hello untitled");
    assert_eq!(st(&mut h).tabs.tabs.len(), 1, "the empty tab was overwritten");
    h.get_by_role_and_label(Role::Tab, "Untitled-1 (unsaved)");
    assert_eq!(active_text(&mut h), "hello untitled");
    assert!(h.query_by_label("Start typing to begin an untitled note.").is_none(), "the page goes away with the note");
    assert!(!untitled_dir(&fx).join("Untitled-1.md").exists(), "autosave has not fired yet");
    type_text(&mut h, "!");
    assert_eq!(active_text(&mut h), "hello untitled!", "the editor took the following keys");
    shot(&mut h, "untitled_typed");
    autosave(&mut h, 450);
    assert_eq!(std::fs::read_to_string(untitled_dir(&fx).join("Untitled-1.md")).expect("autosaved file"), "hello untitled!");
    assert!(!st(&mut h).docs.get(&untitled_dir(&fx).join("Untitled-1.md")).expect("doc").is_dirty());
}

#[test]
fn whitespace_alone_does_not_start_a_note_and_the_untitled_button_does() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 400);
    type_text(&mut h, "  ");
    assert!(st(&mut h).tabs.active_is_empty(), "Space alone activates buttons, it does not start a note");
    h.get_by_role_and_label(Role::Button, "New untitled note").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    h.get_by_role_and_label(Role::Tab, "Untitled-1");
    assert_eq!(active_text(&mut h), "");
    assert!(h.get_by_role(Role::MultilineTextInput).accesskit_node().is_focused(), "the editor of the new note is focused");
    assert!(untitled_files(&fx).is_empty());
}

#[test]
fn restore_on_reopens_the_previous_tabs_without_an_extra_untitled_note() {
    let fx = Fx::new();
    {
        let mut h = start(&fx, vec![fx.notes()], 50);
        st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
        st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Ideas.md"), new_tab: true, line: None });
        settle(&mut h);
        h.state_mut().app().on_quit();
    }
    let mut h = start(&fx, vec![], 50);
    let names: Vec<String> = st(&mut h).tabs.tabs.iter().map(|t| t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
    assert_eq!(names, ["Welcome.md", "Ideas.md"]);
}

#[test]
fn restore_on_brings_back_an_untitled_note_with_its_text() {
    let fx = Fx::new();
    {
        let mut h = start(&fx, vec![fx.notes()], 50);
        type_text(&mut h, "# kept across restarts");
        autosave(&mut h, 120);
        h.state_mut().app().on_quit();
    }
    let mut h = start(&fx, vec![], 50);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    h.get_by_role_and_label(Role::Tab, "Untitled-1");
    assert_eq!(active_text(&mut h), "# kept across restarts");
}

#[test]
fn quit_keeps_untitled_text_without_asking_when_restore_is_on_even_with_autosave_off() {
    let fx = Fx::new();
    {
        let mut o = fx.opts(vec![fx.notes()]);
        o.config.autosave = false;
        let mut h = harness(o);
        type_text(&mut h, "hot exit");
        assert!(!untitled_dir(&fx).join("Untitled-1.md").exists());
        st(&mut h).commands.push(Command::Quit);
        settle(&mut h);
        assert!(st(&mut h).dialogs.current.is_none(), "no prompt: the note is kept");
        assert_eq!(std::fs::read_to_string(untitled_dir(&fx).join("Untitled-1.md")).expect("saved on quit"), "hot exit");
        h.state_mut().app().on_quit();
    }
    let mut h = start(&fx, vec![], 50);
    assert_eq!(active_text(&mut h), "hot exit");
}

#[test]
fn restore_off_starts_with_one_empty_tab() {
    let fx = Fx::new();
    {
        let mut h = start(&fx, vec![fx.notes()], 50);
        st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
        settle(&mut h);
        h.state_mut().app().on_quit();
    }
    let mut o = fx.opts(vec![]);
    o.config.restore_session = false;
    let mut h = harness(o);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
    h.get_by_role_and_label(Role::Tab, "New tab");
}

#[test]
fn a_missing_untitled_file_of_the_session_restores_as_an_empty_note() {
    let fx = Fx::new();
    {
        let mut h = start(&fx, vec![fx.notes()], 50);
        type_text(&mut h, "gone soon");
        autosave(&mut h, 120);
        h.state_mut().app().on_quit();
    }
    std::fs::remove_file(untitled_dir(&fx).join("Untitled-1.md")).expect("remove");
    let mut h = start(&fx, vec![], 50);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert_eq!(active_text(&mut h), "");
}

#[test]
fn opening_a_file_replaces_an_empty_untitled_note_but_not_one_with_text() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    h.get_all_by_label("Welcome.md").next().expect("tree entry").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1, "the empty note was replaced");
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p.ends_with("Welcome.md")));
    assert!(untitled_files(&fx).is_empty(), "nothing was ever written for it");
    // A note with text stays; the file opens next to it, also on a plain click.
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    type_text(&mut h, "keep me");
    h.get_all_by_label("Ideas.md").next().expect("tree entry").click();
    settle(&mut h);
    let s = st(&mut h);
    assert_eq!(s.tabs.tabs.len(), 3);
    assert!(s.tabs.active_path().is_some_and(|p| p.ends_with("Ideas.md")));
    let u = s.tabs.tabs[1].path.clone();
    assert!(s.tabs.is_untitled(&u) && s.docs.get(&u).is_some_and(|d| d.text == "keep me"));
}

#[test]
fn typing_on_a_second_empty_tab_starts_another_untitled_note_with_a_new_number() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    type_text(&mut h, "first");
    autosave(&mut h, 120);
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    assert!(st(&mut h).tabs.active_is_empty(), "Ctrl+T opens an empty tab");
    h.get_by_role_and_label(Role::Tab, "Untitled-1");
    type_text(&mut h, "second");
    h.get_by_role_and_label(Role::Tab, "Untitled-2 (unsaved)");
    assert_eq!(active_text(&mut h), "second");
    assert_eq!(st(&mut h).tabs.active, Some(1));
}

#[test]
fn files_on_the_command_line_open_without_an_untitled_note() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes(), fx.notes().join("Welcome.md"), fx.notes().join("Ideas.md")], 50);
    let names: Vec<String> = st(&mut h).tabs.tabs.iter().map(|t| t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
    assert_eq!(names, ["Welcome.md", "Ideas.md"]);
    assert!(untitled_files(&fx).is_empty());
}

#[test]
fn ctrl_s_asks_for_a_name_and_moves_the_note_into_the_workspace() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    type_text(&mut h, "My new idea\n\nbody");
    autosave(&mut h, 120);
    assert!(untitled_dir(&fx).join("Untitled-1.md").exists());
    key_with(&mut h, Modifiers::COMMAND, Key::S);
    assert!(dialog_is_save_prompt(&mut h));
    shot(&mut h, "untitled_save_as");
    h.get_by_label("OK").click();
    settle(&mut h);
    let target = fx.notes().join("My new idea.md");
    assert_eq!(std::fs::read_to_string(&target).expect("saved"), "My new idea\n\nbody");
    assert!(untitled_files(&fx).is_empty(), "the app data file is gone");
    let s = st(&mut h);
    assert_eq!(s.tabs.tabs.len(), 1);
    assert_eq!(s.tabs.active_path(), Some(target.as_path()));
    assert!(s.docs.get(&target).is_some_and(|d| !d.is_dirty() && d.text == "My new idea\n\nbody"));
    assert!(s.indexer.wait_idle(Duration::from_secs(5)));
    assert!(s.indexer.index().read().by_path(&target).is_some(), "indexed like a new note");
    h.get_by_role_and_label(Role::Tab, "My new idea.md");
    // From here on Ctrl+S is an ordinary save.
    type_text(&mut h, "!");
    key_with(&mut h, Modifiers::COMMAND, Key::S);
    assert!(st(&mut h).dialogs.current.is_none());
}

#[test]
fn save_as_never_overwrites_and_reports_the_clash() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    type_text(&mut h, "Welcome");
    key_with(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
    assert!(dialog_is_save_prompt(&mut h), "Ctrl+Shift+S is Save as");
    let before = std::fs::read_to_string(fx.notes().join("Welcome.md")).expect("read");
    h.get_by_label("OK").click();
    settle(&mut h);
    assert_eq!(std::fs::read_to_string(fx.notes().join("Welcome.md")).expect("read"), before);
    let s = st(&mut h);
    assert!(s.tabs.is_untitled(s.tabs.active_path().expect("active")), "the note stays untitled");
    assert!(s.status.current().is_some_and(|m| m.error && m.text.contains("already exists")));
}

#[test]
fn save_as_picks_the_target_folder_and_accepts_a_subfolder_path() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes(), fx.journal()], 50);
    type_text(&mut h, "journal entry");
    let path = st(&mut h).tabs.tabs[0].path.clone();
    st(&mut h).commands.push(Command::SaveAs { path, after: iron_sheets::dialogs::AfterSave::Nothing });
    settle(&mut h);
    // Name with a sub path, target folder = second root.
    if let Some(Dialog::Prompt { value, dir_sel, dirs, .. }) = &mut st(&mut h).dialogs.current {
        assert_eq!(dirs.len(), 2);
        *dir_sel = 1;
        *value = "Projects2/entry.typ".into();
    } else {
        panic!("no prompt");
    }
    std::fs::create_dir(fx.journal().join("Projects2")).expect("mkdir");
    h.get_by_label("OK").click();
    settle(&mut h);
    let target = fx.journal().join("Projects2").join("entry.typ");
    assert_eq!(std::fs::read_to_string(&target).expect("saved"), "journal entry");
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p == target));
}

#[test]
fn closing_an_untitled_note_with_text_asks_and_discard_deletes_the_file() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    h.get_all_by_label("Welcome.md").next().expect("entry").click();
    settle(&mut h);
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    type_text(&mut h, "scribble");
    autosave(&mut h, 120);
    let file = untitled_dir(&fx).join("Untitled-1.md");
    assert!(file.exists());
    key_with(&mut h, Modifiers::COMMAND, Key::W);
    assert!(matches!(st(&mut h).dialogs.current, Some(Dialog::Unsaved { .. })), "asks even though autosave wrote it");
    shot(&mut h, "untitled_close_dialog");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    assert!(file.exists());
    key_with(&mut h, Modifiers::COMMAND, Key::W);
    h.get_by_label("Discard").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(!file.exists(), "discard deletes the app data file");
    assert!(st(&mut h).tabs.closed.is_empty(), "not remembered for reopening");
}

#[test]
fn closing_an_untitled_note_with_save_saves_then_closes_it() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    h.get_all_by_label("Welcome.md").next().expect("entry").click();
    settle(&mut h);
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    type_text(&mut h, "worth keeping");
    key_with(&mut h, Modifiers::COMMAND, Key::W);
    h.get_by_label("Save").click();
    settle(&mut h);
    assert!(dialog_is_save_prompt(&mut h));
    h.get_by_label("OK").click();
    settle(&mut h);
    assert_eq!(std::fs::read_to_string(fx.notes().join("worth keeping.md")).expect("saved"), "worth keeping");
    assert_eq!(st(&mut h).tabs.tabs.len(), 1, "the tab closed after saving");
    assert!(untitled_files(&fx).is_empty());
}

#[test]
fn closing_an_empty_untitled_note_asks_nothing_and_leaves_no_file() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    st(&mut h).commands.push(Command::StartUntitled { text: String::new() });
    key_with(&mut h, Modifiers::COMMAND, Key::T);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    h.get_by_role_and_label(Role::Tab, "Untitled-1");
    st(&mut h).commands.push(Command::ActivateTab(0));
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.active, Some(0));
    key_with(&mut h, Modifiers::COMMAND, Key::W);
    assert!(st(&mut h).dialogs.current.is_none());
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.closed.is_empty());
    assert!(untitled_files(&fx).is_empty());
}

#[test]
fn text_typed_and_deleted_again_leaves_no_file_behind() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![fx.notes()], 50);
    type_text(&mut h, "x");
    autosave(&mut h, 120);
    assert_eq!(untitled_files(&fx), ["Untitled-1.md"]);
    key_with(&mut h, Modifiers::NONE, Key::Backspace);
    autosave(&mut h, 120);
    assert_eq!(active_text(&mut h), "");
    assert!(untitled_files(&fx).is_empty(), "an empty note has no file");
}

#[test]
fn quit_with_restore_off_asks_about_untitled_text_and_discard_deletes_it() {
    let fx = Fx::new();
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.restore_session = false;
    let mut h = harness(o);
    type_text(&mut h, "temporary");
    std::thread::sleep(Duration::from_millis(1200));
    settle(&mut h);
    st(&mut h).commands.push(Command::Quit);
    settle(&mut h);
    assert!(matches!(st(&mut h).dialogs.current, Some(Dialog::Unsaved { .. })), "autosaved text is still the user's call");
    h.get_by_label("Discard").click();
    settle(&mut h);
    assert!(st(&mut h).quit_ok);
    assert!(untitled_files(&fx).is_empty());
    h.state_mut().app().on_quit();
    assert!(untitled_files(&fx).is_empty(), "nothing is written again on exit");
}

#[test]
fn quit_with_restore_off_and_an_empty_note_just_quits() {
    let fx = Fx::new();
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.restore_session = false;
    let mut h = harness(o);
    st(&mut h).commands.push(Command::Quit);
    settle(&mut h);
    assert!(st(&mut h).dialogs.current.is_none());
    assert!(st(&mut h).quit_ok);
}

#[test]
fn without_folders_the_empty_page_keeps_the_workspace_reachable() {
    let fx = Fx::new();
    let mut h = start(&fx, vec![], 400);
    h.get_by_label("This workspace has no folders yet");
    assert_eq!(h.get_all_by_role_and_label(Role::Button, "Add folder by path...").count(), 2, "sidebar and empty page");
    h.get_by_role_and_label(Role::Button, "Open note Ctrl+O");
    shot(&mut h, "untitled_no_roots");
    // Save as without folders uses the native dialog (a thread) or, when that is unavailable, the in-app prompt;
    // here only the command routing is checked.
    type_text(&mut h, "x");
    let p = st(&mut h).tabs.tabs[0].path.clone();
    let routed = iron_sheets::core::command::execute(st(&mut h), &mut NoPlatform, Command::SaveAs { path: p, after: iron_sheets::dialogs::AfterSave::Nothing });
    assert!(matches!(routed, Some(Command::SaveAsDialog { .. })));
}

struct NoPlatform;
impl iron_sheets::platform::Platform for NoPlatform {
    fn clipboard_set(&mut self, _: &str) {}
    fn set_title(&mut self, _: &str) {}
    fn close_window(&mut self) {}
    fn waker(&self) -> std::sync::Arc<dyn iron_sheets::platform::Waker> {
        struct W;
        impl iron_sheets::platform::Waker for W {
            fn wake(&self) {}
        }
        std::sync::Arc::new(W)
    }
}

/// A background untitled tab whose file cannot be read (here: a folder) closes instead of "Still loading" forever.
#[test]
fn a_background_untitled_tab_that_failed_to_load_still_closes() {
    let fx = Fx::new();
    {
        let mut h = start(&fx, vec![fx.notes()], 50);
        type_text(&mut h, "text");
        autosave(&mut h, 120);
        st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
        settle(&mut h);
        h.state_mut().app().on_quit();
    }
    let file = untitled_dir(&fx).join("Untitled-1.md");
    std::fs::remove_file(&file).expect("remove");
    std::fs::create_dir(&file).expect("dir in its place");
    let mut h = start(&fx, vec![fx.notes()], 50);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    for _ in 0..20 {
        let Some(i) = st(&mut h).tabs.tabs.iter().position(|t| t.path == file) else { return };
        st(&mut h).commands.push(Command::CloseTab(i));
        settle(&mut h);
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the tab never closed");
}
