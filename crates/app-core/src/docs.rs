//! Open documents: one `Doc` per path with a text buffer, version counter and save state.
use crate::editor::buffer::LineIndex;
use crate::editor::history::History;
use crate::editor::selection::Selections;
use crate::editor::transaction::{EditError, Origin, Transaction};
use crate::platform::Waker;
use crate::saver::{self, SaveDone, SaveJob, Saver, hash_text};
use iron_sheets_core::FileKind;
use iron_sheets_core::textio::{self, LineEnding};
use crossbeam_channel::TryRecvError;
use rustc_hash::{FxHashMap, FxHashSet};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// Result of `Doc::apply`, `undo` and `redo`.
pub struct Applied {
    /// Document version after the change.
    pub version: u64,
    /// For undo/redo: the selections to restore, if the history knew them.
    pub select: Option<Selections>,
}

static HIGHLIGHT_BIG_PARTIAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Setting `highlight_big_partial`: notes above the highlight limit get the part around the viewport highlighted
/// (otherwise plain text). Read by the editor.
pub fn highlight_big_partial() -> bool {
    HIGHLIGHT_BIG_PARTIAL.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_highlight_big_partial(on: bool) {
    HIGHLIGHT_BIG_PARTIAL.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Document versions start at a value no other document (and no earlier load of the same file) has had, so a
/// `(path, version)` pair identifies one text state for the whole run: caches that key on it (preview parse, outline,
/// find, Typst) cannot mistake a reopened note for the one they saw before.
static VERSION_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn fresh_version() -> u64 {
    VERSION_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) << 24
}

pub(crate) const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// Words (`split_whitespace`) and chars of `s`.
pub fn count_text(s: &str) -> (usize, usize) {
    count_text_raw(s)
}

pub(crate) fn count_text_raw(text: &str) -> (usize, usize) {
    (text.split_whitespace().count(), text.chars().count())
}

/// `a..b` around bytes `start..end`: from the line start before `start` to the line break at or after `end`. A line
/// break separates words, so counts of the text outside this window do not depend on what is inside it.
fn line_window(text: &str, start: usize, end: usize) -> (usize, usize) {
    (text[..start].rfind('\n').map_or(0, |i| i + 1), text[end..].find('\n').map_or(text.len(), |i| end + i))
}

/// Exact change of (words, chars) when `old` became `new`, given that they share the first `p` bytes and the last `s`
/// bytes (char boundaries in both). Counts only the lines around the edit.
pub fn count_delta(old: &str, new: &str, p: usize, s: usize) -> (isize, isize) {
    let (a, b) = line_window(old, p, old.len() - s);
    let e = (b + new.len()).wrapping_sub(old.len());
    let (ow, oc) = count_text(&old[a..b]);
    let (nw, nc) = count_text(&new[a..e]);
    (nw as isize - ow as isize, nc as isize - oc as isize)
}

/// Word and char counts, valid for the text of document version `version`.
#[derive(Clone, Copy, Default)]
struct Counts {
    version: u64,
    words: usize,
    chars: usize,
    /// How many times the whole text was counted (diagnostics and tests).
    full_runs: usize,
}

pub struct Doc {
    pub path: PathBuf,
    pub kind: FileKind,
    /// Always LF; `line_ending` and `bom` are restored on save.
    pub text: String,
    /// Bumped on every change of `text`.
    pub version: u64,
    pub saved_version: u64,
    /// Length and hash of the text at `saved_version`: an edit that restores it makes the note clean again.
    saved_len: usize,
    saved_hash: u64,
    pub line_ending: LineEnding,
    pub bom: bool,
    /// Loaded lossily (invalid UTF-8): editing is disabled so saving cannot corrupt the file.
    pub read_only: bool,
    pub disk_mtime: Option<SystemTime>,
    pub last_edit: Instant,
    /// Last version sent to the indexer.
    pub indexed_version: u64,
    /// The file changed on disk while the buffer has unsaved edits.
    pub external_change: bool,
    /// The file was removed on disk while the note is open: autosave must not silently recreate it (IS-23).
    pub deleted: bool,
    pub save_error: Option<String>,
    /// Time of the last failed save, for autosave back-off.
    pub last_save_fail: Option<Instant>,
    /// Kept up to date from the edit range where possible; recounted in full when a change did not report one.
    counts: std::cell::Cell<Counts>,
    /// Line starts, valid for the document version stored with them; built by the first `line_index`, then
    /// kept in step by `apply`. A change behind the doc's back (`touch`) makes it stale and it is rebuilt on demand.
    lines: (u64, LineIndex),
    pub(crate) history: History,
    /// Document version the history belongs to: another version means the text changed without it (`touch`).
    history_at: u64,
}

impl Doc {
    /// (words, chars) of the text. O(1) when `apply_edit` or `add_count_delta` kept the counts current, else a full count.
    pub fn counts(&self) -> (usize, usize) {
        let c = self.counts.get();
        if c.version == self.version {
            return (c.words, c.chars);
        }
        let (words, chars) = count_text(&self.text);
        self.counts.set(Counts { version: self.version, words, chars, full_runs: c.full_runs + 1 });
        (words, chars)
    }

    /// The text changed from version `from` (the current one is the result) by `delta` words/chars, as `count_delta`
    /// computes it. Ignored (a full recount follows) when the counts were not current for `from`.
    pub fn add_count_delta(&self, from: u64, delta: (isize, isize)) {
        let c = self.counts.get();
        if c.version == from {
            let (words, chars) = (c.words.saturating_add_signed(delta.0), c.chars.saturating_add_signed(delta.1));
            self.counts.set(Counts { version: self.version, words, chars, ..c });
        }
    }

    /// How many times `counts` counted the whole text of this document.
    pub fn full_count_runs(&self) -> usize {
        self.counts.get().full_runs
    }

    pub(crate) fn seed_counts(&self, counts: (usize, usize)) {
        let c = self.counts.get();
        self.counts.set(Counts { version: self.version, words: counts.0, chars: counts.1, full_runs: c.full_runs });
    }

    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }

    /// A clean note has nothing to write: a new version makes it count as unsaved, so `save` recreates a deleted file.
    pub fn mark_unsaved_if_clean(&mut self) {
        if !self.is_dirty() {
            self.version += 1;
        }
    }

    pub fn mark_unsaved_if_clean_when_deleted(&mut self) {
        if self.deleted {
            self.mark_unsaved_if_clean();
        }
    }

    /// Autosave must wait for the user: the file changed or vanished on disk.
    pub fn autosave_blocked(&self) -> bool {
        self.external_change || self.deleted
    }

    /// Line starts of the current text.
    pub fn line_index(&mut self) -> &LineIndex {
        if self.lines.0 != self.version {
            self.lines = (self.version, LineIndex::new(&self.text));
        }
        &self.lines.1
    }

    /// Replace `range` (bytes, on char boundaries) with `insert`. Returns false when the range is invalid.
    pub fn apply_edit(&mut self, range: Range<usize>, insert: &str) -> bool {
        self.apply(Transaction::one(range, insert, Origin::Command)).is_ok()
    }

    /// The one door for text changes: validates, applies all edits in one pass, keeps line index, counts, dirty state and
    /// history current. Nothing changes on `Err`.
    pub fn apply(&mut self, tx: Transaction) -> Result<Applied, EditError> {
        self.apply_at(tx, None, Instant::now())
    }

    /// `apply` with the selections from before the edit (restored by undo) and an explicit clock.
    pub fn apply_at(&mut self, tx: Transaction, before: Option<Selections>, now: Instant) -> Result<Applied, EditError> {
        if self.read_only {
            return Err(EditError::ReadOnly);
        }
        let from = self.version;
        let inverse = self.apply_raw(&tx)?;
        let noop = tx.edits.iter().all(|e| e.range.is_empty() && e.insert.is_empty());
        if noop {
            if self.history_at == from {
                self.history_at = self.version;
            }
            return Ok(Applied { version: self.version, select: None });
        }
        if self.history_at != from {
            self.history.clear(); // the text changed behind the history's back
        }
        self.history.record(&tx, inverse, before, now);
        self.history_at = self.version;
        Ok(Applied { version: self.version, select: None })
    }

    /// The next typing edit starts a new undo group.
    pub fn close_history_group(&mut self) {
        self.history.close_group();
    }

    pub fn undo(&mut self) -> Option<Applied> {
        self.step_history(false)
    }

    pub fn redo(&mut self) -> Option<Applied> {
        self.step_history(true)
    }

    fn step_history(&mut self, redo: bool) -> Option<Applied> {
        if self.read_only {
            return None;
        }
        if self.history_at != self.version {
            self.history.clear();
            return None;
        }
        let mut history = std::mem::take(&mut self.history);
        let select = history.step(redo, Instant::now(), |tx| self.apply_raw(tx).ok());
        self.history = history;
        self.history_at = self.version;
        Some(Applied { version: self.version, select: select? })
    }

    /// Apply the edits to the text, counts and (when current) the line index; returns the inverse. No history.
    fn apply_raw(&mut self, tx: &Transaction) -> Result<Transaction, EditError> {
        tx.validate(&self.text)?;
        let (Some(first), Some(last)) = (tx.edits.first(), tx.edits.last()) else {
            return Ok(tx.invert(&self.text));
        };
        let new_len = self.text.len() as isize + tx.delta();
        if new_len > u32::MAX as isize {
            return Err(EditError::TooLarge);
        }
        let inverse = tx.invert(&self.text);
        let (start, end) = (first.range.start, last.range.end);
        let from = self.version;
        let base = self.counts.get();
        let window = (base.version == from).then(|| line_window(&self.text, start, end));
        let before = window.map(|(a, b)| count_text(&self.text[a..b]));
        let index_current = self.lines.0 == from;
        // shortcut: many edits rebuild the string in one pass (O(text)); few edits memmove the tail each (O(text) each).
        if tx.edits.len() > 4 {
            let mut out = String::with_capacity(new_len as usize);
            let mut at = 0;
            for e in &tx.edits {
                out.push_str(&self.text[at..e.range.start]);
                out.push_str(&e.insert);
                at = e.range.end;
            }
            out.push_str(&self.text[at..]);
            self.text = out;
        } else {
            for e in tx.edits.iter().rev() {
                self.text.replace_range(e.range.clone(), &e.insert);
            }
        }
        self.touch();
        if let (Some((a, b)), Some((ow, oc))) = (window, before) {
            let e = (b as isize + tx.delta()) as usize;
            let (nw, nc) = count_text(&self.text[a..e]);
            self.add_count_delta(from, (nw as isize - ow as isize, nc as isize - oc as isize));
        }
        // A stale index stays stale: `line_index` rebuilds it when somebody asks.
        if index_current {
            self.lines.1.apply(&tx.edits, &self.text);
            self.lines.0 = self.version;
        }
        Ok(inverse)
    }

    /// Record that `text` was changed from outside `apply_edit` (e.g. by the editor widget).
    pub fn touch(&mut self) {
        self.version += 1;
        // Hashing only when the length matches keeps ordinary typing free; an edit back to the saved text is clean.
        if self.text.len() == self.saved_len && hash_text(&self.text) == self.saved_hash {
            self.saved_version = self.version;
        }
        self.last_edit = Instant::now();
    }

    /// The current text is what is on disk.
    pub(crate) fn mark_saved(&mut self) {
        self.saved_version = self.version;
        self.saved_len = self.text.len();
        self.saved_hash = hash_text(&self.text);
    }

    /// A write of the text of `version` (`len` bytes, hashed `hash`) finished. The note becomes clean only when it was
    /// not edited meanwhile; either way the disk state is remembered, so editing back to it counts as clean.
    fn mark_written(&mut self, version: u64, len: usize, hash: u64) {
        self.saved_len = len;
        self.saved_hash = hash;
        if self.version == version || (self.text.len() == len && hash_text(&self.text) == hash) {
            self.saved_version = self.version;
        } else if self.saved_version == self.version {
            // `touch` judged the text clean against the older disk state (undo during the write): it is not.
            self.saved_version = self.version.wrapping_sub(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn scratch(text: &str) -> Doc {
        Doc::from_loaded(
            Path::new("n.md"),
            FileKind::Markdown,
            textio::LoadedText { text: text.into(), line_ending: LineEnding::Lf, bom: false, lossy: false, mtime: None },
        )
    }

    fn empty_with_mtime(path: &Path, kind: FileKind, mtime: Option<SystemTime>) -> Doc {
        let d = Doc::from_loaded(
            path,
            kind,
            textio::LoadedText { text: String::new(), line_ending: LineEnding::Lf, bom: false, lossy: false, mtime },
        );
        d.seed_counts((0, 0));
        d
    }

    fn from_loaded(path: &Path, kind: FileKind, l: textio::LoadedText) -> Doc {
        let version = fresh_version();
        let mut d = Doc {
            path: path.to_path_buf(),
            kind,
            text: l.text,
            version,
            saved_version: version,
            saved_len: 0,
            saved_hash: 0,
            line_ending: l.line_ending,
            bom: l.bom,
            read_only: l.lossy,
            disk_mtime: l.mtime,
            last_edit: Instant::now(),
            indexed_version: version,
            external_change: false,
            deleted: false,
            save_error: None,
            last_save_fail: None,
            counts: Default::default(),
            lines: (0, LineIndex::new("")),
            history: History::default(),
            history_at: version,
        };
        d.mark_saved();
        d
    }
}

#[derive(Default)]
pub struct DocStore {
    docs: FxHashMap<PathBuf, Doc>,
    /// Folder of untitled notes (empty: none). A missing file there is an empty document, not an error.
    pub untitled_dir: PathBuf,
    /// Number of files read from disk (diagnostics and tests).
    pub loads: usize,
    /// Of those, reads done synchronously by `open` (on the calling thread); tests assert this stays 0 on the UI path.
    pub sync_loads: usize,
    /// Background reads of notes the user is about to open (hover, quick open).
    pub prefetch: crate::prefetch::Prefetch,
    saver: Saver,
    /// Paths with a write on the worker.
    saving: FxHashMap<PathBuf, InFlight>,
    waker: Option<Arc<dyn Waker>>,
    /// Paths with a change event while their write was in flight: checked once the result is in.
    recheck: parking_lot::Mutex<FxHashSet<PathBuf>>,
    finished: Vec<SaveFinished>,
}

#[derive(Default)]
struct InFlight {
    announce: bool,
    /// A save requested meanwhile: written once the current write is done. Further requests merge into it.
    next: Option<Next>,
    /// When the write was handed to the worker (for a status cue that waits before it shows).
    started: Option<Instant>,
}

#[derive(Default)]
struct Next {
    force: bool,
    announce: bool,
}

/// A background save that completed (see `DocStore::poll_saves`).
pub struct SaveFinished {
    pub path: PathBuf,
    /// The save was an explicit one: tell the user.
    pub announce: bool,
    /// The error text when the write failed.
    pub error: Option<String>,
    /// The file changed on disk meanwhile and the (clean) note was reloaded.
    pub reloaded: bool,
}

impl DocStore {
    pub fn new(untitled_dir: PathBuf) -> DocStore {
        DocStore { untitled_dir, ..Default::default() }
    }

    pub fn is_untitled(&self, path: &Path) -> bool {
        iron_sheets_core::untitled::number_of(&self.untitled_dir, path).is_some()
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn get(&self, path: &Path) -> Option<&Doc> {
        self.docs.get(path)
    }

    pub fn get_mut(&mut self, path: &Path) -> Option<&mut Doc> {
        self.docs.get_mut(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Doc> {
        self.docs.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Doc> {
        self.docs.values_mut()
    }

    pub fn dirty_paths(&self) -> Vec<PathBuf> {
        self.docs.values().filter(|d| d.is_dirty()).map(|d| d.path.clone()).collect()
    }

    /// The document for `path`, read synchronously from disk when not yet open.
    pub fn open(&mut self, path: &Path) -> io::Result<&mut Doc> {
        if !self.docs.contains_key(path) {
            let kind = FileKind::from_path(path);
            let doc = if kind == FileKind::Image {
                let meta = std::fs::metadata(path)?;
                self.prefetch.invalidate(path);
                Doc::empty_with_mtime(path, kind, meta.modified().ok())
            } else if let Some((loaded, counts)) = self.prefetch.take_with_counts(path) {
                self.loads += 1;
                let d = Doc::from_loaded(path, kind, loaded);
                d.seed_counts(counts);
                d
            } else {
                self.prefetch.invalidate(path);
                match textio::load_text_bounded(path, MAX_TEXT_BYTES) {
                    Ok(loaded) => {
                        self.loads += 1;
                        self.sync_loads += 1;
                        Doc::from_loaded(path, kind, loaded)
                    }
                    // An untitled note that was never written (it was empty) is an empty document.
                    Err(textio::BoundedLoadError::Io(e)) if e.kind() == io::ErrorKind::NotFound && self.is_untitled(path) => {
                        Doc::empty_with_mtime(path, kind, None)
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            self.docs.insert(path.to_path_buf(), doc);
        }
        match self.docs.get_mut(path) {
            Some(d) => Ok(d),
            None => Err(io::Error::other("document vanished")),
        }
    }

    /// Installs a document already loaded in the background via `Prefetch::request_open` (or hover-prefetch).
    /// Returns `None` if no completed result is ready yet, performing zero UI-thread file reads.
    pub fn take_open(&mut self, path: &Path) -> Option<io::Result<&mut Doc>> {
        if self.docs.contains_key(path) {
            return Some(self.docs.get_mut(path).ok_or_else(|| io::Error::other("document vanished")));
        }
        let res = self.prefetch.take_open_with_counts(path)?;
        let kind = FileKind::from_path(path);
        let doc = match res {
            Ok((loaded, counts)) => {
                if kind != FileKind::Image {
                    self.loads += 1;
                }
                let d = Doc::from_loaded(path, kind, loaded);
                d.seed_counts(counts);
                d
            }
            Err(e) if kind != FileKind::Image && e.kind() == io::ErrorKind::NotFound && self.is_untitled(path) => {
                Doc::empty_with_mtime(path, kind, None)
            }
            Err(e) => return Some(Err(e)),
        };
        self.docs.insert(path.to_path_buf(), doc);
        Some(self.docs.get_mut(path).ok_or_else(|| io::Error::other("document vanished")))
    }

    /// What `save` would write now; `None` for clean, read-only and image documents.
    fn snapshot(&self, path: &Path) -> Option<SaveJob> {
        let doc = self.docs.get(path)?;
        if !doc.is_dirty() || doc.read_only || doc.kind == FileKind::Image {
            return None;
        }
        let untitled = self.is_untitled(path);
        Some(SaveJob {
            path: path.to_path_buf(),
            text: doc.text.clone(),
            ending: doc.line_ending,
            bom: doc.bom,
            version: doc.version,
            mkdir: untitled,
            remove: untitled && doc.text.is_empty(),
        })
    }

    /// Records the outcome of a write in the document.
    fn apply_done(&mut self, d: SaveDone) -> io::Result<()> {
        let Some(doc) = self.docs.get_mut(&d.path) else { return d.result.map(|_| ()) };
        match d.result {
            Ok(mtime) => {
                doc.disk_mtime = mtime;
                doc.mark_written(d.version, d.len, d.hash);
                doc.external_change = false;
                doc.deleted = false;
                doc.save_error = None;
                doc.last_save_fail = None;
                Ok(())
            }
            Err(e) => {
                doc.save_error = Some(e.to_string());
                doc.last_save_fail = Some(Instant::now());
                Err(e)
            }
        }
    }

    /// Takes in a result from the worker and starts the write that was requested while it ran.
    fn finish_background(&mut self, d: SaveDone) {
        let path = d.path.clone();
        let flight = self.saving.remove(&path).unwrap_or_default();
        let error = self.apply_done(d).err().map(|e| e.to_string());
        let mut reloaded = false;
        if self.recheck.lock().remove(&path) && self.changed_on_disk(&path) {
            // Somebody else wrote the file after us.
            if self.docs.get(&path).is_some_and(|d| d.is_dirty()) {
                if let Some(d) = self.docs.get_mut(&path) {
                    d.external_change = true;
                }
            } else {
                reloaded = self.reload(&path).is_ok();
            }
        }
        self.finished.push(SaveFinished { path: path.clone(), announce: flight.announce, error: error.clone(), reloaded });
        if let (None, Some(next), Some(w)) = (error, flight.next, self.waker.clone()) {
            // A file that changed on disk meanwhile is only overwritten by an explicit save.
            if next.force || !self.docs.get(&path).is_some_and(Doc::autosave_blocked) {
                self.save_async(&path, next.force, next.announce, &w);
            }
        }
    }

    /// Writes the buffer on the background worker and returns at once; `force` also overwrites a file that changed
    /// on disk. A write of the same path in flight is followed by one more for the then-current text. Results come
    /// through `poll_saves`.
    pub fn save_async(&mut self, path: &Path, force: bool, announce: bool, waker: &Arc<dyn Waker>) {
        if let Some(flight) = self.saving.get_mut(path) {
            let next = flight.next.get_or_insert(Next::default());
            next.force |= force;
            next.announce |= announce;
            return;
        }
        let Some(job) = self.snapshot(path) else { return };
        self.waker = Some(waker.clone());
        match self.saver.send(job, waker) {
            Ok(()) => {
                self.saving.insert(path.to_path_buf(), InFlight { announce, next: None, started: Some(Instant::now()) });
            }
            // shortcut: no thread available (resource exhaustion) means a write on the UI thread.
            Err(job) => {
                let error = self.apply_done(saver::run(job)).err().map(|e| e.to_string());
                self.finished.push(SaveFinished { path: path.to_path_buf(), announce, error, reloaded: false });
            }
        }
    }

    /// Applies finished background saves and returns the ones to report (index update, status message).
    pub fn poll_saves(&mut self) -> Vec<SaveFinished> {
        loop {
            match self.saver.try_recv() {
                Ok(d) => self.finish_background(d),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.reset_saver();
                    break;
                }
            }
        }
        std::mem::take(&mut self.finished)
    }

    /// True while a write of `path` is on the worker.
    pub fn is_saving(&self, path: &Path) -> bool {
        self.saving.contains_key(path)
    }

    /// How long the write of `path` has been on the worker, if one is.
    pub fn saving_for(&self, path: &Path) -> Option<std::time::Duration> {
        self.saving.get(path).map(|f| f.started.map_or(std::time::Duration::ZERO, |t| t.elapsed()))
    }

    /// The worker is gone (panic): forget its writes; the next save starts a new one.
    fn reset_saver(&mut self) {
        self.saver = Saver::default();
        self.saving.clear();
    }

    /// Waits until no write of `path` or of anything below it is in flight (also drops their follow-up saves: the
    /// caller is about to write, move or delete). Call before a file operation on the path.
    pub fn wait_saved_under(&mut self, path: &Path) {
        for (p, flight) in self.saving.iter_mut() {
            if p.starts_with(path) {
                flight.next = None;
            }
        }
        while self.saving.keys().any(|p| p.starts_with(path)) {
            match self.saver.recv() {
                Some(d) => self.finish_background(d),
                None => self.reset_saver(),
            }
        }
    }

    /// Write the buffer and wait for it. No-op for clean, read-only and image documents.
    pub fn save(&mut self, path: &Path) -> io::Result<bool> {
        self.wait_saved_under(path);
        let Some(job) = self.snapshot(path) else { return Ok(false) };
        self.apply_done(saver::run(job)).map(|()| true)
    }

    /// Replace the buffer with the disk contents (discarding edits).
    pub fn reload(&mut self, path: &Path) -> io::Result<()> {
        let Some(doc) = self.docs.get_mut(path) else { return Ok(()) };
        self.prefetch.invalidate(path);
        if doc.kind == FileKind::Image {
            doc.disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            return Ok(());
        }
        let l = textio::load_text_bounded(path, MAX_TEXT_BYTES)?;
        self.loads += 1;
        doc.text = l.text;
        doc.line_ending = l.line_ending;
        doc.bom = l.bom;
        doc.read_only = l.lossy;
        doc.disk_mtime = l.mtime;
        doc.version += 1;
        doc.lines.0 = 0;
        doc.history.clear();
        doc.mark_saved();
        doc.last_edit = Instant::now();
        doc.external_change = false;
        doc.deleted = false;
        doc.save_error = None;
        Ok(())
    }

    /// True when the file's mtime differs from what we last read or wrote.
    pub fn changed_on_disk(&self, path: &Path) -> bool {
        // Our own write in flight: its result brings the new mtime.
        if self.saving.contains_key(path) {
            self.recheck.lock().insert(path.to_path_buf());
            return false;
        }
        let Some(doc) = self.docs.get(path) else { return false };
        match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(m) => doc.disk_mtime != Some(m),
            Err(_) => false,
        }
    }

    /// Drop the document when no tab uses it and it is clean.
    pub fn close_if_unused(&mut self, path: &Path, used: impl Fn(&Path) -> bool) {
        if used(path) {
            return;
        }
        if self.docs.get(path).is_some_and(|d| !d.is_dirty()) {
            self.docs.remove(path);
        }
    }

    pub fn remove(&mut self, path: &Path) {
        self.wait_saved_under(path);
        self.docs.remove(path);
        self.prefetch.forget(path);
    }

    /// Re-key a document after a rename on disk.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        let keys: Vec<PathBuf> = self.docs.keys().filter(|k| k.starts_with(from)).cloned().collect();
        for k in keys {
            if let Some(mut d) = self.docs.remove(&k) {
                let new = if k == from { to.to_path_buf() } else { to.join(k.strip_prefix(from).unwrap_or(Path::new(""))) };
                d.path = new.clone();
                d.kind = FileKind::from_path(&new);
                self.docs.insert(new, d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_never_repeat_across_documents_or_reopens() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a.md"), t.path().join("b.md"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let mut s = DocStore::default();
        let va = s.open(&a).unwrap().version;
        let vb = s.open(&b).unwrap().version;
        assert_ne!(va, vb);
        s.get_mut(&a).unwrap().touch();
        s.remove(&a);
        let again = s.open(&a).unwrap();
        assert!(again.version != va && again.version != va + 1, "a reopened note must not look like the earlier state");
        assert!(!again.is_dirty());
    }

    #[test]
    fn editing_back_to_the_saved_text_is_clean() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, "hello\nworld").unwrap();
        let mut s = DocStore::default();
        let d = s.open(&p).unwrap();
        assert!(d.apply_edit(5..5, "!") && d.is_dirty());
        assert!(d.apply_edit(5..6, "") && !d.is_dirty(), "typed and deleted: back to the saved text");
        // Same length, different text: still dirty; restored: clean.
        assert!(d.apply_edit(0..1, "J") && d.is_dirty());
        assert!(d.apply_edit(0..1, "h") && !d.is_dirty());
        // After a save the saved text is the new one.
        assert!(d.apply_edit(0..0, "x") && d.is_dirty());
        assert!(s.save(&p).unwrap());
        let d = s.get_mut(&p).unwrap();
        assert!(d.apply_edit(0..1, "") && d.is_dirty());
        assert!(d.apply_edit(0..0, "x") && !d.is_dirty());
    }

    #[test]
    #[ignore = "timing probe: cargo test -p iron-sheets-app-core --release -- --ignored --nocapture"]
    fn hash_cost_on_5mb() {
        let text = "lorem ipsum dolor sit amet\n".repeat(190_000);
        let t = std::time::Instant::now();
        let h = (0..20).map(|_| hash_text(&text)).fold(0, |a, b| a ^ b);
        eprintln!("{} bytes: {:?} per hash ({h})", text.len(), t.elapsed() / 20);
    }

    #[test]
    fn open_edit_save_reload() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, b"\xEF\xBB\xBFone\r\ntwo").unwrap();
        let mut s = DocStore::default();
        let d = s.open(&p).unwrap();
        assert_eq!(d.text, "one\ntwo");
        assert!(!d.is_dirty());
        assert!(d.apply_edit(3..3, "!"));
        assert!(d.is_dirty());
        assert!(!d.apply_edit(100..101, "x"));
        assert!(s.save(&p).unwrap());
        assert_eq!(std::fs::read(&p).unwrap(), b"\xEF\xBB\xBFone!\r\ntwo");
        assert!(!s.changed_on_disk(&p));
        std::fs::write(&p, "other").unwrap();
        s.reload(&p).unwrap();
        assert_eq!(s.get(&p).unwrap().text, "other");
        assert_eq!(s.loads, 2);
        s.close_if_unused(&p, |_| false);
        assert!(s.is_empty());
    }

    #[test]
    fn refuses_binary() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("b.md");
        std::fs::write(&p, b"ab\0cd").unwrap();
        assert!(DocStore::default().open(&p).is_err());
    }

    #[test]
    fn folder_rename_rekeys_docs() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("Notes");
        std::fs::create_dir(&a).unwrap();
        let p = a.join("a.md");
        std::fs::write(&p, "x").unwrap();
        let mut s = DocStore::default();
        s.open(&p).unwrap().apply_edit(0..0, "y");
        let to = t.path().join("Notes2");
        s.rename(&a, &to);
        let d = s.get(&to.join("a.md")).unwrap();
        assert!(d.is_dirty() && d.text == "yx" && d.path == to.join("a.md"));
        assert!(s.get(&p).is_none());
    }

    #[test]
    fn untitled_notes_are_empty_without_a_file_and_have_no_file_while_empty() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("untitled");
        let p = iron_sheets_core::untitled::path_for(&dir, 1);
        let mut s = DocStore::new(dir.clone());
        // Missing file (and missing folder): an empty document, nothing is created.
        assert_eq!(s.open(&p).unwrap().text, "");
        assert!(!dir.exists() && s.loads == 0);
        // A missing file elsewhere is still an error.
        assert!(s.open(&t.path().join("nope.md")).is_err());
        // Text: the first save creates the folder and the file.
        s.get_mut(&p).unwrap().apply_edit(0..0, "hi");
        assert!(s.save(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi");
        // Emptied again: saving removes the file.
        s.get_mut(&p).unwrap().apply_edit(0..2, "");
        assert!(s.save(&p).unwrap());
        assert!(!p.exists());
        assert!(!s.get(&p).unwrap().is_dirty());
    }

    /// Tiny deterministic generator (no rand dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    fn piece(r: &mut Lcg) -> String {
        const BITS: [&str; 14] = ["a", "word", " ", "  ", "\n", "\n\n", "é", "字", "\u{a0}", "\u{2003}", "\t", "x y", "\r", "ab\ncd"];
        (0..r.next(6)).map(|_| BITS[r.next(BITS.len())]).collect()
    }

    fn boundary(t: &str, mut i: usize) -> usize {
        while !t.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    #[test]
    fn counts_follow_random_edits_exactly() {
        for seed in 0..60 {
            let mut r = Lcg(seed + 1);
            let mut doc = Doc::from_loaded(
                Path::new("n.md"),
                FileKind::Markdown,
                textio::LoadedText {
                    text: (0..r.next(40)).map(|_| piece(&mut r)).collect(),
                    line_ending: LineEnding::Lf,
                    bom: false,
                    lossy: false,
                    mtime: None,
                },
            );
            assert_eq!(doc.counts(), count_text(&doc.text));
            for step in 0..200 {
                let a = boundary(&doc.text, r.next(doc.text.len() + 1));
                let b = boundary(&doc.text, (a + r.next(12)).min(doc.text.len()));
                let ins = piece(&mut r);
                if step % 3 == 0 {
                    // The path the editor widget takes: the text is changed behind the doc's back, the delta comes from `count_delta`.
                    let from = doc.version;
                    let old = doc.text.clone();
                    doc.text.replace_range(a..b, &ins);
                    doc.touch();
                    let (p, s) = iron_sheets_md::diff_texts(&old, &doc.text);
                    doc.add_count_delta(from, count_delta(&old, &doc.text, p, s));
                } else if step % 7 == 1 {
                    // A change that reports nothing (e.g. drag-and-drop in the editor), same length or not: a full recount, never a drift.
                    let ins = if r.next(2) == 0 { doc.text[a..b].chars().rev().collect() } else { ins };
                    doc.text.replace_range(a..b, &ins);
                    doc.touch();
                    assert_eq!(doc.counts(), count_text(&doc.text), "seed {seed} step {step}: unreported change");
                } else {
                    assert!(doc.apply_edit(a..b, &ins));
                }
                let c = doc.counts.get();
                assert_eq!(c.version, doc.version, "seed {seed} step {step}: counts were kept current");
                assert_eq!((c.words, c.chars), count_text(&doc.text), "seed {seed} step {step}");
            }
        }
    }

    #[test]
    fn counts_fall_back_to_a_full_count_when_stale() {
        let mut doc = Doc::from_loaded(
            Path::new("n.md"),
            FileKind::Markdown,
            textio::LoadedText { text: "a b".into(), line_ending: LineEnding::Lf, bom: false, lossy: false, mtime: None },
        );
        assert_eq!(doc.counts(), (2, 3));
        doc.text.push_str(" c");
        doc.touch();
        doc.add_count_delta(doc.version - 1, (1, 2));
        assert_eq!(doc.counts(), (3, 5));
        // A change nobody reported: the next count notices the new version.
        doc.text.push_str(" d");
        doc.touch();
        assert_eq!(doc.counts(), (4, 7));
    }

    #[test]
    fn open_and_reload_clear_stale_open_results() {
        struct NoopWaker;
        impl crate::platform::Waker for NoopWaker {
            fn wake(&self) {}
        }

        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("note.md");
        std::fs::write(&p, "v1").unwrap();

        let mut s = DocStore::default();
        let waker: std::sync::Arc<dyn crate::platform::Waker> = std::sync::Arc::new(NoopWaker);
        s.prefetch.request_open(&p, waker.clone());
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));

        std::fs::write(&p, "v2").unwrap();
        assert_eq!(s.open(&p).unwrap().text, "v2");
        assert!(s.prefetch.take_open(&p).is_none(), "synchronous open clears stale open_results");

        s.prefetch.request_open(&p, waker);
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        std::fs::write(&p, "v3").unwrap();
        s.reload(&p).unwrap();
        assert_eq!(s.get(&p).unwrap().text, "v3");
        assert!(s.prefetch.take_open(&p).is_none(), "reload clears stale open_results");
    }

    #[test]
    fn request_open_and_hover_prefetch_seed_counts_without_full_recount() {
        struct NoopWaker;
        impl crate::platform::Waker for NoopWaker {
            fn wake(&self) {}
        }

        let t = tempfile::tempdir().unwrap();
        let waker: std::sync::Arc<dyn crate::platform::Waker> = std::sync::Arc::new(NoopWaker);

        let p_open = t.path().join("open.md");
        std::fs::write(&p_open, "hello wörld\nsecond   line\twith\u{a0}tabs\n").unwrap();
        let mut s = DocStore::default();
        s.prefetch.request_open(&p_open, waker);
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        let doc = s.take_open(&p_open).expect("ready").expect("ok");
        assert_eq!(doc.full_count_runs(), 0);
        let expected = count_text(&doc.text);
        assert_eq!(doc.counts(), expected);
        assert_eq!(doc.full_count_runs(), 0, "Frame 1 doc.counts() must not run a full recount");
        assert!(doc.apply_edit(0..0, "extra "));
        assert_eq!(doc.counts(), count_text(&doc.text));
        assert_eq!(doc.full_count_runs(), 0);

        let p_hover = t.path().join("hover.md");
        std::fs::write(&p_hover, "one two three\nfour").unwrap();
        s.prefetch.request(&p_hover);
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        let doc = s.open(&p_hover).expect("ok");
        assert_eq!(doc.counts(), count_text(&doc.text));
        assert_eq!(doc.full_count_runs(), 0, "hover-prefetched open must also seed counts");
    }

    #[test]
    fn open_and_take_open_preserve_error_strings_and_lossy_read_only() {
        struct NoopWaker;
        impl crate::platform::Waker for NoopWaker {
            fn wake(&self) {}
        }

        let t = tempfile::tempdir().unwrap();
        let waker: std::sync::Arc<dyn crate::platform::Waker> = std::sync::Arc::new(NoopWaker);
        let mut s = DocStore::default();

        // Binary
        let bin = t.path().join("bin.md");
        std::fs::write(&bin, b"abc\0def").unwrap();
        assert_eq!(s.open(&bin).err().expect("binary err").to_string(), "binary file, not opened");
        s.prefetch.request_open(&bin, waker.clone());
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        assert_eq!(s.take_open(&bin).expect("ready").err().expect("binary err").to_string(), "binary file, not opened");

        // Oversized (> MAX_TEXT_BYTES)
        let big = t.path().join("big.md");
        std::fs::File::create(&big).unwrap().set_len(MAX_TEXT_BYTES + 1).unwrap();
        assert_eq!(s.open(&big).err().expect("big err").to_string(), "file is too large to open");
        s.prefetch.request_open(&big, waker.clone());
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        assert_eq!(s.take_open(&big).expect("ready").err().expect("big err").to_string(), "file is too large to open");

        // Invalid UTF-8 (without NUL in first 8 KiB) opens read-only lossily
        let bad_utf8 = t.path().join("bad_utf8.md");
        std::fs::write(&bad_utf8, b"hello \xFF world").unwrap();
        let sync_doc = s.open(&bad_utf8).expect("lossy opens read-only");
        assert!(sync_doc.read_only);
        assert_eq!(sync_doc.text, "hello \u{FFFD} world");

        let bad_utf8_async = t.path().join("bad_utf8_async.md");
        std::fs::write(&bad_utf8_async, b"hello \xFF async").unwrap();
        s.prefetch.request_open(&bad_utf8_async, waker.clone());
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        let async_doc = s.take_open(&bad_utf8_async).expect("ready").expect("lossy opens read-only");
        assert!(async_doc.read_only);
        assert_eq!(async_doc.text, "hello \u{FFFD} async");
        assert_eq!(async_doc.counts(), count_text(&async_doc.text));
        assert_eq!(async_doc.full_count_runs(), 0);

        // Missing
        let missing = t.path().join("missing.md");
        assert_eq!(s.open(&missing).err().expect("missing err").kind(), io::ErrorKind::NotFound);
        s.prefetch.request_open(&missing, waker);
        s.prefetch.wait_idle(std::time::Duration::from_secs(5));
        assert_eq!(s.take_open(&missing).expect("ready").err().expect("missing err").kind(), io::ErrorKind::NotFound);

        // Reload preserves errors too
        let rel = t.path().join("reload.md");
        std::fs::write(&rel, "ok").unwrap();
        s.open(&rel).unwrap();
        std::fs::write(&rel, b"bad\0bin").unwrap();
        assert_eq!(s.reload(&rel).unwrap_err().to_string(), "binary file, not opened");
        assert_eq!(s.get(&rel).unwrap().text, "ok");
        std::fs::File::create(&rel).unwrap().set_len(MAX_TEXT_BYTES + 1).unwrap();
        assert_eq!(s.reload(&rel).unwrap_err().to_string(), "file is too large to open");
        std::fs::write(&rel, b"lossy \xFF text").unwrap();
        s.reload(&rel).unwrap();
        assert!(s.get(&rel).unwrap().read_only);
    }
}
