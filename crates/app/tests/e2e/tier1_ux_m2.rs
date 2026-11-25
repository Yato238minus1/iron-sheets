use super::driver::*;
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::{NodeT, Queryable};
use std::time::Duration;

#[test]
fn t1_f9_focus_sidebar_and_view_switch_autofocus() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);

    click_tree_item(&mut h, "Reading.md");
    assert!(editor_focused(&h), "editor focused after opening note");

    // Ctrl+Shift+E (Command::FocusSidebar) moves focus out of editor into Files tree.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    assert!(!editor_focused(&h), "Ctrl+Shift+E moved focus out of editor");
    press_key(&mut h, Key::ArrowDown);
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h), "Esc from Files returns focus to Note editor");

    // Switching to Search auto-focuses "Search in workspace".
    switch_sidebar(&mut h, "Search");
    assert!(
        h.get_by_label("Search in workspace").accesskit_node().is_focused(),
        "Search view switch autofocuses Search input"
    );
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h), "Esc on empty Search returns focus to Note editor");

    // Switching to Outline auto-focuses outline_keys and responds immediately to End/Enter/Esc.
    switch_sidebar(&mut h, "Outline");
    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Enter);
    assert!(
        has_label_contains(&h, "Ln 7") || has_label_contains(&h, "Ln 8"),
        "End + Enter in Outline jumped to ## Papers"
    );
    assert!(editor_focused(&h), "Enter in Outline jumps and focuses Note editor");
    // Re-focus Outline, navigate Home, and verify Esc returns focus to Note editor.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    press_key(&mut h, Key::Home);
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h), "Esc from Outline returns focus to Note editor");

    // Switching to Links auto-focuses links_keys and responds to ArrowDown/Esc.
    switch_sidebar(&mut h, "Links");
    wait_until(&mut h, Duration::from_secs(10), "Links panel populated", |h| {
        has_label_contains(h, "Backlinks")
    });
    press_key(&mut h, Key::ArrowDown);
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h), "Esc from Links returns focus to Note editor");
}

#[test]
fn t1_f9_search_outline_links_row_keyboard_nav() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);

    // 1. Search panel: Ctrl+Shift+F -> type query -> ArrowDown/End/Home/Enter -> Esc clears -> Esc focuses editor.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    type_text(&mut h, "plan");
    wait_until(&mut h, Duration::from_secs(10), "search hits", |h| {
        has_label_contains(h, "hits in")
    });
    press_key(&mut h, Key::ArrowDown);
    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Home);
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(5), "note opened from search", |h| {
        h.query_by_role(Role::MultilineTextInput).is_some()
    });

    // Clear search with Esc, then Esc back to editor.
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
    press_key(&mut h, Key::Escape);
    assert!(has_label(&h, "Type to search all notes"));
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h));

    // 2. Open Reading.md via Quick Switcher (mouse-free) and navigate Outline via Command Palette.
    open_note_via_palette(&mut h, "Reading");
    wait_until(&mut h, Duration::from_secs(5), "Reading.md opened", |h| {
        editor_text(h).contains("## Papers")
    });
    press_shortcut(&mut h, Modifiers::COMMAND, Key::P);
    type_text(&mut h, "Show outline");
    press_key(&mut h, Key::Enter);
    press_key(&mut h, Key::End);
    press_key(&mut h, Key::Home);
    press_key(&mut h, Key::ArrowDown);
    press_key(&mut h, Key::Enter);
    assert!(editor_focused(&h));
    press_shortcut(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    press_key(&mut h, Key::Escape);
    assert!(editor_focused(&h));

    // 3. Navigate Links panel purely by keyboard.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::P);
    type_text(&mut h, "Show links");
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(10), "Links panel loaded", |h| {
        has_label_contains(h, "Backlinks") && !has_label_contains(h, "Finding backlinks")
    });
    press_key(&mut h, Key::Home);
    press_key(&mut h, Key::End);
    press_key(&mut h, Key::ArrowUp);
    press_key(&mut h, Key::Enter);
    wait_until(&mut h, Duration::from_secs(5), "navigated to linked note", |h| {
        h.query_by_role(Role::MultilineTextInput).is_some()
    });
}

#[test]
fn t1_f9_statusbar_dots_and_conflict_accent() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut opts = fx.opts(vec![fx.notes()]);
    opts.config.autosave = false;
    let mut h = launch(opts);
    wait_indexed(&mut h);

    let p = fx.notes().join("Ideas.md");
    click_tree_item(&mut h, "Ideas.md");
    let saved_rect = h.get_all_by_label("Saved").next().expect("Saved label").rect();
    let saved_img = render_pixels(&mut h);
    let ppp = h.ctx.pixels_per_point();
    let saved_dot_rect = egui::Rect::from_min_max(
        egui::pos2(saved_rect.left() - 14.0, saved_rect.top()),
        egui::pos2(saved_rect.left() - 1.0, saved_rect.bottom()),
    );
    assert!(
        distinct_colors_in_rect(&saved_img, saved_dot_rect, ppp) >= 2,
        "Saved status dot should render distinct pixels left of Saved label"
    );

    // Edit note without saving -> "Unsaved" badge + amber dot.
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    type_text(&mut h, "unsaved ");
    let unsaved_rect = h.get_all_by_label("Unsaved").next().expect("Unsaved label").rect();
    let unsaved_img = render_pixels(&mut h);
    let unsaved_dot_rect = egui::Rect::from_min_max(
        egui::pos2(unsaved_rect.left() - 14.0, unsaved_rect.top()),
        egui::pos2(unsaved_rect.left() - 1.0, unsaved_rect.bottom()),
    );
    assert!(
        distinct_colors_in_rect(&unsaved_img, unsaved_dot_rect, ppp) >= 2,
        "Unsaved status dot should render distinct pixels"
    );
    assert!(
        rect_pixels_differ(&saved_img, &unsaved_img, unsaved_dot_rect, ppp),
        "Saved dot and Unsaved dot colors should differ"
    );

    // Modify file externally on disk -> conflict controls appear without hiding counts.
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&p, "# Ideas\n\nExternal disk edit.\n").expect("write external");
    wait_until(&mut h, Duration::from_secs(10), "external conflict controls", |h| {
        has_label(h, "Changed on disk")
            && has_label(h, "Reload")
            && has_label(h, "Keep mine")
    });
    assert!(has_label_contains(&h, "words"), "counts remain visible alongside conflict controls");

    let conflict_img = render_pixels(&mut h);
    let has_accent_border = |rect: egui::Rect| {
        let x0 = ((rect.left() * ppp).max(0.0) as u32).min(conflict_img.width());
        let y0 = ((rect.top() * ppp).max(0.0) as u32).min(conflict_img.height());
        let x1 = ((rect.right() * ppp).ceil().max(0.0) as u32).min(conflict_img.width());
        let y1 = (((rect.top() + 3.0) * ppp).ceil().max(0.0) as u32).min(conflict_img.height());
        (y0..y1).any(|y| {
            (x0..x1).any(|x| {
                let [r, _g, b, _a] = conflict_img.get_pixel(x, y).0;
                b > r.saturating_add(30)
            })
        })
    };
    let reload_rect = h.get_all_by_label("Reload").next().expect("Reload button").rect();
    let keep_rect = h.get_all_by_label("Keep mine").next().expect("Keep mine button").rect();
    assert!(
        has_accent_border(reload_rect) && has_accent_border(keep_rect),
        "Reload and Keep mine conflict buttons must paint accent on_stroke borders"
    );

    click_label(&mut h, "Keep mine");
    assert!(!has_label(&h, "Changed on disk"), "Keep mine dismisses conflict state");
    assert!(editor_text(&h).contains("unsaved "), "local edit preserved");
}

#[test]
fn t1_f9_tabstrip_overflow_affordance_and_view_hint() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut paths = Vec::new();
    for i in 0..15 {
        let p = fx.notes().join(format!("Overflow{i:02}.md"));
        std::fs::write(&p, format!("# Overflow {i:02}\n")).expect("write");
        paths.push(p);
    }
    let mut launch_paths = vec![fx.notes()];
    launch_paths.extend(paths);
    let mut h = launch_size(fx.opts(launch_paths), egui::vec2(520.0, 560.0), 0.25);

    wait_until(&mut h, Duration::from_secs(5), "Open tabs overflow button", |h| {
        has_label(h, "Open tabs")
    });
    click_label(&mut h, "Open tabs");
    // Choose the first tab from the overflow menu.
    h.get_all_by_label("Overflow00.md").last().expect("menu item").click();
    settle(&mut h);
    wait_until(&mut h, Duration::from_secs(5), "Overflow00.md loaded", |h| {
        editor_text(h).contains("# Overflow 00")
    });

    // At 520px width, compact view-mode button is "Mode" with hover tooltip "View mode (Ctrl+E)".
    let mode_btn = h.get_all_by_label("Mode").next().expect("Mode button");
    mode_btn.hover();
    for _ in 0..4 {
        h.step();
    }
    assert!(
        has_label_contains(&h, "View mode (Ctrl+E)"),
        "compact Mode button tooltip must mention Ctrl+E"
    );
}

#[test]
fn t1_f9_accessibility_tree_check_enabled() {
    let _env = read_env_lock();
    let fx = new_fx();
    let mut h = launch_a11y(fx.opts(vec![fx.notes(), fx.journal()]));
    wait_indexed(&mut h);

    click_tree_item(&mut h, "Welcome.md");
    assert!(h.query_by_label("Note editor").is_some(), "editor TextEdit labeled 'Note editor'");
    assert_a11y_ok(&mut h);

    for view in ["Search", "Outline", "Links", "Files"] {
        switch_sidebar(&mut h, view);
        assert_a11y_ok(&mut h);
    }

    // Find bar compact & expanded.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::F);
    assert_a11y_ok(&mut h);
    press_shortcut(&mut h, Modifiers::COMMAND, Key::H);
    assert_a11y_ok(&mut h);
    press_key(&mut h, Key::Escape);

    // Quick switcher palette.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::O);
    type_text(&mut h, "Welcome");
    assert_a11y_ok(&mut h);
    press_key(&mut h, Key::Escape);

    // Settings across sections.
    press_shortcut(&mut h, Modifiers::COMMAND, Key::Comma);
    assert_a11y_ok(&mut h);
    for sec in ["General", "Editor", "Files", "Modules"] {
        click_label(&mut h, sec);
        assert_a11y_ok(&mut h);
    }
    press_key(&mut h, Key::Escape);

    // Rename modal dialog.
    press_key(&mut h, Key::F2);
    assert!(
        h.query_all_by_role(Role::Dialog).next().is_some(),
        "F2 must open rename modal dialog"
    );
    assert_a11y_ok(&mut h);
    press_key(&mut h, Key::Escape);
}
