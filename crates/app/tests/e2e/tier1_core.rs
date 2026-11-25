use super::driver::*;
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use std::time::Duration;

// ============================================================================
// F1. Multi-Root Workspaces & Virtualized File Tree
// ============================================================================

#[test]
fn t1_f1_multi_root_launch() {
    let _env = read_env_lock();
    let fx = new_fx();
    let h = launch(fx.opts(vec![fx.notes(), fx.journal()]));

    assert!(h.query_by_role_and_label(Role::TreeItem, "notes").is_some());
    assert!(h.query_by_role_and_label(Role::TreeItem, "journal").is_some());
    assert!(h.query_by_role_and_label(Role::TreeItem, "Welcome.md").is_some());
    assert!(h.query_by_role_and_label(Role::TreeItem, "2024-01-01.md").is_some());
}

#[test]
fn t1_f1_add_root_drop_and_remove_undo() {
    let _env = read_env_lock();
    let fx = new_fx();
    let extra = fx.dir.path().join("extra");
    std::fs::create_dir_all(&extra).expect("mkdir extra");
    std::fs::write(extra.join("ExtraNote.md"), "# Extra\n").expect("write ExtraNote");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    assert!(h.query_by_role_and_label(Role::TreeItem, "extra").is_none());

    drop_files(&mut h, vec![extra]);
    wait_until(&mut h, Duration::from_secs(5), "dropped root visible", |h| {
        h.query_by_role_and_label(Role::TreeItem, "extra").is_some()
    });

    h.get_by_role_and_label(Role::TreeItem, "extra").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Remove folder from workspace");
    assert!(h.query_by_role_and_label(Role::TreeItem, "extra").is_none());
    assert!(h.query_by_label_contains("Removed extra from workspace").is_some());

    click_label(&mut h, "Undo");
    wait_until(&mut h, Duration::from_secs(5), "extra root restored by Undo", |h| {
        h.query_by_role_and_label(Role::TreeItem, "extra").is_some()
    });
}

#[test]
fn t1_f1_workspace_switch_create_rename() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));

    click_label(&mut h, "Workspace");
    click_label(&mut h, "New workspace...");
    type_text(&mut h, "Work2");
    click_label(&mut h, "OK");
    wait_until(&mut h, Duration::from_secs(5), "switched to Work2", |h| {
        h.query_by_label("Work2").is_some()
            && h.query_by_label("No folders in this workspace yet.").is_some()
    });

    click_label(&mut h, "Workspace");
    click_label(&mut h, "Rename workspace...");
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "RenamedWs");
    click_label(&mut h, "OK");
    wait_until(&mut h, Duration::from_secs(5), "workspace renamed", |h| {
        h.query_by_label("RenamedWs").is_some()
    });

    click_label(&mut h, "Workspace");
    click_label(&mut h, "Switch workspace");
    click_label(&mut h, "test");
    wait_until(&mut h, Duration::from_secs(5), "switched back to test", |h| {
        h.query_by_role_and_label(Role::TreeItem, "notes").is_some()
            && h.query_by_role_and_label(Role::TreeItem, "journal").is_some()
    });
}

#[test]
fn t1_f1_tree_crud_new_rename_trash() {
    #[cfg(target_os = "linux")]
    let _env = write_env_lock();
    #[cfg(not(target_os = "linux"))]
    let _env = read_env_lock();
    let fx = new_fx();
    #[cfg(target_os = "linux")]
    let _xdg = {
        let xdg = fx.dir.path().join("xdg_data");
        std::fs::create_dir_all(&xdg).expect("mkdir xdg_data");
        EnvVarGuard::set("XDG_DATA_HOME", &xdg)
    };
    let mut h = launch(fx.opts(vec![fx.notes()]));

    // New note via tree context menu on root.
    h.get_by_role_and_label(Role::TreeItem, "notes").click_secondary();
    settle(&mut h);
    click_label(&mut h, "New note");
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "CreatedNote");
    click_label(&mut h, "OK");
    let created = fx.notes().join("CreatedNote.md");
    wait_until(&mut h, Duration::from_secs(5), "CreatedNote.md on disk", |_| created.exists());

    // New folder via tree context menu on root.
    h.get_by_role_and_label(Role::TreeItem, "notes").click_secondary();
    settle(&mut h);
    click_label(&mut h, "New folder");
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "CreatedDir");
    click_label(&mut h, "OK");
    let created_dir = fx.notes().join("CreatedDir");
    wait_until(&mut h, Duration::from_secs(5), "CreatedDir on disk", |_| created_dir.is_dir());

    // Rename note via F2.
    click_tree_item(&mut h, "CreatedNote.md");
    press_key(&mut h, Key::F2);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "RenamedNote");
    click_label(&mut h, "OK");
    let renamed = fx.notes().join("RenamedNote.md");
    wait_until(&mut h, Duration::from_secs(5), "RenamedNote.md on disk", |_| {
        renamed.exists() && !created.exists()
    });

    // Move to trash with confirmation dialog.
    h.get_by_role_and_label(Role::TreeItem, "RenamedNote.md").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Move to trash");
    assert!(h.query_all_by_role(Role::Dialog).next().is_some(), "trash confirmation dialog shown");
    h.get_by_role_and_label(Role::Button, "Move to trash").click();
    settle(&mut h);
    wait_until(&mut h, Duration::from_secs(8), "RenamedNote.md moved to trash", |h| {
        assert!(
            h.query_by_label_contains("Cannot move to trash").is_none(),
            "trash move failed unexpectedly"
        );
        !renamed.exists()
    });
    assert!(
        !renamed.exists() && h.query_by_label_contains("Cannot move to trash").is_none(),
        "RenamedNote.md must be moved to trash without error"
    );
}

#[test]
fn t1_f1_tree_sort_and_focus_folder() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));

    click_label(&mut h, "Name, asc.");
    click_label(&mut h, "Date modified");
    click_label(&mut h, "Descending");
    press_key(&mut h, Key::Escape);
    assert!(h.query_by_label("Modified, desc.").is_some());

    h.get_by_role_and_label(Role::TreeItem, "Projects").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Show only this folder");
    assert!(h.query_by_label("Show the whole tree (Esc)").is_some());
    assert!(h.query_by_role_and_label(Role::TreeItem, "Welcome.md").is_none());
    assert!(h.query_by_role_and_label(Role::TreeItem, "Plan.md").is_some());

    click_label(&mut h, "Show the whole tree (Esc)");
    wait_until(&mut h, Duration::from_secs(5), "whole tree restored", |h| {
        h.query_by_role_and_label(Role::TreeItem, "Welcome.md").is_some()
    });
}

// ============================================================================
// F2. Notes, Tabs, CRLF/BOM, Autosave, Conflicts & History
// ============================================================================

#[test]
fn t1_f2_untitled_type_autosave_saveas() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut opts = fx.opts(vec![fx.notes()]);
    opts.config.autosave_delay_ms = 250;
    let mut h = launch(opts);

    assert!(h.query_by_label("Start typing to begin an untitled note.").is_some());
    type_text(&mut h, "Draft idea body");
    assert!(
        h.query_by_role_and_label(Role::Tab, "Untitled-1 (unsaved)").is_some()
            || h.query_by_role_and_label(Role::Tab, "Untitled-1").is_some(),
        "untitled tab created"
    );

    // Wait for autosave to write Untitled-1.md in untitled_dir.
    let untitled_file = fx.dirs().untitled_dir().join("Untitled-1.md");
    wait_until(&mut h, Duration::from_secs(5), "untitled autosaved", |_| {
        untitled_file.exists()
    });

    // Ctrl+Shift+S -> Save as prompt into workspace root.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
    assert!(h.query_all_by_role(Role::Dialog).next().is_some());
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "SavedIdea");
    click_label(&mut h, "OK");

    let target = fx.notes().join("SavedIdea.md");
    wait_until(&mut h, Duration::from_secs(5), "SavedIdea.md written & Saved badge", |h| {
        target.exists() && has_label(h, "Saved")
    });
    assert_eq!(std::fs::read_to_string(&target).expect("read"), "Draft idea body");
}

#[test]
fn t1_f2_crlf_bom_roundtrip() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("CrlfBom.md");
    std::fs::write(&p, "\u{FEFF}# Title\r\nLine two\r\n").expect("write BOM+CRLF");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "CrlfBom.md");
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::End);
    press_key(&mut h, Key::Enter);
    type_text(&mut h, "Line three");
    press_shortcut(&mut h, Modifiers::COMMAND, Key::S);

    wait_until(&mut h, Duration::from_secs(5), "CrlfBom.md saved", |h| {
        has_label(h, "Saved")
            && std::fs::read_to_string(&p).is_ok_and(|s| s.contains("Line three"))
    });
    let bytes = std::fs::read(&p).expect("read bytes");
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "UTF-8 BOM must be preserved");
    let raw = String::from_utf8(bytes[3..].to_vec()).expect("utf8");
    assert!(raw.contains("\r\n"), "CRLF must be preserved");
    assert!(
        !raw.replace("\r\n", "").contains('\n'),
        "no bare LF line endings allowed: {raw:?}"
    );
}

#[test]
fn t1_f2_tabs_open_switch_close_reopen() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));

    click_tree_item(&mut h, "Welcome.md");
    let ideas_pos = h.get_by_role_and_label(Role::TreeItem, "Ideas.md").rect().center();
    click_pos_mods(&mut h, ideas_pos, Modifiers::COMMAND);
    let reading_pos = h.get_by_role_and_label(Role::TreeItem, "Reading.md").rect().center();
    middle_click_pos(&mut h, reading_pos);

    assert_eq!(h.get_all_by_role(Role::Tab).count(), 3);
    assert!(editor_text(&h).contains("## Books"), "Reading.md active");

    press_shortcut(&mut h, Modifiers::CTRL, Key::Tab);
    assert!(editor_text(&h).contains("# Welcome"), "Ctrl+Tab wrapped to Welcome.md");

    press_shortcut(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::Tab);
    assert!(editor_text(&h).contains("## Books"), "Ctrl+Shift+Tab returned to Reading.md");

    press_shortcut(&mut h, Modifiers::COMMAND, Key::W);
    assert_eq!(h.get_all_by_role(Role::Tab).count(), 2);

    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::T);
    assert_eq!(h.get_all_by_role(Role::Tab).count(), 3);
    assert!(editor_text(&h).contains("## Books"), "Ctrl+Shift+T reopened Reading.md");
}

#[test]
fn t1_f2_history_back_forward() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));

    click_tree_item(&mut h, "Welcome.md");
    click_tree_item(&mut h, "Ideas.md");
    click_tree_item(&mut h, "Reading.md");
    assert!(editor_text(&h).contains("## Books"));

    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowLeft);
    assert!(editor_text(&h).contains("# Ideas"));
    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowLeft);
    assert!(editor_text(&h).contains("# Welcome"));

    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowRight);
    assert!(editor_text(&h).contains("# Ideas"));

    click_label(&mut h, "Forward (Alt+Right)");
    assert!(editor_text(&h).contains("## Books"));
    click_label(&mut h, "Back (Alt+Left)");
    assert!(editor_text(&h).contains("# Ideas"));
}

#[test]
fn t1_f2_close_other_and_right_preserves_dirty() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut opts = fx.opts(vec![fx.notes()]);
    opts.config.autosave = false;
    let mut h = launch(opts);

    click_tree_item(&mut h, "Welcome.md");
    let ideas_pos = h.get_by_role_and_label(Role::TreeItem, "Ideas.md").rect().center();
    click_pos_mods(&mut h, ideas_pos, Modifiers::COMMAND);
    let reading_pos = h.get_by_role_and_label(Role::TreeItem, "Reading.md").rect().center();
    click_pos_mods(&mut h, reading_pos, Modifiers::COMMAND);

    // Dirty Ideas.md.
    h.get_by_role_and_label(Role::Tab, "Ideas.md").click();
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    type_text(&mut h, "DIRTY ");
    assert!(has_label(&h, "Unsaved"));

    // Close others from Welcome.md -> closes clean Reading.md, preserves dirty Ideas.md.
    h.get_by_role_and_label(Role::Tab, "Welcome.md").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Close others");
    assert!(h.query_by_role_and_label(Role::Tab, "Reading.md").is_none(), "clean tab closed");
    assert!(h.query_by_role_and_label(Role::Tab, "Ideas.md (unsaved)").is_some(), "dirty tab kept");
    assert!(has_label_contains(&h, "unsaved changes were kept"));

    // Close tabs to the right from Welcome.md (with Ideas.md to its right) -> preserves dirty tab.
    h.get_by_role_and_label(Role::Tab, "Welcome.md").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Close tabs to the right");
    assert!(h.query_by_role_and_label(Role::Tab, "Ideas.md (unsaved)").is_some());

    // Close all -> closes Welcome.md, preserves dirty Ideas.md.
    h.get_by_role_and_label(Role::Tab, "Welcome.md").click_secondary();
    settle(&mut h);
    click_label(&mut h, "Close all");
    assert!(h.query_by_role_and_label(Role::Tab, "Welcome.md").is_none());
    assert!(h.query_by_role_and_label(Role::Tab, "Ideas.md (unsaved)").is_some());
}

// ============================================================================
// F3. Full GFM Markdown, Editor, Mermaid, Kanban & Images
// ============================================================================

#[test]
fn t1_f3_gfm_blocks_render_and_task_toggle() {
    let _env = read_env_lock();
    let fx = new_fx();
    let gfm_note = fx.notes().join("GfmAll.md");
    std::fs::write(
        &gfm_note,
        "- [ ] task one\n- [x] task two\n\n# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n\n**bold** *italic* ~~strike~~ ==mark== H~2~O x^2^ <kbd>Ctrl</kbd>[^fn]\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n> [!NOTE]\n> Alert body\n\n[^fn]: footnote body\n",
    )
    .expect("write GfmAll.md");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "GfmAll.md");

    // Verify Outline lists all H1-H6 headings parsed from GFM blocks.
    switch_sidebar(&mut h, "Outline");
    for heading in ["H1", "H2", "H3", "H4", "H5", "H6"] {
        assert!(has_label(&h, heading), "Outline must list heading {heading}");
    }
    switch_sidebar(&mut h, "Files");

    // Verify rendered preview text (headings, inline formatting, kbd, table cells, alert, footnote).
    click_label(&mut h, "Preview");
    let prev_area = preview_area_rect(&h);
    let footnote_band = egui::Rect::from_min_max(
        egui::pos2(prev_area.left() + 8.0, prev_area.top() + 140.0),
        egui::pos2(
            (prev_area.left() + 520.0).min(prev_area.right()),
            (prev_area.top() + 360.0).min(prev_area.bottom()),
        ),
    );
    assert!(
        find_tooltip_pos_dwell(&mut h, footnote_band, 8.0, 8.0, "Footnote fn").is_some(),
        "footnote reference tooltip must appear in preview"
    );
    drag(
        &mut h,
        egui::pos2(prev_area.left() + 6.0, prev_area.top() + 4.0),
        egui::pos2(prev_area.right() - 8.0, prev_area.bottom() - 8.0),
        8,
    );
    right_click_pos(&mut h, prev_area.center());
    h.get_by_role_and_label(Role::Button, "Copy").click();
    h.step();
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(s) => Some(s.clone()),
            _ => None,
        })
        .expect("Copy command in preview must copy rendered GFM text");
    for expected in [
        "H1",
        "H6",
        "bold",
        "italic",
        "strike",
        "mark",
        "H2O",
        "x^2^",
        "Ctrl[1]",
        "A\tB",
        "1\t2",
        "Alert body",
        "footnote body",
    ] {
        assert!(
            copied.contains(expected),
            "rendered preview selection missing {expected:?} in {copied:?}"
        );
    }

    // Switch back to Split and click the `- [ ] task one` checkbox in preview.
    click_label(&mut h, "Split");
    let area = preview_area_rect(&h);
    let mut toggled = false;
    'scan: for dy in (12..=42).step_by(4) {
        for dx in (10..=46).step_by(4) {
            let p = egui::pos2(area.left() + dx as f32, area.top() + dy as f32);
            click_pos(&mut h, p);
            if editor_text(&h).contains("- [x] task one") {
                toggled = true;
                break 'scan;
            }
        }
    }
    assert!(toggled, "clicking preview task checkbox must update source to - [x] task one");
}

#[test]
fn t1_f3_editor_shortcuts_and_auto_close() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("EdKeys.md");
    std::fs::write(&p, "").expect("write empty");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "EdKeys.md");
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);

    // Bracket auto-close.
    type_text(&mut h, "(");
    assert_eq!(editor_text(&h), "()");
    type_text(&mut h, "a");
    type_text(&mut h, ")");
    assert_eq!(editor_text(&h), "(a)");

    // Bold, italic, link shortcuts on word.
    press_key(&mut h, Key::Enter);
    type_text(&mut h, "hello");
    press_shortcut(&mut h, Modifiers::SHIFT, Key::Home);
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::B);
    assert!(editor_text(&h).contains("**hello**"), "{}", editor_text(&h));

    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Enter);
    type_text(&mut h, "ital");
    press_shortcut(&mut h, Modifiers::SHIFT, Key::Home);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::I);
    assert!(editor_text(&h).contains("*ital*"), "{}", editor_text(&h));

    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Enter);
    type_text(&mut h, "ref");
    press_shortcut(&mut h, Modifiers::SHIFT, Key::Home);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::K);
    assert!(editor_text(&h).contains("[ref]("), "{}", editor_text(&h));

    // Line move / duplicate, list continuation, Tab/Shift+Tab indent/outdent.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, "- item 1");
    press_key(&mut h, Key::Enter);
    type_text(&mut h, "item 2");
    assert!(editor_text(&h).contains("- item 1\n- item 2"), "{}", editor_text(&h));
    press_key(&mut h, Key::Tab);
    assert!(editor_text(&h).contains("  - item 2"), "{}", editor_text(&h));
    press_shortcut(&mut h, Modifiers::SHIFT, Key::Tab);
    assert!(editor_text(&h).contains("\n- item 2"), "{}", editor_text(&h));

    press_shortcut(&mut h, Modifiers::ALT | Modifiers::SHIFT, Key::ArrowUp);
    assert_eq!(editor_text(&h).matches("item 2").count(), 2, "{}", editor_text(&h));
    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowUp);
    assert!(editor_text(&h).starts_with("- item 2"), "{}", editor_text(&h));
    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowDown);
    assert!(editor_text(&h).starts_with("- item 1"), "Alt+Down: {}", editor_text(&h));
    press_shortcut(&mut h, Modifiers::ALT | Modifiers::SHIFT, Key::ArrowDown);
    assert_eq!(
        editor_text(&h).matches("item 2").count(),
        3,
        "Alt+Shift+Down: {}",
        editor_text(&h)
    );
}

#[cfg(feature = "mermaid")]
#[test]
fn t1_f3_mermaid_diagram_renders() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("FlowPlain.md"), "# Flow\n").expect("write FlowPlain.md");
    let p = fx.notes().join("Mermaid.md");
    std::fs::write(
        &p,
        "# Flow\n\n```mermaid\nflowchart LR\n  A[Start] --> B{Check}\n  B -->|ok| C[Done]\n```\n",
    )
    .expect("write Mermaid.md");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    click_tree_item(&mut h, "FlowPlain.md");
    click_label(&mut h, "Preview");
    let area = preview_area_rect(&h);
    let ppp = h.ctx.pixels_per_point();
    let baseline_img = render_pixels(&mut h);

    click_tree_item(&mut h, "Mermaid.md");
    let scan_area = egui::Rect::from_min_max(
        area.min,
        egui::pos2(area.right(), (area.top() + 300.0).min(area.bottom())),
    );
    wait_until(&mut h, Duration::from_secs(12), "Mermaid diagram rendered", |h| {
        let img = render_pixels(h);
        if !rect_pixels_differ(&baseline_img, &img, area, ppp) {
            return false;
        }
        find_tooltip_pos(h, scan_area, 32.0, 24.0, "Fullscreen").is_some()
            && has_label(h, "Open in new window")
    });
}

#[cfg(not(feature = "mermaid"))]
#[test]
fn t1_f3_mermaid_disabled_fallback() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("Mermaid.md");
    std::fs::write(&p, "# Flow\n\n```mermaid\nflowchart LR\n  A --> B\n```\n").expect("write");
    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Mermaid.md");
    assert!(editor_text(&h).contains("flowchart LR"));
    let area = preview_area_rect(&h);
    let img = render_pixels(&mut h);
    assert!(distinct_colors_in_rect(&img, area, h.ctx.pixels_per_point()) >= 2);
}

#[cfg(feature = "kanban")]
#[test]
fn t1_f3_kanban_board_card_toggle_edit_drag() {
    let _env = read_env_lock();
    let fx = new_fx();
    let board = "---\nkanban-plugin: basic\n---\n\n## Todo\n\n- [ ] Alpha task\n- [ ] Beta task\n\n## Doing\n\n- [ ] Gamma task\n";
    let p = fx.notes().join("Board.md");
    std::fs::write(&p, board).expect("write Board.md");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 1.0 / 60.0);
    click_tree_item(&mut h, "Board.md");
    click_label(&mut h, "Preview");

    // Toggle Alpha task checkbox.
    let cb = h.query_all_by_label_contains("Alpha").next().expect("Alpha card").rect().center();
    click_pos(&mut h, cb);
    wait_until(&mut h, Duration::from_secs(5), "Alpha toggled on disk", |_| {
        std::fs::read_to_string(&p).is_ok_and(|s| s.contains("- [x] Alpha task"))
    });

    // Double-click Gamma task card to inline-edit + Enter.
    for _ in 0..30 {
        h.step();
    }
    let gamma = h.query_all_by_label_contains("Gamma").next().expect("Gamma card").rect().center()
        + egui::vec2(60.0, 0.0);
    double_click_pos(&mut h, gamma);
    type_text(&mut h, " edited");
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(5), "Gamma edited on disk", |_| {
        std::fs::read_to_string(&p).is_ok_and(|s| s.contains("- [ ] Gamma task edited"))
    });

    // Drag Beta task into Doing lane.
    let from = h.query_all_by_label_contains("Beta").next().expect("Beta card").rect().center()
        + egui::vec2(60.0, 0.0);
    let to = h.query_all_by_label_contains("Gamma").next().expect("Gamma card").rect().center()
        + egui::vec2(60.0, 0.0);
    drag(&mut h, from, to + egui::vec2(0.0, -8.0), 10);
    wait_until(&mut h, Duration::from_secs(5), "Beta moved to Doing", |_| {
        std::fs::read_to_string(&p)
            .is_ok_and(|s| s.contains("## Doing\n\n- [ ] Beta task\n- [ ] Gamma task edited"))
    });
}

#[cfg(not(feature = "kanban"))]
#[test]
fn t1_f3_kanban_disabled_fallback() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("Board.md");
    std::fs::write(&p, "---\nkanban-plugin: basic\n---\n\n## Todo\n\n- [ ] Alpha\n").expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Board.md");
    click_label(&mut h, "Preview");
    assert!(h.query_by_label("+ Add lane").is_none());
}

#[test]
fn t1_f3_image_preview_and_md_inline_image() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("PicPlain.md"), "# Pic\n").expect("write PicPlain.md");
    let host = fx.notes().join("ImgNote.md");
    std::fs::write(&host, "# Pic\n\n![](../journal/picture.png)\n").expect("write ImgNote.md");

    let mut h = launch_dt(fx.opts(vec![fx.notes(), fx.journal()]), 0.05);

    // 1. Standalone image preview.
    click_tree_item(&mut h, "picture.png");
    let area = preview_area_rect(&h);
    let ppp = h.ctx.pixels_per_point();
    wait_until(&mut h, Duration::from_secs(10), "standalone PNG decoded", |h| {
        assert!(!has_label_contains(h, "Cannot"), "standalone image error shown");
        if h.query_by_label("Loading image").is_some() {
            return false;
        }
        let img = render_pixels(h);
        distinct_colors_in_rect(&img, area, ppp) >= 8
    });

    // 2. Inline Markdown image compared against text-only `# Pic` baseline.
    click_tree_item(&mut h, "PicPlain.md");
    click_label(&mut h, "Preview");
    let area2 = preview_area_rect(&h);
    let plain_img = render_pixels(&mut h);

    click_tree_item(&mut h, "ImgNote.md");
    let scan_area = egui::Rect::from_min_max(
        area2.min,
        egui::pos2(area2.right(), (area2.top() + 300.0).min(area2.bottom())),
    );
    wait_until(&mut h, Duration::from_secs(10), "inline Markdown PNG decoded", |h| {
        assert!(!has_label_contains(h, "Cannot"), "inline Markdown image error shown");
        let img = render_pixels(h);
        rect_pixels_differ(&plain_img, &img, area2, ppp)
            && find_tooltip_pos(h, scan_area, 32.0, 24.0, "Fullscreen").is_some()
    });
}

// ============================================================================
// F4. Obsidian Wiki-Links, Embeds, Dead Links & Links Panel
// ============================================================================

#[test]
fn t1_f4_wikilink_basic_and_alias() {
    let _env = read_env_lock();
    let fx = new_fx();
    let p = fx.notes().join("WikiHost.md");
    std::fs::write(&p, "[[Ideas]]\n[[Ideas|My Ideas]]\n").expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    wait_indexed(&mut h);
    click_tree_item(&mut h, "WikiHost.md");
    click_label(&mut h, "Preview");

    let area = preview_area_rect(&h);
    let link_pos = find_tooltip_pos(&mut h, area, 12.0, 10.0, "Ideas")
        .expect("found [[Ideas]] link tooltip in preview");
    click_pos(&mut h, link_pos);
    wait_until(&mut h, Duration::from_secs(5), "navigated to Ideas.md from preview", |h| {
        h.query_by_role_and_label(Role::Tab, "Ideas.md").is_some()
    });

    // Go back, switch to Split, Ctrl+click alias link on line 2 in editor.
    press_shortcut(&mut h, Modifiers::ALT, Key::ArrowLeft);
    click_label(&mut h, "Split");
    let ed_r = editor_rect(&h);
    click_pos_mods(&mut h, ed_r.min + egui::vec2(42.0, 26.0), Modifiers::COMMAND);
    wait_until(&mut h, Duration::from_secs(5), "navigated to Ideas.md from editor Ctrl+click", |h| {
        h.query_by_role_and_label(Role::Tab, "Ideas.md").is_some()
    });
}

#[test]
fn t1_f4_wikilink_heading_jump() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut plan = String::from("# Plan\n\n");
    for i in 1..=30 {
        plan.push_str(&format!("Line {i}\n"));
    }
    plan.push_str("## Phase 2\n\nPhase two body.\n");
    std::fs::write(fx.notes().join("Projects/Plan.md"), plan).expect("write Plan.md");
    std::fs::write(fx.notes().join("JumpHost.md"), "Go to [[Projects/Plan#Phase 2]].\n").expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    wait_indexed(&mut h);
    click_tree_item(&mut h, "JumpHost.md");
    click_label(&mut h, "Preview");

    let area = preview_area_rect(&h);
    let link_pos = find_tooltip_pos(&mut h, area, 12.0, 10.0, "Phase 2")
        .expect("found [[Projects/Plan#Phase 2]] tooltip");
    click_pos(&mut h, link_pos);

    wait_until(&mut h, Duration::from_secs(5), "Plan.md opened at Phase 2 line", |h| {
        h.query_by_role_and_label(Role::Tab, "Plan.md").is_some()
            && has_label_contains(h, "Ln 33")
    });
}

#[test]
fn t1_f4_embed_note_and_image_inline() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(
        fx.notes().join("EmbedBoth.md"),
        "# Embeds\n\n![[Welcome]]\n\n![[picture.png]]\n",
    )
    .expect("write");
    std::fs::write(fx.notes().join("PlainOnly.md"), "# Embeds\n").expect("write");

    let mut h = launch_dt(fx.opts(vec![fx.notes(), fx.journal()]), 0.05);
    wait_indexed(&mut h);

    click_tree_item(&mut h, "PlainOnly.md");
    click_label(&mut h, "Preview");
    let area = preview_area_rect(&h);
    let plain_img = render_pixels(&mut h);

    click_tree_item(&mut h, "EmbedBoth.md");
    let ppp = h.ctx.pixels_per_point();
    wait_until(&mut h, Duration::from_secs(10), "embedded note and image rendered", |h| {
        let img = render_pixels(h);
        distinct_colors_in_rect(&img, area, ppp) >= 8 && rect_pixels_differ(&plain_img, &img, area, ppp)
    });
}

#[test]
fn t1_f4_links_panel_backlinks_outgoing_tags() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(
        fx.notes().join("Ideas.md"),
        "# Ideas\n\nSee [[Welcome]] and [[Reading#Books]].\n\n#brainstorm\n",
    )
    .expect("write Ideas.md");

    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);
    click_tree_item(&mut h, "Ideas.md");
    switch_sidebar(&mut h, "Links");

    wait_until(&mut h, Duration::from_secs(10), "Links panel populated", |h| {
        has_label_contains(h, "Backlinks (")
            && has_label_contains(h, "Outgoing (2)")
            && has_label_contains(h, "Tags (1)")
            && has_label(h, "#brainstorm")
    });

    h.get_all_by_label_contains("Welcome — line").next().expect("backlink row").click();
    settle(&mut h);
    wait_until(&mut h, Duration::from_secs(5), "clicked backlink opened Welcome.md", |h| {
        h.query_by_role_and_label(Role::Tab, "Welcome.md").is_some()
    });
}

#[test]
fn t1_f4_tree_move_rewrites_links() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    wait_indexed(&mut h);

    click_tree_item(&mut h, "Projects");
    click_tree_item(&mut h, "Archive");

    let src = h.get_by_role_and_label(Role::TreeItem, "Plan.md").rect().center();
    let dst = h.get_by_role_and_label(Role::TreeItem, "Archive").rect().center();
    drag(&mut h, src, dst, 10);

    wait_until(&mut h, Duration::from_secs(5), "Update links confirmation", |h| {
        h.query_by_label("Update links in 1 note?").is_some()
    });
    click_label(&mut h, "Update");

    let moved = fx.notes().join("Projects/Archive/Plan.md");
    wait_until(&mut h, Duration::from_secs(8), "Plan.md moved and Welcome.md updated", |_| {
        moved.exists()
            && !fx.notes().join("Projects/Plan.md").exists()
            && std::fs::read_to_string(fx.notes().join("Welcome.md"))
                .is_ok_and(|s| s.contains("[[Projects/Archive/Plan]]"))
    });
}

// ============================================================================
// F5. Typst Live Preview, Markdown Math & PDF Export
// ============================================================================

#[cfg(feature = "typst")]
#[test]
fn t1_f5_typst_note_live_compile_and_zoom() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);

    click_tree_item(&mut h, "Report.typ");
    wait_until(&mut h, Duration::from_secs(30), "Typst 1 page compiled", |h| {
        has_label(h, "1 page") && !has_label(h, "compiling...")
    });

    click_label(&mut h, "100%");
    click_label(&mut h, "Zoom in");
    assert!(has_label(&h, "125%"));
    click_label(&mut h, "Fit width");

    // Live edit adds a pagebreak -> recompiles to 2 pages.
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::End);
    type_text(&mut h, "\n#pagebreak()\n= Second page\n");
    wait_until(&mut h, Duration::from_secs(30), "Typst recompiled to 2 pages", |h| {
        has_label(h, "2 pages") && !has_label(h, "compiling...")
    });
}

#[cfg(feature = "typst")]
#[test]
fn t1_f5_typst_diagnostic_click_jumps_to_line() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);

    click_tree_item(&mut h, "Report.typ");
    wait_until(&mut h, Duration::from_secs(30), "initial Typst render", |h| {
        has_label(h, "1 page")
    });
    let area = preview_area_rect(&h);
    let before_img = render_pixels(&mut h);
    assert!(distinct_colors_in_rect(&before_img, area, h.ctx.pixels_per_point()) >= 2);

    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::End);
    type_text(&mut h, "\n#unknownfn(1)\n");

    wait_until(&mut h, Duration::from_secs(30), "Typst compile failed diagnostic", |h| {
        has_label_contains(h, "compile failed")
            && has_label_contains(h, "(showing last good pages)")
    });
    // Move caret back to line 1 by clicking the top of the editor, then click diagnostic to verify cursor jumps to line 5.
    let ed_top = editor_rect(&h).min + egui::vec2(6.0, 6.0);
    click_pos(&mut h, ed_top);
    assert!(has_label_contains(&h, "Ln 1,"));
    h.get_all_by_label_contains("unknown variable").next().expect("diagnostic row").click();
    settle(&mut h);
    wait_until(&mut h, Duration::from_secs(5), "cursor jumped to error line", |h| {
        has_label_contains(h, "Ln 5")
    });
}

#[cfg(feature = "typst")]
#[test]
fn t1_f5_markdown_inline_and_display_math() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("MathPlain.md"), "# Math\n").expect("write MathPlain.md");
    std::fs::write(
        fx.notes().join("TypMathAll.md"),
        "# Math\n\nInline $a^2 + b^2 = c^2$\n\n$$ sum_(i=1)^n i = (n(n+1))/2 $$\n\n```math\nintegral_0^1 x dif x\n```\n",
    )
    .expect("write");

    let mut h = launch_dt(fx.opts(vec![fx.notes()]), 0.05);
    click_tree_item(&mut h, "MathPlain.md");
    click_label(&mut h, "Preview");
    let area = preview_area_rect(&h);
    let ppp = h.ctx.pixels_per_point();
    let baseline_img = render_pixels(&mut h);

    click_tree_item(&mut h, "TypMathAll.md");
    let scan_area = egui::Rect::from_min_max(
        egui::pos2(area.left() + 20.0, area.top() + 40.0),
        egui::pos2(
            (area.left() + 650.0).min(area.right()),
            (area.top() + 280.0).min(area.bottom()),
        ),
    );
    let mut attempts = 0usize;
    wait_until(&mut h, Duration::from_secs(30), "Markdown math formulas rendered", |h| {
        attempts += 1;
        let img = render_pixels(h);
        if !rect_pixels_differ(&baseline_img, &img, area, ppp) {
            return false;
        }
        if !attempts.is_multiple_of(8) {
            std::thread::sleep(Duration::from_millis(20));
            return false;
        }
        find_tooltip_pos_dwell(h, scan_area, 28.0, 16.0, "Rendered as ").is_some()
    });
    assert!(
        has_label_contains(&h, "Rendered as Typst"),
        "expected math hover tooltip 'Rendered as Typst'"
    );
}

#[cfg(feature = "typst")]
#[test]
fn t1_f5_typst_page_click_jumps_to_source() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut doc = String::from("#set page(width: 14cm, height: 14cm, margin: 0.5cm)\n= Heading Top\n\n");
    for i in 1..=18 {
        doc.push_str(&format!("Paragraph {i} with clickable text across the whole line.\n\n"));
    }
    std::fs::write(fx.journal().join("ClickJump.typ"), doc).expect("write");

    let mut h = launch_dt(fx.opts(vec![fx.journal()]), 0.05);
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    click_tree_item(&mut h, "ClickJump.typ");
    wait_until(&mut h, Duration::from_secs(30), "ClickJump.typ compiled", |h| {
        has_label_contains(h, " page") && !has_label(h, "compiling...")
    });
    settle(&mut h);
    // Ensure caret starts at line 1.
    let ed_top = editor_rect(&h).min + egui::vec2(6.0, 6.0);
    click_pos(&mut h, ed_top);
    assert!(has_label_contains(&h, "Ln 1,"));

    // Click over lower text on the rendered page.
    let area = preview_area_rect(&h);
    let mut jumped = false;
    for dy in [160.0_f32, 220.0, 280.0, 340.0] {
        let pt = egui::pos2(area.left() + 80.0, area.top() + dy);
        click_pos(&mut h, pt);
        for _ in 0..10 {
            h.step();
            if !has_label_contains(&h, "Ln 1,") {
                jumped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if jumped {
            break;
        }
    }
    assert!(jumped, "page click must jump caret below line 1");
}

#[cfg(not(feature = "typst"))]
#[test]
fn t1_f5_non_typst_fallback_build() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));

    click_tree_item(&mut h, "Report.typ");
    assert!(
        h.query_by_label("Typst support is not built in (cargo feature `typst`).").is_some()
    );
    click_tree_item(&mut h, "Math.md");
    assert!(editor_text(&h).contains("$a^2 + b^2 = c^2$"));
}

// ============================================================================
// F6. Hand-Drawn Symbol Recognizer & Symbol Search (`symrec`)
// ============================================================================

#[cfg(feature = "typst")]
fn open_symbols_and_canvas(h: &mut H) -> egui::Rect {
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    switch_sidebar(h, "Symbols");
    wait_until(h, Duration::from_secs(60), "symrec recognizer ready", |h| {
        h.query_by_label("Preparing...").is_none() && h.query_by_label("Auto").is_some()
    });
    let top = h.get_by_label("Auto").rect().bottom() + 4.0;
    let bottom = h.get_by_label("Clear").rect().top() - 4.0;
    let left = h.get_by_label("Auto").rect().left();
    let side = (bottom - top).max(80.0);
    egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(side, side))
}

#[cfg(feature = "typst")]
fn stroke_line(a: egui::Pos2, b: egui::Pos2, n: usize) -> Vec<egui::Pos2> {
    (0..=n).map(|i| a + (b - a) * (i as f32 / n as f32)).collect()
}

#[cfg(feature = "typst")]
fn draw_plus(h: &mut H, r: egui::Rect) {
    let c = r.center();
    let s = r.width() * 0.3;
    drag_path(h, &stroke_line(c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0), 12));
    drag_path(h, &stroke_line(c - egui::vec2(0.0, s), c + egui::vec2(0.0, s), 12));
}

#[cfg(feature = "typst")]
#[test]
fn t1_f6_symbols_panel_open_and_draw_candidate() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    click_tree_item(&mut h, "Report.typ");
    let canvas = open_symbols_and_canvas(&mut h);
    draw_plus(&mut h, canvas);
    wait_until(&mut h, Duration::from_secs(20), "+ candidate shown", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_some()
            && h.query_by_label("Thinking...").is_none()
    });
}

#[cfg(feature = "typst")]
#[test]
fn t1_f6_symbol_insert_modes() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));

    click_tree_item(&mut h, "Report.typ");
    let canvas = open_symbols_and_canvas(&mut h);
    draw_plus(&mut h, canvas);
    wait_until(&mut h, Duration::from_secs(20), "+ candidate shown", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_some()
    });

    // Test #sym.name, Name, and Char modes in .typ.
    for (mode, expected) in [("Name", "plus"), ("#sym.name", "#sym.plus"), ("Char", "+")] {
        click_label(&mut h, mode);
        h.get_by_role_and_label(Role::Button, "+").click();
        settle(&mut h);
        assert!(editor_text(&h).contains(expected), "mode {mode} inserted {expected}");
    }

    // Test Auto mode in .md (outside math -> inserts unicode character 'α').
    open_note_via_palette(&mut h, "Welcome");
    wait_until(&mut h, Duration::from_secs(5), "Welcome.md opened", |h| {
        editor_text(h).contains("# Welcome")
    });
    click_label(&mut h, "Auto");
    click_label(&mut h, "Search symbols by name");
    type_text(&mut h, "alpha");
    wait_until(&mut h, Duration::from_secs(5), "alpha glyph visible", |h| {
        h.query_by_role_and_label(Role::Button, "α").is_some()
    });
    h.get_by_role_and_label(Role::Button, "α").click();
    settle(&mut h);
    assert!(editor_text(&h).contains('α'), "{}", editor_text(&h));
}

#[cfg(feature = "typst")]
#[test]
fn t1_f6_symbol_search_by_name_and_insert() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    click_tree_item(&mut h, "Report.typ");
    open_symbols_and_canvas(&mut h);

    click_label(&mut h, "Search symbols by name");
    type_text(&mut h, "alpha");
    wait_until(&mut h, Duration::from_secs(5), "alpha glyph button", |h| {
        h.query_by_role_and_label(Role::Button, "α").is_some()
    });
    h.get_by_role_and_label(Role::Button, "α").click();
    settle(&mut h);
    assert!(editor_text(&h).contains("#sym.alpha"), "{}", editor_text(&h));
}

#[cfg(feature = "typst")]
#[test]
fn t1_f6_canvas_undo_stroke_and_clear_esc() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    click_tree_item(&mut h, "Report.typ");
    let canvas = open_symbols_and_canvas(&mut h);
    draw_plus(&mut h, canvas);
    wait_until(&mut h, Duration::from_secs(20), "candidate ready", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_some()
    });

    click_label(&mut h, "Undo stroke");
    wait_until(&mut h, Duration::from_secs(20), "+ gone and - remains after Undo stroke", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_none()
            && h.query_by_role_and_label(Role::Button, "-").is_some()
            && h.query_by_label("Thinking...").is_none()
    });
    click_pos(&mut h, canvas.center());
    press_key(&mut h, Key::Escape);
    wait_until(&mut h, Duration::from_secs(5), "candidates cleared on Esc", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_none()
            && h.query_by_role_and_label(Role::Button, "-").is_none()
            && h.query_by_label("Search symbols by name").is_some()
    });
}

#[cfg(feature = "typst")]
#[test]
fn t1_f6_canvas_keyboard_candidate_nav() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.journal()]));
    click_tree_item(&mut h, "Report.typ");
    let canvas = open_symbols_and_canvas(&mut h);
    draw_plus(&mut h, canvas);
    wait_until(&mut h, Duration::from_secs(20), "candidates shown", |h| {
        h.query_by_role_and_label(Role::Button, "+").is_some()
    });
    let before = editor_text(&h);
    press_key(&mut h, Key::ArrowRight);
    press_key(&mut h, Key::ArrowLeft);
    press_key(&mut h, Key::Enter);
    assert_ne!(editor_text(&h), before, "Enter on canvas inserted selected candidate");
}

#[cfg(not(feature = "typst"))]
#[test]
fn t1_f6_non_typst_symbols_fallback() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    switch_sidebar(&mut h, "Symbols");
    assert!(
        h.query_by_label("Typst support is not built in (cargo feature `typst`), so the symbol panel is unavailable.").is_some()
    );
}

// ============================================================================
// F7. Synchronized Source <-> Preview Scrolling
// ============================================================================

fn long_markdown_doc() -> String {
    let mut s = String::new();
    for i in 0..100 {
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i} with enough words to wrap across multiple lines in split view.\n\n"
        ));
    }
    s
}

#[test]
fn t1_f7_md_split_editor_scroll_drives_preview() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("LongSync.md"), long_markdown_doc()).expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "LongSync.md");
    let prev_r = preview_area_rect(&h);
    let before = render_pixels(&mut h);

    let ed_c = editor_visible_center(&h);
    wheel_at(&mut h, ed_c, egui::vec2(0.0, -1600.0));
    let after = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&before, &after, prev_r, h.ctx.pixels_per_point()),
        "scrolling editor in Split mode must scroll Markdown preview"
    );
}

#[test]
fn t1_f7_md_split_preview_scroll_drives_editor() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("LongSyncRev.md"), long_markdown_doc()).expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "LongSyncRev.md");
    let ed_r = editor_rect(&h).intersect(h.ctx.content_rect());
    let before = render_pixels(&mut h);

    let prev_c = preview_area_rect(&h).center();
    wheel_at(&mut h, prev_c, egui::vec2(0.0, -1600.0));
    let after = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&before, &after, ed_r, h.ctx.pixels_per_point()),
        "scrolling preview in Split mode must scroll editor"
    );
    assert!(!wants_immediate_repaint(&h), "must settle without feedback oscillation");
}

#[cfg(feature = "typst")]
#[test]
fn t1_f7_typst_split_sync_scroll() {
    let _env = read_env_lock();
    let fx = new_fx();
    let doc = "#set page(width: 12cm, height: 6cm, margin: 1cm)\n"
        .to_string()
        + &(0..4).map(|p| format!("= Page {p}\n\n{}\n#pagebreak()\n", "Line of text.\n\n".repeat(12))).collect::<String>();
    std::fs::write(fx.journal().join("MultiScroll.typ"), doc).expect("write");

    let mut h = launch_dt(fx.opts(vec![fx.journal()]), 0.05);
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    click_tree_item(&mut h, "MultiScroll.typ");
    wait_until(&mut h, Duration::from_secs(30), "MultiScroll.typ 5 pages rendered", |h| {
        has_label_contains(h, "pages") && !has_label(h, "compiling...")
    });
    settle(&mut h);

    let prev_r = preview_area_rect(&h);
    let ed_r = editor_rect(&h).intersect(h.ctx.content_rect());
    let ed_c = editor_visible_center(&h);
    let p_before = render_pixels(&mut h);
    wheel_at(&mut h, ed_c, egui::vec2(0.0, -1200.0));
    let p_after = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&p_before, &p_after, prev_r, h.ctx.pixels_per_point()),
        "Typst editor scroll must drive Typst preview"
    );

    let e_before = render_pixels(&mut h);
    wheel_at(&mut h, prev_r.center(), egui::vec2(0.0, 900.0));
    let e_after = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&e_before, &e_after, ed_r, h.ctx.pixels_per_point()),
        "Typst preview scroll must drive Typst editor"
    );
}

#[test]
fn t1_f7_toggle_sync_scroll_decouples() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("Decouple.md"), long_markdown_doc()).expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Decouple.md");
    let prev_r = preview_area_rect(&h);
    let ppp = h.ctx.pixels_per_point();

    // Turn Sync off -> scrolling editor leaves preview stationary.
    click_label(&mut h, "Sync");
    let p0 = render_pixels(&mut h);
    let ed_c = editor_visible_center(&h);
    wheel_at(&mut h, ed_c, egui::vec2(0.0, -1400.0));
    let p1 = render_pixels(&mut h);
    assert!(
        !rect_pixels_differ(&p0, &p1, prev_r, ppp),
        "with Sync off, editor wheel must not scroll preview"
    );

    // Turn Sync back on -> scrolling editor drives preview again.
    click_label(&mut h, "Sync");
    let ed_c2 = editor_visible_center(&h);
    wheel_at(&mut h, ed_c2, egui::vec2(0.0, -1400.0));
    let p2 = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&p1, &p2, prev_r, ppp),
        "with Sync back on, editor wheel must scroll preview"
    );
}

#[test]
fn t1_f7_heading_outline_jump_syncs_both_panes() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("OutlineSync.md"), long_markdown_doc()).expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "OutlineSync.md");
    switch_sidebar(&mut h, "Outline");
    let prev_r = preview_area_rect(&h);
    let p0 = render_pixels(&mut h);

    // Scroll Outline to reach late heading or use End + Enter to jump to Section 99.
    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Enter);
    let p1 = render_pixels(&mut h);
    assert!(h.query_by_label_contains("Ln 1,").is_none(), "editor cursor jumped to late section");
    assert!(
        rect_pixels_differ(&p0, &p1, prev_r, h.ctx.pixels_per_point()),
        "Outline jump must scroll preview to target section"
    );
}

// ============================================================================
// F8. Command Palette, Workspace Search, Find/Replace, Outline & Settings
// ============================================================================

#[test]
fn t1_f8_quick_switcher_files_tags_and_create() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("TagNote.md"), "# Tagged\n\n#research\n").expect("write");
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);

    // File switch.
    open_note_via_palette(&mut h, "Tasks");
    assert!(editor_text(&h).contains("See [[Plan]]"));

    // Tag switch -> opens Workspace Search for #research -> click hit to open TagNote.md.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::O);
    type_text(&mut h, "#research");
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(8), "tag search hit visible", |h| {
        has_label_contains(h, "TagNote.md") && has_label_contains(h, "#research")
    });
    press_key(&mut h, Key::ArrowDown);
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(5), "TagNote.md opened from tag search", |h| {
        editor_text(h).contains("# Tagged")
    });

    // Create new note via quick switcher.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::O);
    type_text(&mut h, "PaletteCreated");
    assert!(h.query_by_label_contains("Create note 'PaletteCreated'").is_some());
    press_key(&mut h, Key::ArrowUp);
    press_key(&mut h, Key::Enter);
    let created = fx.notes().join("PaletteCreated.md");
    wait_until(&mut h, Duration::from_secs(5), "PaletteCreated.md created", |_| created.exists());
}

#[test]
fn t1_f8_command_palette_executes_command() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));

    assert!(h.query_by_role_and_label(Role::TreeItem, "notes").is_some());
    press_shortcut(&mut h, Modifiers::COMMAND, Key::P);
    type_text(&mut h, "Toggle sidebar");
    press_key(&mut h, Key::Enter);
    assert!(h.query_by_role_and_label(Role::TreeItem, "notes").is_none());

    press_shortcut(&mut h, Modifiers::COMMAND, Key::O);
    type_text(&mut h, ">Toggle sidebar");
    press_key(&mut h, Key::Enter);
    assert!(h.query_by_role_and_label(Role::TreeItem, "notes").is_some());
}

#[test]
fn t1_f8_workspace_search_case_word_jump() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);

    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    type_text(&mut h, "Plan");
    click_label(&mut h, "Aa");
    click_label(&mut h, "W");
    wait_until(&mut h, Duration::from_secs(10), "case+whole-word search hit", |h| {
        h.query_by_label("6: See [[Plan]].").is_some()
    });
    click_label(&mut h, "6: See [[Plan]].");
    wait_until(&mut h, Duration::from_secs(5), "jumped to Tasks.md line 6", |h| {
        h.query_by_role_and_label(Role::Tab, "Tasks.md").is_some()
            && has_label_contains(h, "Ln 6,")
    });
}

#[test]
fn t1_f8_find_replace_next_prev_regex_all() {
    let _env = read_env_lock();
    let fx = new_fx();
    std::fs::write(fx.notes().join("FindRep.md"), "k1=v1\nk2=v2\nplain\n").expect("write");

    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "FindRep.md");
    click_label(&mut h, "Source");

    press_shortcut(&mut h, Modifiers::COMMAND, Key::F);
    type_text(&mut h, "k");
    assert!(h.query_by_label("1/2").is_some());
    press_key(&mut h, Key::F3);
    assert!(h.query_by_label("2/2").is_some());
    press_shortcut(&mut h, Modifiers::SHIFT, Key::F3);
    assert!(h.query_by_label("1/2").is_some());

    press_shortcut(&mut h, Modifiers::COMMAND, Key::H);
    click_label(&mut h, "Regular expression");
    h.get_by_label("Find in note").click();
    settle(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::A);
    type_text(&mut h, r"^(\w+)=(\w+)$");
    assert!(h.query_by_label("1/2").is_some());

    h.get_by_label("Replace with").click();
    settle(&mut h);
    type_text(&mut h, "$2:$1");
    press_key(&mut h, Key::Enter);
    assert!(editor_text(&h).starts_with("v1:k1\nk2=v2"));
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::ALT, Key::Enter);
    assert_eq!(editor_text(&h), "v1:k1\nv2:k2\nplain\n");
}

#[test]
fn t1_f8_settings_modify_and_persist_restart() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Welcome.md");
    let no_nums_left = editor_rect(&h).left();

    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    click_label(&mut h, "Editor");
    click_label(&mut h, "Show line numbers");
    press_key(&mut h, Key::Escape);
    assert!(editor_rect(&h).left() > no_nums_left + 10.0);

    let h = relaunch_same_dirs(h, || {
        let mut restart_opts = iron_sheets::AppOptions::new(fx.dirs());
        restart_opts.workspace = Some("test".into());
        restart_opts
    });
    assert!(
        editor_rect(&h).left() > no_nums_left + 10.0,
        "Show line numbers setting persisted across restart"
    );
}

// ============================================================================
// F12. Startup CLI, Session Restore, Theme/Zoom & Idle/Perf Overlay
// ============================================================================

#[test]
fn t1_f12_cli_paths_attach_roots_and_open_tabs() {
    let _env = read_env_lock();
    let fx = new_fx();
    let h = launch(fx.opts(vec![fx.notes(), fx.journal().join("2024-01-01.md")]));

    assert!(h.query_by_role_and_label(Role::TreeItem, "notes").is_some());
    assert!(h.query_by_role_and_label(Role::Tab, "2024-01-01.md").is_some());
    assert!(editor_text(&h).contains("Resolutions: [[Ideas]]."));
}

#[test]
fn t1_f12_session_restore_lazy_tabs_across_restart() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));

    click_tree_item(&mut h, "Welcome.md");
    let ideas_pos = h.get_by_role_and_label(Role::TreeItem, "Ideas.md").rect().center();
    click_pos_mods(&mut h, ideas_pos, Modifiers::COMMAND);
    let reading_pos = h.get_by_role_and_label(Role::TreeItem, "Reading.md").rect().center();
    click_pos_mods(&mut h, reading_pos, Modifiers::COMMAND);

    let h = relaunch_same_dirs(h, || {
        let mut restart_opts = fx.opts(vec![]);
        restart_opts.workspace = Some("test".into());
        restart_opts
    });

    assert_eq!(h.get_all_by_role(Role::Tab).count(), 3);
    assert!(h.query_by_role_and_label(Role::Tab, "Welcome.md").is_some());
    assert!(h.query_by_role_and_label(Role::Tab, "Ideas.md").is_some());
    assert!(h.query_by_role_and_label(Role::Tab, "Reading.md").is_some());
    assert!(editor_text(&h).contains("## Books"));
}

#[test]
fn t1_f12_theme_light_dark_high_contrast_switch() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Welcome.md");

    // Switch to Light via Command Palette, then Dark, then toggle High contrast in Settings.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::P);
    type_text(&mut h, "Theme: light");
    press_key(&mut h, Key::Enter);
    let light_img = render_pixels(&mut h);
    let light_px = light_img.get_pixel(500, 100).0;

    press_shortcut(&mut h, Modifiers::COMMAND, Key::P);
    type_text(&mut h, "Theme: dark");
    press_key(&mut h, Key::Enter);
    let dark_img = render_pixels(&mut h);
    let dark_px = dark_img.get_pixel(500, 100).0;

    assert!(light_px[0] > dark_px[0] + 80, "light {light_px:?} vs dark {dark_px:?}");

    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    type_text(&mut h, "high contrast");
    click_label(&mut h, "High contrast");
    press_key(&mut h, Key::Escape);
    let hc_img = render_pixels(&mut h);
    assert!(
        rect_pixels_differ(&dark_img, &hc_img, h.ctx.content_rect(), h.ctx.pixels_per_point()),
        "High contrast theme updates rendered pixels"
    );
}

#[test]
fn t1_f12_zoom_in_out_reset_shortcuts() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    let z0 = h.ctx.zoom_factor();

    press_shortcut(&mut h, Modifiers::COMMAND, Key::Equals);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Equals);
    let z_in = h.ctx.zoom_factor();
    assert!(z_in > z0 + 0.05, "zoomed in: {z0} -> {z_in}");

    press_shortcut(&mut h, Modifiers::COMMAND, Key::Minus);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Minus);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Minus);
    let z_out = h.ctx.zoom_factor();
    assert!(z_out < z0 - 0.05, "zoomed out: {z0} -> {z_out}");

    press_shortcut(&mut h, Modifiers::COMMAND, Key::Num0);
    let z_reset = h.ctx.zoom_factor();
    assert!((z_reset - z0).abs() < 0.01, "zoom reset: {z_reset} vs {z0}");
}

#[test]
fn t1_f12_perf_overlay_f12_and_idle_zero_repaints() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes()]));
    click_tree_item(&mut h, "Welcome.md");

    press_key(&mut h, Key::F12);
    assert!(h.query_by_label_contains("fps:").is_some());
    assert!(h.query_by_label_contains("frame cpu:").is_some());
    assert!(h.query_by_label_contains("in flight:").is_some());

    press_key(&mut h, Key::F12);
    assert!(h.query_by_label_contains("fps:").is_none());

    settle(&mut h);
    let mut immediate = 0usize;
    for _ in 0..20 {
        h.step();
        immediate += usize::from(wants_immediate_repaint(&h));
    }
    assert_eq!(immediate, 0, "idle app must request 0 immediate repaints");
}
