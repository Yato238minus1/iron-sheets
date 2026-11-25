//! Git v2 UI: initialize repository, ahead/behind, discard with backup and undo, branch picker, remotes dialog.
use crate::common::*;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use iron_sheets::command::Command;
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_app_core::git::{GitAvailability, GitModule};
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

fn git(dir: &Path, args: &[&str]) -> String {
    let out = SysCommand::new("git").current_dir(dir).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Iron Sheets Test"]);
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

fn availability(h: &mut H) -> GitAvailability {
    GitModule::of(st(h)).map_or(GitAvailability::Unavailable, |g| g.snapshot().availability)
}

fn open_git_panel(h: &mut H) {
    st(h).commands.push(Command::SetSidebarView(SidebarView::Git));
    settle(h);
}

#[test]
fn git_panel_without_a_repo_offers_initialize_and_switches_to_changes() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| !GitModule::of(st(h)).is_some_and(|g| g.scanning())), "first scan done");
    assert_eq!(availability(&mut h), GitAvailability::NotARepo);
    assert!(h.query_by_label("Git").is_some(), "the Git tab shows without a repository");
    shot(&mut h, "git_tab_without_repo");
    h.get_by_label("Git").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("not Git repositories").is_some(), "headline");
    shot(&mut h, "git_panel_no_repo");

    // Keyboard: focus the button, Enter runs `git init` on the worker.
    h.get_by_label("Initialize repository in notes").focus();
    h.key_press(egui::Key::Enter);
    assert!(wait(&mut h, 10, |h| availability(h) == GitAvailability::Ready), "snapshot turns Ready");
    settle(&mut h);
    assert!(notes.join(".git").is_dir());
    assert!(h.query_by_label("Initialize repository in notes").is_none(), "the panel switched to the changes view");
    assert!(h.query_by_label("Commit message").is_some());
    shot(&mut h, "git_panel_after_init");
}

#[test]
fn git_init_failure_shows_git_line_and_keeps_the_button() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| !GitModule::of(st(h)).is_some_and(|g| g.scanning())));
    // `.git` as a plain file: git refuses to init there, and the scan does not see a repo either.
    std::fs::write(notes.join(".git"), "not a gitdir").unwrap();
    open_git_panel(&mut h);
    h.get_by_label("Initialize repository in notes").click();
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Init" && o.result.is_err())), "init failed");
    settle(&mut h);
    assert!(h.query_by_label("Initialize repository in notes").is_some(), "retry stays possible");
    shot(&mut h, "git_panel_init_failed");
}

#[test]
fn git_ahead_behind_in_status_bar_and_panel() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_repo(&notes);
    git(&notes, &["add", "Welcome.md"]);
    git(&notes, &["commit", "-m", "first"]);
    let bare = fx.dir.path().join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "-b", "main"]);
    git(&notes, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&notes, &["push", "-u", "origin", "main"]);
    git(&notes, &["commit", "--allow-empty", "-m", "local"]);

    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.ahead == 1))), "ahead 1 from the status");
    settle(&mut h);
    assert!(h.query_by_label("ahead 1").is_some(), "status bar shows the ahead count");
    open_git_panel(&mut h);
    assert!(h.query_all_by_label("ahead 1").count() >= 2, "panel header too");
    shot(&mut h, "git_panel_ahead");
}

#[test]
fn git_discard_asks_backs_up_and_undo_restores() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_repo(&notes);
    git(&notes, &["add", "Welcome.md", "Ideas.md"]);
    git(&notes, &["commit", "-m", "first"]);
    let orig = std::fs::read_to_string(notes.join("Ideas.md")).unwrap();
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nMy unsaved thoughts.\n").unwrap();
    std::fs::write(notes.join("Scratch.md"), "scratch\n").unwrap();

    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.changes.len() >= 2))));
    open_git_panel(&mut h);

    // Cancel does nothing.
    h.get_by_label("Discard Ideas.md").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Discard the changes in Ideas.md").is_some(), "confirm text names the file");
    shot(&mut h, "git_discard_confirm");
    h.get_by_label("Cancel").click();
    settle(&mut h);
    assert_eq!(std::fs::read_to_string(notes.join("Ideas.md")).unwrap(), "# Ideas\n\nMy unsaved thoughts.\n");

    // Confirm: file back at HEAD, a toast says where the copy is, Undo brings the edit back.
    h.get_by_label("Discard Ideas.md").click();
    settle(&mut h);
    h.get_by_label("Discard").click();
    assert!(wait(&mut h, 10, |_| std::fs::read_to_string(notes.join("Ideas.md")).is_ok_and(|t| t == orig)), "restored to HEAD");
    settle(&mut h);
    let toast = st(&mut h).notice.as_ref().map(|n| (n.text.clone(), n.action.as_ref().map(|a| a.0)));
    let (text, action) = toast.expect("toast");
    assert!(text.contains("Backup: ") && action == Some("Undo"), "{text}");
    let backup = Path::new(text.split("Backup: ").nth(1).unwrap_or_default()).to_path_buf();
    assert!(backup.is_file() && !backup.starts_with(&notes), "backup outside the repo: {}", backup.display());
    shot(&mut h, "git_discard_toast");
    st(&mut h).commands.push(Command::Module { id: "git.undo_discard", arg: notes.to_string_lossy().into_owned() });
    assert!(wait(&mut h, 10, |_| std::fs::read_to_string(notes.join("Ideas.md")).is_ok_and(|t| t.contains("unsaved thoughts"))), "undo restored the edit");

    // An untracked file: the confirm says it is deleted. Wait for the status that follows the undo first.
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.changes.iter().any(|c| c.rel == "Ideas.md")))), "status after the undo");
    settle(&mut h);
    h.get_by_label("Discard Scratch.md").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Delete the untracked file Scratch.md").is_some());
    h.get_by_label("Discard").click();
    assert!(wait(&mut h, 10, |_| !notes.join("Scratch.md").exists()), "untracked file deleted");
    let _ = std::fs::remove_dir_all(backup.ancestors().nth(2).unwrap_or(&backup));
}

fn branch_now(notes: &Path) -> String {
    git(notes, &["branch", "--show-current"])
}

fn branch_names(h: &mut H, root: &Path) -> Vec<String> {
    match GitModule::of(st(h)).and_then(|g| g.branches(root)) {
        Some(Ok(l)) => l.iter().map(|b| b.name.clone()).collect(),
        _ => Vec::new(),
    }
}

fn picker_open(h: &mut H) -> bool {
    st(h).ui.git_dialogs.is_open()
}

#[test]
fn git_branch_picker_switches_creates_deletes_and_shows_git_refusal() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_repo(&notes);
    git(&notes, &["add", "Welcome.md", "Ideas.md"]);
    git(&notes, &["commit", "-m", "first"]);
    git(&notes, &["switch", "-c", "feat-a"]);
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\non feat-a\n").unwrap();
    git(&notes, &["commit", "-am", "work on feat-a"]);
    git(&notes, &["switch", "-c", "merged-b"]);
    git(&notes, &["switch", "main"]);

    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| availability(h) == GitAvailability::Ready));
    settle(&mut h);

    // The status bar branch opens the picker: first row preselected, the current branch marked.
    h.get_by_label("Switch Git branch").click();
    assert!(wait(&mut h, 10, |h| branch_names(h, &notes).len() == 3 && picker_open(h)), "list loaded");
    settle(&mut h);
    shot(&mut h, "git_branch_picker");
    h.get_by_label("Branch").type_text("feat");
    settle(&mut h);
    assert!(h.query_by_label("Create branch feat").is_some(), "a partial name can still become a new branch");
    shot(&mut h, "git_branch_picker_filtered");

    // Dirty tree: git refuses the switch with its first error line, nothing changes.
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nlocal edit\n").unwrap();
    h.key_press(egui::Key::Enter);
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Switch branch" && o.result.is_err())), "refused");
    settle(&mut h);
    assert!(!picker_open(&mut h), "the picker closed");
    let msg = st(&mut h).status.current().map(|m| (m.error, m.text.clone()));
    assert!(msg.as_ref().is_some_and(|(e, t)| *e && t.contains("would be overwritten")), "{msg:?}");
    assert_eq!(branch_now(&notes), "main");
    shot(&mut h, "git_branch_switch_refused");
    git(&notes, &["checkout", "--", "Ideas.md"]);

    // Enter switches; the tree follows.
    st(&mut h).commands.push(Command::Module { id: "git.branches", arg: notes.to_string_lossy().into_owned() });
    settle(&mut h);
    h.get_by_label("Branch").type_text("feat-a");
    settle(&mut h);
    h.key_press(egui::Key::Enter);
    assert!(wait(&mut h, 10, |_| branch_now(&notes) == "feat-a"), "switched");
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.first().is_some_and(|r| r.branch == "feat-a"))));
    assert!(std::fs::read_to_string(notes.join("Ideas.md")).unwrap().contains("on feat-a"));

    // A new name creates a branch from the current one.
    st(&mut h).commands.push(Command::Module { id: "git.branches", arg: notes.to_string_lossy().into_owned() });
    settle(&mut h);
    h.get_by_label("Branch").type_text("topic");
    settle(&mut h);
    h.key_press(egui::Key::Enter);
    assert!(wait(&mut h, 10, |_| branch_now(&notes) == "topic"), "created and switched");

    // Alt+Delete asks first; a merged branch goes, the picker stays.
    st(&mut h).commands.push(Command::Module { id: "git.branches", arg: notes.to_string_lossy().into_owned() });
    settle(&mut h);
    h.get_by_label("Branch").type_text("merged-b");
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::Delete);
    settle(&mut h);
    assert!(h.query_by_label_contains("Delete the branch merged-b").is_some());
    shot(&mut h, "git_branch_delete_confirm");
    h.get_by_label("Delete").click();
    assert!(wait(&mut h, 10, |h| !branch_names(h, &notes).contains(&"merged-b".to_string())), "deleted");
    assert!(picker_open(&mut h));
    // Esc closes it.
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!picker_open(&mut h));
}

#[test]
fn git_remotes_dialog_adds_validates_fetches_renames_and_removes() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    init_repo(&notes);
    git(&notes, &["add", "Welcome.md"]);
    git(&notes, &["commit", "-m", "first"]);
    let bare = fx.dir.path().join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "--bare", "-b", "main"]);

    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| availability(h) == GitAvailability::Ready));
    open_git_panel(&mut h);
    h.get_by_label("Remotes...").click();
    assert!(wait(&mut h, 10, |h| picker_open(h) && GitModule::of(st(h)).and_then(|g| g.remote_list(&notes)).is_some()));
    settle(&mut h);
    assert!(h.query_by_label("No remotes yet. Add one below.").is_some(), "designed empty state");
    shot(&mut h, "git_remotes_empty");

    // Validation is inline, and nothing runs for a URL that would run a command.
    h.get_by_label("Remote name").type_text("origin");
    h.get_by_label("Remote URL").click();
    h.get_by_label("Remote URL").type_text("ext::sh -c touch pwned");
    settle(&mut h);
    shot(&mut h, "git_remotes_invalid_url");
    assert!(h.query_by_label("This URL would run a command").is_some());
    assert!(h.get_by_label("Add remote").accesskit_node().is_disabled(), "Add is disabled");
    h.get_by_label("Remote URL").click();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Remote URL").type_text(bare.to_str().unwrap());
    settle(&mut h);
    h.get_by_label("Add remote").click();
    assert!(wait(&mut h, 10, |h| matches!(GitModule::of(st(h)).and_then(|g| g.remote_list(&notes)), Some(Ok(l)) if l.len() == 1)), "added");
    settle(&mut h);
    assert_eq!(git(&notes, &["remote"]), "origin");
    shot(&mut h, "git_remotes_listed");

    // Fetch (local bare remote): works, the line says so; a vanished remote fails with git's line.
    h.get_by_label("Fetch origin").click();
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Fetch" && o.result.is_ok())), "fetched");
    settle(&mut h);
    assert!(h.query_all_by_label("Fetched").count() >= 1, "dialog line and status bar say so");
    std::fs::remove_dir_all(&bare).unwrap();
    h.get_by_label("Fetch origin").click();
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).and_then(|g| g.last_outcome().cloned()).is_some_and(|o| o.op == "Fetch" && o.result.is_err())), "failed fetch");
    settle(&mut h);
    shot(&mut h, "git_remotes_fetch_failed");

    // Rename, then remove with a confirm.
    h.get_by_label("Rename remote origin").click();
    settle(&mut h);
    h.get_by_label("New remote name").click();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("New remote name").type_text("upstream");
    settle(&mut h);
    h.get_by_label("Rename to this name").click();
    assert!(wait(&mut h, 10, |h| matches!(GitModule::of(st(h)).and_then(|g| g.remote_list(&notes)), Some(Ok(l)) if l.iter().any(|r| r.name == "upstream"))), "renamed");
    settle(&mut h);
    h.get_by_label("Remove remote upstream").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Remove the remote upstream").is_some());
    h.get_by_label("Remove").click();
    assert!(wait(&mut h, 10, |h| matches!(GitModule::of(st(h)).and_then(|g| g.remote_list(&notes)), Some(Ok(l)) if l.is_empty())), "removed");
    settle(&mut h);
    assert!(git(&notes, &["remote"]).is_empty());
    assert!(h.query_by_label("No remotes yet. Add one below.").is_some());
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(!picker_open(&mut h));
}

#[test]
fn git_panel_lists_repos_found_below_the_workspace_folder_with_their_own_status() {
    if !has_git() {
        return;
    }
    let fx = Fx::new();
    let notes = fx.notes();
    // The workspace folder is no repository; two folders below it are, one inside another subfolder.
    for (sub, file) in [("projects/foo", "foo.md"), ("bar", "bar.md")] {
        let dir = notes.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);
        std::fs::write(dir.join(file), "one\ntwo\nthree\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-m", "first"]);
        std::fs::write(dir.join(file), "one\nTWO\nthree\n").unwrap();
    }
    let mut h = harness(fx.opts(vec![notes.clone()]));
    assert!(wait(&mut h, 10, |h| GitModule::of(st(h)).is_some_and(|g| g.snapshot().repos.len() == 2)), "both repos found after the first frame");
    assert_eq!(availability(&mut h), GitAvailability::Ready);
    open_git_panel(&mut h);
    for label in ["Repository notes/projects/foo", "Repository notes/bar"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert!(h.query_by_label("foo.md M not staged").is_some() && h.query_by_label("bar.md M not staged").is_some(), "files of both repos are listed");
    shot(&mut h, "git_panel_nested_repos");

    // A header click moves the commit box to that repo.
    h.get_by_label("Repository notes/bar").click();
    settle(&mut h);
    shot(&mut h, "git_panel_nested_repo_picked");
    assert!(h.query_by_label_contains("Commit (0)").is_some());

    // A note inside a nested repo gets that repo's gutter marks.
    let note = notes.join("projects/foo/foo.md");
    st(&mut h).commands.push(Command::OpenPath { path: note.clone(), new_tab: true, line: None });
    settle(&mut h);
    assert!(
        wait(&mut h, 10, |h| {
            let s = st(h);
            let (Some(g), Some(doc)) = (GitModule::of(s), s.docs.get(&note)) else { return false };
            let (marks, _) = g.diff_marks(&note, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
            marks.len() == 1
        }),
        "gutter marks come from the nested repo"
    );
    settle(&mut h);
    shot(&mut h, "git_nested_repo_gutter");
}
