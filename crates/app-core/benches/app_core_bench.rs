use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use iron_sheets_app_core::palette_model::{PaletteMode, PaletteModel};
use iron_sheets_app_core::platform::Waker;
use iron_sheets_app_core::state::{AppOptions, AppState};
use iron_sheets_core::config::ViewMode;
use iron_sheets_core::paths::AppDirs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

struct NoopWaker;

impl Waker for NoopWaker {
    fn wake(&self) {}
}

fn setup_vault(n_notes: usize, dirs_per_root: usize, subdirs_per_dir: usize) -> (TempDir, AppState, Vec<PathBuf>, Vec<PathBuf>) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(&root).unwrap();

    let mut all_dirs = vec![root.clone()];
    for a in 0..dirs_per_root {
        let d = root.join(format!("folder{a:02}"));
        std::fs::create_dir_all(&d).unwrap();
        all_dirs.push(d.clone());
        for b in 0..subdirs_per_dir {
            let sub = d.join(format!("sub{b:02}"));
            std::fs::create_dir_all(&sub).unwrap();
            all_dirs.push(sub);
        }
    }

    let mut note_paths = Vec::with_capacity(n_notes);
    for i in 0..n_notes {
        let target_dir = &all_dirs[1 + (i % (all_dirs.len() - 1))];
        let p = target_dir.join(format!("note-{i:04}-project.md"));
        std::fs::write(&p, format!("# Note {i}\n\nBody text with #tag{} and [[note-{:04}-project]].\n", i % 10, (i + 1) % n_notes)).unwrap();
        note_paths.push(p);
    }

    let mut opts = AppOptions::new(AppDirs::at(&dir.path().join("home")));
    opts.paths = vec![root];
    opts.workspace = Some("bench".into());
    opts.config.restore_session = false;
    let mut state = AppState::new(Arc::new(NoopWaker), opts, ());
    assert!(state.indexer.wait_idle(Duration::from_secs(30)));
    state.tabs.tabs.clear();
    state.tabs.active = None;
    (dir, state, all_dirs, note_paths)
}

fn bench_frame_state(c: &mut Criterion) {
    let (_dir, mut state, all_dirs, note_paths) = setup_vault(500, 25, 20);
    for d in &all_dirs {
        state.tree.set_expanded(d, true);
    }
    for p in note_paths.iter().take(20) {
        let _ = state.docs.open(p);
        state.tabs.push_tab(p.clone(), ViewMode::Split, true);
    }
    state.tabs.active = Some(0);
    let expanded_count = state.tree.expanded().len() as u64;

    let mut group = c.benchmark_group("app_core/frame");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(expanded_count));
    group.bench_function("compute_session_sig_526_dirs_20_tabs", |b| b.iter(|| black_box(state.compute_session_sig())));
    group.bench_function("tick_frame_bookkeeping_526_dirs_20_docs", |b| {
        b.iter(|| {
            let mut dirty_count = 0usize;
            for d in state.docs.iter() {
                if d.is_dirty() {
                    dirty_count += 1;
                }
            }
            if let Some(p) = state.active_path()
                && let Some(doc) = state.docs.get(&p)
            {
                state.stats.update(&p, doc, Some(0));
            }
            let sig = state.compute_session_sig();
            black_box((dirty_count, sig))
        })
    });
    group.finish();
}

fn bench_palette_model(c: &mut Criterion) {
    let (_dir, mut state, _, note_paths) = setup_vault(2000, 20, 4);
    for p in note_paths.iter().take(10) {
        state.tabs.push_tab(p.clone(), ViewMode::Split, true);
    }
    state.tabs.active = Some(0);

    let mut pal_hit = PaletteModel::default();
    pal_hit.begin(PaletteMode::Files);
    pal_hit.query = "note".into();
    pal_hit.refresh(&state);

    let mut pal_hit_empty = PaletteModel::default();
    pal_hit_empty.begin(PaletteMode::Files);
    pal_hit_empty.refresh(&state);

    let mut pal_miss = PaletteModel::default();
    pal_miss.begin(PaletteMode::Files);
    pal_miss.query = "note1".into();
    pal_miss.refresh(&state);
    let mut flip = false;

    let mut group = c.benchmark_group("app_core/palette");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Elements(2000));
    group.bench_function("refresh_cache_hit_query", |b| {
        b.iter(|| {
            pal_hit.refresh(black_box(&state));
            black_box(pal_hit.rows().len())
        })
    });
    group.bench_function("refresh_cache_hit_empty", |b| {
        b.iter(|| {
            pal_hit_empty.refresh(black_box(&state));
            black_box(pal_hit_empty.rows().len())
        })
    });
    group.bench_function("refresh_cache_miss_toggle", |b| {
        b.iter(|| {
            flip = !flip;
            pal_miss.query = if flip { "note2".into() } else { "note1".into() };
            pal_miss.query_changed();
            pal_miss.refresh(black_box(&state));
            black_box(pal_miss.rows().len())
        })
    });
    group.bench_function("refresh_cache_miss_narrow", |b| {
        b.iter_batched(
            || {
                let mut m = PaletteModel::default();
                m.begin(PaletteMode::Files);
                m.query = "f0".into();
                m.refresh(&state);
                m
            },
            |mut m| {
                m.query = "f01n".into();
                m.query_changed();
                m.refresh(black_box(&state));
                black_box(m.rows().len())
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_frame_state, bench_palette_model);
criterion_main!(benches);
