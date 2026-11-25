//! Integration & UI screenshot tests for M4 Git integration (G1–G2).
use crate::common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::settings::Section;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_app_core::git::{DiffKind, GitAvailability, GitFileStatus, GitModule, MAX_DIFF_BYTES};
use iron_sheets_app_core::modules::Module;
use std::path::Path;
use std::process::Command as SysCommand;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn has_git() -> bool {
    SysCommand::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = SysCommand::new("git").current_dir(dir).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

fn init_git_repo(dir: &Path) {
    run_git(dir, &["init", "-b", "main"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Iron Sheets Test"]);
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

fn wait_git_ready(h: &mut H) {
    assert!(
        wait(h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().availability == GitAvailability::Ready)),
        "timeout waiting for GitAvailability::Ready"
    );
    settle(h);
}

#[test]
fn git_repo_branch_tree_badges_and_ignored_dimming() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    std::fs::write(notes.join(".gitignore"), "*.tmp\nsecret/\n").unwrap();
    std::fs::write(notes.join("Added.md"), "# Added\n").unwrap();
    std::fs::write(notes.join("Deleted.md"), "# Deleted\n").unwrap();
    run_git(&notes, &["add", ".gitignore", "Welcome.md", "Ideas.md", "Reading.md", "Deleted.md", "Projects"]);
    run_git(&notes, &["commit", "-m", "initial commit"]);

    // Create dirty states: M, A, D, ?, and ignored entries.
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nModified line.\n").unwrap();
    std::fs::write(notes.join("Added.md"), "# Added\n\nStaged.\n").unwrap();
    run_git(&notes, &["add", "Added.md"]);
    std::fs::remove_file(notes.join("Deleted.md")).unwrap();
    std::fs::write(notes.join("Untracked.md"), "# Untracked\n").unwrap();
    std::fs::write(notes.join("scratch.tmp"), "ignored file").unwrap();
    std::fs::create_dir_all(notes.join("secret")).unwrap();
    std::fs::write(notes.join("secret/KeyNote.md"), "# Secret\n").unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    // Expand `secret` folder so `KeyNote.md` is visible for dimming check & screenshot.
    st(&mut h).tree.set_expanded(&notes.join("secret"), true);
    settle(&mut h);

    assert!(
        wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.dirty_count >= 4))),
        "expected dirty_count >= 4"
    );
    settle(&mut h);

    // Status bar shows branch name and dirty count.
    assert!(h.query_by_label("main").is_some(), "branch 'main' in status bar");
    assert!(h.query_by_label_contains("*").is_some(), "dirty indicator in status bar");

    // Verify per-file git status & ignored exclusion from dirty_count.
    {
        let snap = GitModule::of(st(&mut h)).unwrap().snapshot();
        assert_eq!(snap.status_of(&notes.join("Ideas.md")), Some(GitFileStatus::Modified));
        assert_eq!(snap.status_of(&notes.join("Added.md")), Some(GitFileStatus::Added));
        assert_eq!(snap.status_of(&notes.join("Untracked.md")), Some(GitFileStatus::Untracked));
        assert_eq!(snap.status_of(&notes.join("scratch.tmp")), Some(GitFileStatus::Ignored));
        assert_eq!(snap.status_of(&notes.join("secret/KeyNote.md")), Some(GitFileStatus::Ignored));
        assert_eq!(snap.status_of(&notes.join("Welcome.md")), None);
        // Deleted.md is not on disk so tree doesn't list it, but snapshot tracks it and counts it.
        assert_eq!(snap.status_of(&notes.join("Deleted.md")), Some(GitFileStatus::Deleted));
    }

    // Ignored markdown note is dimmed (`weak_text_color`) compared to normal markdown note (`text_color`).
    let rows = iron_sheets::theme::recorded_rows(&h.ctx, "files");
    let normal_col = rows.iter().find(|(p, _)| p.ends_with("Welcome.md")).map(|(_, c)| *c).expect("Welcome.md row");
    let ignored_col = rows.iter().find(|(p, _)| p.ends_with("KeyNote.md")).map(|(_, c)| *c).expect("KeyNote.md row");
    assert_ne!(normal_col, ignored_col, "ignored note must render dimmed vs normal note");

    shot(&mut h, "git_tree_statusbar");
}

#[test]
fn git_live_edit_save_updates_status_and_diff_gutter() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let note = notes.join("GutterNote.md");
    std::fs::write(&note, "# Gutter Note\nline 2\nline 3\nline 4\nline 5\n").unwrap();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    st(&mut h).commands.push(Command::OpenPath { path: note.clone(), new_tab: true, line: None });
    settle(&mut h);

    let editor_left = |h: &mut H| h.get_by_role(Role::MultilineTextInput).rect().left();
    let clean_left = editor_left(&mut h);
    assert!(h.query_by_label_contains("*").is_none(), "initially clean");

    // Edit in memory: modify line 2, delete line 4, add line at end.
    {
        let doc = st(&mut h).docs.get_mut(&note).expect("doc");
        doc.text = "# Gutter Note\nline 2 edited\nline 3\nline 5\nline 6 added\n".into();
        doc.touch();
    }
    // Wait past 150ms diff debounce + worker diff computation.
    std::thread::sleep(Duration::from_millis(180));
    assert!(
        wait(&mut h, 10, |h| {
            let s = st(h);
            let Some(git) = s.modules.get::<GitModule>(iron_sheets_app_core::modules::GIT) else { return false };
            let Some(doc) = s.docs.get(&note) else { return false };
            let (marks, _) = git.diff_marks(&note, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
            marks.iter().any(|m| m.kind == DiffKind::Modified)
                && marks.iter().any(|m| m.kind == DiffKind::Deleted)
                && marks.iter().any(|m| m.kind == DiffKind::Added)
        }),
        "diff marks (Modified, Deleted, Added) must arrive after debounce"
    );
    settle(&mut h);

    // Editor shifted right by DIFF_W (6 px) even when show_line_numbers == false.
    let diff_only_left = editor_left(&mut h);
    assert!(diff_only_left >= clean_left + 5.0, "diff gutter reserves width when line numbers are off ({clean_left} -> {diff_only_left})");
    shot(&mut h, "git_diff_gutter");

    // Line numbers ON + diff gutter: shifts further without collision.
    st(&mut h).config.show_line_numbers = true;
    settle(&mut h);
    let with_nums_left = editor_left(&mut h);
    assert!(with_nums_left > diff_only_left + 15.0, "line numbers + diff gutter shifts further ({diff_only_left} -> {with_nums_left})");
    shot(&mut h, "git_diff_gutter_with_numbers");

    // Save note -> status bar dirty count updates to *1 and tree status becomes Modified.
    st(&mut h).commands.push(Command::Save);
    assert!(
        wait(&mut h, 10, |h| {
            GitModule::of(st(h)).is_some_and(|g| g.snapshot().status_of(&note) == Some(GitFileStatus::Modified))
                && h.query_by_label("*1").is_some()
        }),
        "saving updates status bar dirty count and M status"
    );
}

#[test]
fn git_external_commit_refreshes_snapshot() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);

    std::fs::write(notes.join("Welcome.md"), "# Welcome\n\nExternal dirty edit.\n").unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    assert!(wait(&mut h, 10, |h| h.query_by_label("*1").is_some()), "dirty count *1 shown");

    // Commit externally via CLI; `.git` metadata stat polling detects it within ~400ms.
    run_git(&notes, &["add", "-A"]);
    run_git(&notes, &["commit", "-m", "external commit"]);

    assert!(
        wait(&mut h, 10, |h| {
            GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.dirty_count == 0))
                && h.query_by_label("*1").is_none()
        }),
        "external git commit refreshes snapshot and clears dirty count"
    );
}

#[test]
fn git_settings_toggle_disables_and_reenables_cleanly() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);
    std::fs::write(notes.join("Welcome.md"), "# Welcome\n\nEdited.\n").unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    st(&mut h).commands.push(Command::OpenPath { path: notes.join("Welcome.md"), new_tab: true, line: None });
    settle(&mut h);
    assert!(h.query_by_label("main").is_some());

    // Open Settings -> Modules -> toggle "Git integration" off.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    st(&mut h).settings.section = Section::Modules;
    settle(&mut h);
    h.get_by_label("Git integration").click();
    settle(&mut h);

    assert!(GitModule::of(st(&mut h)).is_none(), "GitModule dropped when disabled");
    assert!(!st(&mut h).config.module_git);
    st(&mut h).settings.open = false;
    settle(&mut h);

    assert!(h.query_by_label("main").is_none(), "branch removed from statusbar when disabled");
    assert!(h.query_by_label("*1").is_none(), "dirty count removed when disabled");
    shot(&mut h, "git_module_disabled");

    // Re-enable in Settings -> Modules.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    st(&mut h).settings.section = Section::Modules;
    settle(&mut h);
    h.get_by_label("Git integration").click();
    settle(&mut h);
    st(&mut h).settings.open = false;
    wait_git_ready(&mut h);
    assert!(h.query_by_label("main").is_some(), "branch restored when re-enabled");
}

#[test]
fn git_non_git_workspace_root_clean() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    let git = GitModule::of(st(&mut h)).expect("module enabled");
    assert_eq!(git.snapshot().availability, GitAvailability::NotARepo);
    assert_eq!(git.in_flight(), 0);
    assert!(st(&mut h).status.current().is_none());
    assert!(h.query_by_label("Git unavailable").is_none());
}

#[test]
fn git_unborn_repo_and_detached_head() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "Welcome.md"]);

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    assert!(h.query_by_label("main").is_some(), "unborn repo still shows branch name");
    {
        let snap = GitModule::of(st(&mut h)).unwrap().snapshot();
        assert!(snap.repos[0].unborn);
        assert_eq!(snap.status_of(&notes.join("Welcome.md")), Some(GitFileStatus::Added));
    }

    // Open staged Added file in unborn repo -> diff gutter marks all lines Added without crash.
    st(&mut h).commands.push(Command::OpenPath { path: notes.join("Welcome.md"), new_tab: true, line: None });
    assert!(
        wait(&mut h, 10, |h| {
            let s = st(h);
            let Some(git) = s.modules.get::<GitModule>(iron_sheets_app_core::modules::GIT) else { return false };
            let Some(doc) = s.docs.get(&notes.join("Welcome.md")) else { return false };
            let (marks, _) = git.diff_marks(&notes.join("Welcome.md"), doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
            marks.iter().any(|m| m.kind == DiffKind::Added)
        }),
        "staged file in unborn repo marks lines Added"
    );

    // Commit and detach HEAD.
    run_git(&notes, &["commit", "-m", "first"]);
    run_git(&notes, &["checkout", "--detach", "HEAD"]);
    assert!(
        wait(&mut h, 10, |h| {
            GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.detached))
                && h.query_by_label_contains("HEAD (").is_some()
        }),
        "detached HEAD displays HEAD (<oid>)"
    );
}

#[test]
fn git_merge_conflict_status() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let conflict_file = notes.join("Conflict.md");
    std::fs::write(&conflict_file, "base\n").unwrap();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "base"]);

    run_git(&notes, &["checkout", "-b", "side"]);
    std::fs::write(&conflict_file, "side change\n").unwrap();
    run_git(&notes, &["commit", "-am", "side"]);

    run_git(&notes, &["checkout", "main"]);
    std::fs::write(&conflict_file, "main change\n").unwrap();
    run_git(&notes, &["commit", "-am", "main"]);

    let merge = SysCommand::new("git").current_dir(&notes).args(["merge", "side"]).output().unwrap();
    assert!(!merge.status.success(), "merge should conflict");

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    st(&mut h).commands.push(Command::OpenPath { path: conflict_file.clone(), new_tab: true, line: None });
    settle(&mut h);

    let snap = GitModule::of(st(&mut h)).unwrap().snapshot();
    assert_eq!(snap.status_of(&conflict_file), Some(GitFileStatus::Conflict));
    assert_eq!(snap.status_of(&notes), Some(GitFileStatus::Conflict));
}

#[test]
fn git_large_note_over_2mb_diff_gutter_cap() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let big = notes.join("Big.md");
    let content = "0123456789abcdef0123456789abcdef\n".repeat((MAX_DIFF_BYTES / 33) + 500);
    assert!(content.len() > MAX_DIFF_BYTES);
    std::fs::write(&big, &content).unwrap();
    init_git_repo(&notes);
    run_git(&notes, &["add", "Big.md"]);
    run_git(&notes, &["commit", "-m", "big"]);
    std::fs::write(&big, format!("changed\n{content}")).unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    st(&mut h).commands.push(Command::OpenPath { path: big.clone(), new_tab: true, line: None });
    settle(&mut h);

    let s = st(&mut h);
    let git = s.modules.get::<GitModule>(iron_sheets_app_core::modules::GIT).unwrap();
    let doc = s.docs.get(&big).unwrap();
    let (marks, repaint_after) = git.diff_marks(&big, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
    assert!(marks.is_empty(), ">2 MB file must skip diff gutter");
    assert!(repaint_after.is_none());
}

#[test]
fn git_missing_binary_degrades_gracefully() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);

    let mut h = harness(fx.opts(vec![notes]));
    wait_git_ready(&mut h);

    GitModule::of_mut(st(&mut h)).unwrap().set_git_bin_override(Some("/nonexistent-git-binary-for-test".into()));
    assert!(
        wait(&mut h, 10, |h| {
            GitModule::of(st(h)).is_some_and(|g| g.snapshot().availability == GitAvailability::Unavailable)
                && h.query_by_label("Git unavailable").is_some()
        }),
        "missing git binary surfaces clean Unavailable state"
    );
    assert!(st(&mut h).status.current().is_none(), "no noisy status error message");
    assert!(st(&mut h).dialogs.current.is_none(), "no modal error dialog");
}

#[test]
fn git_idle_zero_repaints_and_accessibility_check() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    std::fs::write(notes.join(".gitignore"), "*.tmp\n").unwrap();
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nModified.\n").unwrap();
    std::fs::write(notes.join("Untracked.md"), "# New\n").unwrap();
    std::fs::write(notes.join("ignored.tmp"), "tmp").unwrap();

    let opts = fx.opts(vec![notes.clone()]);
    iron_sheets::theme::set_row_recording(true);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 720.0))
        .with_step_dt(0.25)
        .with_accessibility_check(true)
        .build_ui_state(
            |ui, slot: &mut Slot| {
                if slot.app.is_none() {
                    if let Some(o) = slot.opts.take() {
                        slot.app = Some(iron_sheets::App::new(ui.ctx(), o));
                    }
                }
                slot.app().frame(ui);
            },
            Slot { opts: Some(opts), app: None },
        );
    wait_git_ready(&mut h);

    st(&mut h).commands.push(Command::OpenPath { path: notes.join("Ideas.md"), new_tab: true, line: None });
    std::thread::sleep(Duration::from_millis(180));
    settle(&mut h);

    // Open Settings -> Modules under accessibility check.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    st(&mut h).settings.section = Section::Modules;
    settle(&mut h);
    st(&mut h).settings.open = false;
    settle(&mut h);

    // Settled git repo must ask for 0 immediate repaints when idle.
    let mut immediate = 0usize;
    for _ in 0..20 {
        h.step();
        immediate += usize::from(wants_immediate_repaint(&h));
    }
    assert_eq!(immediate, 0, "settled git UI must request 0 immediate repaints");
}

#[test]
fn git_txt_keystroke_schedules_debounce_repaint_without_animations_or_autosave() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let txt = notes.join("Plain.txt");
    std::fs::write(&txt, "line 1\nline 2\nline 3\n").unwrap();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);

    let mut opts = fx.opts(vec![notes.clone()]);
    opts.config.animations = false;
    opts.config.autosave = false;
    opts.config.show_other_files = true;
    let mut h = harness(opts);
    wait_git_ready(&mut h);

    st(&mut h).commands.push(Command::OpenPath { path: txt.clone(), new_tab: true, line: None });
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.active_tab().map(|t| t.path.clone()), Some(txt.clone()));

    // Send a single keystroke frame to the tracked .txt file.
    h.event(egui::Event::Text("x".into()));
    h.step();

    let min_delay = h
        .output()
        .viewport_output
        .values()
        .map(|v| v.repaint_delay)
        .min()
        .expect("viewport output");
    assert!(
        min_delay <= Duration::from_millis(160),
        "tracked .txt keystroke must schedule <= 160ms debounce repaint with animations=false & autosave=false (got {min_delay:?})"
    );

    // Wait past 150ms diff debounce and verify diff gutter marks arrive cleanly.
    std::thread::sleep(Duration::from_millis(180));
    assert!(
        wait(&mut h, 10, |h| {
            let s = st(h);
            let Some(git) = s.modules.get::<GitModule>(iron_sheets_app_core::modules::GIT) else { return false };
            let Some(doc) = s.docs.get(&txt) else { return false };
            let (marks, _) = git.diff_marks(&txt, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
            !marks.is_empty()
        }),
        "diff gutter marks must arrive for tracked .txt after debounce"
    );
    settle(&mut h);

    // Verify 0 immediate repaints when settled.
    let mut immediate = 0usize;
    for _ in 0..20 {
        h.step();
        immediate += usize::from(wants_immediate_repaint(&h));
    }
    assert_eq!(immediate, 0, "settled .txt git editor must request 0 immediate repaints");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = SysCommand::new("git").current_dir(dir).args(args).output().expect("spawn git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn snap_changes(h: &mut H) -> Vec<(String, bool, bool)> {
    GitModule::of(st(h)).and_then(|g| g.snapshot().repos.first().map(|r| r.changes.iter().map(|c| (c.rel.clone(), c.staged(), c.unstaged())).collect())).unwrap_or_default()
}

#[test]
fn git_panel_stage_by_keyboard_and_mouse_commit_and_failed_push() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "Welcome.md", "Ideas.md"]);
    run_git(&notes, &["commit", "-m", "initial commit"]);
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nChanged.\n").unwrap();
    std::fs::write(notes.join("Untracked.md"), "# New\n").unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    wait_git_ready(&mut h);
    assert!(wait(&mut h, 10, |h| snap_changes(h).len() >= 2), "status lists both changes");
    st(&mut h).commands.push(Command::SetSidebarView(SidebarView::Git));
    settle(&mut h);
    assert_eq!(st(&mut h).sidebar_view, SidebarView::Git);
    shot(&mut h, "git_panel_changes");

    // Keyboard: the list has focus, Down selects the first file, Space stages it.
    h.key_press(egui::Key::ArrowDown);
    h.key_press(egui::Key::Space);
    assert!(wait(&mut h, 10, |h| snap_changes(h).iter().any(|c| c.0 == "Ideas.md" && c.1)), "Space staged Ideas.md");
    settle(&mut h);
    // The selection followed the file into the Staged section: Space again unstages it, and once more stages it.
    h.key_press(egui::Key::Space);
    assert!(wait(&mut h, 10, |h| snap_changes(h).iter().any(|c| c.0 == "Ideas.md" && !c.1)), "Space unstaged Ideas.md");
    settle(&mut h);
    h.key_press(egui::Key::Space);
    assert!(wait(&mut h, 10, |h| snap_changes(h).iter().any(|c| c.0 == "Ideas.md" && c.1)), "Space staged Ideas.md again");
    settle(&mut h);

    // Mouse: the row's + button.
    h.get_by_label("Stage Untracked.md").click();
    let two_staged = |h: &mut H| snap_changes(h).iter().filter(|c| c.1).map(|c| c.0.clone()).collect::<Vec<_>>() == ["Ideas.md", "Untracked.md"];
    assert!(wait(&mut h, 10, two_staged), "both staged");
    settle(&mut h);

    h.get_by_label("Commit message").click();
    h.get_by_label("Commit message").type_text("two notes from the panel");
    settle(&mut h);
    shot(&mut h, "git_panel_staged_with_message");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    assert!(
        wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Commit") && snap_changes(h).iter().all(|c| !c.1)),
        "committed through Ctrl+Enter"
    );
    settle(&mut h);
    assert_eq!(git_out(&notes, &["log", "-1", "--format=%s"]), "two notes from the panel");
    assert_eq!(git_out(&notes, &["show", "--name-only", "--format=", "HEAD"]), "Ideas.md\nUntracked.md");
    assert_eq!(snap_changes(&mut h).len(), 3, "the other untracked files stay");

    // Stage all, commit with the button: the clean state is a designed empty state.
    h.get_by_label("Stage all").click();
    assert!(wait(&mut h, 10, |h| snap_changes(h).iter().all(|c| c.1)), "all staged");
    settle(&mut h);
    h.get_by_label("Commit message").click();
    h.get_by_label("Commit message").type_text("the rest");
    settle(&mut h);
    h.get_by_label_contains("Commit (").click();
    assert!(wait(&mut h, 10, |h| snap_changes(h).is_empty() && h.query_by_label("Working tree clean. Nothing to commit.").is_some()), "empty state after the commit");
    shot(&mut h, "git_panel_clean");

    // Push without a remote fails with git's line and the terminal hint, in the panel and the status bar.
    h.get_by_label("Push").click();
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Push" && o.result.is_err())), "push failed");
    settle(&mut h);
    let msg = st(&mut h).status.current().map(|m| (m.error, m.text.clone()));
    assert!(msg.as_ref().is_some_and(|(e, t)| *e && t.contains("Push failed: fatal:") && t.contains("`git push` in a terminal")), "{msg:?}");
    assert!(GitModule::of(st(&mut h)).unwrap().busy(&notes).is_none());
    shot(&mut h, "git_panel_push_failed");
}
