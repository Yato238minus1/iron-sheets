//! Helpers for unit tests that need an `AppState` over a temporary vault (no UI toolkit involved).
use crate::platform::Waker;
use crate::state::{AppOptions, AppState};
use iron_sheets_core::config::ViewMode;
use iron_sheets_core::paths::AppDirs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct NoopWaker;

impl Waker for NoopWaker {
    fn wake(&self) {}
}

/// A state whose single workspace root holds `files` (relative path, text); the index is settled.
pub fn state_with(files: &[(&str, &str)]) -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vault");
    for (rel, text) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, text).expect("write note");
    }
    std::fs::create_dir_all(&root).expect("mkdir root");
    let mut opts = AppOptions::new(AppDirs::at(&dir.path().join("home")));
    opts.paths = vec![root];
    opts.workspace = Some("test".into());
    let mut state = AppState::new(Arc::new(NoopWaker), opts, ());
    // Tests start without the empty tab a real start opens.
    state.tabs.tabs.clear();
    state.tabs.active = None;
    assert!(state.indexer.wait_idle(Duration::from_secs(20)), "index settled");
    (dir, state)
}

impl<U> crate::state::AppState<U> {
    /// Path of a note of the vault created by `state_with`.
    pub fn vault_path(&self, rel: &str) -> PathBuf {
        self.workspace.roots[0].join(rel)
    }
}

/// Opens vault note `rel` as the active tab (no document is loaded).
pub fn activate_tab(state: &mut AppState, rel: &str) {
    let p = state.vault_path(rel);
    let i = state.tabs.push_tab(p, ViewMode::Source, true);
    state.tabs.active = Some(i);
}

/// `state_with` over `n` tiny notes spread over 500 folders (ignored perf tests).
pub fn state_with_many(n: usize) -> (tempfile::TempDir, AppState) {
    let files: Vec<(String, String)> = (0..n).map(|i| (format!("f{:03}/Note Ünï {i:05}.md", i % 500), format!("# N{i}\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    state_with(&refs)
}
