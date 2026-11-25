//! Quick switcher and command palette: candidate cache, ranking, selection and activation. No UI toolkit here.
//!
//! Prefix `>` in files mode switches to commands, `#` lists tags.
use crate::command::{Command, all_commands};
use crate::keymap::Keymap;
use crate::state::AppState;
use crate::tabs::Tabs;
use iron_sheets_core::fuzzy::{fuzzy_match, fuzzy_rank};
use iron_sheets_core::index::{Index, Indexer};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Maximum rows shown.
pub const LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaletteMode {
    #[default]
    Files,
    Commands,
}

/// What the query currently selects (mode plus prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effective {
    Files,
    Commands,
    Tags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    File(PathBuf),
    Create(String),
    /// Index into `all_commands()`.
    Cmd(usize),
    Tag(String),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub name: String,
    pub dir: String,
    /// Matched char indices within `name`.
    pub name_pos: Vec<u32>,
    /// Matched char indices within `dir`.
    pub dir_pos: Vec<u32>,
    pub hint: String,
}

/// Selection movement from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Down,
    Up,
    PageDown,
    PageUp,
    Home,
    End,
}

struct Cand {
    path: PathBuf,
    rel: String,
    name_off: u32,
    stem_lc: String,
    /// Folded like the fuzzy matcher does, for the cheap subsequence pre-filter.
    rel_lc: Vec<char>,
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

fn subseq(q: &[char], hay: &[char]) -> bool {
    let mut it = hay.iter();
    q.iter().all(|c| it.any(|h| h == c))
}

fn split_rel(rel: &str) -> (String, String) {
    match rel.rsplit_once('/') {
        Some((d, n)) => (n.to_string(), d.to_string()),
        None => (rel.to_string(), String::new()),
    }
}

fn row_for_path(ix: Option<&Index>, path: &Path, hint: &str, untitled: bool) -> Row {
    // An untitled note lives in the app data folder, which is not shown.
    let (name, dir) = if untitled {
        (crate::command::untitled_title(path), "not saved to a folder".to_string())
    } else {
        match ix.and_then(|ix| ix.by_path(path).and_then(|id| ix.get(id))) {
            Some(m) => split_rel(&m.rel),
            None => (
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            ),
        }
    };
    Row { kind: RowKind::File(path.to_path_buf()), name, dir, name_pos: Vec::new(), dir_pos: Vec::new(), hint: hint.to_string() }
}

/// Cache key of the ranked rows: (effective mode, query, index generation, tabs signature for the empty query).
type RankKey = (Effective, String, u64, u64, bool);

/// While the palette is open the candidate list is rebuilt at most this often when only the index changed (a scan bumps
/// its generation about every 50 ms).
const REBUILD_EVERY: std::time::Duration = std::time::Duration::from_millis(150);

#[derive(Default)]
pub struct PaletteModel {
    pub open: bool,
    pub mode: PaletteMode,
    pub query: String,
    /// Incremented by every `begin`; a frontend resets its focus/scroll state when it changes.
    pub open_seq: u32,
    sel: usize,
    cands: Vec<Cand>,
    cands_gen: Option<u64>,
    /// Candidates passing the subsequence test for `pool_q`; typing more characters narrows it.
    pool: Vec<u32>,
    pool_q: Vec<char>,
    cands_at: Option<std::time::Instant>,
    /// A newer index generation is waiting for `REBUILD_EVERY` to pass: the frontend repaints soon.
    throttled: bool,
    /// Notes known while the index is still scanning.
    indexing: Option<u32>,
    rows: Vec<Row>,
    last: Option<RankKey>,
}

impl PaletteModel {
    /// Opens in `mode` with an empty query and the first row selected.
    pub fn begin(&mut self, mode: PaletteMode) {
        self.open = true;
        self.open_seq = self.open_seq.wrapping_add(1);
        self.mode = mode;
        self.query.clear();
        self.sel = 0;
        self.last = None;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn effective(&self) -> Effective {
        self.eff().0
    }

    fn eff(&self) -> (Effective, &str) {
        let q = self.query.as_str();
        match self.mode {
            PaletteMode::Commands => (Effective::Commands, q.strip_prefix('>').unwrap_or(q)),
            PaletteMode::Files => {
                if let Some(r) = q.strip_prefix('>') {
                    (Effective::Commands, r)
                } else if let Some(r) = q.strip_prefix('#') {
                    (Effective::Tags, r)
                } else {
                    (Effective::Files, q)
                }
            }
        }
    }

    /// Placeholder text of the query field.
    pub fn placeholder(&self) -> &'static str {
        match self.eff().0 {
            Effective::Commands => "Run a command...",
            Effective::Tags => "Search a tag...",
            Effective::Files => "Open note (type > for commands, # for tags)",
        }
    }

    /// "Indexing... N notes" while the index is still being built (the list may be incomplete), else `None`.
    pub fn status(&self) -> Option<String> {
        self.indexing.map(|n| format!("Indexing... {n} notes"))
    }

    /// True when a refresh was postponed: ask for another frame in `REBUILD_EVERY`.
    pub fn throttled(&self) -> bool {
        self.throttled
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.sel
    }

    /// The note the selected row would open (for reading it in the background while the user is still choosing).
    pub fn selected_file(&self) -> Option<&std::path::Path> {
        match self.rows.get(self.sel).map(|r| &r.kind) {
            Some(RowKind::File(p)) => Some(p.as_path()),
            _ => None,
        }
    }

    /// The query text was edited: selection returns to the top.
    pub fn query_changed(&mut self) {
        self.sel = 0;
    }

    /// Moves the selection; returns true when it changed.
    pub fn nav(&mut self, nav: Nav) -> bool {
        let n = self.rows.len();
        let old = self.sel;
        self.sel = match nav {
            Nav::Down => {
                if n == 0 {
                    0
                } else {
                    (self.sel + 1) % n
                }
            }
            Nav::Up => {
                if n == 0 {
                    0
                } else {
                    (self.sel + n - 1) % n
                }
            }
            Nav::PageDown => (self.sel + 10).min(n.saturating_sub(1)),
            Nav::PageUp => self.sel.saturating_sub(10),
            Nav::Home => 0,
            Nav::End => n.saturating_sub(1),
        };
        self.sel != old
    }

    /// Candidate cache per index generation; ranking only when mode, query or (for the empty query) tabs change.
    /// Never blocks the UI thread if the index write lock is briefly held.
    pub fn refresh<U>(&mut self, state: &AppState<U>) {
        let st = state.indexer.status();
        self.refresh_with_parts(&state.indexer, &state.tabs, &state.keymap, st.scanning.then_some(st.notes));
    }

    #[cfg(test)]
    fn refresh_with<U>(&mut self, state: &AppState<U>, scanning: Option<u32>) {
        self.refresh_with_parts(&state.indexer, &state.tabs, &state.keymap, scanning);
    }

    fn refresh_with_parts(&mut self, indexer: &Indexer, tabs: &Tabs, keymap: &Keymap, scanning: Option<u32>) {
        let (eff, q) = self.eff();
        let q = q.to_string();
        let ix_arc = indexer.index();
        let ix = ix_arc.try_read();
        // No index yet (the first scan holds the lock) counts as indexing, whatever the indexer reports.
        let scanning = scanning.or((ix.is_none() && self.cands_gen.is_none()).then_some(0));
        self.indexing = scanning.map(|n| n.max(self.cands.len() as u32));
        let gen_ = match (&ix, self.cands_gen) {
            (Some(ix), _) => {
                let g = ix.generation();
                if self.cands_gen != Some(g) {
                    let same_query = self.last.as_ref().is_some_and(|l| l.0 == eff && l.1 == q);
                    if same_query && self.cands_at.is_some_and(|t| t.elapsed() < REBUILD_EVERY) {
                        self.throttled = true;
                        return;
                    }
                    self.cands_gen = Some(g);
                    self.cands_at = Some(std::time::Instant::now());
                    self.pool_q.clear();
                    let mut cands = Vec::with_capacity(ix.len());
                    cands.extend(ix.notes().map(|n| {
                        let name_off = n.rel.rfind('/').map_or(0, |i| n.rel[..=i].chars().count() as u32);
                        Cand { path: n.path.clone(), rel: n.rel.clone(), name_off, stem_lc: n.stem.to_lowercase(), rel_lc: n.rel.chars().map(fold).collect() }
                    }));
                    self.cands = cands;
                }
                self.throttled = false;
                g
            }
            (None, Some(g)) => {
                if eff == Effective::Tags || (eff == Effective::Files && q.trim().is_empty()) {
                    return;
                }
                g
            }
            // The first scan holds the lock: show what is known without the index (the open tabs for an empty query).
            (None, None) => {
                if eff == Effective::Tags {
                    return;
                }
                0
            }
        };
        let sig = if q.trim().is_empty() && eff == Effective::Files {
            let mut h = rustc_hash::FxHasher::default();
            for t in &tabs.tabs {
                t.path.hash(&mut h);
            }
            tabs.closed.hash(&mut h);
            h.finish()
        } else {
            0
        };
        if self.last.as_ref().is_some_and(|l| l.0 == eff && l.1 == q && l.2 == gen_ && l.3 == sig && l.4 == self.indexing.is_some()) {
            return;
        }
        self.last = Some((eff, q.clone(), gen_, sig, self.indexing.is_some()));
        self.rows = match eff {
            Effective::Files => {
                if !q.trim().is_empty() {
                    self.update_pool(&q);
                }
                self.file_rows_parts(ix.as_deref(), tabs, &q)
            }
            Effective::Commands => {
                let cmds = all_commands();
                let mk = |i: usize, pos: Vec<u32>| Row {
                    kind: RowKind::Cmd(i),
                    name: cmds[i].title.to_string(),
                    dir: String::new(),
                    name_pos: pos,
                    dir_pos: Vec::new(),
                    hint: keymap.hint(i).to_string(),
                };
                fuzzy_rank(&q, cmds.iter().enumerate().map(|(i, c)| (i, c.title)), LIMIT).into_iter().map(|(i, m)| mk(i, m.positions.to_vec())).collect()
            }
            Effective::Tags => ix
                .as_deref()
                .map(|ix| {
                    ix.all_tags()
                        .into_iter()
                        .filter_map(|(t, n)| {
                            let t = t.trim_start_matches('#').to_string();
                            let m = fuzzy_match(&q, &t)?;
                            let row = Row {
                                name: format!("#{t}"),
                                dir: format!("{n} notes"),
                                name_pos: m.positions.iter().map(|p| p + 1).collect(),
                                dir_pos: Vec::new(),
                                hint: String::new(),
                                kind: RowKind::Tag(t),
                            };
                            Some((m.score, row))
                        })
                        .take(LIMIT)
                        .map(|(_, r)| r)
                        .collect()
                })
                .unwrap_or_default(),
        };
        if self.sel >= self.rows.len() {
            self.sel = self.rows.len().saturating_sub(1);
        }
    }

    fn update_pool(&mut self, q: &str) {
        let ql: Vec<char> = q.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
        if !self.pool_q.is_empty() && ql.starts_with(&self.pool_q) {
            self.pool.retain(|&i| subseq(&ql, &self.cands[i as usize].rel_lc));
        } else {
            self.pool = (0..self.cands.len() as u32).filter(|&i| subseq(&ql, &self.cands[i as usize].rel_lc)).collect();
        }
        self.pool_q = ql;
    }

    fn file_rows_parts(&self, ix: Option<&Index>, tabs: &Tabs, q: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        let qt = q.trim();
        if qt.is_empty() {
            let mut seen = rustc_hash::FxHashSet::default();
            for t in &tabs.tabs {
                if !t.is_empty() && seen.insert(t.path.clone()) {
                    rows.push(row_for_path(ix, &t.path, "open", tabs.is_untitled(&t.path)));
                }
            }
            for p in tabs.closed.iter().rev() {
                if rows.len() >= LIMIT {
                    break;
                }
                if seen.insert(p.clone()) {
                    rows.push(row_for_path(ix, p, "closed", false));
                }
            }
            for c in &self.cands {
                if rows.len() >= LIMIT {
                    break;
                }
                if seen.insert(c.path.clone()) {
                    rows.push(row_for_path(ix, &c.path, "", false));
                }
            }
            return rows;
        }
        for (c, m) in fuzzy_rank(qt, self.pool.iter().map(|&i| &self.cands[i as usize]).map(|c| (c, c.rel.as_str())), LIMIT) {
            let (name, dir) = split_rel(&c.rel);
            // Positions below `name_off` belong to the directory part, the rest to the name (shifted).
            let name_pos = m.positions.iter().filter(|&&p| p >= c.name_off).map(|p| p - c.name_off).collect();
            let dir_pos = m.positions.iter().filter(|&&p| p < c.name_off).copied().collect();
            rows.push(Row { kind: RowKind::File(c.path.clone()), name, dir, name_pos, dir_pos, hint: String::new() });
        }
        let lc = qt.to_lowercase();
        // While indexing, a note of that name may just not be known yet.
        if self.indexing.is_none() && !self.cands.iter().any(|c| c.stem_lc == lc) {
            rows.push(Row {
                kind: RowKind::Create(qt.to_string()),
                name: format!("Create note '{qt}'"),
                dir: String::new(),
                name_pos: Vec::new(),
                dir_pos: Vec::new(),
                hint: String::new(),
            });
        }
        rows
    }

    /// Runs row `i` (by queueing commands) and closes the palette.
    pub fn activate<U>(&mut self, i: usize, new_tab: bool, state: &mut AppState<U>) {
        self.activate_parts(i, new_tab, &state.tabs, &mut state.commands);
    }

    fn activate_parts(&mut self, i: usize, new_tab: bool, tabs: &Tabs, commands: &mut Vec<Command>) {
        let Some(row) = self.rows.get(i) else { return };
        match &row.kind {
            RowKind::File(p) => commands.push(Command::OpenPath { path: p.clone(), new_tab, line: None }),
            RowKind::Create(name) => {
                commands.push(Command::CreateLinkedNote { note: name.clone(), from: tabs.active_path().map(Path::to_path_buf), new_tab });
            }
            RowKind::Cmd(c) => {
                if let Some(cmd) = (all_commands()[*c].make)(tabs) {
                    commands.push(cmd);
                }
            }
            RowKind::Tag(t) => commands.push(Command::GlobalSearch { query: Some(format!("#{t}")) }),
        }
        self.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{activate_tab, state_with, state_with_many};
    use std::time::Duration;

    /// `cargo test -p iron-sheets-app-core --lib palette_rebuild_time -- --ignored --nocapture`
    #[test]
    #[ignore = "perf measurement"]
    fn palette_rebuild_time_at_50k_notes() {
        let (_d, state) = state_with_many(50_000);
        let mut best = f64::MAX;
        for _ in 0..5 {
            let mut m = PaletteModel::default();
            m.begin(PaletteMode::Files);
            let t = std::time::Instant::now();
            m.refresh(&state);
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(m.cands.len(), 50_000);
        }
        println!("PERF palette cand build 50k notes: best of 5 {best:.1} ms");
    }

    const VAULT: &[(&str, &str)] = &[
        ("Welcome.md", "# Welcome\n\n#intro #shared\n"),
        ("Ideas.md", "# Ideas\n\n#shared\n"),
        ("Projects/Plan.md", "# Plan\n"),
        ("Projects/Tasks.md", "# Tasks\n"),
        ("Archive/Plan.md", "# Old plan\n"),
    ];

    fn names(m: &PaletteModel) -> Vec<String> {
        m.rows().iter().map(|r| r.name.clone()).collect()
    }

    #[test]
    fn file_query_ranks_and_offers_create() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "tasks".into();
        m.refresh(&state);
        let rows = m.rows();
        assert_eq!(rows[0].name, "Tasks.md");
        assert_eq!(rows[0].dir, "Projects");
        assert!(matches!(&rows[0].kind, RowKind::File(p) if *p == state.vault_path("Projects/Tasks.md")));
        // Matched chars inside the name: "tasks" -> 0..5 of "Tasks.md"; none in the directory.
        assert_eq!(rows[0].name_pos, vec![0, 1, 2, 3, 4]);
        assert!(rows[0].dir_pos.is_empty());
        // No note with the stem "tasks" exists? It does, so no create row.
        assert!(!rows.iter().any(|r| matches!(r.kind, RowKind::Create(_))));
        m.query = "zzq".into();
        m.refresh(&state);
        assert_eq!(names(&m), vec!["Create note 'zzq'"]);
    }

    #[test]
    fn exact_stem_and_offsets_with_case_and_non_ascii() {
        let (_d, state) = state_with(&[("Über/Café.md", "# Café\n"), ("Projects/Tasks.md", "# Tasks\n")]);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "TASKS".into(); // ASCII fast path, other case: still an exact stem
        m.refresh(&state);
        assert!(!m.rows().iter().any(|r| matches!(r.kind, RowKind::Create(_))));
        m.query = "überCAFÉ".into(); // non-ASCII: char offsets, folded case
        m.refresh(&state);
        let r = &m.rows()[0];
        assert_eq!((r.name.as_str(), r.dir.as_str()), ("Café.md", "Über"));
        assert_eq!(r.dir_pos, vec![0, 1, 2, 3]);
        assert_eq!(r.name_pos, vec![0, 1, 2, 3]);
        m.query = "café".into();
        m.refresh(&state);
        assert!(!m.rows().iter().any(|r| matches!(r.kind, RowKind::Create(_))));
    }

    #[test]
    fn path_match_splits_between_dir_and_name() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "projplan".into();
        m.refresh(&state);
        let r = &m.rows()[0];
        assert_eq!((r.name.as_str(), r.dir.as_str()), ("Plan.md", "Projects"));
        assert!(!r.dir_pos.is_empty() && !r.name_pos.is_empty());
        assert!(r.dir_pos.iter().all(|&p| p < "Projects/".len() as u32));
    }

    #[test]
    fn empty_query_lists_open_then_closed_then_rest() {
        let (_d, mut state) = state_with(VAULT);
        activate_tab(&mut state, "Ideas.md");
        state.tabs.closed.push(state.vault_path("Welcome.md"));
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.refresh(&state);
        let rows = m.rows();
        assert_eq!((rows[0].name.as_str(), rows[0].hint.as_str()), ("Ideas.md", "open"));
        assert_eq!((rows[1].name.as_str(), rows[1].hint.as_str()), ("Welcome.md", "closed"));
        assert_eq!(rows.len(), 5, "all notes once");
        // The tab set is part of the cache key for the empty query.
        activate_tab(&mut state, "Projects/Tasks.md");
        m.refresh(&state);
        assert_eq!(m.rows().iter().filter(|r| r.hint == "open").count(), 2);
    }

    #[test]
    fn rows_are_cached_until_the_inputs_change() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "plan".into();
        m.refresh(&state);
        let p = m.rows().as_ptr();
        m.refresh(&state);
        assert_eq!(p, m.rows().as_ptr(), "same inputs: rows untouched");
        m.query = "plans".into();
        m.refresh(&state);
        assert_ne!(names(&m), vec!["Plan.md".to_string(), "Plan.md".to_string()]);
    }

    #[test]
    fn prefixes_switch_to_commands_and_tags() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = ">save".into();
        m.refresh(&state);
        assert_eq!(m.effective(), Effective::Commands);
        assert_eq!(m.placeholder(), "Run a command...");
        assert!(m.rows().iter().any(|r| r.name == "Save" && matches!(r.kind, RowKind::Cmd(_))));
        m.query = "#sha".into();
        m.refresh(&state);
        assert_eq!(m.effective(), Effective::Tags);
        assert_eq!(m.rows().len(), 1);
        let r = &m.rows()[0];
        assert_eq!((r.name.as_str(), r.dir.as_str()), ("#shared", "2 notes"));
        assert_eq!(r.name_pos, vec![1, 2, 3], "positions are shifted past the #");
        // Command mode without a prefix.
        m.begin(PaletteMode::Commands);
        m.query = "save".into();
        m.refresh(&state);
        assert_eq!(m.effective(), Effective::Commands);
        assert!(m.rows()[0].name.to_lowercase().contains("save"));
    }

    #[test]
    fn navigation_wraps_and_pages() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.refresh(&state);
        assert_eq!(m.rows().len(), 5);
        assert_eq!(m.selected(), 0);
        assert!(!m.nav(Nav::Home));
        assert!(m.nav(Nav::Up));
        assert_eq!(m.selected(), 4, "up from the top wraps");
        assert!(m.nav(Nav::Down));
        assert_eq!(m.selected(), 0);
        assert!(m.nav(Nav::PageDown));
        assert_eq!(m.selected(), 4, "page down clamps");
        assert!(m.nav(Nav::PageUp));
        assert_eq!(m.selected(), 0);
        assert!(m.nav(Nav::End));
        assert_eq!(m.selected(), 4);
        m.query_changed();
        assert_eq!(m.selected(), 0);
        // Selection is clamped when the list shrinks.
        m.nav(Nav::End);
        m.query = "zzq".into();
        m.refresh(&state);
        assert_eq!(m.selected(), 0);
    }

    #[test]
    fn activation_queues_commands_and_closes() {
        let (_d, mut state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "welcome".into();
        m.refresh(&state);
        m.activate(0, true, &mut state);
        assert!(!m.open);
        assert!(matches!(state.commands.pop(), Some(Command::OpenPath { new_tab: true, line: None, .. })));
        m.begin(PaletteMode::Files);
        m.query = "brand new".into();
        m.refresh(&state);
        let last = m.rows().len() - 1;
        m.activate(last, false, &mut state);
        assert!(matches!(state.commands.pop(), Some(Command::CreateLinkedNote { note, .. }) if note == "brand new"));
        m.begin(PaletteMode::Files);
        m.query = "#intro".into();
        m.refresh(&state);
        m.activate(0, false, &mut state);
        assert!(matches!(state.commands.pop(), Some(Command::GlobalSearch { query: Some(q) }) if q == "#intro"));
        // Out of range: nothing happens and the palette stays open.
        m.begin(PaletteMode::Files);
        m.activate(99, false, &mut state);
        assert!(m.open && state.commands.is_empty());
    }

    #[test]
    fn refresh_is_non_blocking_when_index_write_locked() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "tasks".into();
        m.refresh(&state);
        assert_eq!(m.rows()[0].name, "Tasks.md");

        let ix_arc = state.indexer.index();
        let _write_guard = ix_arc.write();

        // While write lock is held, cached candidates still answer filtered file queries without blocking.
        m.query = "plan".into();
        m.refresh(&state);
        assert!(m.rows().iter().any(|r| r.name == "Plan.md"));

        // Commands mode works without touching the index lock.
        m.query = ">save".into();
        m.refresh(&state);
        assert_eq!(m.effective(), Effective::Commands);
        assert!(m.rows().iter().any(|r| r.name == "Save"));

        // Empty query keeps previous cached entries instead of blocking or dropping them.
        m.query = "".into();
        let prev_len = m.rows().len();
        m.refresh(&state);
        assert_eq!(m.rows().len(), prev_len);

        // Fresh palette before first candidate cache build does not block either.
        let mut fresh = PaletteModel::default();
        fresh.begin(PaletteMode::Files);
        fresh.refresh(&state);
        assert!(fresh.rows().is_empty());
        drop(_write_guard);

        fresh.refresh(&state);
        assert_eq!(fresh.rows().len(), 5);
    }

    #[test]
    fn shows_an_indexing_status_and_the_open_tabs_while_the_first_scan_holds_the_lock() {
        let (_d, mut state) = state_with(VAULT);
        activate_tab(&mut state, "Ideas.md");
        let idx = state.indexer.index();
        let _held = idx.write();
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.refresh_with(&state, Some(7));
        assert_eq!(m.status().as_deref(), Some("Indexing... 7 notes"));
        assert_eq!(names(&m), vec!["Ideas.md"], "the open tabs are listed without the index");
        // A note of that name may just not be indexed yet: no "Create" row while indexing.
        m.query = "zzq".into();
        m.refresh_with(&state, Some(7));
        assert!(names(&m).is_empty());
        drop(_held);
        m.refresh_with(&state, None);
        assert_eq!(m.status(), None);
        assert_eq!(names(&m), vec!["Create note 'zzq'"]);
    }

    #[test]
    fn candidates_are_rebuilt_at_most_every_150ms_unless_the_query_changed() {
        let (_d, state) = state_with(VAULT);
        let mut m = PaletteModel::default();
        m.begin(PaletteMode::Files);
        m.query = "plan".into();
        m.refresh(&state);
        assert_eq!(m.rows().len(), 2, "two plans");
        // The index changes (a new note) right after the rebuild.
        let p = state.vault_path("Projects/Plan B.md");
        std::fs::write(&p, "# Plan B\n").expect("write");
        state.indexer.file_changed(p);
        assert!(state.indexer.wait_idle(Duration::from_secs(10)));
        m.refresh(&state);
        assert!(m.throttled());
        assert_eq!(m.rows().len(), 2, "not rebuilt yet");
        // A new query does not wait.
        m.query = "plan b".into();
        m.refresh(&state);
        assert!(m.rows().iter().any(|r| r.name == "Plan B.md"));
        // Same query: rebuilt once the interval has passed.
        m.query = "plan".into();
        m.refresh(&state);
        std::thread::sleep(Duration::from_millis(170));
        m.refresh(&state);
        assert!(!m.throttled());
        assert!(m.rows().iter().any(|r| r.name == "Plan B.md"));
    }
}
