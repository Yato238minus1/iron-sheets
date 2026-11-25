//! `World` implementations: one for documents (project files + packages, with
//! mtime-validated caching) and one sandboxed for math snippets.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

use ecow::eco_format;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, World};
use typst_ide::IdeWorld;
use typst_kit::datetime::Time;
use typst_kit::files::FsRoot;

use crate::shared::{Fonts, Shared};

/// Per-document cache of files read from disk. Survives across compiles so
/// that unchanged files are not re-read or re-parsed, and changed sources are
/// re-parsed incrementally.
#[derive(Default)]
pub struct DocFiles {
    slots: Mutex<FxHashMap<FileId, Slot>>,
}

struct Slot {
    /// The compile epoch in which the slot was last validated.
    epoch: u64,
    stamp: Option<(SystemTime, u64)>,
    data: FileResult<Bytes>,
    source: Option<Source>,
    source_fresh: bool,
    /// Package files never change.
    immutable: bool,
}

impl Slot {
    fn new() -> Self {
        Slot { epoch: 0, stamp: None, data: Err(FileError::Other(None)), source: None, source_fresh: false, immutable: false }
    }
}

pub struct DocWorld {
    shared: Arc<Shared>,
    fonts: Arc<Fonts>,
    main: FileId,
    main_source: Source,
    root: Option<FsRoot>,
    files: Arc<DocFiles>,
    epoch: u64,
    now: Time,
}

impl DocWorld {
    pub fn new(shared: Arc<Shared>, fonts: Arc<Fonts>, main_source: Source, root: Option<PathBuf>, files: Arc<DocFiles>, epoch: u64) -> Self {
        DocWorld { shared, fonts, main: main_source.id(), main_source, root: root.map(FsRoot::new), files, epoch, now: Time::system() }
    }

    pub fn main_source(&self) -> &Source {
        &self.main_source
    }

    /// Display path of a file for diagnostics.
    pub fn display_path(&self, id: FileId) -> PathBuf {
        match id.root() {
            VirtualRoot::Project => match &self.root {
                Some(root) => id.vpath().realize(root.path()).unwrap_or_else(|_| PathBuf::from(id.vpath().get_without_slash())),
                None => PathBuf::from(id.vpath().get_without_slash()),
            },
            VirtualRoot::Package(spec) => PathBuf::from(format!("{spec}{}", id.vpath().get_with_slash())),
        }
    }

    fn refresh(&self, slot: &mut Slot, id: FileId) {
        if slot.epoch == self.epoch {
            return;
        }
        slot.epoch = self.epoch;
        if slot.immutable && slot.data.is_ok() {
            return;
        }
        let (root, immutable) = match id.root() {
            VirtualRoot::Project => match &self.root {
                Some(root) => (root.clone(), false),
                None => {
                    slot.data = Err(FileError::Other(Some(eco_format!("this document has no project folder, so it cannot read other files"))));
                    return;
                }
            },
            VirtualRoot::Package(spec) => match self.shared.packages.obtain(spec) {
                Ok(root) => (root, true),
                Err(err) => {
                    slot.data = Err(FileError::Package(err));
                    slot.stamp = None;
                    return;
                }
            },
        };
        let path = match root.resolve(id.vpath()) {
            Ok(path) => path,
            Err(err) => {
                slot.data = Err(err);
                return;
            }
        };
        slot.immutable = immutable;
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                slot.data = Err(FileError::from_io(err, &path));
                slot.stamp = None;
                slot.source_fresh = false;
                return;
            }
        };
        if meta.is_dir() {
            slot.data = Err(FileError::IsDirectory);
            slot.stamp = None;
            slot.source_fresh = false;
            return;
        }
        let stamp = meta.modified().ok().map(|m| (m, meta.len()));
        if slot.data.is_ok() && stamp.is_some() && slot.stamp == stamp {
            return;
        }
        slot.stamp = stamp;
        slot.source_fresh = false;
        slot.data = fs::read(&path).map(Bytes::new).map_err(|err| FileError::from_io(err, &path));
    }
}

impl World for DocWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.shared.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main {
            return Ok(self.main_source.clone());
        }
        let mut slots = self.files.slots.lock();
        let slot = slots.entry(id).or_insert_with(Slot::new);
        self.refresh(slot, id);
        let bytes = slot.data.clone()?;
        if !slot.source_fresh {
            const BOM: &[u8] = b"\xef\xbb\xbf";
            let raw: &[u8] = bytes.as_slice();
            let text = std::str::from_utf8(raw.strip_prefix(BOM).unwrap_or(raw)).map_err(|_| FileError::InvalidUtf8)?;
            match &mut slot.source {
                Some(source) => {
                    source.replace(text);
                }
                None => slot.source = Some(Source::new(id, text.to_string())),
            }
            slot.source_fresh = true;
        }
        slot.source.clone().ok_or(FileError::Other(None))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main {
            return Ok(Bytes::from_string(self.main_source.clone()));
        }
        let mut slots = self.files.slots.lock();
        let slot = slots.entry(id).or_insert_with(Slot::new);
        self.refresh(slot, id);
        slot.data.clone()
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.store.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.now.today(offset)
    }
}

impl IdeWorld for DocWorld {
    fn upcast(&self) -> &dyn World {
        self
    }
}

/// The id used for the main source of math snippets.
static MATH_ID: LazyLock<FileId> = LazyLock::new(|| {
    // A constant, valid path: this cannot fail.
    #[allow(clippy::expect_used)]
    let path = VirtualPath::new("math-snippet.typ").expect("constant path is valid");
    FileId::unique(RootedPath::new(VirtualRoot::Project, path))
});

/// A sandboxed world for math snippets: no project files, no packages.
pub struct MathWorld {
    shared: Arc<Shared>,
    fonts: Arc<Fonts>,
    source: Source,
}

impl MathWorld {
    pub fn new(shared: Arc<Shared>, text: String) -> Self {
        let fonts = shared.fonts();
        MathWorld { shared, fonts, source: Source::new(*MATH_ID, text) }
    }
}

impl World for MathWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.shared.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() { Ok(self.source.clone()) } else { Err(FileError::AccessDenied) }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.source.id() { Ok(Bytes::from_string(self.source.clone())) } else { Err(FileError::AccessDenied) }
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.store.font(index)
    }

    fn today(&self, _: Option<Duration>) -> Option<Datetime> {
        None
    }
}
