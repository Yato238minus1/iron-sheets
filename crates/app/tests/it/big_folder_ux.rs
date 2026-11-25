//! Instant startup, lazy priority loading, stale-while-revalidate, and skeleton UI tests on big folders.
use crate::common::*;
use egui::{Pos2, RawInput, Rect, vec2};
use egui_kittest::kittest::Queryable;
use iron_sheets::{App, AppOptions, command::Command};
use iron_sheets_core::config::{AppConfig, ThemePref};
use iron_sheets_core::paths::AppDirs;
use std::path::Path;
use std::time::{Duration, Instant};

fn create_big_folder(root: &Path, top_notes: usize, subdirs: usize, notes_per_subdir: usize) {
    std::fs::create_dir_all(root).expect("mkdir big root");
    for i in 0..top_notes {
        std::fs::write(root.join(format!("note_{i:04}.md")), "# Note\n").expect("write note");
    }
    for d in 0..subdirs {
        let sub = root.join(format!("sub_{d:03}"));
        std::fs::create_dir_all(&sub).expect("mkdir sub");
        for j in 0..notes_per_subdir {
            std::fs::write(sub.join(format!("item_{j}.md")), "# Sub\n").expect("write sub note");
        }
    }
}

#[test]
fn instant_startup_and_add_root_on_5000_files_200_subdirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = dir.path().join("big_vault");
    // 4,000 top-level notes + 200 subdirectories * 5 notes = 5,000 files + 200 subdirs.
    create_big_folder(&big, 4000, 200, 5);

    // 1. Measure App::new + Frame 1 directly without blocking on 5,000 files.
    let mut opts = AppOptions::new(AppDirs::at(&dir.path().join("home1")));
    opts.config = AppConfig { autosave_delay_ms: 50, ..AppConfig::default() };
    opts.paths = vec![big.clone()];
    opts.workspace = Some("big".into());

    let ctx = egui::Context::default();
    ctx.disable_accesskit();
    let t_new = Instant::now();
    let mut app = App::new(&ctx, opts);
    let app_new_ms = t_new.elapsed().as_secs_f64() * 1000.0;
    // Early startup kickoff enqueues at most the root on a background thread without blocking App::new.
    assert!(app.state.tree.dir_reads() <= 1, "at most the root may be read before Frame 1, got {}", app.state.tree.dir_reads());

    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
    let t_f1 = Instant::now();
    let mut out = ctx.run_ui(input, |ui| app.frame(ui));
    out.textures_delta.clear();
    let frame1_ms = t_f1.elapsed().as_secs_f64() * 1000.0;

    // Frame 1 only queues the root asynchronously (collapsed subdirs are untouched).
    assert!(app.state.tree.dir_reads() <= 1, "only root may be read lazily, got {}", app.state.tree.dir_reads());

    // Wait for background loading of top-level root to finish.
    let wait_start = Instant::now();
    while app.state.tree.has_pending_loads() {
        assert!(wait_start.elapsed() < Duration::from_secs(10), "tree load timeout");
        std::thread::sleep(Duration::from_millis(1));
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| app.frame(ui));
        out.textures_delta.clear();
    }
    let rows = app.state.tree.rows();
    assert!(!rows.iter().any(|r| r.skeleton), "no skeleton rows after completion");
    assert_eq!(rows.len(), 1 + 4200, "all 4000 files + 200 subdirs under root");
    assert_eq!(app.state.tree.dir_reads(), 1, "collapsed subdirs must NEVER be read: {}", app.state.tree.dir_reads());

    // 2. Measure Command::AddRoot + frame time on the UI thread when adding the 5,000-file root.
    let mut opts2 = AppOptions::new(AppDirs::at(&dir.path().join("home2")));
    opts2.config = AppConfig { autosave_delay_ms: 50, ..AppConfig::default() };
    opts2.workspace = Some("addroot".into());
    let mut h = harness(opts2);
    h.state_mut().app().state.tree.set_async_paused(true);
    h.state_mut().app().state.commands.push(Command::AddRoot(big.clone()));
    let t_add = Instant::now();
    h.step();
    let add_root_frame_ms = t_add.elapsed().as_secs_f64() * 1000.0;
    {
        let st = &mut h.state_mut().app().state;
        assert!(st.tree.has_pending_loads(), "AddRoot must load asynchronously");
        assert!(st.tree.rows().iter().any(|r| r.skeleton), "skeleton rows visible immediately on AddRoot");
    }
    h.state_mut().app().state.tree.set_async_paused(false);
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    assert!(!st.tree.has_pending_loads());
    assert_eq!(st.tree.rows().len(), 1 + 4200);
    assert_eq!(st.tree.dir_reads(), 1, "AddRoot expands only the new root; subdirs stay lazy");

    eprintln!(
        "BIG_FOLDER_UX 5000-file/200-subdir: App::new={app_new_ms:.2} ms, Frame1={frame1_ms:.2} ms, AddRoot+Frame={add_root_frame_ms:.2} ms"
    );
    assert!(frame1_ms < 250.0, "Frame 1 blocked UI thread: {frame1_ms:.2} ms");
    assert!(add_root_frame_ms < 250.0, "AddRoot frame blocked UI thread: {add_root_frame_ms:.2} ms");
}

#[test]
fn skeleton_ui_visual_verification_and_screenshot() {
    let fx = Fx::new();
    let mut o = fx.opts(vec![fx.notes(), fx.journal()]);
    o.config.theme = ThemePref::Dark;
    o.config.sidebar_width = 270.0;
    let mut h = harness(o);
    // Open Welcome.md so the split editor/preview looks realistic alongside the sidebar.
    h.state_mut().app().state.commands.push(Command::OpenPath { path: fx.notes().join("Welcome.md"), new_tab: false, line: None });
    settle(&mut h);

    // Pause background worker, then expand `Projects` so skeleton rows appear cleanly under `Projects`.
    h.state_mut().app().state.tree.set_async_paused(true);
    h.get_by_label("Projects").click();
    h.step();
    h.step();

    {
        let st = &mut h.state_mut().app().state;
        assert!(st.tree.has_pending_loads(), "Projects load is pending while paused");
        let skel_count = st.tree.rows().iter().filter(|r| r.skeleton).count();
        assert_eq!(skel_count, 3, "three skeleton placeholder rows shown under Projects");
        // Skeleton rows must not pollute recorded clickable tree row labels.
        let rec = iron_sheets::theme::recorded_rows(&h.ctx, "files");
        assert!(!rec.iter().any(|(name, _)| name.is_empty()), "skeleton rows must not emit recorded row entries");
    }

    let shot_path = shot(&mut h, "skeleton-loading");
    assert!(shot_path.exists(), "screenshot written to {}", shot_path.display());

    // Resume loading and verify skeleton rows swap cleanly into real children.
    h.state_mut().app().state.tree.set_async_paused(false);
    settle(&mut h);
    {
        let st = &mut h.state_mut().app().state;
        assert!(!st.tree.has_pending_loads());
        assert_eq!(st.tree.rows().iter().filter(|r| r.skeleton).count(), 0);
    }
    h.get_by_label("Plan.md");
    h.get_by_label("Tasks.md");

    // Also verify Light theme renders skeleton bars cleanly without panic.
    {
        let st = &mut h.state_mut().app().state;
        st.config.theme = ThemePref::Light;
        st.tree.set_async_paused(true);
    }
    h.get_by_label("Archive").click();
    h.step();
    h.step();
    assert_eq!(h.state_mut().app().state.tree.rows().iter().filter(|r| r.skeleton).count(), 3);
    let _ = shot(&mut h, "skeleton-loading-light");
    h.state_mut().app().state.tree.set_async_paused(false);
    settle(&mut h);
}

#[test]
fn priority_order_lazy_loading_and_stale_while_revalidate() {
    let fx = Fx::new();
    // Create extra subdirectories to verify collapsed subdirs are never read.
    for i in 0..10 {
        let sub = fx.notes().join(format!("Collapsed_{i:02}"));
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(sub.join("hidden_child.md"), "# c\n").expect("write");
    }
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    settle(&mut h);

    // Top-level roots loaded (2 reads total: notes + journal); collapsed subdirs were NEVER read.
    assert_eq!(h.state_mut().app().state.tree.dir_reads(), 2, "only the 2 roots were read");

    // Stale-while-revalidate on invalidate(dir): keeps existing rows visible with ZERO skeleton flicker.
    std::fs::write(fx.notes().join("FreshNote.md"), "# Fresh\n").expect("write fresh");
    h.state_mut().app().state.tree.set_async_paused(true);
    h.state_mut().app().state.tree.invalidate(&fx.notes());
    h.step();
    h.step();
    {
        let st = &mut h.state_mut().app().state;
        assert!(st.tree.has_pending_loads(), "refresh queued");
        assert!(!st.tree.rows().iter().any(|r| r.skeleton), "stale-while-revalidate must NOT flash skeleton rows");
    }
    h.get_by_label("Welcome.md");
    h.get_by_label("Ideas.md");

    // Unpause and settle: FreshNote.md appears cleanly.
    h.state_mut().app().state.tree.set_async_paused(false);
    settle(&mut h);
    h.get_by_label("FreshNote.md");
    assert_eq!(h.state_mut().app().state.tree.dir_reads(), 3, "only invalidated root was re-read; collapsed subdirs untouched");

    // Verify top-level-first priority with restored nested session.expanded vs top-level root + explicit reveal:
    // Save session with Collapsed_00 expanded, then restart workspace with session restore.
    h.state_mut().app().state.tree.set_expanded(&fx.notes().join("Collapsed_00"), true);
    settle(&mut h);
    h.state_mut().app().on_quit();

    let mut o2 = fx.opts(vec![]);
    o2.workspace = Some("test".into());
    let mut h2 = harness(o2);
    // All restored expanded paths (notes, journal, Collapsed_00) should now be loaded cleanly, while Collapsed_01..09 remain unread.
    let reads = h2.state_mut().app().state.tree.dir_reads();
    assert!(reads <= 3, "only roots + restored Collapsed_00 may be read, got {reads}");
    for i in 1..=9 {
        assert!(
            !h2.state_mut().app().state.tree.is_expanded(&fx.notes().join(format!("Collapsed_{i:02}"))),
            "Collapsed_{i:02} must remain collapsed and unread"
        );
    }
    h2.get_by_label("hidden_child.md");
}

#[test]
fn non_blocking_frame1_open_of_multi_mb_note_and_editor_skeleton() {
    let fx = Fx::new();
    let big_path = fx.notes().join("Huge.md");
    let mut big_text = String::with_capacity(4 * 1024 * 1024);
    for i in 0..45_000 {
        big_text.push_str(&format!("# Heading {i}\n\nParagraph {i} with enough words to build a 4 MB Markdown file.\n\n"));
    }
    std::fs::write(&big_path, &big_text).expect("write 4 MB note");

    // Save a session with Huge.md as the active restored tab.
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        h.state_mut().app().state.commands.push(Command::OpenPath { path: big_path.clone(), new_tab: true, line: None });
        settle(&mut h);
        h.state_mut().app().on_quit();
    }

    // 1. Measure real Frame-1 startup without blocking on the 4 MB note read.
    let mut o1 = fx.opts(vec![]);
    o1.workspace = Some("test".into());
    let ctx = egui::Context::default();
    ctx.disable_accesskit();
    let t_new = Instant::now();
    let mut app = App::new(&ctx, o1);
    let app_new_ms = t_new.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(app.state.docs.loads, 0, "zero document disk reads before Frame 1");
    assert_eq!(app.state.docs.len(), 0);

    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
    let t_f1 = Instant::now();
    let mut out = ctx.run_ui(input, |ui| app.frame(ui));
    out.textures_delta.clear();
    let frame1_ms = t_f1.elapsed().as_secs_f64() * 1000.0;
    eprintln!("BIG_NOTE_FRAME1 4MB async open: App::new={app_new_ms:.2} ms, Frame1={frame1_ms:.2} ms");
    assert!(frame1_ms < 200.0, "Frame 1 blocked on 4 MB file load: {frame1_ms:.2} ms");

    // Pump until background open finishes and installs the document.
    let wait_start = Instant::now();
    while app.state.is_active_tab_loading() || app.state.tree.has_pending_loads() {
        assert!(wait_start.elapsed() < Duration::from_secs(10), "timeout waiting for 4 MB async open");
        std::thread::sleep(Duration::from_millis(1));
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| app.frame(ui));
        out.textures_delta.clear();
    }
    assert_eq!(app.state.docs.loads, 1);
    assert_eq!(app.state.docs.get(&big_path).expect("loaded").text.len(), big_text.len());
    drop(app);

    // 2. Deterministic skeleton rendering & screenshot verification while background open is paused.
    let small_path = fx.notes().join("Welcome.md");
    {
        let store = iron_sheets_core::workspace::WorkspaceStore::new(&fx.dirs());
        let mut ws = store.load("test").expect("ws");
        ws.session.tabs = vec![
            iron_sheets_core::workspace::TabState { path: small_path, ..Default::default() },
            iron_sheets_core::workspace::TabState { path: big_path.clone(), ..Default::default() },
        ];
        ws.session.active = Some(0);
        ws.name = "skeleton".into();
        let _ = store.save(&ws);
    }
    for (theme, shot_name) in [(ThemePref::Dark, "editor-skeleton-dark"), (ThemePref::Light, "editor-skeleton-light")] {
        let mut o2 = fx.opts(vec![]);
        o2.workspace = Some("skeleton".into());
        o2.config.theme = theme;
        o2.config.animations = false;
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(1100.0, 720.0))
            .with_step_dt(0.25)
            .with_accessibility_check(false)
            .build_ui_state(
                |ui, slot: &mut Slot| {
                    if slot.app.is_none()
                        && let Some(o) = slot.opts.take()
                    {
                        let mut a = App::new(ui.ctx(), o);
                        a.state.docs.prefetch.set_open_paused(true);
                        a.state.tabs.active = Some(1);
                        slot.app = Some(a);
                    }
                    if let Some(a) = slot.app.as_mut() {
                        a.frame(ui);
                    }
                },
                Slot { opts: Some(o2), app: None },
            );
        h.step();
        h.step();
        h.step();
        {
            let st = &h.state_mut().app().state;
            assert!(st.is_active_tab_loading(), "active tab still loading while open worker paused");
            assert!(st.docs.get(&big_path).is_none());
            assert!(st.status.current().is_none(), "no error shown while loading");
        }
        let shot_path = shot(&mut h, shot_name);
        assert!(shot_path.exists());

        h.state_mut().app().state.docs.prefetch.set_open_paused(false);
        settle(&mut h);
        let st = &h.state_mut().app().state;
        assert!(!st.is_active_tab_loading());
        assert_eq!(st.docs.get(&big_path).expect("doc installed").text.len(), big_text.len());
    }

    // 3. Error case: a restored binary tab surfaces the same error and stays as a failed tab (IS-33).
    let bin_path = fx.notes().join("Bad.md");
    std::fs::write(&bin_path, b"not\0text").expect("write binary");
    {
        let store = iron_sheets_core::workspace::WorkspaceStore::new(&fx.dirs());
        let mut ws = store.load("test").expect("ws");
        ws.session.tabs.push(iron_sheets_core::workspace::TabState { path: bin_path.clone(), ..Default::default() });
        ws.session.active = Some(ws.session.tabs.len() - 1);
        let _ = store.save(&ws);
    }
    let mut o3 = fx.opts(vec![]);
    o3.workspace = Some("test".into());
    let mut h3 = harness(o3);
    let st = &h3.state_mut().app().state;
    assert!(st.tabs.tabs.iter().any(|t| t.path == bin_path && t.load_error.is_some()), "broken tab stays with its reason");
    assert!(st.status.msgs.iter().any(|m| m.error && m.text.contains("binary file, not opened")));
}

#[test]
fn warm_restart_renders_cached_tree_on_frame1_and_reconciles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = dir.path().join("warm_vault");
    // 3,000 top-level notes + 100 subdirs * 5 notes = 3,500 files + 100 subdirs.
    create_big_folder(&big, 3000, 100, 5);
    let dirs = AppDirs::at(&dir.path().join("home"));

    // Session 1: open workspace, expand sub_000, settle, and quit to persist tree-<slug>.bin.
    {
        let mut opts = AppOptions::new(dirs.clone());
        opts.config = AppConfig { autosave_delay_ms: 50, ..AppConfig::default() };
        opts.paths = vec![big.clone()];
        opts.workspace = Some("warm_big".into());
        let mut h = harness(opts);
        h.state_mut().app().state.tree.set_expanded(&big.join("sub_000"), true);
        settle(&mut h);
        assert_eq!(h.state_mut().app().state.tree.rows().len(), 1 + 3100 + 5);
        h.state_mut().app().on_quit();
    }
    let store = iron_sheets_core::workspace::WorkspaceStore::new(&dirs);
    assert!(store.tree_cache_file("warm_big").exists(), "tree-<slug>.bin persisted on quit");

    // Mutate folder externally while closed.
    std::fs::remove_file(big.join("note_0000.md")).expect("remove note_0000");
    std::fs::write(big.join("ExternalAdded.md"), "# External\n").expect("write external note");

    // Session 2: Frame 1 immediately renders cached rows with ZERO skeleton rows (even while worker is paused).
    let mut opts2 = AppOptions::new(dirs);
    opts2.config = AppConfig { autosave_delay_ms: 50, ..AppConfig::default() };
    opts2.workspace = Some("warm_big".into());
    let ctx = egui::Context::default();
    ctx.disable_accesskit();
    let t_new = Instant::now();
    let mut app = App::new(&ctx, opts2);
    let app_new_ms = t_new.elapsed().as_secs_f64() * 1000.0;
    app.state.tree.set_async_paused(true);

    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
    let t_f1 = Instant::now();
    let mut out = ctx.run_ui(input, |ui| app.frame(ui));
    out.textures_delta.clear();
    let frame1_ms = t_f1.elapsed().as_secs_f64() * 1000.0;

    {
        let rows = app.state.tree.rows();
        assert!(!rows.iter().any(|r| r.skeleton), "Frame 1 warm restart must emit ZERO skeleton rows");
        assert_eq!(rows.len(), 1 + 3100 + 5, "Frame 1 immediately renders all cached rows including nested sub_000");
        assert!(rows.iter().any(|r| r.entry.name == "item_0.md"));
        assert!(rows.iter().any(|r| r.entry.name == "note_0000.md"), "cached snapshot visible before revalidation runs");
    }

    // Resume background revalidation: external add/delete reconcile cleanly.
    app.state.tree.set_async_paused(false);
    let wait_start = Instant::now();
    while app.state.tree.has_pending_loads() {
        assert!(wait_start.elapsed() < Duration::from_secs(10), "tree revalidation timeout");
        std::thread::sleep(Duration::from_millis(1));
        let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| app.frame(ui));
        out.textures_delta.clear();
    }
    let rows = app.state.tree.rows();
    assert!(!rows.iter().any(|r| r.skeleton));
    assert!(rows.iter().any(|r| r.entry.name == "ExternalAdded.md"), "added file reconciled");
    assert!(!rows.iter().any(|r| r.entry.name == "note_0000.md"), "deleted file reconciled");
    eprintln!("WARM_RESTART 3500-file: App::new={app_new_ms:.2} ms, Frame1={frame1_ms:.2} ms");
    assert!(frame1_ms < 250.0, "Warm Frame 1 blocked UI thread: {frame1_ms:.2} ms");
}

#[test]
fn early_startup_kicks_off_active_tab_before_frame1() {
    let fx = Fx::new();
    let big_path = fx.notes().join("BigStart.md");
    let mut big_text = String::with_capacity(2 * 1024 * 1024);
    for i in 0..22_000 {
        big_text.push_str(&format!("# Heading {i}\n\nParagraph {i} for early startup active-tab kickoff.\n\n"));
    }
    std::fs::write(&big_path, &big_text).expect("write note");

    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        h.state_mut().app().state.commands.push(Command::OpenPath { path: big_path.clone(), new_tab: true, line: None });
        settle(&mut h);
        h.state_mut().app().on_quit();
    }

    let mut o = fx.opts(vec![]);
    o.workspace = Some("test".into());
    let ctx = egui::Context::default();
    ctx.disable_accesskit();
    let mut app = App::new(&ctx, o);
    assert_eq!(app.state.docs.loads, 0, "no synchronous doc read in App::new");

    // Wait for background prefetch kicked off inside App::new BEFORE running Frame 1.
    app.state.docs.prefetch.wait_idle(Duration::from_secs(5));

    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1100.0, 720.0))), predicted_dt: 1.0 / 60.0, ..Default::default() };
    let mut out = ctx.run_ui(input, |ui| app.frame(ui));
    out.textures_delta.clear();

    assert!(!app.state.is_active_tab_loading(), "active tab installed immediately on Frame 1 from early kickoff");
    assert_eq!(app.state.docs.get(&big_path).expect("doc installed on Frame 1").text.len(), big_text.len());
}
