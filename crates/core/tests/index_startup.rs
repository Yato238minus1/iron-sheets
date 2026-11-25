//! Index startup timings on a generated 20k-note / 2k-dir tree (ignored by default):
//! `cargo test -p iron-sheets-core --release --test index_startup -- --ignored --nocapture`
//! Set `IS_BENCH_ROOT=<dir>` to measure an existing tree instead of the generated one.
//! "max lock hold" is the longest stretch in which a polling reader (spinning `try_read`) could not get the
//! read lock, i.e. a lower bound of the longest write-lock hold.
use iron_sheets_core::index::{IndexHandle, Indexer};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn make_tree(root: &Path, dirs: usize, notes: usize) {
    for d in 0..dirs {
        let dir = root.join(format!("a{:02}", d / 100)).join(format!("b{:02}", (d / 10) % 10)).join(format!("c{}", d % 10));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for n in 0..notes / dirs {
            let i = d * (notes / dirs) + n;
            std::fs::write(dir.join(format!("note-{i:05}.md")), format!("# Note {i}\n\nSee [[note-{:05}]] #tag{}\n\n## Part\n", (i + 1) % notes, i % 7)).expect("write");
        }
    }
}

/// Spins on `try_read`; returns (time the index first held a note, longest stretch without read access).
fn poll_reader(h: IndexHandle, stop: Arc<AtomicBool>, t0: Instant) -> std::thread::JoinHandle<(Option<Duration>, Duration)> {
    std::thread::spawn(move || {
        let (mut queryable, mut max_hold, mut blocked_since) = (None, Duration::ZERO, None::<Instant>);
        while !stop.load(Ordering::Relaxed) {
            match h.index().try_read() {
                Some(g) => {
                    blocked_since = None;
                    if queryable.is_none() && !g.is_empty() {
                        queryable = Some(t0.elapsed());
                    }
                }
                None => {
                    let b = *blocked_since.get_or_insert_with(Instant::now);
                    max_hold = max_hold.max(b.elapsed());
                }
            }
            std::hint::spin_loop();
        }
        (queryable, max_hold)
    })
}

/// The measuring method itself: a write lock held for 20 ms must show up as roughly that.
#[test]
fn reader_sees_a_held_write_lock() {
    let t = tempfile::tempdir().expect("tempdir");
    let ix = Indexer::spawn(vec![t.path().to_path_buf()], None, Arc::new(|| {}));
    assert!(ix.wait_idle(Duration::from_secs(10)));
    let stop = Arc::new(AtomicBool::new(false));
    let reader = poll_reader(ix.handle(), stop.clone(), Instant::now());
    std::thread::sleep(Duration::from_millis(10));
    {
        let a = ix.index();
        let _g = a.write();
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(10));
    stop.store(true, Ordering::Relaxed);
    let (_, hold) = reader.join().expect("reader");
    assert!(hold >= Duration::from_millis(15), "saw only {hold:?}");
}

/// A cold scan publishes notes in sections: quick-open can see some of them while the scan still runs.
#[test]
fn cold_scan_is_queryable_while_it_still_runs() {
    let t = tempfile::tempdir().expect("tempdir");
    make_tree(t.path(), 60, 6000);
    let ix = Indexer::spawn(vec![t.path().to_path_buf()], None, Arc::new(|| {}));
    let h = ix.handle();
    let t0 = Instant::now();
    let seen = loop {
        if let Some(g) = h.index().try_read()
            && !g.is_empty()
        {
            break (g.len(), h.status().scanning && h.partial());
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "index never became queryable");
        std::thread::sleep(Duration::from_micros(100));
    };
    assert!(seen.1 && seen.0 < 6000, "first visible state: {} of 6000 notes, scanning and partial={}", seen.0, seen.1);
    assert!(ix.wait_idle(Duration::from_secs(60)));
    assert_eq!(ix.index().read().len(), 6000);
    assert!(!ix.partial(), "complete: search may run");
}

struct Run {
    queryable_ms: f64,
    done_ms: f64,
    max_hold_us: f64,
}

fn run(root: &Path, cache: &Path) -> Run {
    let t0 = Instant::now();
    let ix = Indexer::spawn(vec![root.to_path_buf()], Some(cache.to_path_buf()), Arc::new(|| {}));
    let stop = Arc::new(AtomicBool::new(false));
    let reader = poll_reader(ix.handle(), stop.clone(), t0);
    while ix.status().scanning {
        std::thread::sleep(Duration::from_micros(200));
    }
    let done = t0.elapsed();
    assert!(ix.wait_idle(Duration::from_secs(60)), "index cache written");
    stop.store(true, Ordering::Relaxed);
    let (q, hold) = reader.join().expect("reader");
    Run { queryable_ms: q.unwrap_or(done).as_secs_f64() * 1e3, done_ms: done.as_secs_f64() * 1e3, max_hold_us: hold.as_secs_f64() * 1e6 }
}

#[test]
#[ignore = "perf measurement"]
fn cold_and_warm_index_20k() {
    let t = tempfile::tempdir().expect("tempdir");
    // IS_BENCH_ROOT=<dir> measures an existing tree (e.g. a real vault) instead of the generated one.
    let root = match std::env::var_os("IS_BENCH_ROOT") {
        Some(r) => std::path::PathBuf::from(r),
        None => {
            let root = t.path().join("vault");
            make_tree(&root, 2000, 20_000);
            root
        }
    };
    let cache = t.path().join("idx.bin");
    for (label, n) in [("cold", 1), ("warm", 3)] {
        let runs: Vec<Run> = (0..n).map(|_| run(&root, &cache)).collect();
        for r in &runs {
            println!("PERF index {label}: queryable {:.1} ms, done {:.1} ms, max lock hold {:.0} us", r.queryable_ms, r.done_ms, r.max_hold_us);
        }
    }
}
