//! Highlighting of a window of a big note without highlighting the whole note.
//!
//! The line scanner (`Hl::line`) carries a small state from line to line: inside a fenced code block, inside
//! display math, inside an HTML or `%%` comment, and whether the previous line was part of a table. A window
//! (a few paragraphs near the viewport) can only be highlighted correctly if that state at its first line is
//! known. `WindowHighlighter` finds it without running the scanner over every line:
//!
//! * The only lines that can change the block state are lines containing one of `` ``` ``, `~~~`, `$$`,
//!   `<!--` or `%%`. A handful of `memmem` searches find them; every other line in the `Normal` state is
//!   skipped. The lines of the candidate regions are run through the real `Hl::line`, so the state machine is not
//!   re-implemented.
//! * The result is a sorted list of *regions*: the line starts whose state is not `Normal`, with that state.
//! * The table flag is rebuilt on demand from the lines just before a window: it is reset by every line without
//!   a `|` (except a setext underline), so only the tail of a table has to be replayed.
//! * An edit rescans from the line before the edit and stops as soon as the scanner is `Normal` on a line that
//!   cannot depend on the flag and that was `Normal` before the edit as well; the old regions after it are
//!   kept (shifted). A keystroke costs a diff of the text plus a few lines.
//!
//! The tokens of a window equal the tokens `highlight` produces for the same bytes of the whole text.
use super::{Hl, LINE_START, LineState, State, Tok, diff_texts};
use memchr::memmem::Finder;
use std::ops::Range;

/// Line starts `a..=b` (both line starts) whose parser state at the start of the line is `st`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Region {
    a: usize,
    b: usize,
    st: State,
    /// The table flag while the region lasts (it is not touched inside code and math, and reset by a comment end).
    tbl: bool,
}

impl Region {
    /// The table flag on the first line after the region.
    fn tbl_after(&self) -> bool {
        match self.st {
            State::HtmlComment | State::PercentComment => false,
            _ => self.tbl,
        }
    }
}

/// Region-based block state of a note. It keeps no copy of the text: `update` is given the previous text (the one the
/// regions describe) and the new one, `tokens` the current one.
#[derive(Default)]
pub struct WindowHighlighter {
    /// Length of the text the regions describe (a cheap check that the caller passes the right previous text).
    len: usize,
    fm_end: Option<usize>,
    /// The note starts with `---` and no closing line was found: edits that touch no `---`/`...` line cannot close it.
    fm_open: bool,
    regions: Vec<Region>,
    built: bool,
}

fn line_bounds(b: &[u8], ls: usize) -> (usize, usize) {
    let n = b.len();
    let nl = memchr::memchr(b'\n', &b[ls..]).map(|p| ls + p);
    let end = nl.map_or(n, |p| p + 1);
    let mut le = nl.unwrap_or(n);
    if le > ls && b[le - 1] == b'\r' {
        le -= 1;
    }
    (le, end)
}

/// Start of the line before the line starting at `ls` (`ls > 0`).
fn prev_line_start(b: &[u8], ls: usize) -> usize {
    memchr::memrchr(b'\n', &b[..ls - 1]).map_or(0, |p| p + 1)
}

/// Start of the line containing byte `at`.
fn line_start_of(b: &[u8], at: usize) -> usize {
    memchr::memrchr(b'\n', &b[..at.min(b.len())]).map_or(0, |p| p + 1)
}

/// True when running `block_line` on this line leaves the table flag cleared, whatever it was and whatever
/// follows: blank lines, thematic breaks and every line without a `|`, except a setext underline (`===`).
fn resets_table(b: &[u8], ls: usize, le: usize) -> bool {
    let mut j = ls;
    while j < le && matches!(b[j], b' ' | b'\t') {
        j += 1;
    }
    if j >= le {
        return true;
    }
    if memchr::memchr(b'|', &b[j..le]).is_some() {
        return false;
    }
    !(b[j] == b'=' && le - j >= 3 && b[j..le].iter().all(|&c| matches!(c, b'=' | b' ' | b'\t')))
}

/// Runs the real line scanner on the line at `ls`; returns the start of the next line and the state after it.
fn step(h: &mut Hl, ls: usize, st: LineState) -> (usize, LineState) {
    let (le, end) = line_bounds(h.b, ls);
    h.out.clear();
    h.pos = ls;
    h.state = st.state;
    h.in_table = st.in_table;
    h.line_end = le;
    h.line(ls, le, end);
    (end, LineState { state: h.state, in_table: h.in_table })
}

/// The table flag at the start of line `to`, when all lines in `from..to` are `Normal` lines that do not open
/// anything and the flag was `init` at `from`.
fn table_at(h: &mut Hl, from: usize, to: usize, init: bool) -> bool {
    let b = h.b;
    // Newest line in `from..to` that resets the flag; replay from the line after it.
    let mut e = to;
    let mut replay_from = from;
    let mut tbl = init;
    while e > from {
        let ls = if e - 1 > from { prev_line_start(b, e).max(from) } else { from };
        let (le, _) = line_bounds(b, ls);
        if resets_table(b, ls, le) {
            replay_from = line_bounds(b, ls).1;
            tbl = false;
            break;
        }
        e = ls;
    }
    let mut pos = replay_from;
    while pos < to {
        let (end, st) = step(h, pos, LineState { state: State::Normal, in_table: tbl });
        tbl = st.in_table;
        pos = end;
    }
    tbl
}

/// Finds the next line (start) that contains one of the opening patterns.
struct Candidates<'a> {
    b: &'a [u8],
    finders: [Finder<'static>; 5],
    next: [usize; 5],
}

impl<'a> Candidates<'a> {
    fn new(b: &'a [u8]) -> Self {
        let f = |p: &'static [u8]| Finder::new(p);
        Candidates { b, finders: [f(b"```"), f(b"~~~"), f(b"$$"), f(b"<!--"), f(b"%%")], next: [0; 5] }
    }

    /// Start of the first line at or after `pos` that contains a pattern; `None` when there is none.
    fn line_from(&mut self, pos: usize) -> Option<usize> {
        let mut best = usize::MAX;
        for i in 0..5 {
            // `next[i]` is the first hit at or after the last search start; reuse it while it is still ahead.
            if self.next[i] < pos {
                self.next[i] = self.finders[i].find(&self.b[pos..]).map_or(usize::MAX, |p| pos + p);
            }
            best = best.min(self.next[i]);
        }
        (best != usize::MAX).then(|| line_start_of(self.b, best).max(pos))
    }
}

/// What the rescan after an edit may reuse from the old regions.
struct Sync<'r> {
    /// End of the edit in the new text (everything after it is unchanged).
    new_end: usize,
    delta: isize,
    old: &'r [Region],
    /// The text before the edit.
    old_text: &'r [u8],
}

impl Sync<'_> {
    fn old_region_at(&self, old_pos: usize) -> Option<&Region> {
        let i = self.old.partition_point(|r| r.b < old_pos);
        self.old.get(i).filter(|r| r.a <= old_pos)
    }

    /// First line start `l` in `from..=upto` after which the new scanner is in the same state as the old one was:
    /// the line lies after the edit, it was `Normal` before the edit, and it resets the table flag.
    fn find(&self, b: &[u8], from: usize, upto: usize) -> Option<usize> {
        // The line starting at the end of the edit is unchanged only when the edit also ended at a line start before it.
        let old_end = (self.new_end as isize - self.delta) as usize;
        let starts_line = |t: &[u8], at: usize| at == 0 || t.get(at - 1) == Some(&b'\n');
        let mut l =
            if starts_line(b, self.new_end) && starts_line(self.old_text, old_end) { self.new_end } else { line_bounds(b, self.new_end.min(b.len())).1 };
        l = l.max(from);
        while l <= upto && l < b.len() {
            let old_pos = (l as isize - self.delta) as usize;
            if let Some(r) = self.old_region_at(old_pos) {
                // Inside an old region: continue after its closing line.
                let close = (r.b as isize + self.delta) as usize;
                l = line_bounds(b, close.min(b.len() - 1)).1;
                continue;
            }
            let (le, end) = line_bounds(b, l);
            if resets_table(b, l, le) {
                return Some(l);
            }
            l = end;
        }
        None
    }
}

struct Scanned {
    regions: Vec<Region>,
    /// Start of the first line the old regions are valid for again (new coordinates); `n` when none are.
    stop: usize,
    synced: bool,
}

fn scan(h: &mut Hl, from: usize, init: LineState, sync: Option<&Sync>) -> Scanned {
    let b = h.b;
    let n = b.len();
    let mut regions = Vec::new();
    let mut cand = Candidates::new(b);
    let (mut pos, mut st) = (from, init);
    let mut open: Option<(usize, State, bool)> = None;
    let mut last_ls = from;
    while pos < n {
        if st.state == State::Normal {
            let cs = cand.line_from(pos);
            if let Some(l) = sync.and_then(|sy| sy.find(b, pos, cs.unwrap_or(n))) {
                return Scanned { regions, stop: line_bounds(b, l).1, synced: true };
            }
            let Some(cs) = cs else { break };
            st.in_table = table_at(h, pos, cs, st.in_table);
            pos = cs;
        }
        last_ls = pos;
        let was = st.state;
        let (end, next) = step(h, pos, st);
        if was != next.state {
            // A comment can end and start another one on the same line (`--> %%`), so regions can be adjacent.
            if was != State::Normal
                && let Some((a, _, tbl)) = open.take()
            {
                regions.push(Region { a, b: pos, st: was, tbl });
            }
            if next.state != State::Normal {
                open = Some((end, next.state, next.in_table));
            }
        }
        st = next;
        pos = end;
    }
    if let Some((a, s, tbl)) = open.take()
        && a < n
    {
        regions.push(Region { a, b: last_ls, st: s, tbl });
    }
    Scanned { regions, stop: n, synced: false }
}

impl WindowHighlighter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of non-`Normal` regions (diagnostics).
    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    fn rebuild(&mut self, text: &str) {
        let b = text.as_bytes();
        let mut scratch = Vec::new();
        let mut h = Hl { s: text, b, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
        let from = self.fm_end.unwrap_or(0);
        self.regions = scan(&mut h, from, LINE_START, None).regions;
        self.built = true;
    }

    /// Brings the regions up to date with `new`. `old` is the text of the previous call; `None` (or a text of another
    /// length) builds everything again. Invariant: `old` is exactly the text of the previous call; only its length is
    /// checked (comparing it would cost O(n) per keystroke), so a caller passing another text of that length gets wrong
    /// regions. Returns the byte range (in `new`) whose highlighting may differ from before the
    /// call: the text outside it, shifted by the edit, highlights as it did.
    pub fn update(&mut self, old: Option<&str>, new: &str) -> Range<usize> {
        let n = new.len();
        let old = match old.filter(|o| self.built && o.len() == self.len) {
            Some(o) => o,
            None => {
                self.built = false;
                ""
            }
        };
        let old_len = old.len();
        let (p, s) = diff_texts(old, new);
        if self.built && p == old_len && p == n {
            return 0..0;
        }
        let nb = new.as_bytes();
        let fm_new = {
            let new_end = n - s;
            let recompute = match self.fm_end {
                Some(e) => p <= e,
                None if self.fm_open && self.built && memchr::memchr(b'\n', nb).is_some_and(|nl| p > nl) => {
                    let (lo, hi) = (line_start_of(nb, p), line_bounds(nb, line_start_of(nb, new_end)).1);
                    let r = &nb[lo..hi];
                    r.windows(3).any(|w| w == b"---" || w == b"...")
                }
                None => nb.starts_with(b"---"),
            };
            if recompute {
                let mut scratch = Vec::new();
                let h = Hl { s: new, b: nb, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
                let e = h.front_matter_end();
                self.fm_open = e.is_none() && nb.starts_with(b"---");
                e
            } else {
                self.fm_end
            }
        };
        if !self.built || fm_new != self.fm_end {
            self.len = n;
            self.fm_end = fm_new;
            self.rebuild(new);
            return 0..n;
        }
        let fm_end = fm_new.unwrap_or(0);
        // Anchor: the line before the line of the first changed byte (a table header looks one line ahead), or the
        // opener of the region that contains it.
        let ls_p = line_start_of(nb, p);
        let mut anchor = if ls_p > 0 { prev_line_start(nb, ls_p) } else { 0 };
        let mut k = self.regions.partition_point(|r| r.b < anchor);
        while let Some(r) = self.regions.get(k).filter(|r| r.a <= anchor) {
            anchor = prev_line_start(nb, r.a);
            k = self.regions.partition_point(|r| r.b < anchor);
        }
        anchor = anchor.max(fm_end);
        let delta = n as isize - old_len as isize;
        let new_end = n - s;
        let mut scratch = Vec::new();
        let mut h = Hl { s: new, b: nb, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
        // State at the anchor: its lines are part of the unchanged prefix.
        let init = state_in(&self.regions[..k], &mut h, fm_end, anchor);
        let sync = Sync { new_end, delta, old: &self.regions, old_text: old.as_bytes() };
        let sc = scan(&mut h, anchor, init, Some(&sync));
        let added = sc.regions.len();
        if sc.synced {
            let old_stop = (sc.stop as isize - delta) as usize;
            let t = self.regions.partition_point(|r| r.a < old_stop).max(k);
            self.regions.splice(k..t, sc.regions);
            if delta != 0 {
                for r in &mut self.regions[k + added..] {
                    r.a = (r.a as isize + delta) as usize;
                    r.b = (r.b as isize + delta) as usize;
                }
            }
        } else {
            self.regions.truncate(k);
            self.regions.extend(sc.regions);
        }
        self.len = n;
        anchor..sc.stop
    }

    /// The fenced code block whose body contains the line starting at `ls`: `(opener line, body)` as byte ranges of
    /// `text`; the body ends before the closing fence, or at the text end when the block is not closed. `None` outside
    /// fences, on the closing fence line, in a quoted fence or one whose opener is indented. Costs a binary search and
    /// one line scan, however far above the opener lies.
    pub fn fence_block(&self, text: &str, ls: usize) -> Option<(Range<usize>, Range<usize>)> {
        let b = text.as_bytes();
        let r = self.regions.get(self.regions.partition_point(|r| r.b < ls)).filter(|r| r.a <= ls)?;
        if !matches!(r.st, State::Fence { quote: false, .. }) || r.a == 0 || r.a >= b.len() {
            return None;
        }
        let opener = prev_line_start(b, r.a);
        if !matches!(b[opener], b'`' | b'~') {
            return None;
        }
        let mut scratch = Vec::new();
        let mut h = Hl { s: text, b, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
        let closed = step(&mut h, r.b, LineState { state: r.st, in_table: r.tbl }).1.state == State::Normal;
        if closed && ls == r.b {
            return None;
        }
        Some((opener..r.a, r.a..if closed { r.b } else { b.len() }))
    }

    /// Calls `f` with `(length, token)` of every span of the lines that start in `start..=end`; `start` must be
    /// a line start, `end` the position of a line break (or the text end), as for a paragraph. The spans tile the
    /// lines including their line breaks, like [`super::IncrementalHighlighter::tokens`].
    pub fn tokens(&self, text: &str, start: usize, end: usize, out: &mut Vec<(u32, Tok)>) {
        out.clear();
        let b = text.as_bytes();
        let n = b.len();
        let mut ls = start.min(n);
        let fm_end = self.fm_end.unwrap_or(0);
        while ls < fm_end && ls <= end {
            let e = line_bounds(b, ls).1;
            out.push(((e - ls) as u32, Tok::FrontMatter));
            ls = e;
        }
        if ls >= n || ls > end {
            return;
        }
        let mut scratch = Vec::new();
        let mut h = Hl { s: text, b, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
        let mut st = state_in(&self.regions, &mut h, fm_end, ls);
        while ls < n && ls <= end {
            let (le, e) = line_bounds(b, ls);
            h.out.clear();
            h.pos = ls;
            h.state = st.state;
            h.in_table = st.in_table;
            h.line_end = le;
            h.line(ls, le, e);
            h.emit(e, Tok::Text);
            for sp in h.out.iter() {
                out.push(((sp.range.end - sp.range.start) as u32, sp.tok));
            }
            st = LineState { state: h.state, in_table: h.in_table };
            ls = e;
        }
    }
}

/// Scanner state at the start of line `ls` (`ls >= fm_end`), from the regions that end before it (or contain it).
fn state_in(regions: &[Region], h: &mut Hl, fm_end: usize, ls: usize) -> LineState {
    let i = regions.partition_point(|r| r.b < ls);
    if let Some(r) = regions.get(i).filter(|r| r.a <= ls) {
        return LineState { state: r.st, in_table: r.tbl };
    }
    let (lo, init) = match i.checked_sub(1).map(|j| &regions[j]) {
        Some(r) => (line_bounds(h.b, r.b).1.max(fm_end), r.tbl_after()),
        None => (fm_end, false),
    };
    if ls <= lo {
        return LineState { state: State::Normal, in_table: init };
    }
    LineState { state: State::Normal, in_table: table_at(h, lo, ls, init) }
}
