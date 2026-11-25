use super::driver::*;
#[cfg(feature = "git")]
use egui::accesskit::Role;
use egui::{Key, Modifiers};
#[cfg(feature = "git")]
use egui_kittest::kittest::{NodeT, Queryable};
#[cfg(not(feature = "git"))]
use egui_kittest::kittest::Queryable;
#[cfg(feature = "git")]
use std::time::Duration;

#[cfg(feature = "git")]
fn tree_item_desc(h: &H, label: &str) -> Option<String> {
    h.query_by_role_and_label(Role::TreeItem, label)
        .and_then(|n| n.accesskit_node().description())
}

#[cfg(feature = "git")]
#[test]
fn t1_f11_git_repo_branch_and_tree_badges() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    init_git_repo(&notes);
    std::fs::write(notes.join(".gitignore"), "*.tmp\n").expect("write .gitignore");
    run_git(&notes, &["add", ".gitignore", "Welcome.md", "Ideas.md", "Reading.md", "Projects"]);
    run_git(&notes, &["commit", "-m", "initial commit"]);

    // Create M, A, and ? states.
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nModified content.\n").expect("modify");
    std::fs::write(notes.join("Added.md"), "# Added\n").expect("write Added");
    run_git(&notes, &["add", "Added.md"]);
    std::fs::write(notes.join("Untracked.md"), "# Untracked\n").expect("write Untracked");

    let mut h = launch(fx.opts(vec![notes.clone()]));
    wait_until(&mut h, Duration::from_secs(10), "statusbar branch and dirty count", |h| {
        h.query_by_label("main").is_some() && h.query_by_label_contains("*").is_some()
    });

    assert_eq!(tree_item_desc(&h, "Ideas.md").as_deref(), Some("modified"));
    assert_eq!(tree_item_desc(&h, "Added.md").as_deref(), Some("added"));
    assert_eq!(tree_item_desc(&h, "Untracked.md").as_deref(), Some("untracked"));
    assert_eq!(tree_item_desc(&h, "Welcome.md"), None);
}

#[cfg(feature = "git")]
#[test]
fn t1_f11_git_ignored_files_dimmed() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    init_git_repo(&notes);
    std::fs::write(notes.join(".gitignore"), "*.tmp\nsecret/\n").expect("write .gitignore");
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "initial"]);

    // One tracked dirty file + ignored folder/note.
    std::fs::write(notes.join("Ideas.md"), "# Ideas\n\nDirty.\n").expect("modify");
    std::fs::create_dir_all(notes.join("secret")).expect("mkdir secret");
    std::fs::write(notes.join("secret/KeyNote.md"), "# Secret\n").expect("write KeyNote");

    let mut h = launch(fx.opts(vec![notes.clone()]));
    wait_until(&mut h, Duration::from_secs(10), "git ready", |h| {
        h.query_by_label("main").is_some() && h.query_by_label("*1").is_some()
    });

    click_tree_item(&mut h, "secret");
    wait_until(&mut h, Duration::from_secs(5), "KeyNote.md visible and ignored", |h| {
        tree_item_desc(h, "KeyNote.md").as_deref() == Some("ignored")
    });

    let rows = iron_sheets::theme::recorded_rows(&h.ctx, "files");
    let normal_col = rows
        .iter()
        .find(|(p, _)| p.ends_with("Welcome.md"))
        .map(|(_, c)| *c)
        .expect("Welcome.md recorded row");
    let ignored_col = rows
        .iter()
        .find(|(p, _)| p.ends_with("KeyNote.md"))
        .map(|(_, c)| *c)
        .expect("KeyNote.md recorded row");
    assert_ne!(normal_col, ignored_col, "ignored note must render dimmed vs normal note");
    assert!(h.query_by_label("*1").is_some(), "ignored files do not inflate dirty count");
}

#[cfg(feature = "git")]
#[test]
fn t1_f11_git_live_edit_save_updates_status_and_gutter() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    let note = notes.join("GutterNote.md");
    std::fs::write(&note, "# Gutter Note\nline 2\nline 3\nline 4\nline 5\n").expect("write");
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);

    let mut h = launch(fx.opts(vec![notes.clone()]));
    wait_until(&mut h, Duration::from_secs(10), "git ready", |h| {
        h.query_by_label("main").is_some()
    });
    click_tree_item(&mut h, "GutterNote.md");
    let clean_left = editor_rect(&h).left();
    assert!(h.query_by_label_contains("*").is_none(), "repo initially clean");

    // Edit in editor and save; wait for 8 px diff gutter shift + statusbar *1 + modified badge.
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    type_text(&mut h, "Edited ");
    press_shortcut(&mut h, Modifiers::COMMAND, Key::S);

    wait_until(&mut h, Duration::from_secs(10), "diff gutter shift, *1, and modified badge", |h| {
        editor_rect(h).left() >= clean_left + 7.5
            && h.query_by_label("*1").is_some()
            && tree_item_desc(h, "GutterNote.md").as_deref() == Some("modified")
    });

    let ed_r = editor_rect(&h);
    let top_y = ed_r.top().max(74.0);
    let strip = egui::Rect::from_min_max(
        egui::pos2((clean_left - 2.0).max(0.0), top_y),
        egui::pos2(ed_r.left() + 1.0, (top_y + 120.0).min(ed_r.bottom())),
    );
    let img = render_pixels(&mut h);
    assert!(
        distinct_colors_in_rect(&img, strip, h.ctx.pixels_per_point()) >= 2,
        "diff gutter strip must paint marker pixels left of editor (clean_left={clean_left}, ed_left={}, top_y={top_y})",
        ed_r.left()
    );
}

#[cfg(feature = "git")]
#[test]
fn t1_f11_git_external_commit_refreshes_snapshot() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);
    std::fs::write(notes.join("Welcome.md"), "# Welcome\n\nExternal change.\n").expect("write");

    let mut h = launch(fx.opts(vec![notes.clone()]));
    wait_until(&mut h, Duration::from_secs(10), "dirty *1 and modified badge", |h| {
        h.query_by_label("*1").is_some() && tree_item_desc(h, "Welcome.md").as_deref() == Some("modified")
    });

    run_git(&notes, &["add", "-A"]);
    run_git(&notes, &["commit", "-m", "external commit"]);

    wait_until(&mut h, Duration::from_secs(10), "dirty count and badge cleared after external commit", |h| {
        h.query_by_label("*1").is_none() && tree_item_desc(h, "Welcome.md").is_none()
    });
}

#[cfg(feature = "git")]
#[test]
fn t1_f11_git_settings_toggle_disables_cleanly() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);
    std::fs::write(notes.join("Welcome.md"), "# Welcome\n\nEdited.\n").expect("write");

    let mut h = launch(fx.opts(vec![notes.clone()]));
    wait_until(&mut h, Duration::from_secs(10), "git status and tab active", |h| {
        h.query_by_label("main").is_some()
            && h.query_by_label("*1").is_some()
            && h.query_by_label("Git").is_some()
    });

    // Open Settings -> Modules -> toggle "Git integration" off.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    click_label(&mut h, "Modules");
    click_label(&mut h, "Git integration");
    press_key(&mut h, Key::Escape);

    assert!(h.query_by_label("main").is_none(), "branch removed from statusbar");
    assert!(h.query_by_label("*1").is_none(), "dirty count removed from statusbar");
    assert!(h.query_by_label("Git").is_none(), "Git sidebar tab removed");

    // Re-enable in Settings -> Modules.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    click_label(&mut h, "Modules");
    click_label(&mut h, "Git integration");
    press_key(&mut h, Key::Escape);

    wait_until(&mut h, Duration::from_secs(10), "git status and tab restored", |h| {
        h.query_by_label("main").is_some()
            && h.query_by_label("*1").is_some()
            && h.query_by_label("Git").is_some()
    });
}

#[cfg(not(feature = "git"))]
#[test]
fn t1_f11_non_git_fallback() {
    let _env = read_env_lock();
    if !has_git() {
        return;
    }
    let fx = new_fx();
    let notes = fx.notes();
    init_git_repo(&notes);
    run_git(&notes, &["add", "."]);
    run_git(&notes, &["commit", "-m", "init"]);
    std::fs::write(notes.join("Welcome.md"), "# Welcome\n\nEdited.\n").expect("write");

    let mut h = launch(fx.opts(vec![notes]));
    assert!(h.query_by_label("main").is_none());
    assert!(h.query_by_label("Git").is_none());
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    click_label(&mut h, "Modules");
    assert!(h.query_by_label("Git integration").is_none());
}
