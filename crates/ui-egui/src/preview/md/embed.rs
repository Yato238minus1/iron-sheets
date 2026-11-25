//! `![[...]]` embeds: target syntax, the part of a note an embed shows, and the cache of parsed embedded notes.
use iron_sheets_app_core::docs::DocStore;
use iron_sheets_md::model::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Part {
    Whole,
    Heading(String),
    Block(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub part: Part,
}

/// `Note`, `Note#Heading`, `Note#^id`, with an optional `|alias` that is ignored.
pub fn parse_target(t: &str) -> Target {
    let t = t.split('|').next().unwrap_or(t);
    let (name, part) = match t.split_once('#') {
        None => (t, Part::Whole),
        Some((n, r)) => match r.trim().strip_prefix('^') {
            Some(id) => (n, Part::Block(id.to_string())),
            None if r.trim().is_empty() => (n, Part::Whole),
            None => (n, Part::Heading(r.trim().to_string())),
        },
    };
    Target { name: name.trim().to_string(), part }
}

/// Blocks of `doc` an embed shows: a heading up to the next heading of the same or a higher level, or the one
/// block whose text ends in `^id`. Empty when the heading or id does not exist.
pub fn slice(doc: &Document, part: &Part) -> Range<usize> {
    let n = doc.blocks.len();
    match part {
        Part::Whole => 0..n,
        Part::Heading(h) => {
            let Some(s) = doc.find_heading(h) else { return 0..0 };
            let BlockKind::Heading { level, .. } = &doc.blocks[s].kind else { return 0..0 };
            let e = (s + 1..n).find(|&j| matches!(&doc.blocks[j].kind, BlockKind::Heading { level: l, .. } if l <= level)).unwrap_or(n);
            s..e
        }
        Part::Block(id) => {
            let tag = format!("^{id}");
            let found = doc.blocks.iter().position(|b| match &b.kind {
                BlockKind::Paragraph(i) | BlockKind::Heading { content: i, .. } => i.text.trim_end().ends_with(&tag),
                _ => false,
            });
            found.map_or(0..0, |i| i..i + 1)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Inline,
    Collapsed,
    Link,
}

impl Mode {
    pub fn from_setting(s: &str) -> Mode {
        match s {
            "collapsed" => Mode::Collapsed,
            "link" => Mode::Link,
            _ => Mode::Inline,
        }
    }
}

/// Disk targets are looked at again at most this often after the index changed.
const RESCAN: Duration = Duration::from_millis(500);

/// Version of an open note, or modification time and size of the file (0 when it cannot be read).
fn stamp(path: &Path, open: &DocStore) -> u64 {
    match open.get(path) {
        Some(d) => d.version | 1 << 63,
        None => {
            let Ok(m) = std::fs::metadata(path) else { return 0 };
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
            ((t ^ m.len().rotate_left(40)) & !(1 << 63)).max(1)
        }
    }
}

#[derive(Default)]
pub struct EmbedCache {
    pub mode: Mode,
    pub max_depth: u32,
    /// Hash of the note being shown: embed layouts of different notes never share a cache entry.
    pub host: u64,
    /// Collapsed embeds (by block hash) the user opened.
    pub expanded: FxHashSet<u64>,
    docs: FxHashMap<PathBuf, (u64, Arc<Document>)>,
    /// Target of each embed block (by block hash and host); `None` when it names no note.
    targets: FxHashMap<u64, Option<PathBuf>>,
    /// Stamp of every target at its last check, and how often it changed: part of the cache key of its embeds only.
    watch: FxHashMap<PathBuf, (u64, u64)>,
    /// Changes when the index did (throttled): for embeds that name no note, a new file may resolve them.
    res_epoch: u64,
    last_gen: u64,
    last_scan: Option<Instant>,
}

impl EmbedCache {
    fn slot(&self, b: &Block) -> u64 {
        b.hash ^ self.host.rotate_left(17)
    }

    /// The hash a block's layout is cached under. For an embed it covers the settings (`mode`, `max_depth` are
    /// mixed in by the caller's frame), the host note, the expanded flag and the state of its own target.
    pub fn hash(&self, b: &Block) -> u64 {
        let slot = self.slot(b);
        let tracked = self.targets.get(&slot);
        // Of the image blocks only Excalidraw drawings are laid out like embeds (from their target's content).
        let embed_like = match &b.kind {
            BlockKind::Embed { .. } => true,
            BlockKind::Image { src, .. } => iron_sheets_core::FileKind::from_path(Path::new(src)) == iron_sheets_core::FileKind::Excalidraw,
            _ => false,
        };
        if !embed_like && tracked.is_none() {
            return b.hash;
        }
        // Odd for a registered target, even before: a layout from before registration never matches a later one.
        let epoch = match tracked {
            Some(Some(p)) => self.watch.get(p).map_or(0, |w| w.1) << 1 | 1,
            _ => self.res_epoch << 1,
        };
        let settings = (self.mode as u64) << 56 ^ (self.max_depth as u64) << 48;
        (b.hash ^ self.host.rotate_left(17) ^ settings ^ epoch.wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ self.expanded.contains(&b.hash) as u64
    }

    #[cfg(feature = "excalidraw")]
    pub fn stamp_of(&mut self, path: &Path, open: &DocStore) -> u64 {
        if let Some(d) = open.get(path) {
            return d.version | (1 << 63);
        }
        if let Some((s, _)) = self.watch.get(path) {
            return *s;
        }
        let s = stamp(path, open);
        self.watch.insert(path.to_path_buf(), (s, 0));
        s
    }

    /// Records what the embed block `b` of the shown note points at (called when it is laid out).
    pub fn register(&mut self, b: &Block, target: Option<&Path>, open: &DocStore) {
        let slot = self.slot(b);
        self.targets.insert(slot, target.map(Path::to_path_buf));
        if let Some(p) = target {
            self.watch.entry(p.to_path_buf()).or_insert_with(|| (stamp(p, open), 0));
        }
    }

    /// Once per frame: bumps the epoch of targets that changed. Open notes are compared by version every frame;
    /// files on disk only after the index changed (the watcher updates it), at most every `RESCAN`.
    /// Returns true when a later frame has to look again.
    pub fn refresh(&mut self, open: &DocStore, index_gen: u64) -> bool {
        let mut later = false;
        let mut scan = false;
        if index_gen != self.last_gen {
            if self.last_scan.is_none_or(|t| t.elapsed() >= RESCAN) {
                self.last_gen = index_gen;
                self.last_scan = Some(Instant::now());
                self.res_epoch += 1;
                scan = true;
            } else {
                later = true;
            }
        }
        for (p, w) in self.watch.iter_mut() {
            if scan || open.get(p).is_some() {
                let now = stamp(p, open);
                if now != w.0 {
                    *w = (now, w.1 + 1);
                }
            }
        }
        later
    }

    /// The parsed note: the open document if there is one, else the file; parsed again when its version or
    /// modification time changed. shortcut: parsed on the UI thread (3-6 ms for 100 KB), once per change.
    pub fn doc(&mut self, path: &Path, open: &DocStore) -> Option<Arc<Document>> {
        let stamp = stamp(path, open);
        if stamp == 0 {
            return None;
        }
        if let Some((_, d)) = self.docs.get(path).filter(|(s, _)| *s == stamp) {
            return Some(d.clone());
        }
        let text = match open.get(path) {
            Some(d) => d.text.clone(),
            None => iron_sheets_core::textio::load_text(path).ok()?.text,
        };
        let d = Arc::new(iron_sheets_md::parse(&text));
        if self.docs.len() > 64 {
            self.docs.clear();
        }
        self.docs.insert(path.to_path_buf(), (stamp, d.clone()));
        Some(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(parse_target("Note"), Target { name: "Note".into(), part: Part::Whole });
        assert_eq!(parse_target("Note#Head ing|x").part, Part::Heading("Head ing".into()));
        assert_eq!(parse_target("Note#^abc").part, Part::Block("abc".into()));
        assert_eq!(parse_target("Note#").part, Part::Whole);
    }

    #[test]
    fn slices() {
        let d = iron_sheets_md::parse("intro\n\n# A\n\na1\n\n## A1\n\ndeep\n\n# B\n\nb1\n\nlast ^id1\n");
        assert_eq!(slice(&d, &Part::Whole), 0..d.blocks.len());
        // From "# A" up to "# B": the sub-heading stays inside.
        assert_eq!(slice(&d, &Part::Heading("A".into())), 1..5);
        assert_eq!(slice(&d, &Part::Heading("A1".into())), 3..5);
        assert_eq!(slice(&d, &Part::Heading("B".into())), 5..8);
        assert_eq!(slice(&d, &Part::Heading("Nope".into())), 0..0);
        assert_eq!(slice(&d, &Part::Block("id1".into())), 7..8);
        assert_eq!(slice(&d, &Part::Block("zz".into())), 0..0);
    }

    #[test]
    fn unregistered_and_registered_epochs_never_share_a_key() {
        let d = iron_sheets_md::parse("![[E]]\n");
        let b = &d.blocks[0];
        let mut c = EmbedCache { res_epoch: 1, ..Default::default() };
        // Laid out before its target was registered (after the first index scan).
        let before = c.hash(b);
        // Registered, then the target changed once: the stale first layout must not be reused.
        let p = PathBuf::from("E.md");
        c.targets.insert(c.slot(b), Some(p.clone()));
        c.watch.insert(p, (7, 1));
        assert_ne!(c.hash(b), before);
    }

    #[cfg(feature = "excalidraw")]
    #[test]
    fn only_excalidraw_images_follow_the_index_epoch() {
        let d = iron_sheets_md::parse("![[pic.png]]

![[draw.excalidraw]]
");
        let mut c = EmbedCache { res_epoch: 1, ..Default::default() };
        let (pic, draw) = (c.hash(&d.blocks[0]), c.hash(&d.blocks[1]));
        c.res_epoch += 1; // an autosave or index change
        assert_eq!(c.hash(&d.blocks[0]), pic);
        assert_ne!(c.hash(&d.blocks[1]), draw);
    }
}
