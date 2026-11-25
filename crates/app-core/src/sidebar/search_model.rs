//! Workspace search model: debounce, background job, grouped results, flattened rows, selection.
//! No UI toolkit here.
use crate::command::Command;
use crate::docs::DocStore;
use crate::platform::Waker;
use crate::state::AppState;
use iron_sheets_core::index::{IndexHandle, Indexer};
use iron_sheets_core::search::{OpenTexts, SearchHit, SearchJob, SearchQuery};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_HITS: usize = 5000;
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Hits of one file.
pub struct Group {
    pub path: PathBuf,
    pub name: String,
    pub dir: String,
    pub hits: Vec<SearchHit>,
}

/// One flattened result row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRow {
    /// File header of group `g`.
    File(usize),
    /// Hit `h` of group `g`.
    Hit(usize, usize),
}

/// A hit line with leading whitespace removed and the match range inside it.
pub struct HitText<'a> {
    pub text: &'a str,
    /// Byte range of the match within `text`; `None` when it does not fall on char boundaries.
    pub matched: Option<(usize, usize)>,
}

pub fn hit_text(hit: &SearchHit) -> HitText<'_> {
    let text = &hit.line_text;
    let lead = text.len() - text.trim_start().len();
    let s = (hit.match_range.start as usize).saturating_sub(lead);
    let e = (hit.match_range.end as usize).saturating_sub(lead);
    let t = text.trim_start();
    let (s, e) = (s.min(t.len()), e.min(t.len()));
    HitText { text: t, matched: (t.is_char_boundary(s) && t.is_char_boundary(e)).then_some((s, e)) }
}

#[derive(Default)]
pub struct SearchModel {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    job: Option<SearchJob>,
    pending_since: Option<Instant>,
    pending_after_scan: bool,
    pending_handle: Option<(IndexHandle, Arc<dyn Fn() + Send + Sync>)>,
    /// Unsaved texts taken when the search was deferred until the index scan ends.
    pending_open: OpenTexts,
    groups: Vec<Group>,
    index: FxHashMap<PathBuf, usize>,
    rows: Vec<SearchRow>,
    total: usize,
    finished: bool,
    sel: Option<usize>,
}

impl SearchModel {
    /// (hits, files) found so far.
    pub fn counts(&self) -> (usize, usize) {
        (self.total, self.groups.len())
    }

    /// True once the last started search has delivered everything.
    pub fn status_finished(&self) -> bool {
        self.finished && self.pending_since.is_none() && !self.pending_after_scan
    }

    /// A query change is waiting for its debounce.
    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    pub fn rows(&self) -> &[SearchRow] {
        &self.rows
    }

    pub fn group(&self, g: usize) -> &Group {
        &self.groups[g]
    }

    pub fn selected(&self) -> Option<usize> {
        self.sel
    }

    pub fn set_selected(&mut self, row: Option<usize>) {
        self.sel = row;
    }

    fn clear(&mut self) {
        self.job = None;
        self.groups.clear();
        self.index.clear();
        self.rows.clear();
        self.total = 0;
        self.finished = false;
        self.sel = None;
    }

    fn searchable_note_paths(ix: &iron_sheets_core::index::Index) -> Vec<PathBuf> {
        ix.notes()
            .filter(|n| n.kind != iron_sheets_core::FileKind::Excalidraw || iron_sheets_core::FileKind::is_excalidraw_md(&n.path))
            .map(|n| n.path.clone())
            .collect()
    }

    /// Starts a search now over the indexed notes of the workspace without blocking on the index lock.
    /// If the workspace is still scanning and has 0 indexed notes so far (or the write lock is held),
    /// defers until scanning completes / the lock becomes free.
    pub fn start<U>(&mut self, state: &AppState<U>) {
        self.start_parts(&state.waker, &state.indexer, &state.docs);
    }

    fn start_parts(&mut self, waker: &Arc<dyn Waker>, indexer: &Indexer, docs: &DocStore) {
        let w = waker.clone();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || w.wake());
        let ix_arc = indexer.index();
        let scanning = indexer.status().scanning;
        // shortcut: clones the text of every unsaved open note on the UI thread, fine for a few notes; upgrade to Arc<str> docs if that gets large.
        let open: OpenTexts = docs.iter().filter(|d| d.is_dirty()).map(|d| (d.path.clone(), d.text.clone())).collect();
        if let Some(ix) = ix_arc.try_read() {
            let paths = Self::searchable_note_paths(&ix);
            drop(ix);
            // A cold scan or a hydration still filling the index would give silently incomplete results.
            if (paths.is_empty() || indexer.partial()) && scanning {
                self.defer_after_scan(indexer.handle(), notify, open);
                return;
            }
            self.start_with(paths, open, notify);
        } else {
            self.defer_after_scan(indexer.handle(), notify, open);
        }
    }

    fn defer_after_scan(&mut self, handle: IndexHandle, notify: Arc<dyn Fn() + Send + Sync>, open: OpenTexts) {
        self.clear();
        self.pending_since = None;
        if self.query.is_empty() {
            self.pending_after_scan = false;
            self.pending_handle = None;
            return;
        }
        self.pending_after_scan = true;
        self.pending_handle = Some((handle, notify));
        self.pending_open = open;
    }

    /// Starts a search now over `paths`, using `open` (unsaved texts) instead of the files on disk; `notify` is called from the search thread when results arrive.
    pub fn start_with(&mut self, paths: Vec<PathBuf>, open: OpenTexts, notify: Arc<dyn Fn() + Send + Sync>) {
        self.clear();
        self.pending_since = None;
        self.pending_after_scan = false;
        self.pending_handle = None;
        if self.query.is_empty() {
            return;
        }
        let q = SearchQuery { text: self.query.clone(), case_sensitive: self.case_sensitive, whole_word: self.whole_word };
        self.job = Some(SearchJob::start(paths, open, q, MAX_HITS, notify));
    }

    /// The query or an option changed: cancel the running job at once, start the new one after the debounce.
    pub fn query_edited(&mut self, now: Instant) {
        self.job = None;
        self.pending_after_scan = false;
        self.pending_handle = None;
        if self.query.is_empty() {
            self.clear();
            self.pending_since = None;
        } else {
            self.pending_since = Some(now);
        }
    }

    /// Starts the search when `run_now` or the debounce elapsed. Returns the time to wait before the next call
    /// while a debounce is running.
    pub fn schedule<U>(&mut self, state: &AppState<U>, run_now: bool, now: Instant) -> Option<Duration> {
        self.schedule_parts(&state.waker, &state.indexer, &state.docs, run_now, now)
    }

    fn schedule_parts(&mut self, waker: &Arc<dyn Waker>, indexer: &Indexer, docs: &DocStore, run_now: bool, now: Instant) -> Option<Duration> {
        if run_now {
            self.start_parts(waker, indexer, docs);
        } else if let Some(t) = self.pending_since {
            let el = now.saturating_duration_since(t);
            if el >= DEBOUNCE {
                self.start_parts(waker, indexer, docs);
            } else {
                return Some(DEBOUNCE - el);
            }
        } else if self.pending_after_scan {
            // Keep the handle synced with the current workspace's indexer in case the workspace changed.
            let w = waker.clone();
            self.pending_handle = Some((indexer.handle(), Arc::new(move || w.wake())));
            if !indexer.status().scanning {
                self.start_parts(waker, indexer, docs);
                if self.pending_after_scan {
                    return Some(Duration::from_millis(16));
                }
            }
        }
        None
    }

    /// Takes new hits from the job and rebuilds the rows. `roots` are the workspace roots (for relative names).
    pub fn poll(&mut self, roots: &[PathBuf]) {
        if self.pending_after_scan && self.job.is_none() {
            if let Some((handle, notify)) = self.pending_handle.clone() {
                let scanning = handle.status().scanning;
                if let Some(ix) = handle.index().try_read() {
                    let paths = Self::searchable_note_paths(&ix);
                    drop(ix);
                    if (!paths.is_empty() && !handle.partial()) || !scanning {
                        let open = std::mem::take(&mut self.pending_open);
                        self.start_with(paths, open, notify);
                    }
                }
            }
        }
        let Some(job) = &self.job else { return };
        // `done` first: hits are pushed before the job is marked done, so nothing can slip in between.
        let done = job.finished();
        let new = job.poll();
        let changed = !new.is_empty();
        let first_rows = changed && self.rows.is_empty();
        let sel = self.sel.and_then(|i| self.rows.get(i).copied()).map(|r| match r {
            SearchRow::File(g) => (self.groups[g].path.clone(), None),
            SearchRow::Hit(g, h) => (self.groups[g].path.clone(), Some(h)),
        });
        for h in new {
            self.add_hit(h, roots);
        }
        self.finished |= done;
        if changed {
            self.rebuild();
            // Groups are inserted in final order, so rows can shift: keep the selection on its row.
            if let Some((path, h)) = sel {
                let g = self.index[&path];
                let want = h.map_or(SearchRow::File(g), |h| SearchRow::Hit(g, h));
                self.sel = self.rows.iter().position(|r| *r == want);
            }
            if first_rows && self.sel.is_none() {
                self.sel = self.rows.iter().position(|r| matches!(r, SearchRow::Hit(..)));
            }
        }
    }

    /// Groups are kept sorted by (dir, name) as they arrive, so rows never reorder while results stream in.
    fn add_hit(&mut self, h: SearchHit, roots: &[PathBuf]) {
        self.total += 1;
        let gi = match self.index.get(&h.path) {
            Some(&g) => g,
            None => {
                let rel = roots.iter().find_map(|r| h.path.strip_prefix(r).ok()).unwrap_or(&h.path);
                let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = rel.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                let at = self.groups.partition_point(|g| (&g.dir, &g.name) <= (&dir, &name));
                for v in self.index.values_mut().filter(|v| **v >= at) {
                    *v += 1;
                }
                self.groups.insert(at, Group { path: h.path.clone(), name, dir, hits: Vec::new() });
                self.index.insert(h.path.clone(), at);
                at
            }
        };
        self.groups[gi].hits.push(h);
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        for (g, grp) in self.groups.iter().enumerate() {
            self.rows.push(SearchRow::File(g));
            for h in 0..grp.hits.len() {
                self.rows.push(SearchRow::Hit(g, h));
            }
        }
    }

    /// Moves the selection to the next (`dir > 0`) or previous hit row, skipping file headers.
    /// Returns the new row index.
    pub fn step_sel(&mut self, dir: isize) -> Option<usize> {
        // Count the hit rows and find `sel`'s position among them in one pass.
        let (mut n, mut pos) = (0usize, None);
        for (i, r) in self.rows.iter().enumerate() {
            if matches!(r, SearchRow::Hit(..)) {
                if self.sel == Some(i) {
                    pos = Some(n);
                }
                n += 1;
            }
        }
        if n == 0 {
            return None;
        }
        let np = match (pos, dir) {
            (None, d) if d > 0 => 0,
            (None, _) => n - 1,
            (Some(p), d) if d > 0 => (p + 1).min(n - 1),
            (Some(p), _) => p.saturating_sub(1),
        };
        let row = self.rows.iter().enumerate().filter(|(_, r)| matches!(r, SearchRow::Hit(..))).nth(np).map(|(i, _)| i)?;
        self.sel = Some(row);
        Some(row)
    }

    /// The selected row, else the first hit row (what Enter opens).
    pub fn enter_row(&self) -> Option<usize> {
        self.sel.or_else(|| self.rows.iter().position(|r| matches!(r, SearchRow::Hit(..))))
    }

    /// The command that opens `row` (a file header opens the file, a hit opens it at its line).
    pub fn open_row(&self, row: usize, new_tab: bool) -> Option<Command> {
        let (path, line) = match self.rows.get(row)? {
            SearchRow::File(g) => (&self.groups[*g].path, None),
            SearchRow::Hit(g, h) => (&self.groups[*g].path, Some(self.groups[*g].hits[*h].line)),
        };
        Some(Command::OpenPath { path: path.clone(), new_tab, line })
    }

    /// Status line under the query field.
    pub fn status_text(&self) -> String {
        if self.query.is_empty() {
            "Type to search all notes".into()
        } else if self.pending_since.is_some() {
            "...".into()
        } else if self.pending_after_scan {
            "indexing workspace...".into()
        } else if self.total == 0 && self.finished {
            format!("No results for '{}'", self.query)
        } else {
            let mut s = format!("{} hits in {} files", self.total, self.groups.len());
            if self.total >= MAX_HITS {
                s.push_str(&format!(" (limit of {MAX_HITS} reached)"));
            }
            if !self.finished {
                s.push_str(", searching...");
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn vault() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(d.path().join("sub")).expect("mkdir");
        std::fs::write(d.path().join("b.md"), "alpha\nbeta alpha\n").expect("write");
        std::fs::write(d.path().join("sub").join("a.md"), "Alpha one\n").expect("write");
        std::fs::write(d.path().join("none.md"), "nothing\n").expect("write");
        d
    }

    fn run(m: &mut SearchModel, d: &tempfile::TempDir) {
        let paths = vec![d.path().join("b.md"), d.path().join("sub").join("a.md"), d.path().join("none.md")];
        m.start_with(paths, Default::default(), Arc::new(|| {}));
        let roots = vec![d.path().to_path_buf()];
        let t0 = Instant::now();
        while !m.finished && t0.elapsed() < Duration::from_secs(10) {
            m.poll(&roots);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.finished, "search finished");
    }

    #[test]
    fn groups_rows_and_open_commands() {
        let d = vault();
        let mut m = SearchModel { query: "alpha".into(), case_sensitive: true, ..Default::default() };
        run(&mut m, &d);
        assert_eq!(m.counts(), (2, 1), "case sensitive: only b.md matches lowercase");
        let mut m = SearchModel { query: "alpha".into(), ..Default::default() };
        run(&mut m, &d);
        assert_eq!(m.counts(), (3, 2));
        // Groups are sorted by (dir, name): root file first, then sub/a.md.
        assert_eq!(m.rows(), &[SearchRow::File(0), SearchRow::Hit(0, 0), SearchRow::Hit(0, 1), SearchRow::File(1), SearchRow::Hit(1, 0)]);
        assert_eq!(m.group(0).name, "b.md");
        assert_eq!(m.group(1).dir, "sub");
        assert_eq!(m.status_text(), "3 hits in 2 files");
        match m.open_row(2, true) {
            Some(Command::OpenPath { path, new_tab, line }) => {
                assert_eq!(path, d.path().join("b.md"));
                assert!(new_tab);
                assert_eq!(line, Some(1));
            }
            other => panic!("unexpected {other:?}"),
        }
        match m.open_row(3, false) {
            Some(Command::OpenPath { line, .. }) => assert_eq!(line, None),
            other => panic!("unexpected {other:?}"),
        }
        assert!(m.open_row(99, false).is_none());
    }

    #[test]
    fn selection_skips_headers() {
        let d = vault();
        let mut m = SearchModel { query: "alpha".into(), ..Default::default() };
        run(&mut m, &d);
        assert_eq!(m.enter_row(), Some(1));
        assert_eq!(m.selected(), Some(1), "first hit preselected");
        assert_eq!(m.step_sel(1), Some(2));
        assert_eq!(m.step_sel(1), Some(4));
        assert_eq!(m.step_sel(1), Some(4), "stays on the last hit");
        assert_eq!(m.step_sel(-1), Some(2));
        m.set_selected(Some(3));
        assert_eq!(m.step_sel(1), Some(1), "from a header: first hit");
        m.set_selected(Some(3));
        assert_eq!(m.step_sel(-1), Some(4), "from a header: last hit");
        assert_eq!(m.selected(), Some(4));
    }

    #[test]
    fn no_results_status() {
        let d = vault();
        let mut m = SearchModel { query: "zzz".into(), ..Default::default() };
        run(&mut m, &d);
        assert_eq!(m.status_text(), "No results for 'zzz'");
        assert_eq!(m.selected(), None);
    }

    #[test]
    fn query_edit_debounce_and_status() {
        let mut m = SearchModel::default();
        assert_eq!(m.status_text(), "Type to search all notes");
        m.query = "x".into();
        let t0 = Instant::now();
        m.query_edited(t0);
        assert!(m.is_pending());
        assert_eq!(m.status_text(), "...");
        assert!(!m.status_finished());
        m.query.clear();
        m.query_edited(t0);
        assert!(!m.is_pending());
        assert_eq!(m.counts(), (0, 0));
        assert_eq!(m.status_text(), "Type to search all notes");
    }

    #[test]
    fn hit_text_trims_and_checks_boundaries() {
        let mk = |t: &str, r: std::ops::Range<u32>| SearchHit { path: PathBuf::new(), line: 0, col: 0, line_text: t.into(), match_range: r };
        let h = mk("   foo bar", 7..10);
        let t = hit_text(&h);
        assert_eq!(t.text, "foo bar");
        assert_eq!(t.matched, Some((4, 7)));
        let h = mk("\u{e9}x", 1..2);
        assert_eq!(hit_text(&h).matched, None, "inside a multi-byte char");
    }

    #[test]
    fn start_is_non_blocking_and_waits_for_initial_scan() {
        let (d, mut state) = crate::test_support::state_with(&[("a.md", "alpha needle\n"), ("b.md", "needle two\n")]);
        let roots = state.workspace.roots.clone();

        // Write-locked index: start() does not block, defers, and poll() starts search once unlocked.
        let ix_arc = state.indexer.index();
        let write_guard = ix_arc.write();
        let mut m = SearchModel { query: "needle".into(), ..Default::default() };
        m.start(&state);
        assert_eq!(m.status_text(), "indexing workspace...");
        assert!(!m.status_finished());
        m.poll(&roots);
        assert_eq!(m.status_text(), "indexing workspace...");
        drop(write_guard);

        let t0 = Instant::now();
        while !m.status_finished() && t0.elapsed() < Duration::from_secs(10) {
            m.poll(&roots);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.status_finished());
        assert_eq!(m.counts(), (2, 2));

        // Scanning workspace with empty index defers until scan completes, then kicks off search in poll().
        let big_dir = d.path().join("big_ws");
        std::fs::create_dir_all(&big_dir).expect("mkdir");
        let chunk = format!("# Note\ntarget_token\n{}", "alpha [[other]] #tag\n".repeat(40_000));
        for i in 0..4 {
            std::fs::write(big_dir.join(format!("big{i}.md")), &chunk).expect("write");
        }
        state.indexer = iron_sheets_core::index::Indexer::spawn(vec![big_dir.clone()], None, Arc::new(|| {}));
        let mut m2 = SearchModel { query: "target_token".into(), ..Default::default() };
        m2.start(&state);
        if m2.pending_after_scan {
            assert_eq!(m2.status_text(), "indexing workspace...");
            assert!(!m2.status_finished());
        }
        let t1 = Instant::now();
        while !m2.status_finished() && t1.elapsed() < Duration::from_secs(15) {
            m2.poll(std::slice::from_ref(&big_dir));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m2.status_finished());
        assert_eq!(m2.counts(), (4, 4));
    }

    /// Streams 2000 files (given in reverse of their final order) and counts the polls in which rows that were
    /// already shown changed their relative order.
    #[test]
    fn rows_never_reorder_while_streaming() {
        let d = tempfile::tempdir().expect("tempdir");
        let mut paths = Vec::new();
        for i in 0..2000 {
            let sub = d.path().join(format!("d{}", i % 7));
            std::fs::create_dir_all(&sub).expect("mkdir");
            let p = sub.join(format!("n{i:04}.md"));
            std::fs::write(&p, format!("line\nneedle {i}\n")).expect("write");
            paths.push(p);
        }
        paths.reverse();
        let mut m = SearchModel { query: "needle".into(), ..Default::default() };
        let t0 = Instant::now();
        m.start_with(paths, Default::default(), Arc::new(|| {}));
        let roots = vec![d.path().to_path_buf()];
        let (mut first_hit, mut reorders) = (None, 0);
        let mut shown: Vec<PathBuf> = Vec::new();
        while !m.finished && t0.elapsed() < Duration::from_secs(20) {
            m.poll(&roots);
            if first_hit.is_none() && m.counts().0 > 0 {
                first_hit = Some(t0.elapsed());
            }
            let now: Vec<PathBuf> = (0..m.groups.len()).map(|g| m.group(g).path.clone()).collect();
            let kept: Vec<&PathBuf> = now.iter().filter(|p| shown.contains(p)).collect();
            if !kept.iter().copied().eq(shown.iter()) {
                reorders += 1;
            }
            shown = now;
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(m.finished);
        eprintln!("time to first hit {first_hit:?}, polls with reordered rows: {reorders}");
        assert_eq!(m.counts(), (2000, 2000));
        assert_eq!(reorders, 0);
        let keys: Vec<_> = (0..m.groups.len()).map(|g| (m.group(g).dir.clone(), m.group(g).name.clone())).collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "final order is sorted");
    }

    #[test]
    fn unsaved_edit_is_found_and_stale_disk_text_is_not() {
        let (_d, mut state) = crate::test_support::state_with(&[("a.md", "old text\nneedle on disk\n"), ("b.md", "needle b\n")]);
        let roots = state.workspace.roots.clone();
        let a = state.vault_path("a.md");
        state.docs.open(&a).expect("open");
        assert!(state.docs.get_mut(&a).expect("doc").apply_edit(0..0, "intro\nneedle fresh\n"));
        let mut m = SearchModel { query: "needle".into(), ..Default::default() };
        m.start(&state);
        let t0 = Instant::now();
        while !m.status_finished() && t0.elapsed() < Duration::from_secs(10) {
            m.poll(&roots);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(m.counts(), (3, 2), "two hits in the buffer of a.md, one in b.md");
        let lines: Vec<u32> = m.group(0).hits.iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![1, 3], "line numbers of the buffer, not of the file");
    }
}
