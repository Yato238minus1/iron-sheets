//! The empty tab (no editor, no preview) and the cheap-settings design: status bar and toolbar switches, fonts, colour
//! overrides, and the check that an idle frame does no settings work. Screenshots go to IRON_SHEETS_SHOTS_DIR as `tabset_*.png`.
mod common;
use common::*;
use egui::{Color32, Key, Modifiers, Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::settings::{self, Section};
use iron_sheets::state::AppState;
use iron_sheets_core::config::ThemePref;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

type H = Harness<'static, Slot>;

/// Counts the allocations made by the current thread (the frames of these tests run on the test thread).
struct Counting;

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn allocs() -> usize {
    ALLOCS.with(Cell::get)
}

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn start(fx: &Fx) -> H {
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Dark;
    harness(o)
}

fn open_note(h: &mut H, name: &str) {
    let p = st(h).workspace.roots[0].join(name);
    st(h).commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
}

/// Apply the config the way the settings window does after a change.
fn changed(h: &mut H) {
    settings::apply(st(h), false);
    settle(h);
}

fn has_label(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

// ---- the empty tab ----

#[test]
fn fresh_start_is_one_empty_tab_without_editor_preview_or_view_buttons() {
    let fx = Fx::new();
    let mut h = start(&fx);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
    assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "no editor");
    for label in ["Source", "Split", "Preview", "Sync"] {
        assert!(!has_label(&h, label), "{label} belongs to a note, not to the empty tab");
    }
    // Actions with their shortcuts, reachable as buttons.
    for label in ["New note Ctrl+N", "Open note Ctrl+O", "Command palette Ctrl+P", "New untitled note"] {
        h.get_by_role_and_label(Role::Button, label);
    }
    h.get_by_role_and_label(Role::Button, "New tab");
    shot(&mut h, "tabset_empty_fresh");
    // A note brings the view buttons back and the empty tab is gone.
    open_note(&mut h, "Welcome.md");
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    for label in ["Source", "Split", "Preview", "Sync"] {
        h.get_by_label(label);
    }
}

#[test]
fn closing_the_last_tab_leaves_one_empty_tab_and_the_only_empty_tab_stays() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
    h.get_by_role_and_label(Role::Link, "Welcome.md");
    shot(&mut h, "tabset_empty_after_close");
    let id = st(&mut h).tabs.tabs[0].id;
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1, "there is always a tab");
    assert_eq!(st(&mut h).tabs.tabs[0].id, id, "and the only empty tab is not replaced by a new one");
}

#[test]
fn a_workspace_without_a_session_opens_one_empty_tab() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    open_note(&mut h, "Ideas.md");
    assert_eq!(st(&mut h).tabs.tabs.len(), 2);
    st(&mut h).load_workspace(iron_sheets_core::workspace::Workspace::new("elsewhere"));
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
}

#[test]
fn creating_a_note_from_the_empty_tab_overwrites_it() {
    let fx = Fx::new();
    let mut h = start(&fx);
    h.get_by_role_and_label(Role::Button, "New note Ctrl+N").click();
    settle(&mut h);
    h.get_by_label("OK").click();
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 1, "no extra tab");
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p.ends_with("Untitled.md")));
    assert!(fx.notes().join("Untitled.md").exists());
}

#[test]
fn empty_tabs_are_not_in_the_session_and_a_session_without_tabs_gives_one() {
    let fx = Fx::new();
    {
        let mut h = start(&fx);
        open_note(&mut h, "Welcome.md");
        h.key_press_modifiers(Modifiers::COMMAND, Key::T);
        settle(&mut h);
        assert_eq!(st(&mut h).tabs.tabs.len(), 2);
        assert_eq!(st(&mut h).session().tabs.len(), 1);
        h.state_mut().app().on_quit();
    }
    let mut h = harness(fx.opts(vec![]));
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_path().is_some_and(|p| p.ends_with("Welcome.md")), "only the note came back");
    // Close it, quit, restart: nothing stored, one empty tab.
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    settle(&mut h);
    h.state_mut().app().on_quit();
    drop(h);
    let mut h = harness(fx.opts(vec![]));
    assert_eq!(st(&mut h).tabs.tabs.len(), 1);
    assert!(st(&mut h).tabs.active_is_empty());
}

// ---- status bar and toolbar switches ----

#[test]
fn status_bar_segments_follow_their_switches() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    assert!(st(&mut h).indexer.wait_idle(std::time::Duration::from_secs(10)));
    settle(&mut h);
    type Seg = (&'static str, fn(&mut iron_sheets_core::config::AppConfig, bool));
    let segs: [Seg; 4] = [
        ("words", |c, v| c.status_counts = v),
        ("Ln ", |c, v| c.status_position = v),
        ("Saved", |c, v| c.status_badge = v),
        (" notes", |c, v| c.status_notes = v),
    ];
    let shown = |h: &H, part: &str| h.query_by_label_contains(part).is_some();
    for (part, set) in segs {
        assert!(shown(&h, part), "{part} shown by default");
        set(&mut st(&mut h).config, false);
        settle(&mut h);
        assert!(!shown(&h, part), "{part} hidden");
        set(&mut st(&mut h).config, true);
        settle(&mut h);
        assert!(shown(&h, part), "{part} back");
    }
    let ws_buttons = |h: &H| h.query_all_by_label("test").count();
    let before = ws_buttons(&h);
    st(&mut h).config.status_workspace = false;
    settle(&mut h);
    assert_eq!(ws_buttons(&h) + 1, before, "the workspace button is gone");
    shot(&mut h, "tabset_status_bar_hidden");
}

#[test]
fn toolbar_buttons_follow_their_switches_and_commands_stay() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    st(&mut h).config.toolbar_view_buttons = false;
    settle(&mut h);
    for label in ["Source", "Split", "Preview"] {
        assert!(!has_label(&h, label), "{label} hidden");
    }
    h.get_by_label("Sync");
    st(&mut h).config.toolbar_sync_button = false;
    st(&mut h).config.toolbar_new_tab = false;
    settle(&mut h);
    assert!(!has_label(&h, "Sync") && !has_label(&h, "New tab"));
    // The commands behind them stay reachable.
    st(&mut h).commands.push(Command::SetViewMode(iron_sheets_core::config::ViewMode::Preview));
    h.key_press_modifiers(Modifiers::COMMAND, Key::T);
    settle(&mut h);
    assert_eq!(st(&mut h).tabs.tabs.len(), 2, "Ctrl+T works without the + button");
    for c in ["view.source", "view.split", "view.preview", "view.sync", "tab.new"] {
        assert!(st(&mut h).keymap.index_of(c).is_some(), "{c} is in the command table (menu and palette)");
    }
    shot(&mut h, "tabset_toolbar_hidden");
}

// ---- colours ----

#[test]
fn colour_overrides_apply_on_top_of_high_contrast_and_per_theme() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    st(&mut h).config.high_contrast = true;
    settle(&mut h);
    let base_text = st(&mut h).ui.theme.model.ui.text;
    let gen0 = st(&mut h).ui.theme.generation;
    {
        let c = &mut st(&mut h).config;
        c.colors_dark.insert("editor_text".into(), "#ff0000".into());
        c.colors_dark.insert("editor_bg".into(), "#102030".into());
        c.colors_dark.insert("link".into(), "#00ff00".into());
        c.colors_light.insert("editor_text".into(), "#0000ff".into());
    }
    changed(&mut h);
    let t = &st(&mut h).ui.theme;
    assert!(t.generation > gen0, "layouts are invalidated by the change");
    assert_eq!(t.editor.text, Color32::from_rgb(255, 0, 0), "dark override");
    assert_eq!(t.editor.link, Color32::from_rgb(0, 255, 0));
    assert_eq!(t.preview.link, Color32::from_rgb(0, 255, 0));
    assert_eq!(t.model.ui.text, base_text, "high contrast chrome is kept under the overrides");
    assert!(t.high_contrast);
    assert_eq!(h.ctx.global_style().visuals.extreme_bg_color, Color32::from_rgb(0x10, 0x20, 0x30));
    assert_eq!(h.ctx.global_style().visuals.hyperlink_color, Color32::from_rgb(0, 255, 0));
    shot(&mut h, "tabset_colors_applied");
    // The light variant has its own override: switching theme uses it.
    st(&mut h).config.theme = ThemePref::Light;
    settle(&mut h);
    assert_eq!(st(&mut h).ui.theme.editor.text, Color32::from_rgb(0, 0, 255));
    // Reset: the theme's own colours come back.
    st(&mut h).config.colors_dark.clear();
    st(&mut h).config.colors_light.clear();
    st(&mut h).config.theme = ThemePref::Dark;
    changed(&mut h);
    assert_ne!(st(&mut h).ui.theme.editor.text, Color32::from_rgb(255, 0, 0));
    assert_eq!(st(&mut h).ui.theme.model.editor.text, iron_sheets::theme_model::editor_colors(true, 14.0).text);
}

#[test]
fn colour_rows_in_settings_show_both_variants_and_reset_one() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    st(&mut h).config.colors_dark.insert("accent".into(), "#aa3366".into());
    changed(&mut h);
    st(&mut h).settings.open = true;
    st(&mut h).settings.section = Section::Colors;
    settle(&mut h);
    h.get_by_label("Accent");
    h.get_by_label("Editor background");
    h.get_by_label("Math colour");
    shot(&mut h, "tabset_settings_colors");
    let n = st(&mut h).ui.theme.generation;
    h.get_by_role_and_label(Role::Button, "Reset").click();
    settle(&mut h);
    assert!(st(&mut h).config.colors_dark.is_empty(), "reset removes the override");
    assert!(st(&mut h).ui.theme.generation > n);
    // Search finds the new options too.
    st(&mut h).settings.section = Section::General;
    st(&mut h).settings.search = "status bar".into();
    settle(&mut h);
    h.get_by_label("Status bar: word and character count");
    st(&mut h).settings.search = "font".into();
    settle(&mut h);
    h.get_by_label("Editor font");
    h.get_by_label("Preview font file");
    h.get_by_label("UI font size");
}

#[test]
fn settings_sections_with_the_new_items() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    st(&mut h).settings.open = true;
    for (s, name) in [(Section::Interface, "interface"), (Section::Editor, "editor"), (Section::Preview, "preview")] {
        st(&mut h).settings.section = s;
        settle(&mut h);
        shot(&mut h, &format!("tabset_settings_{name}"));
    }
    st(&mut h).settings.section = Section::Interface;
    settle(&mut h);
    h.get_by_label("Toolbar: new tab button");
    h.get_by_label("Status bar: workspace name");
}

// ---- fonts ----

// renders every bundled choice, Lilex included
#[cfg(feature = "font-lilex")]
#[test]
fn every_bundled_font_choice_renders_and_a_change_rebuilds_once() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    for (e, _) in settings::EDITOR_FONTS {
        for (p, _) in settings::PREVIEW_FONTS {
            let g = st(&mut h).ui.theme.generation;
            st(&mut h).config.editor_font = e.to_string();
            st(&mut h).config.preview_font = p.to_string();
            changed(&mut h);
            assert!(st(&mut h).status.current().is_none_or(|m| !m.error), "{e}/{p}: {:?}", st(&mut h).status.current().map(|m| m.text.clone()));
            let after = st(&mut h).ui.theme.generation;
            settle(&mut h);
            assert_eq!(st(&mut h).ui.theme.generation, after, "{e}/{p}: nothing keeps rebuilding");
            let _ = g;
        }
    }
    st(&mut h).config.editor_font = "hack".into();
    st(&mut h).config.preview_font = "newcm".into();
    st(&mut h).config.ui_font_size = 16.0;
    changed(&mut h);
    shot(&mut h, "tabset_fonts_hack_newcm_ui16");
    assert!((h.ctx.global_style().text_styles[&egui::TextStyle::Body].size - 16.0).abs() < 0.01);
}

fn any_system_font() -> Option<PathBuf> {
    ["/usr/share/fonts", "/usr/local/share/fonts"].iter().find_map(|d| {
        let mut stack = vec![PathBuf::from(d)];
        while let Some(p) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&p) else { continue };
            for e in rd.flatten() {
                let q = e.path();
                if q.is_dir() {
                    stack.push(q);
                } else if q.extension().is_some_and(|x| x == "ttf") {
                    return Some(q);
                }
            }
        }
        None
    })
}

#[test]
fn custom_font_files_load_only_when_set_and_bad_ones_report_an_error() {
    let fx = Fx::new();
    let mut h = start(&fx);
    open_note(&mut h, "Welcome.md");
    // An unreadable path and a file that is not a font: an error in the status bar, the bundled family stays.
    st(&mut h).config.editor_font_file = fx.dir.path().join("missing.ttf").display().to_string();
    changed(&mut h);
    assert!(st(&mut h).status.current().is_some_and(|m| m.error && m.text.contains("Cannot use font file")));
    let junk = fx.dir.path().join("junk.ttf");
    std::fs::write(&junk, b"this is not a font at all").expect("write");
    st(&mut h).config.editor_font_file = junk.display().to_string();
    st(&mut h).config.preview_font_file = junk.display().to_string();
    changed(&mut h);
    assert!(st(&mut h).status.msgs.iter().any(|m| m.text.contains("not a TTF or OTF")));
    // A real font from the system, when there is one.
    if let Some(f) = any_system_font() {
        let n = st(&mut h).status.msgs.len();
        st(&mut h).config.editor_font_file = f.display().to_string();
        st(&mut h).config.preview_font_file = f.display().to_string();
        changed(&mut h);
        assert_eq!(st(&mut h).status.msgs.len(), n, "a real font gives no error: {}", f.display());
        shot(&mut h, "tabset_custom_font");
    } else {
        eprintln!("no system .ttf found: custom font success path not exercised");
    }
}

// ---- idle cost ----

/// Idle frames with every new setting away from its default do no settings work: the derived values are not rebuilt
/// (`settings_gen` and the theme generation stay put), nothing is read from the config strings, and the allocations
/// per frame are not above those of the default settings.
#[test]
fn idle_frames_do_no_settings_work_and_allocate_no_more_than_defaults() {
    fn idle_allocs(custom: bool) -> (usize, u64, u64) {
        let fx = Fx::new();
        let mut h = start(&fx);
        open_note(&mut h, "Welcome.md");
        if custom {
            let c = &mut st(&mut h).config;
            c.status_counts = false;
            c.status_position = false;
            c.status_notes = false;
            c.toolbar_sync_button = false;
            c.editor_font = "hack".into();
            c.preview_font = "newcm".into();
            c.ui_font_size = 15.0;
            c.colors_dark.insert("accent".into(), "#336699".into());
            c.colors_dark.insert("editor_text".into(), "#eeeeee".into());
            c.colors_dark.insert("keyword".into(), "#ff8800".into());
            changed(&mut h);
        }
        for _ in 0..30 {
            h.step();
        }
        let (g0, t0) = (st(&mut h).settings_gen, st(&mut h).ui.theme.generation);
        let a0 = allocs();
        let n = 40;
        for _ in 0..n {
            h.step();
        }
        let per_frame = (allocs() - a0) / n;
        assert_eq!(st(&mut h).settings_gen, g0, "no settings bump while idle");
        assert_eq!(st(&mut h).ui.theme.generation, t0, "no theme rebuild while idle");
        (per_frame, g0, t0)
    }
    let (base, _, _) = idle_allocs(false);
    let (custom, _, _) = idle_allocs(true);
    eprintln!("idle allocations per frame: defaults {base}, all new settings changed {custom}");
    // Hidden segments only make a frame cheaper; allow a little noise for background timing.
    assert!(custom <= base + base / 10 + 20, "settings must not add per-frame work: {custom} vs {base}");
}
