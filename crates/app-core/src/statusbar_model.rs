//! Status bar model: text of every segment, cached so unchanged frames build nothing. No UI toolkit here.
use crate::docs::Doc;
use crate::state::StatusMsg;
use iron_sheets_core::index::IndexStatus;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Save state of the active document as shown in the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocBadge {
    ReadOnly,
    SaveFailed,
    /// A write of the document is on the background worker.
    Saving,
    Unsaved,
    Saved,
    /// An untitled note: kept in the app data folder until it is saved into a folder.
    Draft,
}

impl DocBadge {
    pub fn of(doc: &Doc) -> DocBadge {
        if doc.read_only {
            DocBadge::ReadOnly
        } else if doc.save_error.is_some() {
            DocBadge::SaveFailed
        } else if doc.is_dirty() {
            DocBadge::Unsaved
        } else {
            DocBadge::Saved
        }
    }

    /// Like `of`, but an untitled note that is not failing or read-only is a draft, whether autosave wrote it or not.
    pub fn of_note(doc: &Doc, untitled: bool) -> DocBadge {
        match DocBadge::of(doc) {
            DocBadge::Saved | DocBadge::Unsaved if untitled => DocBadge::Draft,
            b => b,
        }
    }

    /// A write in flight (`DocStore::is_saving`) replaces the dirty/saved/draft states; failure and read-only stay.
    pub fn saving(self, in_flight: bool) -> DocBadge {
        match self {
            DocBadge::Saved | DocBadge::Unsaved | DocBadge::Draft if in_flight => DocBadge::Saving,
            b => b,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            DocBadge::ReadOnly => "Read-only",
            DocBadge::SaveFailed => "Save failed",
            DocBadge::Saving => "Saving…",
            DocBadge::Unsaved => "Unsaved",
            DocBadge::Saved => "Saved",
            DocBadge::Draft => "Draft",
        }
    }

    pub fn is_error(self) -> bool {
        self == DocBadge::SaveFailed
    }
}

/// Text of the transient status message; errors carry a text cue as well as the colour.
pub fn message_text(m: &StatusMsg) -> Cow<'_, str> {
    if m.error { Cow::Owned(format!("Error: {}", m.text)) } else { Cow::Borrowed(m.text.as_str()) }
}

/// Persistent status segment while the OS watch limit is hit: label, and the tooltip with the next step.
pub const WATCH_LIMIT_LABEL: &str = "File watching limit reached";
pub const WATCH_LIMIT_TIP: &str =
    "Changes in some folders are not detected. Raise fs.inotify.max_user_watches (sysctl), then press Rescan.";

/// What the app is doing in the background that the user waits for, shown while no transient message is up. Most
/// important first: a clicked link, then lazy initialisation (`typst` is `Engine::warming`).
pub fn activity_text(link_pending: bool, typst: Option<&'static str>, syntax_loading: bool) -> Option<&'static str> {
    if link_pending {
        Some("Opening link…")
    } else if typst.is_some() {
        typst
    } else if syntax_loading {
        Some("Loading syntax highlighting…")
    } else {
        None
    }
}

/// 1-based (line, column in chars) of byte offset `cursor` in `text`.
pub fn line_col(text: &str, cursor: usize) -> (usize, usize) {
    let upto = &text[..crate::editor::floor_boundary(text, cursor)];
    // Chunks of 255 bytes summed in u8 vectorize: about 8x faster than a plain filter on multi-MB notes.
    let ln = upto.as_bytes().chunks(255).map(|c| c.iter().map(|&b| u8::from(b == b'\n')).sum::<u8>() as usize).sum::<usize>() + 1;
    let col = upto.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (ln, col)
}

#[derive(Default)]
pub struct StatusBarModel {
    /// Word/char counts are keyed by (path, document version).
    path: PathBuf,
    version: u64,
    words: usize,
    chars: usize,
    counts_text: String,
    /// Line/column is keyed by (cursor byte, document version).
    cursor_key: (usize, u64),
    ln_col: (usize, usize),
    pos_text: String,
    /// Index status text is keyed by the status itself.
    index_key: Option<IndexStatus>,
    index_text: String,
}

impl StatusBarModel {
    /// Brings word/char counts and line/column up to date for the active document. Recounts only when the path or
    /// version changed; recomputes the position only when the cursor or version changed.
    pub fn update(&mut self, path: &Path, doc: &Doc, cursor_start: Option<usize>) {
        let version = doc.version;
        if self.path != path || self.version != version {
            self.path = path.to_path_buf();
            self.version = version;
            (self.words, self.chars) = doc.counts();
            self.counts_text = format!("{} words, {} chars", self.words, self.chars);
            self.cursor_key = (usize::MAX, 0);
        }
        let cur = cursor_start.unwrap_or(0);
        if self.cursor_key != (cur, version) {
            self.cursor_key = (cur, version);
            self.ln_col = line_col(&doc.text, cur);
            self.pos_text = format!("Ln {}, Col {}", self.ln_col.0, self.ln_col.1);
        }
    }

    /// "Ln 3, Col 7".
    pub fn position_text(&self) -> &str {
        &self.pos_text
    }

    /// (line, column), both 1-based.
    pub fn ln_col(&self) -> (usize, usize) {
        self.ln_col
    }

    /// "12 words, 80 chars".
    pub fn counts_text(&self) -> &str {
        &self.counts_text
    }

    /// (words, chars) of the last updated document.
    pub fn counts(&self) -> (usize, usize) {
        (self.words, self.chars)
    }

    /// "indexing... (N files)" or "N notes".
    pub fn index_text(&mut self, st: IndexStatus) -> &str {
        if self.index_key != Some(st) {
            self.index_key = Some(st);
            self.index_text = if st.scanning { format!("indexing... ({} files)", st.files) } else { format!("{} notes", st.notes) };
        }
        &self.index_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::DocStore;

    fn doc(text: &str) -> (tempfile::TempDir, PathBuf, DocStore) {
        let d = tempfile::tempdir().expect("tempdir");
        let p = d.path().join("n.md");
        std::fs::write(&p, text).expect("write");
        let mut s = DocStore::default();
        s.open(&p).expect("open");
        (d, p, s)
    }

    #[test]
    fn counts_and_position() {
        let (_d, p, mut s) = doc("hello wörld\nsecond line here\n");
        let mut m = StatusBarModel::default();
        m.update(&p, s.get(&p).expect("doc"), Some(0));
        assert_eq!(m.counts_text(), "5 words, 29 chars");
        assert_eq!(m.position_text(), "Ln 1, Col 1");
        // Byte 14 is inside "second" (after "hello wörld\n" = 13 bytes + 1).
        m.update(&p, s.get(&p).expect("doc"), Some(14));
        assert_eq!(m.position_text(), "Ln 2, Col 2");
        // A cursor in the middle of "ö" (2 bytes) floors to its start: col 8.
        m.update(&p, s.get(&p).expect("doc"), Some(8));
        assert_eq!(m.ln_col(), (1, 8));
        // Edit: counts follow the version.
        s.get_mut(&p).expect("doc").apply_edit(0..0, "x ");
        m.update(&p, s.get(&p).expect("doc"), None);
        assert_eq!(m.counts(), (6, 31));
        assert_eq!(m.position_text(), "Ln 1, Col 1");
    }

    #[test]
    fn counts_are_cached_per_path_and_version() {
        let (_d, p, s) = doc("a b c");
        let mut m = StatusBarModel::default();
        m.update(&p, s.get(&p).expect("doc"), Some(0));
        let ptr = m.counts_text().as_ptr();
        m.update(&p, s.get(&p).expect("doc"), Some(0));
        assert_eq!(ptr, m.counts_text().as_ptr(), "no rebuild when nothing changed");
    }

    #[test]
    fn badges() {
        let (_d, p, mut s) = doc("x");
        assert_eq!(DocBadge::of(s.get(&p).expect("doc")), DocBadge::Saved);
        s.get_mut(&p).expect("doc").apply_edit(0..0, "y");
        assert_eq!(DocBadge::of(s.get(&p).expect("doc")), DocBadge::Unsaved);
        // An untitled note reads "Draft" whether autosave wrote it or not; errors still show.
        assert_eq!(DocBadge::of_note(s.get(&p).expect("doc"), true), DocBadge::Draft);
        assert_eq!(DocBadge::of_note(s.get(&p).expect("doc"), false), DocBadge::Unsaved);
        let b = DocBadge::of(s.get(&p).expect("doc"));
        assert_eq!(b.saving(true).text(), "Saving…");
        assert_eq!(b.saving(false), b);
        s.get_mut(&p).expect("doc").save_error = Some("nope".into());
        assert_eq!(DocBadge::of(s.get(&p).expect("doc")).saving(true), DocBadge::SaveFailed);
        let b = DocBadge::of(s.get(&p).expect("doc"));
        assert_eq!((b, b.text(), b.is_error()), (DocBadge::SaveFailed, "Save failed", true));
        s.get_mut(&p).expect("doc").read_only = true;
        assert_eq!(DocBadge::of(s.get(&p).expect("doc")).text(), "Read-only");
    }

    #[test]
    fn activity_priority() {
        assert_eq!(activity_text(false, None, false), None);
        assert_eq!(activity_text(false, None, true), Some("Loading syntax highlighting…"));
        assert_eq!(activity_text(false, Some("Loading Typst fonts…"), true), Some("Loading Typst fonts…"));
        assert_eq!(activity_text(true, Some("Loading Typst fonts…"), true), Some("Opening link…"));
    }

    #[test]
    fn index_and_message_text() {
        let mut m = StatusBarModel::default();
        assert_eq!(m.index_text(IndexStatus { scanning: true, files: 12, notes: 3, last_scan_ms: 0.0 }), "indexing... (12 files)");
        assert_eq!(m.index_text(IndexStatus { scanning: false, files: 12, notes: 3, last_scan_ms: 1.0 }), "3 notes");
        let now = std::time::Instant::now();
        let ok = StatusMsg { text: "saved".into(), error: false, until: now };
        let bad = StatusMsg { text: "boom".into(), error: true, until: now };
        assert_eq!(message_text(&ok), "saved");
        assert!(matches!(message_text(&ok), Cow::Borrowed(_)));
        assert_eq!(message_text(&bad), "Error: boom");
    }
}
