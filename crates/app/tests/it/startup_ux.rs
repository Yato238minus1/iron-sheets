//! IS-01 / IS-21: the first stable frame with restored tabs, and the "Indexing" line of the Files panel during a scan.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::time::Duration;

static CAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(3);

#[test]
fn first_frames_show_restored_tab_titles_without_reading_docs() {
    let fx = Fx::new();
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        for f in ["Welcome.md", "Ideas.md", "Reading.md"] {
            h.state_mut().app().state.commands.push(Command::OpenPath { path: fx.notes().join(f), new_tab: true, line: None });
            settle(&mut h);
        }
        h.state_mut().app().on_quit();
    }
    let mut o = fx.opts(vec![]);
    o.workspace = Some("test".into());
    // Stop the app after 3 frames (a step can hold several): the state before any background prefetch.
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).with_accessibility_check(false).build_ui_state(
        |ui, slot: &mut Slot| {
            if slot.app.is_none() {
                if let Some(o) = slot.opts.take() {
                    slot.app = Some(iron_sheets::App::new(ui.ctx(), o));
                }
            }
            if let Some(a) = slot.app.as_mut().filter(|a| a.frames < CAP.load(std::sync::atomic::Ordering::Relaxed)) {
                a.frame(ui);
            }
        },
        Slot { opts: Some(o), app: None },
    );
    for _ in 0..3 {
        h.step();
    }
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 3);
    let ms: Vec<_> = st.perf.milestones.iter().map(|(n, t)| format!("{n} {t:.1} ms")).collect();
    eprintln!("STARTUP milestones: {ms:?}");
    assert!(["tree seeded", "state built", "app created"].iter().all(|n| st.perf.milestones.iter().any(|(m, _)| m == n)));
    assert!(st.docs.loads <= 1 && st.docs.len() <= 1, "no doc reads beyond the active one");
    assert!(st.docs.prefetch.counters().0 <= 1);
    // Let the frames paint (frame 4 on) for the screenshot; the counts above were taken at frame 3.
    CAP.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
    h.step();
    for t in ["Welcome.md", "Ideas.md", "Reading.md"] {
        assert!(h.get_all_by_label(t).count() >= 1, "tab title {t} visible");
    }
    shot(&mut h, "startup-first-frames-3-tabs");
}

#[test]
fn files_panel_says_indexing_while_the_index_scans() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    // `harness` settles the tree, not the indexer: wait for the startup scan before checking the idle state.
    let t = std::time::Instant::now();
    while h.state_mut().app().state.indexer.status().scanning && t.elapsed() < Duration::from_secs(10) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.query_by_label_contains("Indexing").is_none(), "no line when idle");
    let idx = h.state_mut().app().state.indexer.index();
    let guard = idx.write();
    h.state_mut().app().state.indexer.rescan();
    for _ in 0..3 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(h.state_mut().app().state.indexer.status().scanning, "the rescan waits for the lock we hold");
    h.get_by_label_contains("Indexing");
    shot(&mut h, "files-indexing-line");
    drop(guard);
    let t = std::time::Instant::now();
    while h.state_mut().app().state.indexer.status().scanning && t.elapsed() < Duration::from_secs(10) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(&mut h);
    assert!(h.query_by_label_contains("Indexing").is_none(), "line gone when the scan is done");
}
