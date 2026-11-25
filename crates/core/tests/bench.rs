//! Run with: cargo test -p iron-sheets-core --release --test bench -- --ignored --nocapture
use iron_sheets_core::fuzzy::fuzzy_rank;
use iron_sheets_core::index::Indexer;
use iron_sheets_core::search::{SearchQuery, search_files};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const WORDS: &[&str] = &[
    "the",
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "markdown",
    "typst",
    "editor",
    "performance",
    "index",
    "note",
    "workspace",
    "folder",
    "system",
    "memory",
    "thread",
    "render",
    "preview",
    "scroll",
    "syntax",
    "window",
    "native",
    "latency",
];

fn gen_notes(dir: &std::path::Path, n: usize) {
    let mut seed = 12345u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for d in 0..40 {
        std::fs::create_dir_all(dir.join(format!("folder{d:02}"))).unwrap();
    }
    for i in 0..n {
        let mut s = format!("---\ntitle: Note number {i}\ntags: [bench, t{}]\n---\n# Note {i}\n\n", i % 17);
        let mut sec = 0;
        while s.len() < 4000 {
            if s.len() % 7 == 0 {
                sec += 1;
                s.push_str(&format!("## Section {sec}\n"));
            }
            for _ in 0..12 {
                s.push_str(WORDS[(rnd() % WORDS.len() as u64) as usize]);
                s.push(' ');
            }
            if rnd() % 3 == 0 {
                s.push_str(&format!("[[note{}]] ", rnd() % n as u64));
            }
            if rnd() % 5 == 0 {
                s.push_str(&format!("[l](../folder{:02}/note{}.md) #tag{} ", rnd() % 40, rnd() % n as u64, rnd() % 30));
            }
            s.push('\n');
        }
        std::fs::write(dir.join(format!("folder{:02}/note{i}.md", i % 40)), s).unwrap();
    }
}

#[test]
#[ignore]
fn bench_2000_notes() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("ws");
    gen_notes(&root, 2000);
    let cache = t.path().join("cache/idx.bin");

    let t0 = Instant::now();
    let ix = Indexer::spawn(vec![root.clone()], Some(cache.clone()), Arc::new(|| {}));
    assert!(ix.wait_idle(Duration::from_secs(60)));
    let cold = t0.elapsed();
    println!("cold scan:  {:.1} ms (reads {}, internal {:.1} ms)", cold.as_secs_f64() * 1e3, ix.read_count(), ix.status().last_scan_ms);
    drop(ix);

    let t0 = Instant::now();
    let ix = Indexer::spawn(vec![root.clone()], Some(cache.clone()), Arc::new(|| {}));
    assert!(ix.wait_idle(Duration::from_secs(60)));
    let warm = t0.elapsed();
    println!("warm scan:  {:.1} ms (reads {}, internal {:.1} ms)", warm.as_secs_f64() * 1e3, ix.read_count(), ix.status().last_scan_ms);

    let t0 = Instant::now();
    ix.rescan();
    assert!(ix.wait_idle(Duration::from_secs(60)));
    println!("rescan (in-memory reuse): {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);

    let binding = ix.index();
    let g = binding.read();
    println!("notes {} files {}", g.len(), g.file_count());
    let targets: Vec<String> = (0..2000).map(|i| format!("note{i}")).collect();
    let from = root.join("folder03/note3.md");
    let iters = 20;
    let t0 = Instant::now();
    let mut found = 0usize;
    for _ in 0..iters {
        for t in &targets {
            if g.resolve(t, Some(&from)).is_some() {
                found += 1;
            }
        }
    }
    let ns = t0.elapsed().as_nanos() as f64 / (iters * targets.len()) as f64;
    println!("resolve:    {ns:.0} ns/op (found {found})");
    let t0 = Instant::now();
    let id = g.resolve("note1", None).unwrap();
    let bl = g.backlinks(id).len();
    println!("backlinks (first call builds map): {:.2} ms ({bl} for note1)", t0.elapsed().as_secs_f64() * 1e3);

    let paths: Vec<PathBuf> = g.notes().map(|n| n.path.clone()).collect();
    let q = SearchQuery { text: "markdown".into(), case_sensitive: false, whole_word: false };
    let no = AtomicBool::new(false);
    let _ = search_files(&paths, &q, &no, 1000); // warm the page cache
    let t0 = Instant::now();
    let hits = search_files(&paths, &q, &no, 1_000_000);
    println!("search:     {:.1} ms ({} hits)", t0.elapsed().as_secs_f64() * 1e3, hits.len());
    let t0 = Instant::now();
    let hits = search_files(&paths, &q, &no, 200);
    println!("search (max 200 hits): {:.1} ms ({} hits)", t0.elapsed().as_secs_f64() * 1e3, hits.len());

    let rels: Vec<String> = g.files().map(|f| f.rel.to_string()).collect();
    let t0 = Instant::now();
    let mut n = 0;
    for _ in 0..20 {
        n += fuzzy_rank("f12n19", rels.iter().map(|s| (s, s.as_str())), 50).len();
    }
    println!("fuzzy_rank: {:.2} ms per 2000 paths ({n} total results)", t0.elapsed().as_secs_f64() * 1e3 / 20.0);
    let t0 = Instant::now();
    for _ in 0..20 {
        n = fuzzy_rank("note", rels.iter().map(|s| (s, s.as_str())), 50).len();
    }
    println!("fuzzy_rank (all match, {n} results): {:.2} ms", t0.elapsed().as_secs_f64() * 1e3 / 20.0);
}
