//! Lazy file tree model for the sidebar.
use crate::glob::ExcludeSet;
use crate::kind::FileKind;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Number of skeleton placeholder rows emitted while an expanded directory is loading asynchronously.
const SKELETON_ROWS: usize = 3;
const TREE_CACHE_MAGIC: &[u8; 8] = b"MDXTRE\0\x01";
const TREE_CACHE_VERSION: u32 = 2;
const MAX_TREE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHED_DIRS: usize = 256;
const MAX_CACHED_ENTRIES: usize = 25_000;
const REVALIDATE_MTIME_MARGIN_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: FileKind,
}

#[derive(Debug, Clone)]
pub struct TreeRow {
    pub depth: u16,
    pub entry: TreeEntry,
    pub expanded: bool,
    pub is_root: bool,
    /// Not a real entry (loading bars, or a note row): navigation and selection skip it.
    pub skeleton: bool,
    /// Set on a note row under an expanded folder that is empty or could not be read.
    pub note: Option<RowNote>,
    /// Folder row whose children are loaded and hold `<name>.md` / `<name>.typ`: that note ("folder note").
    /// shortcut: unknown while the folder's children are not loaded (the UI asks for a background read of the
    /// hovered or focused folder, see `prefetch_dir`), so a click before that just expands the folder.
    pub folder_note: Option<PathBuf>,
}

/// The note named like its folder among the folder's `children`: `<dir_name>.md`, else `<dir_name>.typ`.
/// shortcut: the stem must match exactly (case-sensitive), the extension is any case; upgrade if users ask for `foo/Foo.md`.
pub fn folder_note<'a>(dir_name: &str, children: &'a [TreeEntry]) -> Option<&'a Path> {
    let mut found = None;
    for e in children.iter().filter(|e| !e.is_dir && Path::new(&e.name).file_stem().is_some_and(|s| s == dir_name)) {
        match e.kind {
            FileKind::Markdown => return Some(&e.path),
            FileKind::Typst => found = Some(e.path.as_path()),
            _ => {}
        }
    }
    found
}

/// Text row under an expanded folder without entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowNote {
    Empty,
    Unreadable(String),
}

fn note_row(depth: u16, note: RowNote) -> TreeRow {
    TreeRow { note: Some(note), ..skeleton_row(depth) }
}

fn skeleton_row(depth: u16) -> TreeRow {
    TreeRow {
        depth,
        entry: TreeEntry { name: String::new(), path: PathBuf::new(), is_dir: false, kind: FileKind::Other },
        expanded: false,
        is_root: false,
        skeleton: true,
        note: None,
        folder_note: None,
    }
}

/// Case-insensitive natural comparison ("a2" < "a10").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = trim_zeros(&a[si..i]);
            let nb = trim_zeros(&b[sj..j]);
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            let o = a[i].cmp(&b[j]);
            if o != Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&c| c == b'0').count();
    &s[n..]
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Created,
}

/// How the tree orders the entries of a folder. Folders always come first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TreeSort {
    pub key: SortKey,
    pub desc: bool,
}

impl TreeSort {
    /// State for the sort button: "Name, asc.", "Modified, desc.".
    pub fn label(self) -> &'static str {
        match (self.key, self.desc) {
            (SortKey::Name, false) => "Name, asc.",
            (SortKey::Name, true) => "Name, desc.",
            (SortKey::Modified, false) => "Modified, asc.",
            (SortKey::Modified, true) => "Modified, desc.",
            (SortKey::Created, false) => "Created, asc.",
            (SortKey::Created, true) => "Created, desc.",
        }
    }
}

/// Deterministic 64-bit FNV-1a hasher with a SplitMix64 finalizer (identical across restarts).
struct DetHasher(u64);

impl DetHasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        self.write_bytes(s.as_bytes());
    }

    fn write_path(&mut self, p: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let b = p.as_os_str().as_bytes();
            self.write_u64(b.len() as u64);
            self.write_bytes(b);
        }
        #[cfg(not(unix))]
        {
            self.write_str(&p.to_string_lossy());
        }
    }
}

impl Hasher for DetHasher {
    fn finish(&self) -> u64 {
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }

    fn write_u8(&mut self, v: u8) {
        self.write_bytes(&[v]);
    }

    fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }
}

fn kind_to_u8(k: FileKind) -> u8 {
    match k {
        FileKind::Markdown => 0,
        FileKind::Typst => 1,
        FileKind::Image => 2,
        FileKind::Other => 3,
        FileKind::Excalidraw => 4,
    }
}

fn kind_from_u8(v: u8) -> Option<FileKind> {
    match v {
        0 => Some(FileKind::Markdown),
        1 => Some(FileKind::Typst),
        2 => Some(FileKind::Image),
        3 => Some(FileKind::Other),
        4 => Some(FileKind::Excalidraw),
        _ => None,
    }
}

fn entries_content_hash(entries: &[TreeEntry]) -> u64 {
    let mut h = DetHasher::new();
    h.write_u64(entries.len() as u64);
    for e in entries {
        h.write_str(&e.name);
        h.write_path(&e.path);
        h.write_u8(e.is_dir as u8);
        h.write_u8(kind_to_u8(e.kind));
    }
    h.finish()
}

fn exclude_sig(excludes: &ExcludeSet) -> u64 {
    let mut h = DetHasher::new();
    excludes.hash(&mut h);
    h.finish()
}

fn system_time_ms(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirMeta {
    dir_mtime_ms: Option<u64>,
    content_hash: u64,
    has_symlinks: bool,
    saved_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
struct RevalidateHint {
    cached_mtime_ms: u64,
    saved_at_ms: u64,
    has_symlinks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEntry {
    name: String,
    path: String,
    is_dir: bool,
    kind: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDir {
    path: String,
    dir_mtime_ms: Option<u64>,
    content_hash: u64,
    has_symlinks: bool,
    entries: Vec<CachedEntry>,
}

/// Serializable snapshot of currently-loaded expanded directories for instant warm-start tree rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeCacheSnapshot {
    saved_at_ms: u64,
    config_hash: u64,
    roots_hash: u64,
    dirs: Vec<CachedDir>,
}

impl TreeCacheSnapshot {
    pub fn config_hash(&self) -> u64 {
        self.config_hash
    }

    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    pub fn entry_count(&self) -> usize {
        self.dirs.iter().map(|d| d.entries.len()).sum()
    }
}

/// Directories first, then by `sort` (names naturally, case-insensitive); dot entries skipped unless `show_hidden`.
/// Created time falls back to modified where the file system has none.
pub fn read_dir_sorted(dir: &Path, show_hidden: bool, sort: TreeSort) -> io::Result<Vec<TreeEntry>> {
    read_dir_sorted_filtered(dir, show_hidden, true, &ExcludeSet::default(), "", sort)
}

/// Like [`read_dir_sorted`], applying `show_other` and `excludes` before per-entry `metadata()` calls and sorting.
pub fn read_dir_sorted_filtered(
    dir: &Path,
    show_hidden: bool,
    show_other: bool,
    excludes: &ExcludeSet,
    rel_base: &str,
    sort: TreeSort,
) -> io::Result<Vec<TreeEntry>> {
    read_dir_with_meta(dir, show_hidden, show_other, excludes, rel_base, sort).map(|(v, _)| v)
}

fn read_dir_with_meta(
    dir: &Path,
    show_hidden: bool,
    show_other: bool,
    excludes: &ExcludeSet,
    rel_base: &str,
    sort: TreeSort,
) -> io::Result<(Vec<TreeEntry>, DirMeta)> {
    let dir_mtime_ms = std::fs::metadata(dir).ok().and_then(|m| m.modified().ok()).and_then(system_time_ms);
    let mut has_symlinks = std::fs::symlink_metadata(dir).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let mut v: Vec<(String, Option<SystemTime>, TreeEntry)> = Vec::new();
    let mut rel_buf = String::new();
    if !excludes.is_empty() && !rel_base.is_empty() {
        rel_buf.push_str(rel_base);
        rel_buf.push('/');
    }
    let prefix_len = rel_buf.len();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if !excludes.is_empty() {
            let excluded = if prefix_len == 0 {
                excludes.matches(&name, &name)
            } else {
                rel_buf.truncate(prefix_len);
                rel_buf.push_str(&name);
                excludes.matches(&rel_buf, &name)
            };
            if excluded {
                continue;
            }
        }
        let ft = e.file_type();
        let (is_dir, path_opt) = match ft {
            Ok(t) if t.is_dir() => (true, None),
            Ok(t) if t.is_symlink() => {
                has_symlinks = true;
                let p = e.path();
                let d = std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false);
                (d, Some(p))
            }
            _ => (false, None),
        };
        let kind = if is_dir { FileKind::Other } else { FileKind::from_path(Path::new(&name)) };
        if !show_other && !is_dir && kind == FileKind::Other {
            continue;
        }
        let path = path_opt.unwrap_or_else(|| e.path());
        let time = match sort.key {
            SortKey::Name => None,
            SortKey::Modified => e.metadata().and_then(|m| m.modified()).ok(),
            SortKey::Created => e.metadata().ok().and_then(|m| m.created().or_else(|_| m.modified()).ok()),
        };
        v.push((name.to_lowercase(), time, TreeEntry { name, path, is_dir, kind }));
    }
    v.sort_by(|(ka, ta, a), (kb, tb, b)| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let o = if sort.key == SortKey::Name { natural_cmp(ka, kb) } else { ta.cmp(tb) };
            let o = if sort.desc { o.reverse() } else { o };
            o.then_with(|| natural_cmp(ka, kb)).then_with(|| a.name.cmp(&b.name))
        })
    });
    let entries: Vec<TreeEntry> = v.into_iter().map(|(.., e)| e).collect();
    let content_hash = entries_content_hash(&entries);
    Ok((entries, DirMeta { dir_mtime_ms, content_hash, has_symlinks, saved_at_ms: None }))
}

fn rel_base(roots: &[PathBuf], dir: &Path) -> String {
    roots
        .iter()
        .filter(|r| dir.starts_with(r))
        .max_by_key(|r| r.components().count())
        .and_then(|r| dir.strip_prefix(r).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LoadPriority {
    High = 0,
    Normal = 1,
    Low = 2,
}

enum LoadOutcome {
    Unchanged,
    Loaded {
        entries: Vec<TreeEntry>,
        meta: DirMeta,
        missing: bool,
        /// Why the folder could not be read (its `entries` are empty then).
        error: Option<String>,
    },
}

struct LoadResult {
    load_gen: u64,
    seq: u64,
    dir: PathBuf,
    outcome: LoadOutcome,
}

struct LoadJob {
    dir: PathBuf,
    load_gen: u64,
    seq: u64,
    hint: Option<RevalidateHint>,
}

#[derive(Clone, Copy)]
struct QueuedEntry {
    load_gen: u64,
    prio: LoadPriority,
    seq: u64,
    hint: Option<RevalidateHint>,
}

struct QueueState {
    load_gen: u64,
    closed: bool,
    paused: bool,
    active: usize,
    queues: [VecDeque<LoadJob>; 3],
    queued: FxHashMap<PathBuf, QueuedEntry>,
    roots: Vec<PathBuf>,
    show_hidden: bool,
    show_other: bool,
    excludes: ExcludeSet,
    sort: TreeSort,
}

impl QueueState {
    fn queues_empty(&self) -> bool {
        self.queues.iter().all(VecDeque::is_empty)
    }

    fn clear_queues(&mut self) {
        for q in &mut self.queues {
            q.clear();
        }
        self.queued.clear();
    }
}

struct LoaderShared {
    lock: Mutex<QueueState>,
    work_cv: Condvar,
    idle_cv: Condvar,
    gen_atomic: AtomicU64,
    closed_atomic: AtomicBool,
    reads: Arc<AtomicUsize>,
}

struct TreeLoader {
    shared: Arc<LoaderShared>,
    tx: Sender<LoadResult>,
    rx: Receiver<LoadResult>,
    waker: Arc<dyn Fn() + Send + Sync>,
    spawned: bool,
}

impl TreeLoader {
    #[allow(clippy::too_many_arguments)]
    fn new(
        waker: Arc<dyn Fn() + Send + Sync>,
        load_gen: u64,
        roots: Vec<PathBuf>,
        show_hidden: bool,
        show_other: bool,
        excludes: ExcludeSet,
        sort: TreeSort,
        reads: Arc<AtomicUsize>,
    ) -> Self {
        let (tx, rx) = unbounded();
        let shared = Arc::new(LoaderShared {
            lock: Mutex::new(QueueState {
                load_gen,
                closed: false,
                paused: false,
                active: 0,
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                queued: FxHashMap::default(),
                roots,
                show_hidden,
                show_other,
                excludes,
                sort,
            }),
            work_cv: Condvar::new(),
            idle_cv: Condvar::new(),
            gen_atomic: AtomicU64::new(load_gen),
            closed_atomic: AtomicBool::new(false),
            reads,
        });
        TreeLoader { shared, tx, rx, waker, spawned: false }
    }

    fn ensure_spawned(&mut self) {
        if self.spawned {
            return;
        }
        self.spawned = true;
        let shared = self.shared.clone();
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        if std::thread::Builder::new().name("iron-sheets-tree".into()).spawn(move || worker_loop(shared, tx, waker)).is_err() {
            self.shared.closed_atomic.store(true, AtomicOrdering::Release);
            let mut q = self.shared.lock.lock();
            q.closed = true;
            q.clear_queues();
            self.shared.idle_cv.notify_all();
        }
    }

    fn enqueue(&mut self, dir: PathBuf, prio: LoadPriority, seq: u64, hint: Option<RevalidateHint>) -> Option<u64> {
        self.ensure_spawned();
        let mut q = self.shared.lock.lock();
        if q.closed {
            return None;
        }
        let load_gen = q.load_gen;
        if let Some(&old) = q.queued.get(&dir) {
            if old.load_gen == load_gen && old.prio <= prio && (old.hint.is_none() || hint.is_some()) {
                return Some(old.seq);
            }
        }
        q.queued.insert(dir.clone(), QueuedEntry { load_gen, prio, seq, hint });
        q.queues[prio as usize].push_back(LoadJob { dir, load_gen, seq, hint });
        self.shared.work_cv.notify_one();
        Some(seq)
    }

    fn upgrade_if_queued(&mut self, dir: &Path, prio: LoadPriority, seq: u64) -> Option<u64> {
        let mut q = self.shared.lock.lock();
        if q.closed {
            return None;
        }
        let load_gen = q.load_gen;
        let &old = q.queued.get(dir)?;
        if old.load_gen != load_gen || old.prio <= prio {
            return Some(old.seq);
        }
        q.queued.insert(dir.to_path_buf(), QueuedEntry { load_gen, prio, seq, hint: old.hint });
        q.queues[prio as usize].push_back(LoadJob { dir: dir.to_path_buf(), load_gen, seq, hint: old.hint });
        self.shared.work_cv.notify_one();
        Some(seq)
    }

    fn update_config(&mut self, load_gen: u64, roots: Vec<PathBuf>, show_hidden: bool, show_other: bool, excludes: ExcludeSet, sort: TreeSort) {
        self.shared.gen_atomic.store(load_gen, AtomicOrdering::Release);
        let mut q = self.shared.lock.lock();
        q.load_gen = load_gen;
        q.clear_queues();
        q.roots = roots;
        q.show_hidden = show_hidden;
        q.show_other = show_other;
        q.excludes = excludes;
        q.sort = sort;
    }

    fn try_recv(&self) -> Option<LoadResult> {
        self.rx.try_recv().ok()
    }
}

impl Drop for TreeLoader {
    fn drop(&mut self) {
        self.shared.closed_atomic.store(true, AtomicOrdering::Release);
        let mut q = self.shared.lock.lock();
        q.closed = true;
        q.clear_queues();
        self.shared.work_cv.notify_all();
        self.shared.idle_cv.notify_all();
    }
}

fn pop_next(q: &mut QueueState) -> Option<LoadJob> {
    for prio in 0..3 {
        while let Some(job) = q.queues[prio].pop_front() {
            if job.load_gen == q.load_gen && q.queued.get(&job.dir).is_some_and(|e| e.load_gen == job.load_gen && e.seq == job.seq) {
                q.queued.remove(&job.dir);
                return Some(job);
            }
        }
    }
    None
}

#[cfg(unix)]
fn can_skip_readdir(dir: &Path, sort: TreeSort, hint: RevalidateHint) -> bool {
    if sort.key != SortKey::Name || hint.has_symlinks {
        return false;
    }
    if hint.cached_mtime_ms.checked_add(REVALIDATE_MTIME_MARGIN_MS).is_none_or(|t| t >= hint.saved_at_ms) {
        return false;
    }
    let Ok(md) = std::fs::metadata(dir) else {
        return false;
    };
    md.is_dir() && md.modified().ok().and_then(system_time_ms) == Some(hint.cached_mtime_ms)
}

fn worker_loop(shared: Arc<LoaderShared>, tx: Sender<LoadResult>, waker: Arc<dyn Fn() + Send + Sync>) {
    loop {
        let (job, roots, show_hidden, show_other, excludes, sort) = {
            let mut q = shared.lock.lock();
            loop {
                if q.closed {
                    return;
                }
                if !q.paused {
                    if let Some(job) = pop_next(&mut q) {
                        q.active += 1;
                        break (job, q.roots.clone(), q.show_hidden, q.show_other, q.excludes.clone(), q.sort);
                    }
                }
                if q.active == 0 {
                    shared.idle_cv.notify_all();
                }
                shared.work_cv.wait(&mut q);
            }
        };
        let LoadJob { dir, load_gen: job_gen, seq: job_seq, hint } = job;
        let skip = {
            #[cfg(unix)]
            {
                hint.is_some_and(|h| can_skip_readdir(&dir, sort, h))
            }
            #[cfg(not(unix))]
            {
                let _ = hint;
                false
            }
        };
        let outcome = if skip {
            LoadOutcome::Unchanged
        } else {
            let rel = rel_base(&roots, &dir);
            shared.reads.fetch_add(1, AtomicOrdering::Relaxed);
            match read_dir_with_meta(&dir, show_hidden, show_other, &excludes, &rel, sort) {
                Ok((entries, meta)) => LoadOutcome::Loaded { entries, meta, missing: false, error: None },
                Err(e) => LoadOutcome::Loaded {
                    entries: Vec::new(),
                    meta: DirMeta { dir_mtime_ms: None, content_hash: entries_content_hash(&[]), has_symlinks: false, saved_at_ms: None },
                    missing: !dir.is_dir(),
                    error: Some(e.to_string()),
                },
            }
        };
        let still_valid = !shared.closed_atomic.load(AtomicOrdering::Acquire) && shared.gen_atomic.load(AtomicOrdering::Acquire) == job_gen;
        if still_valid {
            let _ = tx.send(LoadResult { load_gen: job_gen, seq: job_seq, dir, outcome });
        }
        {
            let mut q = shared.lock.lock();
            q.active -= 1;
            if q.active == 0 && q.queues_empty() {
                shared.idle_cv.notify_all();
            }
        }
        if still_valid {
            (waker)();
        }
    }
}

/// 64-bit SplitMix64 finalizer over `FxHasher` so sibling paths XOR cleanly without bit cancellation.
fn path_hash(path: &Path) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    path.hash(&mut h);
    let mut x = h.finish();
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Flattened visible rows; directories are read from disk when first expanded.
/// Roots start collapsed; call `set_expanded` to open them.
pub struct TreeModel {
    roots: Vec<PathBuf>,
    show_hidden: bool,
    show_other: bool,
    excludes: ExcludeSet,
    expanded: HashSet<PathBuf>,
    /// Order-independent XOR hash of `expanded` entries; maintained incrementally on every mutation.
    expanded_hash: u64,
    /// Lazily populated sorted snapshot of `expanded`; cleared via `take()` whenever `expanded` changes.
    sorted_expanded: OnceLock<Vec<PathBuf>>,
    children: HashMap<PathBuf, Vec<TreeEntry>>,
    /// Folders whose last read failed (their `children` are empty).
    errors: HashMap<PathBuf, String>,
    dir_meta: FxHashMap<PathBuf, DirMeta>,
    rows: Vec<TreeRow>,
    dirty: bool,
    reads: Arc<AtomicUsize>,
    sort: TreeSort,
    filter_sig: u64,
    last_persisted_hash: Option<u64>,
    cache_needs_resave: bool,
    cache_revalidated: bool,
    /// Folder shown as the only root ("go into"); always inside one of `roots`.
    focus: Option<PathBuf>,
    folder_notes: bool,
    folder_note_hide: bool,
    async_loader: Option<TreeLoader>,
    loading: FxHashMap<PathBuf, u64>,
    restored_expanded: FxHashSet<PathBuf>,
    pending_restore: bool,
    load_gen: u64,
    next_seq: u64,
}

impl TreeModel {
    pub fn new(roots: Vec<PathBuf>, show_hidden: bool) -> Self {
        TreeModel {
            roots,
            show_hidden,
            show_other: true,
            excludes: Default::default(),
            expanded: HashSet::new(),
            expanded_hash: 0,
            sorted_expanded: OnceLock::new(),
            children: HashMap::new(),
            errors: HashMap::new(),
            dir_meta: FxHashMap::default(),
            rows: Vec::new(),
            dirty: true,
            reads: Arc::new(AtomicUsize::new(0)),
            sort: TreeSort::default(),
            filter_sig: 0,
            last_persisted_hash: None,
            cache_needs_resave: false,
            cache_revalidated: false,
            focus: None,
            folder_notes: true,
            folder_note_hide: true,
            async_loader: None,
            loading: FxHashMap::default(),
            restored_expanded: FxHashSet::default(),
            pending_restore: false,
            load_gen: 0,
            next_seq: 1,
        }
    }

    /// Enable asynchronous background directory loading with a lazy worker thread.
    pub fn enable_async(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.loading.clear();
        self.load_gen += 1;
        self.async_loader = Some(TreeLoader::new(
            waker,
            self.load_gen,
            self.roots.clone(),
            self.show_hidden,
            self.show_other,
            self.excludes.clone(),
            self.sort,
            self.reads.clone(),
        ));
    }

    /// Mark nested paths restored from session state so they load at Normal priority after top-level roots.
    pub fn restore_expanded(&mut self, paths: &[PathBuf]) {
        for p in paths {
            if self.insert_expanded(p) {
                self.dirty = true;
            }
            if !self.roots.iter().any(|r| r == p) {
                self.restored_expanded.insert(p.clone());
            }
        }
        if !self.restored_expanded.is_empty() {
            self.pending_restore = true;
        }
    }

    fn sync_loader_config(&mut self) {
        self.load_gen += 1;
        self.loading.clear();
        if let Some(loader) = self.async_loader.as_mut() {
            loader.update_config(self.load_gen, self.roots.clone(), self.show_hidden, self.show_other, self.excludes.clone(), self.sort);
        }
    }

    fn enqueue(&mut self, dir: PathBuf, prio: LoadPriority, hint: Option<RevalidateHint>) {
        let Some(loader) = self.async_loader.as_mut() else { return };
        let seq = self.next_seq;
        if let Some(effective) = loader.enqueue(dir.clone(), prio, seq, hint) {
            if effective == seq {
                self.next_seq += 1;
            }
            self.loading.insert(dir, effective);
        } else {
            // Worker failed to spawn or shut down: fall back synchronously so `has_pending_loads` never wedges.
            self.loading.remove(&dir);
            self.restored_expanded.remove(&dir);
            self.children.remove(&dir);
            self.dir_meta.remove(&dir);
            self.load_children(&dir);
            self.dirty = true;
        }
    }

    fn revalidate_hint(&self, dir: &Path) -> Option<RevalidateHint> {
        let m = self.dir_meta.get(dir)?;
        Some(RevalidateHint { cached_mtime_ms: m.dir_mtime_ms?, saved_at_ms: m.saved_at_ms?, has_symlinks: m.has_symlinks })
    }

    fn config_hash(&self) -> u64 {
        let mut h = DetHasher::new();
        h.write_u8(self.show_hidden as u8);
        h.write_u8(self.show_other as u8);
        h.write_u8(match self.sort.key {
            SortKey::Name => 0,
            SortKey::Modified => 1,
            SortKey::Created => 2,
        });
        h.write_u8(self.sort.desc as u8);
        h.write_u64(exclude_sig(&self.excludes));
        h.write_u64(self.filter_sig);
        h.finish()
    }

    fn roots_hash(&self) -> u64 {
        let mut h = DetHasher::new();
        h.write_u64(self.roots.len() as u64);
        for r in &self.roots {
            h.write_path(r);
        }
        h.finish()
    }

    /// Set a deterministic signature over the caller's exclude glob configuration (included in the tree cache key).
    pub fn set_filter_sig(&mut self, sig: u64) {
        self.filter_sig = sig;
    }

    /// Seed expanded directories under current `roots` from `<cache>/tree-<slug>.bin` for zero-skeleton Frame 1.
    /// Corrupt, oversized (>8 MiB), or mismatched-config/roots cache files are silently ignored.
    pub fn seed_from_cache(&mut self, cache_path: &Path) -> usize {
        let Ok(file) = std::fs::File::open(cache_path) else {
            return 0;
        };
        let Ok(meta) = file.metadata() else {
            return 0;
        };
        if meta.len() > MAX_TREE_CACHE_BYTES as u64 {
            return 0;
        }
        let mut bytes = Vec::with_capacity((meta.len() as usize).min(MAX_TREE_CACHE_BYTES));
        if file.take(MAX_TREE_CACHE_BYTES as u64 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > MAX_TREE_CACHE_BYTES {
            return 0;
        }
        let Some(rest) = bytes.strip_prefix(TREE_CACHE_MAGIC.as_slice()) else {
            return 0;
        };
        let Some(ver_bytes) = rest.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()) else {
            return 0;
        };
        if u32::from_le_bytes(ver_bytes) != TREE_CACHE_VERSION {
            return 0;
        }
        let Ok(snap) = postcard::from_bytes::<TreeCacheSnapshot>(&rest[4..]) else {
            return 0;
        };
        if snap.config_hash != self.config_hash() || snap.roots_hash != self.roots_hash() {
            return 0;
        }
        if snap.dirs.len() > MAX_CACHED_DIRS {
            return 0;
        }
        let mut total_entries = 0usize;
        let mut validated: Vec<(PathBuf, Vec<TreeEntry>, DirMeta)> = Vec::with_capacity(snap.dirs.len());
        let mut any_young = false;
        for cd in snap.dirs {
            total_entries = total_entries.saturating_add(cd.entries.len());
            if total_entries > MAX_CACHED_ENTRIES {
                return 0;
            }
            let mut entries = Vec::with_capacity(cd.entries.len());
            for ce in cd.entries {
                let Some(kind) = kind_from_u8(ce.kind) else {
                    return 0;
                };
                entries.push(TreeEntry { name: ce.name, path: PathBuf::from(ce.path), is_dir: ce.is_dir, kind });
            }
            if entries_content_hash(&entries) != cd.content_hash {
                return 0;
            }
            let p = PathBuf::from(cd.path);
            if self.expanded.contains(&p) && self.roots.iter().any(|r| p.starts_with(r)) {
                if cd.dir_mtime_ms.is_some_and(|m| m.checked_add(REVALIDATE_MTIME_MARGIN_MS).is_none_or(|t| t >= snap.saved_at_ms)) {
                    any_young = true;
                }
                validated.push((
                    p,
                    entries,
                    DirMeta { dir_mtime_ms: cd.dir_mtime_ms, content_hash: cd.content_hash, has_symlinks: cd.has_symlinks, saved_at_ms: Some(snap.saved_at_ms) },
                ));
            }
        }
        let seeded = validated.len();
        for (p, entries, dir_meta) in validated {
            self.restored_expanded.remove(&p);
            self.children.insert(p.clone(), entries);
            self.dir_meta.insert(p, dir_meta);
        }
        if self.restored_expanded.is_empty() {
            self.pending_restore = false;
        }
        if seeded > 0 {
            self.dirty = true;
            let (hash, _) = self.collect_cache_dirs(false);
            self.last_persisted_hash = Some(hash);
            self.cache_needs_resave = any_young;
            self.cache_revalidated = false;
        }
        seeded
    }

    /// Enqueue visible expanded directories immediately at startup without waiting for the first UI frame.
    pub fn prefetch_startup(&mut self) {
        if self.async_loader.is_none() {
            return;
        }
        let shown = self.focus.as_ref().map_or_else(|| self.roots.clone(), |f| vec![f.clone()]);
        let mut stack: Vec<(PathBuf, bool)> = Vec::new();
        for r in shown.into_iter().rev() {
            if self.expanded.contains(&r) {
                stack.push((r, true));
            }
        }
        while let Some((dir, is_top)) = stack.pop() {
            if self.children.contains_key(&dir) {
                if !self.loading.contains_key(&dir) {
                    let hint = self.revalidate_hint(&dir);
                    self.enqueue(dir.clone(), LoadPriority::Low, hint);
                }
                if let Some(kids) = self.children.get(&dir) {
                    for e in kids.iter().rev() {
                        if e.is_dir && self.expanded.contains(&e.path) {
                            stack.push((e.path.clone(), false));
                        }
                    }
                }
            } else if !self.loading.contains_key(&dir) {
                let prio = if is_top || !self.restored_expanded.contains(&dir) { LoadPriority::High } else { LoadPriority::Normal };
                self.enqueue(dir, prio, None);
            }
        }
        if self.pending_restore {
            self.pending_restore = false;
            let mut restored: Vec<PathBuf> = self
                .restored_expanded
                .iter()
                .filter(|p| {
                    self.focus.as_ref().is_none_or(|f| p.starts_with(f))
                        && self.is_ancestry_expanded(p)
                        && !self.children.contains_key(p.as_path())
                        && !self.loading.contains_key(p.as_path())
                })
                .cloned()
                .collect();
            restored.sort_by_key(|p| p.components().count());
            for p in restored {
                self.enqueue(p, LoadPriority::Normal, None);
            }
        }
        // Also revalidate any cache-seeded visible expanded directories whose parent exceeded cache entry caps.
        let extra: Vec<PathBuf> = self
            .children
            .keys()
            .filter(|p| self.is_dir_visible_expanded(p) && !self.loading.contains_key(p.as_path()))
            .cloned()
            .collect();
        for p in extra {
            let hint = self.revalidate_hint(&p);
            self.enqueue(p, LoadPriority::Low, hint);
        }
    }

    /// The cache content and its hash. `with_entries: false` skips cloning the entries (they are not part of the hash).
    fn collect_cache_dirs(&self, with_entries: bool) -> (u64, Vec<CachedDir>) {
        let mut paths: Vec<&PathBuf> = self
            .children
            .keys()
            .filter(|p| self.expanded.contains(*p) && self.roots.iter().any(|r| p.starts_with(r)))
            .collect();
        paths.sort_by(|a, b| a.components().count().cmp(&b.components().count()).then_with(|| a.as_os_str().cmp(b.as_os_str())));

        let mut dirs = Vec::with_capacity(paths.len().min(MAX_CACHED_DIRS));
        let mut total_entries = 0usize;
        for p in paths {
            if dirs.len() >= MAX_CACHED_DIRS {
                break;
            }
            let Some(path_str) = p.to_str() else { continue };
            let Some(entries) = self.children.get(p) else { continue };
            if total_entries + entries.len() > MAX_CACHED_ENTRIES {
                continue;
            }
            let mut cached_entries = Vec::with_capacity(if with_entries { entries.len() } else { 0 });
            let mut all_utf8 = true;
            for e in entries {
                let Some(ep) = e.path.to_str() else {
                    all_utf8 = false;
                    break;
                };
                if with_entries {
                    cached_entries.push(CachedEntry { name: e.name.clone(), path: ep.to_string(), is_dir: e.is_dir, kind: kind_to_u8(e.kind) });
                }
            }
            if !all_utf8 {
                continue;
            }
            total_entries += entries.len();
            let (dir_mtime_ms, content_hash, has_symlinks) = match self.dir_meta.get(p) {
                Some(m) => (m.dir_mtime_ms, m.content_hash, m.has_symlinks),
                None => (None, entries_content_hash(entries), false),
            };
            dirs.push(CachedDir { path: path_str.to_string(), dir_mtime_ms, content_hash, has_symlinks, entries: cached_entries });
        }

        let mut h = DetHasher::new();
        h.write_u64(self.config_hash());
        h.write_u64(self.roots_hash());
        h.write_u64(dirs.len() as u64);
        for d in &dirs {
            h.write_str(&d.path);
            match d.dir_mtime_ms {
                Some(ms) => {
                    h.write_u8(1);
                    h.write_u64(ms);
                }
                None => h.write_u8(0),
            }
            h.write_u64(d.content_hash);
            h.write_u8(d.has_symlinks as u8);
        }
        (h.finish(), dirs)
    }

    /// Build a cache snapshot when expanded loaded directories differ from the last persisted state
    /// (or once a young-mtime seeded cache has finished revalidating and needs its `saved_at_ms` refreshed).
    pub fn cache_snapshot(&self) -> Option<(u64, TreeCacheSnapshot)> {
        let (hash, dirs) = self.collect_cache_dirs(true);
        if dirs.is_empty() && self.last_persisted_hash.is_none() {
            return None;
        }
        let force_resave = self.cache_needs_resave && self.cache_revalidated && !self.has_pending_loads();
        if self.last_persisted_hash == Some(hash) && !force_resave {
            return None;
        }
        Some((hash, TreeCacheSnapshot { saved_at_ms: now_ms(), config_hash: self.config_hash(), roots_hash: self.roots_hash(), dirs }))
    }

    /// Record that the snapshot matching `hash` has been persisted to disk.
    pub fn mark_cache_persisted(&mut self, hash: u64) {
        self.last_persisted_hash = Some(hash);
        self.cache_needs_resave = false;
    }

    /// Atomically persist a tree cache snapshot to `path`.
    pub fn save_cache_snapshot(path: &Path, snap: &TreeCacheSnapshot) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(snap.dirs.len() * 256 + 64);
        bytes.extend_from_slice(TREE_CACHE_MAGIC);
        bytes.extend_from_slice(&TREE_CACHE_VERSION.to_le_bytes());
        let payload = postcard::to_stdvec(snap).map_err(io::Error::other)?;
        bytes.extend_from_slice(&payload);
        if bytes.len() > MAX_TREE_CACHE_BYTES {
            return Err(io::Error::other("tree cache snapshot exceeds 8 MiB cap"));
        }
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        crate::textio::atomic_write(path, &bytes)
    }

    fn is_ancestry_expanded(&self, dir: &Path) -> bool {
        let Some(root) = self.roots.iter().filter(|r| dir.starts_with(r)).max_by_key(|r| r.components().count()) else {
            return false;
        };
        let mut cur = root.clone();
        if !self.expanded.contains(&cur) {
            return false;
        }
        if let Ok(rel) = dir.strip_prefix(root) {
            for c in rel.components() {
                cur.push(c);
                if !self.expanded.contains(&cur) {
                    return false;
                }
            }
        }
        true
    }

    fn is_dir_visible_expanded(&self, dir: &Path) -> bool {
        if let Some(focus) = &self.focus {
            if !dir.starts_with(focus) {
                return false;
            }
            let mut cur = focus.clone();
            if !self.expanded.contains(&cur) {
                return false;
            }
            if let Ok(rel) = dir.strip_prefix(focus) {
                for c in rel.components() {
                    cur.push(c);
                    if !self.expanded.contains(&cur) {
                        return false;
                    }
                }
            }
            true
        } else {
            self.is_ancestry_expanded(dir)
        }
    }

    /// Drain completed background directory reads. Returns true if any load result was applied.
    pub fn poll_loads(&mut self) -> bool {
        let mut any = false;
        while let Some(res) = self.async_loader.as_mut().and_then(|l| l.try_recv()) {
            if res.load_gen != self.load_gen {
                continue;
            }
            any = true;
            self.cache_revalidated = true;
            if self.loading.get(&res.dir) == Some(&res.seq) {
                self.loading.remove(&res.dir);
            }
            self.restored_expanded.remove(&res.dir);
            match res.outcome {
                LoadOutcome::Unchanged => {}
                LoadOutcome::Loaded { entries, meta, missing, error } => {
                    if missing && self.focus.as_deref() == Some(res.dir.as_path()) {
                        self.focus = None;
                        self.dirty = true;
                    }
                    let prev_hash = self.dir_meta.get(&res.dir).map(|m| m.content_hash);
                    let same_error = self.errors.get(&res.dir) == error.as_ref();
                    let same_entries = same_error && prev_hash == Some(meta.content_hash) && self.children.get(&res.dir) == Some(&entries);
                    match error {
                        Some(e) => self.errors.insert(res.dir.clone(), e),
                        None => self.errors.remove(&res.dir),
                    };
                    self.dir_meta.insert(res.dir.clone(), meta);
                    if !same_entries {
                        self.children.insert(res.dir, entries);
                        self.dirty = true;
                    }
                }
            }
        }
        any
    }

    pub fn has_pending_loads(&self) -> bool {
        !self.loading.is_empty()
    }

    pub fn is_loading(&self, dir: &Path) -> bool {
        self.loading.contains_key(dir)
    }

    /// Pause or resume background worker dequeuing (for deterministic UI/skeleton tests).
    #[doc(hidden)]
    pub fn set_async_paused(&self, paused: bool) {
        if let Some(loader) = &self.async_loader {
            let mut q = loader.shared.lock.lock();
            q.paused = paused;
            if !paused {
                loader.shared.work_cv.notify_all();
            }
        }
    }

    /// Block until all reachable expanded directories have finished loading (for tests).
    pub fn wait_idle(&mut self) {
        if self.async_loader.is_none() {
            return;
        }
        let _ = self.rows();
        loop {
            self.poll_loads();
            if self.dirty {
                let _ = self.rows();
            }
            if !self.has_pending_loads() && !self.dirty {
                break;
            }
            if let Some(loader) = &self.async_loader {
                let mut q = loader.shared.lock.lock();
                if q.active > 0 || !q.queues_empty() {
                    loader.shared.idle_cv.wait_for(&mut q, Duration::from_millis(50));
                }
            }
        }
    }

    fn insert_expanded(&mut self, path: &Path) -> bool {
        if self.expanded.contains(path) {
            return false;
        }
        self.expanded.insert(path.to_path_buf());
        self.expanded_hash ^= path_hash(path);
        let _ = self.sorted_expanded.take();
        true
    }

    fn remove_expanded(&mut self, path: &Path) -> bool {
        if self.expanded.remove(path) {
            self.expanded_hash ^= path_hash(path);
            let _ = self.sorted_expanded.take();
            true
        } else {
            false
        }
    }

    pub fn set_roots(&mut self, roots: Vec<PathBuf>) {
        if self.roots != roots {
            self.roots = roots;
            if self.focus.as_ref().is_some_and(|f| !self.roots.iter().any(|r| f.starts_with(r))) {
                self.focus = None;
            }
            self.sync_loader_config();
            self.dirty = true;
        }
    }

    /// Reorders every folder; the listings are re-read and sorted once, not per frame.
    pub fn set_sort(&mut self, sort: TreeSort) {
        if self.sort != sort {
            self.sort = sort;
            self.children.clear();
            self.dir_meta.clear();
            self.sync_loader_config();
            self.dirty = true;
        }
    }

    pub fn focus(&self) -> Option<&Path> {
        self.focus.as_deref()
    }

    /// Show only `dir` (opened) as the root; `None` shows the workspace roots again. Ignored outside the roots.
    pub fn set_focus(&mut self, dir: Option<PathBuf>) {
        if dir.as_ref().is_some_and(|d| !self.roots.iter().any(|r| d.starts_with(r))) || self.focus == dir {
            return;
        }
        if let Some(d) = &dir {
            self.restored_expanded.remove(d);
            self.insert_expanded(d);
        }
        self.focus = dir;
        self.dirty = true;
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.children.clear();
            self.dir_meta.clear();
            self.sync_loader_config();
            self.dirty = true;
        }
    }

    /// Hide files that are neither notes nor images (`show_other == false`) and entries matching `excludes`.
    pub fn set_filter(&mut self, show_other: bool, excludes: ExcludeSet) {
        if self.show_other == show_other && self.excludes == excludes {
            return;
        }
        self.show_other = show_other;
        self.excludes = excludes;
        self.children.clear();
        self.dir_meta.clear();
        self.sync_loader_config();
        self.dirty = true;
    }

    /// Folder notes on/off and whether the note row inside its folder is left out of the tree.
    pub fn set_folder_notes(&mut self, on: bool, hide: bool) {
        if (self.folder_notes, self.folder_note_hide) != (on, hide) {
            (self.folder_notes, self.folder_note_hide) = (on, hide);
            self.dirty = true;
        }
    }

    /// Start a low-priority background read of one collapsed folder, so its row can tell whether it has a folder note.
    pub fn prefetch_dir(&mut self, dir: &Path) {
        if self.folder_notes && self.async_loader.is_some() && !self.children.contains_key(dir) && !self.loading.contains_key(dir) {
            self.enqueue(dir.to_path_buf(), LoadPriority::Low, None);
        }
    }

    /// Folder note of a loaded folder that is not a shown root; `None` when off or not loaded.
    fn folder_note_of(&self, dir: &Path) -> Option<PathBuf> {
        if !self.folder_notes || self.focus.as_deref() == Some(dir) || self.roots.iter().any(|r| r == dir) {
            return None;
        }
        folder_note(&dir.file_name()?.to_string_lossy(), self.children.get(dir)?).map(Path::to_path_buf)
    }

    /// Cloned children of `dir`, without the folder note when it is hidden.
    fn shown_children(&self, dir: &Path) -> Vec<TreeEntry> {
        let mut kids = self.children.get(dir).cloned().unwrap_or_default();
        if self.folder_note_hide {
            if let Some(n) = self.folder_note_of(dir) {
                kids.retain(|e| e.path != n);
            }
        }
        kids
    }

    fn entry_row(&self, depth: u16, entry: TreeEntry) -> TreeRow {
        let expanded = entry.is_dir && self.expanded.contains(&entry.path);
        let folder_note = if entry.is_dir { self.folder_note_of(&entry.path) } else { None };
        TreeRow { depth, entry, expanded, is_root: false, skeleton: false, note: None, folder_note }
    }

    /// Number of directory reads performed so far (for tests and diagnostics).
    pub fn dir_reads(&self) -> usize {
        self.reads.load(AtomicOrdering::Relaxed)
    }

    fn root_entry(p: &Path) -> TreeEntry {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
        TreeEntry { name, path: p.to_path_buf(), is_dir: true, kind: FileKind::Other }
    }

    fn load_children(&mut self, dir: &Path) {
        if !self.children.contains_key(dir) {
            self.reads.fetch_add(1, AtomicOrdering::Relaxed);
            let rel = rel_base(&self.roots, dir);
            let (v, meta) = match read_dir_with_meta(dir, self.show_hidden, self.show_other, &self.excludes, &rel, self.sort) {
                Ok(pair) => {
                    self.errors.remove(dir);
                    pair
                }
                Err(e) => {
                    self.errors.insert(dir.to_path_buf(), e.to_string());
                    (Vec::new(), DirMeta { dir_mtime_ms: None, content_hash: entries_content_hash(&[]), has_symlinks: false, saved_at_ms: None })
                }
            };
            self.children.insert(dir.to_path_buf(), v);
            self.dir_meta.insert(dir.to_path_buf(), meta);
        }
    }

    fn empty_note(&self, dir: &Path) -> RowNote {
        self.errors.get(dir).map_or(RowNote::Empty, |e| RowNote::Unreadable(e.clone()))
    }

    fn push_children(&mut self, dir: &Path, depth: u16, out: &mut Vec<TreeRow>) {
        self.load_children(dir);
        // Owned copy: the recursion below needs &mut self, so the map cannot be borrowed across it.
        let kids = self.shown_children(dir);
        if kids.is_empty() {
            out.push(note_row(depth, self.empty_note(dir)));
        }
        for entry in kids {
            if entry.is_dir && self.expanded.contains(&entry.path) {
                self.load_children(&entry.path.clone()); // the folder row needs to know its folder note
            }
            let row = self.entry_row(depth, entry);
            let path = row.expanded.then(|| row.entry.path.clone());
            out.push(row);
            if let Some(p) = path {
                self.push_children(&p, depth.saturating_add(1), out);
            }
        }
    }

    fn push_children_async(&mut self, dir: &Path, depth: u16, default_prio: LoadPriority, out: &mut Vec<TreeRow>) {
        if !self.children.contains_key(dir) {
            if !self.loading.contains_key(dir) {
                let prio = if depth <= 1 && (self.focus.as_deref() == Some(dir) || self.roots.iter().any(|r| r == dir)) {
                    LoadPriority::High
                } else if self.restored_expanded.contains(dir) {
                    LoadPriority::Normal
                } else {
                    default_prio
                };
                self.enqueue(dir.to_path_buf(), prio, None);
            }
            if !self.children.contains_key(dir) {
                for _ in 0..SKELETON_ROWS {
                    out.push(skeleton_row(depth));
                }
                return;
            }
        }
        let kids = self.shown_children(dir);
        if kids.is_empty() {
            out.push(note_row(depth, self.empty_note(dir)));
        }
        for entry in kids {
            let row = self.entry_row(depth, entry);
            let path = row.expanded.then(|| row.entry.path.clone());
            out.push(row);
            if let Some(p) = path {
                self.push_children_async(&p, depth.saturating_add(1), LoadPriority::High, out);
            }
        }
    }

    pub fn rows(&mut self) -> &[TreeRow] {
        if self.dirty {
            let is_async = self.async_loader.is_some();
            if !is_async {
                // The entered folder was renamed or deleted: back to the roots (stat only when the tree changed).
                if self.focus.as_ref().is_some_and(|f| !f.is_dir()) {
                    self.focus = None;
                }
            }
            let mut out = Vec::new();
            let shown = self.focus.as_ref().map_or_else(|| self.roots.clone(), |f| vec![f.clone()]);
            for r in shown {
                let expanded = self.expanded.contains(&r);
                out.push(TreeRow { depth: 0, entry: Self::root_entry(&r), expanded, is_root: true, skeleton: false, note: None, folder_note: None });
                if expanded {
                    if is_async {
                        self.push_children_async(&r, 1, LoadPriority::High, &mut out);
                    } else {
                        self.push_children(&r, 1, &mut out);
                    }
                }
            }
            if is_async && self.pending_restore {
                self.pending_restore = false;
                let mut restored: Vec<PathBuf> = self
                    .restored_expanded
                    .iter()
                    .filter(|p| {
                        self.focus.as_ref().is_none_or(|f| p.starts_with(f))
                            && self.is_ancestry_expanded(p)
                            && !self.children.contains_key(p.as_path())
                            && !self.loading.contains_key(p.as_path())
                    })
                    .cloned()
                    .collect();
                restored.sort_by_key(|p| p.components().count());
                for p in restored {
                    self.enqueue(p, LoadPriority::Normal, None);
                }
            }
            self.rows = out;
            self.dirty = false;
        }
        &self.rows
    }

    pub fn toggle(&mut self, path: &Path) {
        let e = !self.is_expanded(path);
        self.set_expanded(path, e);
    }

    pub fn set_expanded(&mut self, path: &Path, expanded: bool) {
        self.restored_expanded.remove(path);
        let changed = if expanded { self.insert_expanded(path) } else { self.remove_expanded(path) };
        if changed {
            self.dirty = true;
        }
    }

    /// Close every folder (the entered folder, if any, stays open).
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.restored_expanded.clear();
        self.pending_restore = false;
        self.expanded_hash = 0;
        if let Some(f) = &self.focus {
            self.expanded_hash = path_hash(f);
            self.expanded.insert(f.clone());
        }
        let _ = self.sorted_expanded.take();
        self.dirty = true;
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Order-independent 64-bit hash of the expanded directory set in O(1).
    pub fn expanded_hash(&self) -> u64 {
        self.expanded_hash
    }

    pub fn expanded(&self) -> Vec<PathBuf> {
        self.sorted_expanded
            .get_or_init(|| {
                let mut v: Vec<PathBuf> = self.expanded.iter().cloned().collect();
                v.sort();
                v
            })
            .clone()
    }

    /// Forget the cached listing of `dir` (re-read on next `rows`, or stale-while-revalidate in async mode).
    pub fn invalidate(&mut self, dir: &Path) {
        if self.async_loader.is_none() {
            self.dir_meta.remove(dir);
            if self.children.remove(dir).is_some() {
                self.dirty = true;
            }
            return;
        }
        if self.children.contains_key(dir) {
            if self.is_dir_visible_expanded(dir) {
                // Stale-while-revalidate: keep existing rows visible without skeleton flicker while refreshing.
                self.enqueue(dir.to_path_buf(), LoadPriority::Low, None);
            } else {
                self.children.remove(dir);
                self.dir_meta.remove(dir);
                self.dirty = true; // the parent's row caches the folder note
            }
        } else if self.loading.contains_key(dir) && self.is_dir_visible_expanded(dir) {
            self.enqueue(dir.to_path_buf(), LoadPriority::Low, None);
        }
    }

    pub fn invalidate_all(&mut self) {
        if self.async_loader.is_none() {
            self.children.clear();
            self.dir_meta.clear();
            self.dirty = true;
            return;
        }
        let cached: Vec<PathBuf> = self.children.keys().cloned().collect();
        for dir in cached {
            if self.is_dir_visible_expanded(&dir) {
                self.enqueue(dir, LoadPriority::Low, None);
            } else {
                self.children.remove(&dir);
                self.dir_meta.remove(&dir);
                self.dirty = true;
            }
        }
        let in_flight: Vec<PathBuf> = self.loading.keys().cloned().collect();
        for dir in in_flight {
            if self.is_dir_visible_expanded(&dir) {
                self.enqueue(dir, LoadPriority::Low, None);
            }
        }
    }

    /// Expand all ancestors of `path` (within its root) and return its row index.
    pub fn reveal(&mut self, path: &Path) -> Option<usize> {
        let root = self.roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.components().count())?.clone();
        let mut cur = root.clone();
        self.set_expanded(&cur, true);
        let mut ancestors = vec![cur.clone()];
        if let Ok(rel) = path.strip_prefix(&root) {
            let comps: Vec<_> = rel.components().collect();
            let mut n = comps.len().saturating_sub(1);
            // A hidden folder note is shown by its folder row: revealing it must not open the folder.
            // shortcut: only known when the folder's listing is loaded, else the folder opens as for any note.
            let parent = path.parent().filter(|_| n > 0);
            if parent.is_some_and(|d| self.folder_note_hide && self.folder_note_of(d).is_some_and(|f| f == path)) {
                n -= 1;
            }
            for c in comps.iter().take(n) {
                cur.push(c);
                self.set_expanded(&cur, true);
                ancestors.push(cur.clone());
            }
        }
        if self.async_loader.is_some() {
            for anc in &ancestors {
                if !self.children.contains_key(anc) && !self.loading.contains_key(anc) {
                    self.enqueue(anc.clone(), LoadPriority::High, None);
                } else if self.loading.contains_key(anc) {
                    let seq = self.next_seq;
                    let updated = self.async_loader.as_mut().and_then(|l| l.upgrade_if_queued(anc, LoadPriority::High, seq));
                    if let Some(eff) = updated {
                        if eff == seq {
                            self.next_seq += 1;
                        }
                        self.loading.insert(anc.clone(), eff);
                    }
                }
            }
        }
        let rows = self.rows();
        // A hidden folder note is shown by its folder row.
        rows.iter().position(|r| !r.skeleton && r.entry.path == path).or_else(|| rows.iter().position(|r| r.folder_note.as_deref() == Some(path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cache of `n_dirs` expanded synthetic directories with `per_dir` files each, plus a model ready to seed from it.
    fn synthetic_cache(n_dirs: usize, per_dir: usize) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("root");
        let m = expanded_model(&root, n_dirs);
        let mut dirs = Vec::new();
        for d in 0..n_dirs {
            let dir = root.join(format!("dir{d:03}"));
            let entries: Vec<TreeEntry> = (0..per_dir)
                .map(|i| TreeEntry { name: format!("note {i:03}.md"), path: dir.join(format!("note {i:03}.md")), is_dir: false, kind: FileKind::Markdown })
                .collect();
            let entries_c = entries.iter().map(|e| CachedEntry { name: e.name.clone(), path: e.path.to_str().unwrap().to_string(), is_dir: false, kind: kind_to_u8(e.kind) }).collect();
            dirs.push(CachedDir { path: dir.to_str().unwrap().to_string(), dir_mtime_ms: Some(1), content_hash: entries_content_hash(&entries), has_symlinks: false, entries: entries_c });
        }
        let snap = TreeCacheSnapshot { saved_at_ms: now_ms(), config_hash: m.config_hash(), roots_hash: m.roots_hash(), dirs };
        let cache = t.path().join("tree.bin");
        TreeModel::save_cache_snapshot(&cache, &snap).unwrap();
        (t, cache, root)
    }

    fn expanded_model(root: &Path, n_dirs: usize) -> TreeModel {
        let mut m = TreeModel::new(vec![root.to_path_buf()], false);
        for d in 0..n_dirs {
            m.set_expanded(&root.join(format!("dir{d:03}")), true);
        }
        m.set_expanded(root, true);
        m
    }

    #[test]
    fn seeding_records_the_hash_of_what_was_seeded() {
        let (_t, cache, root) = synthetic_cache(5, 7);
        let mut m = expanded_model(&root, 5);
        assert_eq!(m.seed_from_cache(&cache), 5);
        assert_eq!(m.last_persisted_hash, Some(m.collect_cache_dirs(true).0));
        assert!(m.cache_snapshot().is_none(), "a freshly seeded cache needs no resave");
    }

    /// `cargo test -p iron-sheets-core --lib seed_time -- --ignored --nocapture`
    #[test]
    #[ignore = "perf measurement"]
    fn seed_time_at_the_cache_caps() {
        let (_t, cache, root) = synthetic_cache(MAX_CACHED_DIRS, 97);
        let mut best = f64::MAX;
        for _ in 0..7 {
            let mut m = expanded_model(&root, MAX_CACHED_DIRS);
            let t = std::time::Instant::now();
            assert_eq!(m.seed_from_cache(&cache), MAX_CACHED_DIRS);
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("PERF tree seed_from_cache 256 dirs / {} entries: best of 7 {best:.1} ms", MAX_CACHED_DIRS * 97);
    }

    #[test]
    fn sort_label_names_key_and_order() {
        assert_eq!(TreeSort::default().label(), "Name, asc.");
        assert_eq!(TreeSort { key: SortKey::Modified, desc: true }.label(), "Modified, desc.");
    }

    #[test]
    fn collapse_all_closes_folders_but_keeps_the_entered_one() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        let mut m = TreeModel::new(vec![t.path().to_path_buf()], false);
        m.set_expanded(t.path(), true);
        m.set_expanded(&t.path().join("a"), true);
        m.set_focus(Some(t.path().join("a")));
        m.collapse_all();
        assert_eq!(m.expanded(), vec![t.path().join("a")]);
        m.set_focus(None);
        m.collapse_all();
        assert!(m.expanded().is_empty());
    }

    fn names(rows: &[TreeRow]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", " ".repeat(r.depth as usize), r.entry.name)).collect()
    }

    fn entry(name: &str, is_dir: bool) -> TreeEntry {
        TreeEntry { name: name.into(), path: PathBuf::from("/r/Foo").join(name), is_dir, kind: if is_dir { FileKind::Other } else { FileKind::from_path(Path::new(name)) } }
    }

    #[test]
    fn folder_note_detection() {
        let name = |n: Option<&Path>| n.map(|p| p.file_name().unwrap().to_string_lossy().into_owned());
        let kids = |v: &[(&str, bool)]| v.iter().map(|(n, d)| entry(n, *d)).collect::<Vec<_>>();
        assert_eq!(name(folder_note("Foo", &kids(&[("a.md", false), ("Foo.md", false)]))), Some("Foo.md".into()));
        assert_eq!(name(folder_note("Foo", &kids(&[("Foo.typ", false)]))), Some("Foo.typ".into()));
        assert_eq!(name(folder_note("Foo", &kids(&[("Foo.typ", false), ("Foo.md", false)]))), Some("Foo.md".into()), ".md wins");
        assert_eq!(folder_note("Foo", &kids(&[("foo.md", false), ("Foo.png", false), ("Foo.txt", false), ("Foo.md.bak", false)])), None, "exact stem, note kinds only");
        assert_eq!(folder_note("Foo", &kids(&[("Foo.md", true)])), None, "a folder is no note");
        assert_eq!(name(folder_note("Foo", &kids(&[("Foo.MD", false)]))), Some("Foo.MD".into()));
        assert_eq!(folder_note("Foo", &[]), None);
    }

    #[test]
    fn folder_note_row_follows_a_deleted_note_after_invalidate() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("Foo")).unwrap();
        std::fs::write(r.join("Foo/Foo.md"), "").unwrap();
        let mut m = TreeModel::new(vec![r.clone()], false);
        m.toggle(&r);
        m.toggle(&r.join("Foo"));
        m.rows(); // loads Foo
        m.toggle(&r.join("Foo")); // collapsed
        assert!(m.rows()[1].folder_note.is_some());
        std::fs::remove_file(r.join("Foo/Foo.md")).unwrap();
        m.invalidate(&r.join("Foo"));
        assert!(m.rows()[1].folder_note.is_none());
    }

    #[test]
    fn folder_note_rows_hide_and_reveal() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("Foo")).unwrap();
        std::fs::create_dir_all(r.join("Plain")).unwrap();
        for f in ["Foo/Foo.md", "Foo/Other.md", "Plain/x.md", "root.md"] {
            std::fs::write(r.join(f), "").unwrap();
        }
        let mut m = TreeModel::new(vec![r.clone()], false);
        m.toggle(&r);
        // Not loaded yet: unknown, so no folder note.
        assert!(m.rows().iter().all(|x| x.folder_note.is_none()));
        m.toggle(&r.join("Foo"));
        m.toggle(&r.join("Plain"));
        let note = r.join("Foo/Foo.md");
        let foo = m.rows().iter().find(|x| x.entry.name == "Foo").unwrap();
        assert_eq!(foo.folder_note.as_deref(), Some(note.as_path()));
        assert!(m.rows().iter().find(|x| x.entry.name == "Plain").unwrap().folder_note.is_none());
        assert!(m.rows()[0].folder_note.is_none(), "a root has none");
        assert_eq!(names(m.rows()), vec!["root", " Foo", "  Other.md", " Plain", "  x.md", " root.md"], "note row hidden");
        m.toggle(&r.join("Foo")); // loaded, now collapsed
        assert_eq!(m.reveal(&note), Some(1), "reveal selects the folder row");
        assert!(!m.is_expanded(&r.join("Foo")), "and does not open it");
        m.toggle(&r.join("Foo"));
        m.set_folder_notes(true, false);
        assert!(names(m.rows()).contains(&"  Foo.md".to_string()));
        let i = m.reveal(&note).unwrap();
        assert_eq!(m.rows()[i].entry.name, "Foo.md");
        m.set_folder_notes(false, true);
        assert!(m.rows().iter().all(|x| x.folder_note.is_none()));
        assert!(names(m.rows()).contains(&"  Foo.md".to_string()), "switched off: nothing hidden");
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["a10", "a2", "B", "a02x", "a1"];
        v.sort_by(|a, b| natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
        assert_eq!(v, vec!["a1", "a2", "a02x", "a10", "B"]);
    }

    #[test]
    fn sorted_and_hidden() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("zdir")).unwrap();
        std::fs::create_dir(t.path().join(".git")).unwrap();
        for f in ["b.md", "A.md", "note10.md", "note2.md", ".hidden", "p.png"] {
            std::fs::write(t.path().join(f), "").unwrap();
        }
        let v = read_dir_sorted(t.path(), false, TreeSort::default()).unwrap();
        let n: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(n, vec!["zdir", "A.md", "b.md", "note2.md", "note10.md", "p.png"]);
        assert_eq!(v[5].kind, FileKind::Image);
        assert_eq!(read_dir_sorted(t.path(), true, TreeSort::default()).unwrap().len(), 8);
        assert!(read_dir_sorted(&t.path().join("nope"), false, TreeSort::default()).is_err());
    }

    #[test]
    fn sort_orders_keep_folders_first() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("adir")).unwrap();
        let base = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        // Names sort one way and modification times the other way round.
        for (i, f) in ["f1.md", "f2.md", "f10.md"].iter().enumerate() {
            let file = std::fs::File::create(t.path().join(f)).unwrap();
            file.set_modified(base + std::time::Duration::from_secs(100 - i as u64)).unwrap();
        }
        let list = |key, desc| -> Vec<String> { read_dir_sorted(t.path(), false, TreeSort { key, desc }).unwrap().into_iter().map(|e| e.name).collect() };
        assert_eq!(list(SortKey::Name, false), ["adir", "f1.md", "f2.md", "f10.md"]);
        assert_eq!(list(SortKey::Name, true), ["adir", "f10.md", "f2.md", "f1.md"]);
        assert_eq!(list(SortKey::Modified, false), ["adir", "f10.md", "f2.md", "f1.md"]);
        assert_eq!(list(SortKey::Modified, true), ["adir", "f1.md", "f2.md", "f10.md"]);
        // Created time is missing on some file systems: the listing must still come out whole.
        assert_eq!(list(SortKey::Created, false).len(), 4);
    }

    #[test]
    fn sort_change_rereads_and_focus_replaces_roots() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        std::fs::write(r.join("a/x.md"), "").unwrap();
        std::fs::write(r.join("z.md"), "").unwrap();
        let mut m = TreeModel::new(vec![r.clone()], false);
        m.toggle(&r);
        m.set_sort(TreeSort { key: SortKey::Name, desc: true });
        assert_eq!(names(m.rows()), vec!["root", " a", " z.md"]);
        m.set_focus(Some(r.join("a")));
        assert_eq!(names(m.rows()), vec!["a", " b", " x.md"]);
        assert!(m.rows()[0].is_root);
        m.set_focus(Some(t.path().join("elsewhere")));
        assert_eq!(m.focus(), Some(r.join("a").as_path()), "outside the roots is ignored");
        m.set_roots(vec![t.path().join("other")]);
        assert_eq!(m.focus(), None);
        m.set_roots(vec![r.clone()]);
        m.set_focus(None);
        assert_eq!(names(m.rows())[0], "root");
    }

    #[test]
    fn lazy_and_reveal() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("a/b/c")).unwrap();
        std::fs::create_dir_all(r.join("other")).unwrap();
        std::fs::write(r.join("a/b/c/deep.md"), "").unwrap();
        std::fs::write(r.join("a/x.md"), "").unwrap();
        let mut m = TreeModel::new(vec![r.clone()], false);
        assert_eq!(names(m.rows()), vec!["root"]);
        assert_eq!(m.dir_reads(), 0);
        m.toggle(&r);
        assert_eq!(names(m.rows()), vec!["root", " a", " other"]);
        assert_eq!(m.dir_reads(), 1);
        let idx = m.reveal(&r.join("a/b/c/deep.md")).unwrap();
        assert_eq!(m.rows()[idx].entry.name, "deep.md");
        assert_eq!(m.rows()[idx].depth, 4);
        assert_eq!(m.dir_reads(), 4);
        // collapsing does not re-read; re-expanding uses the cache
        m.toggle(&r.join("a"));
        assert_eq!(names(m.rows()), vec!["root", " a", " other"]);
        m.toggle(&r.join("a"));
        assert_eq!(m.dir_reads(), 4);
        // external change needs invalidate
        std::fs::write(r.join("a/new.md"), "").unwrap();
        assert!(!names(m.rows()).iter().any(|n| n.contains("new.md")));
        m.invalidate(&r.join("a"));
        assert!(names(m.rows()).iter().any(|n| n.contains("new.md")));
        assert!(m.expanded().contains(&r.join("a/b")));
        assert_eq!(m.reveal(&t.path().join("elsewhere.md")), None);
        m.invalidate_all();
        m.rows();
    }

    #[test]
    fn expanded_hash_is_order_independent_and_incremental() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<TreeModel>();

        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        let a = r.join("dir_01");
        let b = r.join("dir_02");
        let c = r.join("dir_03");
        let d = r.join("dir_00");

        let mut m1 = TreeModel::new(vec![r.clone()], false);
        let mut m2 = TreeModel::new(vec![r.clone()], false);
        assert_eq!(m1.expanded_hash(), 0);
        assert!(m1.expanded().is_empty());

        m1.set_expanded(&a, true);
        m1.set_expanded(&b, true);
        m1.set_expanded(&c, true);

        m2.set_expanded(&c, true);
        m2.set_expanded(&a, true);
        m2.set_expanded(&b, true);

        assert_eq!(m1.expanded_hash(), m2.expanded_hash());
        assert_eq!(m1.expanded(), m2.expanded());
        assert_ne!(m1.expanded_hash(), 0);

        // Removing b matches a fresh tree with only a and c.
        m1.set_expanded(&b, false);
        let mut m3 = TreeModel::new(vec![r.clone()], false);
        m3.set_expanded(&c, true);
        m3.set_expanded(&a, true);
        assert_eq!(m1.expanded_hash(), m3.expanded_hash());
        assert_eq!(m1.expanded(), vec![a.clone(), c.clone()]);

        // Similar sibling paths produce distinct hashes and clean XOR cancellation.
        m3.set_expanded(&d, true);
        assert_ne!(m3.expanded_hash(), m1.expanded_hash());
        m3.set_expanded(&d, false);
        assert_eq!(m3.expanded_hash(), m1.expanded_hash());

        m1.set_expanded(&a, false);
        m1.set_expanded(&c, false);
        assert_eq!(m1.expanded_hash(), 0);
        assert!(m1.expanded().is_empty());
    }

    #[test]
    fn expanded_hash_invariants_and_sorted_cache_invalidation() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        let a = r.join("alpha");
        let b = r.join("beta");
        let c = r.join("gamma");
        let deep = b.join("sub");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("note.md"), "").unwrap();

        let mut m = TreeModel::new(vec![r.clone()], false);
        // Prime sorted_expanded cache while empty.
        assert!(m.expanded().is_empty());

        // Insert out of sorted order and verify cache invalidates on each mutation.
        m.set_expanded(&c, true);
        assert_eq!(m.expanded(), vec![c.clone()]);
        let h_c = m.expanded_hash();

        // Duplicate insert and absent remove keep hash and cached sorted vec invariant.
        m.set_expanded(&c, true);
        assert_eq!(m.expanded_hash(), h_c);
        assert_eq!(m.expanded(), vec![c.clone()]);
        m.set_expanded(&r.join("absent"), false);
        assert_eq!(m.expanded_hash(), h_c);
        assert_eq!(m.expanded(), vec![c.clone()]);

        // Toggle inserts `a`, then removes `a`.
        m.toggle(&a);
        assert_eq!(m.expanded(), vec![a.clone(), c.clone()]);
        assert_ne!(m.expanded_hash(), h_c);
        m.toggle(&a);
        assert_eq!(m.expanded_hash(), h_c);
        assert_eq!(m.expanded(), vec![c.clone()]);

        // set_focus outside roots is ignored.
        m.set_focus(Some(t.path().join("outside")));
        assert_eq!(m.expanded_hash(), h_c);
        assert_eq!(m.expanded(), vec![c.clone()]);

        // set_focus on unexpanded dir adds it identically to set_expanded.
        m.set_focus(Some(b.clone()));
        assert_eq!(m.expanded(), vec![b.clone(), c.clone()]);
        let mut expected_bc = TreeModel::new(vec![r.clone()], false);
        expected_bc.set_expanded(&b, true);
        expected_bc.set_expanded(&c, true);
        assert_eq!(m.expanded_hash(), expected_bc.expanded_hash());

        // Focusing an already-expanded dir does not alter hash; clearing focus leaves expanded intact.
        let h_bc = m.expanded_hash();
        m.set_focus(Some(c.clone()));
        assert_eq!(m.expanded_hash(), h_bc);
        assert_eq!(m.expanded(), vec![b.clone(), c.clone()]);
        m.set_focus(None);
        assert_eq!(m.expanded_hash(), h_bc);
        assert_eq!(m.expanded(), vec![b.clone(), c.clone()]);
        m.set_focus(Some(c.clone()));

        // collapse_all keeps the focused folder `c`.
        m.collapse_all();
        assert_eq!(m.expanded(), vec![c.clone()]);
        assert_eq!(m.expanded_hash(), h_c);
        // Repeated collapse_all is invariant.
        m.collapse_all();
        assert_eq!(m.expanded(), vec![c.clone()]);
        assert_eq!(m.expanded_hash(), h_c);

        // set_roots preserves expanded set/hash even when it clears focus.
        m.set_roots(vec![t.path().join("other")]);
        assert_eq!(m.focus(), None);
        assert_eq!(m.expanded_hash(), h_c);
        assert_eq!(m.expanded(), vec![c.clone()]);

        // collapse_all without focus clears everything back to 0.
        m.collapse_all();
        assert_eq!(m.expanded_hash(), 0);
        assert!(m.expanded().is_empty());

        // reveal updates expanded_hash and sorted expanded() identically to manual ancestor expansion.
        m.set_roots(vec![r.clone()]);
        assert!(m.reveal(&deep.join("note.md")).is_some());
        let mut expected_reveal = TreeModel::new(vec![r.clone()], false);
        expected_reveal.set_expanded(&r, true);
        expected_reveal.set_expanded(&b, true);
        expected_reveal.set_expanded(&deep, true);
        assert_eq!(m.expanded_hash(), expected_reveal.expanded_hash());
        assert_eq!(m.expanded(), expected_reveal.expanded());
    }

    #[test]
    fn filtered_read_dir_excludes_before_sort_and_metadata() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("skip_dir")).unwrap();
        std::fs::create_dir_all(r.join("sub")).unwrap();
        std::fs::write(r.join("sub/keep.md"), "").unwrap();
        std::fs::write(r.join("sub/ignore.bak"), "").unwrap();
        std::fs::write(r.join("note.md"), "").unwrap();
        std::fs::write(r.join("pic.png"), "").unwrap();
        std::fs::write(r.join("blob.bin"), "").unwrap();
        std::fs::write(r.join(".dot.md"), "").unwrap();

        let ex = ExcludeSet::new(&["skip_dir", "*.bak", "sub/ignore.bak"]);
        let v = read_dir_sorted_filtered(&r, false, false, &ex, "", TreeSort { key: SortKey::Modified, desc: false }).unwrap();
        let n: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(n, vec!["sub", "note.md", "pic.png"]);

        let sub = read_dir_sorted_filtered(&r.join("sub"), false, true, &ex, "sub", TreeSort::default()).unwrap();
        assert_eq!(sub.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["keep.md"]);
    }

    #[test]
    fn expanded_empty_folder_gets_an_empty_note_row() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("sub")).unwrap();
        for is_async in [false, true] {
            let mut m = TreeModel::new(vec![r.clone()], false);
            if is_async {
                m.enable_async(Arc::new(|| {}));
            }
            m.set_expanded(&r, true);
            m.set_expanded(&r.join("sub"), true);
            m.wait_idle();
            m.poll_loads();
            let rows = m.rows();
            let last = rows.last().unwrap();
            assert_eq!(last.note, Some(RowNote::Empty), "async={is_async}");
            assert!(last.skeleton && last.depth == 2, "a note row is not an entry");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_gets_an_error_note_row() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        let locked = r.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _restore = Restore(locked.clone());
        let readable = std::fs::read_dir(&locked).is_ok(); // running as root
        for is_async in [false, true] {
            let mut m = TreeModel::new(vec![r.clone()], false);
            if is_async {
                m.enable_async(Arc::new(|| {}));
            }
            m.set_expanded(&r, true);
            m.set_expanded(&locked, true);
            m.wait_idle();
            m.poll_loads();
            let note = m.rows().last().unwrap().note.clone();
            if !readable {
                assert!(matches!(note, Some(RowNote::Unreadable(ref e)) if !e.is_empty()), "async={is_async}: {note:?}");
            }
        }
    }

    #[test]
    fn async_lazy_spawn_skeleton_and_poll() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("sub")).unwrap();
        std::fs::write(r.join("a.md"), "").unwrap();
        std::fs::write(r.join("sub/b.md"), "").unwrap();

        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        let mut m = TreeModel::new(vec![r.clone()], false);
        m.enable_async(Arc::new(move || {
            w.fetch_add(1, AtomicOrdering::Relaxed);
        }));
        m.set_expanded(&r, true);
        // Zero reads before rows() / first load request.
        assert_eq!(m.dir_reads(), 0);
        assert!(!m.has_pending_loads());

        m.set_async_paused(true);
        let rows = m.rows();
        assert_eq!(rows.len(), 1 + SKELETON_ROWS);
        assert!(rows[0].is_root && !rows[0].skeleton);
        for (idx, row) in rows[1..].iter().enumerate() {
            assert!(row.skeleton, "row {idx} should be skeleton");
            assert_eq!(row.depth, 1);
            assert!(!row.is_root && !row.expanded);
        }
        assert!(m.has_pending_loads());
        assert!(m.is_loading(&r));

        m.set_async_paused(false);
        m.wait_idle();
        assert!(!m.has_pending_loads());
        assert!(!m.is_loading(&r));
        assert!(woken.load(AtomicOrdering::Relaxed) >= 1);
        assert_eq!(names(m.rows()), vec!["root", " sub", " a.md"]);
        assert_eq!(m.dir_reads(), 1, "collapsed sub must not be read");
    }

    #[test]
    fn async_priority_queue_orders_high_normal_low_and_upgrades() {
        let mut loader = TreeLoader::new(
            Arc::new(|| {}),
            1,
            vec![],
            false,
            true,
            ExcludeSet::default(),
            TreeSort::default(),
            Arc::new(AtomicUsize::new(0)),
        );
        // Mark spawned so enqueue doesn't start a background worker thread during queue-order inspection.
        loader.spawned = true;
        loader.enqueue(PathBuf::from("low_1"), LoadPriority::Low, 1, None);
        loader.enqueue(PathBuf::from("norm_1"), LoadPriority::Normal, 2, None);
        loader.enqueue(PathBuf::from("high_1"), LoadPriority::High, 3, None);
        loader.enqueue(PathBuf::from("norm_2"), LoadPriority::Normal, 4, None);
        // Upgrade low_1 -> High!
        assert_eq!(loader.upgrade_if_queued(Path::new("low_1"), LoadPriority::High, 5), Some(5));
        // Duplicate at same/lower priority is ignored.
        assert_eq!(loader.enqueue(PathBuf::from("high_1"), LoadPriority::Normal, 6, None), Some(3));

        let mut popped = Vec::new();
        {
            let mut q = loader.shared.lock.lock();
            while let Some(job) = pop_next(&mut q) {
                popped.push(job.dir);
            }
        }
        assert_eq!(
            popped,
            vec![
                PathBuf::from("high_1"),
                PathBuf::from("low_1"),
                PathBuf::from("norm_1"),
                PathBuf::from("norm_2"),
            ]
        );
    }

    #[test]
    fn async_top_level_roots_load_before_restored_nested_and_skip_collapsed() {
        let t = tempfile::tempdir().unwrap();
        let r1 = t.path().join("r1");
        let r2 = t.path().join("r2");
        let n1 = r1.join("nested");
        let collapsed = r1.join("collapsed");
        let skipped_grand = collapsed.join("grand");
        std::fs::create_dir_all(&n1).unwrap();
        std::fs::create_dir_all(&skipped_grand).unwrap();
        std::fs::create_dir_all(&r2).unwrap();
        std::fs::write(n1.join("in_nested.md"), "").unwrap();
        std::fs::write(skipped_grand.join("should_not_read.md"), "").unwrap();

        let mut m = TreeModel::new(vec![r1.clone(), r2.clone()], false);
        m.enable_async(Arc::new(|| {}));
        m.set_expanded(&r1, true);
        m.set_expanded(&r2, true);
        // `n1` has expanded parent `r1`; `skipped_grand` has collapsed parent `collapsed`.
        m.restore_expanded(&[r1.clone(), n1.clone(), skipped_grand.clone()]);
        m.set_async_paused(true);
        let _ = m.rows();

        // Check exact queue priority assignment before worker runs: roots in High, n1 in Normal, skipped_grand absent!
        {
            let loader = m.async_loader.as_ref().unwrap();
            let q = loader.shared.lock.lock();
            let high: Vec<_> = q.queues[LoadPriority::High as usize].iter().map(|j| j.dir.clone()).collect();
            let norm: Vec<_> = q.queues[LoadPriority::Normal as usize].iter().map(|j| j.dir.clone()).collect();
            assert_eq!(high, vec![r1.clone(), r2.clone()]);
            assert_eq!(norm, vec![n1.clone()]);
            assert!(!q.queued.contains_key(&skipped_grand));
        }

        m.set_async_paused(false);
        m.wait_idle();
        assert_eq!(m.dir_reads(), 3, "r1, r2, and n1 read; collapsed subtree never read");
        assert!(m.rows().iter().any(|row| row.entry.path == n1.join("in_nested.md")));
    }

    #[test]
    fn async_stale_while_revalidate_reveal_and_generation_cancellation() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        std::fs::write(r.join("z.md"), "").unwrap();
        std::fs::write(r.join("a/b/deep.md"), "").unwrap();

        let mut m = TreeModel::new(vec![r.clone()], false);
        m.enable_async(Arc::new(|| {}));
        m.set_expanded(&r, true);
        m.wait_idle();
        assert_eq!(names(m.rows()), vec!["root", " a", " z.md"]);

        // Stale-while-revalidate: invalidate keeps cached rows visible (zero skeleton rows) until reload finishes.
        std::fs::write(r.join("a_new.md"), "").unwrap();
        m.set_async_paused(true);
        m.invalidate(&r);
        assert!(m.has_pending_loads());
        let during = m.rows();
        assert!(!during.iter().any(|row| row.skeleton), "zero skeleton flicker during invalidate");
        assert_eq!(names(during), vec!["root", " a", " z.md"]);
        m.set_async_paused(false);
        m.wait_idle();
        assert_eq!(names(m.rows()), vec!["root", " a", " a_new.md", " z.md"]);

        // Non-blocking reveal returns None while loading, then Some(idx) once settled.
        m.set_async_paused(true);
        assert_eq!(m.reveal(&r.join("a/b/deep.md")), None);
        assert!(m.has_pending_loads());
        m.set_async_paused(false);
        m.wait_idle();
        let idx = m.reveal(&r.join("a/b/deep.md")).expect("revealed after load");
        assert_eq!(m.rows()[idx].entry.name, "deep.md");

        // Generation cancellation: stale results sent under an older load_gen are discarded by poll_loads().
        let loader = m.async_loader.as_ref().unwrap();
        let old_gen = m.load_gen;
        loader
            .tx
            .send(LoadResult {
                load_gen: old_gen,
                seq: 999,
                dir: r.clone(),
                outcome: LoadOutcome::Loaded {
                    entries: vec![TreeEntry { name: "stale.md".into(), path: r.join("stale.md"), is_dir: false, kind: FileKind::Markdown }],
                    meta: DirMeta { dir_mtime_ms: None, content_hash: 0, has_symlinks: false, saved_at_ms: None },
                    missing: false,
                    error: None,
                },
            })
            .unwrap();
        m.set_sort(TreeSort { key: SortKey::Name, desc: true });
        assert!(!m.poll_loads(), "stale load_gen must be discarded");
        m.wait_idle();
        assert!(!m.rows().iter().any(|row| row.entry.name == "stale.md"));
    }

    #[test]
    fn failed_loader_spawn_falls_back_synchronously_without_stuck_loading() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(r.join("note.md"), "").unwrap();

        let mut m = TreeModel::new(vec![r.clone()], false);
        m.enable_async(Arc::new(|| {}));
        // Simulate ensure_spawned failing: `spawned = true` and `q.closed = true`.
        {
            let loader = m.async_loader.as_mut().unwrap();
            loader.spawned = true;
            loader.shared.lock.lock().closed = true;
            assert_eq!(loader.enqueue(r.clone(), LoadPriority::High, 1, None), None);
        }

        m.set_expanded(&r, true);
        let rows = m.rows();
        assert!(!rows.iter().any(|row| row.skeleton), "sync fallback must emit real rows immediately");
        assert_eq!(names(rows), vec!["root", " note.md"]);
        assert!(!m.has_pending_loads(), "has_pending_loads must not stay stuck true");
        assert!(!m.is_loading(&r));

        // Subsequent invalidate also falls back synchronously without wedging loading state.
        std::fs::write(r.join("added.md"), "").unwrap();
        m.invalidate(&r);
        assert!(!m.has_pending_loads());
        assert_eq!(names(m.rows()), vec!["root", " added.md", " note.md"]);
    }

    #[test]
    fn disk_cache_roundtrip_and_config_invalidation() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        let sub = r.join("sub");
        let collapsed = r.join("collapsed");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&collapsed).unwrap();
        std::fs::write(r.join("a.md"), "# A").unwrap();
        std::fs::write(r.join("b.typ"), "= B").unwrap();
        std::fs::write(r.join("pic.png"), "png").unwrap();
        std::fs::write(sub.join("c.md"), "# C").unwrap();
        std::fs::write(collapsed.join("hidden.md"), "# H").unwrap();
        let cache = t.path().join("cache/tree-test.bin");

        let mut m1 = TreeModel::new(vec![r.clone()], false);
        m1.enable_async(Arc::new(|| {}));
        m1.set_filter_sig(42);
        m1.set_expanded(&r, true);
        m1.set_expanded(&sub, true);
        m1.wait_idle();
        assert_eq!(m1.dir_reads(), 2);

        let (hash1, snap) = m1.cache_snapshot().expect("initial snapshot");
        assert_eq!(snap.dirs.len(), 2, "only expanded dirs under root cached");
        m1.mark_cache_persisted(hash1);
        assert!(m1.cache_snapshot().is_none(), "no redundant snapshot once persisted");
        TreeModel::save_cache_snapshot(&cache, &snap).unwrap();

        // Seed fresh model with async worker paused: Frame 1 rows must appear immediately with 0 skeletons.
        let mut m2 = TreeModel::new(vec![r.clone()], false);
        m2.enable_async(Arc::new(|| {}));
        m2.set_filter_sig(42);
        m2.set_expanded(&r, true);
        m2.restore_expanded(&[r.clone(), sub.clone()]);
        m2.set_async_paused(true);
        assert_eq!(m2.seed_from_cache(&cache), 2);
        assert!(m2.cache_snapshot().is_none(), "seeded state matches persisted snapshot");
        assert_eq!(m2.dir_reads(), 0);
        let rows = m2.rows();
        assert!(!rows.iter().any(|row| row.skeleton), "zero skeleton rows from cache");
        assert_eq!(names(rows), vec!["root", " collapsed", " sub", "  c.md", " a.md", " b.typ", " pic.png"]);

        // Changing TreeSort, filter_sig, show_hidden, show_other, roots, or corrupting cache ignores stale cache cleanly.
        let mut m_sort = TreeModel::new(vec![r.clone()], false);
        m_sort.set_filter_sig(42);
        m_sort.set_sort(TreeSort { key: SortKey::Name, desc: true });
        m_sort.set_expanded(&r, true);
        assert_eq!(m_sort.seed_from_cache(&cache), 0);

        let mut m_sig = TreeModel::new(vec![r.clone()], false);
        m_sig.set_filter_sig(999);
        m_sig.set_expanded(&r, true);
        assert_eq!(m_sig.seed_from_cache(&cache), 0);

        let mut m_hidden = TreeModel::new(vec![r.clone()], true);
        m_hidden.set_filter_sig(42);
        m_hidden.set_expanded(&r, true);
        assert_eq!(m_hidden.seed_from_cache(&cache), 0);

        let mut m_other = TreeModel::new(vec![r.clone()], false);
        m_other.set_filter_sig(42);
        m_other.set_filter(false, ExcludeSet::default());
        m_other.set_expanded(&r, true);
        assert_eq!(m_other.seed_from_cache(&cache), 0);

        let mut m_roots = TreeModel::new(vec![t.path().join("other")], false);
        m_roots.set_filter_sig(42);
        assert_eq!(m_roots.seed_from_cache(&cache), 0);

        let mut orig = std::fs::read(&cache).unwrap();
        let last = orig.len() - 1;
        for junk in [b"".as_slice(), b"garbage", b"MDXTRE\0\x01\x02\0\0\0", &orig[..orig.len() - 2]] {
            std::fs::write(&cache, junk).unwrap();
            let mut mc = TreeModel::new(vec![r.clone()], false);
            mc.set_filter_sig(42);
            mc.set_expanded(&r, true);
            assert_eq!(mc.seed_from_cache(&cache), 0);
        }
        orig[last] ^= 0xff;
        std::fs::write(&cache, &orig).unwrap();
        let mut mc = TreeModel::new(vec![r.clone()], false);
        mc.set_filter_sig(42);
        mc.set_expanded(&r, true);
        assert_eq!(mc.seed_from_cache(&cache), 0);
    }

    #[test]
    fn startup_revalidation_reconciles_external_changes() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        let sub = r.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(r.join("keep.md"), "").unwrap();
        std::fs::write(r.join("remove_me.md"), "").unwrap();
        std::fs::write(sub.join("sub_keep.md"), "").unwrap();
        std::fs::write(sub.join("sub_gone.md"), "").unwrap();
        let cache = t.path().join("tree-reval.bin");

        {
            let mut m1 = TreeModel::new(vec![r.clone()], false);
            m1.enable_async(Arc::new(|| {}));
            m1.set_expanded(&r, true);
            m1.set_expanded(&sub, true);
            m1.wait_idle();
            let (_, snap) = m1.cache_snapshot().unwrap();
            TreeModel::save_cache_snapshot(&cache, &snap).unwrap();
        }

        // Simulate external additions/deletions while app was closed.
        std::fs::remove_file(r.join("remove_me.md")).unwrap();
        std::fs::remove_file(sub.join("sub_gone.md")).unwrap();
        std::fs::write(r.join("added.md"), "").unwrap();
        std::fs::write(sub.join("sub_added.md"), "").unwrap();

        let mut m2 = TreeModel::new(vec![r.clone()], false);
        m2.enable_async(Arc::new(|| {}));
        m2.set_expanded(&r, true);
        m2.restore_expanded(&[r.clone(), sub.clone()]);
        assert_eq!(m2.seed_from_cache(&cache), 2);
        // Before revalidation completes, stale cached rows show with 0 skeletons.
        let seeded_names = names(m2.rows());
        assert!(seeded_names.iter().any(|n| n.contains("remove_me.md")));
        assert!(!m2.rows().iter().any(|row| row.skeleton));

        m2.prefetch_startup();
        m2.wait_idle();
        let final_names = names(m2.rows());
        assert!(final_names.iter().any(|n| n.contains("added.md")));
        assert!(final_names.iter().any(|n| n.contains("sub_added.md")));
        assert!(!final_names.iter().any(|n| n.contains("remove_me.md")));
        assert!(!final_names.iter().any(|n| n.contains("sub_gone.md")));
    }

    #[cfg(unix)]
    #[test]
    fn unix_name_sort_fast_stat_skips_readdir_on_aged_unchanged_dir() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(r.join("a.md"), "a").unwrap();
        std::fs::write(r.join("b.md"), "b").unwrap();
        let cache = t.path().join("tree-unix.bin");

        let mut m1 = TreeModel::new(vec![r.clone()], false);
        m1.enable_async(Arc::new(|| {}));
        m1.set_expanded(&r, true);
        m1.wait_idle();
        let (_, mut snap) = m1.cache_snapshot().unwrap();
        let mtime = snap.dirs[0].dir_mtime_ms.expect("dir mtime");
        // Aged strictly beyond 2_000 ms margin -> fast stat skip applies.
        snap.saved_at_ms = mtime + 10_000;
        TreeModel::save_cache_snapshot(&cache, &snap).unwrap();

        let mut m2 = TreeModel::new(vec![r.clone()], false);
        m2.enable_async(Arc::new(|| {}));
        m2.set_expanded(&r, true);
        assert_eq!(m2.seed_from_cache(&cache), 1);
        m2.prefetch_startup();
        m2.wait_idle();
        assert_eq!(m2.dir_reads(), 0, "aged unchanged dir on Unix Name-sort must skip read_dir");
        assert_eq!(names(m2.rows()), vec!["root", " a.md", " b.md"]);
        assert!(m2.cache_snapshot().is_none());

        // Within 2-second margin -> must run full read_dir.
        snap.saved_at_ms = mtime + 500;
        TreeModel::save_cache_snapshot(&cache, &snap).unwrap();
        let mut m_recent = TreeModel::new(vec![r.clone()], false);
        m_recent.enable_async(Arc::new(|| {}));
        m_recent.set_expanded(&r, true);
        assert_eq!(m_recent.seed_from_cache(&cache), 1);
        m_recent.prefetch_startup();
        m_recent.wait_idle();
        assert_eq!(m_recent.dir_reads(), 1, "recent cache within 2s margin must still read_dir");

        // SortKey::Modified -> must run full read_dir even when aged.
        let mut m_mod_build = TreeModel::new(vec![r.clone()], false);
        m_mod_build.set_sort(TreeSort { key: SortKey::Modified, desc: false });
        m_mod_build.enable_async(Arc::new(|| {}));
        m_mod_build.set_expanded(&r, true);
        m_mod_build.wait_idle();
        let (_, mut mod_snap) = m_mod_build.cache_snapshot().unwrap();
        mod_snap.saved_at_ms = mod_snap.dirs[0].dir_mtime_ms.expect("mtime") + 10_000;
        TreeModel::save_cache_snapshot(&cache, &mod_snap).unwrap();

        let mut m_mod = TreeModel::new(vec![r.clone()], false);
        m_mod.set_sort(TreeSort { key: SortKey::Modified, desc: false });
        m_mod.enable_async(Arc::new(|| {}));
        m_mod.set_expanded(&r, true);
        assert_eq!(m_mod.seed_from_cache(&cache), 1);
        m_mod.prefetch_startup();
        m_mod.wait_idle();
        assert_eq!(m_mod.dir_reads(), 1, "SortKey::Modified must run read_dir");

        // Touching the directory (creating a file updates dir mtime) -> aged Name-sort cache invalidates and reads dir.
        snap.saved_at_ms = mtime + 10_000;
        TreeModel::save_cache_snapshot(&cache, &snap).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        std::fs::write(r.join("touched.md"), "new").unwrap();
        let mut m_touched = TreeModel::new(vec![r.clone()], false);
        m_touched.enable_async(Arc::new(|| {}));
        m_touched.set_expanded(&r, true);
        assert_eq!(m_touched.seed_from_cache(&cache), 1);
        m_touched.prefetch_startup();
        m_touched.wait_idle();
        assert_eq!(m_touched.dir_reads(), 1, "modified dir mtime must trigger read_dir");
        assert!(names(m_touched.rows()).iter().any(|n| n.contains("touched.md")));
    }

    #[cfg(unix)]
    #[test]
    fn young_mtime_cache_resaves_once_after_revalidation_and_then_hits_fast_stat() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("root");
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(r.join("a.md"), "a").unwrap();
        std::fs::write(r.join("b.md"), "b").unwrap();
        let cache = t.path().join("tree-young.bin");

        // Age the directory on disk 10s into the past, then save a cache snapshot whose `saved_at_ms`
        // was recorded only 500ms after the directory's mtime (failing the 2_000ms fast-stat margin).
        let old_dir_time = SystemTime::now() - Duration::from_secs(10);
        std::fs::File::open(&r).unwrap().set_modified(old_dir_time).unwrap();

        let mut m1 = TreeModel::new(vec![r.clone()], false);
        m1.enable_async(Arc::new(|| {}));
        m1.set_expanded(&r, true);
        m1.wait_idle();
        let (_, mut snap1) = m1.cache_snapshot().unwrap();
        let dir_mtime = snap1.dirs[0].dir_mtime_ms.expect("dir mtime");
        snap1.saved_at_ms = dir_mtime + 500;
        TreeModel::save_cache_snapshot(&cache, &snap1).unwrap();

        // Startup 2: seeded with young-mtime cache -> must revalidate via read_dir (dir_reads() == 1),
        // wait until revalidation completes before exposing a resave snapshot, then trigger one resave
        // with refreshed `saved_at_ms`, and return None thereafter.
        let mut m2 = TreeModel::new(vec![r.clone()], false);
        m2.enable_async(Arc::new(|| {}));
        m2.set_expanded(&r, true);
        assert_eq!(m2.seed_from_cache(&cache), 1);
        assert!(m2.cache_snapshot().is_none(), "must not resave before revalidation runs");
        m2.set_async_paused(true);
        m2.prefetch_startup();
        assert!(m2.has_pending_loads());
        assert!(m2.cache_snapshot().is_none(), "must not resave while revalidation is pending");
        m2.set_async_paused(false);
        m2.wait_idle();
        assert_eq!(m2.dir_reads(), 1, "young-mtime cache must run full read_dir once");

        let (hash2, snap2) = m2.cache_snapshot().expect("young-mtime cache must request a single resave after revalidation");
        assert!(snap2.saved_at_ms > dir_mtime + REVALIDATE_MTIME_MARGIN_MS);
        TreeModel::save_cache_snapshot(&cache, &snap2).unwrap();
        m2.mark_cache_persisted(hash2);
        assert!(m2.cache_snapshot().is_none(), "no second resave after mark_cache_persisted");

        // Startup 3: refreshed `saved_at_ms` now satisfies the 2s margin -> fast-stat skips read_dir!
        let mut m3 = TreeModel::new(vec![r.clone()], false);
        m3.enable_async(Arc::new(|| {}));
        m3.set_expanded(&r, true);
        assert_eq!(m3.seed_from_cache(&cache), 1);
        m3.prefetch_startup();
        m3.wait_idle();
        assert_eq!(m3.dir_reads(), 0, "following startup must hit fast-stat with zero dir reads");
        assert!(m3.cache_snapshot().is_none());
    }
}
