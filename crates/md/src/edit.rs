//! Pure text-editing helpers for the source editor. All offsets are byte offsets on char
//! boundaries; out-of-range or mid-char offsets are clamped, never a panic.

use std::ops::Range;

/// Replace `range` with `insert`, then select `select` (offsets in the *new* text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOp {
    pub range: Range<usize>,
    pub insert: String,
    pub select: Range<usize>,
}

impl EditOp {
    /// Applies the op to `text` (handy for tests and for non-egui front ends).
    pub fn apply(&self, text: &str) -> String {
        let mut s = String::with_capacity(text.len() + self.insert.len());
        s.push_str(&text[..self.range.start]);
        s.push_str(&self.insert);
        s.push_str(&text[self.range.end..]);
        s
    }
}

fn clamp(text: &str, mut pos: usize) -> usize {
    pos = pos.min(text.len());
    while !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |p| p + 1)
}

fn line_end(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |p| pos + p)
}

fn skip_blank(b: &[u8], mut i: usize, e: usize) -> usize {
    while i < e && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bullet(u8),
    Ordered { num: u64, delim: u8 },
}

/// Structure of the start of a line: quote prefixes, then an optional list marker.
struct Parsed {
    /// Start of the list marker (after indentation / quote prefixes); equals `content` end of
    /// prefix when there is no marker.
    marker_start: usize,
    /// End of the quote prefix (indent + `>` markers + one space each), `ls` when none.
    quote_end: usize,
    marker: Option<Marker>,
    /// End of the marker, before the whitespace that follows it.
    marker_end: usize,
    /// First byte of the item's content (after marker, spaces, task box).
    content: usize,
    task: bool,
    quoted: bool,
}

fn parse_line(text: &str, ls: usize, le: usize) -> Parsed {
    let b = text.as_bytes();
    let mut p = ls;
    let mut quoted = false;
    let mut quote_end = ls;
    loop {
        let q = skip_blank(b, p, le);
        if q < le && b[q] == b'>' {
            quoted = true;
            p = q + 1;
            if p < le && b[p] == b' ' {
                p += 1;
            }
            quote_end = p;
        } else {
            break;
        }
    }
    let q = skip_blank(b, p, le);
    let mut res = Parsed { marker_start: q, quote_end, marker: None, marker_end: q, content: q, task: false, quoted };
    if q >= le {
        return res;
    }
    let (marker, mend) = match b[q] {
        c @ (b'-' | b'*' | b'+') if q + 1 == le || matches!(b[q + 1], b' ' | b'\t') => (Marker::Bullet(c), q + 1),
        b'0'..=b'9' => {
            let mut k = q;
            while k < le && b[k].is_ascii_digit() {
                k += 1;
            }
            if k - q <= 9 && k < le && matches!(b[k], b'.' | b')') && (k + 1 == le || matches!(b[k + 1], b' ' | b'\t')) {
                let num = text[q..k].parse().unwrap_or(0);
                (Marker::Ordered { num, delim: b[k] }, k + 1)
            } else {
                return res;
            }
        }
        _ => return res,
    };
    res.marker = Some(marker);
    res.marker_end = mend;
    let mut c = skip_blank(b, mend, le);
    // a single space is part of the marker; keep it simple and take all blanks
    if c + 3 <= le && b[c] == b'[' && matches!(b[c + 1], b' ' | b'x' | b'X') && b[c + 2] == b']' && (c + 3 == le || matches!(b[c + 3], b' ' | b'\t')) {
        res.task = true;
        c = skip_blank(b, c + 3, le);
    }
    res.content = c;
    res
}

/// Enter pressed at `cursor`: continues list items / quotes. `None` means a plain newline.
pub fn on_enter(text: &str, cursor: usize) -> Option<EditOp> {
    let cursor = clamp(text, cursor);
    let ls = line_start(text, cursor);
    let le = line_end(text, cursor);
    let p = parse_line(text, ls, le);
    if p.marker.is_none() && !p.quoted {
        return None;
    }
    let b = text.as_bytes();
    if cursor < p.content.min(le) && p.marker.is_some() {
        return None; // inside the marker / indentation
    }
    if p.marker.is_none() && cursor < p.quote_end {
        return None;
    }
    let empty = if p.marker.is_some() { p.content >= le || text[p.content..le].trim().is_empty() } else { text[p.quote_end.min(le)..le].trim().is_empty() };
    if empty {
        if cursor < le && !text[cursor..le].trim().is_empty() {
            return None;
        }
        // Empty item: drop the marker (keep an enclosing quote prefix).
        let keep = if p.marker.is_some() && p.quoted { text[ls..p.quote_end].to_owned() } else { String::new() };
        let at = ls + keep.len();
        return Some(EditOp { range: ls..le, insert: keep, select: at..at });
    }
    let prefix_end = p.marker_start;
    let mut ins = String::from("\n");
    ins.push_str(&text[ls..prefix_end]);
    match p.marker {
        None => {
            // quote only: reuse the whole prefix verbatim
            ins.clear();
            ins.push('\n');
            ins.push_str(&text[ls..p.quote_end]);
        }
        Some(m) => {
            match m {
                Marker::Bullet(c) => ins.push(c as char),
                Marker::Ordered { num, delim } => {
                    ins.push_str(&(num.saturating_add(1)).to_string());
                    ins.push(delim as char);
                }
            }
            // keep the original spacing after the marker
            let sp_end = skip_blank(b, p.marker_end, le);
            let spacing = &text[p.marker_end..sp_end];
            ins.push_str(if spacing.is_empty() { " " } else { spacing });
            if p.task {
                ins.push_str("[ ] ");
            }
        }
    }
    let at = cursor + ins.len();
    Some(EditOp { range: cursor..cursor, insert: ins, select: at..at })
}

/// Tab / Shift+Tab. Indents or outdents the list item under the cursor or every selected
/// line (2 spaces; 4 for ordered items). `None`: nothing to do, the caller inserts a plain
/// tab (indent) or ignores the key (outdent).
pub fn on_tab(text: &str, sel: Range<usize>, outdent: bool) -> Option<EditOp> {
    let (mut s, mut e) = (clamp(text, sel.start), clamp(text, sel.end));
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let first = line_start(text, s);
    let last_pos = if e > s && text.as_bytes()[e - 1] == b'\n' { e - 1 } else { e };
    let last_le = line_end(text, last_pos);
    let region = &text[first..last_le];
    let multi = region.contains('\n');

    struct L {
        old_start: usize,
        old_len: usize, // without newline
        add: usize,
        remove: usize,
    }
    let mut lines: Vec<L> = Vec::new();
    let mut out = String::with_capacity(region.len() + 16);
    let mut ls = first;
    loop {
        let le = line_end(text, ls).min(last_le);
        let line = &text[ls..le];
        let parsed = parse_line(text, ls, le);
        let is_item = parsed.marker.is_some();
        let width = if matches!(parsed.marker, Some(Marker::Ordered { .. })) { 4 } else { 2 };
        let (mut add, mut remove) = (0, 0);
        let applies = multi || is_item || (outdent && line.starts_with([' ', '\t']));
        if applies {
            if outdent {
                if line.starts_with('\t') {
                    remove = 1;
                } else {
                    remove = line.bytes().take(width).take_while(|&c| c == b' ').count();
                }
            } else if !line.trim().is_empty() {
                add = width;
            }
        }
        if add > 0 {
            out.push_str(&" ".repeat(add));
        }
        out.push_str(&line[remove..]);
        lines.push(L { old_start: ls, old_len: le - ls, add, remove });
        if le >= last_le {
            break;
        }
        out.push('\n');
        ls = le + 1;
    }
    if lines.iter().all(|l| l.add == 0 && l.remove == 0) {
        return None;
    }
    let collapsed = s == e;
    let map = |pos: usize, is_start: bool| -> usize {
        if pos > last_le {
            // selection end sitting at the start of the (excluded) following line
            let delta: isize = lines.iter().map(|l| l.add as isize - l.remove as isize).sum();
            return (pos as isize + delta) as usize;
        }
        let mut new_start = first;
        for l in &lines {
            if pos >= l.old_start && pos <= l.old_start + l.old_len {
                let off = pos - l.old_start;
                return if l.add > 0 {
                    if off == 0 && is_start && !collapsed { new_start } else { new_start + l.add + off }
                } else if off <= l.remove {
                    new_start
                } else {
                    new_start + off - l.remove
                };
            }
            new_start += l.old_len + l.add - l.remove + 1;
        }
        pos
    };
    let select = map(s, true)..map(e, false);
    Some(EditOp { range: first..last_le, insert: out, select })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn word_bounds(text: &str, pos: usize) -> (usize, usize) {
    let mut s = pos;
    for c in text[..pos].chars().rev() {
        if is_word(c) {
            s -= c.len_utf8();
        } else {
            break;
        }
    }
    let mut e = pos;
    for c in text[pos..].chars() {
        if is_word(c) {
            e += c.len_utf8();
        } else {
            break;
        }
    }
    (s, e)
}

/// Run length of byte `c` going forward from the start of `s`.
fn run_fwd(s: &str, c: u8) -> usize {
    s.bytes().take_while(|&x| x == c).count()
}

fn run_back(s: &str, c: u8) -> usize {
    s.bytes().rev().take_while(|&x| x == c).count()
}

/// Is a run of `r` marker characters exactly one wrapper for `marker`?
fn run_matches(marker: &str, r: usize) -> bool {
    let m = marker.len();
    let b = marker.as_bytes();
    if !b.iter().all(|&c| c == b[0]) {
        return r >= m;
    }
    r == m || (b[0] == b'*' && r == 3 && m <= 2)
}

enum Wrapped {
    Added,
    Inner,
    Outer,
}

fn wrap_range(text: &str, s: usize, e: usize, marker: &str) -> (EditOp, Wrapped) {
    let m = marker.len();
    let inner = &text[s..e];
    let uniform = marker.bytes().all(|c| Some(c) == marker.bytes().next());
    let c0 = marker.as_bytes()[0];
    if inner.len() >= 2 * m && inner.starts_with(marker) && inner.ends_with(marker) {
        let ok = if uniform {
            let l = run_fwd(inner, c0).min(inner.len() / 2);
            let r = run_back(inner, c0).min(inner.len() / 2);
            l == r && run_matches(marker, l)
        } else {
            true
        };
        if ok {
            let content = inner[m..inner.len() - m].to_owned();
            let n = content.len();
            return (EditOp { range: s..e, insert: content, select: s..s + n }, Wrapped::Inner);
        }
    }
    let before = &text[..s];
    let after = &text[e..];
    if before.ends_with(marker) && after.starts_with(marker) {
        let ok = if uniform {
            let l = run_back(before, c0);
            let r = run_fwd(after, c0);
            l == r && run_matches(marker, l)
        } else {
            true
        };
        if ok {
            return (EditOp { range: s - m..e + m, insert: inner.to_owned(), select: s - m..e - m }, Wrapped::Outer);
        }
    }
    let mut ins = String::with_capacity(inner.len() + 2 * m);
    ins.push_str(marker);
    ins.push_str(inner);
    ins.push_str(marker);
    (EditOp { range: s..e, insert: ins, select: s + m..e + m }, Wrapped::Added)
}

/// Copies the lines touched by `sel` above (`up`) or below the block; the selection ends on the copy that
/// moved in the pressed direction (VS Code "copy line up/down"). Offsets are clamped.
pub fn duplicate_lines(text: &str, sel: Range<usize>, up: bool) -> EditOp {
    let (a, b) = (clamp(text, sel.start), clamp(text, sel.end.max(sel.start)));
    let start = text[..a].rfind('\n').map_or(0, |i| i + 1);
    let end = text[b..].find('\n').map_or(text.len(), |i| b + i);
    let block = &text[start..end];
    if up {
        EditOp { range: start..start, insert: format!("{block}\n"), select: a..b }
    } else {
        let d = block.len() + 1;
        EditOp { range: end..end, insert: format!("\n{block}"), select: a + d..b + d }
    }
}

/// Toggles an inline marker (`**`, `*`, `~~`, `` ` ``, `==`) around the selection, or around
/// the word under the cursor / an empty pair when the selection is empty. With an empty
/// selection the caret is kept where it was (shifted by the marker length).
pub fn toggle_wrap(text: &str, sel: Range<usize>, marker: &str) -> EditOp {
    let (mut s, mut e) = (clamp(text, sel.start), clamp(text, sel.end));
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    if marker.is_empty() {
        return EditOp { range: s..e, insert: text[s..e].to_owned(), select: s..e };
    }
    let m = marker.len();
    if s != e {
        return wrap_range(text, s, e, marker).0;
    }
    let (ws, we) = word_bounds(text, s);
    if ws == we {
        if text[..s].ends_with(marker) && text[s..].starts_with(marker) {
            return EditOp { range: s - m..s + m, insert: String::new(), select: s - m..s - m };
        }
        let ins = format!("{marker}{marker}");
        return EditOp { range: s..s, insert: ins, select: s + m..s + m };
    }
    let (mut op, kind) = wrap_range(text, ws, we, marker);
    let caret = match kind {
        Wrapped::Added => s + m,
        Wrapped::Outer => s - m,
        Wrapped::Inner => s.saturating_sub(m).max(ws),
    };
    op.select = caret..caret;
    op
}

/// Flips the checkbox char at `offset` (the char between `[` and `]`). The caret ends right
/// after the replaced char.
pub fn toggle_task(text: &str, offset: usize) -> Option<EditOp> {
    if offset == 0 || !text.is_char_boundary(offset) {
        return None;
    }
    let c = text[offset..].chars().next()?;
    let b = text.as_bytes();
    if b[offset - 1] != b'[' || b.get(offset + c.len_utf8()) != Some(&b']') {
        return None;
    }
    let new = if c == ' ' { "x" } else { " " };
    let end = offset + c.len_utf8();
    Some(EditOp { range: offset..end, insert: new.to_owned(), select: offset + 1..offset + 1 })
}

/// Link insertion: `[sel](|)` for a selection, `[|]()` otherwise.
pub fn insert_link(text: &str, sel: Range<usize>) -> EditOp {
    let (mut s, mut e) = (clamp(text, sel.start), clamp(text, sel.end));
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    if s == e {
        return EditOp { range: s..s, insert: "[]()".to_owned(), select: s + 1..s + 1 };
    }
    let inner = &text[s..e];
    let at = s + 1 + inner.len() + 2;
    EditOp { range: s..e, insert: format!("[{inner}]()"), select: at..at }
}

/// Where a `[[` link is being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikiCtx<'a> {
    /// Byte just after the `[[`.
    pub start: usize,
    /// Text between `[[` and the cursor.
    pub query: &'a str,
}

/// The cursor is inside an unclosed `[[...` on its line, before any `|` or `#`.
pub fn wikilink_context(text: &str, cursor: usize) -> Option<WikiCtx<'_>> {
    let cursor = clamp(text, cursor);
    let ls = line_start(text, cursor);
    let seg = &text[ls..cursor];
    let idx = seg.rfind("[[")?;
    let query = &seg[idx + 2..];
    if query.contains(['|', '#', ']', '[']) {
        return None;
    }
    Some(WikiCtx { start: ls + idx + 2, query })
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Can the single `$` at `q` open inline math? It needs a non-space char after it; when that
/// char is a digit (`$5 and $6`) a valid closing `$` must also exist on the line.
fn opens_inline(b: &[u8], q: usize) -> bool {
    let Some(&n) = b.get(q + 1) else { return false };
    if is_space(n) || n == b'$' {
        return false;
    }
    if !n.is_ascii_digit() {
        return true;
    }
    let le = memchr::memchr(b'\n', &b[q..]).map_or(b.len(), |p| q + p);
    let mut k = q + 1;
    while let Some(p) = memchr::memchr(b'$', &b[k..le]) {
        let c = k + p;
        if c > q + 1 && !is_space(b[c - 1]) && b[c - 1] != b'\\' && !b.get(c + 1).is_some_and(u8::is_ascii_digit) {
            return true;
        }
        k = c + 1;
    }
    false
}

/// Is the cursor inside `$...$` or `$$...$$`? Scans from the start of the text up to the
/// cursor (memchr speed); `$` next to whitespace does not open/close inline math.
pub fn in_math(text: &str, cursor: usize) -> bool {
    let cursor = clamp(text, cursor);
    let b = text.as_bytes();
    // 0 = none, 1 = inline, 2 = display
    let mut st = 0u8;
    let mut i = 0usize;
    let mut last = 0usize;
    let mut open_at = 0usize;
    while i < cursor {
        let Some(p) = memchr::memchr(b'$', &b[i..cursor]) else { break };
        let q = i + p;
        let bs = b[..q].iter().rev().take_while(|&&c| c == b'\\').count();
        if bs % 2 == 1 {
            i = q + 1;
            continue;
        }
        if st == 1 && memchr::memchr(b'\n', &b[last..q]).is_some() {
            st = 0;
        }
        last = q;
        let double = b.get(q + 1) == Some(&b'$');
        if double {
            if q + 2 > cursor {
                break; // cursor between the two dollars
            }
            st = match st {
                0 => 2,
                _ => 0,
            };
            i = q + 2;
        } else {
            match st {
                0 => {
                    if opens_inline(b, q) {
                        st = 1;
                        open_at = q;
                    }
                }
                1 if q > 0
                    && (!matches!(b[q - 1], b' ' | b'\t' | b'\n' | b'\r')
                        // `$x + y $`: closes under the relaxed rule of `math`
                        || crate::math::inline_end(b, open_at, b.len()).is_some_and(|m| m.end == q + 1)) =>
                {
                    st = 0;
                }
                _ => {}
            }
            i = q + 1;
        }
    }
    if st == 1 && memchr::memchr(b'\n', &b[last..cursor]).is_some() {
        st = 0;
    }
    st != 0
}

/// Does a math formula read as LaTeX rather than Typst? One byte scan: a backslash before an ASCII
/// letter (`\alpha`), `\\`, or a braced script `^{` / `_{`. Typst only uses `\` for escapes (`\$`,
/// `\#`), the line break (`\` + space) and `\u{..}`, so none of those count.
pub fn looks_like_latex(src: &str) -> bool {
    let b = src.as_bytes();
    // Backslashes inside a Typst string ("a\nb", "C:\dir") are string escapes, not LaTeX commands.
    let mut in_str = false;
    b.iter().enumerate().any(|(i, &c)| {
        if c == b'"' && (i == 0 || b[i - 1] != b'\\') {
            in_str = !in_str;
        }
        !in_str
            && match (c, b.get(i + 1)) {
                (b'\\', Some(&n)) => (n.is_ascii_alphabetic() && !(n == b'u' && b.get(i + 2) == Some(&b'{'))) || n == b'\\',
                (b'^' | b'_', Some(&b'{')) => true,
                _ => false,
            }
    })
}

/// The formula text around the cursor when it is inside `$..$` / `$$..$$`: from the previous to the
/// next unescaped `$` (shortcut: bounded by `$` only, a display formula includes one of its `$$`).
pub fn math_at(text: &str, cursor: usize) -> Option<&str> {
    let cursor = clamp(text, cursor);
    if !in_math(text, cursor) {
        return None;
    }
    let b = text.as_bytes();
    let unescaped = |q: usize| b[..q].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 0;
    let start = (0..cursor).rev().find(|&q| b[q] == b'$' && unescaped(q)).map_or(0, |q| q + 1);
    let end = (cursor..b.len()).find(|&q| b[q] == b'$' && unescaped(q)).unwrap_or(b.len());
    Some(&text[start..end])
}

/// Returns `(hash count, end of "#... " prefix)` when the line is an ATX heading.
fn heading_prefix(text: &str, ls: usize, le: usize) -> Option<(u8, usize, usize)> {
    let b = text.as_bytes();
    let mut i = ls;
    while i < le && i - ls < 3 && b[i] == b' ' {
        i += 1;
    }
    let hs = i;
    while i < le && b[i] == b'#' {
        i += 1;
    }
    let n = i - hs;
    if !(1..=6).contains(&n) || (i < le && !matches!(b[i], b' ' | b'\t')) {
        return None;
    }
    let end = skip_blank(b, i, le);
    Some((n as u8, hs, end))
}

/// Heading level of the line under the cursor (`# ` .. `###### `).
pub fn heading_line(text: &str, cursor: usize) -> Option<u8> {
    let cursor = clamp(text, cursor);
    let (ls, le) = (line_start(text, cursor), line_end(text, cursor));
    heading_prefix(text, ls, le).map(|h| h.0)
}

/// Sets the heading level of the cursor's line to `level` (1..=6); the same level again, or
/// 0, removes the heading prefix.
pub fn set_heading(text: &str, cursor: usize, level: u8) -> EditOp {
    let cursor = clamp(text, cursor);
    let (ls, le) = (line_start(text, cursor), line_end(text, cursor));
    let level = level.min(6);
    let cur = heading_prefix(text, ls, le);
    let (rs, re) = match cur {
        Some((_, hs, end)) => (hs, end),
        None => (ls, ls),
    };
    let new = if level == 0 || cur.map(|c| c.0) == Some(level) { String::new() } else { format!("{} ", "#".repeat(level as usize)) };
    let caret = if cursor < rs {
        cursor
    } else if cursor <= re {
        rs + new.len()
    } else {
        cursor + new.len() - (re - rs)
    };
    EditOp { range: rs..re, insert: new, select: caret..caret }
}

#[cfg(test)]
mod math_auto_tests {
    use super::{looks_like_latex as l, math_at};

    #[test]
    fn detects_latex_only_by_its_own_markers() {
        for s in [r"\alpha + 1", r"\frac{a}{b}", r"x^{2}", r"a_{i}", r"a \\ b", r"1+\sin x"] {
            assert!(l(s), "{s}");
        }
        for s in [
            "",
            "alpha + beta",
            "x^2 + y_1",
            "integral_0^1 x dif x",
            r"\$5",
            r"\# x",
            r"a \ b",
            r"a \",
            r"\u{1F600}",
            r"\1",
            "frac(a, b)",
            r#""a\nb" + x"#,
            r#"op("C:\dir")"#,
        ] {
            assert!(!l(s), "{s}");
        }
    }

    #[test]
    fn math_at_is_bounded_to_the_surrounding_formula() {
        let t = r"a $\alpha$ b $x$ c";
        assert_eq!(math_at(t, 5), Some(r"\alpha"));
        assert_eq!(math_at(t, 14), Some("x"));
        assert_eq!(math_at(t, 0), None);
        assert_eq!(math_at(t, t.len()), None);
        assert_eq!(math_at("$$", 1), None);
    }
}

#[cfg(test)]
mod math_auto_bench {
    /// Run with `cargo test -p iron-sheets-md --release detect_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn detect_cost() {
        let fs = [
            r"\int_0^1 \frac{x}{2} \, dx = \begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            "integral_0^1 x dif x + sum_(i=1)^n i^2 + a_1 b_2",
            "a^2 + b^2 = c^2",
        ];
        let t = std::time::Instant::now();
        let mut n = 0;
        for _ in 0..200_000 {
            for f in &fs {
                n += super::looks_like_latex(std::hint::black_box(f)) as usize;
            }
        }
        eprintln!("looks_like_latex: {:.1} ns per formula ({n})", t.elapsed().as_nanos() as f64 / 600_000.0);
    }
}

#[cfg(test)]
mod duplicate_lines_tests {
    use super::duplicate_lines as d;

    fn run(t: &str, sel: std::ops::Range<usize>, up: bool) -> (String, std::ops::Range<usize>) {
        let op = d(t, sel, up);
        (op.apply(t), op.select)
    }

    #[test]
    fn single_line_down_keeps_the_caret_on_the_copy() {
        assert_eq!(run("a\nbcd\ne", 3..3, false), ("a\nbcd\nbcd\ne".into(), 7..7));
    }

    #[test]
    fn single_line_up_keeps_the_caret_on_the_upper_copy() {
        assert_eq!(run("a\nbcd\ne", 3..3, true), ("a\nbcd\nbcd\ne".into(), 3..3));
    }

    #[test]
    fn multi_line_selection_copies_whole_lines() {
        let t = "a\nbc\nde\nf";
        assert_eq!(run(t, 3..6, false), ("a\nbc\nde\nbc\nde\nf".into(), 9..12));
        assert_eq!(run(t, 3..6, true), ("a\nbc\nde\nbc\nde\nf".into(), 3..6));
    }

    #[test]
    fn last_line_without_trailing_newline() {
        assert_eq!(run("a\nb", 3..3, false), ("a\nb\nb".into(), 5..5));
        assert_eq!(run("a\nb", 2..2, true), ("a\nb\nb".into(), 2..2));
        assert_eq!(run("", 0..0, false), ("\n".into(), 1..1));
    }
}
