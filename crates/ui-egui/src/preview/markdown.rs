//! Markdown preview: virtualized, cached per block, filling `tab.preview_scroll`.
#[cfg(feature = "codehl")]
#[path = "md/codehl.rs"]
pub mod codehl;
/// Without `codehl`: the same names backed by the null service, so every call site stays as it is.
#[cfg(not(feature = "codehl"))]
pub mod codehl {
    use iron_sheets_app_core::platform::Waker;
    use iron_sheets_app_core::services::{CodeHighlighter, CodeHl};
    use std::sync::Arc;

    pub struct SyntectHighlighter;
    impl SyntectHighlighter {
        pub fn shared(_: &Arc<dyn Waker>) -> Arc<SyntectHighlighter> {
            Arc::new(SyntectHighlighter)
        }
        pub fn epoch(&self) -> u64 {
            0
        }
        pub fn is_loading(&self) -> bool {
            false
        }
    }
    impl CodeHighlighter for SyntectHighlighter {
        fn get(&self, _: &str, _: &str) -> CodeHl {
            CodeHl::Plain
        }
    }
}
#[path = "md/diagram.rs"]
mod diagram;
#[path = "md/embed.rs"]
mod embed;
#[path = "md/images.rs"]
pub(crate) mod images;
#[path = "md/layout.rs"]
mod layout;
#[path = "md/mermaid.rs"]
pub(crate) mod mermaid;
#[cfg(feature = "mermaid")]
#[path = "md/mermaid_render.rs"]
mod mermaid_render;
#[cfg(any(feature = "mermaid", feature = "excalidraw"))]
#[path = "md/svgfonts.rs"]
pub(crate) mod svgfonts;
#[path = "md/select.rs"]
mod select;

use super::popup;
use crate::command::Command;
use crate::icons::Icon;
use crate::scrollsync::{PaneScroll, ScrollMap};
use crate::scrollsync_egui::scroll_pane;
use crate::state::AppState;
use crate::theme::PreviewStyle;
use egui::epaint::text::Galley;
use egui::{Align2, Color32, Context, CursorIcon, Id, Pos2, Rect, Sense, Stroke, TextureId, UiBuilder, pos2, vec2};
use embed::EmbedCache;
use images::ImageCache;
pub(super) use layout::percent_decode;
use layout::*;
use iron_sheets_md::model::*;
use rustc_hash::FxHashMap;
use select::{Pos as SelPos, Sel};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_W: f32 = 900.0;
const MARGIN: f32 = 20.0;
const TOP_PAD: f32 = 14.0;
const BOTTOM_PAD: f32 = 60.0;
const BIG_TEXT: usize = 200 * 1024;
/// Notes up to this size are parsed on the UI thread (about 0.7 ms at 24 KB).
const SYNC_PARSE_MAX: usize = 24 * 1024;
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Idle-laid blocks this close behind the visible range rebuild `tops` at once.
const NEAR: usize = 64;
const IDLE_BUDGET: Duration = Duration::from_millis(2);
const MAX_DOCS: usize = 8;
/// Share of the blank lines next to a tall block that the block borrows in the scroll map.
const BORROW: f32 = 0.35;
/// Background parses longer than this show a non-shifting "Updating preview…" cue.
const STALE_CUE_AFTER: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Geom {
    width: u32,
    theme: u64,
    epoch: u32,
    dir: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    hash: u64,
    g: Geom,
}

struct ParseJob {
    version: u64,
    text: String,
    submitted: Instant,
    ctx: Context,
}

struct ParseDone {
    path: PathBuf,
    version: u64,
    doc: Document,
    took: Duration,
    /// Time from `submit` to the end of the parse.
    latency: Duration,
}

#[derive(Default)]
struct Inbox {
    /// Newest queued text per document: submitting again replaces an older job that was not started yet.
    jobs: FxHashMap<PathBuf, ParseJob>,
    /// Replaced parses, freed on the worker (dropping a 100 KB note's blocks takes ~0.25 ms).
    garbage: Vec<Arc<Document>>,
}

struct WorkerShared {
    queue: parking_lot::Mutex<Inbox>,
    wake: parking_lot::Condvar,
    /// Text buffers of finished jobs, reused by `submit` (a fresh 100 KB allocation costs more than the copy).
    spare: parking_lot::Mutex<Vec<String>>,
    closed: std::sync::atomic::AtomicBool,
    active: std::sync::atomic::AtomicBool,
    paused: std::sync::atomic::AtomicBool,
}

/// One long-lived parse thread, started on the first parse of a big note. A keystroke only copies the text
/// into the queue; if several versions are queued only the newest of each document is parsed.
struct ParseWorker {
    shared: Arc<WorkerShared>,
    done: crossbeam_channel::Receiver<ParseDone>,
}

impl ParseWorker {
    fn start() -> Option<ParseWorker> {
        let shared = Arc::new(WorkerShared {
            queue: Default::default(),
            wake: parking_lot::Condvar::new(),
            spare: Default::default(),
            closed: Default::default(),
            active: Default::default(),
            paused: Default::default(),
        });
        let (tx, done) = crossbeam_channel::unbounded();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("iron-sheets-md-parse".into())
            .spawn(move || {
                loop {
                    let (path, job, garbage) = {
                        let mut q = sh.queue.lock();
                        loop {
                            if sh.closed.load(std::sync::atomic::Ordering::Acquire) {
                                return;
                            }
                            let garbage = std::mem::take(&mut q.garbage);
                            if !garbage.is_empty() {
                                break (PathBuf::new(), None, garbage);
                            }
                            if !sh.paused.load(std::sync::atomic::Ordering::Acquire) {
                                if let Some(k) = q.jobs.keys().next().cloned() {
                                    let job = q.jobs.remove(&k);
                                    sh.active.store(true, std::sync::atomic::Ordering::Release);
                                    break (k, job, garbage);
                                }
                            }
                            sh.wake.wait(&mut q);
                        }
                    };
                    drop(garbage);
                    let Some(job) = job else { continue };
                    let t = Instant::now();
                    let doc = iron_sheets_md::parse(&job.text);
                    sh.active.store(false, std::sync::atomic::Ordering::Release);
                    let took = t.elapsed();
                    let latency = job.submitted.elapsed();
                    {
                        let mut spare = sh.spare.lock();
                        if spare.len() < 2 {
                            spare.push(job.text);
                        }
                    }
                    if tx.send(ParseDone { path, version: job.version, doc, took, latency }).is_err() {
                        return;
                    }
                    job.ctx.request_repaint();
                }
            })
            .ok()?;
        Some(ParseWorker { shared, done })
    }

    fn in_flight(&self) -> usize {
        self.shared.queue.lock().jobs.len() + usize::from(self.shared.active.load(std::sync::atomic::Ordering::Acquire))
    }

    fn set_paused(&self, paused: bool) {
        let _q = self.shared.queue.lock();
        self.shared.paused.store(paused, std::sync::atomic::Ordering::Release);
        if !paused {
            self.shared.wake.notify_all();
        }
    }

    fn submit(&self, path: &Path, version: u64, text: &str, ctx: &Context) {
        let mut buf = self.shared.spare.lock().pop().unwrap_or_default();
        buf.clear();
        buf.push_str(text);
        let job = ParseJob { version, text: buf, submitted: Instant::now(), ctx: ctx.clone() };
        self.shared.queue.lock().jobs.insert(path.to_path_buf(), job);
        self.shared.wake.notify_one();
    }

    /// Hands a replaced parse to the worker to free.
    fn discard(&self, doc: Arc<Document>) {
        if Arc::strong_count(&doc) == 1 {
            self.shared.queue.lock().garbage.push(doc);
            self.shared.wake.notify_one();
        }
    }
}

impl Drop for ParseWorker {
    fn drop(&mut self) {
        self.shared.closed.store(true, std::sync::atomic::Ordering::Release);
        self.shared.wake.notify_all();
    }
}

#[derive(Default)]
struct DocView {
    doc: Option<Arc<Document>>,
    parsed_version: u64,
    /// Newest version handed to the worker (and when submitted), cleared when its result landed or a sync parse replaced it.
    submitted: Option<(u64, Instant)>,
    dirty_since: Option<Instant>,
    geom: Geom,
    heights: Vec<f32>,
    exact: Vec<bool>,
    tops: Vec<f32>,
    total: f32,
    map_dirty: bool,
    /// Heights changed since `tops` was built (idle layout); see the throttle in `show`.
    tops_dirty: bool,
    /// A block in the visible range changed height: rebuild `tops` this frame.
    urgent: bool,
    /// Scroll offset of the previous frame, to tell a jump (scrollbar drag, reveal, sync) from a small scroll.
    last_off: f32,
    /// Block hashes `heights` belongs to, so an edit reuses the heights of the unchanged blocks around it.
    hashes: Vec<u64>,
    used: u64,
    /// Mermaid blocks keep their last render (see `Env::mermaid_hold`); part of their layout key.
    hold: bool,
    /// The text selection and the doc version it was made in (an edit clears it).
    sel: Option<Sel>,
    sel_version: u64,
}

struct Cached {
    layout: Arc<BlockLayout>,
    used: u64,
}

#[derive(Default)]
struct Core {
    lcache: FxHashMap<Key, Cached>,
    hcache: FxHashMap<Key, f32>,
    images: ImageCache,
    /// The "Render Mermaid diagrams" setting, copied in each frame.
    mermaid: bool,
    /// Present while the module "Code highlighting" is on.
    hl: Option<Arc<codehl::SyntectHighlighter>>,
    textures: FxHashMap<u64, egui::TextureHandle>,
    nbsp_ratio: f32,
    embeds: EmbedCache,
    copied: Option<(Id, Instant)>,
}

#[derive(Default)]
pub struct MarkdownPreview {
    docs: FxHashMap<PathBuf, DocView>,
    worker: Option<ParseWorker>,
    core: Core,
    frame: u64,
    epoch: u32,
    last_ppp: f32,
    /// Blocks laid out in the last frame (visible and idle).
    pub laid_this_frame: usize,
    pub laid_total: usize,
    pub parse_count: usize,
    /// Number of frames in which this preview was drawn (tests use it to see whether it is on screen).
    pub frames_shown: u64,
    /// Blocks not yet laid out exactly (idle work left), for the current document.
    pub unlaid: usize,
    /// Math formulas / images painted as textures in the last frame.
    pub textures_painted: usize,
    /// Clickable things and math/images painted last frame (screen rect, "task:<offset>", "link:<tip>" or "slot").
    pub hits: Vec<(Rect, String)>,
    /// Screen rect of each block painted last frame (tests).
    pub block_rects: Vec<(usize, Rect)>,
    /// Source line of the block under the last right click (for "Show in source").
    ctx_line: Option<u32>,
    /// The last drag autoscroll request, applied by the next frame (it counts as a user scroll).
    autoscroll: bool,
    /// The last primary press was on the preview, so a selection there is what Ctrl+C copies.
    sel_active: bool,
    /// An image in the fullscreen viewer / in its own window.
    pub image_full: Option<ImageWin>,
    pub image_win: Option<ImageWin>,
    /// A Mermaid diagram in the fullscreen viewer / in its own window.
    pub diagram_full: Option<diagram::DiagramView>,
    pub diagram_win: Option<diagram::DiagramView>,
}

/// An image in the fullscreen viewer or its own window (the same viewers as a Mermaid diagram's).
pub struct ImageWin {
    pub path: PathBuf,
    /// The note it was opened from.
    pub note: PathBuf,
    pub view: crate::viewer::View,
    geom: iron_sheets_core::config::AuxWindowGeom,
    focus: bool,
}

/// Natural size of an image in points; a placeholder size until it is decoded.
fn image_size(images: &mut ImageCache, p: &Path, ppp: f32) -> egui::Vec2 {
    images.texture(p).map_or(vec2(120.0, 40.0), |t| t.size_vec2() / ppp)
}

/// An image in a viewer at `r`. Not decoded (again) yet: a placeholder until the worker reports; the decode wakes the UI.
fn paint_image(ui: &egui::Ui, images: &mut ImageCache, p: &Path, r: Rect) {
    match images.texture(p) {
        Some(t) => {
            ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            images.info(p);
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "Loading...", egui::FontId::proportional(16.0), Color32::from_gray(160)); // readable on the dark overlay and in the window
        }
    }
}

fn path_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    p.hash(&mut h);
    h.finish()
}

fn dir_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    p.parent().hash(&mut h);
    h.finish()
}

fn key(e: &EmbedCache, dv: &DocView, doc: &Document, i: usize) -> Key {
    block_key(e, dv.hold, dv.geom, doc, i)
}

fn block_key(e: &EmbedCache, hold: bool, g: Geom, doc: &Document, i: usize) -> Key {
    let b = &doc.blocks[i];
    if !matches!(&b.kind, BlockKind::Code { lang, .. } if lang.eq_ignore_ascii_case("mermaid")) {
        return Key { hash: e.hash(b), g };
    }
    // A diagram layout carries its block's slot: identical diagrams must not share one (see `diagrams_current`).
    let ord = (layout::mermaid_ordinal(doc, i) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    Key { hash: (e.hash(b) ^ ord) ^ (hold as u64) << 60, g }
}

fn recompute_tops(dv: &mut DocView, doc: &Document, size: f32) {
    let mut y = TOP_PAD;
    dv.tops.clear();
    for i in 0..doc.blocks.len() {
        y += gap_at(doc, i, size);
        dv.tops.push(y);
        y += dv.heights[i];
    }
    dv.total = y + BOTTOM_PAD;
    dv.map_dirty = true;
    dv.tops_dirty = false;
    dv.urgent = false;
}

fn find_range(dv: &DocView, y0: f32, y1: f32) -> Range<usize> {
    let a = dv.tops.partition_point(|t| *t < y0).saturating_sub(1);
    let b = dv.tops.partition_point(|t| *t <= y1);
    a..b.max(a)
}

/// Common prefix and suffix length of two hash lists (the suffix never overlaps the prefix).
fn common_ends(a: &[u64], b: &[u64]) -> (usize, usize) {
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
    (p, s)
}

/// `reuse`: the geometry is unchanged, so blocks with the same hash at the start and end of the note keep their
/// height without a cache lookup (an edit changes a few blocks in the middle of 100 000).
fn rebuild_heights(c: &mut Core, dv: &mut DocView, doc: &Document, size: f32, width: f32, keep_old: bool, reuse: bool) {
    let old = std::mem::take(&mut dv.heights);
    let old_exact = std::mem::take(&mut dv.exact);
    let old_hashes = std::mem::take(&mut dv.hashes);
    dv.hashes = doc.blocks.iter().map(|b| b.hash).collect();
    let (p, suf) = if reuse && old.len() == old_hashes.len() && old.len() == old_exact.len() { common_ends(&old_hashes, &dv.hashes) } else { (0, 0) };
    let n = doc.blocks.len();
    dv.heights = Vec::with_capacity(n);
    dv.exact = Vec::with_capacity(n);
    if c.hcache.len() > 200_000 {
        c.hcache.clear();
    }
    dv.heights.extend_from_slice(&old[..p]);
    dv.exact.extend_from_slice(&old_exact[..p]);
    for i in p..n - suf {
        match c.hcache.get(&key(&c.embeds, dv, doc, i)) {
            Some(h) => {
                dv.heights.push(*h);
                dv.exact.push(true);
            }
            None => {
                let h = if keep_old && old.len() == n { old[i] } else { estimate(&doc.blocks[i], width, size) };
                dv.heights.push(h);
                dv.exact.push(false);
            }
        }
    }
    dv.heights.extend_from_slice(&old[old.len() - suf..]);
    dv.exact.extend_from_slice(&old_exact[old.len() - suf..]);
    recompute_tops(dv, doc, size);
}

/// Applies the results the worker finished. Results of other documents go to their views (`others` does not
/// hold the current one); returns true when `dv` got a new parse.
fn apply_results(
    worker: &ParseWorker,
    path: &Path,
    dv: &mut DocView,
    others: &mut FxHashMap<PathBuf, DocView>,
    perf: &mut crate::perf::Perf,
    parse_count: &mut usize,
) -> bool {
    let mut changed = false;
    while let Ok(r) = worker.done.try_recv() {
        let target = if r.path == path { Some(&mut *dv) } else { others.get_mut(&r.path) };
        // Results of a view with nothing outstanding are stale (a sync parse replaced the job).
        let Some(t) = target.filter(|t| t.submitted.is_some()) else { continue };
        perf.record("md.parse", r.took);
        perf.record("md.parse.latency", r.latency);
        *parse_count += 1;
        if let Some(old) = t.doc.replace(Arc::new(r.doc)) {
            worker.discard(old);
        }
        t.parsed_version = r.version;
        if t.submitted.is_some_and(|(v, _)| v == r.version) {
            t.submitted = None;
        }
        changed |= r.path == path;
    }
    changed
}

/// Parse bookkeeping. Returns true when the document changed.
#[allow(clippy::too_many_arguments)]
fn update_parse(
    worker: &mut Option<ParseWorker>,
    others: &mut FxHashMap<PathBuf, DocView>,
    path: &Path,
    dv: &mut DocView,
    text: &str,
    version: u64,
    ctx: &Context,
    perf: &mut crate::perf::Perf,
    parse_count: &mut usize,
    prefetched: Option<Arc<Document>>,
) -> bool {
    let mut changed = false;
    if let Some(w) = worker {
        changed = apply_results(w, path, dv, others, perf, parse_count);
    }
    if dv.doc.is_none() || dv.parsed_version != version {
        if let Some(d) = prefetched {
            // Parsed in the background before the note was opened (same text).
            dv.doc = Some(d);
            dv.parsed_version = version;
            dv.submitted = None;
            dv.dirty_since = None;
            return true;
        }
        // Small notes (and the first parse of a note up to BIG_TEXT, so there is no empty first frame) are parsed
        // right here; a keystroke in a bigger note must not wait for the parser (3-6 ms for 100 KB), so those are
        // parsed by the worker thread and the preview shows the previous parse until it lands.
        if text.len() <= SYNC_PARSE_MAX || (dv.doc.is_none() && text.len() <= BIG_TEXT) {
            let t = Instant::now();
            let d = iron_sheets_md::parse(text);
            perf.record("md.parse", t.elapsed());
            *parse_count += 1;
            dv.doc = Some(Arc::new(d));
            dv.parsed_version = version;
            dv.submitted = None;
            dv.dirty_since = None;
            changed = true;
        } else if dv.submitted.is_none_or(|(v, _)| v != version) {
            let debounce = if text.len() > BIG_TEXT { DEBOUNCE } else { Duration::ZERO };
            let since = *dv.dirty_since.get_or_insert_with(Instant::now);
            if dv.doc.is_none() || since.elapsed() >= debounce {
                if worker.is_none() {
                    *worker = ParseWorker::start();
                }
                match worker {
                    Some(w) => {
                        w.submit(path, version, text, ctx);
                        dv.submitted = Some((version, Instant::now()));
                        dv.dirty_since = None;
                    }
                    None => {
                        // No thread available: parse here rather than never.
                        let d = iron_sheets_md::parse(text);
                        *parse_count += 1;
                        dv.doc = Some(Arc::new(d));
                        dv.parsed_version = version;
                        dv.dirty_since = None;
                        changed = true;
                    }
                }
            } else {
                ctx.request_repaint_after(debounce - since.elapsed());
            }
        }
    }
    changed
}

/// Lay out block `i` (or fetch it from the cache), updating the stored height.
#[allow(clippy::too_many_arguments)]
fn get_layout(
    c: &mut Core,
    dv: &mut DocView,
    doc: &Document,
    i: usize,
    ctx: &Context,
    style: &PreviewStyle,
    math: &dyn crate::services::MathRenderer,
    index: &Arc<parking_lot::RwLock<iron_sheets_core::index::Index>>,
    docs: &iron_sheets_app_core::docs::DocStore,
    note: &Path,
    ppp: f32,
    frame: u64,
    keep: bool,
    laid: &mut usize,
) -> Arc<BlockLayout> {
    let k = key(&c.embeds, dv, doc, i);
    let cached = c.lcache.get_mut(&k).filter(|e| !e.layout.pending && e.layout.diagrams_current(&c.images)).map(|e| {
        e.used = frame;
        e.layout.clone()
    });
    let hit = cached.is_some();
    let l = cached.unwrap_or_else(|| {
        let mut env = Env {
            ctx,
            style,
            math,
            hl: c.hl.as_deref().map_or(&crate::services::NoHighlight, |h| h),
            index,
            note,
            images: &mut c.images,
            mermaid: c.mermaid,
            mermaid_hold: dv.hold,
            mermaid_ordinal: layout::mermaid_ordinal(doc, i),
            nbsp_ratio: c.nbsp_ratio,
            ppp,
            embeds: &mut c.embeds,
            open: docs,
            stack: Vec::new(),
            budget: &mut 0,
        };
        *laid += 1;
        Arc::new(layout_block(&mut env, &doc.blocks[i], dv.geom.width as f32))
    });
    // Also on a cache hit: a block with the same content as one laid before (a repeated list item) still carries an
    // estimated height and must take the exact one.
    if (dv.heights[i] - l.height).abs() > 0.01 {
        dv.heights[i] = l.height;
        dv.tops_dirty = true;
        dv.urgent |= keep;
    }
    dv.exact[i] = true;
    if hit {
        return l;
    }
    c.hcache.insert(k, l.height);
    if keep {
        c.lcache.insert(k, Cached { layout: l.clone(), used: frame });
    }
    l
}

/// Faint text rows in place of the preview while the first parse of a big note runs (the shape of a note, not a label).
fn loading_skeleton(ui: &mut egui::Ui, animations: bool) {
    let rect = ui.available_rect_before_wrap();
    let pulse = ui.style().animation_time > 0.0 && animations;
    let fill = crate::skeleton::fill(ui, pulse);
    let max_w = (rect.width() - 2.0 * MARGIN).clamp(24.0, MAX_W);
    let x = rect.left() + ((rect.width() - max_w) / 2.0).max(MARGIN);
    crate::skeleton::text_rows(ui, rect, x, max_w, 20.0, fill);
    ui.allocate_rect(rect, Sense::hover());
}

struct Pc<'a> {
    style: &'a PreviewStyle,
    doc: &'a Document,
    text: &'a str,
    text_ok: bool,
    path: &'a Path,
    tab_id: u64,
    tops: &'a [f32],
    textures: &'a mut FxHashMap<u64, egui::TextureHandle>,
    images: &'a mut ImageCache,
    cmds: &'a mut Vec<Command>,
    hits: &'a mut Vec<(Rect, String)>,
    scroll_to: &'a mut Option<f32>,
    painted: usize,
    /// Math syntax is Auto: hovering a formula says which syntax it was rendered in.
    math_tip: bool,
    /// Collapsed embed whose arrow was clicked (its block hash).
    toggle: Option<u64>,
    /// Ordered selection and its colour, for the highlight behind the text.
    sel: Option<(SelPos, SelPos)>,
    sel_color: Color32,
    /// Top-level block being painted.
    cur: usize,
    /// Added to the segment numbers while an embed's blocks are painted (they share the host block's numbering).
    seg_off: usize,
    /// The note whose blocks are painted when it is not the shown one (an embed).
    embed: Option<Arc<PathBuf>>,
    /// An image's Fullscreen (false) or Open in new window (true) button was clicked.
    view_image: Option<(PathBuf, bool)>,
    /// A diagram's Fullscreen (false) or Open in new window (true) button was clicked.
    open_diagram: Option<(u64, bool)>,
    /// The code block whose Copy button was clicked last, and when.
    copied: &'a mut Option<(Id, Instant)>,
    /// The internal link under the pointer (for the page preview popup).
    hover: &'a mut Option<popup::LinkHover>,
}

/// The row and glyph at char index `i`.
fn char_row(g: &Galley, i: usize) -> Option<(&egui::epaint::text::PlacedRow, &egui::epaint::text::Glyph)> {
    let mut base = 0usize;
    for row in &g.rows {
        let local = i.checked_sub(base)?;
        if let Some(gl) = row.glyphs.get(local) {
            return Some((row, gl));
        }
        base += row.char_count_including_newline().0;
    }
    None
}

fn char_rect(g: &Galley, i: usize) -> Option<Rect> {
    let (row, gl) = char_row(g, i)?;
    Some(Rect::from_min_size(pos2(row.pos.x + gl.pos.x, row.pos.y), vec2(gl.advance_width, row.size.y)))
}

fn char_at_pos(g: &Galley, pos: egui::Vec2) -> Option<usize> {
    let mut base = 0usize;
    for row in &g.rows {
        if pos.y >= row.pos.y && pos.y < row.pos.y + row.size.y {
            let x = pos.x - row.pos.x;
            for (k, gl) in row.glyphs.iter().enumerate() {
                if gl.advance_width > 0.0 && x >= gl.pos.x && x < gl.pos.x + gl.advance_width {
                    return Some(base + k);
                }
            }
            return None;
        }
        base += row.char_count_including_newline().0;
    }
    None
}

fn tex_for_math(pc: &mut Pc, ctx: &Context, bm: &crate::services::MathBitmap) -> TextureId {
    if pc.textures.len() > 400 {
        pc.textures.clear();
    }
    pc.textures
        .entry(bm.key)
        .or_insert_with(|| {
            let img = egui::ColorImage::from_rgba_premultiplied([bm.width as usize, bm.height as usize], &bm.rgba_premul);
            ctx.load_texture(format!("md-math-{}", bm.key), img, egui::TextureOptions::LINEAR)
        })
        .id()
}

/// Hover text for a formula in Auto mode; only looked at while the pointer is over the formula.
fn math_tip(ui: &egui::Ui, pc: &Pc, rect: Rect, bm: &crate::services::MathBitmap) {
    if pc.math_tip && ui.rect_contains_pointer(rect) {
        let text = if bm.latex { "Rendered as LaTeX" } else { "Rendered as Typst" };
        ui.interact(rect, ui.scope_id().with(("math_tip", bm.key)), egui::Sense::hover()).on_hover_text(text);
    }
}

impl Pc<'_> {
    /// The note the painted blocks belong to (links resolve against it).
    fn note(&self) -> &Path {
        self.embed.as_deref().map_or(self.path, |p| p.as_path())
    }
}

fn scroll_heading(pc: &mut Pc, name: &str) {
    let name = percent_decode(name);
    if let Some(p) = pc.embed.clone() {
        // Inside an embed the heading is in the embedded note: open it there.
        let note = p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        pc.cmds.push(Command::OpenLink { note, heading: Some(name), from: Some(p.to_path_buf()), new_tab: false });
        return;
    }
    if let Some(i) = pc.doc.find_heading(&name) {
        *pc.scroll_to = Some((pc.tops[i] - 8.0).max(0.0));
    }
}

fn open_url(pc: &mut Pc, u: &str, new_tab: bool) {
    if let Some(h) = u.strip_prefix('#') {
        return scroll_heading(pc, h);
    }
    pc.cmds.extend(url_command(pc.note(), u, new_tab));
}

/// What a link target other than `#heading` opens, seen from `note`: a web address, a file next to the note or a note.
fn url_command(note: &Path, u: &str, new_tab: bool) -> Option<Command> {
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
        return Some(Command::OpenUrl(u.to_string()));
    }
    let (p, frag) = match u.split_once('#') {
        Some((p, f)) => (p, Some(percent_decode(f))),
        None => (u, None),
    };
    let dec = percent_decode(p);
    if dec.is_empty() {
        return None;
    }
    let full = note.parent().unwrap_or_else(|| Path::new("")).join(&dec);
    if full.is_file() {
        return Some(Command::OpenPath { path: full, new_tab, line: None });
    }
    let name = dec.strip_suffix(".md").unwrap_or(&dec).to_string();
    Some(Command::OpenLink { note: name, heading: frag, from: Some(note.to_path_buf()), new_tab })
}

fn click_link(pc: &mut Pc, l: &LinkSpan, new_tab: bool) {
    let LinkAction::Target(t) = &l.action else { return };
    match t {
        LinkTarget::Url(u) => open_url(pc, u, new_tab),
        LinkTarget::Wiki { note, heading, .. } => {
            if note.is_empty() {
                if let Some(h) = heading {
                    scroll_heading(pc, h);
                }
            } else {
                pc.cmds.push(Command::OpenLink { note: note.clone(), heading: heading.clone(), from: Some(pc.note().to_path_buf()), new_tab });
            }
        }
        LinkTarget::Footnote(_) if pc.embed.is_some() => {
            let path = pc.note().to_path_buf();
            pc.cmds.push(Command::OpenPath { path, new_tab, line: None });
        }
        LinkTarget::Footnote(label) => {
            let i = pc.doc.blocks.iter().position(|b| b.containers.iter().any(|c| matches!(&c.kind, ContainerKind::Footnote { label: l } if l == label)));
            if let Some(i) = i {
                *pc.scroll_to = Some((pc.tops[i] - 8.0).max(0.0));
            }
        }
        LinkTarget::Tag(t) => pc.cmds.push(Command::GlobalSearch { query: Some(format!("#{t}")) }),
    }
}

/// Selection highlight behind the galley of segment `seg` of the current block.
fn highlight(ui: &egui::Ui, pc: &Pc, pos: Pos2, g: &Galley, seg: usize) {
    let Some((lo, hi)) = pc.sel else { return };
    if let Some(r) = select::seg_range(lo, hi, pc.cur, pc.seg_off + seg) {
        for rect in select::sel_rects(g, &r) {
            ui.painter().rect_filled(rect.translate(pos.to_vec2()), 0.0, pc.sel_color);
        }
    }
}

fn paint_tb(ui: &mut egui::Ui, pc: &mut Pc, pos: Pos2, tb: &TextBox, id: Id, seg: usize) {
    let ctx = ui.ctx().clone();
    for k in &tb.kbds {
        let r = k.rect.translate(pos.to_vec2());
        ui.painter().rect(r, 3.0, k.fill, Stroke::new(1.0, k.border), egui::StrokeKind::Inside);
        pc.hits.push((r, "kbd".into())); // for tests
    }
    highlight(ui, pc, pos, &tb.galley, seg);
    ui.painter().galley(pos, tb.galley.clone(), Color32::PLACEHOLDER);
    let rect = Rect::from_min_size(pos, tb.size());
    // Before the image buttons of the slots: egui gives a tied hit to the widget registered last.
    let resp = (!tb.links.is_empty()).then(|| ui.interact(rect, id, Sense::click()));
    let mut over_image = false;
    for s in tb.slots.iter().filter(|s| !matches!(s.content, SlotContent::Text)) {
        // The row comes from the char lookup: finding it again by y picks the previous row when rounding makes rows overlap.
        let Some((row, gl)) = char_row(&tb.galley, s.ch) else { continue };
        let r = Rect::from_min_size(pos2(row.pos.x + gl.pos.x, row.pos.y), vec2(gl.advance_width, row.size.y));
        let baseline = row.pos.y + (row.size.y - s.row_l) / 2.0 + s.row_a;
        let rect = Rect::from_min_size(pos + vec2(r.min.x, baseline - s.baseline), vec2(s.w, s.h));
        let tex = match &s.content {
            SlotContent::Text => None,
            SlotContent::Math(bm) => Some(tex_for_math(pc, &ctx, bm)),
            SlotContent::Image(p) => pc.images.texture(p).map(|t| t.id()),
            #[cfg(feature = "excalidraw")]
            SlotContent::Excalidraw { tex_key: p, .. } => pc.images.texture(p).map(|t| t.id()),
        };
        if let Some(t) = tex {
            pc.painted += 1;
            ui.painter().image(t, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            pc.hits.push((rect, "slot".into())); // for tests
            match &s.content {
                SlotContent::Text => {}
                SlotContent::Math(bm) => math_tip(ui, pc, rect, bm),
                SlotContent::Image(p) => {
                    // An image inside a link is the link: a click follows it, no view buttons.
                    let linked = tb.links.iter().any(|l| matches!(l.action, LinkAction::Target(_)) && l.chars.contains(&s.ch));
                    // An image is a picture, not text: no caret over it or its buttons (like a diagram's background).
                    over_image |= ui.rect_contains_pointer(if linked { rect } else { view_area(rect) });
                    if !linked {
                        if let Some(w) = view_buttons(ui, pc, rect, id.with(("slot-img", s.ch))) {
                            pc.view_image = Some((p.clone(), w));
                        }
                    }
                }
                #[cfg(feature = "excalidraw")]
                SlotContent::Excalidraw { target, .. } => {
                    over_image |= ui.rect_contains_pointer(rect);
                    let resp = ui.interact(rect, id.with(("slot-exc", s.ch)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                    pc.hits.push((rect, "excalidraw".into()));
                    if resp.clicked() || resp.middle_clicked() {
                        let new_tab = ui.input(|i| i.modifiers.command) || resp.middle_clicked();
                        pc.cmds.push(Command::OpenPath { path: target.clone(), new_tab, line: None });
                    }
                }
            }
        }
    }
    // A caret only over the text itself, not the empty end of a short row.
    let over_text = ui.rect_contains_pointer(rect)
        && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| tb.galley.rows.iter().any(|r| r.rect_without_leading_space().translate(pos.to_vec2()).contains(p)));
    if over_text && !over_image {
        ctx.set_cursor_icon(CursorIcon::Text);
    }
    let Some(resp) = resp else { return };
    for l in &tb.links {
        if let Some(r) = char_rect(&tb.galley, l.chars.start) {
            pc.hits.push((r.translate(pos.to_vec2()), format!("link:{}", l.tip)));
        }
    }
    if let Some(hp) = resp.hover_pos() {
        if let Some(ci) = char_at_pos(&tb.galley, hp - pos) {
            if let Some(l) = tb.links.iter().find(|l| l.chars.contains(&ci)) {
                if matches!(l.action, LinkAction::Target(_)) {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if let LinkAction::Target(t) = &l.action {
                    if let (Some((note, heading)), Some(r)) = (popup::internal_target(t), char_rect(&tb.galley, ci)) {
                        *pc.hover = Some(popup::LinkHover { note, heading, from: pc.note().to_path_buf(), rect: r.translate(pos.to_vec2()) });
                    }
                }
                let tip = l.tip.clone();
                egui::Tooltip::for_widget(&resp).at_pointer().show(|ui| {
                    ui.label(tip);
                });
                let new_tab = ctx.input(|i| i.modifiers.command) || resp.middle_clicked();
                if resp.clicked() || resp.middle_clicked() {
                    click_link(pc, l, new_tab);
                }
            }
        }
    }
}

const COPIED_FOR: Duration = Duration::from_millis(1200);
const COPY_W: f32 = 24.0;

/// Copy icon button in the top-right corner of a code block; clicking copies the block's text (no fences) and shows
/// a check mark for a moment. Returns whether the pointer is over the button.
fn copy_button(ui: &mut egui::Ui, pc: &mut Pc, code: Rect, id: Id, just_copied: bool, b: &Block) -> bool {
    let st = pc.style;
    let r = Rect::from_min_size(pos2(code.max.x - COPY_W - 4.0, code.min.y + 3.0), vec2(COPY_W, 18.0));
    let rs = ui.interact(r, id, Sense::click()).on_hover_text("Copy");
    crate::icons::name(&rs, "Copy");
    if rs.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if rs.clicked() {
        ui.ctx().copy_text(select::block_text(b));
        *pc.copied = Some((id, Instant::now()));
        ui.ctx().request_repaint_after(COPIED_FOR);
    }
    let fill = if rs.hovered() { st.rule.gamma_multiply(0.5) } else { st.rule.gamma_multiply(0.25) };
    ui.painter().rect_filled(r, 3.0, fill);
    crate::icons::paint(ui.painter(), r, if just_copied { Icon::Check } else { Icon::Copy }, st.text);
    pc.hits.push((r, "code-copy".into())); // for tests
    rs.hovered()
}

/// A picture with the strip of its view buttons, which stick out of a tiny picture.
fn view_area(pic: Rect) -> Rect {
    pic.union(Rect::from_min_max(pos2(pic.max.x - 60.0, pic.min.y), pos2(pic.max.x, pic.min.y + 26.0)))
}

/// Fullscreen and open-in-window icon buttons in the top-right corner of a diagram or image (shown while the pointer
/// or the buttons are over it; on a tiny picture they stick out of it). Returns `Some(true)` for the window,
/// `Some(false)` for fullscreen.
fn view_buttons(ui: &mut egui::Ui, pc: &mut Pc, pic: Rect, id: Id) -> Option<bool> {
    if !ui.rect_contains_pointer(view_area(pic)) {
        return None;
    }
    let st = pc.style;
    let mut clicked = None;
    let mut x = pic.max.x - 4.0;
    for (k, (tip, icon)) in [("Open in new window", Icon::Window), ("Fullscreen", Icon::Fullscreen)].into_iter().enumerate() {
        x -= 24.0;
        let r = Rect::from_min_size(pos2(x, pic.min.y + 4.0), vec2(24.0, 18.0));
        x -= 4.0;
        let rs = ui.interact(r, id.with(("diagram-btn", k)), Sense::click()).on_hover_text(tip);
        crate::icons::name(&rs, tip);
        if rs.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if rs.clicked() {
            clicked = Some(k == 0);
        }
        ui.painter().rect_filled(r, 3.0, if rs.hovered() { st.rule.gamma_multiply(0.7) } else { st.rule.gamma_multiply(0.45) });
        crate::icons::paint(ui.painter(), r, icon, st.text);
        pc.hits.push((r, if k == 0 { "view-window" } else { "view-fullscreen" }.into())); // for tests
    }
    clicked
}

fn paint_table(ui: &mut egui::Ui, pc: &mut Pc, origin: Pos2, t: &TableLayout, id: Id) {
    let st = pc.style;
    let ncols = t.cols.len();
    let stroke = Stroke::new(1.0, st.table_border);
    let mut y = origin.y;
    let total_h: f32 = t.rows_h.iter().sum();
    ui.painter().rect_filled(Rect::from_min_size(origin, vec2(t.total_w, t.rows_h[0])), 0.0, st.code_bg);
    for (r, rh) in t.rows_h.iter().enumerate() {
        let mut x = origin.x;
        for c in 0..ncols {
            let tb = &t.cells[r * ncols + c];
            paint_tb(ui, pc, pos2(x + TABLE_PAD_X + cell_dx(t, c, tb), y + TABLE_PAD_Y), tb, id.with((r, c)), r * ncols + c);
            x += t.cols[c];
        }
        y += rh;
        ui.painter().hline(origin.x..=origin.x + t.total_w, y, stroke);
    }
    ui.painter().hline(origin.x..=origin.x + t.total_w, origin.y, stroke);
    let mut x = origin.x;
    for w in std::iter::once(&0.0).chain(t.cols.iter()) {
        x += w;
        ui.painter().vline(x, origin.y..=origin.y + total_h, stroke);
    }
}

#[allow(clippy::too_many_arguments)]
/// Paints block `i` of `doc` at screen `top` in the column starting at `x0` (the note's own blocks, and those of an embed).
fn paint_block(ui: &mut egui::Ui, pc: &mut Pc, doc: &Document, i: usize, top: f32, x0: f32, col_w: f32, l: &BlockLayout) {
    let st = pc.style;
    let b = &doc.blocks[i];
    let bottom = top + l.height;
    let right = x0 + col_w;
    let block_rect = Rect::from_min_max(pos2(x0, top), pos2(right, bottom));
    let tab_id = pc.tab_id;
    let gap_before = gap_at(doc, i, st.body_size);
    let mut lx = x0;
    let mut lxs = Vec::with_capacity(b.containers.len());
    let mut list_depth = 0;
    let yc = top + l.head_h + l.first_line_h / 2.0;
    for (k, c) in b.containers.iter().enumerate() {
        lxs.push(lx);
        let ind = container_indent(c);
        let up = i > 0 && doc.blocks[i - 1].containers.iter().any(|p| p.id == c.id);
        let ytop = if up { top - gap_before } else { top };
        match &c.kind {
            ContainerKind::Quote => {
                ui.painter().rect_filled(Rect::from_min_max(pos2(lx + 3.0, ytop), pos2(lx + 6.0, bottom)), 0.0, st.quote_bar);
            }
            ContainerKind::Alert { kind, .. } => {
                let col = alert_color(kind, st.dark);
                ui.painter().rect_filled(Rect::from_min_max(pos2(lx, ytop), pos2(right, bottom)), 0.0, col.gamma_multiply(0.08));
                ui.painter().rect_filled(Rect::from_min_max(pos2(lx, ytop), pos2(lx + 3.0, bottom)), 0.0, col);
            }
            ContainerKind::ListItem { ordered, task } => {
                if c.first {
                    let mx = lx + ind - 11.0;
                    if let Some(t) = task {
                        let r = Rect::from_center_size(pos2(mx - 2.0, yc), vec2(14.0, 14.0));
                        let id = ui.scope_id().with(("md-task", tab_id, i, k));
                        let rs = ui.interact(r, id, Sense::click());
                        let fill = if t.checked { st.link } else { Color32::TRANSPARENT };
                        ui.painter().rect(r, 3.0, fill, Stroke::new(1.2, if rs.hovered() { st.link } else { st.text }), egui::StrokeKind::Inside);
                        if t.checked {
                            let s = Stroke::new(1.8, Color32::WHITE);
                            let c0 = r.center();
                            ui.painter().line_segment([c0 + vec2(-3.5, 0.0), c0 + vec2(-1.0, 3.0)], s);
                            ui.painter().line_segment([c0 + vec2(-1.0, 3.0), c0 + vec2(4.0, -3.5)], s);
                        }
                        pc.hits.push((r, format!("task:{}", t.offset)));
                        if rs.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if rs.clicked() && pc.text_ok {
                            if let Some(op) = iron_sheets_md::edit::toggle_task(pc.text, t.offset as usize) {
                                pc.cmds.push(Command::ApplyEdit { path: pc.path.to_path_buf(), range: op.range, insert: op.insert, select: None });
                            }
                        }
                    } else if let Some(n) = ordered {
                        ui.painter().text(
                            pos2(lx + ind - 6.0, yc),
                            Align2::RIGHT_CENTER,
                            format!("{n}."),
                            egui::FontId::new(st.body_size, crate::theme::families().preview),
                            st.text,
                        );
                    } else if list_depth % 2 == 0 {
                        ui.painter().circle_filled(pos2(mx, yc), 2.5, st.text);
                    } else {
                        ui.painter().circle_stroke(pos2(mx, yc), 2.5, Stroke::new(1.0, st.text));
                    }
                }
                list_depth += 1;
            }
            ContainerKind::Footnote { label } => {
                if c.first {
                    ui.painter().text(
                        pos2(lx, yc),
                        Align2::LEFT_CENTER,
                        format!("{label}."),
                        egui::FontId::new(st.body_size * 0.85, crate::theme::families().preview),
                        st.link,
                    );
                }
            }
        }
        lx += ind;
    }
    let mut ty = top;
    for (k, g, col) in &l.titles {
        // `usize::MAX`: a note above the body (no container), aligned with it.
        let x = lxs.get(*k).map_or(x0 + l.indent, |x| x + 12.0);
        ui.painter().galley(pos2(x, ty), g.clone(), *col);
        ty += g.size().y + 4.0;
    }
    let bx = x0 + l.indent;
    let by = top + l.head_h;
    let bw = right - bx;
    let id = ui.scope_id().with(("md-body", tab_id, i));
    match &l.body {
        Body::Text { tb, rule_below } => {
            paint_tb(ui, pc, pos2(bx, by), tb, id, 0);
            if *rule_below {
                let y = by + tb.size().y + 5.0;
                ui.painter().hline(bx..=right, y, Stroke::new(1.0, st.rule));
            }
        }
        Body::Code { galley, lang, text_w } => {
            let r = Rect::from_min_size(pos2(bx, by), vec2(bw, l.body_h));
            ui.painter().rect_filled(r, 4.0, st.code_bg);
            highlight(ui, pc, r.min + vec2(CODE_PAD, CODE_PAD), galley, 0);
            if text_w + 2.0 * CODE_PAD > bw {
                let mut child = ui.new_child(UiBuilder::new().id_salt(("md-code", tab_id, i)).max_rect(r));
                egui::ScrollArea::horizontal().id_salt(("md-code-sa", tab_id, i)).auto_shrink([false, false]).show(&mut child, |ui| {
                    let (rr, _) = ui.allocate_exact_size(vec2(text_w + 2.0 * CODE_PAD, l.body_h), Sense::hover());
                    ui.painter().galley(rr.min + vec2(CODE_PAD, CODE_PAD), galley.clone(), Color32::PLACEHOLDER);
                });
            } else {
                ui.painter().galley(r.min + vec2(CODE_PAD, CODE_PAD), galley.clone(), Color32::PLACEHOLDER);
            }
            let copy_id = ui.scope_id().with(("md-code-copy", pc.tab_id, i));
            let just_copied = pc.copied.is_some_and(|(c, t)| c == copy_id && t.elapsed() < COPIED_FOR);
            let copy_btn = (ui.rect_contains_pointer(r) || just_copied).then(|| copy_button(ui, pc, r, copy_id, just_copied, b));
            let lang_x = r.max.x - 6.0 - if copy_btn.is_some() { COPY_W } else { 0.0 };
            if !lang.is_empty() && text_w + 2.0 * CODE_PAD + 60.0 < bw {
                ui.painter().text(pos2(lang_x, r.min.y + 2.0), Align2::RIGHT_TOP, lang, egui::FontId::proportional(10.5), st.rule);
            }
            if ui.rect_contains_pointer(r) && !copy_btn.unwrap_or(false) {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
        }
        Body::Table(t) => {
            if t.total_w > bw {
                let r = Rect::from_min_size(pos2(bx, by), vec2(bw, l.body_h));
                let mut child = ui.new_child(UiBuilder::new().id_salt(("md-tbl", tab_id, i)).max_rect(r));
                egui::ScrollArea::horizontal().id_salt(("md-tbl-sa", tab_id, i)).auto_shrink([false, false]).show(&mut child, |ui| {
                    let (rr, _) = ui.allocate_exact_size(vec2(t.total_w, l.body_h), Sense::hover());
                    paint_table(ui, pc, rr.min, t, id);
                });
            } else {
                paint_table(ui, pc, pos2(bx, by), t, id);
            }
        }
        Body::Rule => {
            ui.painter().hline(bx..=right, by + 5.0, Stroke::new(1.5, st.rule));
        }
        Body::Math { bm, w, h } => {
            let ctx = ui.ctx().clone();
            let t = tex_for_math(pc, &ctx, bm);
            pc.painted += 1;
            let r = Rect::from_min_size(pos2(bx + ((bw - w) / 2.0).max(0.0), by + 4.0), vec2(*w, *h));
            ui.painter().image(t, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            math_tip(ui, pc, r, bm);
        }
        Body::Embed(e) => {
            // Widget ids are the host note's: an embed is drawn without them. Its text is selected through the host
            // block's selection (see `select::segs`).
            // Task boxes are read-only here: their offsets belong to the embedded note, not to the text in `pc.text`.
            let (tid, ok, outer) = (pc.tab_id, pc.text_ok, pc.embed.take());
            pc.tab_id = tid.rotate_left(13) ^ (i as u64 + 1);
            pc.text_ok = false;
            pc.embed = Some(e.path.clone());
            paint_embed(ui, pc, e, Rect::from_min_size(pos2(bx, by), vec2(bw, l.body_h)), id);
            (pc.tab_id, pc.text_ok, pc.embed) = (tid, ok, outer);
        }
        Body::Diagram(d) => {
            let r = Rect::from_min_size(pos2(bx, by), d.size);
            match pc.images.texture(&d.path) {
                Some(t) => {
                    ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    ui.painter().rect_filled(r, 2.0, st.code_bg);
                    pc.hits.push((r, "diagram-blank".into())); // for tests
                }
            }
            pc.hits.push((r, "diagram".into())); // for tests
            // The text is invisible and lies over the picture: a translucent highlight keeps it readable.
            let sel_color = pc.sel_color;
            pc.sel_color = sel_color.gamma_multiply(0.4);
            // Registered before the text boxes: where both lie, the (later) text box wins.
            for (sh, k) in &d.shapes {
                let sh = sh.translate(r.min.to_vec2());
                let resp = ui.interact(sh, id.with(("diagram-node", *k)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                pc.hits.push((sh, "diagram-node".into())); // for tests
                if let Some(l) = d.texts[*k].1.links.first() {
                    if resp.clicked() || resp.middle_clicked() {
                        click_link(pc, l, ui.input(|i| i.modifiers.command) || resp.middle_clicked());
                    }
                }
            }
            for (k, (p, tb)) in d.texts.iter().enumerate() {
                pc.hits.push((Rect::from_min_size(r.min + *p, tb.size()), "diagram-text".into())); // for tests
                paint_tb(ui, pc, r.min + *p, tb, id.with(("diagram", k)), k);
            }
            pc.sel_color = sel_color;
            if let Some(g) = &d.err {
                ui.painter().galley(pos2(bx, by + d.size.y + 4.0), g.clone(), Color32::PLACEHOLDER);
            }
            if let Some(w) = view_buttons(ui, pc, r, id) {
                pc.open_diagram = Some((d.slot, w));
            }
        }
        #[cfg(feature = "excalidraw")]
        Body::Excalidraw(e) => {
            let r = Rect::from_min_size(pos2(bx, by), e.size);
            if e.size.y > 0.0 {
                match pc.images.texture(&e.path) {
                    Some(t) => {
                        pc.painted += 1;
                        ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        ui.painter().rect_filled(r, 4.0, st.code_bg);
                    }
                }
            }
            if let Some(g) = &e.err {
                let ey = if e.size.y > 0.0 { by + e.size.y + 4.0 } else { by + 4.0 };
                ui.painter().galley(pos2(bx, ey), g.clone(), Color32::PLACEHOLDER);
            }
            let click_r = if e.size.y > 0.0 {
                r
            } else if let Some(g) = &e.err {
                Rect::from_min_size(pos2(bx, by), vec2(g.size().x.max(120.0), g.size().y + 8.0))
            } else {
                r
            };
            pc.hits.push((click_r, "excalidraw".into()));
            let resp = ui
                .interact(click_r, id.with("excalidraw"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Click to open Excalidraw drawing");
            if resp.clicked() || resp.middle_clicked() {
                let new_tab = ui.input(|i| i.modifiers.command) || resp.middle_clicked();
                pc.cmds.push(Command::OpenPath {
                    path: e.target.clone(),
                    new_tab,
                    line: None,
                });
            }
        }
        Body::Image { path, size } => {
            let r = Rect::from_min_size(pos2(bx, by), *size);
            match pc.images.texture(path) {
                Some(t) => {
                    ui.painter().image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    pc.hits.push((r, "image".into())); // for tests
                    if let Some(w) = view_buttons(ui, pc, r, id.with("img")) {
                        pc.view_image = Some((path.clone(), w));
                    }
                }
                None => {
                    ui.painter().rect_filled(r, 2.0, st.code_bg);
                }
            }
        }
    }
    // Blocks without text (math, image, rule, embed) inside the selection are tinted as a whole.
    if let Some((lo, hi)) = pc.sel {
        if pc.embed.is_none()
            && i >= lo.blk
            && i <= hi.blk
            && !matches!(l.body, Body::Text { .. } | Body::Code { .. } | Body::Table(_) | Body::Diagram(_) | Body::Embed(_))
        {
            ui.painter().rect_filled(block_rect, 2.0, pc.sel_color.gamma_multiply(0.5));
        }
    }
}

fn paint_embed(ui: &mut egui::Ui, pc: &mut Pc, e: &EmbedLayout, frame: Rect, id: Id) {
    let st = pc.style;
    ui.painter().rect_filled(frame, 4.0, st.code_bg);
    ui.painter().rect_filled(Rect::from_min_size(frame.min, vec2(3.0, frame.height())), 0.0, st.rule);
    let x = frame.min.x + EMBED_PAD;
    let title_pos = pos2(x, frame.min.y + EMBED_PAD);
    let base = pc.seg_off;
    // Title and "show more" are chrome: not selectable, so they are painted without the selection.
    let sel = pc.sel.take();
    paint_tb(ui, pc, title_pos, &e.title, id.with("title"), 0);
    pc.sel = sel;
    if let Some(h) = e.toggle {
        let arrow = Rect::from_min_size(title_pos, vec2(e.title.size().y * 1.2, e.title.size().y));
        let rs = ui.interact(arrow, id.with("toggle"), Sense::click());
        if rs.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if rs.clicked() {
            pc.toggle = Some(h);
        }
    }
    let y0 = title_pos.y + e.title.size().y + 4.0;
    let mut n = base;
    for (k, y, l) in &e.items {
        pc.seg_off = n;
        paint_block(ui, pc, &e.doc, *k, y0 + y, x, frame.width() - 2.0 * EMBED_PAD, l);
        if pc.sel.is_some() {
            n += select::seg_count(l); // only needed while something is selected
        }
    }
    if let Some(m) = &e.more {
        let sel = pc.sel.take();
        paint_tb(ui, pc, pos2(x, y0 + e.more_y), m, id.with("more"), 0);
        pc.sel = sel;
    }
    pc.seg_off = base;
}

impl MarkdownPreview {
    pub fn in_flight_decodes(&self) -> usize {
        self.core.images.in_flight()
    }

    pub fn in_flight_parses(&self) -> usize {
        self.worker.as_ref().map_or(0, ParseWorker::in_flight)
    }

    #[doc(hidden)]
    pub fn set_parse_paused(&mut self, paused: bool) {
        if self.worker.is_none() && paused {
            self.worker = ParseWorker::start();
        }
        if let Some(w) = &self.worker {
            w.set_paused(paused);
        }
    }

    /// Block tops of a document's last layout (tests compare them with a fresh layout).
    /// Images decoded or decoding (tests).
    pub fn images_started(&self) -> usize {
        self.core.images.started()
    }

    pub fn tops_of(&self, path: &Path) -> Option<&[f32]> {
        self.docs.get(path).map(|d| d.tops.as_slice())
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
        let Some(tab) = state.tabs.tabs.get(tab_index) else { return };
        let (path, tab_id, split) = (tab.path.clone(), tab.id, tab.view == iron_sheets_core::config::ViewMode::Split);
        let mut scroll = std::mem::take(&mut state.tabs.tabs[tab_index].preview_scroll);
        let hover = self.show_doc(ui, state, &path, tab_id, None, &mut scroll, split && state.config.sync_scroll, None);
        if let Some(t) = state.tabs.tabs.get_mut(tab_index) {
            t.preview_scroll = scroll;
        }
        if let Some(h) = hover {
            state.ui.page_popup.offer(h);
        }
    }

    /// Draws the note at `path` into `ui`. `text` replaces the open document's text and version (the page preview
    /// popup shows notes that are not open); `popup` is the popup mode: read-only and
    /// the heading to scroll to once. Returns the internal link the pointer is on, for the page preview.
    #[allow(clippy::too_many_arguments)]
    pub fn show_doc(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        path: &Path,
        tab_id: u64,
        text: Option<(&str, u64)>,
        scroll: &mut PaneScroll,
        sync: bool,
        popup: Option<&mut Option<String>>,
    ) -> Option<popup::LinkHover> {
        let ctx = ui.ctx().clone();
        let path = path.to_path_buf();
        let Some((text, version)) = text.or_else(|| state.docs.get(&path).map(|d| (d.text.as_str(), d.version))) else {
            ui.label("Document not loaded");
            return None;
        };
        let mut hover_link = None;
        self.frame += 1;
        self.frames_shown += 1;
        self.laid_this_frame = 0;
        self.hits.clear();
        self.block_rects.clear();
        let ppp = ctx.pixels_per_point();
        if (ppp - self.last_ppp).abs() > 1e-4 {
            self.last_ppp = ppp;
            self.epoch += 1;
        }
        if self.core.nbsp_ratio == 0.0 {
            let fam = crate::theme::families().preview;
            let r = ctx.fonts_mut(|f| f.glyph_width(&egui::FontId::new(100.0, fam), NBSP)) / 100.0;
            self.core.nbsp_ratio = if r > 0.01 { r } else { 0.25 };
        }
        self.core.images.poll(&ctx);
        self.core.mermaid = cfg!(feature = "mermaid") && state.config.render_mermaid;
        #[cfg(feature = "codehl")]
        let hl_on = state.modules.enabled(iron_sheets_app_core::modules::CODEHL);
        #[cfg(not(feature = "codehl"))]
        let hl_on = false;
        if hl_on != self.core.hl.is_some() {
            self.core.hl = hl_on.then(|| codehl::SyntectHighlighter::shared(&state.waker));
        }

        let mut dv = self.docs.remove(&path).unwrap_or_default();
        dv.used = self.frame;
        dv.hold = state.config.mermaid_render == "save" && state.docs.get(&path).is_some_and(|d| d.is_dirty());
        let prefetched = if dv.doc.is_none() || dv.parsed_version != version { state.docs.prefetch.parsed(&path, text) } else { None };
        let doc_changed =
            update_parse(&mut self.worker, &mut self.docs, &path, &mut dv, text, version, &ctx, &mut state.perf, &mut self.parse_count, prefetched);
        let Some(docp) = dv.doc.clone() else {
            loading_skeleton(ui, state.config.animations);
            self.docs.insert(path, dv);
            return None;
        };
        #[cfg(feature = "mermaid")]
        if doc_changed && state.config.render_mermaid && docp.blocks.iter().any(|b| matches!(&b.kind, BlockKind::Code { lang, .. } if lang.eq_ignore_ascii_case("mermaid"))) {
            svgfonts::warm_fonts();
        }
        // The selection belongs to one parse: a newer one (an edit) clears it.
        if dv.sel.is_some() && dv.sel_version != dv.parsed_version {
            dv.sel = None;
        }
        let style = &state.ui.theme.preview;
        let size = style.body_size;
        let avail = ui.available_width() - ui.spacing().scroll.allocated_width();
        let max_w = if state.config.preview_full_width { f32::MAX } else { MAX_W };
        let col_w = (avail - 2.0 * MARGIN).clamp(120.0, max_w).floor();
        let left = MARGIN + ((avail - 2.0 * MARGIN - col_w) / 2.0).max(0.0);
        let state_math_auto = state.config.math_in_markdown && state.config.math_syntax == iron_sheets_core::config::MathSyntax::Auto;
        let geom = Geom {
            width: col_w as u32,
            theme: state.ui.theme.generation
                ^ ((!state.config.math_in_markdown as u64) << 48)
                ^ (state.config.math_syntax as u64) << 49
                ^ ((state.config.render_mermaid as u64) << 52)
                ^ ((hl_on as u64) << 53),
            epoch: self.epoch,
            dir: dir_hash(&path),
        };
        if doc_changed || dv.heights.len() != docp.blocks.len() {
            let same = dv.geom == geom;
            dv.geom = geom;
            rebuild_heights(&mut self.core, &mut dv, &docp, size, col_w, false, same);
        } else if dv.geom != geom {
            dv.geom = geom;
            rebuild_heights(&mut self.core, &mut dv, &docp, size, col_w, true, false);
        }
        let index = state.indexer.index();
        {
            let e = &mut self.core.embeds;
            e.mode = embed::Mode::from_setting(&state.config.embed_mode);
            e.max_depth = state.config.embed_max_depth;
            e.host = path_hash(&path);
            // Open targets are compared by version, files on disk after the index changed (the watcher updates it).
            if let Some(g) = index.try_read().map(|ix| ix.generation()) {
                if e.refresh(&state.docs, g) {
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
            }
        }
        let math: &dyn crate::services::MathRenderer = if state.config.math_in_markdown { &*state.math } else { &crate::services::NoMath };
        let n = docp.blocks.len();

        // Visible range from last frame's offset (or the forced one).
        let off = scroll.force.unwrap_or(scroll.offset);
        let vh = if scroll.viewport_h > 0.0 { scroll.viewport_h } else { ui.available_height() };
        let had_force = scroll.force.is_some();
        // A jump can land on blocks the idle pass laid since the last rebuild: their tops must be current before they
        // are placed, not up to 7 frames later.
        if dv.tops_dirty && (had_force || (off - dv.last_off).abs() > vh) {
            recompute_tops(&mut dv, &docp, size);
        }
        dv.last_off = off;
        let r = find_range(&dv, off - 100.0, off + vh + 100.0);
        let anchor = r.start;
        let anchor_top = dv.tops.get(anchor).copied().unwrap_or(0.0);
        let t0 = Instant::now();
        let mut laid = 0usize;
        for i in r.clone() {
            get_layout(&mut self.core, &mut dv, &docp, i, &ctx, style, math, &index, &state.docs, &path, ppp, self.frame, true, &mut laid);
        }
        // Idle work: exact heights for the rest, nearest the viewport first.
        let scrolling = ctx.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO);
        let mut remaining = 0usize;
        if r.end <= n {
            let order = (r.end..n).chain(if scrolling { 0..0 } else { 0..r.start }.rev());
            let mut over = false;
            for i in order {
                if dv.exact[i] {
                    continue;
                }
                if over || t0.elapsed() > IDLE_BUDGET {
                    over = true;
                    remaining += 1;
                    continue;
                }
                get_layout(&mut self.core, &mut dv, &docp, i, &ctx, style, math, &index, &state.docs, &path, ppp, self.frame, false, &mut laid);
                // Blocks about to scroll into view must not be drawn at stale positions.
                if dv.tops_dirty && i < r.end + NEAR {
                    dv.urgent = true;
                }
            }
            if scrolling {
                remaining += dv.exact[..r.start].iter().filter(|e| !**e).count();
            }
        }
        if laid > 0 {
            state.perf.record("md.layout", t0.elapsed());
        }
        self.laid_this_frame = laid;
        self.laid_total += laid;
        self.unlaid = remaining;
        if remaining > 0 {
            ctx.request_repaint();
        }
        // Idle layout changes heights far from the viewport on most frames; rebuilding the O(blocks) tops and scroll
        // map each time doubled its cost, so do it when the visible range changed, at the end, or every 8th frame.
        if dv.tops_dirty && (dv.urgent || remaining == 0 || self.frame.is_multiple_of(8)) {
            recompute_tops(&mut dv, &docp, size);
        }
        let mut anchor_force = None;
        if !had_force && off > 1.0 {
            let d = dv.tops.get(anchor).copied().unwrap_or(0.0) - anchor_top;
            if d.abs() > 0.5 {
                anchor_force = Some(off + d);
            }
        }
        if self.core.lcache.len() > 600 {
            let f = self.frame;
            self.core.lcache.retain(|_, e| e.used + 30 >= f);
            if self.core.lcache.len() > 600 {
                self.core.lcache.clear();
            }
        }

        // Paint.
        let tab = &mut *scroll;
        let follow = if sync { tab.follow } else { None };
        let mut map_rebuilt = false;
        if dv.map_dirty {
            let mut pts = Vec::with_capacity(n * 2 + 2);
            // A block much taller than its source lines (an image, a display formula, a wrapped paragraph) would make
            // the editor race through the preview. It borrows part of the blank lines around it: its top and bottom map
            // a little outside its own lines, which spreads the same pixels over more source lines.
            let line_px = size * 1.5;
            let nb = docp.blocks.len();
            for (i, b) in docp.blocks.iter().enumerate() {
                let (s0, e0) = (b.lines.start as f32, b.lines.end as f32);
                let h = dv.heights[i];
                let (mut s1, mut e1) = (s0, e0);
                if h > 3.0 * line_px * (e0 - s0).max(1.0) {
                    let prev_end = if i > 0 { docp.blocks[i - 1].lines.end as f32 } else { s0 };
                    let next_start = if i + 1 < nb { docp.blocks[i + 1].lines.start as f32 } else { docp.line_count as f32 };
                    s1 = s0 - BORROW * (s0 - prev_end).max(0.0);
                    e1 = e0 + BORROW * (next_start - e0).max(0.0);
                }
                pts.push((s1, dv.tops[i]));
                pts.push((e1, dv.tops[i] + h));
            }
            pts.push((docp.line_count as f32, dv.total));
            tab.map = ScrollMap::from_points(pts);
            dv.map_dirty = false;
            map_rebuilt = true;
        }
        match follow {
            // Driven by the editor: block heights changed (estimates replaced by real layouts), so re-derive the
            // offset from the source line instead of keeping the old pixel offset. Not while the user scrolls here.
            Some(f) => {
                if map_rebuilt && (tab.force.is_some() || !scrolling) {
                    let max = (dv.total - vh).max(0.0);
                    let target = tab.follow_offset(f, max);
                    if (target - off).abs() > 0.5 || tab.force.is_some() {
                        tab.force = Some(target);
                    }
                }
            }
            None => {
                if let Some(f) = anchor_force {
                    tab.force = Some(f);
                }
            }
        }
        let mut cmds: Vec<Command> = Vec::new();
        let mut scroll_to: Option<f32> = None;
        let mut alt_line: Option<u32> = None;
        let mut pending_visible = false;
        let text_ok = popup.is_none() && dv.parsed_version == version;
        let total = dv.total;
        let Core { lcache, textures, images, embeds, copied, .. } = &mut self.core;
        let hits = &mut self.hits;
        let block_rects = &mut self.block_rects;
        let mut sel = dv.sel;
        let mut ctx_line = self.ctx_line;
        let auto_prev = std::mem::take(&mut self.autoscroll);
        let mut auto_now = false;
        let mut sel_active = self.sel_active;
        let sel_color = ui.visuals().selection.bg_fill;
        let tops = &dv.tops;
        let frame = self.frame;
        let mut missing = false;
        let mut painted = 0usize;
        let mut toggled = None;
        let mut opened = None;
        let mut open_diagram = None;
        let diagram_open = self.diagram_full.is_some();
        let hold = dv.hold;
        let pane_rect = ui.available_rect_before_wrap();
        scroll_pane(ui, (tab_id, "md_preview"), tab, |ui| {
            ui.set_height(total);
            let origin = ui.max_rect().min;
            let clip = ui.clip_rect();
            let (y0, y1) = (clip.min.y - origin.y, clip.max.y - origin.y);
            let a = tops.partition_point(|t| *t < y0).saturating_sub(1);
            let b = tops.partition_point(|t| *t <= y1).max(a);
            let bg = ui.interact(clip, ui.scope_id().with("md-select"), Sense::click_and_drag());
            let geo = select::Geo { doc: &docp, tops, origin, x0: origin.x + left };
            let mut layout_of = |i: usize| {
                let e = lcache.get_mut(&block_key(embeds, hold, geom, &docp, i))?;
                e.used = frame;
                Some(e.layout.clone())
            };
            let out = select::handle(ui, &bg, &mut sel, &geo, &mut layout_of, off);
            if out.line.is_some() {
                alt_line = out.line;
            }
            scroll_to = out.scroll;
            auto_now = out.scroll.is_some();
            if out.ctx_line.is_some() {
                ctx_line = out.ctx_line;
            }
            if ui.input(|i| i.pointer.primary_pressed()) {
                sel_active = bg.contains_pointer();
            }
            let copy = sel_active
                && !diagram_open
                && sel.is_some_and(|s| s.anchor != s.cursor)
                && ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
                && ui.memory(|m| m.focused().is_none());
            let mut pc = Pc {
                style,
                doc: &docp,
                text,
                text_ok,
                path: &path,
                tab_id,
                tops,
                textures,
                images,
                cmds: &mut cmds,
                hits,
                scroll_to: &mut scroll_to,
                painted: 0,
                math_tip: state_math_auto,
                toggle: None,
                sel: sel.filter(|s| s.anchor != s.cursor).map(|s| s.ordered()),
                sel_color,
                cur: 0,
                seg_off: 0,
                embed: None,
                view_image: None,
                open_diagram: None,
                copied,
                hover: &mut hover_link,
            };
            for (i, &t) in tops.iter().enumerate().take(b).skip(a) {
                let k = block_key(embeds, hold, geom, &docp, i);
                let l = match lcache.get_mut(&k) {
                    Some(e) => {
                        e.used = frame;
                        e.layout.clone()
                    }
                    None => {
                        missing = true;
                        continue; // laid out next frame (range moved this frame)
                    }
                };
                if l.pending {
                    pending_visible = true;
                }
                let (top, x0) = (origin.y + t, origin.x + left);
                block_rects.push((i, Rect::from_min_size(pos2(x0, top), vec2(col_w, l.height))));
                pc.cur = i;
                paint_block(ui, &mut pc, &docp, i, top, x0, col_w, &l);
            }
            painted = pc.painted;
            toggled = pc.toggle;
            opened = pc.view_image.take();
            open_diagram = pc.open_diagram.take();
            let has_sel = sel.is_some_and(|s| s.anchor != s.cursor);
            let mut menu_copy = false;
            crate::anim::context_menu(&bg, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                menu_copy = ui.add_enabled(has_sel, egui::Button::new("Copy")).clicked();
                if ui.button("Show in source").clicked() {
                    alt_line = alt_line.or(ctx_line);
                }
            });
            if copy {
                // Ours: the editor must not copy its own selection as well.
                ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy)));
            }
            if let Some(s) = sel.filter(|_| copy || menu_copy) {
                ctx.copy_text(select::selected_text(&docp, &s, |i| lcache.get(&block_key(embeds, hold, geom, &docp, i)).map(|e| e.layout.clone())));
            }
        });
        if popup.is_none() {
            if let Some((_, since)) = dv.submitted {
                let elapsed = since.elapsed();
                if elapsed < STALE_CUE_AFTER {
                    ctx.request_repaint_after(STALE_CUE_AFTER - elapsed);
                } else {
                    let badge_rect = Rect::from_min_max(
                        pos2((pane_rect.right() - 160.0).max(pane_rect.left() + 8.0), pane_rect.top() + 8.0),
                        pos2((pane_rect.right() - 10.0).max(pane_rect.left() + 80.0), pane_rect.top() + 36.0),
                    );
                    let mut overlay = ui.new_child(
                        UiBuilder::new()
                            .max_rect(badge_rect)
                            .layout(egui::Layout::right_to_left(egui::Align::Min)),
                    );
                    egui::Frame::popup(overlay.style()).show(&mut overlay, |ui| {
                        ui.weak("Updating preview…");
                    });
                }
            }
        }
        if dv.sel != sel {
            dv.sel = sel;
            dv.sel_version = dv.parsed_version;
        }
        self.ctx_line = ctx_line;
        self.sel_active = sel_active;
        self.autoscroll = auto_now;
        if auto_prev {
            scroll.user_scrolled = true;
        }
        if let Some(l) = alt_line {
            cmds.push(Command::ScrollToLine(l));
        }
        if let Some((p, window)) = opened {
            let w = Some(ImageWin { path: p, note: path.clone(), view: Default::default(), geom: Default::default(), focus: true });
            if window {
                self.image_win = w;
            } else {
                self.image_full = w;
            }
        }
        if let Some((slot, window)) = open_diagram {
            let view = diagram::DiagramView::new(slot, path.clone());
            if window {
                self.diagram_win = Some(view);
            } else {
                self.diagram_full = Some(view);
            }
        }
        if self.diagram_full.as_ref().is_some_and(|d| d.note != path) {
            self.diagram_full = None;
        }
        if let Some(d) = &mut self.diagram_full {
            if d.show_fullscreen(&ctx, &mut self.core.images, &mut cmds) {
                self.diagram_full = None;
            }
        }
        // shortcut: the window is drawn only while its note's preview is, so it vanishes while another tab is shown.
        if let Some(d) = self.diagram_win.as_mut().filter(|d| d.note == path) {
            if d.show_window(&ctx, &mut self.core.images, &mut cmds) {
                self.diagram_win = None;
            }
        }
        // Zoomed rasters live as long as their viewer.
        for (k, open) in [self.diagram_full.is_some(), self.diagram_win.is_some()].into_iter().enumerate() {
            if !open {
                self.core.images.release_zoom(k);
            }
        }
        if self.image_full.as_ref().is_some_and(|w| w.note != path) {
            self.image_full = None;
        }
        let ppp = ctx.pixels_per_point();
        let images = &mut self.core.images;
        if let Some(w) = &mut self.image_full {
            let size = image_size(images, &w.path, ppp);
            if crate::viewer::show_with(&ctx, Id::NULL.with("md-image-viewer"), &mut w.view, size, 1.0, &|_| true, |ui, r, _, _| {
                paint_image(ui, images, &w.path, r)
            }) {
                self.image_full = None;
            }
        }
        // shortcut: drawn only while its note's preview is, like the diagram window.
        if let Some(w) = self.image_win.as_mut().filter(|w| w.note == path) {
            let spec = crate::aux_window::AuxSpec { id: "image-viewer", title: "Image", default_size: vec2(900.0, 650.0), min_size: vec2(320.0, 240.0) };
            let size = image_size(images, &w.path, ppp);
            let focus = std::mem::take(&mut w.focus);
            if crate::viewer::window(&ctx, &spec, &mut w.geom, focus, &mut w.view, size, 1.0, &|_| true, |ui, r, _| paint_image(ui, images, &w.path, r)) {
                self.image_win = None;
            }
        }
        if let Some(h) = toggled {
            if !self.core.embeds.expanded.remove(&h) {
                self.core.embeds.expanded.insert(h);
            }
            ctx.request_repaint();
        }
        self.textures_painted = painted;
        if missing {
            ctx.request_repaint();
        }
        if pending_visible {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if let Some(y) = scroll_to {
            scroll.force = Some(y);
            ctx.request_repaint();
        }
        state.commands.extend(cmds);
        if let Some(slot) = popup {
            // The heading's top is only exact once every block above it is laid out: keep the jump until then.
            if let Some(i) = slot.as_deref().and_then(|h| docp.find_heading(h)) {
                if scroll.user_scrolled {
                    *slot = None;
                } else {
                    scroll.force = Some((dv.tops[i] - 8.0).max(0.0));
                    ctx.request_repaint();
                    if dv.exact[..=i].iter().all(|e| *e) && !dv.tops_dirty {
                        *slot = None;
                    }
                }
            } else {
                *slot = None;
            }
        }
        // Images up to one viewport above and below the screen start decoding now (after the visible ones, which the
        // paint above queued first), so a normal scroll finds them ready. Further ones wait until they are painted.
        {
            let pr = find_range(&dv, off - vh, off + 2.0 * vh);
            let t1 = Instant::now();
            for i in (r.end..pr.end).chain((pr.start..r.start).rev()) {
                if t1.elapsed() > IDLE_BUDGET {
                    ctx.request_repaint();
                    break;
                }
                let mut none = 0;
                let l = get_layout(&mut self.core, &mut dv, &docp, i, &ctx, style, math, &index, &state.docs, &path, ppp, self.frame, true, &mut none);
                let imgs = &mut self.core.images;
                match &l.body {
                    Body::Image { path, .. } => imgs.want(path),
                    #[cfg(feature = "excalidraw")]
                    Body::Excalidraw(e) => imgs.want(&e.path),
                    Body::Text { tb, .. } => {
                        for s in &tb.slots {
                            match &s.content {
                                SlotContent::Image(p) => imgs.want(p),
                                #[cfg(feature = "excalidraw")]
                                SlotContent::Excalidraw { tex_key: p, .. } => imgs.want(p),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.docs.insert(path, dv);
        if self.docs.len() > MAX_DOCS {
            if let Some(old) = self.docs.iter().min_by_key(|(_, d)| d.used).map(|(p, _)| p.clone()) {
                self.docs.remove(&old);
            }
        }
        hover_link
    }
}

#[cfg(test)]
mod tests {
    use super::common_ends;

    #[test]
    fn common_ends_never_overlap() {
        assert_eq!(common_ends(&[1, 2, 3, 4], &[1, 2, 9, 3, 4]), (2, 2));
        assert_eq!(common_ends(&[1, 2, 3], &[1, 2, 3]), (3, 0));
        assert_eq!(common_ends(&[1, 1], &[1, 1, 1]), (2, 0));
        assert_eq!(common_ends(&[], &[5]), (0, 0));
        assert_eq!(common_ends(&[7, 8], &[]), (0, 0));
    }
}
