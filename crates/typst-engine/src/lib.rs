//! Typst integration of iron sheets.
//!
//! * live preview of `.typ` documents ([`Engine::set_document`], [`Engine::page`]),
//! * Typst math snippets for Markdown previews ([`Engine::math`]),
//! * static helpers: [`symbols`] and [`highlight`].
//!
//! Nothing here runs on the caller's thread except cache lookups and
//! scheduling. The Typst library and the fonts are initialised lazily on a
//! worker thread at the first request; [`Engine::new`] does no work at all.

mod doc;
mod highlight;
mod latex;
mod lru;
mod math;
mod shared;
mod symbols;
mod world;

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Once, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::doc::{Good, Input};
use crate::lru::Lru;
use crate::math::MathJob;
use crate::shared::Shared;
use crate::world::DocFiles;

pub use crate::highlight::{IncrementalTypstHighlighter, highlight};
pub use crate::latex::latex_to_typst;
pub use crate::symbols::symbols;

/// Implementation details exposed for the benchmark example only.
#[doc(hidden)]
pub mod internals {
    pub use crate::math::{MathJob, render_batch, render_one};
    pub use crate::shared::Shared;
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct EngineConfig {
    /// Scan the operating system's fonts (in the background, after the
    /// embedded fonts are usable).
    pub system_fonts: bool,
    /// Called from worker threads whenever a result is ready.
    pub notify: Arc<dyn Fn() + Send + Sync>,
}

/// A math snippet. `source` is the body of a Typst equation, without `$`.
#[derive(Clone, Debug)]
pub struct MathRequest<'a> {
    pub source: &'a str,
    pub display: bool,
    pub font_size_pt: f32,
    /// RGBA text colour.
    pub color: [u8; 4],
    pub pixels_per_pt: f32,
    /// `source` is LaTeX math; it is converted to Typst on the worker thread (and cached under a
    /// key that includes this flag).
    pub latex: bool,
}

#[derive(Clone, Debug)]
pub enum MathState {
    Pending,
    Ready(Arc<MathImage>),
    Error(Arc<str>),
}

#[derive(Clone)]
pub struct MathImage {
    pub key: u64,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, transparent background.
    pub rgba: Vec<u8>,
    pub size_pt: [f32; 2],
    /// Distance from the image top to the text baseline, in points.
    pub baseline_pt: f32,
}

impl fmt::Debug for MathImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MathImage")
            .field("key", &self.key)
            .field("size_px", &(self.width, self.height))
            .field("size_pt", &self.size_pt)
            .field("baseline_pt", &self.baseline_pt)
            .finish_non_exhaustive()
    }
}

pub type DocId = u64;

#[derive(Clone, Debug)]
pub struct DocSnapshot {
    pub version: u64,
    /// `false`: the compile failed; `pages` and `anchors` are from the last
    /// good compile.
    pub ok: bool,
    /// Page sizes in pt.
    pub pages: Vec<[f32; 2]>,
    pub diagnostics: Vec<Diagnostic>,
    pub anchors: Vec<Anchor>,
    pub compile_ms: f32,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub error: bool,
    pub message: String,
    pub hints: Vec<String>,
    /// `None` = the main buffer.
    pub file: Option<PathBuf>,
    /// 0-based.
    pub line: u32,
    /// 0-based, in characters.
    pub column: u32,
}

/// Position of a source line in the laid out document. Sorted by `line`,
/// non-decreasing in `(page, y_pt)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub line: u32,
    pub page: u32,
    pub y_pt: f32,
}

#[derive(Clone, Debug)]
pub enum PageState {
    Pending,
    Ready(Arc<PageImage>),
}

#[derive(Clone)]
pub struct PageImage {
    /// The document version this image was rendered from.
    pub version: u64,
    pub page: u32,
    pub pixels_per_pt: f32,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, opaque page background.
    pub rgba: Vec<u8>,
}

impl fmt::Debug for PageImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageImage")
            .field("version", &self.version)
            .field("page", &self.page)
            .field("size_px", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct SymbolInfo {
    /// Full dotted name, e.g. `arrow.r.long`.
    pub name: String,
    pub value: String,
    /// Set when `value` is a single code point.
    pub ch: Option<char>,
    pub deprecated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypTok {
    Text,
    Comment,
    Punct,
    Escape,
    Strong,
    Emph,
    Link,
    Raw,
    Label,
    Ref,
    Heading,
    ListMarker,
    MathDelim,
    MathOp,
    Keyword,
    Operator,
    Number,
    String,
    Function,
    Interpolated,
    Error,
}

pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

const MATH_CACHE_BYTES: usize = 64 * 1024 * 1024;
const PAGE_CACHE_BYTES: usize = 256 * 1024 * 1024;
const MATH_QUEUE_CAP: usize = 1024;
const PAGE_QUEUE_CAP: usize = 64;
/// Math strategy, chosen by measurement (see `examples/bench.rs`): one compile
/// per formula on a small thread pool. Batching several formulas into one
/// document is ~1.5x faster on a single thread but slower than the pool, and
/// needs extra work to keep errors isolated.
const MATH_BATCH: usize = 1;
const RENDER_THREADS: usize = 2;
/// How long the first compile waits for the system-font scan before using the embedded fonts only.
// Measured: the scan takes ~220 ms on the dev machine (985 fonts).
const FONT_WAIT: Duration = Duration::from_millis(300);

type PageKey = (DocId, u32, u32);

struct CachedPage {
    image: Arc<PageImage>,
    generation: u64,
}

struct DocEntry {
    instance: u64,
    pending: Option<(u64, Arc<Input>)>,
    last_input: Option<Arc<Input>>,
    compiling: bool,
    snapshot: Option<Arc<DocSnapshot>>,
    good: Option<Arc<Good>>,
    files: Arc<DocFiles>,
    main_source: Option<typst::syntax::Source>,
    epoch: u64,
    /// Font generation the last compile started with.
    fonts_gen: u64,
}

enum Action {
    Jump { id: DocId, page: u32, x: f32, y: f32 },
    Export { id: DocId, out: PathBuf },
}

struct Docs {
    map: FxHashMap<DocId, DocEntry>,
    seq: u64,
    instances: u64,
    generation: u64,
    pages: Lru<PageKey, CachedPage>,
    page_pending: FxHashSet<PageKey>,
    page_queue: VecDeque<PageKey>,
    page_failed: FxHashSet<(PageKey, u64)>,
    actions: VecDeque<Action>,
    jumps: FxHashMap<DocId, (u32, u32)>,
    exports: FxHashMap<DocId, Result<PathBuf, String>>,
}

enum MathEntry {
    Ready(Arc<MathImage>),
    Error(Arc<str>),
}

struct MathQ {
    cache: Lru<u64, MathEntry>,
    pending: FxHashSet<u64>,
    queue: VecDeque<(u64, MathJob)>,
}

struct Inner {
    cfg: EngineConfig,
    shared: OnceLock<Arc<Shared>>,
    shared_init: StdMutex<()>,
    fonts_once: Once,
    /// Set when the system-font scan is over (or was never wanted).
    fonts_done: Mutex<bool>,
    cv_fonts: Condvar,
    warm_once: Once,
    /// True while the `warm` thread builds the library (cleared even if that panics).
    warming_lib: AtomicBool,
    docs_started: Once,
    render_started: Once,
    math_started: Once,
    shutdown: AtomicBool,
    docs: Mutex<Docs>,
    cv_compile: Condvar,
    cv_render: Condvar,
    math: Mutex<MathQ>,
    cv_math: Condvar,
    evict_counter: AtomicUsize,
}

impl Inner {
    fn notify(&self) {
        (self.cfg.notify)();
    }

    /// The shared Typst state, built on first use by the calling worker.
    fn shared(self: &Arc<Self>) -> Arc<Shared> {
        match self.shared.get() {
            Some(shared) => shared.clone(),
            None => {
                // Serialise initialisation so that concurrent workers wait
                // instead of building the library twice.
                let _guard = self.shared_init.lock().unwrap_or_else(|e| e.into_inner());
                self.shared.get_or_init(|| Arc::new(Shared::new())).clone()
            }
        }
    }

    /// Starts the system-font scan once, in parallel with building the library. When it is done
    /// the documents compiled with fewer fonts are recompiled.
    fn start_fonts(self: &Arc<Self>) {
        if !self.cfg.system_fonts {
            return;
        }
        let inner = self.clone();
        self.fonts_once.call_once(move || {
            let spawned = std::thread::Builder::new().name("typst-fonts".into()).spawn(move || {
                let system = catch_unwind(AssertUnwindSafe(Shared::scan_system_fonts)).unwrap_or_default();
                // Whatever happens here, the scan is over: compiles must not wait for it and the cue must go.
                let _ = catch_unwind(AssertUnwindSafe(|| inner.shared().install_system_fonts(system)));
                *inner.fonts_done.lock() = true;
                inner.cv_fonts.notify_all();
                inner.requeue_all_documents();
                inner.notify();
            });
            if spawned.is_err() {
                *self.fonts_done.lock() = true;
            }
        });
    }

    /// Lets a first compile wait briefly for a running font scan, so it does not render with the
    /// embedded fonts only and then again with the system fonts.
    fn wait_fonts(&self, max: Duration) {
        let deadline = Instant::now() + max;
        let mut done = self.fonts_done.lock();
        while !*done {
            if self.cv_fonts.wait_until(&mut done, deadline).timed_out() {
                break;
            }
        }
    }

    /// Recompiles every open document that was compiled with an older font set.
    fn requeue_all_documents(&self) {
        let generation = self.shared.get().map_or(0, |s| s.fonts().generation);
        {
            let mut guard = self.docs.lock();
            let docs = &mut *guard;
            for entry in docs.map.values_mut() {
                if entry.pending.is_none()
                    && entry.fonts_gen < generation
                    && let Some(input) = entry.last_input.clone()
                {
                    docs.seq += 1;
                    entry.pending = Some((docs.seq, input));
                }
            }
        }
        self.cv_compile.notify_all();
    }

    fn evict_cache(&self, every: usize, max_age: usize) {
        if self.evict_counter.fetch_add(1, Ordering::Relaxed) % every == every - 1 {
            comemo::evict(max_age);
        }
    }
}

/// The Typst engine. `Send + Sync`; share it behind an `Arc`.
pub struct Engine {
    inner: Arc<Inner>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        // Take each lock once so that no worker misses the wake-up between
        // checking the flag and waiting.
        drop(self.inner.docs.lock());
        drop(self.inner.math.lock());
        self.inner.cv_compile.notify_all();
        self.inner.cv_render.notify_all();
        self.inner.cv_math.notify_all();
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(f);
}

impl Engine {
    /// Returns immediately; no thread is started and nothing is loaded.
    pub fn new(config: EngineConfig) -> Engine {
        let config_fonts = config.system_fonts;
        Engine {
            inner: Arc::new(Inner {
                cfg: config,
                shared: OnceLock::new(),
                shared_init: StdMutex::new(()),
                fonts_once: Once::new(),
                fonts_done: Mutex::new(!config_fonts),
                cv_fonts: Condvar::new(),
                warm_once: Once::new(),
                warming_lib: AtomicBool::new(false),
                docs_started: Once::new(),
                render_started: Once::new(),
                math_started: Once::new(),
                shutdown: AtomicBool::new(false),
                docs: Mutex::new(Docs {
                    map: FxHashMap::default(),
                    seq: 0,
                    instances: 0,
                    generation: 0,
                    pages: Lru::new(PAGE_CACHE_BYTES),
                    page_pending: FxHashSet::default(),
                    page_queue: VecDeque::new(),
                    page_failed: FxHashSet::default(),
                    actions: VecDeque::new(),
                    jumps: FxHashMap::default(),
                    exports: FxHashMap::default(),
                }),
                cv_compile: Condvar::new(),
                cv_render: Condvar::new(),
                math: Mutex::new(MathQ { cache: Lru::new(MATH_CACHE_BYTES), pending: FxHashSet::default(), queue: VecDeque::new() }),
                cv_math: Condvar::new(),
                evict_counter: AtomicUsize::new(0),
            }),
        }
    }

    /// Starts building the library and scanning the system fonts in the background, so the first
    /// compile or math request finds them ready. Idempotent; call it as soon as Typst will be needed.
    pub fn warm(&self) {
        self.inner.start_fonts();
        let inner = self.inner.clone();
        self.inner.warm_once.call_once(|| {
            inner.warming_lib.store(true, Ordering::Release);
            spawn("typst-warm", move || {
                let _ = catch_unwind(AssertUnwindSafe(|| drop(inner.shared())));
                inner.warming_lib.store(false, Ordering::Release);
                inner.notify();
            })
        });
    }

    /// What the background warm-up (see `warm`) is still loading, for a status cue; `None` when idle or never started.
    /// Cheap: one flag and one short lock.
    pub fn warming(&self) -> Option<&'static str> {
        let i = &self.inner;
        if i.warming_lib.load(Ordering::Acquire) && i.shared.get().is_none() {
            Some("Loading Typst library…")
        } else if i.fonts_once.is_completed() && !*i.fonts_done.lock() {
            Some("Loading Typst fonts…")
        } else {
            None
        }
    }

    /// Documents with a compile queued or running (for the debug overlay; takes the docs lock briefly).
    pub fn compiles_pending(&self) -> usize {
        self.inner.docs.lock().map.values().filter(|e| e.pending.is_some() || e.compiling).count()
    }

    fn start_docs(&self) {
        self.warm();
        self.inner.docs_started.call_once(|| {
            let inner = self.inner.clone();
            spawn("typst-compile", move || compile_worker(inner));
        });
    }

    fn start_render(&self) {
        self.inner.render_started.call_once(|| {
            for i in 0..RENDER_THREADS {
                let inner = self.inner.clone();
                spawn(&format!("typst-render-{i}"), move || render_worker(inner));
            }
        });
    }

    fn start_math(&self) {
        self.warm();
        self.inner.math_started.call_once(|| {
            let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
            let threads = cpus.saturating_sub(1).clamp(2, 4);
            for i in 0..threads {
                let inner = self.inner.clone();
                spawn(&format!("typst-math-{i}"), move || math_worker(inner));
            }
        });
    }

    // ----- math -------------------------------------------------------

    /// Cache lookup; on a miss the snippet is scheduled and `Pending` is
    /// returned. Call again after the `notify` callback fired.
    pub fn math(&self, req: &MathRequest) -> MathState {
        let key = math_key(req);
        let mut q = self.inner.math.lock();
        match q.cache.get(&key) {
            Some(MathEntry::Ready(img)) => return MathState::Ready(img.clone()),
            Some(MathEntry::Error(msg)) => return MathState::Error(msg.clone()),
            None => {}
        }
        if q.pending.contains(&key) {
            // Asked for again while it waits: it is still on screen, so it moves to the newest end, which the worker
            // takes first. Formulas only requested once (laid out ahead of the viewport) stay behind.
            bump(&mut q.queue, key);
            return MathState::Pending;
        }
        q.pending.insert(key);
        q.queue.push_back((
            key,
            MathJob {
                source: req.source.to_string(),
                display: req.display,
                font_size_pt: req.font_size_pt,
                color: req.color,
                pixels_per_pt: req.pixels_per_pt,
                latex: req.latex,
            },
        ));
        // Latest wins: when the backlog grows, the oldest requests (the ones
        // scrolled away from long ago) are dropped; they are re-requested
        // if they are still visible.
        while q.queue.len() > MATH_QUEUE_CAP {
            if let Some((old, _)) = q.queue.pop_front() {
                q.pending.remove(&old);
            }
        }
        drop(q);
        self.start_math();
        self.inner.cv_math.notify_one();
        MathState::Pending
    }

    // ----- documents --------------------------------------------------

    /// Schedules a compile of the (possibly unsaved) editor buffer. Only the
    /// newest queued version of a document is ever compiled.
    pub fn set_document(&self, id: DocId, path: Option<PathBuf>, root: Option<PathBuf>, text: String, version: u64) {
        let input = Arc::new(Input { path, root, text, version });
        {
            let mut docs = self.inner.docs.lock();
            let docs = &mut *docs;
            docs.seq += 1;
            let seq = docs.seq;
            docs.instances += 1;
            let instance = docs.instances;
            let entry = docs.map.entry(id).or_insert_with(|| DocEntry {
                instance,
                pending: None,
                last_input: None,
                compiling: false,
                snapshot: None,
                good: None,
                files: Arc::new(DocFiles::default()),
                main_source: None,
                epoch: 0,
                fonts_gen: 0,
            });
            entry.last_input = Some(input.clone());
            entry.pending = Some((seq, input));
        }
        self.start_docs();
        self.inner.cv_compile.notify_one();
    }

    pub fn close_document(&self, id: DocId) {
        let mut docs = self.inner.docs.lock();
        docs.map.remove(&id);
        docs.pages.retain(|k| k.0 != id);
        docs.page_queue.retain(|k| k.0 != id);
        docs.page_pending.retain(|k| k.0 != id);
        docs.page_failed.retain(|(k, _)| k.0 != id);
        docs.actions.retain(|a| match a {
            Action::Jump { id: i, .. } | Action::Export { id: i, .. } => *i != id,
        });
        docs.jumps.remove(&id);
        docs.exports.remove(&id);
    }

    /// The latest finished compile.
    pub fn document(&self, id: DocId) -> Option<Arc<DocSnapshot>> {
        self.inner.docs.lock().map.get(&id)?.snapshot.clone()
    }

    /// A rendered page. While a newer version renders, the previous image of
    /// that page (if any) keeps being returned.
    pub fn page(&self, id: DocId, page: u32, pixels_per_pt: f32) -> PageState {
        let mut ppp = if pixels_per_pt.is_finite() { pixels_per_pt.clamp(0.1, 8.0) } else { 1.0 };
        let mut docs = self.inner.docs.lock();
        let docs = &mut *docs;
        let Some(good) = docs.map.get(&id).and_then(|e| e.good.clone()) else {
            return PageState::Pending;
        };
        let Some(size) = good.pages.get(page as usize) else {
            return PageState::Pending;
        };
        // Bound the bitmap size (about 48 Mpixel, ~190 MB); the returned
        // image reports the scale actually used in `pixels_per_pt`.
        let area = f64::from(size[0].max(1.0)) * f64::from(size[1].max(1.0));
        let max_ppp = (48.0e6 / area).sqrt() as f32;
        if ppp > max_ppp {
            ppp = max_ppp.max(0.01);
        }
        let key: PageKey = (id, page, ppp.to_bits());
        let mut stale = None;
        if let Some(cached) = docs.pages.get(&key) {
            if cached.generation == good.generation {
                return PageState::Ready(cached.image.clone());
            }
            stale = Some(cached.image.clone());
        }
        if !docs.page_failed.contains(&(key, good.generation)) && docs.page_pending.insert(key) {
            docs.page_queue.push_back(key);
            while docs.page_queue.len() > PAGE_QUEUE_CAP {
                if let Some(old) = docs.page_queue.pop_front() {
                    docs.page_pending.remove(&old);
                }
            }
            self.start_render();
            self.inner.cv_render.notify_one();
        }
        match stale {
            Some(image) => PageState::Ready(image),
            None => PageState::Pending,
        }
    }

    /// Asynchronous click-to-source. The result arrives via [`take_jump`].
    ///
    /// [`take_jump`]: Engine::take_jump
    pub fn jump_from_click(&self, id: DocId, page: u32, x_pt: f32, y_pt: f32) {
        self.inner.docs.lock().actions.push_back(Action::Jump { id, page, x: x_pt, y: y_pt });
        self.start_render();
        self.inner.cv_render.notify_one();
    }

    pub fn take_jump(&self, id: DocId) -> Option<(u32, u32)> {
        self.inner.docs.lock().jumps.remove(&id)
    }

    /// Exports the last good compile as PDF. Result via [`take_export_result`].
    ///
    /// [`take_export_result`]: Engine::take_export_result
    pub fn export_pdf(&self, id: DocId, out: PathBuf) {
        self.inner.docs.lock().actions.push_back(Action::Export { id, out });
        self.start_render();
        self.inner.cv_render.notify_one();
    }

    pub fn take_export_result(&self, id: DocId) -> Option<Result<PathBuf, String>> {
        self.inner.docs.lock().exports.remove(&id)
    }
}

/// Moves the queued job `key` (if it is still queued) to the back.
fn bump(queue: &mut VecDeque<(u64, MathJob)>, key: u64) {
    if let Some(i) = queue.iter().rposition(|(k, _)| *k == key)
        && i + 1 < queue.len()
        && let Some(job) = queue.remove(i)
    {
        queue.push_back(job);
    }
}

/// Cache key of a request; hashes the same fields as `MathJob` (`String` and `&str` hash alike).
fn math_key(req: &MathRequest) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    req.source.hash(&mut h);
    req.display.hash(&mut h);
    req.latex.hash(&mut h);
    req.font_size_pt.to_bits().hash(&mut h);
    req.color.hash(&mut h);
    req.pixels_per_pt.to_bits().hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// Workers
// ---------------------------------------------------------------------------

fn math_worker(inner: Arc<Inner>) {
    loop {
        let batch: Vec<(u64, MathJob)> = {
            let mut q = inner.math.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if !q.queue.is_empty() {
                    break;
                }
                inner.cv_math.wait(&mut q);
            }
            // Newest first.
            let mut batch = Vec::new();
            while batch.len() < MATH_BATCH
                && let Some(item) = q.queue.pop_back()
            {
                batch.push(item);
            }
            batch
        };
        let shared = inner.shared();
        let results: Vec<Result<MathImage, String>> = if batch.len() == 1 {
            vec![math::render_one(&shared, &batch[0].1, batch[0].0)]
        } else {
            let jobs: Vec<(MathJob, u64)> = batch.iter().map(|(k, j)| (j.clone(), *k)).collect();
            math::render_batch(&shared, &jobs)
        };
        {
            let mut q = inner.math.lock();
            for ((key, _), result) in batch.iter().zip(results) {
                q.pending.remove(key);
                match result {
                    Ok(img) => {
                        let size = img.rgba.len() + 128;
                        q.cache.insert(*key, MathEntry::Ready(Arc::new(img)), size);
                    }
                    Err(msg) => {
                        let size = msg.len() + 128;
                        q.cache.insert(*key, MathEntry::Error(msg.into()), size);
                    }
                }
            }
        }
        inner.evict_cache(64 / MATH_BATCH.max(1), 20);
        inner.notify();
    }
}

fn compile_worker(inner: Arc<Inner>) {
    // Started by the first document: its first compile waits (once) for a running font scan.
    inner.wait_fonts(FONT_WAIT);
    loop {
        let (id, instance, input, files, mut main_source, epoch) = {
            let mut docs = inner.docs.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                let next = docs
                    .map
                    .iter()
                    .filter(|(_, e)| e.pending.is_some() && !e.compiling)
                    .min_by_key(|(_, e)| e.pending.as_ref().map(|p| p.0))
                    .map(|(id, _)| *id);
                if let Some(id) = next
                    && let Some(entry) = docs.map.get_mut(&id)
                    && let Some((_, input)) = entry.pending.take()
                {
                    entry.compiling = true;
                    entry.epoch += 1;
                    // Taken under the docs lock, like the requeue check, so a scan finishing meanwhile requeues this document.
                    entry.fonts_gen = inner.shared.get().map_or(0, |s| s.fonts().generation);
                    break (id, entry.instance, input, entry.files.clone(), entry.main_source.take(), entry.epoch);
                }
                inner.cv_compile.wait(&mut docs);
            }
        };

        let shared = inner.shared();
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| doc::compile(&shared, &input, files, &mut main_source, epoch)));
        let compile_ms = started.elapsed().as_secs_f32() * 1000.0;
        comemo::evict(10);

        let mut docs = inner.docs.lock();
        let docs = &mut *docs;
        let new_generation = docs.generation + 1;
        let Some(entry) = docs.map.get_mut(&id).filter(|e| e.instance == instance) else {
            continue;
        };
        entry.compiling = false;
        entry.main_source = main_source;
        match result {
            Ok(outcome) => {
                let (pages, anchors) = match outcome.good {
                    Some((doc, world, pages, anchors)) => {
                        docs.generation = new_generation;
                        entry.good = Some(Arc::new(Good {
                            version: input.version,
                            generation: new_generation,
                            doc,
                            world,
                            pages: pages.clone(),
                            anchors: anchors.clone(),
                        }));
                        (pages, anchors)
                    }
                    None => match &entry.good {
                        Some(good) => (good.pages.clone(), good.anchors.clone()),
                        None => (Vec::new(), Vec::new()),
                    },
                };
                entry.snapshot =
                    Some(Arc::new(DocSnapshot { version: input.version, ok: outcome.ok, pages, diagnostics: outcome.diagnostics, anchors, compile_ms }));
            }
            Err(payload) => {
                let (pages, anchors) = match &entry.good {
                    Some(good) => (good.pages.clone(), good.anchors.clone()),
                    None => (Vec::new(), Vec::new()),
                };
                entry.snapshot = Some(Arc::new(DocSnapshot {
                    version: input.version,
                    ok: false,
                    pages,
                    diagnostics: vec![Diagnostic {
                        error: true,
                        message: format!("internal compiler error: {}", panic_message(&*payload)),
                        hints: Vec::new(),
                        file: None,
                        line: 0,
                        column: 0,
                    }],
                    anchors,
                    compile_ms,
                }));
            }
        }
        inner.notify();
    }
}

enum Task {
    Action(Action),
    Page(PageKey),
}

fn render_worker(inner: Arc<Inner>) {
    loop {
        let task = {
            let mut docs = inner.docs.lock();
            loop {
                if inner.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(action) = docs.actions.pop_front() {
                    break Task::Action(action);
                }
                // Newest page request first.
                if let Some(key) = docs.page_queue.pop_back() {
                    break Task::Page(key);
                }
                inner.cv_render.wait(&mut docs);
            }
        };
        match task {
            Task::Page(key) => render_page_task(&inner, key),
            Task::Action(Action::Jump { id, page, x, y }) => {
                let good = inner.docs.lock().map.get(&id).and_then(|e| e.good.clone());
                let found = good.and_then(|good| catch_unwind(AssertUnwindSafe(|| jump(&good, page, x, y))).ok().flatten());
                if let Some(pos) = found {
                    inner.docs.lock().jumps.insert(id, pos);
                    inner.notify();
                }
            }
            Task::Action(Action::Export { id, out }) => {
                let good = inner.docs.lock().map.get(&id).and_then(|e| e.good.clone());
                let result = match good {
                    None => Err("nothing to export: the document has not compiled yet".to_string()),
                    Some(good) => {
                        catch_unwind(AssertUnwindSafe(|| export(&good, &out))).unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&*p))))
                    }
                };
                inner.docs.lock().exports.insert(id, result.map(|()| out));
                inner.notify();
            }
        }
    }
}

fn render_page_task(inner: &Arc<Inner>, key: PageKey) {
    let (id, page, ppp_bits) = key;
    let good = {
        let mut docs = inner.docs.lock();
        let good = docs.map.get(&id).and_then(|e| e.good.clone());
        if good.as_ref().is_none_or(|g| page as usize >= g.pages.len()) {
            docs.page_pending.remove(&key);
            return;
        }
        good
    };
    let Some(good) = good else { return };
    let ppp = f32::from_bits(ppp_bits);
    let rendered = catch_unwind(AssertUnwindSafe(|| {
        let opts = typst_render::RenderOptions { pixel_per_pt: typst::utils::Scalar::new(f64::from(ppp)), render_bleed: false };
        let pixmap = typst_render::render(&good.doc.pages()[page as usize], &opts);
        PageImage { version: good.version, page, pixels_per_pt: ppp, width: pixmap.width(), height: pixmap.height(), rgba: pixmap.take() }
    }));
    {
        let mut docs = inner.docs.lock();
        docs.page_pending.remove(&key);
        match rendered {
            Ok(image) => {
                let size = image.rgba.len() + 128;
                docs.pages.insert(key, CachedPage { image: Arc::new(image), generation: good.generation }, size);
            }
            Err(_) => {
                docs.page_failed.insert((key, good.generation));
            }
        }
    }
    inner.notify();
}

fn jump(good: &Good, page: u32, x: f32, y: f32) -> Option<(u32, u32)> {
    use typst::World;
    use typst::introspection::PagedPosition;
    use typst::layout::{Abs, Point};
    let position = PagedPosition { page: std::num::NonZeroUsize::new(page as usize + 1)?, point: Point::new(Abs::pt(f64::from(x)), Abs::pt(f64::from(y))) };
    match typst_ide::jump_from_click(&*good.world, &*good.doc, &position)? {
        typst_ide::Jump::File(id, offset) if id == good.world.main() => {
            let (line, column) = good.world.main_source().lines().byte_to_line_column(offset)?;
            Some((line as u32, column as u32))
        }
        _ => None,
    }
}

#[cfg(feature = "pdf")]
fn export(good: &Good, out: &std::path::Path) -> Result<(), String> {
    let options = typst_pdf::PdfOptions::default();
    let bytes = typst_pdf::pdf(&good.doc, &options)
        .map_err(|errors| errors.first().map(|e| e.message.to_string()).unwrap_or_else(|| "PDF export failed".to_string()))?;
    std::fs::write(out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))
}

#[cfg(not(feature = "pdf"))]
fn export(_: &Good, _: &std::path::Path) -> Result<(), String> {
    Err("PDF export is not compiled in (enable the `pdf` feature)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> MathJob {
        MathJob { source: String::new(), display: false, font_size_pt: 10.0, color: [0; 4], pixels_per_pt: 1.0, latex: false }
    }

    #[test]
    fn asking_again_moves_a_queued_formula_to_the_worker_end() {
        let mut q: VecDeque<(u64, MathJob)> = (1..=4).map(|k| (k, job())).collect();
        bump(&mut q, 1);
        // The worker pops from the back: 1 (on screen, asked again) before 4 (asked once, ahead of the viewport).
        assert_eq!(q.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [2, 3, 4, 1]);
        bump(&mut q, 1);
        bump(&mut q, 99);
        assert_eq!(q.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [2, 3, 4, 1]);
    }
}
