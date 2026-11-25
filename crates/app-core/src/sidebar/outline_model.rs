//! Outline model: headings of the active document, throttled rescans, cursor heading. No UI toolkit here.
use iron_sheets_core::FileKind;
use iron_sheets_core::links::{Heading, scan_markdown, scan_typst};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Minimum time between rescans of the same document while it is being edited.
pub const RESCAN_MIN: Duration = Duration::from_millis(150);

/// Result of `OutlineModel::update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Headings match the document version.
    Ready,
    /// A rescan is due after this long; the previous headings are still shown.
    Wait(Duration),
}

#[derive(Default)]
pub struct OutlineModel {
    /// Cache key: (document path, document version) of the last scan.
    key: Option<(PathBuf, u64)>,
    headings: Vec<Heading>,
    line_starts: Vec<usize>,
    scanned_at: Option<Instant>,
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
    v
}

impl OutlineModel {
    /// No document is active: forget the cache key (the next document is scanned at once).
    pub fn clear(&mut self) {
        self.key = None;
    }

    /// Rescans when `(path, version)` changed, at most every `RESCAN_MIN` for the same path.
    pub fn update(&mut self, path: &Path, version: u64, kind: FileKind, text: &str, now: Instant) -> Refresh {
        if self.key.as_ref().is_some_and(|k| k.0 == path && k.1 == version) {
            return Refresh::Ready;
        }
        let same_path = self.key.as_ref().is_some_and(|k| k.0 == path);
        if same_path {
            if let Some(t) = self.scanned_at {
                let el = now.saturating_duration_since(t);
                if el < RESCAN_MIN {
                    return Refresh::Wait(RESCAN_MIN);
                }
            }
        }
        let scan = match kind {
            FileKind::Typst => scan_typst(text),
            FileKind::Excalidraw if !FileKind::is_excalidraw_md(path) => Default::default(),
            _ => scan_markdown(text),
        };
        self.headings = scan.headings;
        self.line_starts = line_starts(text);
        self.key = Some((path.to_path_buf(), version));
        self.scanned_at = Some(now);
        Refresh::Ready
    }

    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    /// 0-based line containing byte offset `byte` of the scanned text.
    pub fn line_of(&self, byte: usize) -> u32 {
        self.line_starts.partition_point(|&s| s <= byte).saturating_sub(1) as u32
    }

    /// Index of the last heading at or before the cursor.
    pub fn current(&self, cursor_start: Option<usize>) -> Option<usize> {
        let line = self.line_of(cursor_start?);
        self.headings.iter().rposition(|h| h.line <= line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# One\n\ntext\n\n## Two\n\nmore\n\n### Three\n";

    #[test]
    fn headings_and_cursor() {
        let mut m = OutlineModel::default();
        let t0 = Instant::now();
        assert_eq!(m.update(Path::new("a.md"), 1, FileKind::Markdown, DOC, t0), Refresh::Ready);
        let h: Vec<(u8, &str, u32)> = m.headings().iter().map(|h| (h.level, h.text.as_str(), h.line)).collect();
        assert_eq!(h, vec![(1, "One", 0), (2, "Two", 4), (3, "Three", 8)]);
        assert_eq!(m.current(None), None);
        assert_eq!(m.current(Some(0)), Some(0));
        assert_eq!(m.current(Some(DOC.find("more").unwrap())), Some(1));
        assert_eq!(m.current(Some(DOC.len())), Some(2));
    }

    #[test]
    fn text_before_first_heading_has_no_current() {
        let mut m = OutlineModel::default();
        m.update(Path::new("a.md"), 1, FileKind::Markdown, "intro\n\n# H\n", Instant::now());
        assert_eq!(m.current(Some(2)), None);
        assert_eq!(m.current(Some(8)), Some(0));
    }

    #[test]
    fn typst_headings() {
        let mut m = OutlineModel::default();
        m.update(Path::new("a.typ"), 1, FileKind::Typst, "= Title\n\n== Sub\n", Instant::now());
        assert_eq!(m.headings().len(), 2);
        assert_eq!(m.headings()[1].level, 2);
    }

    #[test]
    fn rescan_is_throttled_per_path_and_cached_per_version() {
        let mut m = OutlineModel::default();
        let t0 = Instant::now();
        m.update(Path::new("a.md"), 1, FileKind::Markdown, "# A\n", t0);
        // Same key: nothing to do.
        assert_eq!(m.update(Path::new("a.md"), 1, FileKind::Markdown, "# A\n", t0), Refresh::Ready);
        // New version too soon: keeps the old headings.
        assert_eq!(m.update(Path::new("a.md"), 2, FileKind::Markdown, "# B\n", t0 + Duration::from_millis(10)), Refresh::Wait(RESCAN_MIN));
        assert_eq!(m.headings()[0].text, "A");
        // After the minimum interval it rescans.
        assert_eq!(m.update(Path::new("a.md"), 2, FileKind::Markdown, "# B\n", t0 + RESCAN_MIN), Refresh::Ready);
        assert_eq!(m.headings()[0].text, "B");
        // Another path is never throttled.
        assert_eq!(m.update(Path::new("b.md"), 1, FileKind::Markdown, "# C\n", t0 + RESCAN_MIN), Refresh::Ready);
        assert_eq!(m.headings()[0].text, "C");
    }

    #[test]
    fn clear_forces_a_scan() {
        let mut m = OutlineModel::default();
        let t0 = Instant::now();
        m.update(Path::new("a.md"), 1, FileKind::Markdown, "# A\n", t0);
        m.clear();
        assert_eq!(m.update(Path::new("a.md"), 2, FileKind::Markdown, "# B\n", t0), Refresh::Ready);
        assert_eq!(m.headings()[0].text, "B");
    }
}
