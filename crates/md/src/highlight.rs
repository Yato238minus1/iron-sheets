//! Fast, stable source highlighter for the editor: a hand-written line scanner.
//!
//! The output covers the text exactly (sorted, contiguous, on char boundaries). It is not
//! CommonMark-exact: it is meant to be cheap and to not flicker while typing.

use std::ops::Range;

mod window;
pub use window::WindowHighlighter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    Text,
    HeadingMark,
    Heading(u8),
    Bold,
    Italic,
    BoldItalic,
    Strike,
    Code,
    CodeFence,
    CodeBlock,
    LinkText,
    LinkUrl,
    WikiLink,
    Image,
    ListMark,
    TaskMark,
    QuoteMark,
    Rule,
    Html,
    Math,
    Tag,
    FrontMatter,
    Footnote,
    Escape,
    Comment,
    Highlight,
    TableSep,
    MarkupPunct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlSpan {
    pub range: Range<usize>,
    pub tok: Tok,
}

/// Highlights `text` into `out` (cleared first). O(n), never panics.
pub fn highlight(text: &str, out: &mut Vec<HlSpan>) {
    out.clear();
    let mut h = Hl { s: text, b: text.as_bytes(), out, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
    h.run();
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Normal,
    Fence { ch: u8, len: usize, quote: bool },
    Math,
    HtmlComment,
    PercentComment,
}

struct Hl<'a, 'o> {
    s: &'a str,
    b: &'a [u8],
    out: &'o mut Vec<HlSpan>,
    pos: usize,
    state: State,
    in_table: bool,
    line_end: usize,
}

const fn specials() -> [bool; 256] {
    let mut t = [false; 256];
    let list = b"\\`*_~=[!<$#%h|";
    let mut i = 0;
    while i < list.len() {
        t[list[i] as usize] = true;
        i += 1;
    }
    t
}
static SPECIAL: [bool; 256] = specials();

#[inline]
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

#[inline]
fn is_alnum_or_high(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c >= 0x80
}

fn combine(base: Tok, add: Tok) -> Tok {
    match (base, add) {
        (Tok::Bold, Tok::Italic) | (Tok::Italic, Tok::Bold) | (Tok::BoldItalic, Tok::Bold | Tok::Italic) => Tok::BoldItalic,
        (_, x) => x,
    }
}

impl<'a, 'o> Hl<'a, 'o> {
    #[inline]
    fn emit(&mut self, end: usize, tok: Tok) {
        if end <= self.pos {
            return;
        }
        match self.out.last_mut() {
            Some(l) if l.tok == tok && l.range.end == self.pos => l.range.end = end,
            _ => self.out.push(HlSpan { range: self.pos..end, tok }),
        }
        self.pos = end;
    }

    fn skip_ws(&self, mut i: usize, e: usize) -> usize {
        while i < e && matches!(self.b[i], b' ' | b'\t') {
            i += 1;
        }
        i
    }

    fn run(&mut self) {
        let n = self.b.len();
        let mut ls = 0;
        if let Some(end) = self.front_matter_end() {
            self.emit(end, Tok::FrontMatter);
            ls = end;
        }
        while ls < n {
            let nl = memchr::memchr(b'\n', &self.b[ls..]).map(|p| ls + p);
            let end = nl.map_or(n, |p| p + 1);
            let mut le = nl.unwrap_or(n);
            if le > ls && self.b[le - 1] == b'\r' {
                le -= 1;
            }
            self.line_end = le;
            self.line(ls, le, end);
            ls = end;
        }
        self.emit(n, Tok::Text);
    }

    fn front_matter_end(&self) -> Option<usize> {
        let b = self.b;
        if !b.starts_with(b"---") {
            return None;
        }
        let nl = memchr::memchr(b'\n', b)?;
        if !b[3..nl].iter().all(|&c| matches!(c, b' ' | b'\t' | b'\r')) {
            return None;
        }
        let mut ls = nl + 1;
        while ls < b.len() {
            let e = memchr::memchr(b'\n', &b[ls..]).map_or(b.len(), |p| ls + p);
            let line = self.s[ls..e].trim_end();
            if line == "---" || line == "..." {
                return Some((e + 1).min(b.len()));
            }
            ls = e + 1;
        }
        None
    }

    /// Strips `>` prefixes of a line inside a quoted fence, emitting QuoteMark tokens.
    fn quote_prefix(&mut self, ls: usize, le: usize) -> usize {
        let mut p = ls;
        loop {
            let q = self.skip_ws(p, le);
            if q < le && self.b[q] == b'>' {
                self.emit(q, Tok::CodeBlock);
                self.emit(q + 1, Tok::QuoteMark);
                p = q + 1;
            } else {
                return p;
            }
        }
    }

    fn line(&mut self, ls: usize, le: usize, end: usize) {
        match self.state {
            State::Fence { ch, len, quote } => {
                let cs = if quote { self.quote_prefix(ls, le) } else { ls };
                let q = self.skip_ws(cs, le);
                let run = self.b[q..le].iter().take_while(|&&c| c == ch).count();
                if run >= len && self.s[q + run..le].trim().is_empty() {
                    self.emit(end, Tok::CodeFence);
                    self.state = State::Normal;
                } else {
                    self.emit(end, Tok::CodeBlock);
                }
            }
            State::Math => {
                self.emit(end, Tok::Math);
                if memchr::memmem::find(&self.b[ls..le], b"$$").is_some() {
                    self.state = State::Normal;
                }
            }
            State::HtmlComment | State::PercentComment => {
                let pat: &[u8] = if self.state == State::HtmlComment { b"-->" } else { b"%%" };
                match memchr::memmem::find(&self.b[ls..le], pat) {
                    Some(p) => {
                        self.emit(ls + p + pat.len(), Tok::Comment);
                        self.state = State::Normal;
                        self.in_table = false;
                        self.inline(self.pos, le, Tok::Text, 0);
                        self.emit_eol(end);
                    }
                    None => self.emit(end, Tok::Comment),
                }
            }
            State::Normal => self.block_line(ls, le, end),
        }
    }

    fn is_delim_row(&self, s: usize, e: usize) -> bool {
        let l = &self.b[s..e];
        l.contains(&b'-') && l.contains(&b'|') && l.iter().all(|&c| matches!(c, b'|' | b'-' | b':' | b' ' | b'\t' | b'\r'))
    }

    fn block_line(&mut self, ls: usize, le: usize, end: usize) {
        let b = self.b;
        let j = self.skip_ws(ls, le);
        if j >= le {
            self.in_table = false;
            self.emit(end, Tok::Text);
            return;
        }
        let first = b[j];
        // thematic break / setext underline
        if matches!(first, b'-' | b'*' | b'_') {
            let mut cnt = 0;
            let mut ok = true;
            for &c in &b[j..le] {
                if c == first {
                    cnt += 1;
                } else if !matches!(c, b' ' | b'\t') {
                    ok = false;
                    break;
                }
            }
            if ok && cnt >= 3 {
                self.in_table = false;
                self.emit(end, Tok::Rule);
                return;
            }
        } else if first == b'=' && b[j..le].iter().all(|&c| c == b'=' || c == b' ' || c == b'\t') && le - j >= 3 {
            self.emit(end, Tok::HeadingMark);
            return;
        }
        // table detection
        let has_pipe = memchr::memchr(b'|', &b[j..le]).is_some();
        if has_pipe {
            if self.is_delim_row(ls, le) {
                self.in_table = true;
                self.emit(end, Tok::TableSep);
                return;
            }
            if !self.in_table {
                let next_delim = if end < b.len() {
                    let ne = memchr::memchr(b'\n', &b[end..]).map_or(b.len(), |p| end + p);
                    self.is_delim_row(end, ne)
                } else {
                    false
                };
                self.in_table = first == b'|' || next_delim;
            }
        } else {
            self.in_table = false;
        }
        // container prefixes: quotes, list markers, task boxes
        let mut p = ls;
        let mut quoted = false;
        loop {
            let q = self.skip_ws(p, le);
            if q >= le {
                break;
            }
            match b[q] {
                b'>' => {
                    self.emit(q, Tok::Text);
                    self.emit(q + 1, Tok::QuoteMark);
                    p = q + 1;
                    quoted = true;
                }
                b'-' | b'*' | b'+' if q + 1 == le || matches!(b[q + 1], b' ' | b'\t') => {
                    self.emit(q, Tok::Text);
                    self.emit(q + 1, Tok::ListMark);
                    p = q + 1;
                    let t = self.skip_ws(p, le);
                    if t + 3 <= le
                        && b[t] == b'['
                        && matches!(b[t + 1], b' ' | b'x' | b'X')
                        && b[t + 2] == b']'
                        && (t + 3 == le || matches!(b[t + 3], b' ' | b'\t'))
                    {
                        self.emit(t, Tok::Text);
                        self.emit(t + 3, Tok::TaskMark);
                        p = t + 3;
                    }
                }
                b'0'..=b'9' => {
                    let mut k = q;
                    while k < le && k - q < 9 && b[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k < le && matches!(b[k], b'.' | b')') && (k + 1 == le || matches!(b[k + 1], b' ' | b'\t')) {
                        self.emit(q, Tok::Text);
                        self.emit(k + 1, Tok::ListMark);
                        p = k + 1;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        let q = self.skip_ws(p, le);
        if q >= le {
            self.emit(end, Tok::Text);
            return;
        }
        let c = b[q];
        // fenced code opener
        if (c == b'`' || c == b'~') && !self.in_table {
            let run = b[q..le].iter().take_while(|&&x| x == c).count();
            if run >= 3 && (c == b'~' || memchr::memchr(b'`', &b[q + run..le]).is_none()) {
                self.emit(q, Tok::Text);
                self.emit(end, Tok::CodeFence);
                self.state = State::Fence { ch: c, len: run, quote: quoted };
                return;
            }
        }
        // ATX heading
        if c == b'#' {
            let h = b[q..le].iter().take_while(|&&x| x == b'#').count();
            if (1..=6).contains(&h) && (q + h == le || matches!(b[q + h], b' ' | b'\t')) {
                let tok = Tok::Heading(h as u8);
                self.emit(q, Tok::Text);
                self.emit(q + h, Tok::HeadingMark);
                let mut ce = le;
                while ce > q + h && matches!(b[ce - 1], b' ' | b'\t') {
                    ce -= 1;
                }
                let mut cs = ce;
                while cs > q + h && b[cs - 1] == b'#' {
                    cs -= 1;
                }
                if cs < ce && cs > q + h && matches!(b[cs - 1], b' ' | b'\t') {
                    self.inline(self.pos, cs, tok, 0);
                    self.emit(ce, Tok::HeadingMark);
                } else {
                    self.inline(self.pos, ce, tok, 0);
                }
                self.emit(end, tok);
                return;
            }
        }
        // display math block start
        if c == b'$' && !quoted && b[q..le].starts_with(b"$$") && p == ls && memchr::memmem::find(&b[q + 2..le], b"$$").is_none() {
            self.emit(end, Tok::Math);
            self.state = State::Math;
            return;
        }
        self.inline(p, le, Tok::Text, 0);
        self.emit_eol(end);
    }

    /// Newline after a plain line; part of a comment that is still open.
    fn emit_eol(&mut self, end: usize) {
        let tok = if matches!(self.state, State::HtmlComment | State::PercentComment) { Tok::Comment } else { Tok::Text };
        self.emit(end, tok);
    }

    /// Finds the `]` matching the `[` at `i` (nesting aware, bounded).
    fn match_bracket(&self, i: usize, e: usize) -> Option<usize> {
        let b = self.b;
        let limit = (i + 512).min(e);
        let mut depth = 0usize;
        let mut k = i;
        while k < limit {
            match b[k] {
                b'\\' => k += 1,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        None
    }

    /// Finds the `)` matching the `(` at `i`.
    fn match_paren(&self, i: usize, e: usize) -> Option<usize> {
        let b = self.b;
        let limit = (i + 1024).min(e);
        let mut depth = 0usize;
        let mut k = i;
        while k < limit {
            match b[k] {
                b'\\' => k += 1,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(k);
                    }
                }
                b' ' | b'\t' if depth == 1 => {
                    // allow a title: `(url "title")`
                    if let Some(rel) = memchr::memchr(b')', &b[k..limit]) {
                        let inner = &b[k..k + rel];
                        if inner.iter().filter(|&&c| c == b'(').count() == 0 {
                            return Some(k + rel);
                        }
                    }
                    return None;
                }
                _ => {}
            }
            k += 1;
        }
        None
    }

    /// Finds a closer run of `c` of exactly `n` bytes at or after `from`. Code spans are skipped
    /// and inline math (both bind tighter than emphasis).
    // shortcut: unmatched backtick runs rescan the line, fine for note-sized lines; memo like `inline`'s code_fail if needed.
    fn find_closer(&self, c: u8, n: usize, from: usize, e: usize, underscore: bool) -> Option<usize> {
        let b = self.b;
        let mut k = from;
        while let Some(p) = memchr::memchr3(c, b'`', b'$', &b[k..e]) {
            let q = k + p;
            if b[q] == b'$' {
                k = crate::math::inline_end(b, q, e).map_or(q + 1, |m| m.end);
                continue;
            }
            let mut m = 1;
            while q + m < e && b[q + m] == b[q] {
                m += 1;
            }
            if b[q] == b'`' {
                // Skip a whole code span; an unmatched run is literal.
                let mut j = q + m;
                k = q + m;
                while let Some(p) = memchr::memchr(b'`', &b[j..e]) {
                    let r = j + p;
                    let mut l = 1;
                    while r + l < e && b[r + l] == b'`' {
                        l += 1;
                    }
                    if l == m {
                        k = r + l;
                        break;
                    }
                    j = r + l;
                }
                continue;
            }
            let prev = b[q - 1];
            let ok = m == n && !is_ws(prev) && prev != b'\\' && (!underscore || q + m >= e || !is_alnum_or_high(b[q + m]));
            if ok {
                return Some(q);
            }
            k = q + m;
        }
        None
    }

    fn inline(&mut self, s: usize, e: usize, base: Tok, depth: u8) {
        let b = self.b;
        let mut i = s;
        let mut fail = [false; 10];
        let mut code_fail = 0u64;
        let mut gt_fail = false;
        while i < e {
            while i < e && !SPECIAL[b[i] as usize] {
                i += 1;
            }
            if i >= e {
                break;
            }
            let c = b[i];
            match c {
                b'\\' => {
                    if i + 1 < e && b[i + 1].is_ascii_punctuation() {
                        self.emit(i, base);
                        self.emit(i + 2, Tok::Escape);
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                b'`' => {
                    let mut n = 1;
                    while i + n < e && b[i + n] == b'`' {
                        n += 1;
                    }
                    let mut found = None;
                    if n < 64 && code_fail & (1 << n) == 0 {
                        let mut k = i + n;
                        while let Some(p) = memchr::memchr(b'`', &b[k..e]) {
                            let q = k + p;
                            let mut m = 1;
                            while q + m < e && b[q + m] == b'`' {
                                m += 1;
                            }
                            if m == n {
                                found = Some(q);
                                break;
                            }
                            k = q + m;
                        }
                        if found.is_none() {
                            code_fail |= 1 << n;
                        }
                    }
                    match found {
                        Some(q) => {
                            self.emit(i, base);
                            self.emit(q + n, Tok::Code);
                            i = q + n;
                        }
                        None => i += n,
                    }
                }
                b'*' | b'_' | b'~' | b'=' => {
                    let mut n = 1;
                    while i + n < e && b[i + n] == c {
                        n += 1;
                    }
                    let left = i + n < e && !is_ws(b[i + n]);
                    let intraword = c == b'_' && i > s && is_alnum_or_high(b[i - 1]);
                    let valid_n = match c {
                        b'*' | b'_' => n <= 3,
                        b'~' => n <= 2,
                        _ => n == 2,
                    };
                    if !left || intraword || !valid_n || depth >= 6 {
                        i += n;
                        continue;
                    }
                    let fi = match c {
                        b'*' => n - 1,
                        b'_' => 3 + n - 1,
                        b'~' => 6 + n - 1,
                        _ => 8,
                    };
                    if fail[fi] {
                        i += n;
                        continue;
                    }
                    match self.find_closer(c, n, i + n, e, c == b'_') {
                        Some(q) => {
                            let tok = match (c, n) {
                                (b'*' | b'_', 1) => Tok::Italic,
                                (b'*' | b'_', 2) => Tok::Bold,
                                (b'*' | b'_', _) => Tok::BoldItalic,
                                (b'~', _) => Tok::Strike,
                                _ => Tok::Highlight,
                            };
                            self.emit(i, base);
                            self.emit(i + n, Tok::MarkupPunct);
                            self.inline(i + n, q, combine(base, tok), depth + 1);
                            self.emit(q + n, Tok::MarkupPunct);
                            i = q + n;
                        }
                        None => {
                            fail[fi] = true;
                            i += n;
                        }
                    }
                }
                b'[' => i = self.bracket(i, e, base),
                b'!' => {
                    if i + 1 < e && b[i + 1] == b'[' {
                        let end = if i + 2 < e && b[i + 2] == b'[' {
                            self.wiki_end(i + 1, e)
                        } else {
                            self.match_bracket(i + 1, e)
                                .and_then(|cj| if cj + 1 < e && b[cj + 1] == b'(' { self.match_paren(cj + 1, e).map(|p| p + 1) } else { None })
                        };
                        if let Some(end) = end {
                            self.emit(i, base);
                            self.emit(end, Tok::Image);
                            i = end;
                            continue;
                        }
                    }
                    i += 1;
                }
                b'<' => {
                    if b[i..e].starts_with(b"<!--") {
                        self.emit(i, base);
                        match memchr::memmem::find(&b[i + 4..e], b"-->") {
                            Some(p) => {
                                self.emit(i + 4 + p + 3, Tok::Comment);
                                i = i + 4 + p + 3;
                            }
                            None => {
                                self.emit(e, Tok::Comment);
                                if e == self.line_end {
                                    self.state = State::HtmlComment;
                                }
                                return;
                            }
                        }
                        continue;
                    }
                    let nx = b.get(i + 1).copied().unwrap_or(b' ');
                    if !(nx.is_ascii_alphabetic() || nx == b'/' || nx == b'!') || gt_fail {
                        i += 1;
                        continue;
                    }
                    match memchr::memchr(b'>', &b[i..(i + 1024).min(e)]) {
                        Some(p) => {
                            let q = i + p + 1;
                            let tag = &b[i..q];
                            let url = (tag.starts_with(b"<http") || tag.starts_with(b"<mailto:")) && !tag.iter().any(|&c| is_ws(c));
                            let tok = if url { Tok::LinkUrl } else { Tok::Html };
                            self.emit(i, base);
                            self.emit(q, tok);
                            i = q;
                        }
                        None => {
                            if memchr::memchr(b'>', &b[i..e]).is_none() {
                                gt_fail = true;
                            }
                            i += 1;
                        }
                    }
                }
                b'$' => {
                    if i + 1 < e && b[i + 1] == b'$' {
                        match memchr::memmem::find(&b[i + 2..e], b"$$") {
                            Some(p) => {
                                self.emit(i, base);
                                self.emit(i + 2 + p + 2, Tok::Math);
                                i = i + 2 + p + 2;
                            }
                            None => i += 2,
                        }
                    } else {
                        // The one rule shared with the parser (see `math`).
                        match crate::math::inline_end(b, i, e) {
                            Some(m) => {
                                self.emit(i, base);
                                self.emit(m.end, Tok::Math);
                                i = m.end;
                            }
                            None => i += 1,
                        }
                    }
                }
                b'#' => {
                    let boundary = i == s || is_ws(b[i - 1]);
                    let mut j = i + 1;
                    let mut non_digit = false;
                    while j < e {
                        let x = b[j];
                        if x.is_ascii_alphabetic() || x >= 0x80 || matches!(x, b'_' | b'-' | b'/') {
                            non_digit = true;
                        } else if !x.is_ascii_digit() {
                            break;
                        }
                        j += 1;
                    }
                    if boundary && j > i + 1 && non_digit {
                        self.emit(i, base);
                        self.emit(j, Tok::Tag);
                        i = j;
                    } else {
                        i += 1;
                    }
                }
                b'%' => {
                    if i + 1 < e && b[i + 1] == b'%' {
                        self.emit(i, base);
                        match memchr::memmem::find(&b[i + 2..e], b"%%") {
                            Some(p) => {
                                self.emit(i + 2 + p + 2, Tok::Comment);
                                i = i + 2 + p + 2;
                            }
                            None => {
                                self.emit(e, Tok::Comment);
                                if e == self.line_end {
                                    self.state = State::PercentComment;
                                }
                                return;
                            }
                        }
                    } else {
                        i += 1;
                    }
                }
                b'h' => {
                    let rest = &b[i..e];
                    let scheme = if rest.starts_with(b"http://") {
                        7
                    } else if rest.starts_with(b"https://") {
                        8
                    } else {
                        0
                    };
                    if scheme > 0 && (i == s || !b[i - 1].is_ascii_alphanumeric()) && rest.len() > scheme {
                        let mut j = i + scheme;
                        while j < e && !is_ws(b[j]) && !matches!(b[j], b'<' | b'>' | b'`') {
                            j += 1;
                        }
                        while j > i + scheme
                            && (matches!(b[j - 1], b'.' | b',' | b';' | b':' | b'!' | b'?' | b'\'' | b'"' | b'*' | b'_' | b'~')
                                || (b[j - 1] == b')' && !b[i..j].contains(&b'(')))
                        {
                            j -= 1;
                        }
                        if j > i + scheme {
                            self.emit(i, base);
                            self.emit(j, Tok::LinkUrl);
                            i = j;
                            continue;
                        }
                    }
                    i += 1;
                }
                b'|' => {
                    if self.in_table {
                        self.emit(i, base);
                        self.emit(i + 1, Tok::TableSep);
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        self.emit(e, base);
    }

    fn wiki_end(&self, i: usize, e: usize) -> Option<usize> {
        // b[i..i+2] == "[["
        let b = self.b;
        let limit = (i + 512).min(e);
        let mut k = i + 2;
        while let Some(p) = memchr::memchr(b']', &b[k..limit]) {
            let q = k + p;
            if q + 1 < e && b[q + 1] == b']' {
                return Some(q + 2);
            }
            k = q + 1;
        }
        None
    }

    /// Handles `[` at `i`; returns the new position.
    fn bracket(&mut self, i: usize, e: usize, base: Tok) -> usize {
        let b = self.b;
        if i + 1 < e && b[i + 1] == b'[' {
            if let Some(end) = self.wiki_end(i, e) {
                self.emit(i, base);
                self.emit(end, Tok::WikiLink);
                return end;
            }
            return i + 1;
        }
        if i + 1 < e && b[i + 1] == b'^' {
            let limit = (i + 66).min(e);
            if let Some(p) = memchr::memchr(b']', &b[i..limit]) {
                let q = i + p;
                if q > i + 2 && !b[i + 2..q].iter().any(|&c| is_ws(c)) {
                    let end = if q + 1 < e && b[q + 1] == b':' { q + 2 } else { q + 1 };
                    self.emit(i, base);
                    self.emit(end, Tok::Footnote);
                    return end;
                }
            }
            return i + 1;
        }
        let Some(cj) = self.match_bracket(i, e) else { return i + 1 };
        if cj + 1 >= e {
            return i + 1;
        }
        match b[cj + 1] {
            b'(' => {
                if let Some(pj) = self.match_paren(cj + 1, e) {
                    self.emit(i, base);
                    self.emit(i + 1, Tok::MarkupPunct);
                    self.emit(cj, Tok::LinkText);
                    self.emit(cj + 2, Tok::MarkupPunct);
                    let url_end = b[cj + 2..pj].iter().position(|&c| is_ws(c)).map_or(pj, |p| cj + 2 + p);
                    self.emit(url_end, Tok::LinkUrl);
                    self.emit(pj, base); // optional "title"
                    self.emit(pj + 1, Tok::MarkupPunct);
                    return pj + 1;
                }
                i + 1
            }
            b'[' => {
                let limit = (cj + 2 + 256).min(e);
                if let Some(p) = memchr::memchr(b']', &b[cj + 2..limit]) {
                    let rj = cj + 2 + p;
                    self.emit(i, base);
                    self.emit(i + 1, Tok::MarkupPunct);
                    self.emit(cj, Tok::LinkText);
                    self.emit(cj + 2, Tok::MarkupPunct);
                    self.emit(rj, Tok::LinkUrl);
                    self.emit(rj + 1, Tok::MarkupPunct);
                    return rj + 1;
                }
                i + 1
            }
            _ => i + 1,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Incremental highlighting
// ---------------------------------------------------------------------------------------------

/// What carries from one line to the next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct LineState {
    state: State,
    in_table: bool,
}

const LINE_START: LineState = LineState { state: State::Normal, in_table: false };

#[derive(Clone, Copy, Debug)]
struct Line {
    /// Bytes including the line break.
    len: u32,
    nspans: u32,
    /// Parser state at the start of the line.
    st: LineState,
    /// Part of the front matter block (one `FrontMatter` span, no state change).
    fm: bool,
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    // Big blocks first (one `bcmp` each), then fine steps.
    while i + 4096 <= n && a[i..i + 4096] == b[i..i + 4096] {
        i += 4096;
    }
    while i + 64 <= n && a[i..i + 64] == b[i..i + 64] {
        i += 64;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// Common suffix length of `a` and `b`, at most `max`.
fn common_suffix(a: &[u8], b: &[u8], max: usize) -> usize {
    let (la, lb) = (a.len(), b.len());
    let n = max.min(la).min(lb);
    let mut i = 0;
    while i + 4096 <= n && a[la - i - 4096..la - i] == b[lb - i - 4096..lb - i] {
        i += 4096;
    }
    while i + 64 <= n && a[la - i - 64..la - i] == b[lb - i - 64..lb - i] {
        i += 64;
    }
    while i < n && a[la - 1 - i] == b[lb - 1 - i] {
        i += 1;
    }
    i
}

/// Length of the common prefix and suffix of `old` and `new` (char-boundary aligned, not overlapping).
pub fn diff_texts(old: &str, new: &str) -> (usize, usize) {
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let mut p = common_prefix(ob, nb);
    while p > 0 && !(new.is_char_boundary(p) && old.is_char_boundary(p)) {
        p -= 1;
    }
    let mut s = common_suffix(ob, nb, ob.len().min(nb.len()) - p);
    while s > 0 && !(new.is_char_boundary(nb.len() - s) && old.is_char_boundary(ob.len() - s)) {
        s -= 1;
    }
    (p, s)
}

/// A highlighter that keeps per-line results and the parser state at every line start. `update` finds
/// the edited range against the previous text, re-highlights from the line before it (a table header
/// looks one line ahead) until the line-start state matches the cached one again, and reuses the rest.
/// The result equals [`highlight`] (spans of equal tokens are only not merged across lines).
#[derive(Default)]
pub struct IncrementalHighlighter {
    text: String,
    lines: Vec<Line>,
    /// `(length, token)` per span, line after line; each line's lengths sum to its `len`.
    spans: Vec<(u32, Tok)>,
    fm_end: Option<usize>,
    redone: usize,
}

impl IncrementalHighlighter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the cached lines describe.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of lines the last `update` highlighted again.
    pub fn lines_redone(&self) -> usize {
        self.redone
    }

    /// Length of the common prefix and suffix of the cached text and `new` (char-boundary aligned,
    /// not overlapping). Cheap; used to pick the best cache for a text.
    pub fn similarity(&self, new: &str) -> usize {
        let (p, s) = self.diff(new);
        p + s
    }

    fn diff(&self, new: &str) -> (usize, usize) {
        diff_texts(&self.text, new)
    }

    /// Brings the cached lines up to date with `new`.
    pub fn update(&mut self, new: &str) {
        let old_len = self.text.len();
        let (p, s) = self.diff(new);
        if p == old_len && p == new.len() {
            self.redone = 0;
            return;
        }
        let nb = new.as_bytes();
        let n = nb.len();
        let mut scratch = Vec::new();
        let mut h = Hl { s: new, b: nb, out: &mut scratch, pos: 0, state: State::Normal, in_table: false, line_end: 0 };
        let recompute = match self.fm_end {
            Some(e) => p <= e,
            None => nb.starts_with(b"---"),
        };
        let fm_new = if recompute { h.front_matter_end() } else { self.fm_end };
        let fm_end = fm_new.unwrap_or(0);

        // First line to redo: the one before the line holding the first changed byte.
        let (mut start, mut sidx, mut a0) = (0usize, 0usize, 0usize);
        let mut prev = (0usize, 0usize);
        while a0 < self.lines.len() && start + self.lines[a0].len as usize <= p {
            prev = (start, sidx);
            start += self.lines[a0].len as usize;
            sidx += self.lines[a0].nspans as usize;
            a0 += 1;
        }
        // Lines before the edit keep their result, unless the front matter block appeared, vanished or moved.
        let dirty = if fm_new != self.fm_end {
            start = 0;
            sidx = 0;
            0
        } else if a0 > 0 {
            start = prev.0;
            sidx = prev.1;
            a0 - 1
        } else {
            a0
        };

        let sfx = n - s;
        let delta = n as isize - old_len as isize;
        let mut state = self.lines.get(dirty).map_or(LINE_START, |l| l.st);
        let (mut oi, mut old_start, mut old_sidx) = (dirty, start, sidx);
        let mut ns = start;
        // A cold start (nothing cached) fills everything: size the buffers for it.
        let cold = self.lines.is_empty();
        let mut mid_lines: Vec<Line> = Vec::with_capacity(if cold { n / 40 + 8 } else { 16 });
        let mut mid_spans: Vec<(u32, Tok)> = Vec::with_capacity(if cold { n / 10 + 8 } else { 64 });
        loop {
            if ns >= n {
                oi = self.lines.len();
                old_sidx = self.spans.len();
                break;
            }
            if ns >= sfx {
                let target = (ns as isize - delta) as usize;
                while oi < self.lines.len() && old_start < target {
                    old_start += self.lines[oi].len as usize;
                    old_sidx += self.lines[oi].nspans as usize;
                    oi += 1;
                }
                if oi < self.lines.len() && old_start == target && !self.lines[oi].fm && self.lines[oi].st == state && ns >= fm_end {
                    break;
                }
            }
            let nl = memchr::memchr(b'\n', &nb[ns..]).map(|q| ns + q);
            let end = nl.map_or(n, |q| q + 1);
            if ns < fm_end {
                mid_lines.push(Line { len: (end - ns) as u32, nspans: 1, st: state, fm: true });
                mid_spans.push(((end - ns) as u32, Tok::FrontMatter));
                state = LINE_START;
            } else {
                let mut le = nl.unwrap_or(n);
                if le > ns && nb[le - 1] == b'\r' {
                    le -= 1;
                }
                h.out.clear();
                h.pos = ns;
                h.state = state.state;
                h.in_table = state.in_table;
                h.line_end = le;
                h.line(ns, le, end);
                h.emit(end, Tok::Text);
                for sp in h.out.iter() {
                    mid_spans.push(((sp.range.end - sp.range.start) as u32, sp.tok));
                }
                mid_lines.push(Line { len: (end - ns) as u32, nspans: h.out.len() as u32, st: state, fm: false });
                state = LineState { state: h.state, in_table: h.in_table };
            }
            ns = end;
        }
        self.redone = mid_lines.len();
        if cold {
            self.lines = mid_lines;
            self.spans = mid_spans;
        } else {
            self.lines.splice(dirty..oi, mid_lines);
            self.spans.splice(sidx..old_sidx, mid_spans);
        }
        self.text.replace_range(p..old_len - s, &new[p..n - s]);
        self.fm_end = fm_new;
    }

    /// `(length, token)` of every span in text order. No span contains a line break except as its last byte.
    pub fn tokens(&self) -> &[(u32, Tok)] {
        &self.spans
    }

    /// Calls `f(range, spans)` for every line: its byte range in [`Self::text`] and its `(length, token)` spans.
    pub fn for_each_line(&self, mut f: impl FnMut(Range<usize>, &[(u32, Tok)])) {
        let (mut at, mut si) = (0usize, 0usize);
        for l in &self.lines {
            let end = at + l.len as usize;
            f(at..end, &self.spans[si..si + l.nspans as usize]);
            at = end;
            si += l.nspans as usize;
        }
    }

    /// The spans as `highlight` returns them (adjacent equal tokens merged).
    pub fn spans(&self, out: &mut Vec<HlSpan>) {
        out.clear();
        self.for_each_line(|r, spans| {
            let mut at = r.start;
            for &(len, tok) in spans {
                let end = at + len as usize;
                match out.last_mut() {
                    Some(l) if l.tok == tok && l.range.end == at => l.range.end = end,
                    _ => out.push(HlSpan { range: at..end, tok }),
                }
                at = end;
            }
        });
    }
}
