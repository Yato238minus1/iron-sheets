//! Text file loading/saving with CRLF and BOM preservation, atomic writes.
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

const BINARY_SNIFF_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

#[derive(Debug, Clone)]
pub struct LoadedText {
    /// LF line endings only, BOM stripped.
    pub text: String,
    pub line_ending: LineEnding,
    pub bom: bool,
    /// Invalid UTF-8 was replaced with U+FFFD.
    pub lossy: bool,
    pub mtime: Option<SystemTime>,
}

#[derive(Debug)]
pub enum BoundedLoadError {
    Io(io::Error),
    TooLarge { size: u64, limit: u64 },
    Binary,
}

impl fmt::Display for BoundedLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::TooLarge { .. } => f.write_str("file is too large to open"),
            Self::Binary => f.write_str("binary file, not opened"),
        }
    }
}

impl std::error::Error for BoundedLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::TooLarge { .. } | Self::Binary => None,
        }
    }
}

impl From<io::Error> for BoundedLoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<BoundedLoadError> for io::Error {
    fn from(e: BoundedLoadError) -> Self {
        match e {
            BoundedLoadError::Io(err) => err,
            BoundedLoadError::TooLarge { .. } => io::Error::other("file is too large to open"),
            BoundedLoadError::Binary => io::Error::other("binary file, not opened"),
        }
    }
}

fn initial_capacity(len: u64) -> usize {
    if len == 0 { 0 } else { usize::try_from(len.saturating_add(1)).unwrap_or(usize::MAX) }
}

pub fn load_text(path: &Path) -> io::Result<LoadedText> {
    let mut file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    let mtime = meta.modified().ok();
    let mut bytes = Vec::with_capacity(initial_capacity(meta.len()));
    file.read_to_end(&mut bytes)?;
    Ok(decode(bytes, mtime))
}

/// Opens `path` once, checks `max_bytes` and NUL bytes in the first 8 KiB, and decodes BOM/CRLF/UTF-8.
pub fn load_text_bounded(path: &Path, max_bytes: u64) -> Result<LoadedText, BoundedLoadError> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    load_text_from_file_bounded(file, &meta, max_bytes)
}

/// Reads an already-opened `file` (with its `meta`) up to `max_bytes`, rejecting directories, oversized files, and
/// files with a NUL byte in the first 8 KiB, then decodes BOM/CRLF/UTF-8.
pub fn load_text_from_file_bounded(
    file: std::fs::File,
    meta: &std::fs::Metadata,
    max_bytes: u64,
) -> Result<LoadedText, BoundedLoadError> {
    if meta.is_dir() {
        return Err(BoundedLoadError::Io(io::Error::from(io::ErrorKind::IsADirectory)));
    }
    let len = meta.len();
    if len > max_bytes {
        return Err(BoundedLoadError::TooLarge { size: len, limit: max_bytes });
    }
    let mtime = meta.modified().ok();
    let mut buf = Vec::with_capacity(initial_capacity(len));
    file.take(max_bytes.saturating_add(1)).read_to_end(&mut buf)?;
    if buf.len() as u64 > max_bytes {
        return Err(BoundedLoadError::TooLarge { size: buf.len() as u64, limit: max_bytes });
    }
    if memchr::memchr(0, &buf[..buf.len().min(BINARY_SNIFF_BYTES)]).is_some() {
        return Err(BoundedLoadError::Binary);
    }
    Ok(decode(buf, mtime))
}

pub(crate) fn decode(mut bytes: Vec<u8>, mtime: Option<SystemTime>) -> LoadedText {
    let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    if bom {
        bytes.drain(..3);
    }
    let (mut text, lossy) = match String::from_utf8(bytes) {
        Ok(s) => (s, false),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
    };
    let mut line_ending = LineEnding::Lf;
    if memchr::memmem::find(text.as_bytes(), b"\r\n").is_some() {
        line_ending = LineEnding::Crlf;
        text = text.replace("\r\n", "\n");
    }
    LoadedText { text, line_ending, bom, lossy, mtime }
}

/// Normalizes raw UTF-8/CRLF/BOM bytes to LF text; returns `None` if binary (NUL in first 8 KiB).
pub fn decode_text_lf(bytes: Vec<u8>) -> Option<String> {
    if memchr::memchr(0, &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)]).is_some() {
        return None;
    }
    Some(decode(bytes, None).text)
}

/// The line ending and BOM `load_text` would report for these raw bytes.
pub fn detect_style(bytes: &[u8]) -> (LineEnding, bool) {
    let ending = if memchr::memmem::find(bytes, b"\r\n").is_some() { LineEnding::Crlf } else { LineEnding::Lf };
    (ending, bytes.starts_with(&[0xEF, 0xBB, 0xBF]))
}

/// The bytes `save_text` writes: `text` (LF internally) with the given line ending and BOM flag.
pub fn encode_text(text: &str, ending: LineEnding, bom: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(text.len() + text.len() / 32 + 3);
    if bom {
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    match ending {
        LineEnding::Lf => out.extend_from_slice(text.as_bytes()),
        LineEnding::Crlf => {
            let b = text.as_bytes();
            let mut last = 0;
            for i in memchr::memchr_iter(b'\n', b) {
                out.extend_from_slice(&b[last..i]);
                if !(i > 0 && b[i - 1] == b'\r') {
                    out.push(b'\r');
                }
                out.push(b'\n');
                last = i + 1;
            }
            out.extend_from_slice(&b[last..]);
        }
    }
    out
}

/// Atomically write `text` (LF internally) using the given line ending and BOM flag.
/// Returns the new modification time if available.
pub fn save_text(path: &Path, text: &str, ending: LineEnding, bom: bool) -> io::Result<Option<SystemTime>> {
    atomic_write(path, &encode_text(text, ending, bom))?;
    Ok(std::fs::metadata(path).and_then(|m| m.modified()).ok())
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write to a temp file in the same directory, then rename over the target.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let res = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all().ok();
        drop(f);
        // Preserve permissions of an existing target.
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        // std::fs::rename replaces an existing file on Windows too (MOVEFILE_REPLACE_EXISTING).
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// `Untitled.md`, `Untitled 1.md`, ... first name that does not exist in `dir`.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let ext = ext.trim_start_matches('.');
    let make = |n: u32| {
        let mut name = String::from(stem);
        if n > 0 {
            name.push(' ');
            name.push_str(&n.to_string());
        }
        if !ext.is_empty() {
            name.push('.');
            name.push_str(ext);
        }
        dir.join(name)
    };
    let mut n = 0;
    loop {
        let p = make(n);
        if std::fs::symlink_metadata(&p).is_err() {
            return p;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_bom_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, b"\xEF\xBB\xBFone\r\ntwo\r\n\r\nthree").unwrap();
        let l = load_text(&p).unwrap();
        assert_eq!(l.text, "one\ntwo\n\nthree");
        assert_eq!(l.line_ending, LineEnding::Crlf);
        assert!(l.bom && !l.lossy && l.mtime.is_some());
        save_text(&p, &l.text, l.line_ending, l.bom).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"\xEF\xBB\xBFone\r\ntwo\r\n\r\nthree");
    }

    #[test]
    fn lf_and_lossy() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, b"a\nb\xFF\n").unwrap();
        let l = load_text(&p).unwrap();
        assert_eq!(l.line_ending, LineEnding::Lf);
        assert!(l.lossy && !l.bom);
        assert_eq!(l.text, "a\nb\u{FFFD}\n");
        assert!(load_text(&t.path().join("missing")).is_err());
    }

    #[test]
    fn atomic_save_over_existing() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.md");
        std::fs::write(&p, "old").unwrap();
        let m = save_text(&p, "new\ntext", LineEnding::Lf, false).unwrap();
        assert!(m.is_some());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\ntext");
        let names: Vec<_> = std::fs::read_dir(t.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no temp files left: {names:?}");
    }

    #[test]
    fn unique_names() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(t.path(), "Untitled", "md"), t.path().join("Untitled.md"));
        std::fs::write(t.path().join("Untitled.md"), "").unwrap();
        assert_eq!(unique_path(t.path(), "Untitled", "md"), t.path().join("Untitled 1.md"));
        std::fs::write(t.path().join("Untitled 1.md"), "").unwrap();
        assert_eq!(unique_path(t.path(), "Untitled", ".md"), t.path().join("Untitled 2.md"));
    }

    #[test]
    fn bounded_loads_utf8_bom_crlf_and_lossy() {
        let t = tempfile::tempdir().unwrap();

        let p_utf8 = t.path().join("utf8.md");
        std::fs::write(&p_utf8, "hello wörld\n").unwrap();
        let l = load_text_bounded(&p_utf8, 1024).unwrap();
        assert_eq!(l.text, "hello wörld\n");
        assert_eq!(l.line_ending, LineEnding::Lf);
        assert!(!l.bom && !l.lossy && l.mtime.is_some());

        let p_bom = t.path().join("bom.md");
        std::fs::write(&p_bom, b"\xEF\xBB\xBFone\r\ntwo\r\n").unwrap();
        let l = load_text_bounded(&p_bom, 1024).unwrap();
        assert_eq!(l.text, "one\ntwo\n");
        assert_eq!(l.line_ending, LineEnding::Crlf);
        assert!(l.bom && !l.lossy);

        let p_lossy = t.path().join("lossy.md");
        std::fs::write(&p_lossy, b"a\nb\xFF\n").unwrap();
        let l = load_text_bounded(&p_lossy, 1024).unwrap();
        assert_eq!(l.text, "a\nb\u{FFFD}\n");
        assert!(l.lossy && !l.bom);
        assert_eq!(l.text, load_text(&p_lossy).unwrap().text);

        let p_empty = t.path().join("empty.md");
        std::fs::write(&p_empty, b"").unwrap();
        let l = load_text_bounded(&p_empty, 0).unwrap();
        assert_eq!(l.text, "");
        assert!(!l.bom && !l.lossy);
    }

    #[test]
    fn bounded_binary_sniff_and_nul_past_8kb() {
        let t = tempfile::tempdir().unwrap();

        let p_bin0 = t.path().join("bin0.md");
        std::fs::write(&p_bin0, b"\0abc").unwrap();
        let err = load_text_bounded(&p_bin0, 1024).unwrap_err();
        assert!(matches!(err, BoundedLoadError::Binary));
        assert_eq!(io::Error::from(err).to_string(), "binary file, not opened");

        let p_bin_edge = t.path().join("bin_edge.md");
        let mut edge = vec![b'a'; BINARY_SNIFF_BYTES - 1];
        edge.push(0);
        std::fs::write(&p_bin_edge, &edge).unwrap();
        assert!(matches!(load_text_bounded(&p_bin_edge, 64 * 1024), Err(BoundedLoadError::Binary)));

        // NUL past 8 KiB with valid UTF-8 matches check_openable + load_text (opens non-lossy).
        let p_past_valid = t.path().join("past_valid.md");
        let mut valid = vec![b'a'; BINARY_SNIFF_BYTES];
        valid.extend_from_slice(b"\0tail");
        std::fs::write(&p_past_valid, &valid).unwrap();
        let bounded = load_text_bounded(&p_past_valid, 64 * 1024).unwrap();
        let plain = load_text(&p_past_valid).unwrap();
        assert!(!bounded.lossy);
        assert_eq!(bounded.text, plain.text);

        // NUL past 8 KiB with invalid UTF-8 matches check_openable + load_text (opens lossy).
        let p_past_lossy = t.path().join("past_lossy.md");
        let mut invalid = vec![b'a'; BINARY_SNIFF_BYTES];
        invalid.extend_from_slice(b"\0\xFFtail");
        std::fs::write(&p_past_lossy, &invalid).unwrap();
        let bounded = load_text_bounded(&p_past_lossy, 64 * 1024).unwrap();
        let plain = load_text(&p_past_lossy).unwrap();
        assert!(bounded.lossy);
        assert_eq!(bounded.text, plain.text);
    }

    #[test]
    fn bounded_oversized_missing_and_directory() {
        let t = tempfile::tempdir().unwrap();

        let p_exact = t.path().join("exact.md");
        std::fs::write(&p_exact, "0123456789abcdef").unwrap();
        assert_eq!(load_text_bounded(&p_exact, 16).unwrap().text, "0123456789abcdef");

        let p_big = t.path().join("big.md");
        std::fs::write(&p_big, "0123456789abcdefg").unwrap();
        let err = load_text_bounded(&p_big, 16).unwrap_err();
        assert!(matches!(err, BoundedLoadError::TooLarge { size: 17, limit: 16 }));
        assert_eq!(io::Error::from(err).to_string(), "file is too large to open");

        let err = load_text_bounded(&t.path().join("missing.md"), 1024).unwrap_err();
        assert!(matches!(err, BoundedLoadError::Io(ref e) if e.kind() == io::ErrorKind::NotFound));

        let dir = t.path().join("sub");
        std::fs::create_dir(&dir).unwrap();
        // Small max_bytes too: directory must still report Io error rather than TooLarge.
        let err = load_text_bounded(&dir, 0).unwrap_err();
        assert!(matches!(err, BoundedLoadError::Io(_)));
        #[cfg(unix)]
        assert!(matches!(err, BoundedLoadError::Io(ref e) if e.kind() == io::ErrorKind::IsADirectory));
    }
}
