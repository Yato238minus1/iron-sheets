//! Parallel plain-text search over files.
use memchr::memmem;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 0-based line.
    pub line: u32,
    /// 0-based column in characters.
    pub col: u32,
    /// The line, or a window of about 240 chars around the match.
    pub line_text: String,
    /// Byte range of the match within `line_text`.
    pub match_range: Range<u32>,
}

const WINDOW_BEFORE: usize = 80;
const WINDOW_TOTAL: usize = 240;
const FIRST_CHUNK: usize = 8;
const MAX_FILE: u64 = 32 * 1024 * 1024;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Lowercase with one char -> one char; keeps `c` when lowercasing expands.
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

struct Matcher<'q> {
    query: &'q SearchQuery,
    needle: String,
    finder: memmem::Finder<'static>,
    needle_ascii: bool,
}

impl<'q> Matcher<'q> {
    fn new(query: &'q SearchQuery) -> Option<Self> {
        if query.text.is_empty() {
            return None;
        }
        let needle: String = if query.case_sensitive { query.text.clone() } else { query.text.chars().map(fold_char).collect() };
        let finder = memmem::Finder::new(needle.as_bytes()).into_owned();
        let needle_ascii = needle.is_ascii();
        Some(Matcher { query, needle, finder, needle_ascii })
    }

    fn word_ok(&self, text: &str, s: usize, e: usize) -> bool {
        if !self.query.whole_word {
            return true;
        }
        let first = text[s..e].chars().next();
        let last = text[s..e].chars().next_back();
        if first.is_some_and(is_word) && text[..s].chars().next_back().is_some_and(is_word) {
            return false;
        }
        if last.is_some_and(is_word) && text[e..].chars().next().is_some_and(is_word) {
            return false;
        }
        true
    }

    /// Find all matches in `text` as byte ranges (original offsets).
    fn find_all(&self, text: &str, fold: &mut FoldScratch, mut f: impl FnMut(usize, usize) -> bool) {
        if self.query.case_sensitive {
            for s in self.finder.find_iter(text.as_bytes()) {
                let e = s + self.needle.len();
                if self.word_ok(text, s, e) && !f(s, e) {
                    return;
                }
            }
            return;
        }
        if text.is_ascii() {
            if !self.needle_ascii {
                return;
            }
            fold.buf.clear();
            fold.buf.extend_from_slice(text.as_bytes());
            fold.buf.make_ascii_lowercase();
            for s in self.finder.find_iter(&fold.buf) {
                let e = s + self.needle.len();
                if self.word_ok(text, s, e) && !f(s, e) {
                    return;
                }
            }
            return;
        }
        // Non-ASCII text: fold per char; if byte lengths are preserved offsets map 1:1.
        fold.folded.clear();
        fold.folded.reserve(text.len());
        let mut same_len = true;
        for c in text.chars() {
            let fc = fold_char(c);
            same_len &= fc.len_utf8() == c.len_utf8();
            fold.folded.push(fc);
        }
        if same_len {
            for s in self.finder.find_iter(fold.folded.as_bytes()) {
                let e = s + self.needle.len();
                if text.is_char_boundary(s) && text.is_char_boundary(e) && self.word_ok(text, s, e) && !f(s, e) {
                    return;
                }
            }
            return;
        }
        // Slow path: map folded offsets back to original offsets.
        fold.map.clear();
        fold.map.reserve(fold.folded.len() + 1);
        for (i, c) in text.char_indices() {
            for _ in 0..fold_char(c).len_utf8() {
                fold.map.push(i);
            }
        }
        fold.map.push(text.len());
        let mut from = 0;
        while let Some(p) = self.finder.find(&fold.folded.as_bytes()[from..]) {
            let fs = from + p;
            let fe = fs + self.needle.len();
            from = fs + 1;
            while from < fold.folded.len() && !fold.folded.is_char_boundary(from) {
                from += 1;
            }
            if fe >= fold.map.len() || !fold.folded.is_char_boundary(fs) || !fold.folded.is_char_boundary(fe) {
                continue;
            }
            let (s, e) = (fold.map[fs], fold.map[fe]);
            if self.word_ok(text, s, e) && !f(s, e) {
                return;
            }
        }
    }
}

#[derive(Default)]
struct FoldScratch {
    buf: Vec<u8>,
    folded: String,
    map: Vec<usize>,
}

#[derive(Default)]
struct SearchScratch {
    bytes: Vec<u8>,
    fold: FoldScratch,
}

fn make_hit(path: &std::path::Path, text: &str, line_no: u32, line_start: usize, s: usize, e: usize) -> SearchHit {
    let line_end = memchr::memchr(b'\n', &text.as_bytes()[line_start..]).map_or(text.len(), |p| line_start + p);
    let mut line = &text[line_start..line_end];
    let e = e.min(line_end);
    if let Some(l) = line.strip_suffix('\r') {
        line = l;
    }
    let (ms, me) = (s - line_start, (e - line_start).min(line.len()));
    let col = line[..ms].chars().count() as u32;
    let (mut ws, mut we) = (0, line.len());
    if line.chars().count() > WINDOW_TOTAL {
        let mut st = line[..ms].char_indices().rev().nth(WINDOW_BEFORE).map_or(0, |(i, _)| i);
        while st > 0 && !line.is_char_boundary(st) {
            st -= 1;
        }
        let en = line[st..].char_indices().nth(WINDOW_TOTAL).map_or(line.len(), |(i, _)| st + i).max(me);
        ws = st;
        we = en;
    }
    SearchHit { path: path.to_path_buf(), line: line_no, col, line_text: line[ws..we].to_string(), match_range: (ms - ws) as u32..(me - ws) as u32 }
}

/// Unsaved text of open documents, by path: searched instead of the file on disk.
pub type OpenTexts = HashMap<PathBuf, String>;

fn search_one(path: &Path, m: &Matcher, cancel: &AtomicBool, max_hits: usize, open: &OpenTexts, scratch: &mut SearchScratch) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    if cancel.load(Ordering::Relaxed) {
        return hits;
    }
    let text = if let Some(t) = open.get(path) {
        std::borrow::Cow::Borrowed(t.as_str())
    } else {
        let Ok(mut file) = std::fs::File::open(path) else { return hits };
        match file.metadata() {
            Ok(md) if md.is_file() && md.len() <= MAX_FILE => {
                scratch.bytes.clear();
                scratch.bytes.reserve((md.len() as usize).saturating_add(1));
            }
            _ => return hits,
        }
        if std::io::Read::read_to_end(&mut file, &mut scratch.bytes).is_err() {
            return hits;
        }
        if memchr::memchr(0, &scratch.bytes[..scratch.bytes.len().min(8192)]).is_some() {
            return hits; // binary
        }
        String::from_utf8_lossy(&scratch.bytes)
    };
    let t: &str = &text;
    let (mut line_no, mut line_start, mut pos) = (0u32, 0usize, 0usize);
    m.find_all(t, &mut scratch.fold, |s, e| {
        for nl in memchr::memchr_iter(b'\n', &t.as_bytes()[pos..s]) {
            line_no += 1;
            line_start = pos + nl + 1;
        }
        pos = s;
        hits.push(make_hit(path, t, line_no, line_start, s, e));
        hits.len() < max_hits
    });
    hits
}

/// Search `paths` in order; results are ordered by path position, then line.
pub fn search_files(paths: &[PathBuf], query: &SearchQuery, cancel: &AtomicBool, max_hits: usize) -> Vec<SearchHit> {
    let mut out = Vec::new();
    search_chunks(paths, &OpenTexts::new(), query, cancel, max_hits, |h| {
        out.extend(h);
    });
    out.truncate(max_hits);
    out
}

fn search_chunks(paths: &[PathBuf], open: &OpenTexts, query: &SearchQuery, cancel: &AtomicBool, max_hits: usize, mut sink: impl FnMut(Vec<SearchHit>)) {
    let Some(m) = Matcher::new(query) else { return };
    if max_hits == 0 {
        return;
    }
    // The first chunk is small so one slow file delays only a few others; chunks then double up to a size that keeps all threads busy.
    let max_chunk = (rayon::current_num_threads() * 8).max(16);
    let (mut chunk, mut rest) = (FIRST_CHUNK.min(max_chunk), paths);
    let mut total = 0usize;
    while !rest.is_empty() {
        let (c, tail) = rest.split_at(chunk.min(rest.len()));
        rest = tail;
        chunk = (chunk * 2).min(max_chunk);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let per: Vec<Vec<SearchHit>> =
            c.par_iter().map_init(SearchScratch::default, |scratch, p| search_one(p, &m, cancel, max_hits - total.min(max_hits), open, scratch)).collect();
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let mut batch = Vec::new();
        for h in per {
            for hit in h {
                if total >= max_hits {
                    break;
                }
                total += 1;
                batch.push(hit);
            }
        }
        if !batch.is_empty() {
            sink(batch);
        }
        if total >= max_hits {
            return;
        }
    }
}

/// Background search that delivers hits incrementally.
pub struct SearchJob {
    hits: Arc<Mutex<Vec<SearchHit>>>,
    done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl SearchJob {
    /// `open` holds the text of unsaved open documents; it is searched instead of the file on disk.
    pub fn start(paths: Vec<PathBuf>, open: OpenTexts, query: SearchQuery, max_hits: usize, notify: Arc<dyn Fn() + Send + Sync>) -> SearchJob {
        let hits = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let (h, d, c) = (hits.clone(), done.clone(), cancel.clone());
        let spawned = std::thread::Builder::new().name("iron-sheets-search".into()).spawn(move || {
            search_chunks(&paths, &open, &query, &c, max_hits, |batch| {
                h.lock().extend(batch);
                notify();
            });
            d.store(true, Ordering::Release);
            notify();
        });
        if spawned.is_err() {
            done.store(true, Ordering::Release);
        }
        SearchJob { hits, done, cancel }
    }

    /// New hits since the last poll.
    pub fn poll(&self) -> Vec<SearchHit> {
        std::mem::take(&mut *self.hits.lock())
    }

    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

impl Drop for SearchJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(t: &str, cs: bool, ww: bool) -> SearchQuery {
        SearchQuery { text: t.into(), case_sensitive: cs, whole_word: ww }
    }

    fn setup() -> (tempfile::TempDir, Vec<PathBuf>) {
        let t = tempfile::tempdir().unwrap();
        let files = [
            ("a.md", "Hello World\nsecond line hello\r\nthird"),
            ("b.md", "no match here\nÄpfel und äpfel\nHELLO hello Hello"),
            ("c.md", "helloworld hello_x hello"),
        ];
        let mut v = vec![];
        for (n, c) in files {
            let p = t.path().join(n);
            std::fs::write(&p, c).unwrap();
            v.push(p);
        }
        std::fs::write(t.path().join("bin.dat"), b"hello\0bin").unwrap();
        v.push(t.path().join("bin.dat"));
        v.push(t.path().join("missing.md"));
        (t, v)
    }

    #[test]
    fn case_insensitive_ordered() {
        let (_t, v) = setup();
        let no = AtomicBool::new(false);
        let h = search_files(&v, &q("hello", false, false), &no, 100);
        let got: Vec<_> = h.iter().map(|x| (x.path.file_name().unwrap().to_str().unwrap(), x.line, x.col)).collect();
        assert_eq!(
            got,
            vec![("a.md", 0, 0), ("a.md", 1, 12), ("b.md", 2, 0), ("b.md", 2, 6), ("b.md", 2, 12), ("c.md", 0, 0), ("c.md", 0, 11), ("c.md", 0, 19)]
        );
        let x = &h[1];
        assert_eq!(x.line_text, "second line hello");
        assert_eq!(&x.line_text[x.match_range.start as usize..x.match_range.end as usize], "hello");
    }

    #[test]
    fn case_sensitive_and_word() {
        let (_t, v) = setup();
        let no = AtomicBool::new(false);
        assert_eq!(search_files(&v, &q("Hello", true, false), &no, 100).len(), 2);
        let w = search_files(&v, &q("hello", false, true), &no, 100);
        assert_eq!(w.len(), 6); // helloworld and hello_x excluded
        assert_eq!(search_files(&v, &q("", false, false), &no, 100).len(), 0);
        assert_eq!(search_files(&v, &q("hello", false, false), &no, 3).len(), 3);
    }

    #[test]
    fn unicode_insensitive() {
        let (_t, v) = setup();
        let no = AtomicBool::new(false);
        let h = search_files(&v, &q("ÄPFEL", false, false), &no, 100);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].col, h[1].col), (0, 10));
        assert_eq!(&h[1].line_text[h[1].match_range.start as usize..h[1].match_range.end as usize], "äpfel");
        assert_eq!(search_files(&v, &q("äpfel", true, false), &no, 100).len(), 1);
        assert!(search_files(&v, &q("äpfel", false, false), &AtomicBool::new(true), 100).is_empty());
    }

    #[test]
    fn length_changing_fold_and_long_lines() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("x.md");
        // U+023A lowercases to a 3-byte char, so offsets shift.
        let long = format!("{} Ⱥbc target {}", "x".repeat(500), "y".repeat(500));
        std::fs::write(&p, format!("Ⱥ Ⱥbc\n{long}")).unwrap();
        let no = AtomicBool::new(false);
        let h = search_files(std::slice::from_ref(&p), &q("ⱥbc", false, false), &no, 10);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].line, h[0].col), (0, 2));
        assert_eq!(&h[0].line_text[h[0].match_range.start as usize..h[0].match_range.end as usize], "Ⱥbc");
        let h = search_files(&[p], &q("target", false, false), &no, 10);
        assert_eq!(h[0].line, 1);
        assert!(h[0].line_text.chars().count() <= 241);
        assert_eq!(&h[0].line_text[h[0].match_range.start as usize..h[0].match_range.end as usize], "target");
        assert_eq!(h[0].col, 505);
    }

    #[test]
    fn job_polls() {
        let (_t, v) = setup();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        let job = SearchJob::start(
            v,
            OpenTexts::new(),
            q("hello", false, false),
            100,
            Arc::new(move || {
                n2.fetch_add(1, Ordering::Relaxed);
            }),
        );
        let mut total = 0;
        for _ in 0..500 {
            total += job.poll().len();
            if job.finished() {
                total += job.poll().len();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(total, 8);
        assert!(n.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn open_text_replaces_the_file_on_disk() {
        let (t, _) = setup();
        let p = t.path().join("a.md");
        let open = OpenTexts::from([(p.clone(), "fresh\nunsaved hello here\n".to_string()), (t.path().join("new.md"), "hello unsaved only\n".to_string())]);
        let mut hits = Vec::new();
        search_chunks(&[p, t.path().join("new.md")], &open, &q("hello", false, false), &AtomicBool::new(false), 100, |h| hits.extend(h));
        let got: Vec<_> = hits.iter().map(|h| (h.path.file_name().unwrap().to_str().unwrap().to_string(), h.line, h.col)).collect();
        assert_eq!(got, vec![("a.md".to_string(), 1, 8), ("new.md".to_string(), 0, 0)], "disk text of a.md ignored, line numbers from the buffer");
    }

    #[test]
    fn first_chunk_is_small() {
        let t = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..100)
            .map(|i| {
                let p = t.path().join(format!("{i:03}.md"));
                std::fs::write(&p, "needle\n").unwrap();
                p
            })
            .collect();
        let mut sizes = Vec::new();
        search_chunks(&paths, &OpenTexts::new(), &q("needle", false, false), &AtomicBool::new(false), 1000, |h| sizes.push(h.len()));
        assert_eq!(sizes.iter().sum::<usize>(), 100);
        assert_eq!(sizes[0], FIRST_CHUNK);
        assert!(sizes.windows(2).all(|w| w[1] >= w[0] || w[1] == *sizes.last().unwrap()), "chunks grow: {sizes:?}");
    }
}
