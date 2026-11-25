use crate::common::*;
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::{Command, all_commands};
use iron_sheets::keymap::Keymap;
use iron_sheets::keys::{Shortcut, parse_shortcut};
use iron_sheets::settings::{self, Section};
use iron_sheets::{App, AppOptions};
use iron_sheets_core::config::{AppConfig, ViewMode};
use std::time::Duration;

type H = Harness<'static, Slot>;

fn open_note(h: &mut H, name: &str) -> std::path::PathBuf {
    let p = h.state_mut().app().state.workspace.roots[0].join(name);
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    settle(h);
    p
}

fn view(h: &mut H) -> ViewMode {
    h.state_mut().app().state.tabs.active_tab().expect("tab").view
}

fn bind(h: &mut H, id: &str, keys: &[&str]) {
    let st = &mut h.state_mut().app().state;
    let i = st.keymap.index_of(id).expect("command id");
    let v: Vec<Shortcut> = keys.iter().map(|k| parse_shortcut(k).expect("shortcut")).collect();
    settings::set_bindings(st, i, v);
}

fn restart_opts(fx: &Fx) -> AppOptions {
    let mut o = AppOptions::new(fx.dirs());
    o.paths = vec![fx.notes()];
    o.workspace = Some("test".into());
    o
}

#[test]
fn rebound_shortcut_runs_the_command_and_old_one_does_not() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_note(&mut h, "Welcome.md");
    let v0 = view(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    settle(&mut h);
    assert_ne!(view(&mut h), v0, "default Ctrl+E cycles the view");
    let v1 = view(&mut h);
    bind(&mut h, "view.cycle", &["Ctrl+J"]);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    settle(&mut h);
    assert_eq!(view(&mut h), v1, "old shortcut is gone");
    h.key_press_modifiers(Modifiers::COMMAND, Key::J);
    settle(&mut h);
    assert_ne!(view(&mut h), v1, "new shortcut works");
    // Persisted as a difference only.
    let cfg = &h.state_mut().app().state.config;
    assert_eq!(cfg.keybindings.len(), 1);
    assert_eq!(cfg.keybindings["view.cycle"], vec!["Ctrl+J".to_string()]);
    // Back to the default removes the override.
    bind(&mut h, "view.cycle", &["Ctrl+E"]);
    assert!(h.state_mut().app().state.config.keybindings.is_empty());
}

#[test]
fn recording_captures_next_key_and_reports_conflicts() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_note(&mut h, "Welcome.md");
    {
        let st = &mut h.state_mut().app().state;
        st.settings.open = true;
        st.settings.section = Section::Keyboard;
        st.settings.recording = st.keymap.index_of("view.sync");
    }
    settle(&mut h);
    // Ctrl+E is "view.cycle": recording takes it instead of running it.
    let v0 = view(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    settle(&mut h);
    assert_eq!(view(&mut h), v0);
    let st = &h.state_mut().app().state;
    assert!(st.settings.recording.is_none());
    assert_eq!(st.keymap.conflicts.len(), 1, "{:?}", st.keymap.conflicts);
    assert!(st.settings.note.starts_with("Conflict"), "{}", st.settings.note);
    assert!(h.get_all_by_label_contains("Conflict: Ctrl+E").next().is_some());
    shot(&mut h, "settings_keyboard_conflict");
    // A bare letter is refused.
    {
        let st = &mut h.state_mut().app().state;
        st.settings.recording = st.keymap.index_of("view.sync");
    }
    h.key_press(Key::A);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert!(st.settings.note.contains("needs Ctrl or Alt"), "{}", st.settings.note);
    assert_eq!(st.keymap.bindings[st.keymap.index_of("view.sync").expect("id")].len(), 1);
}

#[test]
fn settings_persist_across_restart() {
    let fx = Fx::new();
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        bind(&mut h, "file.save", &["Ctrl+Alt+S", "F9"]);
        let st = &mut h.state_mut().app().state;
        st.config.editor_font_size = 19.0;
        st.config.exclude_globs = vec!["*.bak".into()];
        st.save_config_now();
    }
    let mut h = harness(restart_opts(&fx));
    let st = &h.state_mut().app().state;
    assert_eq!(st.config.editor_font_size, 19.0);
    assert_eq!(st.config.exclude_globs, vec!["*.bak".to_string()]);
    let i = st.keymap.index_of("file.save").expect("id");
    assert_eq!(st.keymap.bindings[i].len(), 2);
    assert_eq!(st.keymap.hint(all_commands().iter().position(|c| c.id == "file.save").expect("cmd")), "Ctrl+Alt+S");
}

#[test]
fn broken_config_falls_back_to_defaults_with_a_message() {
    let fx = Fx::new();
    std::fs::create_dir_all(&fx.dirs().config).expect("mkdir");
    std::fs::write(AppConfig::file(&fx.dirs()), b"{ this is not json").expect("write");
    let mut h = harness(restart_opts(&fx));
    let st = &h.state_mut().app().state;
    assert_eq!(st.config.autosave_delay_ms, AppConfig::default().autosave_delay_ms);
    assert!(st.keymap.conflicts.is_empty());
    let msg = st.status.current().map(|m| m.text.clone()).unwrap_or_default();
    assert!(msg.contains("not valid JSON"), "{msg:?}");
    // One bad value keeps the rest.
    std::fs::write(AppConfig::file(&fx.dirs()), br#"{"autosave": "maybe", "editor_font_size": 17.0, "keybindings": {"file.save": ["Bogus+"]}}"#)
        .expect("write");
    let mut h = harness(restart_opts(&fx));
    let st = &h.state_mut().app().state;
    assert_eq!(st.config.editor_font_size, 17.0);
    assert!(st.config.autosave);
    assert!(!st.keymap.warnings.is_empty());
}

#[test]
fn autosave_off_keeps_the_document_dirty() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let p = open_note(&mut h, "Ideas.md");
    h.state_mut().app().state.config.autosave = false;
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p.clone(), range: 0..0, insert: "EDIT ".into(), select: None });
    settle(&mut h);
    std::thread::sleep(Duration::from_millis(300));
    h.run_steps(3);
    assert!(h.state_mut().app().state.docs.get(&p).expect("doc").is_dirty());
    assert!(!std::fs::read_to_string(&p).expect("read").starts_with("EDIT"));
    h.state_mut().app().state.config.autosave = true;
    std::thread::sleep(Duration::from_millis(300));
    h.run_steps(3);
    assert!(std::fs::read_to_string(&p).expect("read").starts_with("EDIT"));
}

#[test]
fn wrap_off_makes_long_lines_one_row() {
    let fx = Fx::new();
    let long = format!("# Long\n\n{}\n", "word ".repeat(6000));
    std::fs::write(fx.notes().join("Long.md"), &long).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_note(&mut h, "Long.md");
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").view = ViewMode::Source;
    settle(&mut h);
    let wrapped = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.content_h;
    h.state_mut().app().state.config.editor_wrap = false;
    settle(&mut h);
    let flat = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.content_h;
    assert!(wrapped > flat * 2.0, "wrapped {wrapped} flat {flat}");
}

#[test]
fn files_options_hide_non_notes_and_excluded_folders() {
    let fx = Fx::new();
    std::fs::write(fx.notes().join("data.bin"), b"x").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    assert!(h.query_by_label("data.bin").is_some());
    assert!(h.query_by_label("Projects").is_some());
    {
        let st = &mut h.state_mut().app().state;
        st.config.show_other_files = false;
        st.config.exclude_globs = vec!["Projects".into()];
    }
    settings::apply(&mut h.state_mut().app().state, false);
    settle(&mut h);
    assert!(h.query_by_label("data.bin").is_none());
    assert!(h.query_by_label("Projects").is_none());
    assert!(h.query_by_label("Welcome.md").is_some());
}

#[test]
fn settings_window_sections_and_search() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open_note(&mut h, "Welcome.md");
    h.key_press_modifiers(Modifiers::COMMAND, Key::Comma);
    settle(&mut h);
    assert!(h.state_mut().app().state.settings.open, "Ctrl+, opens settings");
    h.get_by_label("Autosave");
    shot(&mut h, "settings_general");
    for s in [Section::Editor, Section::Preview, Section::Typst, Section::Files, Section::Keyboard] {
        h.state_mut().app().state.settings.section = s;
        settle(&mut h);
        shot(&mut h, &format!("settings_{}", s.title().to_lowercase()));
    }
    // Search across sections.
    {
        let st = &mut h.state_mut().app().state;
        st.settings.search = "font".into();
    }
    settle(&mut h);
    h.get_by_label("Editor font size");
    h.get_by_label("Preview font size");
    assert!(h.query_by_label("Autosave").is_none());
    shot(&mut h, "settings_search");
    assert_eq!(settings::matching_items("zzzz"), 0);
    assert!(settings::matching_items("") >= 18);
    // Toggle a checkbox through the UI.
    h.state_mut().app().state.settings.search.clear();
    h.state_mut().app().state.settings.section = Section::General;
    settle(&mut h);
    let before = h.state_mut().app().state.config.autosave;
    h.get_by_label("Autosave").click();
    settle(&mut h);
    assert_ne!(h.state_mut().app().state.config.autosave, before);
}

#[test]
fn keymap_dispatch_cost_old_vs_new() {
    let ctx = egui::Context::default();
    let km = Keymap::defaults();
    let legacy_order: Vec<Shortcut> = {
        let mut v: Vec<Shortcut> = all_commands().iter().filter_map(|c| c.shortcut).collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.mods.alt as u32 + s.mods.ctrl as u32 + s.mods.shift as u32 + s.mods.command as u32));
        v
    };
    let legacy = |ctx: &egui::Context| -> usize {
        // The previous implementation: allocate, sort, probe every command.
        let mut order: Vec<&iron_sheets::command::CommandInfo> = all_commands().iter().filter(|c| c.shortcut.is_some()).collect();
        order
            .sort_by_key(|c| std::cmp::Reverse(c.shortcut.map_or(0, |s| s.mods.alt as u32 + s.mods.ctrl as u32 + s.mods.shift as u32 + s.mods.command as u32)));
        let mut n = 0;
        for info in order {
            if let Some(s) = info.shortcut {
                if ctx.input_mut(|i| i.consume_shortcut(&iron_sheets::keys_egui::shortcut_to_egui(&s))) {
                    n += 1;
                }
            }
        }
        n
    };
    let _ = legacy_order;
    let press = |k: Key, m: Modifiers| egui::RawInput {
        events: vec![Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers: m }],
        ..Default::default()
    };
    let n = 2000;
    let mut results = Vec::new();
    for (name, input) in [("no key", None), ("Ctrl+S", Some(press(Key::S, Modifiers::COMMAND)))] {
        let (mut t_old, mut t_new) = (Duration::ZERO, Duration::ZERO);
        let (mut hit_old, mut hit_new) = (0, 0);
        for _ in 0..n {
            let raw = input.clone().unwrap_or_default();
            ctx.run_ui(raw, |ui| {
                let t = std::time::Instant::now();
                hit_new += iron_sheets::keys_egui::dispatch(&km, ui.ctx()).len();
                t_new += t.elapsed();
            })
            .drop_without_applying_deltas();
            let raw = input.clone().unwrap_or_default();
            ctx.run_ui(raw, |ui| {
                let t = std::time::Instant::now();
                hit_old += legacy(ui.ctx());
                t_old += t.elapsed();
            })
            .drop_without_applying_deltas();
        }
        assert_eq!(hit_old, hit_new, "{name}: same commands fire");
        results.push(format!("{name}: old {:.2} us, new {:.2} us per frame", t_old.as_secs_f64() * 1e6 / n as f64, t_new.as_secs_f64() * 1e6 / n as f64));
    }
    println!("DISPATCH {}", results.join(" | "));
}

#[allow(dead_code)]
fn _unused(_: App) {}
