//! Background image decoding for the Markdown preview: dimensions first (so layout does not jump),
//! then the downscaled pixels. Textures are kept under a byte cap.
use super::mermaid;
#[cfg(feature = "excalidraw")]
use crate::preview::excalidraw::{self, RenderError};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
#[cfg(feature = "excalidraw")]
use iron_sheets_core::index::Index;
#[cfg(feature = "excalidraw")]
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_SIDE: u32 = 2048;
const TEXTURE_CAP_BYTES: usize = 192 * 1024 * 1024;
const MAX_JOBS: usize = 3;
/// Texture uploads per frame stop once this many bytes are in (at least one image always goes): a 2048 px image is
/// 16 MB, and uploading a dozen in one frame is a visible hitch.
const UPLOAD_BYTES_PER_FRAME: usize = 8 * 1024 * 1024;
/// A changed diagram renders this long after its last edit.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(300);
/// A render that failed for load reasons is tried again after this.
const RETRY: Duration = Duration::from_secs(1);
/// A zoomed raster for the viewers starts this long after the zoom stopped changing its size class.
const ZOOM_DEBOUNCE: Duration = Duration::from_millis(250);

#[cfg(feature = "excalidraw")]
#[derive(Clone)]
pub struct ExcalidrawEmbedJob {
    pub path: PathBuf,
    pub source: Option<String>,
    pub dark: bool,
    pub w_pts: f32,
    pub ppp: f32,
    pub px_w: Option<f32>,
    pub index: Arc<RwLock<Index>>,
}

#[derive(Clone)]
enum RenderJob {
    Mermaid(mermaid::Job),
    #[cfg(feature = "excalidraw")]
    Excalidraw(ExcalidrawEmbedJob),
}

enum Msg {
    Dims(PathBuf, [u32; 2]),
    Done(PathBuf, ColorImage, Option<[u32; 2]>, Option<Arc<mermaid::DiagramText>>),
    Failed(PathBuf, String),
    /// Failed for load reasons (not the content): try again later.
    Retry(PathBuf),
}

enum Entry {
    /// Only the size is known (header read, or the texture was evicted): no decode until it is painted.
    Sized([u32; 2]),
    /// Job queued or running; dimensions known once read.
    Loading {
        dims: Option<[u32; 2]>,
    },
    Ready {
        dims: [u32; 2],
        tex: TextureHandle,
        bytes: usize,
        used: u64,
        /// Selectable text and links (diagrams).
        text: Option<Arc<mermaid::DiagramText>>,
    },
    Failed(String),
}

/// The diagram of a (note, line) slot: the current key and the last key that rendered (still shown while the
/// current one renders or fails).
struct Slot {
    key: PathBuf,
    good: Option<PathBuf>,
}

/// What to paint for a diagram block.
#[derive(Debug)]
pub struct Mer {
    /// The cache key whose texture to paint (the last good render while the current source has not rendered).
    pub path: PathBuf,
    pub info: ImgInfo,
    /// The current source failed (and `path` is the last good render).
    pub err: Option<String>,
    /// `path` is not rendered yet or the current source still is: layout again when it is.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImgInfo {
    /// Dimensions not known yet.
    Unknown,
    /// Original pixel size.
    Known([u32; 2]),
    Failed(String),
}

type UploadItem = (PathBuf, ColorImage, Option<[u32; 2]>, Option<Arc<mermaid::DiagramText>>);

pub struct ImageCache {
    entries: FxHashMap<PathBuf, Entry>,
    /// Waiting for a job, oldest first: images in the order they were painted without a texture (top of the screen first).
    queue: VecDeque<PathBuf>,
    /// Decoded, waiting for a texture upload (the per-frame cap).
    uploads: VecDeque<UploadItem>,
    /// Mermaid diagrams & Excalidraw embeds: the cache key stands in for a path and the job says how to render it.
    jobs: FxHashMap<PathBuf, RenderJob>,
    /// Jobs that start at the given time (debounce, retry).
    delayed: Vec<(Instant, PathBuf)>,
    /// Size of every diagram rendered so far, by cache key: a diagram that comes back (note reopened, scrolled far
    /// away and released) reserves its real size before it renders again.
    sizes: FxHashMap<PathBuf, [u32; 2]>,
    /// The diagram key of each (note, first line) slot: a new key for a slot replaces the old one.
    slots: FxHashMap<u64, Slot>,
    /// The zoomed raster of each viewer (0 fullscreen, 1 window), at most one each.
    zoom: [Option<PathBuf>; 2],
    /// `tick` when the previous poll started: what was painted after it is on screen and not evicted.
    mark: u64,
    running: usize,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    bytes: usize,
    tick: u64,
}

impl Default for ImageCache {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        ImageCache {
            entries: Default::default(),
            queue: VecDeque::new(),
            uploads: VecDeque::new(),
            jobs: Default::default(),
            delayed: Vec::new(),
            slots: Default::default(),
            sizes: Default::default(),
            zoom: [None, None],
            mark: 0,
            running: 0,
            tx,
            rx,
            bytes: 0,
            tick: 0,
        }
    }
}

impl ImageCache {
    pub fn in_flight(&self) -> usize {
        self.running + self.queue.len() + self.uploads.len()
    }

    /// Drain finished jobs and start queued ones. Returns true when something changed.
    pub fn poll(&mut self, ctx: &Context) -> bool {
        let mut changed = false;
        while let Ok(m) = self.rx.try_recv() {
            changed = true;
            match m {
                Msg::Dims(p, d) => {
                    if let Some(Entry::Loading { dims }) = self.entries.get_mut(&p) {
                        *dims = Some(d);
                    }
                }
                Msg::Done(p, img, dims_pts, text) => {
                    self.running = self.running.saturating_sub(1);
                    self.uploads.push_back((p, img, dims_pts, text));
                }
                Msg::Failed(p, e) => {
                    self.running = self.running.saturating_sub(1);
                    if self.entries.contains_key(&p) {
                        self.entries.insert(p, Entry::Failed(e));
                    }
                }
                Msg::Retry(p) => {
                    self.running = self.running.saturating_sub(1);
                    if self.entries.contains_key(&p) {
                        self.delayed.push((Instant::now() + RETRY, p));
                    }
                }
            }
        }
        let mut uploaded = 0;
        while uploaded < UPLOAD_BYTES_PER_FRAME {
            let Some((p, img, dims_pts, text)) = self.uploads.pop_front() else { break };
            // The diagram was replaced while it rendered.
            if !self.entries.contains_key(&p) {
                continue;
            }
            changed = true;
            // A diagram's size is its SVG size, never its pixels (a second render of a key whose entry is
            // already Ready would otherwise take the raster size, `ppp` times too big, and scale the text overlay).
            let dims = match (dims_pts, &text, self.entries.get(&p)) {
                (Some(d), _, _) => d,
                (_, Some(t), _) => [t.size[0].ceil() as u32, t.size[1].ceil() as u32],
                (_, _, Some(Entry::Loading { dims: Some(d) })) => *d,
                _ => [img.size[0] as u32, img.size[1] as u32],
            };
            if let Some(Entry::Ready { bytes, .. }) = self.entries.get(&p) {
                self.bytes = self.bytes.saturating_sub(*bytes);
            }
            let bytes = img.size[0] * img.size[1] * 4;
            uploaded += bytes;
            let tex = ctx.load_texture(format!("md-img:{}", p.display()), img, TextureOptions::LINEAR);
            self.bytes += bytes;
            if dims_pts.is_some() || text.is_some() {
                if self.sizes.len() > 4096 {
                    self.sizes.clear();
                }
                self.sizes.insert(p.clone(), dims);
            }
            self.entries.insert(p, Entry::Ready { dims, tex, bytes, used: self.tick, text });
        }
        if !self.uploads.is_empty() {
            ctx.request_repaint();
        }
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.delayed).into_iter().partition(|(t, _)| *t <= now);
        self.delayed = later;
        self.queue.extend(due.into_iter().map(|(_, p)| p));
        let shown = std::mem::replace(&mut self.mark, self.tick);
        self.evict(shown);
        // Finished decodes still waiting for their upload count too, so the backlog stays bounded.
        while self.running + self.uploads.len() < MAX_JOBS {
            let Some(p) = self.queue.pop_front() else { break };
            self.running += 1;
            let (tx, ctx2) = (self.tx.clone(), ctx.clone());
            let job = self.jobs.get(&p).cloned();
            let key = p.clone();
            let spawned = std::thread::Builder::new().name("iron-sheets-md-img".into()).spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &job {
                    Some(RenderJob::Mermaid(job)) => render_mermaid(&p, job, &tx, &ctx2),
                    #[cfg(feature = "excalidraw")]
                    Some(RenderJob::Excalidraw(job)) => render_excalidraw(&p, job, &tx, &ctx2),
                    None => decode(&p, &tx, &ctx2),
                }));
                if r.is_err() {
                    let _ = tx.send(Msg::Failed(p, "the worker crashed".into()));
                }
                ctx2.request_repaint();
            });
            if spawned.is_err() {
                self.running -= 1;
                self.delayed.push((Instant::now() + RETRY, key));
            }
        }
        changed
    }

    /// Dimensions (and failure) of `path`; starts loading on first use.
    pub fn info(&mut self, path: &Path) -> ImgInfo {
        match self.entries.get(path) {
            Some(Entry::Sized(d)) | Some(Entry::Loading { dims: Some(d) }) => ImgInfo::Known(*d),
            Some(Entry::Loading { dims: None }) => ImgInfo::Unknown,
            Some(Entry::Ready { dims, .. }) => ImgInfo::Known(*dims),
            Some(Entry::Failed(e)) => ImgInfo::Failed(e.clone()),
            None => {
                let p = path.to_path_buf();
                // The size comes from the header (a few hundred bytes), so layout is final before any decode. The
                // decode starts when the image is painted (`texture`), never from here: a note with hundreds of photos
                // must not decode them all while idle.
                // shortcut: the read is on the UI thread, so a network path or a special file skips it (the decode
                // worker reports the size then, 80 px placeholder meanwhile); a slow disk still stalls one layout.
                if !header_read_is_cheap(path) {
                    self.entries.insert(p.clone(), Entry::Loading { dims: None });
                    self.queue.push_back(p);
                    return ImgInfo::Unknown;
                }
                match image::image_dimensions(path) {
                    Ok((w, h)) => {
                        self.entries.insert(p, Entry::Sized([w, h]));
                        ImgInfo::Known([w, h])
                    }
                    Err(e) => {
                        let e = e.to_string();
                        self.entries.insert(p, Entry::Failed(e.clone()));
                        ImgInfo::Failed(e)
                    }
                }
            }
        }
    }

    fn drop_entry(&mut self, k: &Path) {
        self.queue.retain(|q| q != k);
        self.delayed.retain(|(_, q)| q != k);
        self.jobs.remove(k);
        if let Some(Entry::Ready { bytes, .. }) = self.entries.remove(k) {
            self.bytes = self.bytes.saturating_sub(bytes);
        }
    }

    /// `drop_entry` unless another slot still shows `k` (two identical diagrams share one key).
    fn release(&mut self, k: &Path) {
        if !self.slots.values().any(|s| s.key == k || s.good.as_deref() == Some(k)) {
            self.drop_entry(k);
        }
    }

    fn ready(&self, k: &Path) -> Option<[u32; 2]> {
        match self.entries.get(k) {
            Some(Entry::Ready { dims, .. }) => Some(*dims),
            _ => None,
        }
    }

    /// Like `info` for a Mermaid diagram; `key` identifies (source, theme, width) and is never a real path. `slot`
    /// identifies the block. A new key for a known slot (the source was edited) waits for the typing to stop; the
    /// last good render stays shown meanwhile and when the new source fails. `hold` (setting "On save", note not
    /// saved): an edited block keeps its last render and nothing new starts.
    pub fn mermaid(&mut self, key: &Path, slot: u64, hold: bool, job: impl FnOnce() -> mermaid::Job) -> Mer {
        self.slot_render(key, slot, hold, || RenderJob::Mermaid(job()))
    }

    #[cfg(feature = "excalidraw")]
    pub fn excalidraw(&mut self, key: &Path, slot: u64, hold: bool, job: impl FnOnce() -> ExcalidrawEmbedJob) -> Mer {
        self.slot_render(key, slot, hold, || RenderJob::Excalidraw(job()))
    }

    fn slot_render(&mut self, key: &Path, slot: u64, hold: bool, job: impl FnOnce() -> RenderJob) -> Mer {
        let prev = self.slots.remove(&slot);
        let mut cur = Slot { key: key.to_path_buf(), good: None };
        let mut edited = false;
        if let Some(p) = prev {
            if p.key == key || (hold && !self.entries.contains_key(key) && self.ready(&p.key).is_some()) {
                cur = p;
            } else {
                edited = true;
                if self.ready(&p.key).is_some() {
                    if let Some(g) = p.good.filter(|g| *g != key) {
                        self.release(&g);
                    }
                    cur.good = Some(p.key);
                } else {
                    cur.good = p.good.filter(|g| self.ready(g).is_some());
                    self.release(&p.key);
                }
            }
        }
        if !self.entries.contains_key(&cur.key) {
            self.entries.insert(cur.key.clone(), Entry::Loading { dims: self.sizes.get(&cur.key).copied() });
            self.jobs.insert(cur.key.clone(), job());
            if edited {
                self.delayed.push((Instant::now() + DEBOUNCE, cur.key.clone()));
            } else {
                self.queue.push_back(cur.key.clone());
            }
        }
        let k = cur.key.clone();
        let info = self.info(&k);
        let good = cur.good.as_ref().and_then(|g| self.ready(g).map(|d| (g.clone(), d)));
        let ready = self.ready(&k).is_some();
        let stale = if ready { cur.good.take() } else { None };
        self.slots.insert(slot, cur);
        if let Some(g) = stale {
            self.release(&g);
        }
        match (good, info) {
            (Some((g, d)), ImgInfo::Failed(e)) => Mer { path: g, info: ImgInfo::Known(d), err: Some(e), pending: false },
            (Some((g, d)), _) if !ready => Mer { path: g, info: ImgInfo::Known(d), err: None, pending: true },
            (_, info) => {
                let failed = matches!(self.entries.get(&k), Some(Entry::Failed(_)));
                Mer { path: k, info, err: None, pending: !ready && !failed }
            }
        }
    }

    /// Selectable text and links of a rendered diagram.
    pub fn texts(&self, path: &Path) -> Option<Arc<mermaid::DiagramText>> {
        match self.entries.get(path) {
            Some(Entry::Ready { text, .. }) => text.clone(),
            _ => None,
        }
    }

    /// Where to look for the diagram of `slot` now: its current key, or the last good one while that renders or fails.
    pub fn slot_path(&self, slot: u64) -> Option<PathBuf> {
        let s = self.slots.get(&slot)?;
        if !self.entries.contains_key(&s.key) {
            return None;
        }
        Some(s.good.as_ref().filter(|g| self.ready(&s.key).is_none() && self.ready(g).is_some()).unwrap_or(&s.key).clone())
    }

    /// The key `slot` was last asked for (in "On save" hold, the held one).
    pub fn slot_key(&self, slot: u64) -> Option<&Path> {
        self.slots.get(&slot).map(|s| s.key.as_path())
    }

    /// Natural size of a diagram whose dimensions are known (never starts a job).
    pub fn dims(&self, path: &Path) -> Option<[u32; 2]> {
        match self.entries.get(path) {
            Some(Entry::Ready { dims, .. }) | Some(Entry::Sized(dims)) | Some(Entry::Loading { dims: Some(dims) }) => Some(*dims),
            _ => None,
        }
    }

    /// A raster of diagram `base` at least `need_px` wide for viewer `viewer` (0 fullscreen, 1 window), once rendered;
    /// `None` while it renders or when the base raster is sharp enough (the caller then scales the base texture).
    /// Each viewer keeps one; `release_zoom` frees it.
    pub fn zoom_texture(&mut self, base: &Path, need_px: f32, viewer: usize) -> Option<TextureHandle> {
        let base_w = match self.entries.get(base) {
            Some(Entry::Ready { tex, .. }) => tex.size()[0] as f32,
            _ => return None,
        };
        if need_px <= base_w * 1.05 {
            return None;
        }
        let bucket = (need_px.ceil() as u32).next_power_of_two().min(8192);
        let key = PathBuf::from(format!("{}@{bucket}@{viewer}", base.display()));
        if self.zoom[viewer].as_ref() != Some(&key) {
            self.release_zoom(viewer);
            let mut j = self.jobs.get(base).cloned()?;
            match &mut j {
                RenderJob::Mermaid(m) => m.px_w = Some(bucket as f32),
                #[cfg(feature = "excalidraw")]
                RenderJob::Excalidraw(e) => e.px_w = Some(bucket as f32),
            }
            self.zoom[viewer] = Some(key.clone());
            self.entries.insert(key.clone(), Entry::Loading { dims: None });
            self.jobs.insert(key.clone(), j);
            self.delayed.push((Instant::now() + ZOOM_DEBOUNCE, key.clone()));
        }
        self.tick += 1;
        match self.entries.get_mut(&key) {
            Some(Entry::Ready { tex, used, .. }) => {
                *used = self.tick;
                Some(tex.clone())
            }
            _ => None,
        }
    }

    /// Frees the zoomed raster (and a pending job for it) of a viewer that closed or switched diagram.
    pub fn release_zoom(&mut self, viewer: usize) {
        if let Some(k) = self.zoom[viewer].take() {
            self.drop_entry(&k);
        }
    }

    /// The texture when decoded.
    pub fn texture(&mut self, path: &Path) -> Option<TextureHandle> {
        self.tick += 1;
        match self.entries.get_mut(path) {
            Some(Entry::Ready { tex, used, .. }) => {
                *used = self.tick;
                Some(tex.clone())
            }
            // Painted without a texture: now it is worth a decode (queued in paint order).
            Some(Entry::Sized(_)) => {
                self.want(path);
                None
            }
            _ => None,
        }
    }

    /// Queues the decode of an image whose size is known but which has no texture (a no-op for anything else).
    pub fn want(&mut self, path: &Path) {
        if let Some(Entry::Sized(d)) = self.entries.get(path) {
            let d = *d;
            self.entries.insert(path.to_path_buf(), Entry::Loading { dims: Some(d) });
            self.queue.push_back(path.to_path_buf());
        }
    }

    /// Images decoded or being decoded (tests).
    pub fn started(&self) -> usize {
        self.entries.values().filter(|e| matches!(e, Entry::Loading { .. } | Entry::Ready { .. })).count()
    }

    /// Drop the least recently used textures while over the cap (they reload on demand).
    /// Textures painted since `shown` (the previous frame) stay: evicting what is on screen would render it again.
    fn evict(&mut self, shown: u64) {
        while self.bytes > TEXTURE_CAP_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(p, e)| if let Entry::Ready { used, .. } = e { Some((*used, p.clone())).filter(|(u, _)| *u <= shown) } else { None })
                .min();
            let Some((_, p)) = oldest else { break };
            // A zoom raster is not worth a render of its own: the viewer asks again if it still wants it.
            if let Some(z) = self.zoom.iter_mut().find(|z| z.as_ref() == Some(&p)) {
                *z = None;
                self.drop_entry(&p);
            } else if let Some(Entry::Ready { bytes, dims, .. }) = self.entries.insert(p.clone(), Entry::Loading { dims: None }) {
                // Decoded again only when it is painted again.
                self.bytes = self.bytes.saturating_sub(bytes);
                self.entries.insert(p, Entry::Sized(dims));
            }
        }
    }
}

/// Reading the header on the UI thread is cheap: no UNC/network path, no FIFO or device (a missing file fails at once).
fn header_read_is_cheap(p: &Path) -> bool {
    let s = p.to_string_lossy();
    !(s.starts_with("\\\\") || s.starts_with("//")) && !std::fs::metadata(p).is_ok_and(|m| !m.is_file())
}

fn decode(p: &Path, tx: &Sender<Msg>, ctx: &Context) {
    let fail = |e: String| {
        let _ = tx.send(Msg::Failed(p.to_path_buf(), e));
        ctx.request_repaint();
    };
    let img = match image::open(p) {
        Ok(i) => i,
        Err(e) => return fail(e.to_string()),
    };
    let img = if img.width().max(img.height()) > MAX_SIDE { img.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let _ = tx.send(Msg::Done(p.to_path_buf(), ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), None, None));
}

fn render_mermaid(key: &Path, job: &mermaid::Job, tx: &Sender<Msg>, ctx: &Context) {
    let k = key.to_path_buf();
    let _ = match mermaid::render(job) {
        Ok(r) => {
            let _ = tx.send(Msg::Dims(k.clone(), r.dims));
            tx.send(Msg::Done(k, r.image, Some(r.dims), Some(r.text)))
        }
        Err(f) if f.transient => tx.send(Msg::Retry(k)),
        Err(f) => tx.send(Msg::Failed(k, f.msg)),
    };
    ctx.request_repaint();
}

#[cfg(feature = "excalidraw")]
fn render_excalidraw(key: &Path, job: &ExcalidrawEmbedJob, tx: &Sender<Msg>, ctx: &Context) {
    let k = key.to_path_buf();
    let rjob = excalidraw::RasterJob {
        path: job.path.clone(),
        source: job.source.clone(),
        dark: job.dark,
        w_pts: job.w_pts,
        ppp: job.ppp,
        px_w: job.px_w,
        index: job.index.clone(),
    };
    let _ = match excalidraw::rasterise_job(&rjob) {
        Ok(r) => {
            let _ = tx.send(Msg::Dims(k.clone(), r.dims));
            tx.send(Msg::Done(k, r.image, Some(r.dims), None))
        }
        Err(RenderError::Empty) => tx.send(Msg::Failed(k, "Empty Excalidraw drawing".into())),
        Err(RenderError::Failed { transient: true, .. }) => tx.send(Msg::Retry(k)),
        Err(RenderError::Failed { message, .. }) => tx.send(Msg::Failed(k, message)),
    };
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(s: &str) -> mermaid::Job {
        mermaid::Job { source: s.into(), dark: true, w_pts: 100.0, ppp: 1.0, px_w: None }
    }

    fn ready(c: &mut ImageCache, k: &str) {
        let tex = Context::default().load_texture("t", ColorImage::example(), TextureOptions::LINEAR);
        c.entries.insert(PathBuf::from(k), Entry::Ready { dims: [40, 20], tex, bytes: 4, used: 0, text: None });
    }

    fn png(name: &str, w: u32, h: u32) -> PathBuf {
        let p = std::env::temp_dir().join(format!("iron-sheets-img-test-{}-{name}.png", std::process::id()));
        image::RgbaImage::new(w, h).save(&p).expect("png");
        p
    }

    #[test]
    fn looking_reads_the_header_only_and_painting_queues_the_decode() {
        let (a, missing) = (png("dims", 30, 20), std::env::temp_dir().join("iron-sheets-img-test-missing.png"));
        let mut c = ImageCache::default();
        assert_eq!(c.info(&a), ImgInfo::Known([30, 20]));
        assert!(c.queue.is_empty(), "layout alone never decodes");
        assert!(matches!(c.info(&missing), ImgInfo::Failed(_)) && c.queue.is_empty());
        // Painted without a texture: queued once, in paint order.
        let b = PathBuf::from("b");
        c.entries.insert(b.clone(), Entry::Sized([1, 1]));
        assert!(c.texture(&b).is_none() && c.texture(&a).is_none() && c.texture(&b).is_none());
        assert_eq!(c.queue, [b, a.clone()]);
        let _ = std::fs::remove_file(a);
    }

    #[test]
    fn evicted_textures_wait_until_painted_again() {
        let mut c = ImageCache::default();
        ready(&mut c, "x");
        c.bytes = TEXTURE_CAP_BYTES + 1;
        c.evict(u64::MAX);
        assert!(c.queue.is_empty() && matches!(c.entries.get(Path::new("x")), Some(Entry::Sized([40, 20]))));
    }

    #[test]
    fn network_paths_and_special_files_skip_the_header_read() {
        assert!(!header_read_is_cheap(Path::new("//server/share/a.png")) && !header_read_is_cheap(Path::new("\\\\server\\a.png")));
        assert!(!header_read_is_cheap(Path::new("/dev/null")));
        let mut c = ImageCache::default();
        assert_eq!(c.info(Path::new("//server/share/a.png")), ImgInfo::Unknown);
        assert_eq!(c.queue.len(), 1, "the decode worker delivers the size");
    }

    #[test]
    fn texture_uploads_per_frame_are_capped() {
        let ctx = Context::default();
        let mut c = ImageCache::default();
        for i in 0..3 {
            let k = PathBuf::from(format!("img{i}"));
            c.entries.insert(k.clone(), Entry::Loading { dims: Some([1600, 1600]) });
            let _ = c.tx.send(Msg::Done(k, ColorImage::new([1600, 1600], vec![egui::Color32::WHITE; 1600 * 1600]), None, None));
        }
        let ready = |c: &ImageCache| c.entries.values().filter(|e| matches!(e, Entry::Ready { .. })).count();
        c.poll(&ctx);
        assert_eq!((ready(&c), c.uploads.len()), (1, 2), "10 MB each: one per frame");
        c.poll(&ctx);
        c.poll(&ctx);
        assert_eq!((ready(&c), c.uploads.len()), (3, 0));
        // Small ones go together.
        for i in 3..8 {
            let k = PathBuf::from(format!("img{i}"));
            c.entries.insert(k.clone(), Entry::Loading { dims: Some([100, 100]) });
            let _ = c.tx.send(Msg::Done(k, ColorImage::new([100, 100], vec![egui::Color32::WHITE; 10_000]), None, None));
        }
        c.poll(&ctx);
        assert_eq!(ready(&c), 8);
    }

    #[test]
    fn a_diagram_that_comes_back_reserves_its_old_size() {
        let ctx = Context::default();
        let mut c = ImageCache::default();
        assert_eq!(c.mermaid(Path::new("mermaid:a"), 1, false, || job("a")).info, ImgInfo::Unknown);
        let text = Some(Arc::new(mermaid::DiagramText { size: [94.9, 163.2], labels: vec![] }));
        let _ = c.tx.send(Msg::Done("mermaid:a".into(), ColorImage::example(), None, text));
        c.poll(&ctx);
        c.drop_entry(Path::new("mermaid:a"));
        c.slots.clear();
        let m = c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        assert_eq!((m.info, m.pending), (ImgInfo::Known([95, 164]), true), "known size, still rendering");
    }

    #[test]
    fn editing_a_diagram_replaces_the_queued_one_and_debounces() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        assert_eq!(c.queue.len(), 1);
        c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert!(c.queue.is_empty() && c.delayed.len() == 1 && c.jobs.len() == 1);
        assert!(!c.entries.contains_key(Path::new("mermaid:a")));
        // Another block is not affected.
        c.mermaid(Path::new("mermaid:c"), 2, false, || job("c"));
        assert_eq!((c.queue.len(), c.jobs.len()), (1, 2));
    }

    #[test]
    fn last_good_render_stays_while_the_new_source_renders_and_after_it_fails() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        // Edited: the old picture is shown (pending), not a placeholder.
        let m = c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert_eq!((m.path.as_path(), m.pending, m.err.as_deref()), (Path::new("mermaid:a"), true, None));
        assert!(matches!(m.info, ImgInfo::Known([40, 20])));
        // The new source has a syntax error: still the old picture, with the message.
        c.entries.insert("mermaid:b".into(), Entry::Failed("bad".into()));
        let m = c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert_eq!((m.path.as_path(), m.pending, m.err.as_deref()), (Path::new("mermaid:a"), false, Some("bad")));
        // Edited again while failed: the old picture is kept through the next key too.
        let m = c.mermaid(Path::new("mermaid:c"), 1, false, || job("c"));
        assert_eq!((m.path.as_path(), m.pending), (Path::new("mermaid:a"), true));
        // The new source renders: the new picture, the old one is dropped.
        ready(&mut c, "mermaid:c");
        let m = c.mermaid(Path::new("mermaid:c"), 1, false, || job("c"));
        assert_eq!((m.path.as_path(), m.pending, m.err), (Path::new("mermaid:c"), false, None));
        assert!(!c.entries.contains_key(Path::new("mermaid:a")) && !c.entries.contains_key(Path::new("mermaid:b")));
    }

    #[test]
    fn without_a_good_render_a_failure_is_reported_as_before() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        c.entries.insert("mermaid:a".into(), Entry::Failed("bad".into()));
        let m = c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        assert!(matches!(m.info, ImgInfo::Failed(_)) && m.err.is_none() && !m.pending);
    }

    #[test]
    fn on_save_holds_the_render_until_released() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, true, || job("a"));
        ready(&mut c, "mermaid:a");
        c.queue.clear();
        // Unsaved edits: the old picture, nothing queued or delayed.
        let m = c.mermaid(Path::new("mermaid:b"), 1, true, || job("b"));
        assert_eq!((m.path.as_path(), m.pending), (Path::new("mermaid:a"), false));
        assert!(c.queue.is_empty() && c.delayed.is_empty() && !c.entries.contains_key(Path::new("mermaid:b")));
        // Saved: the render of the current source starts.
        let m = c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert_eq!((m.path.as_path(), m.pending), (Path::new("mermaid:a"), true));
        assert_eq!(c.delayed.len(), 1);
    }

    #[test]
    fn zoom_raster_is_requested_once_per_size_class() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        // The base texture (ColorImage::example, 64 px) is sharp enough for a small view.
        assert!(c.zoom_texture(Path::new("mermaid:a"), 60.0, 0).is_none() && c.delayed.is_empty());
        assert!(c.zoom_texture(Path::new("mermaid:a"), 500.0, 0).is_none());
        assert_eq!(c.delayed.len(), 1);
        assert!(c.zoom_texture(Path::new("mermaid:a"), 480.0, 0).is_none());
        assert_eq!(c.delayed.len(), 1, "same class: no second job");
        c.zoom_texture(Path::new("mermaid:a"), 3000.0, 0);
        assert_eq!(c.delayed.len(), 1, "the old class is dropped");
        assert_eq!(
            c.jobs.get(Path::new("mermaid:a@4096@0")).and_then(|j| match j {
                RenderJob::Mermaid(m) => m.px_w,
                #[cfg(feature = "excalidraw")]
                RenderJob::Excalidraw(e) => e.px_w,
            }),
            Some(4096.0)
        );
    }

    #[test]
    fn viewers_have_their_own_zoom_raster_and_release_it() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        c.zoom_texture(Path::new("mermaid:a"), 500.0, 0);
        c.zoom_texture(Path::new("mermaid:a"), 500.0, 1);
        assert_eq!(c.delayed.len(), 2, "neither job evicts the other's");
        c.release_zoom(0);
        assert_eq!((c.delayed.len(), c.jobs.len()), (1, 2));
        assert!(!c.entries.contains_key(Path::new("mermaid:a@512@0")) && c.entries.contains_key(Path::new("mermaid:a@512@1")));
    }

    #[test]
    fn evicting_a_zoom_raster_frees_it_instead_of_queueing_a_render() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        c.queue.clear();
        c.zoom_texture(Path::new("mermaid:a"), 500.0, 0);
        ready(&mut c, "mermaid:a@512@0");
        c.delayed.clear();
        c.bytes = TEXTURE_CAP_BYTES + 1;
        c.entries.insert("mermaid:a".into(), Entry::Failed(String::new()));
        c.evict(u64::MAX);
        assert!(c.queue.is_empty() && !c.entries.contains_key(Path::new("mermaid:a@512@0")) && c.zoom[0].is_none());
    }

    #[test]
    fn a_finished_render_replaces_the_bytes_of_the_old_one() {
        let ctx = Context::default();
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        c.bytes = 4;
        let _ = c.tx.send(Msg::Done("mermaid:a".into(), ColorImage::example(), None, None));
        c.poll(&ctx);
        let img = ColorImage::example();
        assert_eq!(c.bytes, img.size[0] * img.size[1] * 4, "old 4 bytes subtracted");
    }

    #[test]
    fn slot_path_follows_the_slot_and_never_invents_keys() {
        let mut c = ImageCache::default();
        assert!(c.slot_path(1).is_none());
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        ready(&mut c, "mermaid:a");
        c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert_eq!(c.slot_path(1).as_deref(), Some(Path::new("mermaid:a")), "last good while b renders");
        ready(&mut c, "mermaid:b");
        assert_eq!(c.slot_path(1).as_deref(), Some(Path::new("mermaid:b")));
        assert_eq!(c.dims(Path::new("mermaid:zzz")), None);
    }

    #[test]
    fn a_second_render_of_a_ready_key_keeps_the_svg_size() {
        let ctx = Context::default();
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        let text = || Some(Arc::new(mermaid::DiagramText { size: [94.9, 163.2], labels: vec![] }));
        // Two renders of one key: the second one's Dims message finds the entry Ready and is ignored.
        for _ in 0..2 {
            let _ = c.tx.send(Msg::Dims("mermaid:a".into(), [95, 164]));
            let _ = c.tx.send(Msg::Done("mermaid:a".into(), ColorImage::new([140, 240], vec![egui::Color32::WHITE; 140 * 240]), None, text()));
            c.poll(&ctx);
        }
        assert_eq!(c.dims(Path::new("mermaid:a")), Some([95, 164]));
    }

    #[test]
    fn identical_diagrams_keep_their_shared_render_when_one_is_edited() {
        let mut c = ImageCache::default();
        c.mermaid(Path::new("mermaid:a"), 1, false, || job("a"));
        c.mermaid(Path::new("mermaid:a"), 2, false, || job("a"));
        ready(&mut c, "mermaid:a");
        c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        ready(&mut c, "mermaid:b");
        c.mermaid(Path::new("mermaid:b"), 1, false, || job("b"));
        assert!(c.ready(Path::new("mermaid:a")).is_some(), "slot 2 still shows it");
        // Slot 2 edited too: now nobody uses it.
        c.mermaid(Path::new("mermaid:c"), 2, false, || job("c"));
        ready(&mut c, "mermaid:c");
        c.mermaid(Path::new("mermaid:c"), 2, false, || job("c"));
        assert!(!c.entries.contains_key(Path::new("mermaid:a")));
    }

    #[test]
    fn in_flight_counts_running_queued_and_uploads() {
        let ctx = Context::default();
        let mut c = ImageCache::default();
        assert_eq!(c.in_flight(), 0);
        let k = PathBuf::from("missing_in_flight");
        c.entries.insert(k.clone(), Entry::Sized([10, 10]));
        c.want(&k);
        assert_eq!(c.in_flight(), 1);
        c.queue.clear();
        c.uploads.push_back((k, ColorImage::example(), None, None));
        assert_eq!(c.in_flight(), 1);
        c.poll(&ctx);
        assert_eq!(c.in_flight(), 0);
    }
}
