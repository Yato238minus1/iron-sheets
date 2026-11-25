//! Reads (and parses) a note in the background before or while it is opened.
//!
//! Two paths share one background worker:
//! - [`Prefetch::request`] (hover / quick-open dwell): caps at 16 MiB and keeps an LRU cache (`take`).
//! - [`Prefetch::request_open`] (active-tab lazy load): immediate priority, supports up to [`crate::docs::MAX_TEXT_BYTES`]
//!   (64 MiB), records open errors with the same semantics as `DocStore::open`, and wakes the UI via `Waker`.
//!
//! Never blocks the caller: requests only enqueue the path and wake the worker.
use crate::platform::Waker;
use iron_sheets_core::FileKind;
use iron_sheets_core::textio::{self, LineEnding, LoadedText};
use iron_sheets_md::Document;
use parking_lot::{Condvar, Mutex};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Files bigger than this are not hover-prefetched (explicit open still loads up to `MAX_TEXT_BYTES`).
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// Cache caps: entries and bytes (text plus an estimate of the parse).
const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Parsed documents kept for the preview after the text was taken by `DocStore::open`.
const MAX_PARSES: usize = 4;
/// Completed active-tab opens kept waiting for `take_open`.
const MAX_OPEN_RESULTS: usize = 4;

struct Entry {
    path: PathBuf,
    loaded: LoadedText,
    counts: (usize, usize),
    size: u64,
    mtime: Option<SystemTime>,
    parse: Option<(u64, Arc<Document>)>,
    bytes: usize,
}

struct OpenEntry {
    loaded: LoadedText,
    counts: (usize, usize),
    parse: Option<(u64, Arc<Document>)>,
}

#[derive(Default)]
struct Queue {
    pending_open: Option<(PathBuf, Arc<dyn Waker>)>,
    in_flight_open: Option<PathBuf>,
    in_flight_open_invalidated: bool,
    pending_prefetch: Option<PathBuf>,
    in_flight_prefetch: Option<PathBuf>,
    in_flight_prefetch_invalidated: bool,
    open_paused: bool,
}

#[derive(Default)]
struct Shared {
    /// Queued work (`pending_open` has priority over `pending_prefetch`).
    queue: Mutex<Queue>,
    wake: Condvar,
    cache: Mutex<Cache>,
    closed: std::sync::atomic::AtomicBool,
    /// Files the worker is working on (0 or 1).
    busy: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct Cache {
    /// Oldest first.
    entries: Vec<Entry>,
    /// Completed `request_open` results (oldest first, capped at `MAX_OPEN_RESULTS`).
    open_results: Vec<(PathBuf, io::Result<OpenEntry>)>,
    /// Parses of notes whose text was taken: path, text length, text hash, parse.
    parses: Vec<(PathBuf, usize, u64, Arc<Document>)>,
    /// Paths `request_once` has asked for (so a cache that evicted one is not asked again forever).
    once: rustc_hash::FxHashSet<PathBuf>,
    /// Last path requested (a repeated request for it does nothing).
    last: Option<PathBuf>,
    /// Counters for tests and diagnostics.
    reads: usize,
    hits: usize,
}

pub struct Prefetch {
    shared: Arc<Shared>,
    started: std::sync::atomic::AtomicBool,
}

impl Default for Prefetch {
    fn default() -> Self {
        Prefetch { shared: Arc::new(Shared::default()), started: Default::default() }
    }
}

impl Drop for Prefetch {
    fn drop(&mut self) {
        // Under the lock the worker waits with: otherwise it can check `closed`, then miss this notify and sleep forever.
        let _guard = self.shared.queue.lock();
        self.shared.closed.store(true, std::sync::atomic::Ordering::Release);
        self.shared.wake.notify_all();
    }
}

pub fn text_hash(text: &str) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    text.hash(&mut h);
    h.finish()
}

fn size_estimate(l: &LoadedText, parsed: bool) -> usize {
    l.text.len() * if parsed { 3 } else { 1 } + 256
}

fn prefetchable(path: &Path) -> bool {
    !matches!(FileKind::from_path(path), FileKind::Image)
}

impl Prefetch {
    /// Starts reading `path` in the background (no-op when it was requested last, or is not a text note).
    pub fn request(&self, path: &Path) {
        if !prefetchable(path) {
            return;
        }
        {
            let mut c = self.shared.cache.lock();
            if c.last.as_deref() == Some(path) {
                return;
            }
            c.last = Some(path.to_path_buf());
        }
        self.start();
        self.shared.queue.lock().pending_prefetch = Some(path.to_path_buf());
        self.shared.wake.notify_one();
    }

    /// Like [`Self::request`], but each path only once (until it is invalidated). True when it was requested now.
    pub fn request_once(&self, path: &Path) -> bool {
        if !prefetchable(path) || !self.shared.cache.lock().once.insert(path.to_path_buf()) {
            return false;
        }
        self.request(path);
        true
    }

    /// True when nothing is queued or being read.
    pub fn is_idle(&self) -> bool {
        let q = self.shared.queue.lock();
        q.pending_open.is_none() && q.pending_prefetch.is_none() && self.idle()
    }

    /// Requests an immediate background open of `path` (up to `DocStore::MAX_TEXT_BYTES`, including images and error
    /// capture), waking `waker` once finished. No-op if already queued, in flight, or waiting in `open_results`.
    pub fn request_open(&self, path: &Path, waker: Arc<dyn Waker>) {
        if self.shared.cache.lock().open_results.iter().any(|(p, _)| p == path) {
            waker.wake();
            return;
        }
        {
            let mut q = self.shared.queue.lock();
            if q.in_flight_open.as_deref() == Some(path) && !q.in_flight_open_invalidated {
                return;
            }
            if let Some((p, w)) = q.pending_open.as_mut() {
                if p == path {
                    *w = waker;
                    return;
                }
            }
            q.pending_open = Some((path.to_path_buf(), waker.clone()));
        }
        if !self.start() {
            self.shared.queue.lock().pending_open = None;
            self.shared.cache.lock().push_open_result(path.to_path_buf(), Err(io::Error::other("cannot start background reader")));
            waker.wake();
            return;
        }
        self.shared.wake.notify_one();
    }

    /// Pause/resume processing of `request_open` jobs (for deterministic tests of the loading skeleton).
    pub fn set_open_paused(&self, paused: bool) {
        let mut q = self.shared.queue.lock();
        q.open_paused = paused;
        if !paused {
            self.shared.wake.notify_all();
        }
    }

    /// True when `path` is currently queued or being read by `request_open`.
    pub fn has_pending_open(&self, path: &Path) -> bool {
        let q = self.shared.queue.lock();
        (!q.in_flight_open_invalidated && q.in_flight_open.as_deref() == Some(path))
            || q.pending_open.as_ref().is_some_and(|(p, _)| p == path)
    }

    fn start(&self) -> bool {
        if self.started.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return true;
        }
        let sh = self.shared.clone();
        let spawned = std::thread::Builder::new().name("iron-sheets-prefetch".into()).spawn(move || worker(sh));
        if spawned.is_err() {
            self.started.store(false, std::sync::atomic::Ordering::Release);
            return false;
        }
        true
    }

    /// Takes the result of a finished `request_open` (or a valid hover-prefetch cache entry for `path`), moving any
    /// pre-parsed Markdown AST into the parse cache. Returns `None` when nothing is cached/ready (zero syscalls).
    pub fn take_open(&self, path: &Path) -> Option<io::Result<LoadedText>> {
        self.take_open_with_counts(path).map(|res| res.map(|(loaded, _)| loaded))
    }

    pub(crate) fn take_open_with_counts(&self, path: &Path) -> Option<io::Result<(LoadedText, (usize, usize))>> {
        let has_hover_entry = {
            let mut c = self.shared.cache.lock();
            if let Some(i) = c.open_results.iter().position(|(p, _)| p == path) {
                let (_, res) = c.open_results.remove(i);
                return Some(res.map(|e| {
                    c.record_taken(path, e.loaded.text.len(), e.parse);
                    (e.loaded, e.counts)
                }));
            }
            c.entries.iter().any(|e| e.path == path)
        };
        if has_hover_entry {
            return self.take_with_counts(path).map(Ok);
        }
        None
    }

    /// The cached text of `path`, if it was prefetched and the file still has the same modification time and size.
    /// The entry is removed (the text moves into the document); its parse stays available through [`Self::parsed`].
    pub fn take(&self, path: &Path) -> Option<LoadedText> {
        self.take_with_counts(path).map(|(loaded, _)| loaded)
    }

    pub(crate) fn take_with_counts(&self, path: &Path) -> Option<(LoadedText, (usize, usize))> {
        {
            let c = self.shared.cache.lock();
            if !c.entries.iter().any(|e| e.path == path) {
                return None;
            }
        }
        let meta = std::fs::metadata(path).ok()?;
        let mut c = self.shared.cache.lock();
        let i = c.entries.iter().position(|e| e.path == path)?;
        let valid = c.entries[i].size == meta.len() && c.entries[i].mtime == meta.modified().ok() && c.entries[i].mtime.is_some();
        let e = c.entries.remove(i);
        // Taken: a later hover on the note (after it was closed) must be able to request it again.
        if c.last.as_deref() == Some(path) {
            c.last = None;
        }
        if !valid {
            return None;
        }
        c.open_results.retain(|(p, _)| p != path);
        c.record_taken(path, e.loaded.text.len(), e.parse);
        Some((e.loaded, e.counts))
    }

    /// The prefetched parse of `path` when `text` is exactly the text it was made from. The parse is used up either way
    /// (a mismatch never matches later), so the text is hashed at most once per prefetched note.
    pub fn parsed(&self, path: &Path, text: &str) -> Option<Arc<Document>> {
        let mut c = self.shared.cache.lock();
        let i = c.parses.iter().position(|p| p.0 == path)?;
        let (_, len, hash, doc) = c.parses.remove(i);
        drop(c);
        (len == text.len() && text_hash(text) == hash).then_some(doc)
    }

    /// Drops what is kept for `path` (its document was closed).
    pub fn forget(&self, path: &Path) {
        let mut c = self.shared.cache.lock();
        c.parses.retain(|p| p.0 != path);
        c.open_results.retain(|(p, _)| p != path);
    }

    /// Drops cached, open-result, and pending prefetch state for `path`.
    pub fn invalidate(&self, path: &Path) {
        let mut q = self.shared.queue.lock();
        if q.pending_open.as_ref().is_some_and(|(p, _)| p == path) {
            q.pending_open = None;
        }
        if q.pending_prefetch.as_deref() == Some(path) {
            q.pending_prefetch = None;
        }
        if q.in_flight_open.as_deref() == Some(path) {
            q.in_flight_open_invalidated = true;
        }
        if q.in_flight_prefetch.as_deref() == Some(path) {
            q.in_flight_prefetch_invalidated = true;
        }
        let mut c = self.shared.cache.lock();
        c.entries.retain(|e| e.path != path);
        c.open_results.retain(|(p, _)| p != path);
        c.parses.retain(|p| p.0 != path);
        c.once.remove(path);
        if c.last.as_deref() == Some(path) {
            c.last = None;
        }
    }

    /// Drops all cached, open-result, and pending prefetch state (e.g. on `FsEvent::Rescan`).
    pub fn invalidate_all(&self) {
        let mut q = self.shared.queue.lock();
        q.pending_open = None;
        q.pending_prefetch = None;
        if q.in_flight_open.is_some() {
            q.in_flight_open_invalidated = true;
        }
        if q.in_flight_prefetch.is_some() {
            q.in_flight_prefetch_invalidated = true;
        }
        let mut c = self.shared.cache.lock();
        c.entries.clear();
        c.open_results.clear();
        c.parses.clear();
        c.once.clear();
        c.last = None;
    }

    #[cfg(test)]
    fn cached_bytes(&self) -> usize {
        self.shared.cache.lock().entries.iter().map(|e| e.bytes).sum()
    }

    #[cfg(test)]
    fn cached_count(&self) -> usize {
        self.shared.cache.lock().entries.len()
    }

    /// (files read by the worker, entries taken by `open`), for tests.
    pub fn counters(&self) -> (usize, usize) {
        let c = self.shared.cache.lock();
        (c.reads, c.hits)
    }

    /// True when `path` is cached (tests).
    pub fn has(&self, path: &Path) -> bool {
        self.shared.cache.lock().entries.iter().any(|e| e.path == path)
    }

    /// Waits until the worker has nothing pending (tests).
    pub fn wait_idle(&self, timeout: std::time::Duration) {
        let t = std::time::Instant::now();
        while t.elapsed() < timeout {
            let q = self.shared.queue.lock();
            let pending = q.pending_prefetch.is_some() || (!q.open_paused && q.pending_open.is_some());
            drop(q);
            if !pending && self.idle() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn idle(&self) -> bool {
        self.shared.busy.load(std::sync::atomic::Ordering::Acquire) == 0
    }
}

impl Cache {
    fn record_taken(&mut self, path: &Path, len: usize, parse: Option<(u64, Arc<Document>)>) {
        self.hits += 1;
        if let Some((hash, doc)) = parse {
            self.parses.retain(|p| p.0 != path);
            self.parses.push((path.to_path_buf(), len, hash, doc));
            trim(self);
        }
    }

    fn push_open_result(&mut self, path: PathBuf, res: io::Result<OpenEntry>) {
        self.open_results.retain(|(p, _)| p != &path);
        self.open_results.push((path, res));
        while self.open_results.len() > MAX_OPEN_RESULTS {
            let _ = self.open_results.remove(0);
        }
    }
}

/// Enforces the caps; parses count against the byte cap and go first.
fn trim(c: &mut Cache) {
    let bytes = |c: &Cache| c.entries.iter().map(|e| e.bytes).sum::<usize>() + c.parses.iter().map(|p| p.1 * 3).sum::<usize>();
    while c.parses.len() > MAX_PARSES || (!c.parses.is_empty() && bytes(c) > MAX_BYTES) {
        c.parses.remove(0);
    }
    while c.entries.len() > MAX_ENTRIES || (c.entries.len() > 1 && bytes(c) > MAX_BYTES) {
        c.entries.remove(0);
    }
}

enum Work {
    Open(PathBuf, Arc<dyn Waker>),
    Prefetch(PathBuf),
}

fn maybe_parse_markdown(path: &Path, text: &str, cancel: bool) -> Option<(u64, Arc<Document>)> {
    if !cancel && FileKind::from_path(path) == FileKind::Markdown && text.len() <= 8 * 1024 * 1024 {
        let doc = iron_sheets_md::parse(text);
        Some((text_hash(text), Arc::new(doc)))
    } else {
        None
    }
}

fn load_for_open(sh: &Shared, path: &Path) -> io::Result<OpenEntry> {
    if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return Err(io::Error::other("is a folder"));
    }
    let kind = FileKind::from_path(path);
    if kind == FileKind::Image {
        let meta = std::fs::metadata(path)?;
        let loaded = LoadedText { text: String::new(), line_ending: LineEnding::Lf, bom: false, lossy: false, mtime: meta.modified().ok() };
        return Ok(OpenEntry { loaded, counts: (0, 0), parse: None });
    }
    let loaded = textio::load_text_bounded(path, crate::docs::MAX_TEXT_BYTES)?;
    let counts = crate::docs::count_text_raw(&loaded.text);
    let newer_open = sh.queue.lock().pending_open.is_some();
    let parse = maybe_parse_markdown(path, &loaded.text, newer_open);
    Ok(OpenEntry { loaded, counts, parse })
}

fn worker(sh: Arc<Shared>) {
    loop {
        let work = {
            let mut q = sh.queue.lock();
            loop {
                if sh.closed.load(std::sync::atomic::Ordering::Acquire) {
                    return;
                }
                if !q.open_paused {
                    if let Some((path, waker)) = q.pending_open.take() {
                        q.in_flight_open = Some(path.clone());
                        q.in_flight_open_invalidated = false;
                        sh.busy.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        break Work::Open(path, waker);
                    }
                }
                if let Some(path) = q.pending_prefetch.take() {
                    q.in_flight_prefetch = Some(path.clone());
                    q.in_flight_prefetch_invalidated = false;
                    sh.busy.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    break Work::Prefetch(path);
                }
                sh.wake.wait(&mut q);
            }
        };
        match work {
            Work::Open(path, waker) => {
                let kind = FileKind::from_path(&path);
                let res = load_for_open(&sh, &path);
                {
                    let mut q = sh.queue.lock();
                    let invalidated = q.in_flight_open_invalidated;
                    if q.in_flight_open.as_deref() == Some(path.as_path()) {
                        q.in_flight_open = None;
                        q.in_flight_open_invalidated = false;
                    }
                    if !invalidated {
                        let mut c = sh.cache.lock();
                        if res.is_ok() && kind != FileKind::Image {
                            c.reads += 1;
                        }
                        c.push_open_result(path.clone(), res);
                    }
                }
                sh.busy.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                waker.wake();
            }
            Work::Prefetch(path) => {
                let newer = |sh: &Shared| {
                    let q = sh.queue.lock();
                    q.pending_prefetch.is_some() || (!q.open_paused && q.pending_open.is_some())
                };
                let result = (|| {
                    let file = std::fs::File::open(&path).ok()?;
                    let meta = file.metadata().ok()?;
                    if !meta.is_file() || meta.len() > MAX_FILE {
                        return None;
                    }
                    // Modification time and size from before the read: a file changed during the read does not validate.
                    let size = meta.len();
                    let mtime = meta.modified().ok();
                    // Already cached and unchanged: nothing to do.
                    {
                        let c = sh.cache.lock();
                        if c.entries.iter().any(|e| e.path == path && e.size == size && e.mtime == mtime) {
                            return None;
                        }
                    }
                    let loaded = textio::load_text_from_file_bounded(file, &meta, MAX_FILE).ok()?;
                    let counts = crate::docs::count_text_raw(&loaded.text);
                    let parse = maybe_parse_markdown(&path, &loaded.text, newer(sh.as_ref()));
                    let bytes = size_estimate(&loaded, parse.is_some());
                    Some(Entry { path: path.clone(), loaded, counts, size, mtime, parse, bytes })
                })();
                {
                    let mut q = sh.queue.lock();
                    let invalidated = q.in_flight_prefetch_invalidated;
                    if q.in_flight_prefetch.as_deref() == Some(path.as_path()) {
                        q.in_flight_prefetch = None;
                        q.in_flight_prefetch_invalidated = false;
                    }
                    if !invalidated {
                        if let Some(e) = result {
                            let mut c = sh.cache.lock();
                            c.reads += 1;
                            c.entries.retain(|x| x.path != e.path);
                            c.entries.push(e);
                            trim(&mut c);
                        }
                    }
                }
                sh.busy.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetched_text_is_taken_once_and_validated_by_mtime_and_size() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, "# A\n\nsome text\n").unwrap();
        let pf = Prefetch::default();
        pf.request(&p);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(pf.has(&p));
        let l = pf.take(&p).expect("cached");
        assert_eq!(l.text, "# A\n\nsome text\n");
        assert!(pf.take(&p).is_none(), "taken once");
        let doc = pf.parsed(&p, "# A\n\nsome text\n").expect("parse kept for the preview");
        assert!(!doc.blocks.is_empty());
        assert!(pf.parsed(&p, "# A\n\nsome text\n").is_none(), "used up");
        pf.request(&p);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(pf.has(&p), "requested again after it was taken");
        pf.take(&p);
        assert!(pf.parsed(&p, "# B\n\nsome text\n").is_none(), "other text: no parse");
        assert!(pf.parsed(&p, "# A\n\nsome text\n").is_none(), "mismatch used it up");
        pf.request(&p);
        pf.wait_idle(std::time::Duration::from_secs(5));
        pf.take(&p);
        pf.forget(&p);
        assert!(pf.parsed(&p, "# A\n\nsome text\n").is_none(), "forgotten");
        // A changed file is not served from the cache.
        let q = t.path().join("b.md");
        std::fs::write(&q, "one").unwrap();
        pf.request(&q);
        pf.wait_idle(std::time::Duration::from_secs(5));
        std::fs::write(&q, "changed text").unwrap();
        assert!(pf.take(&q).is_none());
    }

    #[test]
    fn cache_is_bounded() {
        let t = tempfile::tempdir().unwrap();
        let pf = Prefetch::default();
        for i in 0..40 {
            let p = t.path().join(format!("n{i}.md"));
            std::fs::write(&p, format!("# {i}\n").repeat(100)).unwrap();
            pf.request(&p);
            pf.wait_idle(std::time::Duration::from_secs(5));
        }
        assert!(pf.cached_count() <= MAX_ENTRIES);
        assert!(pf.cached_bytes() <= MAX_BYTES);
    }

    #[test]
    fn request_open_handles_large_files_errors_and_pause() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountWaker(AtomicUsize);
        impl Waker for CountWaker {
            fn wake(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let t = tempfile::tempdir().unwrap();
        let pf = Prefetch::default();
        let waker = Arc::new(CountWaker(AtomicUsize::new(0)));

        // > 16 MiB (hover cap) still opens via request_open up to 64 MiB.
        let big = t.path().join("17m.md");
        std::fs::write(&big, vec![b'a'; 17 * 1024 * 1024]).unwrap();
        pf.request(&big);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(!pf.has(&big), "hover prefetch stays capped at 16 MiB");

        pf.set_open_paused(true);
        pf.request_open(&big, waker.clone());
        assert!(pf.has_pending_open(&big));
        assert!(pf.take_open(&big).is_none());
        pf.set_open_paused(false);
        pf.wait_idle(std::time::Duration::from_secs(5));
        let loaded = pf.take_open(&big).expect("ready").expect("ok");
        assert_eq!(loaded.text.len(), 17 * 1024 * 1024);
        assert!(waker.0.load(Ordering::Relaxed) >= 1);

        // Binary and missing files surface the exact same error strings as DocStore::open.
        let bin = t.path().join("bin.md");
        std::fs::write(&bin, b"abc\0def").unwrap();
        pf.request_open(&bin, waker.clone());
        pf.wait_idle(std::time::Duration::from_secs(5));
        let err = pf.take_open(&bin).expect("ready").unwrap_err();
        assert_eq!(err.to_string(), "binary file, not opened");

        let missing = t.path().join("missing.md");
        pf.request_open(&missing, waker.clone());
        pf.wait_idle(std::time::Duration::from_secs(5));
        let err = pf.take_open(&missing).expect("ready").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        let too_big = t.path().join("too_big.md");
        std::fs::File::create(&too_big).unwrap().set_len(crate::docs::MAX_TEXT_BYTES + 1).unwrap();
        pf.request_open(&too_big, waker);
        pf.wait_idle(std::time::Duration::from_secs(5));
        let err = pf.take_open(&too_big).expect("ready").unwrap_err();
        assert_eq!(err.to_string(), "file is too large to open");
    }

    #[test]
    fn prefetch_and_request_open_compute_counts_off_thread() {
        struct NoopWaker;
        impl Waker for NoopWaker {
            fn wake(&self) {}
        }

        let t = tempfile::tempdir().unwrap();
        let pf = Prefetch::default();
        let waker: Arc<dyn Waker> = Arc::new(NoopWaker);

        let p_hover = t.path().join("hover.md");
        std::fs::write(&p_hover, "one two\nthree\u{a0}four  ").unwrap();
        pf.request(&p_hover);
        pf.wait_idle(std::time::Duration::from_secs(5));
        let (loaded, counts) = pf.take_with_counts(&p_hover).expect("cached");
        assert_eq!(counts, crate::docs::count_text(&loaded.text));

        let p_open = t.path().join("open.md");
        std::fs::write(&p_open, "alpha beta\ngamma wörld").unwrap();
        pf.request_open(&p_open, waker);
        pf.wait_idle(std::time::Duration::from_secs(5));
        let (loaded, counts) = pf.take_open_with_counts(&p_open).expect("ready").expect("ok");
        assert_eq!(counts, crate::docs::count_text(&loaded.text));
    }

    #[test]
    fn invalidate_and_invalidate_all_drop_cached_and_open_results() {
        struct NoopWaker;
        impl Waker for NoopWaker {
            fn wake(&self) {}
        }

        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a.md");
        let b = t.path().join("b.md");
        let c = t.path().join("c.md");
        std::fs::write(&a, "# A\n\nhello\n").unwrap();
        std::fs::write(&b, "# B\n\nworld\n").unwrap();
        std::fs::write(&c, "# C\n\nother\n").unwrap();

        let pf = Prefetch::default();
        let waker: Arc<dyn Waker> = Arc::new(NoopWaker);

        // Hover cache entry + open_results entry for `a`, open_results for `b`, hover cache for `c`.
        pf.request(&a);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(pf.has(&a));
        pf.request_open(&a, waker.clone());
        pf.wait_idle(std::time::Duration::from_secs(5));
        pf.request_open(&b, waker.clone());
        pf.wait_idle(std::time::Duration::from_secs(5));
        pf.request(&c);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(pf.has(&c));

        // Invalidating `a` drops both its hover cache and open_results while leaving `b` and `c` intact.
        pf.invalidate(&a);
        assert!(!pf.has(&a));
        assert!(pf.take_open(&a).is_none());
        assert!(pf.parsed(&a, "# A\n\nhello\n").is_none());
        // `last` was cleared for `a`, so re-requesting `a` works.
        pf.request(&a);
        pf.wait_idle(std::time::Duration::from_secs(5));
        assert!(pf.has(&a));

        // Invalidating all drops everything across `entries` and `open_results`.
        assert!(pf.has(&c));
        pf.invalidate_all();
        assert!(!pf.has(&a));
        assert!(!pf.has(&c));
        assert!(pf.take_open(&b).is_none());
        assert!(pf.take_open(&c).is_none());
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    #[test]
    fn worker_exits_when_dropped() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, "x").unwrap();
        for _ in 0..200 {
            let pf = Prefetch::default();
            pf.request(&p);
            let sh = Arc::downgrade(&pf.shared);
            drop(pf);
            let t0 = std::time::Instant::now();
            while sh.strong_count() > 0 {
                assert!(t0.elapsed() < std::time::Duration::from_secs(5), "worker leaked");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}
