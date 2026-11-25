//! Tabs: one per open file, with Obsidian-like navigation history.
use crate::scrollsync::PaneScroll;
use iron_sheets_core::config::ViewMode;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Tab {
    pub id: u64,
    pub path: PathBuf,
    pub view: ViewMode,
    pub history: Vec<PathBuf>,
    hist_state: Vec<Option<(usize, f32)>>,
    pub hist_pos: usize,
    /// Bumped when the tab shows another file (panes reset per-document state on change).
    pub nav_gen: u64,
    pub split_ratio: f32,
    pub editor_scroll: PaneScroll,
    pub preview_scroll: PaneScroll,
    /// Sync was just switched on: the preview takes the editor's position once (see `scrollsync::sync`).
    pub resync: bool,
    /// Byte range to select; applied by the editor next frame and scrolled into view.
    pub pending_cursor: Option<Range<usize>>,
    /// Line to select once the document has loaded (the tab was opened before its text arrived).
    pub pending_line: Option<u32>,
    /// The note (and view) this tab showed before an open that is still loading replaced it; restored if the load fails.
    pub replaced: Option<(PathBuf, ViewMode)>,
    /// History position, path, and view before a history navigation whose target is still loading; restored on error without popping `history`.
    pub replaced_hist: Option<(usize, PathBuf, ViewMode)>,
    /// Why the file could not be opened; the tab shows a "Can't open" panel instead of the document (Retry clears it).
    pub load_error: Option<String>,
    /// Current selection in bytes, written by the editor every frame.
    pub cursor: Option<Range<usize>>,
}

impl Tab {
    /// A tab with no document has an empty path: the empty tab (a page of actions, no editor and no preview).
    pub fn new(id: u64, path: PathBuf) -> Tab {
        Tab {
            id,
            history: if path.as_os_str().is_empty() { Vec::new() } else { vec![path.clone()] },
            hist_state: if path.as_os_str().is_empty() { Vec::new() } else { vec![None] },
            path,
            view: ViewMode::Split,
            hist_pos: 0,
            nav_gen: 0,
            split_ratio: 0.5,
            editor_scroll: PaneScroll::default(),
            preview_scroll: PaneScroll::default(),
            resync: false,
            pending_cursor: None,
            pending_line: None,
            replaced: None,
            replaced_hist: None,
            load_error: None,
            cursor: None,
        }
    }

    /// True for a tab without a document (empty path).
    pub fn is_empty(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    pub fn reset_panes(&mut self) {
        self.nav_gen += 1;
        self.editor_scroll = PaneScroll::default();
        self.preview_scroll = PaneScroll::default();
        self.pending_cursor = None;
        self.pending_line = None;
        self.replaced = None;
        self.replaced_hist = None;
        self.load_error = None;
        self.cursor = None;
    }

    pub fn record_current_history_state(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.hist_state.len() < self.history.len() {
            self.hist_state.resize(self.history.len(), None);
        }
        if self.hist_pos < self.hist_state.len() {
            let cursor = self.cursor.as_ref().or(self.pending_cursor.as_ref()).map_or(0, |c| c.start);
            let scroll = self.editor_scroll.force.filter(|_| self.editor_scroll.offset == 0.0).unwrap_or(self.editor_scroll.offset);
            self.hist_state[self.hist_pos] = Some((cursor, scroll));
        }
    }

    pub fn restore_history_entry(&mut self, pos: usize, path: PathBuf, view: ViewMode) {
        self.hist_pos = pos.min(self.history.len().saturating_sub(1));
        self.path = path;
        self.view = view;
        self.reset_panes();
        if let Some((cursor, scroll)) = self.hist_state.get(self.hist_pos).copied().flatten() {
            self.pending_cursor = Some(cursor..cursor);
            self.editor_scroll.force = Some(scroll);
        }
    }

    pub fn pop_replaced_history(&mut self) {
        self.history.pop();
        self.hist_state.truncate(self.history.len());
        self.hist_pos = self.history.len().saturating_sub(1);
    }

    pub fn can_back(&self) -> bool {
        self.hist_pos > 0
    }

    pub fn can_forward(&self) -> bool {
        self.hist_pos + 1 < self.history.len()
    }
}

#[derive(Default)]
pub struct Tabs {
    pub tabs: Vec<Tab>,
    pub active: Option<usize>,
    /// Most recently closed last.
    pub closed: Vec<PathBuf>,
    /// Folder of untitled notes (empty: none); tabs on files directly inside it are untitled tabs.
    pub untitled_dir: PathBuf,
    next_id: u64,
}

pub struct Opened {
    pub index: usize,
    /// File no longer shown by a tab because this one navigated away.
    pub released: Option<PathBuf>,
    pub created: bool,
}

impl Tabs {
    pub fn new(untitled_dir: PathBuf) -> Tabs {
        Tabs { untitled_dir, ..Default::default() }
    }

    /// True for `<untitled dir>/Untitled-<n>.md`.
    pub fn is_untitled(&self, path: &Path) -> bool {
        iron_sheets_core::untitled::number_of(&self.untitled_dir, path).is_some()
    }

    pub fn find(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|t| t.path == path)
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.active.and_then(|i| self.tabs.get_mut(i))
    }

    /// Path of the active document; `None` for no tab and for a tab without a document.
    pub fn active_path(&self) -> Option<&Path> {
        self.active_tab().filter(|t| !t.is_empty()).map(|t| t.path.as_path())
    }

    /// New empty tab (no document) right after the active one; becomes active.
    pub fn new_empty(&mut self) -> usize {
        let i = self.push_tab(PathBuf::new(), ViewMode::Split, false);
        self.active = Some(i);
        i
    }

    /// There is never a state without tabs: adds one empty tab when none is left. True when it did.
    pub fn ensure_one(&mut self) -> bool {
        if self.tabs.is_empty() {
            self.new_empty();
            true
        } else {
            false
        }
    }

    /// True when the active tab is empty (no document).
    pub fn active_is_empty(&self) -> bool {
        self.active_tab().is_some_and(Tab::is_empty)
    }

    /// New tab for an untitled note right after the active one; becomes active.
    pub fn new_untitled(&mut self, path: PathBuf, view: ViewMode) -> usize {
        let i = self.push_tab(path, view, false);
        self.active = Some(i);
        i
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.tabs.iter().any(|t| t.path == path)
    }

    /// New tab placed right after the active one (or at the end). Does not activate.
    pub fn push_tab(&mut self, path: PathBuf, view: ViewMode, at_end: bool) -> usize {
        self.next_id += 1;
        let mut t = Tab::new(self.next_id, path);
        t.view = view;
        let at = match self.active {
            Some(a) if !at_end && a < self.tabs.len() => a + 1,
            _ => self.tabs.len(),
        };
        self.tabs.insert(at, t);
        if let Some(a) = self.active.as_mut() {
            if *a >= at {
                *a += 1;
            }
        }
        at
    }

    /// Obsidian-like open: focus an existing tab for `path`; else a new tab when `new_tab` or `!can_replace`
    /// or nothing is active; else replace the active tab's document (pushing history).
    pub fn open(&mut self, path: &Path, new_tab: bool, can_replace: bool, view: ViewMode) -> Opened {
        self.open_ex(path, new_tab, can_replace, view, false)
    }

    /// Like `open`; with `take_over` the active tab is reused whatever `new_tab` and `can_replace` say (it shows an
    /// empty untitled note that the caller drops), its old path is returned as `released`.
    pub fn open_ex(&mut self, path: &Path, new_tab: bool, can_replace: bool, view: ViewMode, take_over: bool) -> Opened {
        if let Some(i) = self.find(path) {
            self.active = Some(i);
            return Opened { index: i, released: None, created: false };
        }
        match self.active {
            // An empty tab is always taken over, like Obsidian does.
            Some(a) if a < self.tabs.len() && self.tabs[a].is_empty() => {
                let t = &mut self.tabs[a];
                t.path = path.to_path_buf();
                t.history = vec![path.to_path_buf()];
                t.hist_state = vec![None];
                t.hist_pos = 0;
                t.view = view;
                t.reset_panes();
                Opened { index: a, released: None, created: false }
            }
            Some(a) if take_over && a < self.tabs.len() => {
                let t = &mut self.tabs[a];
                let old = std::mem::replace(&mut t.path, path.to_path_buf());
                t.history = vec![path.to_path_buf()];
                t.hist_state = vec![None];
                t.hist_pos = 0;
                t.view = view;
                t.reset_panes();
                Opened { index: a, released: Some(old), created: false }
            }
            Some(a) if !new_tab && can_replace && a < self.tabs.len() => {
                let t = &mut self.tabs[a];
                t.record_current_history_state();
                let old = std::mem::replace(&mut t.path, path.to_path_buf());
                t.history.truncate(t.hist_pos + 1);
                t.hist_state.truncate(t.hist_pos + 1);
                t.history.push(path.to_path_buf());
                t.hist_state.push(None);
                t.hist_pos = t.history.len() - 1;
                t.view = view;
                t.reset_panes();
                Opened { index: a, released: Some(old), created: false }
            }
            _ => {
                let i = self.push_tab(path.to_path_buf(), view, false);
                self.active = Some(i);
                Opened { index: i, released: None, created: true }
            }
        }
    }

    /// Move through history of the active tab. Returns (index, released path, new path).
    pub fn go_history(&mut self, back: bool) -> Option<(usize, PathBuf, PathBuf)> {
        let a = self.active?;
        let t = self.tabs.get_mut(a)?;
        let pos = if back { t.hist_pos.checked_sub(1)? } else { (t.hist_pos + 1 < t.history.len()).then_some(t.hist_pos + 1)? };
        t.record_current_history_state();
        t.hist_pos = pos;
        let new = t.history[pos].clone();
        let old = std::mem::replace(&mut t.path, new.clone());
        t.reset_panes();
        if let Some((cursor, scroll)) = t.hist_state.get(pos).copied().flatten() {
            t.pending_cursor = Some(cursor..cursor);
            t.editor_scroll.force = Some(scroll);
        }
        Some((a, old, new))
    }

    /// Remove tab `i`; the neighbour becomes active. Returns its path.
    pub fn close(&mut self, i: usize) -> Option<PathBuf> {
        if i >= self.tabs.len() {
            return None;
        }
        let t = self.tabs.remove(i);
        if !t.is_empty() && !self.is_untitled(&t.path) {
            self.closed.push(t.path.clone());
            if self.closed.len() > 30 {
                self.closed.remove(0);
            }
        }
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(i.min(self.tabs.len() - 1)),
            other => other,
        };
        Some(t.path)
    }

    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.active_tab().map(|t| t.id);
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        if let Some(id) = active_id {
            self.active = self.tabs.iter().position(|t| t.id == id);
        }
    }

    pub fn rename_path(&mut self, from: &Path, to: &Path) {
        for t in &mut self.tabs {
            for h in t.history.iter_mut() {
                if h.starts_with(from) {
                    *h = to.join(h.strip_prefix(from).unwrap_or(Path::new("")));
                }
            }
            if t.path.starts_with(from) {
                t.path = to.join(t.path.strip_prefix(from).unwrap_or(Path::new("")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn replace_new_focus_history() {
        let mut t = Tabs::default();
        let o = t.open(&p("a"), false, true, ViewMode::Split);
        assert!(o.created);
        let o = t.open(&p("b"), false, true, ViewMode::Split);
        assert_eq!(o.released, Some(p("a")));
        assert_eq!(t.tabs.len(), 1);
        let o = t.open(&p("c"), true, true, ViewMode::Split);
        assert!(o.created);
        assert_eq!(t.tabs.len(), 2);
        assert_eq!(t.open(&p("b"), false, true, ViewMode::Split).index, 0);
        assert_eq!(t.active, Some(0));
        let (_, old, new) = t.go_history(true).unwrap();
        assert_eq!((old, new), (p("b"), p("a")));
        assert!(t.go_history(true).is_none());
        assert!(t.go_history(false).is_some());
    }

    #[test]
    fn untitled_tabs_are_taken_over_on_request_and_never_remembered() {
        let dir = p("/data/untitled");
        let mut t = Tabs::new(dir.clone());
        let u1 = dir.join("Untitled-1.md");
        assert!(t.is_untitled(&u1) && !t.is_untitled(&p("/x/Untitled-1.md")));
        t.open(&p("a"), false, true, ViewMode::Split);
        let i = t.new_untitled(u1.clone(), ViewMode::Split);
        assert_eq!((i, t.tabs.len(), t.active), (1, 2, Some(1)));
        // `take_over` reuses the active tab whatever the other flags say.
        let o = t.open_ex(&p("b"), true, false, ViewMode::Source, true);
        assert_eq!((o.index, o.created, o.released), (1, false, Some(u1.clone())));
        assert_eq!(t.tabs.len(), 2);
        assert_eq!(t.tabs[1].history, vec![p("b")]);
        // Without it an untitled tab is not replaced and a new tab is created.
        t.new_untitled(u1.clone(), ViewMode::Split);
        let o = t.open_ex(&p("c"), false, false, ViewMode::Source, false);
        assert!(o.created);
        // Closing untitled tabs is not remembered.
        let before = t.closed.len();
        t.close(2);
        assert_eq!(t.closed.len(), before);
        t.close(1);
        assert_eq!(t.closed.len(), before + 1);
    }

    #[test]
    fn empty_tabs_are_taken_over_and_never_remembered() {
        let mut t = Tabs::default();
        assert!(t.ensure_one() && !t.ensure_one());
        assert!(t.active_is_empty() && t.active_path().is_none() && t.tabs[0].history.is_empty());
        // Opening a file overwrites the empty tab, even when a new tab was asked for.
        let o = t.open(&p("a"), true, false, ViewMode::Source);
        assert_eq!((o.index, o.created, t.tabs.len()), (0, false, 1));
        assert_eq!(t.tabs[0].history, vec![p("a")]);
        // A new empty tab goes right after the active one.
        t.open(&p("b"), true, true, ViewMode::Split);
        t.active = Some(0);
        assert_eq!(t.new_empty(), 1);
        assert_eq!((t.active, t.tabs.len()), (Some(1), 3));
        // Closing an empty tab is not remembered.
        let before = t.closed.len();
        t.close(1);
        assert_eq!(t.closed.len(), before);
        t.close(0);
        t.close(0);
        assert!(t.tabs.is_empty() && t.active.is_none());
        assert!(t.ensure_one());
    }

    #[test]
    fn close_activates_neighbour() {
        let mut t = Tabs::default();
        for n in ["a", "b", "c"] {
            t.open(&p(n), true, true, ViewMode::Split);
        }
        t.active = Some(1);
        t.close(1);
        assert_eq!(t.active, Some(1));
        t.close(1);
        assert_eq!(t.active, Some(0));
        t.close(0);
        assert_eq!(t.active, None);
        assert_eq!(t.closed.len(), 3);
    }
}
