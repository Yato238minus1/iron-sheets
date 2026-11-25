//! The default junk-folder excludes reach every consumer: tree listing, indexer walk and file watcher.
use iron_sheets_core::config::DEFAULT_EXCLUDES;
use iron_sheets_core::glob::ExcludeSet;
use iron_sheets_core::index::Indexer;
use iron_sheets_core::tree::{TreeSort, read_dir_sorted_filtered};
use iron_sheets_core::watch::{FsEvent, Watcher};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

fn ws() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("ws");
    for d in ["a/b/node_modules/pkg", "a/__pycache__", "node_modules/x", "a/b/keep"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in ["a/b/node_modules/pkg/README.md", "node_modules/x/r.md", "a/__pycache__/c.md", "a/b/keep/n.md", "top.md"] {
        std::fs::write(root.join(f), "# t\n").unwrap();
    }
    (t, root)
}

#[test]
fn tree_hides_default_junk_at_any_depth() {
    let (_t, root) = ws();
    let ex = ExcludeSet::new(&DEFAULT_EXCLUDES);
    let names = |rel: &str| -> Vec<String> {
        let dir = root.join(rel);
        read_dir_sorted_filtered(&dir, false, true, &ex, rel, TreeSort::default()).unwrap().into_iter().map(|e| e.name).collect()
    };
    assert_eq!(names(""), vec!["a", "top.md"]);
    assert_eq!(names("a"), vec!["b"]);
    assert_eq!(names("a/b"), vec!["keep"]);
}

#[test]
fn indexer_skips_default_junk() {
    let (_t, root) = ws();
    let ix = Indexer::spawn_with(vec![root.clone()], None, Arc::new(|| {}), ExcludeSet::new(&DEFAULT_EXCLUDES));
    assert!(ix.wait_idle(Duration::from_secs(10)));
    let idx = ix.index();
    let g = idx.read();
    assert!(g.by_path(&root.join("top.md")).is_some() && g.by_path(&root.join("a/b/keep/n.md")).is_some());
    for junk in ["a/b/node_modules/pkg/README.md", "node_modules/x/r.md", "a/__pycache__/c.md"] {
        assert!(g.by_path(&root.join(junk)).is_none(), "{junk} was indexed");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn watcher_ignores_default_junk() {
    let (_t, root) = ws();
    let (tx, rx) = mpsc::channel();
    let tx = parking_lot::Mutex::new(tx);
    let Some(w) = Watcher::new_with_excludes(std::slice::from_ref(&root), ExcludeSet::new(&DEFAULT_EXCLUDES), move |e| {
        let _ = tx.lock().send(e);
    }) else {
        return;
    };
    let end = Instant::now() + Duration::from_secs(10);
    while !w.registered() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(w.registered());
    std::fs::write(root.join("a/b/node_modules/pkg/new.md"), "x").unwrap();
    std::fs::write(root.join("a/b/keep/new.md"), "x").unwrap();
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && !seen.iter().any(|p: &std::path::PathBuf| p.ends_with("keep/new.md")) {
        if let Ok(v) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.extend(v.into_iter().filter_map(|e| if let FsEvent::Changed(p) = e { Some(p) } else { None }));
        }
    }
    assert!(seen.iter().any(|p| p.ends_with("keep/new.md")), "no event for a normal folder");
    assert!(!seen.iter().any(|p| p.components().any(|c| c.as_os_str() == "node_modules")), "event from node_modules: {seen:?}");
}
