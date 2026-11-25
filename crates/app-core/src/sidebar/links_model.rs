//! Links model: backlinks (computed on a background thread), outgoing links and tags of the active note.
//! No UI toolkit here.
use crate::command::Command;
use crate::platform::Waker;
use crate::state::AppState;
use crossbeam_channel::{Receiver, bounded};
use iron_sheets_core::index::{Index, Indexer};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// (index generation, note id) a backlink computation was made for.
type BackKey = (u64, u32);
/// (path, stem, 0-based line) per backlink.
type BackRows = Vec<(PathBuf, String, u32)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRow {
    Header(String),
    Note(String),
    Back { path: PathBuf, text: String, line: u32 },
    Out { target: String, text: String, resolved: bool },
    Tag(String),
}

impl LinkRow {
    /// The command a click on this row runs; `from` is the active note, `new_tab` the modifier state.
    pub fn command(&self, from: Option<&Path>, new_tab: bool) -> Option<Command> {
        match self {
            LinkRow::Header(_) | LinkRow::Note(_) => None,
            LinkRow::Back { path, line, .. } => Some(Command::OpenPath { path: path.clone(), new_tab, line: Some(*line) }),
            LinkRow::Out { target, .. } => Some(Command::OpenLink { note: target.clone(), heading: None, from: from.map(Path::to_path_buf), new_tab }),
            LinkRow::Tag(t) => Some(Command::GlobalSearch { query: Some(format!("#{t}")) }),
        }
    }
}

#[derive(Default)]
pub struct LinksModel {
    /// Finished backlink rows for a key.
    back: Option<(BackKey, BackRows)>,
    pending: Option<(BackKey, Receiver<BackRows>)>,
    /// Bumped whenever a backlink result arrives (part of the rows cache key).
    epoch: u64,
    rows: Vec<LinkRow>,
    /// Rows cache key: (active path, index generation, backlink epoch, indexing active).
    rows_key: Option<(Option<PathBuf>, u64, u64, bool)>,
    /// True while waiting for an initial scan or contended index read lock.
    waiting_for_index: bool,
    /// The last `update` had an active note.
    has_note: bool,
}

impl LinksModel {
    /// True while a backlink computation is running or the index is not ready yet (poll again soon).
    pub fn is_pending(&self) -> bool {
        self.pending.is_some() || self.waiting_for_index
    }

    pub fn rows(&self) -> &[LinkRow] {
        &self.rows
    }

    /// What to show instead of the list while there are no rows.
    pub fn empty_message(&self) -> &'static str {
        if !self.has_note {
            "No note open."
        } else if self.is_pending() {
            "Finding backlinks\u{2026}"
        } else {
            "No links"
        }
    }

    /// The selectable row after (`dir > 0`) or before the row `from`, skipping headers and notes; stays on `from` at the ends.
    pub fn step_sel(&self, from: Option<usize>, dir: isize) -> Option<usize> {
        let ok = |i: &usize| self.rows[*i].command(None, false).is_some();
        if dir > 0 {
            (from.map_or(0, |f| f + 1)..self.rows.len()).find(ok).or(from)
        } else {
            match from {
                None => (0..self.rows.len()).rev().find(ok),
                Some(f) => (0..f).rev().find(ok).or(from),
            }
        }
    }

    /// Collects a finished backlink computation, starts one if the note or index changed, rebuilds rows if needed.
    /// Cheap when nothing changed; never blocks the UI thread if the index write lock is held.
    pub fn update<U>(&mut self, state: &AppState<U>, path: Option<&Path>) {
        self.update_parts(&state.waker, &state.indexer, path);
    }

    fn update_parts(&mut self, waker: &Arc<dyn Waker>, indexer: &Indexer, path: Option<&Path>) {
        if let Some((k, rx)) = &self.pending {
            if let Ok(rows) = rx.try_recv() {
                self.back = Some((*k, rows));
                self.pending = None;
                self.epoch += 1;
            }
        }

        self.has_note = path.is_some();
        let scanning = indexer.status().scanning;
        let ix_arc = indexer.index();
        let Some(ix) = ix_arc.try_read() else {
            self.waiting_for_index = true;
            if self.rows_key.as_ref().is_none_or(|k| k.0.as_deref() != path) {
                self.rows_key = None;
                self.rows = match path {
                    Some(_) if scanning => vec![LinkRow::Note("Indexing workspace...".into())],
                    _ => Vec::new(),
                };
            }
            return;
        };

        let gen_ = ix.generation();
        let id = path.and_then(|p| ix.by_path(p));
        self.waiting_for_index = scanning && path.is_some() && id.is_none();

        if let Some(id) = id {
            let want = (gen_, id);
            // While the index is filled section by section every bump would start an O(N) backlink build over unfinished data.
            if self.back.as_ref().map(|b| b.0) != Some(want) && self.pending.is_none() && !indexer.partial() {
                let (tx, rx) = bounded(1);
                let ix = ix_arc.clone();
                let waker: Arc<dyn Waker> = waker.clone();
                let spawned = std::thread::Builder::new().name("iron-sheets-backlinks".into()).spawn(move || {
                    let ix = ix.read();
                    let rows: BackRows =
                        ix.backlinks(id).into_iter().filter_map(|b| ix.get(b.from).map(|m| (m.path.clone(), m.stem.clone(), b.line))).collect();
                    let _ = tx.send(rows);
                    waker.wake();
                });
                if spawned.is_ok() {
                    self.pending = Some((want, rx));
                }
            }
        }

        let same = self.rows_key.as_ref().is_some_and(|k| k.0.as_deref() == path && k.1 == gen_ && k.2 == self.epoch && k.3 == scanning);
        if !same {
            self.rows_key = Some((path.map(Path::to_path_buf), gen_, self.epoch, scanning));
            self.rows = self.build(&ix, id, path, scanning);
        }
    }

    fn build(&self, ix: &Index, id: Option<u32>, path: Option<&Path>, scanning: bool) -> Vec<LinkRow> {
        let mut rows = Vec::new();
        let (Some(id), Some(path)) = (id, path) else {
            if path.is_some() {
                let msg = if scanning { "Indexing workspace..." } else { "Not indexed yet (or not a note)." };
                rows.push(LinkRow::Note(msg.into()));
            }
            return rows;
        };
        let Some(meta) = ix.get(id) else { return rows };
        match &self.back {
            Some((k, b)) if k.1 == id => {
                rows.push(LinkRow::Header(format!("Backlinks ({})", b.len())));
                if b.is_empty() {
                    rows.push(LinkRow::Note("None.".into()));
                }
                for (p, stem, line) in b {
                    rows.push(LinkRow::Back { path: p.clone(), text: format!("{stem} \u{2014} line {}", line + 1), line: *line });
                }
            }
            _ => {
                rows.push(LinkRow::Header("Backlinks".into()));
                rows.push(LinkRow::Note("Finding backlinks\u{2026}".into()));
            }
        }
        rows.push(LinkRow::Header(format!("Outgoing ({})", meta.links.len())));
        if meta.links.is_empty() {
            rows.push(LinkRow::Note("None.".into()));
        }
        for l in &meta.links {
            let resolved = ix.resolve(&l.target, Some(path)).is_some();
            let text = format!("-> {}{} \u{2014} line {}{}", if l.embed { "!" } else { "" }, l.target, l.line + 1, if resolved { "" } else { " (unresolved)" });
            rows.push(LinkRow::Out { target: l.target.clone(), text, resolved });
        }
        rows.push(LinkRow::Header(format!("Tags ({})", meta.tags.len())));
        if meta.tags.is_empty() {
            rows.push(LinkRow::Note("None.".into()));
        }
        for t in &meta.tags {
            rows.push(LinkRow::Tag(t.trim_start_matches('#').to_string()));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::state_with;
    use std::time::{Duration, Instant};

    const VAULT: &[(&str, &str)] = &[
        ("Welcome.md", "# Welcome\n\nSee [[Plan]] and [[Missing]] and ![[Ideas]].\n\n#intro #shared\n"),
        ("Plan.md", "# Plan\n\nBack to [[Welcome]].\n"),
        ("Ideas.md", "# Ideas\n\nSee [[Welcome]].\n\nAnd again [[Welcome]].\n"),
    ];

    fn settle<U>(m: &mut LinksModel, state: &AppState<U>, path: &Path) {
        m.update(state, Some(path));
        let t0 = Instant::now();
        while m.is_pending() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
            m.update(state, Some(path));
        }
        assert!(!m.is_pending(), "backlinks computed");
    }

    fn texts(m: &LinksModel) -> Vec<String> {
        m.rows()
            .iter()
            .map(|r| match r {
                LinkRow::Header(t) | LinkRow::Note(t) | LinkRow::Tag(t) => t.clone(),
                LinkRow::Back { text, .. } | LinkRow::Out { text, .. } => text.clone(),
            })
            .collect()
    }

    #[test]
    fn groups_backlinks_outgoing_and_tags() {
        let (_d, state) = state_with(VAULT);
        let mut m = LinksModel::default();
        let welcome = state.vault_path("Welcome.md");
        // Immediately after the first update the backlinks are still being computed.
        m.update(&state, Some(&welcome));
        assert!(matches!(&m.rows()[..2], [LinkRow::Header(h), LinkRow::Note(n)] if h == "Backlinks" && n == "Finding backlinks\u{2026}"));
        settle(&mut m, &state, &welcome);
        let t = texts(&m);
        assert_eq!(t[0], "Backlinks (3)");
        let backs: Vec<&str> = t[1..4].iter().map(String::as_str).collect();
        assert!(backs.contains(&"Plan \u{2014} line 3"), "{backs:?}");
        assert_eq!(backs.iter().filter(|s| s.starts_with("Ideas")).count(), 2, "one row per link");
        assert_eq!(t[4], "Outgoing (3)");
        assert_eq!(t[5], "-> Plan \u{2014} line 3");
        assert_eq!(t[6], "-> Missing \u{2014} line 3 (unresolved)");
        assert_eq!(t[7], "-> !Ideas \u{2014} line 3");
        assert!(matches!(&m.rows()[6], LinkRow::Out { resolved: false, .. }));
        assert_eq!(t[8], "Tags (2)");
        assert_eq!(&t[9..], ["intro", "shared"]);
    }

    #[test]
    fn empty_messages_and_stepping() {
        let (_d, state) = state_with(VAULT);
        let mut m = LinksModel::default();
        m.update(&state, None);
        assert_eq!(m.empty_message(), "No note open.");
        m.update(&state, Some(&state.vault_path("Welcome.md")));
        assert!(m.is_pending());
        assert_eq!(m.empty_message(), "Finding backlinks\u{2026}");
        settle(&mut m, &state, &state.vault_path("Welcome.md"));
        assert_eq!(m.empty_message(), "No links");
        // Rows: Backlinks header, 3 backs, Outgoing header, 3 outs, Tags header, 2 tags.
        assert_eq!(m.step_sel(None, 1), Some(1));
        assert_eq!(m.step_sel(Some(3), 1), Some(5), "skips the header");
        assert_eq!(m.step_sel(Some(5), -1), Some(3));
        assert_eq!(m.step_sel(Some(1), -1), Some(1), "stays at the first");
        assert_eq!(m.step_sel(Some(10), 1), Some(10), "stays at the last");
        assert_eq!(m.step_sel(None, -1), Some(10));
    }

    #[test]
    fn empty_sections_say_none() {
        let (_d, state) = state_with(&[("Alone.md", "# Alone\n")]);
        let mut m = LinksModel::default();
        let p = state.vault_path("Alone.md");
        settle(&mut m, &state, &p);
        assert_eq!(texts(&m), ["Backlinks (0)", "None.", "Outgoing (0)", "None.", "Tags (0)", "None."]);
    }

    #[test]
    fn no_note_and_unindexed_note() {
        let (_d, state) = state_with(VAULT);
        let mut m = LinksModel::default();
        m.update(&state, None);
        assert!(m.rows().is_empty());
        assert!(!m.is_pending());
        m.update(&state, Some(&state.vault_path("nope.md")));
        assert_eq!(texts(&m), ["Not indexed yet (or not a note)."]);
    }

    #[test]
    fn rows_are_cached_between_updates() {
        let (_d, state) = state_with(VAULT);
        let mut m = LinksModel::default();
        let p = state.vault_path("Plan.md");
        settle(&mut m, &state, &p);
        let ptr = m.rows().as_ptr();
        m.update(&state, Some(&p));
        assert_eq!(ptr, m.rows().as_ptr());
    }

    #[test]
    fn click_commands() {
        let from = PathBuf::from("/v/a.md");
        let back = LinkRow::Back { path: PathBuf::from("/v/b.md"), text: "b".into(), line: 4 };
        assert!(matches!(back.command(Some(&from), true), Some(Command::OpenPath { new_tab: true, line: Some(4), .. })));
        let out = LinkRow::Out { target: "T".into(), text: "t".into(), resolved: true };
        assert!(matches!(out.command(Some(&from), false), Some(Command::OpenLink { note, from: Some(f), new_tab: false, .. }) if note == "T" && f == from));
        assert!(matches!(LinkRow::Tag("x".into()).command(None, false), Some(Command::GlobalSearch { query: Some(q) }) if q == "#x"));
        assert!(LinkRow::Header("h".into()).command(None, false).is_none());
        assert!(LinkRow::Note("n".into()).command(None, false).is_none());
    }

    #[test]
    fn non_blocking_and_scanning_messages() {
        let (_d, mut state) = state_with(VAULT);
        let welcome = state.vault_path("Welcome.md");
        let missing = state.vault_path("not_yet.md");
        let mut m = LinksModel::default();
        settle(&mut m, &state, &welcome);
        let initial_rows = m.rows().len();

        // Write lock held on the index: update returns immediately without blocking and keeps cached rows for the same path.
        let ix_arc = state.indexer.index();
        let write_guard = ix_arc.write();
        m.update(&state, Some(&welcome));
        assert!(m.is_pending());
        assert_eq!(m.rows().len(), initial_rows);

        // Switching to an unseen path while write lock is held does not block.
        m.update(&state, Some(&missing));
        assert!(m.is_pending());
        drop(write_guard);

        // While a scan is active on a larger folder, an unindexed note shows "Indexing workspace..." and marks is_pending().
        let dir = tempfile::tempdir().expect("tempdir");
        let chunk = "# H\n[[Target]] word\n".repeat(40_000);
        for i in 0..4 {
            std::fs::write(dir.path().join(format!("big{i}.md")), &chunk).expect("write");
        }
        let unindexed = dir.path().join("later.md");
        let scanning_ix = iron_sheets_core::index::Indexer::spawn(vec![dir.path().to_path_buf()], None, Arc::new(|| {}));
        state.indexer = scanning_ix;
        if state.indexer.status().scanning {
            let mut m2 = LinksModel::default();
            m2.update(&state, Some(&unindexed));
            if state.indexer.status().scanning && state.indexer.index().try_read().is_none_or(|g| g.by_path(&unindexed).is_none()) {
                assert_eq!(texts(&m2), ["Indexing workspace..."]);
                assert!(m2.is_pending());
            }
            assert!(state.indexer.wait_idle(Duration::from_secs(10)));
            m2.update(&state, Some(&unindexed));
            assert_eq!(texts(&m2), ["Not indexed yet (or not a note)."]);
            assert!(!m2.is_pending());
        }
    }
}
