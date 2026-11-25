//! Inline `$...$` math: the one rule shared by the preview parser, the editor highlighter and the
//! editing helpers.
//!
//! # The rule
//!
//! comrak (`math_dollars`) follows pandoc: a single `$` opens math only if a non-space follows,
//! and the closing `$` must not be preceded by whitespace nor followed by a digit. Typst authors
//! write `$ x + y $` and `$x + y $`, which pandoc leaves as text. This module accepts those too,
//! without turning prices or shell variables into math.
//!
//! For a single `$` at `i` (the caller has already handled `\$`, code spans and `$$`):
//!
//! 1. The opener may be followed by spaces or tabs, but then some content must follow on the same
//!    line (`$` at end of line, or `$ ` + newline, is text).
//! 2. The closer is the first following `$` that is not directly preceded by a backslash
//!    (exactly comrak's search). A blank line before it ends the attempt (paragraphs end there);
//!    soft line breaks are fine. A digit directly after the closer ends the attempt (`$5 or $6`).
//!    A failed attempt does not look at further `$`: they become openers themselves. So every
//!    byte is looked at a bounded number of times and the scan is linear.
//! 3. Strict case (no whitespace after the opener, none before the closer): accepted, exactly
//!    what comrak does.
//! 4. Relaxed case (whitespace after the opener and/or before the closer) is accepted only if
//!    * the whitespace before the closer is a space or tab (not a line break),
//!    * with whitespace before the closer: no letter, digit or `$` directly follows the closer
//!      (`$HOME and $PATH` is text, `$x + y $.` is math) and, after skipping blanks, no digit
//!      (`$ 5 and $ 10` is text),
//!    * with whitespace before the closer: the opener is not directly followed by a digit
//!      (`$5 and $ more`),
//!    * the trimmed content is a single word, or contains at least one character that is not a
//!      letter, digit or whitespace (`$PATH are $` is text, `$ a + b $` and `$ x $` are math).
//!
//! `$$ ... $$` is not affected: comrak already accepts any whitespace there.
//!
//! # How the parser uses it
//!
//! comrak cannot be told about the relaxed rule. [`mask_relaxed`] finds the relaxed formulas and
//! replaces the one blank byte next to the delimiter by [`MASK`] (a single byte, so offsets and
//! line numbers stay identical). comrak then sees a strict formula; [`unmask`] turns the byte back
//! into a space wherever text is read out of the tree. Text without a relaxed formula is passed
//! to comrak untouched and costs one `memchr` for `$`.

use std::borrow::Cow;

/// Placeholder byte (ASCII, not whitespace, not punctuation for comrak).
pub(crate) const MASK: u8 = 0x01;

/// comrak's `isspace`.
#[inline]
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

#[inline]
fn is_blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t')
}

#[inline]
fn is_wordy(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c >= 0x80
}

/// A recognised inline formula. `end` is exclusive (one past the closing `$`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineMath {
    pub end: usize,
    /// Position of the blank byte after the opener, if the formula is relaxed there.
    pub open_blank: Option<usize>,
    /// Position of the blank byte before the closer, if the formula is relaxed there.
    pub close_blank: Option<usize>,
}

impl InlineMath {
    /// Does comrak need help (masking) to see this formula?
    pub fn is_relaxed(&self) -> bool {
        self.open_blank.is_some() || self.close_blank.is_some()
    }
}

/// Is there a blank line (a line with only spaces/tabs) inside `b`?
fn has_blank_line(b: &[u8]) -> bool {
    for p in memchr::memchr_iter(b'\n', b) {
        let mut k = p + 1;
        while k < b.len() && is_blank(b[k]) {
            k += 1;
        }
        if k < b.len() && matches!(b[k], b'\n' | b'\r') {
            return true;
        }
    }
    false
}

/// Applies the rule above to the single `$` at `b[i]`, looking at `b[..limit]` only.
/// Returns `None` if it does not open inline math.
pub fn inline_end(b: &[u8], i: usize, limit: usize) -> Option<InlineMath> {
    let limit = limit.min(b.len());
    if i >= limit || b[i] != b'$' {
        return None;
    }
    let j = i + 1;
    if j >= limit || b[j] == b'$' {
        return None;
    }
    let mut k = j;
    let mut open_blank = None;
    if is_blank(b[j]) {
        open_blank = Some(j);
        while k < limit && is_blank(b[k]) {
            k += 1;
        }
    }
    // Content must start on this line.
    if k >= limit || is_space(b[k]) || b[k] == b'$' {
        return None;
    }
    let digit_open = open_blank.is_none() && b[k].is_ascii_digit();

    let mut from = k + 1;
    loop {
        let q = from + memchr::memchr(b'$', &b[from..limit])?;
        if has_blank_line(&b[from..q]) {
            return None;
        }
        let prev = b[q - 1];
        if prev == b'\\' {
            from = q + 1;
            continue;
        }
        let next = b.get(q + 1).copied().filter(|_| q + 1 < limit);
        if next.is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
        let before_ws = is_space(prev);
        if open_blank.is_none() && !before_ws {
            return Some(InlineMath { end: q + 1, open_blank: None, close_blank: None });
        }
        // Relaxed.
        if before_ws {
            if !is_blank(prev) || digit_open {
                return None;
            }
            if next.is_some_and(|c| is_wordy(c) || c == b'$') {
                return None;
            }
            let mut a = q + 1;
            while a < limit && is_blank(b[a]) {
                a += 1;
            }
            if a < limit && b[a].is_ascii_digit() {
                return None;
            }
        }
        let mut ce = q;
        while ce > k && is_space(b[ce - 1]) {
            ce -= 1;
        }
        if !word_or_symbolic(&b[k..ce]) {
            return None;
        }
        return Some(InlineMath { end: q + 1, open_blank, close_blank: before_ws.then_some(q - 1) });
    }
}

/// A single word, or anything containing a character that is not letter, digit or whitespace.
fn word_or_symbolic(content: &[u8]) -> bool {
    let mut spaced = false;
    for &c in content {
        if is_space(c) {
            spaced = true;
        } else if !is_wordy(c) {
            return true;
        }
    }
    !spaced
}

/// Replaces the blanks next to the delimiters of every relaxed formula in `text` by [`MASK`], so
/// that comrak accepts them. Returns `None` if there is nothing to do (the common case).
pub fn mask_relaxed(text: &str) -> Option<String> {
    let b = text.as_bytes();
    memchr::memchr(b'$', b)?;
    if !maybe_relaxed(b) {
        return None;
    }
    let mut positions: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while let Some(p) = memchr::memchr3(b'$', b'`', b'\\', &b[i..]) {
        let q = i + p;
        match b[q] {
            b'\\' => i = q + 2,
            b'`' => {
                // Code span or fence: skip to the closing run of the same length (a fence's
                // body is skipped the same way); an unmatched run is plain text.
                let run = b[q..].iter().take_while(|&&c| c == b'`').count();
                i = skip_code(b, q + run, run).unwrap_or(q + run);
            }
            _ => {
                let run = b[q..].iter().take_while(|&&c| c == b'$').count();
                if run >= 2 {
                    // `$$ ... $$`: the first run of two or more `$` closes it.
                    i = skip_display(b, q + run, run).map_or(q + run, |e| e.max(q + run));
                } else if b.get(q + 1) == Some(&b'`') {
                    // `$`code`$` is comrak's code math.
                    i = memchr::memmem::find(&b[q + 2..], b"`$").map_or(q + 1, |e| q + 2 + e + 2);
                } else if let Some(m) = inline_end(b, q, b.len()) {
                    positions.extend(m.open_blank);
                    positions.extend(m.close_blank);
                    i = m.end;
                } else {
                    i = q + 1;
                }
            }
        }
        if i >= b.len() {
            break;
        }
    }
    if positions.is_empty() {
        return None;
    }
    let mut out = b.to_vec();
    for p in positions {
        out[p] = MASK;
    }
    String::from_utf8(out).ok()
}

/// Cheap pre-check: is there any relaxed formula if code spans and escapes are ignored? Almost all
/// documents answer no here and skip the careful scan.
fn maybe_relaxed(b: &[u8]) -> bool {
    let mut i = 0usize;
    while let Some(p) = memchr::memchr(b'$', &b[i..]) {
        let q = i + p;
        if b.get(q + 1) == Some(&b'$') {
            i = q + b[q..].iter().take_while(|&&c| c == b'$').count();
            continue;
        }
        match inline_end(b, q, b.len()) {
            Some(m) if m.is_relaxed() => return true,
            Some(m) => i = m.end,
            None => i = q + 1,
        }
    }
    false
}

/// End (exclusive) of the code span whose opening run of `run` backticks ends at `from`.
fn skip_code(b: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut k = from;
    while let Some(p) = memchr::memchr(b'`', &b[k..]) {
        let s = k + p;
        let n = b[s..].iter().take_while(|&&c| c == b'`').count();
        if n == run {
            return Some(s + n);
        }
        k = s + n;
    }
    None
}

/// End (exclusive) of the `$$` formula whose opening run ends at `from`.
fn skip_display(b: &[u8], from: usize, run: usize) -> Option<usize> {
    if run != 2 {
        return None;
    }
    let mut k = from;
    while let Some(p) = memchr::memchr(b'$', &b[k..]) {
        let s = k + p;
        let n = b[s..].iter().take_while(|&&c| c == b'$').count();
        if has_blank_line(&b[from..s]) {
            return None;
        }
        if n >= 2 {
            return Some(s + 2);
        }
        k = s + n;
    }
    None
}

/// Turns [`MASK`] bytes back into spaces.
pub(crate) fn unmask(s: &str) -> Cow<'_, str> {
    if memchr::memchr(MASK, s.as_bytes()).is_some() { Cow::Owned(s.replace(MASK as char, " ")) } else { Cow::Borrowed(s) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slice of the first inline formula in `s` (`None` if none), using the same walk as the
    /// parser prescan but with a code span / escape aware loop of its own.
    fn first(s: &str) -> Option<&str> {
        let b = s.as_bytes();
        let q = memchr::memchr(b'$', b)?;
        inline_end(b, q, b.len()).map(|m| &s[q..m.end])
    }

    fn masked(s: &str) -> String {
        mask_relaxed(s).unwrap_or_else(|| s.to_owned()).replace(MASK as char, "_")
    }

    #[test]
    fn strict_cases_are_left_to_comrak() {
        assert_eq!(first("a $x+y$ b"), Some("$x+y$"));
        assert!(mask_relaxed("a $x+y$ b").is_none());
        assert!(!inline_end(b"$x$", 0, 3).unwrap().is_relaxed());
    }

    #[test]
    fn space_before_closing() {
        assert_eq!(masked("a $x + y $ b"), "a $x + y_$ b");
        assert_eq!(masked("$x + y $."), "$x + y_$.");
        assert_eq!(masked("$x\t$"), "$x_$");
    }

    #[test]
    fn spaces_on_both_sides() {
        assert_eq!(masked("so $ x + y $ done"), "so $_x + y_$ done");
        assert_eq!(masked("$ x $"), "$_x_$");
        assert_eq!(masked("$  x$"), "$_ x$");
    }

    #[test]
    fn space_after_opening_only() {
        assert_eq!(masked("$ x+y$"), "$_x+y$");
    }

    #[test]
    fn currency_stays_text() {
        for s in ["costs $5 and $10", "$5 – $10", "$ 5", "a lone $", "$", "$ 5 and $ 10", "$5 and $ later", "pay $ 5 or $6"] {
            assert!(mask_relaxed(s).is_none(), "{s:?}");
            assert!(first(s).is_none() || !first(s).unwrap().contains(' ') || s == "$5 – $10", "{s:?}");
        }
        assert!(first("costs $5 and $10").is_none());
        assert!(first("$ 5").is_none());
    }

    #[test]
    fn shell_variables_stay_text() {
        assert!(mask_relaxed("echo $HOME and $PATH").is_none());
        assert!(mask_relaxed("$PATH are $ important").is_none());
        assert!(mask_relaxed("use $a and $b").is_none());
    }

    #[test]
    fn newline_next_to_delimiters() {
        assert!(mask_relaxed("$\nx$").is_none());
        assert!(mask_relaxed("$ \nx$").is_none());
        assert!(mask_relaxed("$x +\n$").is_none());
        // soft break inside is fine
        assert_eq!(masked("$x +\ny $"), "$x +\ny_$");
        // blank line ends the attempt
        assert!(mask_relaxed("$x +\n\ny $").is_none());
    }

    #[test]
    fn escaped_dollar() {
        assert!(mask_relaxed("\\$x + y \\$").is_none());
        // escaped dollar inside the formula does not close it
        assert_eq!(masked("$a \\$ b $"), "$a \\$ b_$");
    }

    #[test]
    fn code_is_skipped() {
        assert!(mask_relaxed("`$x + y $`").is_none());
        assert!(mask_relaxed("```\n$x + y $\n```\n").is_none());
        assert!(mask_relaxed("``a ` $x + y $``").is_none());
        assert_eq!(masked("`c` $x + y $"), "`c` $x + y_$");
    }

    #[test]
    fn display_is_not_touched() {
        assert!(mask_relaxed("$$ x + y $$").is_none());
        assert!(mask_relaxed("$$\nx + y\n$$\n").is_none());
        // a `$` inside display math is not an opener
        assert!(mask_relaxed("$$ a $ b $ c $$").is_none());
    }

    #[test]
    fn word_only_content() {
        assert_eq!(masked("$ n $"), "$_n_$");
        assert!(mask_relaxed("$a b $").is_none());
        assert_eq!(masked("$a_b $"), "$a_b_$");
    }

    #[test]
    fn several_formulas_and_text_after() {
        assert_eq!(masked("$a + b $ and $ c + d $ end"), "$a + b_$ and $_c + d_$ end");
        // after a failed attempt the next `$` opens
        assert_eq!(masked("$5 and $ x + y $"), "$5 and $_x + y_$");
    }

    #[test]
    fn closer_followed_by_digit_or_letter() {
        assert!(mask_relaxed("$x + y $5").is_none());
        assert!(mask_relaxed("$x + y $z").is_none());
        assert!(mask_relaxed("$x + y $ 5").is_none());
    }

    #[test]
    fn unmask_roundtrip() {
        let m = mask_relaxed("$ x + y $ é").unwrap();
        assert_eq!(unmask(&m), "$ x + y $ é");
        assert!(matches!(unmask("plain"), Cow::Borrowed(_)));
    }

    #[test]
    fn linear_on_pathological_input() {
        // Many dollars with no valid pair: must finish instantly.
        let s = "$ a ".repeat(200_000);
        let t = std::time::Instant::now();
        let _ = mask_relaxed(&s);
        assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
        let s = "$x ".repeat(200_000);
        let t = std::time::Instant::now();
        let _ = mask_relaxed(&s);
        assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
    }
}
