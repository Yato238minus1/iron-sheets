//! Accessibility check across core UI flows + Statusbar, Palette, Tabstrip visual polish (Item #14).
use crate::common::*;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::dialogs::{Dialog, PromptAction};
use iron_sheets::state::{AppState, SidebarView};
use iron_sheets_core::config::ViewMode;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

#[test]
fn accessibility_check_passes_across_main_shell_find_palette_settings_and_dialogs() {
    let fx = Fx::new();
    let opts = fx.opts(vec![fx.notes(), fx.journal()]);
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
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    settle(&mut h);

    // 1. Main shell + Welcome.md in Split view.
    st(&mut h).commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: true, line: None });
    st(&mut h).commands.push(Command::SetViewMode(ViewMode::Split));
    settle(&mut h);

    // 2. Search / Outline / Links sidebars.
    for v in [SidebarView::Search, SidebarView::Outline, SidebarView::Links, SidebarView::Files] {
        st(&mut h).commands.push(Command::SetSidebarView(v));
        settle(&mut h);
    }

    // 3. Find bar (Ctrl+H expanded).
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);

    // 4. Quick switcher palette.
    st(&mut h).commands.push(Command::QuickOpen);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);

    // 5. Settings window across all sections.
    st(&mut h).commands.push(Command::OpenSettings);
    settle(&mut h);
    for sec in iron_sheets::settings::Section::ALL {
        st(&mut h).settings.section = sec;
        settle(&mut h);
    }
    st(&mut h).settings.open = false;
    settle(&mut h);

    // 6. Prompt dialog with Folder ComboBox.
    st(&mut h).dialogs.current = Some(Dialog::Prompt {
        title: "New note".into(),
        label: "Note name".into(),
        value: String::new(),
        action: PromptAction::NewNote { dir: fx.notes() },
        error: None,
        focus: true,
        dirs: vec![("notes".into(), fx.notes()), ("journal".into(), fx.journal())],
        dir_sel: 0,
    });
    settle(&mut h);
    st(&mut h).dialogs.current = None;
    settle(&mut h);

    // 7. Confirm dialog.
    st(&mut h).dialogs.current = Some(Dialog::Confirm {
        title: "Create note".into(),
        message: "Note 'A11yTest' does not exist.".into(),
        ok_label: "Create".into(),
        on_ok: Vec::new(),
        alt: None,
        dont_ask: None,
        focus: true,
    });
    settle(&mut h);
    st(&mut h).dialogs.current = None;
    settle(&mut h);

    // 8. FolderBrowser dialog.
    st(&mut h).dialogs.current = Some(Dialog::FolderBrowser {
        path_text: fx.notes().display().to_string(),
        dir: fx.notes(),
        error: None,
        listing: None,
    });
    settle(&mut h);
}

#[test]
fn statusbar_dots_conflict_outlines_and_error_tooltip() {
    let fx = Fx::new();
    let p = fx.notes().join("Ideas.md");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    st(&mut h).commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    settle(&mut h);
    assert!(h.query_by_label("Saved").is_some());

    // Simulate external disk modification + error message in statusbar.
    {
        let doc = st(&mut h).docs.get_mut(&p).expect("doc");
        doc.apply_edit(0..0, "unsaved ");
        doc.external_change = true;
    }
    st(&mut h).status.error("Very long status error message describing an actionable storage failure condition for verification");
    settle(&mut h);

    assert!(h.query_by_label("Unsaved").is_some());
    assert!(h.query_by_label("Reload").is_some());
    assert!(h.query_by_label("Keep mine").is_some());
    assert!(h.query_by_label("Dismiss error").is_some());
    shot(&mut h, "m2-statusbar-dots-conflict");

    h.get_by_label("Dismiss error").click();
    settle(&mut h);
    assert!(h.query_by_label("Dismiss error").is_none());
}

#[test]
fn palette_long_dir_elides_without_overlapping_hint() {
    let fx = Fx::new();
    let deep = fx
        .notes()
        .join("deep/very_long_folder_segment/another_nested_directory/somewhere_inside_workspace/sub_archive/TargetElideNote.md");
    std::fs::create_dir_all(deep.parent().expect("parent")).expect("mkdir");
    std::fs::write(&deep, "# TargetElideNote\n\nBody.\n").expect("write");

    let mut h = harness(fx.opts(vec![fx.notes()]));
    let t0 = Instant::now();
    while st(&mut h).indexer.index().read().by_path(&deep).is_none() {
        h.step();
        assert!(t0.elapsed() < Duration::from_secs(10), "timeout indexing deep note");
        std::thread::sleep(Duration::from_millis(5));
    }

    st(&mut h).commands.push(Command::QuickOpen);
    settle(&mut h);
    h.get_by_label("Quick switcher").type_text("TargetElideNote");
    settle(&mut h);
    assert!(!st(&mut h).palette.rows().is_empty());
    shot(&mut h, "m2-palette-elide");
}

#[test]
fn tabstrip_overflow_fade_open_tabs_menu_and_compact_view_mode() {
    let fx = Fx::new();
    let mut paths = Vec::new();
    for i in 0..10 {
        let p = fx.notes().join(format!("OverflowTab{i:02}.md"));
        std::fs::write(&p, format!("# OverflowTab{i:02}\n")).expect("write");
        paths.push(p);
    }

    let opts = fx.opts(vec![fx.notes()]);
    iron_sheets::theme::set_row_recording(true);
    let mut h = Harness::builder()
        .with_size(egui::vec2(720.0, 560.0))
        .with_step_dt(0.25)
        .with_accessibility_check(false)
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
    for p in &paths {
        st(&mut h).commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    }
    settle(&mut h);
    settle(&mut h);

    h.get_by_label("Open tabs").click();
    settle(&mut h);
    assert!(
        h.get_all_by_label("OverflowTab00.md").count() >= 3,
        "Open tabs overflow menu lists first tab (tree + tab + menu item)"
    );
    shot(&mut h, "m2-tabstrip-overflow");

    // Close overflow popup, check the View menu lists "Cycle view mode" in its Editor mode submenu.
    h.key_press(Key::Escape);
    settle(&mut h);
    h.get_by_label("View").click();
    settle(&mut h);
    h.get_by_label("Editor mode").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Cycle view mode").is_some());
}
