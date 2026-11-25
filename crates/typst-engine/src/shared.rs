//! Process-wide Typst state: the standard library, the font book and the
//! package store. Built lazily on a worker thread, never on the UI thread.

use std::sync::Arc;

use parking_lot::RwLock;
use typst::text::{Font, FontBook, FontInfo};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt};
use typst_kit::fonts::{FontPath, FontStore};
use typst_kit::packages::SystemPackages;

/// A snapshot of the available fonts. Immutable; replaced as a whole when the
/// system fonts have been scanned.
pub struct Fonts {
    pub store: FontStore,
    /// Incremented every time the set of fonts changes.
    pub generation: u64,
}

impl Fonts {
    pub fn book(&self) -> &LazyHash<FontBook> {
        self.store.book()
    }
}

pub struct Shared {
    pub library: LazyHash<Library>,
    embedded: Vec<(Font, FontInfo)>,
    fonts: RwLock<Arc<Fonts>>,
    pub packages: SystemPackages,
}

#[cfg(feature = "packages")]
fn downloader() -> impl typst_kit::downloader::Downloader {
    typst_kit::downloader::SystemDownloader::new(concat!("iron-sheets-typst/", env!("CARGO_PKG_VERSION")))
}

/// Never reaches the network: a `@preview` package that is not on disk fails with this message in the diagnostics.
#[cfg(not(feature = "packages"))]
fn downloader() -> impl typst_kit::downloader::Downloader {
    struct Offline;
    impl typst_kit::downloader::Downloader for Offline {
        fn stream(&self, _: &dyn std::any::Any, _: &str) -> std::io::Result<(Option<usize>, Box<dyn std::io::Read>)> {
            Err(std::io::Error::other("package downloads are not compiled in (enable the `typst-packages` feature)"))
        }
    }
    Offline
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

impl Shared {
    pub fn new() -> Shared {
        // The library and the embedded fonts are independent: build them in
        // parallel, this is the bulk of the cold start.
        let (library, embedded) = std::thread::scope(|s| {
            let fonts = s.spawn(|| typst_kit::fonts::embedded().collect::<Vec<_>>());
            let library = LazyHash::new(Library::builder().build());
            (library, fonts.join().unwrap_or_default())
        });
        let mut store = FontStore::new();
        for (font, info) in &embedded {
            store.push((font.clone(), info.clone()));
        }
        let packages = SystemPackages::new(downloader());
        Shared { library, embedded, fonts: RwLock::new(Arc::new(Fonts { store, generation: 0 })), packages }
    }

    pub fn fonts(&self) -> Arc<Fonts> {
        self.fonts.read().clone()
    }

    /// Scans the system fonts. Slow: call from a background thread only. Needs no `Shared`, so it can
    /// run while the library is still being built.
    pub fn scan_system_fonts() -> Vec<(FontPath, FontInfo)> {
        typst_kit::fonts::system().collect()
    }

    /// Publishes a new font snapshot (embedded fonts first, so their indices are stable).
    pub fn install_system_fonts(&self, system: Vec<(FontPath, FontInfo)>) {
        let mut store = FontStore::new();
        for (font, info) in &self.embedded {
            store.push((font.clone(), info.clone()));
        }
        store.extend(system);
        let mut slot = self.fonts.write();
        let generation = slot.generation + 1;
        *slot = Arc::new(Fonts { store, generation });
    }

    /// `(family, style summary)` of the embedded fonts, in index order.
    pub fn embedded_font_infos(&self) -> Vec<FontInfo> {
        self.embedded.iter().map(|(_, i)| i.clone()).collect()
    }
}
