//! Alone in its binary: parallel tests would slow the font scan past the compile's wait.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use iron_sheets_typst::{Engine, EngineConfig};

#[test]
fn system_fonts_arriving_do_not_recompile_the_first_document() {
    let notified = Arc::new(AtomicUsize::new(0));
    let n = notified.clone();
    let engine = Engine::new(EngineConfig {
        system_fonts: true,
        notify: Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
        }),
    });
    let t0 = Instant::now();
    engine.set_document(1, None, None, "= Hello\nworld".into(), 1);
    let snap = loop {
        if let Some(s) = engine.document(1) {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    let first = t0.elapsed();
    if first > Duration::from_millis(290) {
        eprintln!("font scan slower than the compile's 300 ms wait (loaded machine): fallback path, nothing to check");
        return;
    }
    // Long enough for the font scan to finish and (before the fix) trigger a recompile.
    std::thread::sleep(Duration::from_millis(1500));
    // Notifications also wake the UI when the warm-up and the font scan finish (status cue), so they are no measure of
    // compiles: a recompile replaces the snapshot.
    let notified = notified.load(Ordering::SeqCst);
    let later = engine.document(1).expect("snapshot");
    eprintln!("first snapshot after {first:?}, notifications: {notified}");
    assert!(Arc::ptr_eq(&snap, &later), "the document was compiled more than once");
}
