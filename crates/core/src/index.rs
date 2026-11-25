//! In-memory note index (outlinks, headings, tags, name resolution, backlinks) and the
//! background `Indexer` that builds and maintains it.
use crate::glob::ExcludeSet;
use crate::kind::FileKind;
use crate::links::{Heading, NoteScan, OutLink, scan_markdown, scan_typst};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub type NoteId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteMeta {
    pub id: NoteId,
    pub path: PathBuf,
    pub root: u16,
    /// Path relative to the root, '/' separators.
    pub rel: String,
    pub stem: String,
    pub kind: FileKind,
    pub mtime_ms: u64,
    pub size: u64,
    pub title: Option<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<OutLink>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backlink {
    pub from: NoteId,
    pub line: u32,
}

/// An indexed file of any kind.
#[derive(Debug, Clone, Copy)]
pub struct FileRef<'a> {
    pub path: &'a Path,
    pub root: u16,
    pub rel: &'a str,
}

struct Slot {
    meta: NoteMeta,
    rel_lc: String,
    rel_lc_noext: String,
}

struct FileSlot {
    root: u16,
    rel: String,
    rel_lc: String,
}

/// An insert with its allocation-heavy part (lowercasing, copies, stem) done, so the part that runs under the
/// index write lock is hash-map work only. Build these in parallel, insert them in short write sections.
pub(crate) struct Prepared {
    path: PathBuf,
    fname: String,
    /// `vec![path.clone()]`: becomes the `by_fname` bucket when the name is new.
    fname_bucket: Vec<PathBuf>,
    file: FileSlot,
    note: Option<PreparedNote>,
}

struct PreparedNote {
    path: PathBuf,
    slot: Slot,
    stem_lc: String,
    /// Empty with room for one id: becomes the `by_stem` bucket when the stem is new.
    stem_bucket: Vec<NoteId>,
    aliases_lc: Vec<String>,
}

impl Prepared {
    pub(crate) fn new(path: PathBuf, root: u16, rel: String, kind: FileKind, note: Option<NoteData>) -> Prepared {
        let rel_lc = rel.to_lowercase();
        let fname = rel_lc.rsplit('/').next().unwrap_or("").to_string();
        let note = note.map(|nd| {
            let stem = FileKind::note_stem(&path).unwrap_or_default().into_owned();
            let stem_lc = stem.to_lowercase();
            let rel_lc_noext = strip_note_ext(&rel_lc).0.to_string();
            let NoteScan { title, headings, links, tags, aliases, .. } = nd.scan;
            let aliases_lc = aliases.iter().map(|a| a.to_lowercase()).collect();
            let meta = NoteMeta { id: 0, path: path.clone(), root, rel: rel.clone(), stem, kind, mtime_ms: nd.mtime_ms, size: nd.size, title, headings, links, tags, aliases };
            PreparedNote { path: path.clone(), slot: Slot { meta, rel_lc: rel_lc.clone(), rel_lc_noext }, stem_lc, stem_bucket: Vec::with_capacity(1), aliases_lc }
        });
        Prepared { fname_bucket: vec![path.clone()], path, fname, file: FileSlot { root, rel, rel_lc }, note }
    }
}

type BacklinkMap = FxHashMap<NoteId, Vec<Backlink>>;

#[derive(Default)]
pub struct Index {
    roots: Vec<PathBuf>,
    slots: Vec<Option<Slot>>,
    free: Vec<NoteId>,
    by_path: FxHashMap<PathBuf, NoteId>,
    by_stem: FxHashMap<String, Vec<NoteId>>,
    by_alias: FxHashMap<String, Vec<NoteId>>,
    files: FxHashMap<PathBuf, FileSlot>,
    by_fname: FxHashMap<String, Vec<PathBuf>>,
    n_notes: usize,
    generation: u64,
    backlink_cache: Mutex<Option<(u64, Arc<BacklinkMap>)>>,
}

pub(crate) struct NoteData {
    pub mtime_ms: u64,
    pub size: u64,
    pub scan: NoteScan,
}

fn strip_target_prefixes(t: &str) -> &str {
    let mut s = t.trim();
    loop {
        if let Some(r) = s.strip_prefix("./").or_else(|| s.strip_prefix(".\\")) {
            s = r;
        } else if let Some(r) = s.strip_prefix('/').or_else(|| s.strip_prefix('\\')) {
            s = r;
        } else {
            break;
        }
    }
    s
}

/// Normalize a link target without heap allocations on the common ASCII path:
/// borrows directly when already lowercase ASCII without `\`, uses a 256-byte
/// stack buffer for mixed-case/backslash ASCII, and falls back to `String`
/// only for non-ASCII or targets > 256 bytes.
#[inline]
fn with_norm_target<R>(target: &str, f: impl FnOnce(&str) -> R) -> R {
    let s = strip_target_prefixes(target);
    let mut needs_norm = false;
    let mut is_ascii = true;
    for &b in s.as_bytes() {
        if b >= 0x80 {
            is_ascii = false;
            break;
        }
        needs_norm |= b.is_ascii_uppercase() | (b == b'\\');
    }
    let mut buf: [u8; 256];
    let owned: String;
    let norm: &str = if is_ascii && !needs_norm {
        s
    } else if is_ascii && s.len() <= 256 {
        buf = [0u8; 256];
        for (dst, &b) in buf.iter_mut().zip(s.as_bytes()) {
            *dst = if b == b'\\' { b'/' } else { b.to_ascii_lowercase() };
        }
        // All bytes are verified ASCII (< 0x80), so UTF-8 validation always succeeds; fallback avoids unwrap.
        if let Ok(norm) = std::str::from_utf8(&buf[..s.len()]) {
            norm
        } else {
            owned = s.replace('\\', "/").to_lowercase();
            &owned
        }
    } else {
        owned = if s.as_bytes().contains(&b'\\') { s.replace('\\', "/").to_lowercase() } else { s.to_lowercase() };
        &owned
    };
    f(norm)
}

fn ends_with_component(hay: &str, needle: &str) -> bool {
    hay.ends_with(needle) && (hay.len() == needle.len() || hay.as_bytes()[hay.len() - needle.len() - 1] == b'/')
}

fn strip_note_ext(name: &str) -> (&str, bool) {
    const EXCALIDRAW_MD: &str = ".excalidraw.md";
    if cfg!(feature = "excalidraw")
        && name.len() > EXCALIDRAW_MD.len()
        && name.as_bytes()[name.len() - EXCALIDRAW_MD.len()..].eq_ignore_ascii_case(EXCALIDRAW_MD.as_bytes())
    {
        let stem = &name[..name.len() - EXCALIDRAW_MD.len()];
        if !stem.ends_with('/') && !stem.ends_with('\\') {
            return (stem, true);
        }
    }
    if let Some((stem, ext)) = name.rsplit_once('.') {
        if !stem.is_empty() && !stem.ends_with('/') && !stem.ends_with('\\') && FileKind::from_ext(ext).is_note() {
            return (stem, true);
        }
    }
    (name, false)
}

fn lexical_join(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Index {
    pub fn new() -> Index {
        Index::default()
    }

    pub fn len(&self) -> usize {
        self.n_notes
    }

    pub fn is_empty(&self) -> bool {
        self.n_notes == 0
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn notes(&self) -> impl Iterator<Item = &NoteMeta> {
        self.slots.iter().filter_map(|s| s.as_ref().map(|s| &s.meta))
    }

    pub fn get(&self, id: NoteId) -> Option<&NoteMeta> {
        self.slots.get(id as usize)?.as_ref().map(|s| &s.meta)
    }

    pub fn by_path(&self, path: &Path) -> Option<NoteId> {
        self.by_path.get(path).copied()
    }

    /// Every indexed file (notes, images, PDFs, ...).
    pub fn files(&self) -> impl Iterator<Item = FileRef<'_>> {
        self.files.iter().map(|(p, f)| FileRef { path: p, root: f.root, rel: &f.rel })
    }

    pub(crate) fn with_capacity(roots: Vec<PathBuf>, cap: usize) -> Index {
        let mut ix = Index { roots, ..Index::default() };
        ix.reserve(cap);
        ix
    }

    /// Room for `cap` more files: growing a 50k-entry map while readers wait would be a long lock hold.
    pub(crate) fn reserve(&mut self, cap: usize) {
        self.slots.reserve(cap);
        self.files.reserve(cap);
        self.by_path.reserve(cap);
        self.by_stem.reserve(cap);
        self.by_fname.reserve(cap);
    }

    #[cfg(test)]
    pub(crate) fn set_roots(&mut self, roots: Vec<PathBuf>) {
        self.roots = roots;
    }

    pub(crate) fn bump(&mut self) {
        self.generation += 1;
    }

    /// Insert or replace a file; notes carry parsed data.
    pub(crate) fn insert(&mut self, path: PathBuf, root: u16, rel: String, kind: FileKind, note: Option<NoteData>) {
        self.insert_prepared(Prepared::new(path, root, rel, kind, note));
    }

    /// The map work of `insert`; everything that allocates or lowercases was done in `Prepared::new`, off any lock.
    pub(crate) fn insert_prepared(&mut self, p: Prepared) {
        use std::collections::hash_map::Entry;
        let Prepared { path, fname, fname_bucket, file, note } = p;
        let replaced = match self.files.entry(path) {
            Entry::Vacant(v) => {
                match self.by_fname.entry(fname) {
                    Entry::Vacant(b) => {
                        b.insert(fname_bucket);
                    }
                    Entry::Occupied(mut b) => b.get_mut().extend(fname_bucket),
                }
                v.insert(file);
                false
            }
            Entry::Occupied(mut o) => {
                o.insert(file);
                true
            }
        };
        let Some(n) = note else {
            return;
        };
        // A path new to `files` cannot be a note yet: skip the lookup.
        if replaced {
            if let Some(&id) = self.by_path.get(&n.path) {
                self.unlink_names(id);
                self.slots[id as usize] = None;
                self.n_notes -= 1;
                self.by_path.remove(&n.path);
                self.free.push(id);
            }
        }
        let id = match self.free.pop() {
            Some(id) => id,
            None => {
                self.slots.push(None);
                (self.slots.len() - 1) as NoteId
            }
        };
        let mut slot = n.slot;
        slot.meta.id = id;
        let mut bucket = n.stem_bucket;
        bucket.push(id);
        match self.by_stem.entry(n.stem_lc) {
            Entry::Vacant(b) => {
                b.insert(bucket);
            }
            Entry::Occupied(mut b) => b.get_mut().push(id),
        }
        for a in n.aliases_lc {
            self.by_alias.entry(a).or_default().push(id);
        }
        self.by_path.insert(n.path, id);
        self.slots[id as usize] = Some(slot);
        self.n_notes += 1;
    }

    fn unlink_names(&mut self, id: NoteId) {
        let Some(slot) = self.slots[id as usize].as_ref() else { return };
        let stem = slot.meta.stem.to_lowercase();
        let aliases: Vec<String> = slot.meta.aliases.iter().map(|a| a.to_lowercase()).collect();
        if let Some(v) = self.by_stem.get_mut(&stem) {
            v.retain(|&x| x != id);
            if v.is_empty() {
                self.by_stem.remove(&stem);
            }
        }
        for a in aliases {
            if let Some(v) = self.by_alias.get_mut(&a) {
                v.retain(|&x| x != id);
                if v.is_empty() {
                    self.by_alias.remove(&a);
                }
            }
        }
    }

    pub(crate) fn remove(&mut self, path: &Path) -> bool {
        let Some(f) = self.files.remove(path) else {
            return false;
        };
        let fname = f.rel_lc.rsplit('/').next().unwrap_or("").to_string();
        if let Some(v) = self.by_fname.get_mut(&fname) {
            v.retain(|p| p != path);
            if v.is_empty() {
                self.by_fname.remove(&fname);
            }
        }
        if let Some(id) = self.by_path.remove(path) {
            self.unlink_names(id);
            self.slots[id as usize] = None;
            self.free.push(id);
            self.n_notes -= 1;
        }
        true
    }

    /// Paths of all indexed files under `dir`.
    pub(crate) fn paths_under(&self, dir: &Path) -> Vec<PathBuf> {
        // Byte prefix plus separator check: about 10x cheaper than `Path::starts_with` over 50k keys.
        let d = dir.as_os_str().as_encoded_bytes();
        // Windows accepts `/` as well as `\\`.
        let sep = |c: u8| c == std::path::MAIN_SEPARATOR as u8 || (cfg!(windows) && c == b'/');
        let d = match d.split_last() {
            Some((&c, rest)) if sep(c) && !rest.is_empty() => rest,
            _ => d,
        };
        self.files
            .keys()
            .filter(|p| {
                let b = p.as_os_str().as_encoded_bytes();
                b.starts_with(d) && (b.len() == d.len() || sep(b[d.len()]) || d.last().is_some_and(|&c| sep(c)))
            })
            .cloned()
            .collect()
    }

    fn root_of(&self, path: &Path) -> Option<u16> {
        if let Some(f) = self.files.get(path) {
            return Some(f.root);
        }
        self.roots.iter().enumerate().filter(|(_, r)| path.starts_with(r)).max_by_key(|(_, r)| r.components().count()).map(|(i, _)| i as u16)
    }

    /// Dir-relative targets (`../x`) resolved against `from`'s directory.
    fn resolve_relative(&self, target: &str, from: Option<&Path>) -> Option<NoteId> {
        let trimmed = target.trim();
        // Check relative prefixes before normalizing backslashes so standard targets allocate nothing.
        if !(trimmed.starts_with("../") || trimmed.starts_with("./") || trimmed.starts_with("..\\") || trimmed.starts_with(".\\")) {
            return None;
        }
        let base = from?.parent()?;
        let t: Cow<'_, str> = if trimmed.contains('\\') { Cow::Owned(trimmed.replace('\\', "/")) } else { Cow::Borrowed(trimmed) };
        let p = lexical_join(base, &t);
        if let Some(&id) = self.by_path.get(&p) {
            return Some(id);
        }
        for ext in [".md", ".typ", ".excalidraw", ".excalidraw.md"] {
            let mut s = p.clone().into_os_string();
            s.push(ext);
            if let Some(&id) = self.by_path.get(Path::new(&s)) {
                return Some(id);
            }
        }
        None
    }

    fn pick_best_note<'a>(&'a self, ids: &[NoteId], from: Option<&Path>, mut pred: impl FnMut(&Slot) -> bool) -> Option<NoteId> {
        if ids.len() == 1 {
            let id = ids[0];
            let slot = self.slots[id as usize].as_ref()?;
            return pred(slot).then_some(id);
        }
        // Defer TieCtx::new (which probes `self.files`) until at least two candidates actually match.
        let mut first: Option<(NoteId, &'a Slot)> = None;
        let mut best: Option<(TieCtx<'_>, TieKey<'a>, NoteId)> = None;
        for &id in ids {
            let Some(slot) = self.slots[id as usize].as_ref() else { continue };
            if !pred(slot) {
                continue;
            }
            if let Some((ctx, best_key, best_id)) = &mut best {
                let key = ctx.key(&slot.meta.path, slot.meta.root, &slot.rel_lc);
                if key < *best_key {
                    *best_key = key;
                    *best_id = id;
                }
            } else if let Some((id0, slot0)) = first {
                let ctx = TieCtx::new(self, from);
                let k0 = ctx.key(&slot0.meta.path, slot0.meta.root, &slot0.rel_lc);
                let k1 = ctx.key(&slot.meta.path, slot.meta.root, &slot.rel_lc);
                best = Some(if k1 < k0 { (ctx, k1, id) } else { (ctx, k0, id0) });
            } else {
                first = Some((id, slot));
            }
        }
        best.map(|(_, _, id)| id).or_else(|| first.map(|(id, _)| id))
    }

    fn resolve_norm(&self, mut t: &str, from: Option<&Path>) -> Option<NoteId> {
        while let Some(r) = t.strip_prefix("../") {
            t = r;
        }
        if t.is_empty() {
            return None;
        }
        let last = t.rsplit_once('/').map_or(t, |(_, r)| r);
        let (stem_key, has_ext) = strip_note_ext(last);
        if let Some(ids) = self.by_stem.get(stem_key) {
            if let Some(id) = self.pick_best_note(ids, from, |slot| if has_ext { ends_with_component(&slot.rel_lc, t) } else { ends_with_component(&slot.rel_lc_noext, t) }) {
                return Some(id);
            }
            if has_ext && t.ends_with(".excalidraw") {
                if let Some(id) = self.pick_best_note(ids, from, |slot| {
                    slot.meta.kind == FileKind::Excalidraw
                        && ends_with_component(slot.rel_lc.strip_suffix(".md").unwrap_or(&slot.rel_lc), t)
                }) {
                    return Some(id);
                }
            }
            if stem_key.len() != t.len() {
                if let Some(id) = self.pick_best_note(ids, from, |_| true) {
                    return Some(id);
                }
            }
        }
        if let Some(ids) = self.by_alias.get(t) {
            return self.pick_best_note(ids, from, |_| true);
        }
        None
    }

    pub fn resolve(&self, target: &str, from: Option<&Path>) -> Option<NoteId> {
        if let Some(id) = self.resolve_relative(target, from) {
            return Some(id);
        }
        with_norm_target(target, |t| self.resolve_norm(t, from))
    }

    fn resolve_file_norm(&self, mut t: &str, from: Option<&Path>) -> Option<PathBuf> {
        while let Some(r) = t.strip_prefix("../") {
            t = r;
        }
        if t.is_empty() {
            return None;
        }
        let last = t.rsplit_once('/').map_or(t, |(_, r)| r);
        let paths = self.by_fname.get(last)?;
        let mut first: Option<(&PathBuf, &FileSlot)> = None;
        let mut best: Option<(TieCtx<'_>, TieKey<'_>, &PathBuf)> = None;
        for p in paths {
            let Some(f) = self.files.get(p) else { continue };
            if !ends_with_component(&f.rel_lc, t) {
                continue;
            }
            if let Some((ctx, best_key, best_p)) = &mut best {
                let key = ctx.key(p, f.root, &f.rel_lc);
                if key < *best_key {
                    *best_key = key;
                    *best_p = p;
                }
            } else if let Some((p0, f0)) = first {
                let ctx = TieCtx::new(self, from);
                let k0 = ctx.key(p0, f0.root, &f0.rel_lc);
                let k1 = ctx.key(p, f.root, &f.rel_lc);
                best = Some(if k1 < k0 { (ctx, k1, p) } else { (ctx, k0, p0) });
            } else {
                first = Some((p, f));
            }
        }
        best.map(|(_, _, p)| p.clone()).or_else(|| first.map(|(p, _)| p.clone()))
    }

    /// Any indexed file (images, PDFs, ...) by name or relative path.
    pub fn resolve_file(&self, target: &str, from: Option<&Path>) -> Option<PathBuf> {
        let trimmed = target.trim();
        if (trimmed.starts_with("../") || trimmed.starts_with("./") || trimmed.starts_with("..\\") || trimmed.starts_with(".\\")) && from.is_some() {
            if let Some(base) = from.and_then(|f| f.parent()) {
                let tt: Cow<'_, str> = if trimmed.contains('\\') { Cow::Owned(trimmed.replace('\\', "/")) } else { Cow::Borrowed(trimmed) };
                let p = lexical_join(base, &tt);
                if self.files.contains_key(&p) {
                    return Some(p);
                }
            }
        }
        if let Some(p) = with_norm_target(target, |t| self.resolve_file_norm(t, from)) {
            return Some(p);
        }
        self.resolve(target, from).and_then(|id| self.get(id)).map(|m| m.path.clone())
    }

    /// Another file than those in `except` already has the name of `path`: its stem for a note (what `[[name]]` matches), its file name otherwise.
    pub fn name_taken(&self, path: &Path, except: &rustc_hash::FxHashSet<PathBuf>) -> bool {
        let lc = |o: Option<&std::ffi::OsStr>| o.map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        if FileKind::from_path(path).is_note() {
            let stem = FileKind::note_stem(path).map(|s| s.to_lowercase()).unwrap_or_default();
            let ids = self.by_stem.get(&stem);
            ids.is_some_and(|v| v.iter().filter_map(|&i| self.slots[i as usize].as_ref()).any(|s| !except.contains(&s.meta.path)))
        } else {
            self.by_fname.get(&lc(path.file_name())).is_some_and(|v| v.iter().any(|p| !except.contains(p)))
        }
    }

    fn backlink_map(&self) -> Arc<BacklinkMap> {
        let mut cache = self.backlink_cache.lock();
        if let Some((g, m)) = cache.as_ref() {
            if *g == self.generation {
                return m.clone();
            }
        }
        let mut map: BacklinkMap = FxHashMap::default();
        for n in self.notes() {
            for l in &n.links {
                if let Some(to) = self.resolve(&l.target, Some(&n.path)) {
                    if to != n.id {
                        map.entry(to).or_default().push(Backlink { from: n.id, line: l.line });
                    }
                }
            }
        }
        for v in map.values_mut() {
            v.sort_by_key(|b| (b.from, b.line));
        }
        let m = Arc::new(map);
        *cache = Some((self.generation, m.clone()));
        m
    }

    /// Notes linking to `id` (cached per generation).
    pub fn backlinks(&self, id: NoteId) -> Vec<Backlink> {
        self.backlink_map().get(&id).cloned().unwrap_or_default()
    }

    /// `(tag, note count)` sorted by count descending then name.
    pub fn all_tags(&self) -> Vec<(String, u32)> {
        let mut m: FxHashMap<&str, u32> = FxHashMap::default();
        for n in self.notes() {
            for t in &n.tags {
                *m.entry(t.as_str()).or_default() += 1;
            }
        }
        let mut v: Vec<(String, u32)> = m.into_iter().map(|(k, c)| (k.to_string(), c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }
}

type TieKey<'a> = (bool, bool, usize, &'a str);

struct TieCtx<'a> {
    from_dir: Option<&'a Path>,
    from_root: Option<u16>,
}

impl<'a> TieCtx<'a> {
    fn new(ix: &Index, from: Option<&'a Path>) -> Self {
        TieCtx { from_dir: from.and_then(|f| f.parent()), from_root: from.and_then(|f| ix.root_of(f)) }
    }

    fn key<'b>(&self, path: &Path, root: u16, rel_lc: &'b str) -> TieKey<'b> {
        let same_dir = self.from_dir.is_some_and(|d| path.parent() == Some(d));
        let same_root = self.from_root == Some(root);
        (!same_dir, !same_root, rel_lc.matches('/').count(), rel_lc)
    }
}

// ---------------------------------------------------------------------------------------------
// Indexer
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IndexStatus {
    pub scanning: bool,
    pub files: u32,
    pub notes: u32,
    pub last_scan_ms: f32,
}

enum Cmd {
    Rescan,
    SetRoots(Vec<PathBuf>),
    FileChanged(PathBuf),
    UpdateText(PathBuf, String),
}

struct Shared {
    index: Arc<RwLock<Index>>,
    status: Mutex<IndexStatus>,
    reads: AtomicUsize,
    pending: AtomicUsize,
    scans_pending: AtomicUsize,
    /// See `Indexer::partial`.
    partial: AtomicBool,
}

impl Shared {
    fn status(&self) -> IndexStatus {
        let mut s = *self.status.lock();
        s.scanning = self.scans_pending.load(Ordering::SeqCst) > 0;
        s
    }
}

/// Read-only handle for observing index status and contents without keeping the worker thread alive.
#[derive(Clone)]
pub struct IndexHandle {
    shared: Arc<Shared>,
}

impl IndexHandle {
    pub fn index(&self) -> &Arc<RwLock<Index>> {
        &self.shared.index
    }

    pub fn status(&self) -> IndexStatus {
        self.shared.status()
    }

    /// See `Indexer::partial`.
    pub fn partial(&self) -> bool {
        self.shared.partial.load(Ordering::SeqCst)
    }
}

pub struct Indexer {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
}

impl Indexer {
    /// Returns immediately; the initial scan runs on a background thread.
    pub fn spawn(roots: Vec<PathBuf>, cache_file: Option<PathBuf>, notify: Arc<dyn Fn() + Send + Sync>) -> Indexer {
        Self::spawn_with(roots, cache_file, notify, ExcludeSet::default())
    }

    /// Like `spawn`; entries matching `excludes` (see `glob::ExcludeSet`) are neither walked nor indexed.
    pub fn spawn_with(roots: Vec<PathBuf>, cache_file: Option<PathBuf>, notify: Arc<dyn Fn() + Send + Sync>, excludes: ExcludeSet) -> Indexer {
        Self::spawn_after(roots, cache_file, notify, excludes, Duration::ZERO)
    }

    /// Like `spawn_with`; the worker waits `start_delay` before it starts. At app start the CPU and allocator burst of
    /// hydrating a big index slows the first frames (measured: frame 3 +10 ms on 52k files), a short delay avoids that.
    /// shortcut: a fixed delay instead of a "first frames are out" signal from the frontend; replace it with that signal when there is one.
    pub fn spawn_after(roots: Vec<PathBuf>, cache_file: Option<PathBuf>, notify: Arc<dyn Fn() + Send + Sync>, excludes: ExcludeSet, start_delay: Duration) -> Indexer {
        let shared = Arc::new(Shared {
            index: Arc::new(RwLock::new(Index::new())),
            status: Mutex::new(IndexStatus { scanning: true, ..Default::default() }),
            reads: AtomicUsize::new(0),
            pending: AtomicUsize::new(1),
            scans_pending: AtomicUsize::new(1),
            partial: AtomicBool::new(false),
        });
        let (tx, rx) = unbounded();
        let worker_shared = shared.clone();
        let spawned = std::thread::Builder::new().name("iron-sheets-index".into()).spawn(move || {
            let Some(pool) = build_pool() else {
                worker_shared.pending.store(0, Ordering::SeqCst);
                worker_shared.scans_pending.store(0, Ordering::SeqCst);
                worker_shared.status.lock().scanning = false;
                return;
            };
            let worker = Worker {
                shared: worker_shared,
                rx,
                roots,
                excludes,
                pool: Arc::new(pool),
                start_delay,
                disk_cache: None,
                cache_file,
                overrides: FxHashSet::default(),
                notify,
                last_notify: Instant::now() - Duration::from_secs(1),
                pending_notify: false,
            };
            worker.run();
        });
        if spawned.is_err() {
            shared.pending.store(0, Ordering::SeqCst);
            shared.scans_pending.store(0, Ordering::SeqCst);
        }
        Indexer { shared, tx }
    }

    fn send(&self, c: Cmd, full: bool) {
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        if full {
            self.shared.scans_pending.fetch_add(1, Ordering::SeqCst);
        }
        if self.tx.send(c).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
            if full {
                self.shared.scans_pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    pub fn index(&self) -> Arc<RwLock<Index>> {
        self.shared.index.clone()
    }

    pub fn handle(&self) -> IndexHandle {
        IndexHandle { shared: self.shared.clone() }
    }

    pub fn status(&self) -> IndexStatus {
        self.shared.status()
    }

    /// True while the index is being filled section by section (cold scan, or hydrating from the cache) and may not
    /// hold every note yet. Cleared when that filling ends, which for a warm start is before the reconcile with the disk
    /// (a swap that can still add or drop notes); `status().scanning` covers that. What must see all notes (search) waits
    /// for this to clear; browsing (quick-open) may show what is there.
    pub fn partial(&self) -> bool {
        self.shared.partial.load(Ordering::SeqCst)
    }

    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        self.send(Cmd::SetRoots(roots), true);
    }

    pub fn rescan(&self) {
        self.send(Cmd::Rescan, true);
    }

    /// Re-index one file (or drop it when gone); a directory triggers a rescan of that subtree.
    pub fn file_changed(&self, path: PathBuf) {
        self.send(Cmd::FileChanged(path), false);
    }

    /// Index unsaved editor content. Call `file_changed` with the path to go back to the disk version.
    pub fn update_from_text(&self, path: &Path, text: &str) {
        self.send(Cmd::UpdateText(path.to_path_buf(), text.to_string()), false);
    }

    /// Number of note files read from disk so far (diagnostics and tests).
    pub fn read_count(&self) -> usize {
        self.shared.reads.load(Ordering::Relaxed)
    }

    /// Block until all queued work is done. Returns false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.shared.pending.load(Ordering::SeqCst) == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }
}

/// The indexer's own pool: all but two cores (UI thread and the rest), at least one thread; the global pool stays free.
fn build_pool() -> Option<rayon::ThreadPool> {
    // At least 2: the walk occupies one pool thread while hydrate's chunks need another.
    let n = std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(2).max(2);
    let build = |n| rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("iron-sheets-index-{i}")).build().ok();
    // shortcut: with a single thread the walk and hydrate run one after the other (slower, still correct); if even that fails the worker gives up.
    build(n).or_else(|| build(2)).or_else(|| build(1))
}

const CACHE_MAGIC: &[u8; 8] = b"MDXIDX\0\x01";
/// 3: `FileKind::Excalidraw` (`.excalidraw` & `.excalidraw.md`), stem & scan updates.
const CACHE_VERSION: u32 = 3;
const MAX_NOTE_BYTES: u64 = 16 * 1024 * 1024;
const NOTIFY_INTERVAL: Duration = Duration::from_millis(50);
/// Items per write section / cache chunk, at most (a section holds the index write lock for about a millisecond) ...
const SECTION: usize = 1024;
/// ... and the first one, small so the first results show up early; sections double up to `SECTION`.
const FIRST_SECTION: usize = 256;

#[derive(Serialize, Deserialize)]
struct DiskNote {
    mtime_ms: u64,
    size: u64,
    scan: NoteScan,
}

/// A cached file: `note` is `Some` for notes, `None` for images and other files.
#[derive(Serialize, Deserialize)]
struct DiskItem {
    root: u16,
    rel: String,
    note: Option<DiskNote>,
}

// Borrowing twins of the types above (same field order, so postcard encodes the same bytes): persist without cloning scans.
#[derive(Serialize)]
struct ScanRef<'a> {
    title: &'a Option<String>,
    headings: &'a [Heading],
    links: &'a [OutLink],
    tags: &'a [String],
    aliases: &'a [String],
}

#[derive(Serialize)]
struct NoteRef<'a> {
    mtime_ms: u64,
    size: u64,
    scan: ScanRef<'a>,
}

#[derive(Serialize)]
struct ItemRef<'a> {
    root: u16,
    rel: &'a str,
    note: Option<NoteRef<'a>>,
}

/// The cache file in memory: header parsed, chunk payloads still encoded (decoded in parallel while hydrating).
/// Layout: magic, version u32, chunk count u32, item count u32, roots length u32 + postcard `Vec<String>`, then per chunk length u32 + postcard `Vec<DiskItem>`.
struct RawCache {
    bytes: Vec<u8>,
    roots: Vec<String>,
    chunks: Vec<std::ops::Range<usize>>,
    items: usize,
}

fn read_u32(b: &[u8], pos: &mut usize) -> Option<usize> {
    let v = u32::from_le_bytes(b.get(*pos..pos.checked_add(4)?)?.try_into().ok()?);
    *pos += 4;
    Some(v as usize)
}

fn load_raw_cache(file: &Path) -> Option<RawCache> {
    let bytes = std::fs::read(file).ok()?;
    bytes.strip_prefix(CACHE_MAGIC.as_slice())?;
    let mut pos = CACHE_MAGIC.len();
    if read_u32(&bytes, &mut pos)? != CACHE_VERSION as usize {
        return None;
    }
    let n = read_u32(&bytes, &mut pos)?;
    let items = read_u32(&bytes, &mut pos)?;
    // Every item takes at least a few bytes: a count beyond the file size is a garbage header (and would blow up `reserve`).
    if items > bytes.len() {
        return None;
    }
    let roots_len = read_u32(&bytes, &mut pos)?;
    let roots: Vec<String> = postcard::from_bytes(bytes.get(pos..pos.checked_add(roots_len)?)?).ok()?;
    pos += roots_len;
    let mut chunks = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let len = read_u32(&bytes, &mut pos)?;
        let end = pos.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        chunks.push(pos..end);
        pos = end;
    }
    Some(RawCache { bytes, roots, chunks, items })
}

/// Chunk boundaries for `n` items: `FIRST_SECTION`, doubling up to `SECTION`.
fn growing_ranges(n: usize) -> Vec<std::ops::Range<usize>> {
    let (mut out, mut start, mut size) = (Vec::new(), 0, FIRST_SECTION);
    while start < n {
        let end = (start + size).min(n);
        out.push(start..end);
        start = end;
        size = (size * 2).min(SECTION);
    }
    out
}

fn split_growing<T>(items: Vec<T>) -> Vec<Vec<T>> {
    let mut it = items.into_iter();
    growing_ranges(it.len()).into_iter().map(|r| it.by_ref().take(r.len()).collect()).collect()
}

fn persist_disk_cache(pool: &rayon::ThreadPool, file: &Path, index: &RwLock<Index>) -> std::io::Result<()> {
    let ix = index.read();
    let mut items: Vec<ItemRef<'_>> = Vec::with_capacity(ix.files.len());
    for n in ix.notes() {
        let scan = ScanRef { title: &n.title, headings: &n.headings, links: &n.links, tags: &n.tags, aliases: &n.aliases };
        items.push(ItemRef { root: n.root, rel: &n.rel, note: Some(NoteRef { mtime_ms: n.mtime_ms, size: n.size, scan }) });
    }
    for f in ix.files.values() {
        if !FileKind::from_path(Path::new(&f.rel)).is_note() {
            items.push(ItemRef { root: f.root, rel: &f.rel, note: None });
        }
    }
    let n_items = items.len();
    let chunks = pool
        .install(|| growing_ranges(items.len()).into_par_iter().map(|r| postcard::to_stdvec(&items[r])).collect::<Result<Vec<_>, _>>())
        .map_err(std::io::Error::other)?;
    let roots: Vec<String> = ix.roots().iter().map(|r| r.to_string_lossy().into_owned()).collect();
    drop(items);
    drop(ix);
    let roots = postcard::to_stdvec(&roots).map_err(std::io::Error::other)?;
    let mut bytes = Vec::with_capacity(chunks.iter().map(|c| c.len() + 4).sum::<usize>() + roots.len() + 20);
    bytes.extend_from_slice(CACHE_MAGIC);
    bytes.extend_from_slice(&CACHE_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(n_items as u32).to_le_bytes());
    bytes.extend_from_slice(&(roots.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&roots);
    for c in &chunks {
        bytes.extend_from_slice(&(c.len() as u32).to_le_bytes());
        bytes.extend_from_slice(c);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::textio::atomic_write(file, &bytes)
}

/// Test helper: the cached notes by path.
#[cfg(test)]
fn load_disk_cache(file: &Path) -> Option<FxHashMap<PathBuf, DiskItem>> {
    let raw = load_raw_cache(file)?;
    let mut out = FxHashMap::default();
    for r in &raw.chunks {
        let items: Vec<DiskItem> = postcard::from_bytes(&raw.bytes[r.clone()]).ok()?;
        for it in items.into_iter().filter(|i| i.note.is_some()) {
            out.insert(join_rel(Path::new(raw.roots.get(it.root as usize)?), &it.rel), it);
        }
    }
    Some(out)
}

/// `root` + the '/'-separated `rel` with native separators, one allocation.
fn join_rel(root: &Path, rel: &str) -> PathBuf {
    use std::ffi::OsString;
    let mut s = OsString::with_capacity(root.as_os_str().len() + rel.len() + 1);
    s.push(root);
    for c in rel.split('/') {
        let ends_with_sep = s.as_encoded_bytes().last().is_some_and(|&b| b == b'/' || (cfg!(windows) && b == b'\\'));
        if !ends_with_sep {
            s.push(std::path::MAIN_SEPARATOR_STR);
        }
        s.push(c);
    }
    PathBuf::from(s)
}

/// A cached relative path must stay inside its root: no empty, `.`, `..`, absolute or drive components.
fn plausible_rel(rel: &str) -> bool {
    // `\\` and `:` are legal in Unix file names; only on Windows can they escape the root (`C:x`, `a\\..`).
    let bad_char = |c: &str| cfg!(windows) && (c.contains('\\') || c.contains(':'));
    !rel.is_empty() && !Path::new(rel).has_root() && rel.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !bad_char(c))
}

/// True when `rel` or any of its leading components matches `excludes` (what `walk` would have skipped).
fn rel_excluded(excludes: &ExcludeSet, rel: &str) -> bool {
    let mut end = 0;
    for name in rel.split('/') {
        end += name.len();
        if excludes.matches(&rel[..end], name) {
            return true;
        }
        end += 1;
    }
    false
}

/// Runs `prep` over `inputs` on `pool` and hands the results to `sink` on the calling thread, in input order, as soon as
/// the next one is ready: parallel preparation overlapped with the serial (locked) insertion.
/// The second argument of `sink` tells whether the following result is already waiting.
/// The caller must not be a thread of `pool` (it blocks waiting for results).
fn pipeline<I: Send, T: Send>(pool: &rayon::ThreadPool, inputs: Vec<I>, prep: impl Fn(I) -> T + Sync + Send, mut sink: impl FnMut(T, bool)) {
    let n = inputs.len();
    let (tx, rx) = unbounded::<(usize, T)>();
    pool.in_place_scope(|s| {
        s.spawn(move |_| {
            inputs.into_par_iter().enumerate().for_each_with(tx, |tx, (i, x)| {
                let _ = tx.send((i, prep(x)));
            });
        });
        let (mut next, mut ready) = (0, std::collections::BTreeMap::new());
        while next < n {
            let Ok((i, v)) = rx.recv() else { break };
            ready.insert(i, v);
            while let Some(v) = ready.remove(&next) {
                next += 1;
                sink(v, ready.contains_key(&next));
            }
        }
    });
}

struct Item {
    path: PathBuf,
    root: u16,
    rel: String,
    kind: FileKind,
}

struct Entry {
    item: Item,
    note: Option<NoteData>,
}

fn mtime_ms_of(md: &std::fs::Metadata) -> u64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64)
}

struct Progress<'a> {
    shared: &'a Shared,
    notify: &'a (dyn Fn() + Send + Sync),
    start: Instant,
    files_seen: AtomicUsize,
    floor: AtomicUsize,
    next_notify_ms: AtomicU64,
}

impl<'a> Progress<'a> {
    #[cfg(test)]
    fn new(shared: &'a Shared, notify: &'a (dyn Fn() + Send + Sync), start: Instant) -> Self {
        Self::with_floor(shared, notify, start, 0)
    }

    fn with_floor(shared: &'a Shared, notify: &'a (dyn Fn() + Send + Sync), start: Instant, floor: usize) -> Self {
        Self {
            shared,
            notify,
            start,
            files_seen: AtomicUsize::new(0),
            floor: AtomicUsize::new(floor),
            next_notify_ms: AtomicU64::new(NOTIFY_INTERVAL.as_millis() as u64),
        }
    }

    fn set_floor(&self, n: usize) {
        self.floor.store(n, Ordering::Relaxed);
    }

    fn add_files(&self, n: usize) {
        if n > 0 {
            self.files_seen.fetch_add(n, Ordering::Relaxed);
        }
        self.maybe_notify();
    }

    fn maybe_notify(&self) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        let next = self.next_notify_ms.load(Ordering::Relaxed);
        if elapsed_ms >= next
            && self
                .next_notify_ms
                .compare_exchange(next, elapsed_ms + NOTIFY_INTERVAL.as_millis() as u64, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            self.shared.status.lock().files = self.files_seen.load(Ordering::Relaxed).max(self.floor.load(Ordering::Relaxed)) as u32;
            (self.notify)();
        }
    }
}

/// Parallel breadth-first directory walk. Skips dot entries, does not follow symlinked directories.
type DirJob = (PathBuf, u16, String);

fn walk(start: Vec<DirJob>, excludes: &ExcludeSet, progress: Option<&Progress<'_>>) -> Vec<Item> {
    let mut items = Vec::new();
    let mut frontier = start;
    while !frontier.is_empty() {
        let results: Vec<(Vec<Item>, Vec<DirJob>)> = frontier
            .par_iter()
            .map(|(dir, root, rel)| {
                let mut files = Vec::new();
                let mut dirs = Vec::new();
                let mut batch = 0usize;
                let Ok(rd) = std::fs::read_dir(dir) else { return (files, dirs) };
                for e in rd.flatten() {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with('.') {
                        continue;
                    }
                    let Ok(ft) = e.file_type() else { continue };
                    let path = e.path();
                    let child_rel = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
                    if !excludes.is_empty() && excludes.matches(&child_rel, &name) {
                        continue;
                    }
                    if ft.is_dir() {
                        dirs.push((path, *root, child_rel));
                    } else if ft.is_file() || (ft.is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_file())) {
                        let kind = FileKind::from_path(&path);
                        files.push(Item { path, root: *root, rel: child_rel, kind });
                        batch += 1;
                        if batch >= 64 {
                            if let Some(p) = progress {
                                p.add_files(batch);
                            }
                            batch = 0;
                        }
                    }
                }
                if let Some(p) = progress {
                    p.add_files(batch);
                }
                (files, dirs)
            })
            .collect();
        frontier = Vec::new();
        for (f, d) in results {
            items.extend(f);
            frontier.extend(d);
        }
    }
    items
}

fn scan_text(kind: FileKind, path: &Path, text: &str) -> NoteScan {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    match kind {
        FileKind::Typst => scan_typst(text),
        FileKind::Excalidraw if !FileKind::is_excalidraw_md(path) => NoteScan::default(),
        _ => scan_markdown(text),
    }
}

fn data_from_meta(m: &NoteMeta) -> NoteData {
    NoteData {
        mtime_ms: m.mtime_ms,
        size: m.size,
        scan: NoteScan {
            title: m.title.clone(),
            headings: m.headings.clone(),
            links: m.links.clone(),
            tags: m.tags.clone(),
            aliases: m.aliases.clone(),
            ..Default::default()
        },
    }
}

struct ComputeStats {
    read_any: bool,
    missed_or_new: bool,
    reused_notes: usize,
    all_notes_unchanged: bool,
}

fn roots_disjoint(roots: &[PathBuf]) -> bool {
    !roots.iter().enumerate().any(|(i, a)| roots.iter().enumerate().any(|(j, b)| i != j && a.starts_with(b)))
}

struct HydrateInfo {
    /// Note and non-note entries found in the cache file, before dropping those outside the roots or excluded.
    raw_notes: usize,
    raw_files: usize,
    corrupt: bool,
}

struct Hydrated {
    prepared: Vec<Prepared>,
    raw_notes: usize,
    raw_files: usize,
    corrupt: bool,
}

/// Clears `Shared::partial` when the filling phase ends, however it ends (also on a panic).
struct ClearPartial(Arc<Shared>);

impl Drop for ClearPartial {
    fn drop(&mut self) {
        self.0.partial.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct PubState {
    last_bump: Option<Instant>,
    dirty: bool,
    sections: usize,
}

/// The miss branch of `compute`: read and scan one note (empty scan above `MAX_NOTE_BYTES` or for raw `.excalidraw` JSON); `None` when unreadable.
fn read_scan(shared: &Shared, kind: FileKind, path: &Path, size: u64, read_any: &AtomicBool) -> Option<NoteScan> {
    if size > MAX_NOTE_BYTES || (kind == FileKind::Excalidraw && !FileKind::is_excalidraw_md(path)) {
        return Some(NoteScan::default());
    }
    shared.reads.fetch_add(1, Ordering::Relaxed);
    read_any.store(true, Ordering::Relaxed);
    let bytes = std::fs::read(path).ok()?;
    Some(scan_text(kind, path, &String::from_utf8_lossy(&bytes)))
}

/// A walked file ready to insert, notes read and parsed; `None` when it vanished or is unreadable.
fn load_item(shared: &Shared, item: Item, read_any: &AtomicBool) -> Option<Prepared> {
    let note = if item.kind.is_note() {
        let md = std::fs::metadata(&item.path).ok()?;
        let size = md.len();
        let scan = read_scan(shared, item.kind, &item.path, size, read_any)?;
        Some(NoteData { mtime_ms: mtime_ms_of(&md), size, scan })
    } else {
        None
    };
    Some(Prepared::new(item.path, item.root, item.rel, item.kind, note))
}

struct Worker {
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    roots: Vec<PathBuf>,
    excludes: ExcludeSet,
    pool: Arc<rayon::ThreadPool>,
    start_delay: Duration,
    disk_cache: Option<RawCache>,
    cache_file: Option<PathBuf>,
    overrides: FxHashSet<PathBuf>,
    notify: Arc<dyn Fn() + Send + Sync>,
    last_notify: Instant,
    pending_notify: bool,
}

impl Worker {
    fn run(mut self) {
        if !self.start_delay.is_zero() {
            std::thread::sleep(self.start_delay);
        }
        let mut cache_loaded = false;
        if let Some(f) = &self.cache_file {
            if let Some(c) = load_raw_cache(f) {
                self.disk_cache = Some(c);
                cache_loaded = true;
            }
        }
        // initial scan (counted in pending/scans_pending at spawn)
        let to_persist = self.full_scan(!cache_loaded);
        self.shared.scans_pending.fetch_sub(1, Ordering::SeqCst);
        self.fire();
        if let Some(f) = to_persist {
            let _ = persist_disk_cache(&self.pool, &f, &self.shared.index);
        }
        self.shared.pending.fetch_sub(1, Ordering::SeqCst);
        self.disk_cache = None;
        loop {
            let first = if self.pending_notify {
                let wait = NOTIFY_INTERVAL.saturating_sub(self.last_notify.elapsed());
                match self.rx.recv_timeout(wait) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => {
                        self.fire();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match self.rx.recv() {
                    Ok(c) => c,
                    Err(_) => break,
                }
            };
            let mut batch = vec![first];
            batch.extend(self.rx.try_iter());
            self.handle_batch(batch);
        }
    }

    fn fire(&mut self) {
        self.pending_notify = false;
        self.last_notify = Instant::now();
        (self.notify)();
    }

    fn mark_changed(&mut self) {
        if self.last_notify.elapsed() >= NOTIFY_INTERVAL {
            self.fire();
        } else {
            self.pending_notify = true;
        }
    }

    fn handle_batch(&mut self, batch: Vec<Cmd>) {
        let n = batch.len();
        let last_full = batch.iter().rposition(|c| matches!(c, Cmd::Rescan | Cmd::SetRoots(_)));
        let n_full = batch.iter().filter(|c| matches!(c, Cmd::Rescan | Cmd::SetRoots(_))).count();
        let mut changed = false;
        let mut to_persist = None;
        for (i, c) in batch.into_iter().enumerate() {
            match c {
                Cmd::SetRoots(r) => self.roots = r,
                Cmd::Rescan => {}
                Cmd::FileChanged(p) => {
                    if last_full.is_none_or(|l| i > l) {
                        changed |= self.file_changed(&p);
                    } else {
                        self.overrides.remove(&p);
                    }
                }
                Cmd::UpdateText(p, t) => changed |= self.update_text(&p, &t),
            }
            if last_full == Some(i) {
                to_persist = self.full_scan(false);
                changed = false;
            }
        }
        if changed {
            self.shared.index.write().bump();
            self.update_counts(None);
        }
        if n_full > 0 {
            self.shared.scans_pending.fetch_sub(n_full, Ordering::SeqCst);
        }
        if last_full.is_some() {
            self.fire();
        } else if changed {
            self.mark_changed();
        }
        if let Some(f) = to_persist {
            let _ = persist_disk_cache(&self.pool, &f, &self.shared.index);
        }
        self.shared.pending.fetch_sub(n, Ordering::SeqCst);
    }

    fn update_counts(&self, scan_ms: Option<f32>) {
        let (files, notes) = {
            let ix = self.shared.index.read();
            (ix.file_count() as u32, ix.len() as u32)
        };
        let mut s = self.shared.status.lock();
        s.files = files;
        s.notes = notes;
        if let Some(ms) = scan_ms {
            s.last_scan_ms = ms;
        }
    }

    /// Cached entries into `self.shared.index`, in short write sections while the pool decodes the next chunks.
    /// Entries outside `self.roots` or matching `self.excludes` are dropped. `Some` when a cache was loaded.
    fn hydrate_from_disk_cache(&mut self, progress: &Progress<'_>) -> Option<HydrateInfo> {
        let raw = self.disk_cache.take()?;
        let _partial = ClearPartial(self.shared.clone());
        let root_map: Vec<Option<u16>> = raw.roots.iter().map(|r| self.roots.iter().position(|c| c.to_string_lossy() == r.as_str()).map(|i| i as u16)).collect();
        let (roots, excludes, pool) = (self.roots.clone(), self.excludes.clone(), self.pool.clone());
        let decode = |range: std::ops::Range<usize>| -> Hydrated {
            let mut h = Hydrated { prepared: Vec::new(), raw_notes: 0, raw_files: 0, corrupt: false };
            let Ok(items) = postcard::from_bytes::<Vec<DiskItem>>(&raw.bytes[range]) else {
                h.corrupt = true;
                return h;
            };
            h.prepared.reserve(items.len());
            for it in items {
                if !plausible_rel(&it.rel) {
                    h.corrupt = true;
                    continue;
                }
                let kind = FileKind::from_path(Path::new(&it.rel));
                let note = match (kind.is_note(), it.note) {
                    (true, Some(n)) => {
                        h.raw_notes += 1;
                        Some(NoteData { mtime_ms: n.mtime_ms, size: n.size, scan: n.scan })
                    }
                    (false, None) => {
                        h.raw_files += 1;
                        None
                    }
                    _ => {
                        h.corrupt = true;
                        continue;
                    }
                };
                let Some(Some(root)) = root_map.get(it.root as usize).copied() else { continue };
                if !excludes.is_empty() && rel_excluded(&excludes, &it.rel) {
                    continue;
                }
                h.prepared.push(Prepared::new(join_rel(&roots[root as usize], &it.rel), root, it.rel, kind, note));
            }
            h
        };
        let mut info = HydrateInfo { raw_notes: 0, raw_files: 0, corrupt: false };
        let mut st = PubState::default();
        pipeline(&pool, raw.chunks.clone(), decode, |h, backlog| {
            info.raw_notes += h.raw_notes;
            info.raw_files += h.raw_files;
            info.corrupt |= h.corrupt;
            if !h.prepared.is_empty() {
                self.publish_section(h.prepared, raw.items, backlog, &mut st, progress);
            }
        });
        self.finish_publish(&st);
        Some(info)
    }

    /// Inserts `items` into the shared index in one write section (a few ms at most) and reports progress.
    /// The generation is bumped for the first section and then at most every `NOTIFY_INTERVAL`, so consumers
    /// that rebuild per generation are not hammered; `finish_publish` closes the last gap.
    fn publish_section(&mut self, items: Vec<Prepared>, expect: usize, backlog: bool, st: &mut PubState, progress: &Progress<'_>) {
        // Before the first note is visible, so nobody takes the first section for the whole index.
        self.shared.partial.store(true, Ordering::SeqCst);
        let th = Instant::now();
        let (files, notes, bumped) = {
            let mut g = self.shared.index.write();
            if st.sections == 0 {
                g.roots = self.roots.clone();
                g.reserve(expect);
            }
            for p in items {
                g.insert_prepared(p);
            }
            let bumped = st.last_bump.is_none_or(|t| t.elapsed() >= NOTIFY_INTERVAL);
            if bumped {
                g.bump();
            }
            (g.file_count() as u32, g.len() as u32, bumped)
        };
        let hold = th.elapsed();
        st.sections += 1;
        st.dirty = !bumped;
        if bumped {
            st.last_bump = Some(Instant::now());
        }
        {
            let mut s = self.shared.status.lock();
            s.scanning = true;
            s.files = files;
            s.notes = notes;
        }
        progress.set_floor(files as usize);
        if bumped {
            self.fire();
        }
        // Readers that poll with `try_read` (the UI) only get in between sections: when the next one is ready
        // anyway, leave the lock free for half as long as it was held instead of re-taking it at once.
        if backlog {
            std::thread::sleep(hold / 2);
        }
    }

    fn finish_publish(&mut self, st: &PubState) {
        if st.dirty {
            self.shared.index.write().bump();
            self.fire();
        }
    }

    /// Cold scan into an empty index: results appear section by section while the pool reads and parses the rest.
    fn scan_progressive(&mut self, items: Vec<Item>, progress: &Progress<'_>) -> ComputeStats {
        let (shared, pool) = (self.shared.clone(), self.pool.clone());
        let _partial = ClearPartial(self.shared.clone());
        let read_any = AtomicBool::new(false);
        let mut st = PubState::default();
        let mut any_note = false;
        let expect = items.len();
        pipeline(
            &pool,
            split_growing(items),
            |chunk| chunk.into_iter().filter_map(|item| load_item(&shared, item, &read_any)).collect::<Vec<Prepared>>(),
            |prepared, backlog| {
                any_note |= prepared.iter().any(|p| p.note.is_some());
                if !prepared.is_empty() {
                    self.publish_section(prepared, expect, backlog, &mut st, progress);
                }
            },
        );
        if st.sections == 0 {
            let mut g = self.shared.index.write();
            g.roots = self.roots.clone();
            g.bump();
        } else {
            self.finish_publish(&st);
        }
        ComputeStats { read_any: read_any.load(Ordering::Relaxed), missed_or_new: any_note, reused_notes: 0, all_notes_unchanged: false }
    }

    /// Stat + (maybe) read + scan the notes among `items`, in parallel, without holding any index lock across I/O.
    ///
    /// When `materialize_reused` is `false` and every walked note hits the index (with matching roots and no
    /// deletions), `stats.all_notes_unchanged` is set and reused note scans are not cloned into `Entry.note`.
    fn compute(&self, items: Vec<Item>, force_refresh: Option<&Path>, progress: Option<&Progress<'_>>, materialize_reused: bool) -> (Vec<Entry>, ComputeStats) {
        let read_any = AtomicBool::new(false);
        let missed_or_new = AtomicBool::new(false);
        let reused_notes = AtomicUsize::new(0);
        let mut entries: Vec<Entry> = {
            // Only this thread writes the index, so a read guard across the stats blocks nobody (readers share it).
            let guard = self.shared.index.read();
            let ix: &Index = &guard;
            let known = |p: &Path| ix.by_path(p).and_then(|id| ix.get(id)).map(|m| (m.mtime_ms, m.size));
            self.pool.install(|| {
                items
                    .into_par_iter()
                    .filter_map(|item| {
                        if !item.kind.is_note() {
                            return Some(Entry { item, note: None });
                        }
                        let forced = force_refresh == Some(item.path.as_path());
                        if !forced && self.overrides.contains(&item.path) && known(&item.path).is_some() {
                            reused_notes.fetch_add(1, Ordering::Relaxed);
                            return Some(Entry { item, note: None });
                        }
                        let md = std::fs::metadata(&item.path).ok()?;
                        let (mtime_ms, size) = (mtime_ms_of(&md), md.len());
                        if !forced && known(&item.path) == Some((mtime_ms, size)) {
                            reused_notes.fetch_add(1, Ordering::Relaxed);
                            if let Some(p) = progress {
                                p.maybe_notify();
                            }
                            return Some(Entry { item, note: None });
                        }
                        missed_or_new.store(true, Ordering::Relaxed);
                        let scan = read_scan(&self.shared, item.kind, &item.path, size, &read_any)?;
                        if let Some(p) = progress {
                            p.maybe_notify();
                        }
                        Some(Entry { item, note: Some(NoteData { mtime_ms, size, scan }) })
                    })
                    .collect()
            })
        };

        let read_any = read_any.load(Ordering::Relaxed);
        let missed_or_new = missed_or_new.load(Ordering::Relaxed);
        let reused_notes = reused_notes.load(Ordering::Relaxed);
        let mut all_notes_unchanged = false;

        let check_skip = !materialize_reused && !read_any && !missed_or_new && force_refresh.is_none();
        if reused_notes > 0 || check_skip {
            let index = self.shared.index.read();
            if check_skip && reused_notes == index.len() && index.roots() == self.roots.as_slice() && roots_disjoint(&self.roots) {
                all_notes_unchanged = true;
            } else if reused_notes > 0 {
                entries.retain_mut(|e| {
                    if e.item.kind.is_note() && e.note.is_none() {
                        if let Some(m) = index.by_path(&e.item.path).and_then(|id| index.get(id)) {
                            e.note = Some(data_from_meta(m));
                            return true;
                        }
                        return false;
                    }
                    true
                });
            }
        }

        (
            entries,
            ComputeStats {
                read_any,
                missed_or_new,
                reused_notes,
                all_notes_unchanged,
            },
        )
    }

    /// Reconciles non-note files (`Image` / `Other`) when all notes in `self.shared.index` are unchanged.
    /// Returns the note count and whether any file was added or removed.
    fn sync_non_notes(&self, entries: Vec<Entry>) -> (usize, bool) {
        let (need_sync, notes_len) = {
            let ix = self.shared.index.read();
            let existing_non_notes = ix.file_count().saturating_sub(ix.len());
            let ix_ref: &Index = &ix;
            let (walked_non_notes, all_match) = self.pool.install(|| {
                entries
                    .par_iter()
                    .filter(|e| !e.item.kind.is_note())
                    .map(|e| (1usize, ix_ref.files.get(&e.item.path).is_some_and(|f| f.root == e.item.root && f.rel == e.item.rel)))
                    .reduce(|| (0, true), |a, b| (a.0 + b.0, a.1 && b.1))
            });
            (!(all_match && walked_non_notes == existing_non_notes), ix.len())
        };
        if !need_sync {
            // Freeing 50k paths takes milliseconds: not on the scan's critical path.
            self.pool.spawn(move || drop(entries));
            return (notes_len, false);
        }
        // shortcut: one write section however many files changed; chunk it like `publish_section` if offline bulk adds of non-notes get large.
        let mut guard = self.shared.index.write();
        let existing_non_notes = guard.file_count().saturating_sub(guard.len());
        if existing_non_notes > 0 {
            let keep: FxHashSet<PathBuf> = entries.iter().filter(|e| !e.item.kind.is_note()).map(|e| e.item.path.clone()).collect();
            let stale: Vec<PathBuf> = guard.files.keys().filter(|p| !guard.by_path.contains_key(*p) && !keep.contains(*p)).cloned().collect();
            for p in stale {
                guard.remove(&p);
            }
        }
        for e in entries {
            if !e.item.kind.is_note() {
                let same = guard.files.get(&e.item.path).is_some_and(|f| f.root == e.item.root && f.rel == e.item.rel);
                if !same {
                    guard.insert(e.item.path, e.item.root, e.item.rel, e.item.kind, None);
                }
            }
        }
        guard.bump();
        (guard.len(), true)
    }

    fn full_scan(&mut self, force_persist: bool) -> Option<PathBuf> {
        let t0 = Instant::now();
        let (pool, shared, notify, excludes) = (self.pool.clone(), self.shared.clone(), self.notify.clone(), self.excludes.clone());
        let start: Vec<DirJob> = self.roots.iter().enumerate().map(|(i, r)| (r.clone(), i as u16, String::new())).collect();
        let files_before = shared.index.read().file_count();
        {
            let mut s = shared.status.lock();
            s.scanning = true;
            s.files = if files_before == 0 { 0 } else { s.files.max(files_before as u32) };
        }
        let progress = Progress::with_floor(&shared, &*notify, t0, files_before);
        // The walk only needs the file system, the hydration only the cache: run them side by side.
        let (info, items) = std::thread::scope(|sc| {
            let walk_all = || pool.install(|| walk(start.clone(), &excludes, Some(&progress)));
            let walker = std::thread::Builder::new().name("iron-sheets-walk".into()).spawn_scoped(sc, walk_all);
            let info = self.hydrate_from_disk_cache(&progress);
            let items = match walker {
                Ok(h) => h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)),
                Err(_) => walk_all(),
            };
            (info, items)
        });

        // An empty index (cold start, or nothing usable cached) is filled section by section; otherwise reconcile.
        let progressive = self.overrides.is_empty() && self.shared.index.read().file_count() == 0;
        let mut files_changed = false;
        let (stats, notes_changed, final_notes_len) = if progressive {
            let stats = self.scan_progressive(items, &progress);
            let n = self.shared.index.read().len();
            (stats, true, n)
        } else {
            let (entries, stats) = self.compute(items, None, Some(&progress), false);
            if stats.all_notes_unchanged {
                let (len, changed) = self.sync_non_notes(entries);
                files_changed = changed;
                (stats, false, len)
            } else {
                // Keep unsaved-text notes that vanished from the walk out of the index; build the new index off-lock.
                let mut fresh = Index::with_capacity(self.roots.clone(), entries.len());
                pipeline(
                    &pool,
                    split_growing(entries),
                    |chunk| chunk.into_iter().map(|e| Prepared::new(e.item.path, e.item.root, e.item.rel, e.item.kind, e.note)).collect::<Vec<_>>(),
                    |prepared, _| {
                        for p in prepared {
                            fresh.insert_prepared(p);
                        }
                    },
                );
                if !self.overrides.is_empty() {
                    self.overrides.retain(|p| fresh.files.contains_key(p));
                }
                let fresh_len = fresh.len();
                let (notes_changed, old) = {
                    let mut guard = self.shared.index.write();
                    let notes_changed = stats.read_any || stats.missed_or_new || guard.len() != fresh.len() || guard.roots() != fresh.roots();
                    fresh.generation = guard.generation + 1;
                    (notes_changed, std::mem::replace(&mut *guard, fresh))
                };
                self.pool.spawn(move || drop(old));
                (stats, notes_changed, fresh_len)
            }
        };

        self.update_counts(Some(t0.elapsed().as_secs_f32() * 1000.0));
        self.shared.status.lock().scanning = false;
        let final_files = self.shared.index.read().file_count();
        self.cache_file.clone().filter(|_| match &info {
            // On a warm initial scan the index was hydrated from the cache, so compare reused notes and file counts
            // with what was loaded: an unchanged `idx.bin` is not rewritten, offline deletions and filter changes are detected.
            Some(d) => {
                force_persist
                    || d.corrupt
                    || stats.read_any
                    || stats.missed_or_new
                    || stats.reused_notes != d.raw_notes
                    || final_notes_len != d.raw_notes
                    || files_changed
                    || final_files != d.raw_notes + d.raw_files
            }
            None => force_persist || notes_changed || files_changed || final_files != files_before,
        })
    }

    fn locate(&self, path: &Path) -> Option<(u16, String)> {
        let (i, root) = if self.roots.len() == 1 {
            let r = &self.roots[0];
            if !path.starts_with(r) {
                return None;
            }
            (0, r)
        } else {
            self.roots.iter().enumerate().filter(|(_, r)| path.starts_with(r)).max_by_key(|(_, r)| r.components().count())?
        };
        let rel = path.strip_prefix(root).ok()?;
        let mut s = String::with_capacity(rel.as_os_str().len());
        for c in rel.components() {
            let Component::Normal(n) = c else { return None };
            let n = n.to_string_lossy();
            if n.starts_with('.') {
                return None;
            }
            if !s.is_empty() {
                s.push('/');
            }
            s.push_str(&n);
            if !self.excludes.is_empty() && self.excludes.matches(&s, &n) {
                return None;
            }
        }
        if s.is_empty() {
            return None;
        }
        Some((i as u16, s))
    }

    /// Returns true when the index changed.
    fn file_changed(&mut self, path: &Path) -> bool {
        // Check root containment and excludes first so watcher bursts in ignored/outside paths do zero syscalls and zero locks.
        let (is_root, located) = if self.roots.iter().any(|r| r == path) {
            (true, None)
        } else {
            match self.locate(path) {
                Some(loc) => (false, Some(loc)),
                None => return false,
            }
        };
        let was_override = self.overrides.remove(path);
        let md = std::fs::symlink_metadata(path);
        let is_dir = md.as_ref().is_ok_and(|m| m.is_dir());
        if is_dir {
            return self.rescan_subtree(path);
        }
        let is_file = match &md {
            Ok(m) if m.is_file() => true,
            Ok(m) if m.file_type().is_symlink() => std::fs::metadata(path).is_ok_and(|m| m.is_file()),
            _ => false,
        };
        if !is_file {
            let under = {
                let ix = self.shared.index.read();
                if ix.files.contains_key(path) {
                    vec![path.to_path_buf()]
                } else {
                    ix.paths_under(path)
                }
            };
            if under.is_empty() {
                return false;
            }
            let mut ix = self.shared.index.write();
            let mut any = false;
            for p in under {
                any |= ix.remove(&p);
            }
            return any;
        }
        if is_root {
            return false;
        }
        let Some((root, rel)) = located else { return false };
        let kind = FileKind::from_path(path);
        let force_refresh = was_override.then_some(path);
        let (mut entries, _) = self.compute(vec![Item { path: path.to_path_buf(), root, rel, kind }], force_refresh, None, true);
        let Some(e) = entries.pop() else { return false };
        let mut ix = self.shared.index.write();
        ix.insert(e.item.path, e.item.root, e.item.rel, e.item.kind, e.note);
        true
    }

    fn rescan_subtree(&mut self, dir: &Path) -> bool {
        let Some((root, rel)) = self.roots.iter().position(|r| r == dir).map(|i| (i as u16, String::new())).or_else(|| self.locate(dir)) else {
            return false;
        };
        let items = self.pool.install(|| walk(vec![(dir.to_path_buf(), root, rel)], &self.excludes, None));
        let (entries, _) = self.compute(items, None, None, true);
        // Scan under the read lock: readers (UI queries) keep going during the ~16 ms scan of 50k files.
        let existing = self.shared.index.read().paths_under(dir);
        let mut ix = self.shared.index.write();
        let keep: FxHashSet<&Path> = entries.iter().map(|e| e.item.path.as_path()).collect();
        for p in existing {
            if !keep.contains(p.as_path()) {
                ix.remove(&p);
            }
        }
        drop(keep);
        for e in entries {
            ix.insert(e.item.path, e.item.root, e.item.rel, e.item.kind, e.note);
        }
        true
    }

    fn update_text(&mut self, path: &Path, text: &str) -> bool {
        let kind = FileKind::from_path(path);
        if !kind.is_note() {
            return false;
        }
        let Some((root, rel)) = self.locate(path) else { return false };
        let scan = scan_text(kind, path, text);
        let mut ix = self.shared.index.write();
        let mtime_ms = ix.by_path(path).and_then(|id| ix.get(id)).map_or(0, |m| m.mtime_ms);
        ix.insert(path.to_path_buf(), root, rel, kind, Some(NoteData { mtime_ms, size: text.len() as u64, scan }));
        drop(ix);
        self.overrides.insert(path.to_path_buf());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nd(scan: NoteScan) -> Option<NoteData> {
        Some(NoteData { mtime_ms: 1, size: 1, scan })
    }

    fn add(ix: &mut Index, root: &str, rel: &str, scan: NoteScan) -> NoteId {
        let roots = ix.roots.clone();
        let ri = roots.iter().position(|r| r == Path::new(root)).unwrap() as u16;
        let path = Path::new(root).join(rel);
        ix.insert(path.clone(), ri, rel.to_string(), FileKind::from_path(&path), nd(scan));
        ix.by_path(&path).unwrap()
    }

    fn sample() -> Index {
        let mut ix = Index::new();
        ix.set_roots(vec![PathBuf::from("/r1"), PathBuf::from("/r2")]);
        ix
    }

    fn mk() -> NoteScan {
        NoteScan::default()
    }

    #[test]
    fn resolve_basic_and_ext() {
        let mut ix = sample();
        let a = add(&mut ix, "/r1", "dir/Alpha.md", mk());
        let t = add(&mut ix, "/r1", "dir/Alpha.typ", mk());
        let b = add(&mut ix, "/r1", "Beta Note.md", NoteScan { aliases: vec!["Bee".into()], ..mk() });
        let v2 = add(&mut ix, "/r1", "x/Note.v2.md", mk());
        assert_eq!(ix.resolve("alpha", None), Some(a));
        assert_eq!(ix.resolve("Alpha.md", None), Some(a));
        assert_eq!(ix.resolve("Alpha.typ", None), Some(t));
        assert_eq!(ix.resolve("dir/alpha", None), Some(a));
        assert_eq!(ix.resolve("other/dir/alpha", None), Some(a)); // falls back to stem
        assert_eq!(ix.resolve("beta note", None), Some(b));
        assert_eq!(ix.resolve("BEE", None), Some(b));
        assert_eq!(ix.resolve("Note.v2", None), Some(v2));
        assert_eq!(ix.resolve("nothing", None), None);
        assert_eq!(ix.resolve("", None), None);
        assert_eq!(ix.resolve("ir/alpha", None), Some(a)); // stem fallback, not component suffix
    }

    #[test]
    fn resolve_tie_breaks() {
        let mut ix = sample();
        let far = add(&mut ix, "/r1", "a/b/c/Note.md", mk());
        let root_note = add(&mut ix, "/r1", "Note.md", mk());
        let other_root = add(&mut ix, "/r2", "Note.md", mk());
        let sib = add(&mut ix, "/r1", "x/Note.md", mk());
        // no context: fewest components, then lexicographic across roots (equal rel -> first inserted wins by id order)
        assert_eq!(ix.resolve("note", None), Some(root_note));
        // same directory wins
        assert_eq!(ix.resolve("note", Some(Path::new("/r1/x/other.md"))), Some(sib));
        assert_eq!(ix.resolve("note", Some(Path::new("/r1/a/b/c/z.md"))), Some(far));
        // same root next
        assert_eq!(ix.resolve("note", Some(Path::new("/r2/q/other.md"))), Some(other_root));
        assert_eq!(ix.resolve("note", Some(Path::new("/r1/q/other.md"))), Some(root_note));
        // path suffix disambiguates
        assert_eq!(ix.resolve("c/note", None), Some(far));
        // relative
        assert_eq!(ix.resolve("../Note", Some(Path::new("/r1/x/other.md"))), Some(root_note));
        assert_eq!(ix.resolve("./Note.md", Some(Path::new("/r1/x/other.md"))), Some(sib));
    }

    #[test]
    fn resolve_file_and_remove() {
        let mut ix = sample();
        let n = add(&mut ix, "/r1", "n.md", mk());
        ix.insert(PathBuf::from("/r1/img/Pic.PNG"), 0, "img/Pic.PNG".into(), FileKind::Image, None);
        ix.insert(PathBuf::from("/r2/Pic.png"), 1, "Pic.png".into(), FileKind::Image, None);
        ix.insert(PathBuf::from("/r1/doc.pdf"), 0, "doc.pdf".into(), FileKind::Other, None);
        assert_eq!(ix.resolve_file("pic.png", Some(Path::new("/r1/n.md"))), Some(PathBuf::from("/r1/img/Pic.PNG")));
        assert_eq!(ix.resolve_file("pic.png", Some(Path::new("/r2/n.md"))), Some(PathBuf::from("/r2/Pic.png")));
        assert_eq!(ix.resolve_file("img/pic.png", None), Some(PathBuf::from("/r1/img/Pic.PNG")));
        assert_eq!(ix.resolve_file("doc.pdf", None), Some(PathBuf::from("/r1/doc.pdf")));
        assert_eq!(ix.resolve_file("n", None), Some(PathBuf::from("/r1/n.md")));
        assert_eq!(ix.resolve_file("../doc.pdf", Some(Path::new("/r1/sub/n.md"))), Some(PathBuf::from("/r1/doc.pdf")));
        assert_eq!(ix.file_count(), 4);
        assert!(ix.remove(Path::new("/r1/n.md")));
        assert_eq!(ix.resolve("n", None), None);
        assert!(ix.get(n).is_none());
        assert_eq!(ix.len(), 0);
        assert!(!ix.remove(Path::new("/nope")));
    }

    #[test]
    fn backlinks_and_tags() {
        let mut ix = sample();
        let link = |t: &str, l: u32| OutLink { target: t.into(), line: l, embed: false };
        let a = add(&mut ix, "/r1", "a.md", NoteScan { links: vec![link("b", 3), link("a", 4), link("zzz", 5)], tags: vec!["x".into(), "y".into()], ..mk() });
        let b = add(&mut ix, "/r1", "b.md", NoteScan { links: vec![link("a.md", 1), link("b", 2)], tags: vec!["x".into()], ..mk() });
        let c = add(&mut ix, "/r1", "sub/c.md", NoteScan { links: vec![link("b", 7), link("b", 9)], ..mk() });
        ix.bump();
        assert_eq!(ix.backlinks(b), vec![Backlink { from: a, line: 3 }, Backlink { from: c, line: 7 }, Backlink { from: c, line: 9 }]);
        assert_eq!(ix.backlinks(a), vec![Backlink { from: b, line: 1 }]);
        assert!(ix.backlinks(c).is_empty());
        assert_eq!(ix.all_tags(), vec![("x".to_string(), 2), ("y".to_string(), 1)]);
        // cache invalidated by generation bump
        ix.remove(Path::new("/r1/sub/c.md"));
        ix.bump();
        assert_eq!(ix.backlinks(b).len(), 1);
    }

    fn write_notes(dir: &Path, n: usize) {
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for i in 0..n {
            let d = if i % 2 == 0 { dir.to_path_buf() } else { dir.join("sub") };
            std::fs::write(d.join(format!("note{i}.md")), format!("# Note {i}\n[[note{}]] #tag{}\n", (i + 1) % n, i % 3)).unwrap();
        }
        std::fs::write(dir.join("pic.png"), b"x").unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join(".hidden/secret.md"), "x").unwrap();
    }

    fn noop() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    #[test]
    fn indexer_scan_cache_reuse_and_changes() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 20);
        let cache = t.path().join("cache/idx.bin");
        let notified = Arc::new(AtomicUsize::new(0));
        let n2 = notified.clone();
        let ix = Indexer::spawn(
            vec![root.clone()],
            Some(cache.clone()),
            Arc::new(move || {
                n2.fetch_add(1, Ordering::Relaxed);
            }),
        );
        assert!(ix.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix.read_count(), 20);
        let st = ix.status();
        assert!(!st.scanning);
        assert_eq!((st.notes, st.files), (20, 21));
        assert!(notified.load(Ordering::Relaxed) >= 1);
        {
            let idx = ix.index();
            let g = idx.read();
            assert_eq!(g.len(), 20);
            assert!(g.resolve_file("pic.png", None).is_some());
            assert!(g.resolve("secret", None).is_none());
            let id = g.resolve("note5", None).unwrap();
            assert_eq!(g.get(id).unwrap().title.as_deref(), Some("Note 5"));
            assert_eq!(g.backlinks(id).len(), 1);
            assert_eq!(g.all_tags().len(), 3);
        }
        assert!(cache.exists());

        // rescan with unchanged files reads nothing
        ix.rescan();
        assert!(ix.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix.read_count(), 20);

        // a new indexer with the cache reads nothing either
        let ix2 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 0);
        assert_eq!(ix2.index().read().len(), 20);

        // modify one file (different size) -> exactly one read
        std::fs::write(root.join("note0.md"), "# Changed title\n[[note3]] extra\n").unwrap();
        ix2.rescan();
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 1);
        {
            let idx2 = ix2.index();
            let g = idx2.read();
            let id = g.by_path(&root.join("note0.md")).unwrap();
            assert_eq!(g.get(id).unwrap().title.as_deref(), Some("Changed title"));
        }

        // single-file change, delete, unsaved text, dir change
        let new = root.join("sub/fresh.md");
        std::fs::write(&new, "# Fresh\n[[note0]]\n").unwrap();
        let gen0 = ix2.index().read().generation();
        ix2.file_changed(new.clone());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.index().read().len(), 21);
        assert!(ix2.index().read().generation() > gen0);
        ix2.update_from_text(&new, "# Typing\n[[note1]]\n");
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        {
            let idx2 = ix2.index();
            let g = idx2.read();
            assert_eq!(g.get(g.by_path(&new).unwrap()).unwrap().title.as_deref(), Some("Typing"));
        }
        ix2.rescan(); // unsaved content survives rescans
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        {
            let idx2 = ix2.index();
            let g = idx2.read();
            assert_eq!(g.get(g.by_path(&new).unwrap()).unwrap().title.as_deref(), Some("Typing"));
        }
        ix2.file_changed(new.clone()); // back to disk version
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        {
            let idx2 = ix2.index();
            let g = idx2.read();
            assert_eq!(g.get(g.by_path(&new).unwrap()).unwrap().title.as_deref(), Some("Fresh"));
        }
        std::fs::remove_file(&new).unwrap();
        ix2.file_changed(new.clone());
        std::fs::remove_dir_all(root.join("sub")).unwrap();
        ix2.file_changed(root.join("sub"));
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.index().read().len(), 10);

        // roots change
        let other = t.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("o.typ"), "= Typst Title\n== Sec\n").unwrap();
        ix2.set_roots(vec![other.clone()]);
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        let idx2 = ix2.index();
        let g = idx2.read();
        assert_eq!(g.len(), 1);
        assert_eq!(g.notes().next().unwrap().headings.len(), 2);
    }

    #[test]
    fn corrupt_or_stale_cache_is_ignored() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 4);
        let cache = t.path().join("idx.bin");
        for junk in [b"".as_slice(), b"garbage", b"MDXIDX\0\x01\x07\0\0\0zzzz", b"MDXIDX\0\x01\x01\0\0\0\xff\xff\xff"] {
            std::fs::write(&cache, junk).unwrap();
            let ix = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
            assert!(ix.wait_idle(Duration::from_secs(10)));
            assert_eq!(ix.read_count(), 4);
            assert!(load_disk_cache(&cache).is_some(), "cache rewritten");
        }
        let ix = Indexer::spawn(vec![t.path().join("missing")], None, noop());
        assert!(ix.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix.index().read().len(), 0);
    }

    #[test]
    fn disk_cache_skips_unchanged_rewrite_and_detects_deletions() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..5 {
            std::fs::write(root.join(format!("note{i}.md")), format!("# Note {i}\n")).unwrap();
        }
        let cache = t.path().join("cache/idx.bin");

        // Build initial cache for 5 notes.
        let ix1 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix1.read_count(), 5);
        assert_eq!(ix1.index().read().len(), 5);
        assert_eq!(load_disk_cache(&cache).unwrap().len(), 5);
        drop(ix1);

        // Set a deterministic old timestamp on idx.bin; a 100% warm hit must not rewrite it.
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::OpenOptions::new().write(true).open(&cache).unwrap().set_modified(past).unwrap();
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);

        let ix2 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 0);
        assert_eq!(ix2.index().read().len(), 5);
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past, "unchanged warm cache must not rewrite idx.bin");
        ix2.rescan();
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past, "unchanged rescan must not rewrite idx.bin");
        drop(ix2);

        // Delete 1 note while offline -> new Indexer must detect deletion and update idx.bin to 4 notes.
        std::fs::remove_file(root.join("note4.md")).unwrap();
        let ix3 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix3.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix3.read_count(), 0);
        assert_eq!(ix3.index().read().len(), 4);
        assert_ne!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past, "deletion must update idx.bin");
        assert_eq!(load_disk_cache(&cache).unwrap().len(), 4);
        drop(ix3);

        // Next warm start on 4 notes hits cache and again skips rewriting idx.bin.
        std::fs::OpenOptions::new().write(true).open(&cache).unwrap().set_modified(past).unwrap();
        let ix4 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix4.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix4.read_count(), 0);
        assert_eq!(ix4.index().read().len(), 4);
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);
        drop(ix4);

        // Delete 1 + add 1 (count stays 4) must still detect miss and rewrite idx.bin.
        std::fs::remove_file(root.join("note3.md")).unwrap();
        std::fs::write(root.join("note5.md"), "# Note 5\n").unwrap();
        let ix5 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix5.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix5.read_count(), 1);
        assert_eq!(ix5.index().read().len(), 4);
        assert_ne!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);
        let cached = load_disk_cache(&cache).unwrap();
        assert_eq!(cached.len(), 4);
        assert!(cached.contains_key(&root.join("note5.md")));
        assert!(!cached.contains_key(&root.join("note3.md")));
    }

    #[test]
    fn resolve_normalization_and_edge_cases() {
        let mut ix = sample();
        let alpha = add(&mut ix, "/r1", "dir/Alpha.md", mk());
        let root_note = add(&mut ix, "/r1", "Note.md", mk());
        let sub_note = add(&mut ix, "/r1", "x/Note.md", mk());
        let v2 = add(&mut ix, "/r1", "x/Note.v2.md", mk());
        let deep_lc_rel = format!("{}/longtarget.md", "a".repeat(270));
        let deep_lc = add(&mut ix, "/r1", &deep_lc_rel, mk());
        let deep_mix_rel = format!("MixDir/{}/LongMix.md", "B".repeat(270));
        let deep_mix = add(&mut ix, "/r1", &deep_mix_rel, mk());
        let umlaut = add(&mut ix, "/r1", "café/Äpfel.md", mk());
        let turkish = add(&mut ix, "/r1", "tr/İ.md", mk());
        let cjk = add(&mut ix, "/r1", "jp/ノート.md", mk());
        ix.insert(PathBuf::from("/r1/img/Pic.PNG"), 0, "img/Pic.PNG".into(), FileKind::Image, None);
        ix.insert(PathBuf::from("/r1/doc.pdf"), 0, "doc.pdf".into(), FileKind::Other, None);
        ix.insert(PathBuf::from("/r1/café/Bild.PNG"), 0, "café/Bild.PNG".into(), FileKind::Image, None);

        // Multi-stem single-match vs multi-match tie-break (lazy TieCtx).
        assert_eq!(ix.resolve("x/Note", None), Some(sub_note));
        assert_eq!(ix.resolve("Note", Some(Path::new("/r1/x/other.md"))), Some(sub_note));
        assert_eq!(ix.resolve("Note", None), Some(root_note));

        // Mixed-case & backslash targets (<= 256 bytes stack path).
        assert_eq!(ix.resolve("ALPHA", None), Some(alpha));
        assert_eq!(ix.resolve("Dir\\Alpha", None), Some(alpha));
        assert_eq!(ix.resolve("dIr\\AlPhA.MD", None), Some(alpha));
        assert_eq!(ix.resolve("\\dir\\Alpha", None), Some(alpha));
        assert_eq!(ix.resolve("Note.V2", None), Some(v2));

        // Relative ./, ../, .\, ..\ paths with and without context.
        let from_x = Path::new("/r1/x/other.md");
        assert_eq!(ix.resolve("..\\Note", Some(from_x)), Some(root_note));
        assert_eq!(ix.resolve(".\\Note.md", Some(from_x)), Some(sub_note));
        assert_eq!(ix.resolve("./../Note", Some(from_x)), Some(root_note));
        assert_eq!(ix.resolve(".././Note", Some(from_x)), Some(root_note));
        assert_eq!(ix.resolve("./../dir/Alpha", Some(from_x)), Some(alpha));
        assert_eq!(ix.resolve("../dir\\Alpha", None), Some(alpha));
        assert_eq!(ix.resolve("./dir/Alpha", None), Some(alpha));
        assert_eq!(ix.resolve_file("..\\doc.pdf", Some(from_x)), Some(PathBuf::from("/r1/doc.pdf")));
        assert_eq!(ix.resolve_file(".\\Pic.PNG", Some(Path::new("/r1/img/a.md"))), Some(PathBuf::from("/r1/img/Pic.PNG")));

        // Empty / prefix-only targets resolve to None.
        for empty in ["", "   ", "/", "\\", "./", ".\\", "../", "..\\", "/./\\"] {
            assert_eq!(ix.resolve(empty, Some(from_x)), None, "target={empty:?}");
            assert_eq!(ix.resolve_file(empty, Some(from_x)), None, "file target={empty:?}");
        }

        // Long (> 256 byte) lowercase and mixed-case/backslash targets.
        let deep_lc_target = format!("{}/longtarget", "a".repeat(270));
        assert!(deep_lc_target.len() > 256);
        assert_eq!(ix.resolve(&deep_lc_target, None), Some(deep_lc));
        let deep_mix_target = format!("MixDir\\{}\\LongMix.MD", "B".repeat(270));
        assert!(deep_mix_target.len() > 256);
        assert_eq!(ix.resolve(&deep_mix_target, None), Some(deep_mix));

        // Non-ASCII Unicode targets (including backslash + length-changing case fold).
        assert_eq!(ix.resolve("äpfel", None), Some(umlaut));
        assert_eq!(ix.resolve("ÄPFEL", None), Some(umlaut));
        assert_eq!(ix.resolve("CAFÉ\\äpfel.md", None), Some(umlaut));
        assert_eq!(ix.resolve("İ", None), Some(turkish));
        assert_eq!(ix.resolve("tr\\İ.MD", None), Some(turkish));
        assert_eq!(ix.resolve("ノート", None), Some(cjk));
        assert_eq!(ix.resolve_file("CAFÉ\\bild.png", None), Some(PathBuf::from("/r1/café/Bild.PNG")));
    }

    #[test]
    fn file_changed_ignores_excluded_and_outside_paths_and_reverts_same_len_override() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let note = root.join("note.md");
        std::fs::write(&note, "# Fresh\n[[a]]\n").unwrap();
        std::fs::write(root.join("target/junk.md"), "# Ignored\n").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref\n").unwrap();
        std::fs::write(outside.join("ext.md"), "# Outside\n").unwrap();

        let excludes = ExcludeSet::new(&["target", "*.tmp"]);
        let ix = Indexer::spawn_with(vec![root.clone()], None, noop(), excludes);
        assert!(ix.wait_idle(Duration::from_secs(10)));
        let gen0 = ix.index().read().generation();
        let reads0 = ix.read_count();

        // Excluded, hidden, outside-root, and missing excluded paths must not bump generation or read files.
        ix.file_changed(root.join("target/junk.md"));
        ix.file_changed(root.join("target/deleted.tmp"));
        ix.file_changed(root.join("missing.tmp"));
        ix.file_changed(root.join(".git/HEAD"));
        ix.file_changed(outside.join("ext.md"));
        ix.file_changed(root.join("never_existed.md"));
        assert!(ix.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix.index().read().generation(), gen0);
        assert_eq!(ix.read_count(), reads0);

        // Same-length unsaved override must still revert to disk content when `file_changed` runs.
        ix.update_from_text(&note, "# Typin\n[[b]]\n");
        assert!(ix.wait_idle(Duration::from_secs(10)));
        {
            let idx = ix.index();
            let g = idx.read();
            let id = g.by_path(&note).unwrap();
            assert_eq!(g.get(id).unwrap().title.as_deref(), Some("Typin"));
        }
        ix.file_changed(note.clone());
        assert!(ix.wait_idle(Duration::from_secs(10)));
        {
            let idx = ix.index();
            let g = idx.read();
            let id = g.by_path(&note).unwrap();
            assert_eq!(g.get(id).unwrap().title.as_deref(), Some("Fresh"));
        }
    }

    #[test]
    fn progress_updates_files_and_throttles_notifications() {
        let shared = Arc::new(Shared {
            index: Arc::new(RwLock::new(Index::new())),
            status: Mutex::new(IndexStatus { scanning: true, ..Default::default() }),
            reads: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            scans_pending: AtomicUsize::new(1),
            partial: AtomicBool::new(false),
        });
        let hits = AtomicUsize::new(0);
        let notify = || {
            hits.fetch_add(1, Ordering::Relaxed);
        };
        let p = Progress::new(&shared, &notify, Instant::now() - Duration::from_millis(60));
        p.add_files(25);
        assert_eq!(hits.load(Ordering::Relaxed), 1);
        assert_eq!(shared.status().files, 25);

        // Immediate second batch within NOTIFY_INTERVAL is throttled (no duplicate notify).
        p.add_files(10);
        assert_eq!(hits.load(Ordering::Relaxed), 1);

        // With a hydrated floor, status.files never resets backwards while files_seen < floor.
        let shared2 = Arc::new(Shared {
            index: Arc::new(RwLock::new(Index::new())),
            status: Mutex::new(IndexStatus { scanning: true, files: 500, notes: 500, last_scan_ms: 0.0 }),
            reads: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            scans_pending: AtomicUsize::new(1),
            partial: AtomicBool::new(false),
        });
        let p2 = Progress::with_floor(&shared2, &notify, Instant::now() - Duration::from_millis(60), 500);
        p2.add_files(64);
        assert_eq!(shared2.status().files, 500, "files count must not dip below hydrated floor");
        p2.next_notify_ms.store(0, Ordering::Relaxed);
        p2.add_files(500);
        assert_eq!(shared2.status().files, 564, "files count advances once files_seen exceeds floor");
    }

    #[test]
    fn handle_observes_status_without_blocking_worker_shutdown() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 3);
        let ix = Indexer::spawn(vec![root.clone()], None, noop());
        assert!(ix.wait_idle(Duration::from_secs(10)));
        let handle = ix.handle();
        assert!(!handle.status().scanning);
        assert_eq!(handle.index().read().len(), 3);
        drop(ix);
        assert_eq!(handle.index().read().len(), 3);
    }

    struct Snap {
        status: IndexStatus,
        notes: usize,
        files: usize,
        generation: u64,
        paths: FxHashSet<PathBuf>,
        titles: FxHashMap<PathBuf, Option<String>>,
        resolved_note1: bool,
        backlinks_note1: usize,
    }

    fn spawn_recording(
        roots: Vec<PathBuf>,
        cache_file: Option<PathBuf>,
        excludes: ExcludeSet,
    ) -> (Indexer, Arc<std::sync::Mutex<Vec<Snap>>>) {
        let gate = Arc::new((std::sync::Mutex::new(None::<IndexHandle>), std::sync::Condvar::new()));
        let snaps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let g2 = gate.clone();
        let s2 = snaps.clone();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let handle = {
                let mut guard = g2.0.lock().unwrap();
                while guard.is_none() {
                    guard = g2.1.wait(guard).unwrap();
                }
                guard.clone().unwrap()
            };
            let status = handle.status();
            let ix = handle.index().read();
            let paths = ix.notes().map(|n| n.path.clone()).collect();
            let titles = ix.notes().map(|n| (n.path.clone(), n.title.clone())).collect();
            let resolved_note1 = ix.resolve("note1", None);
            let backlinks_note1 = resolved_note1.map_or(0, |id| ix.backlinks(id).len());
            s2.lock().unwrap().push(Snap {
                status,
                notes: ix.len(),
                files: ix.file_count(),
                generation: ix.generation(),
                paths,
                titles,
                resolved_note1: resolved_note1.is_some(),
                backlinks_note1,
            });
        });
        let ix = Indexer::spawn_with(roots, cache_file, notify, excludes);
        {
            let mut guard = gate.0.lock().unwrap();
            *guard = Some(ix.handle());
            gate.1.notify_all();
        }
        (ix, snaps)
    }

    #[test]
    fn instant_hydration_before_walk_and_no_redundant_bump_when_unchanged() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..5 {
            let body = if i == 0 {
                format!("# Note {i}\n[[note1]]\n")
            } else {
                format!("# Note {i}\n")
            };
            std::fs::write(root.join(format!("note{i}.md")), body).unwrap();
        }
        let cache = t.path().join("cache/idx.bin");

        let ix1 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix1.read_count(), 5);
        drop(ix1);

        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::OpenOptions::new().write(true).open(&cache).unwrap().set_modified(past).unwrap();

        // Warm start with notes only: hydrates immediately before walk finishes, no extra generation bump, no cache rewrite.
        let (ix2, snaps2) = spawn_recording(vec![root.clone()], Some(cache.clone()), ExcludeSet::default());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 0);
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);
        {
            let s = snaps2.lock().unwrap();
            assert!(s.len() >= 2, "expected hydration notify + completion notify, got {}", s.len());
            let first = &s[0];
            assert!(first.status.scanning);
            assert_eq!((first.status.files, first.status.notes), (5, 5));
            assert_eq!((first.notes, first.files, first.generation), (5, 5, 1));
            assert!(first.resolved_note1);
            assert_eq!(first.backlinks_note1, 1);

            let last = s.last().unwrap();
            assert!(!last.status.scanning);
            assert_eq!((last.status.files, last.status.notes), (5, 5));
            assert_eq!(last.generation, 1, "unchanged workspace must not bump generation again");
        }
        drop(ix2);

        // Warm start when a non-note file exists on disk: hydrates 5 notes while scanning, then adds pic.png cleanly.
        std::fs::write(root.join("pic.png"), b"png").unwrap();
        let (ix3, snaps3) = spawn_recording(vec![root.clone()], Some(cache.clone()), ExcludeSet::default());
        assert!(ix3.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix3.read_count(), 0);
        // The cache holds non-notes too, so a new image is a change to persist.
        assert_ne!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past, "a new non-note must update idx.bin");
        {
            let s = snaps3.lock().unwrap();
            assert!(s[0].status.scanning);
            assert_eq!(s[0].notes, 5);
            let last = s.last().unwrap();
            assert!(!last.status.scanning);
            assert_eq!((last.notes, last.files), (5, 6));
            assert_eq!(last.generation, 2);
        }
        assert!(ix3.index().read().resolve_file("pic.png", None).is_some());
        drop(ix3);

        // Warm start again: images come from the cache, nothing is re-inserted (generation stays) and idx.bin is left alone.
        std::fs::OpenOptions::new().write(true).open(&cache).unwrap().set_modified(past).unwrap();
        let (ix4, snaps4) = spawn_recording(vec![root.clone()], Some(cache.clone()), ExcludeSet::default());
        assert!(ix4.wait_idle(Duration::from_secs(10)));
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);
        let s = snaps4.lock().unwrap();
        assert_eq!((s[0].notes, s[0].files), (5, 6), "images are queryable right after hydration");
        assert_eq!(s.last().unwrap().generation, 1, "an unchanged tree, images included, is not touched again");
    }

    #[test]
    fn hydrated_index_equals_the_scanned_one() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 3 * SECTION + 7);
        std::fs::write(root.join("sub/aliased.md"), "---\naliases: [Other Name]\n---\n# A\n[[note3]] [[pic.png]]\n").unwrap();
        let cache = t.path().join("idx.bin");
        let snapshot = |ix: &Indexer| {
            let idx = ix.index();
            let g = idx.read();
            let notes: Vec<NoteMeta> = g.notes().cloned().collect();
            let mut files: Vec<(PathBuf, u16, String)> = g.files().map(|f| (f.path.to_path_buf(), f.root, f.rel.to_string())).collect();
            files.sort();
            let backlinks: Vec<Vec<Backlink>> = notes.iter().map(|n| g.backlinks(n.id)).collect();
            (notes, files, backlinks, g.all_tags(), g.resolve("other name", None), g.resolve_file("pic.png", None))
        };
        let cold = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(cold.wait_idle(Duration::from_secs(30)));
        let a = snapshot(&cold);
        drop(cold);
        let warm = Indexer::spawn(vec![root], Some(cache), noop());
        assert!(warm.wait_idle(Duration::from_secs(30)));
        assert_eq!(warm.read_count(), 0);
        let b = snapshot(&warm);
        assert!(a.0.len() > 3 * SECTION && a.4.is_some() && a.5.is_some());
        assert!(a == b, "a warm start must rebuild exactly what the cold scan built");
    }

    #[test]
    fn cache_of_another_version_falls_back_to_a_cold_scan() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 6);
        let cache = t.path().join("idx.bin");
        let ix1 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        drop(ix1);
        let mut bytes = std::fs::read(&cache).unwrap();
        bytes[CACHE_MAGIC.len()] = (CACHE_VERSION - 1) as u8;
        std::fs::write(&cache, &bytes).unwrap();
        let ix2 = Indexer::spawn(vec![root], Some(cache.clone()), noop());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 6, "an old cache is ignored, every note is read");
        assert_eq!(ix2.index().read().len(), 6);
        assert!(load_raw_cache(&cache).is_some(), "rewritten in the current version");
    }

    #[test]
    fn garbage_headers_and_escaping_paths_fall_back_to_a_cold_scan() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 6);
        let cache = t.path().join("idx.bin");
        let ix1 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        drop(ix1);
        let good = std::fs::read(&cache).unwrap();
        // Item count (bytes 16..20) far beyond the file: must not reach `reserve`.
        let mut huge = good.clone();
        huge[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&cache, &huge).unwrap();
        let ix2 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix2.wait_idle(Duration::from_secs(10)), "worker must survive a garbage header");
        assert_eq!((ix2.read_count(), ix2.index().read().len(), ix2.partial()), (6, 6, false));
        drop(ix2);
        // A path that climbs out of its root is corrupt: not hydrated, everything re-read.
        assert!(plausible_rel("a/b.md") && !plausible_rel("../x.md") && !plausible_rel("a//b.md") && !plausible_rel("/etc/x.md") && !plausible_rel(""));
        assert_eq!(plausible_rel("Meeting 10:30.md"), !cfg!(windows));
    }

    #[test]
    fn pipeline_hands_results_over_in_input_order() {
        let pool = build_pool().unwrap();
        let mut out = Vec::new();
        pipeline(
            &pool,
            (0..64u64).collect(),
            |i| {
                std::thread::sleep(Duration::from_micros((64 - i) * 50));
                i
            },
            |i, _| out.push(i),
        );
        assert_eq!(out, (0..64).collect::<Vec<_>>());
    }

    #[test]
    fn cold_scan_publishes_sections_before_it_finishes() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        write_notes(&root, 3 * SECTION);
        let (ix, snaps) = spawn_recording(vec![root], None, ExcludeSet::default());
        assert!(ix.wait_idle(Duration::from_secs(30)));
        let s = snaps.lock().unwrap();
        let first = &s[0];
        assert!(first.status.scanning && first.notes > 0 && first.notes < 3 * SECTION, "first publish holds {} of {} notes", first.notes, 3 * SECTION);
        assert_eq!(s.last().unwrap().notes, 3 * SECTION);
        assert!(!s.last().unwrap().status.scanning);
        assert!(s.last().unwrap().generation > first.generation, "the final state is announced under a newer generation");
    }

    #[test]
    fn hydration_reconciles_offline_additions_modifications_and_deletions() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..4 {
            std::fs::write(root.join(format!("n{i}.md")), format!("# Note {i}\n")).unwrap();
        }
        let cache = t.path().join("cache/idx.bin");
        let ix1 = Indexer::spawn(vec![root.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        drop(ix1);

        // Offline changes while closed: modify n0, delete n3, add n4.
        std::fs::write(root.join("n0.md"), "# Updated 0\nextra content\n").unwrap();
        std::fs::remove_file(root.join("n3.md")).unwrap();
        std::fs::write(root.join("n4.md"), "# Added 4\n").unwrap();

        let (ix2, snaps) = spawn_recording(vec![root.clone()], Some(cache.clone()), ExcludeSet::default());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 2);

        {
            let s = snaps.lock().unwrap();
            let first = &s[0];
            assert!(first.status.scanning);
            assert_eq!(first.notes, 4);
            assert_eq!(first.generation, 1);
            assert_eq!(first.titles.get(&root.join("n0.md")).and_then(|t| t.as_deref()), Some("Note 0"));
            assert!(first.paths.contains(&root.join("n3.md")));

            let last = s.last().unwrap();
            assert!(!last.status.scanning);
            assert_eq!(last.notes, 4);
            assert_eq!(last.generation, 2);
            assert_eq!(last.titles.get(&root.join("n0.md")).and_then(|t| t.as_deref()), Some("Updated 0"));
            assert!(!last.paths.contains(&root.join("n3.md")));
            assert_eq!(last.titles.get(&root.join("n4.md")).and_then(|t| t.as_deref()), Some("Added 4"));
        }

        let cached = load_disk_cache(&cache).unwrap();
        assert_eq!(cached.len(), 4);
        assert!(!cached.contains_key(&root.join("n3.md")));
        assert!(cached.contains_key(&root.join("n4.md")));
    }

    #[test]
    fn hydration_filters_changed_excludes_and_roots() {
        let t = tempfile::tempdir().unwrap();
        let r1 = t.path().join("r1");
        let r2 = t.path().join("r2");
        std::fs::create_dir_all(r1.join("drafts")).unwrap();
        std::fs::create_dir_all(&r2).unwrap();
        let keep = r1.join("keep.md");
        let draft = r1.join("drafts/wip.md");
        let bak = r1.join("temp.bak.md");
        let other = r2.join("other.md");
        std::fs::write(&keep, "# Keep\n").unwrap();
        std::fs::write(&draft, "# Draft\n").unwrap();
        std::fs::write(&bak, "# Bak\n").unwrap();
        std::fs::write(&other, "# Other\n").unwrap();

        let cache = t.path().join("cache/idx.bin");
        let ix1 = Indexer::spawn(vec![r1.clone(), r2.clone()], Some(cache.clone()), noop());
        assert!(ix1.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix1.index().read().len(), 4);
        drop(ix1);

        // Respawn with r2 removed and excludes hiding drafts/ + *.bak.md: stale entries must never hydrate.
        let excludes = ExcludeSet::new(&["drafts", "*.bak.md"]);
        let (ix2, snaps) = spawn_recording(vec![r1.clone()], Some(cache.clone()), excludes.clone());
        assert!(ix2.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix2.read_count(), 0);
        {
            let s = snaps.lock().unwrap();
            let first = &s[0];
            assert!(first.status.scanning);
            assert_eq!((first.notes, first.files, first.generation), (1, 1, 1));
            assert!(first.paths.contains(&keep));
            assert!(!first.paths.contains(&draft));
            assert!(!first.paths.contains(&bak));
            assert!(!first.paths.contains(&other));

            let last = s.last().unwrap();
            assert!(!last.status.scanning);
            assert_eq!((last.notes, last.generation), (1, 1));
        }
        assert_eq!(load_disk_cache(&cache).unwrap().len(), 1);
        drop(ix2);

        // Subsequent launch with the pruned cache does not rewrite idx.bin.
        let past = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::OpenOptions::new().write(true).open(&cache).unwrap().set_modified(past).unwrap();
        let ix3 = Indexer::spawn_with(vec![r1.clone()], Some(cache.clone()), noop(), excludes);
        assert!(ix3.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix3.read_count(), 0);
        assert_eq!(std::fs::metadata(&cache).unwrap().modified().unwrap(), past);
        drop(ix3);

        // If all cached entries are now excluded, nothing hydrates and cache updates to empty.
        let all_excluded = ExcludeSet::new(&["*.md"]);
        let (ix4, snaps4) = spawn_recording(vec![r1], Some(cache.clone()), all_excluded);
        assert!(ix4.wait_idle(Duration::from_secs(10)));
        assert_eq!(ix4.read_count(), 0);
        assert_eq!(ix4.index().read().len(), 0);
        {
            let s = snaps4.lock().unwrap();
            assert_eq!(s.len(), 1);
            assert!(!s[0].status.scanning);
            assert_eq!(s[0].notes, 0);
        }
        assert!(load_disk_cache(&cache).unwrap().is_empty());
    }

    #[test]
    fn paths_under_matches_whole_components_only() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        for d in ["a/sub", "ab"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in ["a/x.md", "a/sub/y.md", "ab/z.md", "a.md"] {
            std::fs::write(root.join(f), "# t\n").unwrap();
        }
        let ix = Indexer::spawn_with(vec![root.clone()], None, noop(), ExcludeSet::default());
        assert!(ix.wait_idle(Duration::from_secs(10)));
        let idx = ix.index();
        let g = idx.read();
        let mut got = g.paths_under(&root.join("a"));
        got.sort();
        assert_eq!(got, vec![root.join("a/sub/y.md"), root.join("a/x.md")]);
        assert_eq!(g.paths_under(&root).len(), 4);
        assert_eq!(g.paths_under(&root.join("a/")).len(), 2);
        assert!(g.paths_under(&root.join("nope")).is_empty());
    }

    #[cfg(feature = "excalidraw")]
    #[test]
    fn excalidraw_index_resolution_and_scan_skip() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("ws");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let raw_path = root.join("sub/Shape.excalidraw");
        let md_path = root.join("Diagram.excalidraw.md");
        // Raw JSON has hex colors (`#1e1e1e`) and `[[0,0]]` arrays — must not be scanned as markdown tags/links.
        std::fs::write(&raw_path, r##"{"type":"excalidraw","elements":[{"strokeColor":"#1e1e1e","points":[[0,0],[10,10]]}]}"##).unwrap();
        std::fs::write(&md_path, "---\nexcalidraw-plugin: parsed\n---\n# Excalidraw Data\n## Text Elements\nHello #arch [[Shape]]\n").unwrap();

        let ix = Indexer::spawn_with(vec![root.clone()], None, noop(), ExcludeSet::default());
        assert!(ix.wait_idle(Duration::from_secs(10)));
        // Only `.excalidraw.md` should be read from disk; raw `.excalidraw` skips disk read.
        assert_eq!(ix.read_count(), 1);

        let idx = ix.index();
        let g = idx.read();
        let raw_id = g.by_path(&raw_path).expect("raw excalidraw indexed as note");
        let md_id = g.by_path(&md_path).expect("excalidraw.md indexed as note");
        assert_eq!(g.get(raw_id).unwrap().stem, "Shape");
        assert_eq!(g.get(md_id).unwrap().stem, "Diagram");
        assert!(g.get(raw_id).unwrap().tags.is_empty());
        assert!(g.get(raw_id).unwrap().links.is_empty());
        assert_eq!(g.get(md_id).unwrap().tags, vec!["arch"]);

        // Stem, .excalidraw, .excalidraw.md, and relative resolution
        assert_eq!(g.resolve("Diagram", None), Some(md_id));
        assert_eq!(g.resolve("diagram.excalidraw", None), Some(md_id));
        assert_eq!(g.resolve("diagram.excalidraw.md", None), Some(md_id));
        assert_eq!(g.resolve("Shape", None), Some(raw_id));
        assert_eq!(g.resolve("shape.excalidraw", None), Some(raw_id));
        assert_eq!(g.resolve("./sub/Shape", Some(&md_path)), Some(raw_id));
        assert_eq!(g.resolve("./Diagram", Some(&root.join("note.md"))), Some(md_id));
        assert!(g.name_taken(&root.join("other/Diagram.excalidraw.md"), &FxHashSet::default()));
    }
}
