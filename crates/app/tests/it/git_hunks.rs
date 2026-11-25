//! Hunk popup: opens from a click on a gutter bar and from the keyboard, reverts as an undoable edit, stages and unstages
//! one change, walks the changes with Alt+] and Alt+[.
use crate::common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use iron_sheets_app_core::git::{DiffKind, GitAvailability, GitModule};
use std::path::{Path, PathBuf};
use std::process::Command as SysCommand;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn has_git() -> bool {
    SysCommand::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = SysCommand::new("git").current_dir(dir).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn wait(h: &mut H, secs: u64, mut f: impl FnMut(&mut H) -> bool) -> bool {
    let t0 = Instant::now();
    loop {
        h.step();
        if f(h) {
            return true;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

const BASE: &str = "# Hunks\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12\nline 13\nline 14\nline 15\nline 16\nline 17\nline 18\nline 19\nline 20\nline 21\nline 22\nline 23\nline 24\n";

/// `BASE` with line 3 changed, line 12 deleted and a line added after line 20.
fn edited() -> String {
    let mut v: Vec<String> = BASE.lines().map(str::to_string).collect();
    v[2] = "line 3 changed".into();
    v.remove(11);
    v.insert(19, "an added line".into());
    v.join("\n") + "\n"
}

/// A repo with `Hunks.md` committed as `BASE`, the file on disk edited and opened in the editor, marks arrived.
fn setup() -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let notes = fx.notes();
    git(&notes, &["init", "-b", "main"]);
    git(&notes, &["config", "user.email", "test@example.com"]);
    git(&notes, &["config", "user.name", "Iron Sheets Test"]);
    let note = notes.join("Hunks.md");
    std::fs::write(&note, BASE).unwrap();
    git(&notes, &["add", "Hunks.md"]);
    git(&notes, &["commit", "-m", "base"]);
    std::fs::write(&note, edited()).unwrap();
    let mut h = harness(fx.opts(vec![notes]));
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().availability == GitAvailability::Ready)));
    st(&mut h).commands.push(Command::OpenPath { path: note.clone(), new_tab: true, line: None });
    settle(&mut h);
    assert!(wait(&mut h, 10, |h| marks(h, &note).len() == 3), "three changes arrived");
    settle(&mut h);
    (fx, h, note)
}

fn marks(h: &mut H, note: &Path) -> Vec<iron_sheets_app_core::git::DiffMark> {
    let s = st(h);
    let (Some(g), Some(doc)) = (GitModule::of(s), s.docs.get(note)) else { return Vec::new() };
    g.diff_marks(note, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone()).0.to_vec()
}

fn shown(h: &mut H) -> Option<(DiffKind, u32)> {
    st(h).ui.git_hunk.shown().map(|(_, m)| (m.kind, m.start))
}

fn text(h: &mut H, note: &Path) -> String {
    st(h).docs.get(note).map(|d| d.text.clone()).unwrap_or_default()
}

/// Screen position of the change bar of 0-based `line`: just left of the editor text, in the row of that line.
fn bar_at(h: &mut H, line: u32) -> egui::Pos2 {
    let r = h.get_by_role(Role::MultilineTextInput).rect();
    let lh = st(h).ui.theme.editor.line_height;
    egui::pos2(r.left() - 5.5, r.top() + (line as f32 + 0.5) * lh)
}

fn click(h: &mut H, pos: egui::Pos2) {
    h.hover_at(pos);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    settle(h);
}

#[test]
fn clicking_a_gutter_bar_opens_the_popup_and_r_reverts_that_change_as_one_undo() {
    if !has_git() {
        return;
    }
    let (_fx, mut h, note) = setup();
    shot(&mut h, "git_hunk_gutter");
    assert_eq!(shown(&mut h), None);

    // Line 3 (0-based 2) changed: the bar there opens the popup with the old and the new line.
    let p = bar_at(&mut h, 2);
    click(&mut h, p);
    assert_eq!(shown(&mut h), Some((DiffKind::Modified, 2)), "the click opened the popup of that change");
    assert!(h.query_by_label_contains("Changed line 3").is_some());
    assert!(h.query_by_label("- line 3").is_some() && h.query_by_label("+ line 3 changed").is_some(), "old and new lines");
    shot(&mut h, "git_hunk_popup_modified");

    // R reverts it: the text is the edited one with only that change undone; one Ctrl+Z puts it back.
    let before = text(&mut h, &note);
    h.key_press(egui::Key::R);
    settle(&mut h);
    assert_eq!(text(&mut h, &note), before.replace("line 3 changed", "line 3"));
    assert_eq!(shown(&mut h), None, "the popup closed");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    settle(&mut h);
    assert_eq!(text(&mut h, &note), before, "Ctrl+Z undoes the revert in one step");
    assert!(!st(&mut h).docs.get(&note).unwrap().text.is_empty());
}

#[test]
fn keyboard_opens_walks_the_changes_stages_unstages_and_esc_closes() {
    if !has_git() {
        return;
    }
    let (fx, mut h, note) = setup();
    let notes = fx.notes();
    let arg = note.to_string_lossy().into_owned();
    // Caret on line 3: `git.hunk` shows that change. Typing a letter later must not edit the note.
    st(&mut h).tabs.active_tab_mut().unwrap().pending_cursor = Some(12..12);
    settle(&mut h);
    st(&mut h).commands.push(Command::Module { id: "git.hunk", arg: arg.clone() });
    settle(&mut h);
    assert_eq!(shown(&mut h), Some((DiffKind::Modified, 2)));
    let before = text(&mut h, &note);

    // Alt+] walks to the next change (the deletion), then the added line, then wraps; Alt+[ goes back.
    for want in [(DiffKind::Deleted, 11), (DiffKind::Added, 19), (DiffKind::Modified, 2)] {
        h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::CloseBracket);
        settle(&mut h);
        assert_eq!(shown(&mut h), Some(want));
    }
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::OpenBracket);
    settle(&mut h);
    assert_eq!(shown(&mut h), Some((DiffKind::Added, 19)));
    assert!(h.query_by_label("+ an added line").is_some());
    shot(&mut h, "git_hunk_popup_added");
    h.key_press(egui::Key::P);
    settle(&mut h);
    assert_eq!(shown(&mut h), Some((DiffKind::Deleted, 11)));
    assert!(h.query_by_label("- line 13").is_none() && h.query_by_label_contains("- line 12").is_some(), "the removed line is listed");
    shot(&mut h, "git_hunk_popup_deleted");
    assert_eq!(text(&mut h, &note), before, "navigating and the popup keys typed nothing");

    // S stages only that change.
    h.key_press(egui::Key::S);
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Stage hunk")));
    settle(&mut h);
    assert_eq!(shown(&mut h), None);
    let cached = git(&notes, &["diff", "--cached", "-U0"]);
    assert!(cached.contains("-line 12") && !cached.contains("+an added line") && !cached.contains("line 3 changed"), "{cached}");
    assert_eq!(text(&mut h, &note), before, "the note is untouched");
    assert!(st(&mut h).status.current().is_some_and(|m| !m.error && m.text.contains("Staged hunk")), "toast");
    shot(&mut h, "git_hunk_staged");

    // Reopen it and U takes it out again; a second U says it is not staged.
    for want_ok in [true, false] {
        st(&mut h).tabs.active_tab_mut().unwrap().pending_cursor = Some(0..0);
        settle(&mut h);
        st(&mut h).commands.push(Command::Module { id: "git.hunk_next", arg: arg.clone() });
        settle(&mut h);
        st(&mut h).commands.push(Command::Module { id: "git.hunk_next", arg: arg.clone() });
        settle(&mut h);
        assert_eq!(shown(&mut h), Some((DiffKind::Deleted, 11)));
        h.key_press(egui::Key::U);
        assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Unstage hunk" && o.result.is_ok() == want_ok)));
        settle(&mut h);
    }
    assert!(git(&notes, &["diff", "--cached"]).is_empty(), "nothing staged any more");
    assert!(st(&mut h).status.current().is_some_and(|m| m.error && m.text.contains("not staged")), "the refusal is shown");

    // Esc closes.
    st(&mut h).commands.push(Command::Module { id: "git.hunk", arg });
    settle(&mut h);
    assert!(shown(&mut h).is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(shown(&mut h), None);
}

#[test]
fn the_popup_closes_on_an_edit_a_press_elsewhere_and_without_changes_says_so() {
    if !has_git() {
        return;
    }
    let (_fx, mut h, note) = setup();
    let arg = note.to_string_lossy().into_owned();
    let p = bar_at(&mut h, 2);
    click(&mut h, p);
    assert!(shown(&mut h).is_some());
    // The note changes under it: closed.
    {
        let doc = st(&mut h).docs.get_mut(&note).unwrap();
        doc.text.push_str("more\n");
        doc.touch();
    }
    settle(&mut h);
    assert_eq!(shown(&mut h), None);
    // A press elsewhere closes it too.
    // (Once the marks of the new text have arrived.)
    assert!(wait(&mut h, 10, |h| {
        st(h).commands.push(Command::Module { id: "git.hunk", arg: arg.clone() });
        h.step();
        h.step();
        shown(h).is_some()
    }));
    settle(&mut h);
    let r = h.get_by_role(Role::MultilineTextInput).rect();
    click(&mut h, egui::pos2(r.center().x, r.bottom() - 30.0));
    assert_eq!(shown(&mut h), None);
    // A note with no change says so instead of opening an empty popup.
    git(&_fx.notes(), &["checkout", "--", "Hunks.md"]);
    let doc = st(&mut h).docs.get_mut(&note).unwrap();
    doc.text = BASE.into();
    doc.touch();
    assert!(wait(&mut h, 10, |h| marks(h, &note).is_empty()));
    st(&mut h).commands.push(Command::Module { id: "git.hunk", arg });
    settle(&mut h);
    assert_eq!(shown(&mut h), None);
    assert!(st(&mut h).status.current().is_some_and(|m| m.text.contains("No changes")));
}

#[test]
fn the_popup_waits_for_marks_of_the_current_text() {
    if !has_git() {
        return;
    }
    let (_fx, mut h, note) = setup();
    {
        let doc = st(&mut h).docs.get_mut(&note).unwrap();
        doc.text.push_str("typed\n");
        doc.touch();
    }
    // Within the 150 ms debounce the cached marks are for the older text: no popup, a message instead.
    st(&mut h).commands.push(Command::Module { id: "git.hunk", arg: note.to_string_lossy().into_owned() });
    h.step();
    h.step();
    assert_eq!(shown(&mut h), None);
    assert!(st(&mut h).status.current().is_some_and(|m| m.text.contains("being updated")));
}
