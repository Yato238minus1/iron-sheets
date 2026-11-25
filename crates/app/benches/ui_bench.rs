use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use egui::{Context, Event, Key, Modifiers, Pos2, RawInput, Rect, vec2};
use iron_sheets::command::Command;
use iron_sheets::editor::highlight;
use iron_sheets::theme::EditorStyle;
use iron_sheets::theme_model::editor_colors;
use iron_sheets::{App, AppOptions};
use iron_sheets_core::FileKind;
use iron_sheets_core::config::{AppConfig, ViewMode};
use iron_sheets_core::paths::AppDirs;
use std::time::Duration;
use tempfile::TempDir;

fn make_note(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 1024);
    let mut i = 0usize;
    while s.len() < bytes {
        let m = i % 10;
        s.push_str(&format!(
            "## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, with inline math $x^{m} + y$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n> quote {i} with a little more text in it\n\n```rust\nfn f{i}() {{ let a = {i}; }}\n```\n\n| a | b |\n|---|---|\n| {i} | 2 |\n\n"
        ));
        i += 1;
    }
    s
}

fn cpu_frame(ctx: &Context, app: &mut App, events: Vec<Event>) {
    ctx.disable_accesskit();
    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), events, predicted_dt: 1.0 / 60.0, ..Default::default() };
    let mut out = ctx.run_ui(input, |ui| app.frame(ui));
    let _ = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
    out.textures_delta.clear();
}

fn setup_app(note_text: &str, mode: ViewMode, tree_dirs: usize) -> (TempDir, Context, App, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("notes");
    std::fs::create_dir_all(&root).unwrap();
    for d in 0..tree_dirs {
        let sub = root.join(format!("folder-{d:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("item.md"), "# Item\n").unwrap();
    }
    let note_path = root.join("Target.md");
    std::fs::write(&note_path, note_text).unwrap();

    let mut opts = AppOptions::new(AppDirs::at(&dir.path().join("home")));
    opts.config = AppConfig { autosave: false, restore_session: false, default_view: mode, ..AppConfig::default() };
    opts.paths = vec![root.clone()];
    opts.workspace = Some("bench".into());

    let ctx = Context::default();
    let mut app = App::new(&ctx, opts);
    assert!(app.state.indexer.wait_idle(Duration::from_secs(30)));
    for d in 0..tree_dirs {
        app.state.tree.set_expanded(&root.join(format!("folder-{d:03}")), true);
    }
    app.state.commands.push(Command::OpenPath { path: note_path.clone(), new_tab: true, line: None });
    app.state.commands.push(Command::SetViewMode(mode));
    for _ in 0..16 {
        cpu_frame(&ctx, &mut app, vec![]);
    }
    // Wait for background doc load to complete before touching docs/cursor.
    for _ in 0..200 {
        if app.state.docs.get(&note_path).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
        cpu_frame(&ctx, &mut app, vec![]);
    }
    // Wait for initial preview parse/layout if preview is visible.
    if mode != ViewMode::Source {
        for _ in 0..200 {
            cpu_frame(&ctx, &mut app, vec![]);
            if app.state.ui.md_preview.parse_count > 0 && app.state.ui.md_preview.unlaid == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    // Place caret in prose in the middle of the note and settle focus.
    let mid = note_text[..note_text.len() / 2].rfind("Lorem ipsum ").map_or(note_text.len() / 2, |p| p + "Lorem ipsum ".len());
    if let Some(t) = app.state.tabs.active_tab_mut() {
        t.pending_cursor = Some(mid..mid);
    }
    for _ in 0..8 {
        cpu_frame(&ctx, &mut app, vec![]);
    }
    (dir, ctx, app, note_path)
}

fn backspace_events() -> Vec<Event> {
    vec![
        Event::Key { key: Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
        Event::Key { key: Key::Backspace, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE },
    ]
}

fn bench_editor_highlight_layout(c: &mut Criterion) {
    let text_a = make_note(100 * 1024);
    let mid = text_a[..text_a.len() / 2].rfind("Lorem ipsum ").map_or(text_a.len() / 2, |p| p + "Lorem ipsum ".len());
    let mut text_b = text_a.clone();
    text_b.insert(mid, 'x');
    let style = EditorStyle::from_model(&editor_colors(false, 14.0));

    let mut group = c.benchmark_group("ui/editor_highlight_layout");
    group.sample_size(30);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Bytes(text_a.len() as u64));
    group.bench_function("layout_100kb_cold_miss", |b| {
        let mut alt = String::with_capacity(text_a.len());
        alt.push_str("# Alternate\n");
        alt.push_str(&text_a[..text_a.len() - 12]);
        let mut flip = false;
        b.iter(|| {
            flip = !flip;
            let t = if flip { &text_a } else { &alt };
            black_box(highlight::layout(black_box(t), FileKind::Markdown, &style, 800.0))
        })
    });
    let _ = highlight::layout(&text_a, FileKind::Markdown, &style, 800.0);
    let mut kflip = true;
    group.bench_function("layout_100kb_keystroke", |b| {
        b.iter(|| {
            let t = if kflip { &text_b } else { &text_a };
            kflip = !kflip;
            black_box(highlight::layout(black_box(t), FileKind::Markdown, &style, 800.0))
        })
    });
    group.finish();
}

fn bench_app_frames(c: &mut Criterion) {
    let note_100k = make_note(100 * 1024);
    let note_350k = make_note(360 * 1024);

    let mut group = c.benchmark_group("ui/app_frame");
    group.sample_size(25);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));

    // 1. Steady idle frame in Source mode (~100 KB, sidebar with 200 expanded dirs).
    {
        let (_dir, ctx, mut app, _) = setup_app(&note_100k, ViewMode::Source, 200);
        group.bench_function("idle_100kb_source_200_dirs", |b| b.iter(|| cpu_frame(&ctx, &mut app, vec![])));
    }

    // 2. Keystroke edit frame in Source mode (~100 KB).
    {
        let (_dir, ctx, mut app, path) = setup_app(&note_100k, ViewMode::Source, 50);
        let v0 = app.state.docs.get(&path).unwrap().version;
        let len0 = app.state.docs.get(&path).unwrap().text.len();
        cpu_frame(&ctx, &mut app, vec![Event::Text("a".into())]);
        cpu_frame(&ctx, &mut app, backspace_events());
        let v1 = app.state.docs.get(&path).unwrap().version;
        let len1 = app.state.docs.get(&path).unwrap().text.len();
        assert_eq!(v1, v0 + 2, "keystroke reached editor");
        assert_eq!(len1, len0, "backspace restored length");

        let mut insert_next = true;
        group.bench_function("keystroke_100kb_source", |b| {
            b.iter(|| {
                let evs = if insert_next { vec![Event::Text("a".into())] } else { backspace_events() };
                insert_next = !insert_next;
                cpu_frame(&ctx, &mut app, evs);
            })
        });
    }

    // 3. Keystroke edit frame on big note (> 300 KB highlight limit -> relayout + layout_big + BigSource::sections + galley patch).
    {
        let (_dir, ctx, mut app, path) = setup_app(&note_350k, ViewMode::Source, 50);
        let tab_id = app.state.tabs.active_tab().unwrap().id;
        let st0 = app.state.ui.editor.stats(tab_id).unwrap();
        assert!(st0.placeholders > 0, "expected big-note placeholder layout path");

        let v0 = app.state.docs.get(&path).unwrap().version;
        cpu_frame(&ctx, &mut app, vec![Event::Text("a".into())]);
        cpu_frame(&ctx, &mut app, backspace_events());
        assert_eq!(app.state.docs.get(&path).unwrap().version, v0 + 2);

        let mut insert_next = true;
        group.bench_function("keystroke_360kb_big_source", |b| {
            b.iter(|| {
                let evs = if insert_next { vec![Event::Text("a".into())] } else { backspace_events() };
                insert_next = !insert_next;
                cpu_frame(&ctx, &mut app, evs);
            })
        });
    }

    // 4. Steady idle frame in Split mode (~100 KB, editor + markdown preview + scrollsync + statusbar + sidebar).
    {
        let (_dir, ctx, mut app, _) = setup_app(&note_100k, ViewMode::Split, 50);
        group.bench_function("idle_100kb_split", |b| b.iter(|| cpu_frame(&ctx, &mut app, vec![])));
    }

    // 5. Keystroke edit frame in Split mode (~100 KB).
    {
        let (_dir, ctx, mut app, path) = setup_app(&note_100k, ViewMode::Split, 50);
        let v0 = app.state.docs.get(&path).unwrap().version;
        cpu_frame(&ctx, &mut app, vec![Event::Text("a".into())]);
        cpu_frame(&ctx, &mut app, backspace_events());
        assert_eq!(app.state.docs.get(&path).unwrap().version, v0 + 2);

        let mut insert_next = true;
        group.bench_function("keystroke_100kb_split", |b| {
            b.iter(|| {
                let evs = if insert_next { vec![Event::Text("a".into())] } else { backspace_events() };
                insert_next = !insert_next;
                cpu_frame(&ctx, &mut app, evs);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_editor_highlight_layout, bench_app_frames);
criterion_main!(benches);
