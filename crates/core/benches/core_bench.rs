use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use iron_sheets_core::fuzzy::fuzzy_rank;
use iron_sheets_core::index::Indexer;
use iron_sheets_core::search::{SearchQuery, search_files};
use iron_sheets_core::tree::TreeModel;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tempfile::TempDir;

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

const UTF8_WORDS: &[&str] =
    &["Äpfel", "über", "naïve", "résumé", "café", "Schloß", "markdown", "typst", "Notizbuch", "Übersicht", "Größe", "Entwurf", "Verknüpfung", "Satzbau"];

fn gen_notes(dir: &Path, n: usize, bytes_per_note: usize) {
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
        let mut s = format!("---\ntitle: Note number {i}\ntags: [bench, t{}]\naliases: [alias-{i}]\n---\n# Note {i}\n\n", i % 17);
        let mut sec = 0;
        while s.len() < bytes_per_note {
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

fn bench_tree_expanded(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let mut dirs = vec![root.clone()];
    for a in 0..25 {
        let parent = root.join(format!("area-{a:02}"));
        std::fs::create_dir_all(&parent).unwrap();
        dirs.push(parent.clone());
        for b in 0..30 {
            let sub = parent.join(format!("topic-{b:02}"));
            std::fs::create_dir_all(&sub).unwrap();
            dirs.push(sub);
        }
    }
    let mut tree = TreeModel::new(vec![root], false);
    for d in &dirs {
        tree.set_expanded(d, true);
    }
    let _ = tree.rows();

    let mut group = c.benchmark_group("core/tree");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(dirs.len() as u64));
    group.bench_function("expanded_776_dirs", |b| b.iter(|| black_box(tree.expanded())));
    group.finish();
}

fn bench_fuzzy_rank(c: &mut Criterion) {
    let items: Vec<String> = (0..2000).map(|i| format!("folder{:02}/sub{:02}/note{i}-design-notes.md", i % 40, (i / 40) % 5)).collect();

    let mut group = c.benchmark_group("core/fuzzy");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(items.len() as u64));
    group.bench_function("fuzzy_rank_2000_sparse", |b| b.iter(|| black_box(fuzzy_rank(black_box("f12n19"), items.iter().map(|s| (s, s.as_str())), 50))));
    group.bench_function("fuzzy_rank_2000_broad", |b| b.iter(|| black_box(fuzzy_rank(black_box("note"), items.iter().map(|s| (s, s.as_str())), 50))));
    group.finish();
}

fn bench_index_and_resolve(c: &mut Criterion) {
    let tmp_small = TempDir::new().unwrap();
    let root_small = tmp_small.path().join("ws500");
    gen_notes(&root_small, 500, 2000);
    let cache_path = tmp_small.path().join("cache/idx.bin");
    let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

    // Pre-populate disk cache for warm build benchmark.
    {
        let ix = Indexer::spawn(vec![root_small.clone()], Some(cache_path.clone()), notify.clone());
        assert!(ix.wait_idle(Duration::from_secs(30)));
    }

    let mut build_group = c.benchmark_group("core/index_build");
    build_group.sample_size(15);
    build_group.warm_up_time(Duration::from_millis(500));
    build_group.measurement_time(Duration::from_secs(2));
    build_group.throughput(Throughput::Elements(500));
    build_group.bench_function("cold_500_notes", |b| {
        b.iter_batched(
            || {},
            |()| {
                let ix = Indexer::spawn(vec![root_small.clone()], None, notify.clone());
                assert!(ix.wait_idle(Duration::from_secs(30)));
                let len = ix.index().read().len();
                black_box(len)
            },
            BatchSize::PerIteration,
        )
    });
    build_group.bench_function("warm_cache_500_notes", |b| {
        b.iter_batched(
            || {},
            |()| {
                let ix = Indexer::spawn(vec![root_small.clone()], Some(cache_path.clone()), notify.clone());
                assert!(ix.wait_idle(Duration::from_secs(30)));
                let len = ix.index().read().len();
                black_box(len)
            },
            BatchSize::PerIteration,
        )
    });
    build_group.finish();

    let tmp_big = TempDir::new().unwrap();
    let root_big = tmp_big.path().join("ws2000");
    gen_notes(&root_big, 2000, 2000);
    let ix = Indexer::spawn(vec![root_big.clone()], None, notify);
    assert!(ix.wait_idle(Duration::from_secs(30)));
    let idx_arc = ix.index();
    let guard = idx_arc.read();
    let from = root_big.join("folder03/note3.md");

    let mut targets = Vec::with_capacity(500);
    for i in 0..300 {
        targets.push(format!("note{}", (i * 7) % 2000));
    }
    for i in 0..100 {
        let id = (i * 13) % 2000;
        targets.push(format!("folder{:02}/note{id}", id % 40));
    }
    for i in 0..50 {
        let id = (i * 17) % 2000;
        targets.push(format!("../folder{:02}/note{id}", id % 40));
    }
    for i in 0..50 {
        targets.push(format!("missing-note-{i}"));
    }

    let mut res_group = c.benchmark_group("core/index_resolve");
    res_group.warm_up_time(Duration::from_millis(500));
    res_group.measurement_time(Duration::from_secs(2));
    res_group.bench_function("resolve_single_stem", |b| b.iter(|| black_box(guard.resolve(black_box("note42"), Some(&from)))));
    res_group.throughput(Throughput::Elements(targets.len() as u64));
    res_group.bench_function("resolve_many_500", |b| {
        b.iter(|| {
            let mut found = 0usize;
            for t in &targets {
                if guard.resolve(black_box(t), Some(&from)).is_some() {
                    found += 1;
                }
            }
            black_box(found)
        })
    });
    res_group.finish();
}

fn bench_search(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    let ascii_dir = tmp.path().join("ascii");
    let utf8_dir = tmp.path().join("utf8");
    std::fs::create_dir_all(&ascii_dir).unwrap();
    std::fs::create_dir_all(&utf8_dir).unwrap();

    let mut seed = 98765u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    let mut ascii_paths = Vec::with_capacity(150);
    let mut ascii_bytes = 0u64;
    let mut utf8_paths = Vec::with_capacity(150);
    let mut utf8_bytes = 0u64;

    for i in 0..150 {
        let mut a = format!("# Note {i}\n\n");
        while a.len() < 4096 {
            for _ in 0..12 {
                a.push_str(WORDS[(rnd() % WORDS.len() as u64) as usize]);
                a.push(' ');
            }
            a.push('\n');
        }
        ascii_bytes += a.len() as u64;
        let p = ascii_dir.join(format!("ascii-{i:03}.md"));
        std::fs::write(&p, a).unwrap();
        ascii_paths.push(p);

        let mut u = format!("# Notiz {i} über Äpfel\n\n");
        while u.len() < 4096 {
            for _ in 0..10 {
                u.push_str(UTF8_WORDS[(rnd() % UTF8_WORDS.len() as u64) as usize]);
                u.push(' ');
            }
            u.push('\n');
        }
        utf8_bytes += u.len() as u64;
        let p = utf8_dir.join(format!("utf8-{i:03}.md"));
        std::fs::write(&p, u).unwrap();
        utf8_paths.push(p);
    }

    let cancel = AtomicBool::new(false);
    let q_ascii_ci = SearchQuery { text: "markdown".into(), case_sensitive: false, whole_word: false };
    let q_ascii_cs = SearchQuery { text: "markdown".into(), case_sensitive: true, whole_word: false };
    let q_utf8_ci = SearchQuery { text: "äpfel".into(), case_sensitive: false, whole_word: false };

    // Warm page cache once.
    let _ = search_files(&ascii_paths, &q_ascii_ci, &cancel, 5000);
    let _ = search_files(&utf8_paths, &q_utf8_ci, &cancel, 5000);

    let mut group = c.benchmark_group("core/search");
    group.sample_size(25);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Bytes(ascii_bytes));
    group.bench_function("search_ascii_ci_150_files", |b| b.iter(|| black_box(search_files(black_box(&ascii_paths), black_box(&q_ascii_ci), &cancel, 5000))));
    group.bench_function("search_ascii_cs_150_files", |b| b.iter(|| black_box(search_files(black_box(&ascii_paths), black_box(&q_ascii_cs), &cancel, 5000))));
    group.throughput(Throughput::Bytes(utf8_bytes));
    group.bench_function("search_utf8_ci_150_files", |b| b.iter(|| black_box(search_files(black_box(&utf8_paths), black_box(&q_utf8_ci), &cancel, 5000))));
    group.finish();
}

criterion_group!(benches, bench_tree_expanded, bench_fuzzy_rank, bench_index_and_resolve, bench_search);
criterion_main!(benches);
