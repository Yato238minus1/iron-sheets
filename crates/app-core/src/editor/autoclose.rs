//! Auto-close of brackets and quotes as pure text edits: what typing one char or Backspace does at a selection.
//! Skip-over and Backspace-deletes-both apply only to closers this module inserted (`Closers`), so `)` can still be typed
//! in front of an existing `)`. `$` is not paired at all: no context rule keeps `$5` from becoming `$5$`.
use iron_sheets_md::edit::EditOp;
use std::ops::Range;

fn closer(open: char, markdown: bool) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' | '\'' => Some(open),
        '`' if markdown => Some(open),
        _ => None,
    }
}

fn is_closer(c: char, markdown: bool) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'') || (markdown && c == '`')
}

fn symmetric(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

/// How far back `in_code` looks. shortcut: beyond this the answer is "treat as code" (no auto-close), so a keystroke
/// never costs O(note); upgrade with the highlighter's block state if auto-close deep inside huge notes matters.
const SCAN: usize = 64 * 1024;

/// Inside a fenced block or an inline code span at byte `at` (or too far from the top of the note to tell cheaply).
fn in_code(text: &str, at: usize) -> bool {
    let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
    if ls > SCAN || at - ls > SCAN {
        return true;
    }
    let fences = text[..ls].lines().filter(|l| l.trim_start().starts_with("```") || l.trim_start().starts_with("~~~")).count();
    fences % 2 == 1 || text[ls..at].matches('`').count() % 2 == 1
}

/// Byte positions of the closers inserted by auto-close that are still waiting to be typed over.
#[derive(Default)]
pub struct Closers {
    /// Text length the positions are valid for.
    len: usize,
    at: Vec<usize>,
}

impl Closers {
    pub fn forget(&mut self) {
        self.at.clear();
    }

    /// Re-anchors after edits made elsewhere (typing between a pair): positions move by the length change; one that no
    /// longer sits at or after the cursor on a closer char is dropped. shortcut: assumes such edits happened at the cursor.
    fn sync(&mut self, text: &str, cursor: usize, markdown: bool) {
        let delta = text.len() as isize - self.len as isize;
        self.len = text.len();
        if delta == 0 {
            return;
        }
        self.at.retain_mut(|p| {
            *p = p.saturating_add_signed(delta);
            *p >= cursor && text.get(*p..).and_then(|t| t.chars().next()).is_some_and(|c| is_closer(c, markdown))
        });
    }

    /// Records `op` (applied to the synced text): drops closers it removed, shifts those behind it.
    fn applied(&mut self, op: &EditOp) {
        let d = op.insert.len() as isize - op.range.len() as isize;
        self.at.retain(|&p| !op.range.contains(&p));
        for p in &mut self.at {
            if *p >= op.range.end {
                *p = p.saturating_add_signed(d);
            }
        }
        self.len = self.len.saturating_add_signed(d);
    }

    /// The edit for typing `ch` with `sel` selected; `None` leaves the keystroke to the text field.
    pub fn on_text(&mut self, text: &str, sel: Range<usize>, ch: char, markdown: bool) -> Option<EditOp> {
        let (s, e) = (sel.start, sel.end);
        if !text.is_char_boundary(s) || !text.is_char_boundary(e) {
            return None;
        }
        // Ordinary letters leave here: no scan of the note on a plain keystroke.
        if !is_closer(ch, markdown) && closer(ch, markdown).is_none() {
            return None;
        }
        self.sync(text, s, markdown);
        let next = text[e..].chars().next();
        if s == e && next == Some(ch) && self.at.contains(&s) {
            // Typing over the closer auto-close put there (before the code check: the closer of `code` is typed in code).
            let at = s + ch.len_utf8();
            let op = EditOp { range: s..s, insert: String::new(), select: at..at };
            self.at.retain(|&p| p != s);
            return Some(op);
        }
        if markdown && in_code(text, s) {
            return None;
        }
        let close = closer(ch, markdown)?;
        if s != e {
            let inner = &text[s..e];
            let op = EditOp { range: s..e, insert: format!("{ch}{inner}{close}"), select: s + ch.len_utf8()..e + ch.len_utf8() };
            self.applied(&op);
            return Some(op);
        }
        let prev = text[..s].chars().next_back();
        if symmetric(ch) && prev.is_some_and(|p| p.is_alphanumeric() || p == ch) {
            return None;
        }
        if next.is_some_and(|n| !n.is_whitespace() && !is_closer(n, markdown)) {
            return None;
        }
        if ch == '`' && text[text[..s].rfind('\n').map_or(0, |i| i + 1)..s].trim_start().chars().all(|c| c == '`') && prev == Some('`') {
            return None; // third backtick of a fence
        }
        let at = s + ch.len_utf8();
        let op = EditOp { range: s..s, insert: format!("{ch}{close}"), select: at..at };
        self.applied(&op);
        self.at.push(at);
        Some(op)
    }

    /// Backspace between an empty pair that auto-close made removes both chars.
    pub fn on_backspace(&mut self, text: &str, sel: Range<usize>, markdown: bool) -> Option<EditOp> {
        if sel.start != sel.end || !text.is_char_boundary(sel.start) {
            return None;
        }
        self.sync(text, sel.start, markdown);
        if !self.at.contains(&sel.start) {
            return None;
        }
        let (prev, next) = (text[..sel.start].chars().next_back()?, text[sel.start..].chars().next()?);
        if closer(prev, markdown) != Some(next) || (markdown && in_code(text, sel.start - prev.len_utf8())) {
            return None;
        }
        let (a, b) = (sel.start - prev.len_utf8(), sel.start + next.len_utf8());
        let op = EditOp { range: a..b, insert: String::new(), select: a..a };
        self.applied(&op);
        Some(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str, at: usize, ch: char) -> Option<String> {
        Closers::default().on_text(text, at..at, ch, true).map(|o| o.apply(text))
    }

    /// Types `s` char by char at `cur` like the editor does: the op when auto-close handles the char, else the plain char.
    fn type_all(c: &mut Closers, text: &mut String, cur: &mut usize, s: &str) {
        for ch in s.chars() {
            match c.on_text(text, *cur..*cur, ch, true) {
                Some(op) => {
                    *text = op.apply(text);
                    *cur = op.select.start;
                }
                None => {
                    text.insert(*cur, ch);
                    *cur += ch.len_utf8();
                }
            }
        }
    }

    #[test]
    fn pairs_brackets_and_quotes() {
        assert_eq!(typed("", 0, '('), Some("()".into()));
        assert_eq!(Closers::default().on_text("", 0..0, '[', true).map(|o| o.select), Some(1..1));
        assert_eq!(typed("a ", 2, '"'), Some("a \"\"".into()));
        assert_eq!(typed("x", 1, 'y'), None, "plain chars are left alone");
    }

    #[test]
    fn dollar_is_never_paired() {
        assert_eq!(typed("a ", 2, '$'), None);
        assert_eq!(typed("costs ", 6, '$'), None, "`$5` stays `$5`");
    }

    #[test]
    fn quotes_and_code_marks_not_inside_a_word() {
        assert_eq!(typed("don", 3, '\''), None);
        assert_eq!(typed("don ", 4, '\''), Some("don ''".into()));
        assert_eq!(typed("(", 1, '"'), Some("(\"\"".into()));
        assert_eq!(typed("x", 0, '\''), None, "not before a letter");
        assert_eq!(typed("x", 0, '"'), None, "not before a letter");
    }

    #[test]
    fn only_an_auto_inserted_closer_is_skipped() {
        let mut c = Closers::default();
        let (mut t, mut cur) = (String::new(), 0);
        type_all(&mut c, &mut t, &mut cur, "(a");
        assert_eq!(t, "(a)");
        let o = c.on_text(&t, 2..2, ')', true).expect("skip");
        assert_eq!((o.insert.as_str(), o.select), ("", 3..3));
        // A closer that was already there is not ours: `)` is inserted (left to the text field).
        assert!(Closers::default().on_text("f()", 2..2, ')', true).is_none());
        assert!(Closers::default().on_text("\"a\"", 2..2, '"', true).is_none());
    }

    #[test]
    fn nested_pairs_skip_in_order() {
        let mut c = Closers::default();
        let (mut t, mut cur) = (String::new(), 0);
        type_all(&mut c, &mut t, &mut cur, "f(g[x");
        assert_eq!(t, "f(g[x])");
        for (at, ch) in [(5, ']'), (6, ')')] {
            let o = c.on_text(&t, at..at, ch, true).expect("skip");
            assert_eq!(o.select, at + 1..at + 1);
        }
    }

    #[test]
    fn closing_backtick_of_a_code_span_skips_over() {
        let mut c = Closers::default();
        let (mut t, mut cur) = (String::new(), 0);
        type_all(&mut c, &mut t, &mut cur, "`code");
        assert_eq!(t, "`code`");
        let o = c.on_text(&t, 5..5, '`', true).expect("skip, not a stray backtick");
        assert_eq!((o.insert.as_str(), o.select), ("", 6..6));
    }

    #[test]
    fn selection_is_wrapped() {
        let o = Closers::default().on_text("a word b", 2..6, '[', true).expect("wrap");
        assert_eq!(o.apply("a word b"), "a [word] b");
        assert_eq!(o.select, 3..7);
    }

    #[test]
    fn not_before_a_word_and_not_in_code() {
        assert_eq!(typed("x", 0, '('), None, "next char is a word char");
        assert_eq!(typed("a `b ", 5, '('), None, "inside an inline code span");
        assert_eq!(typed("```\ncode ", 9, '('), None, "inside a fence");
        assert_eq!(typed("```\ncode\n```\n", 13, '('), Some("```\ncode\n```\n()".into()), "after the fence");
        assert_eq!(typed("``", 2, '`'), None, "third backtick of a fence");
    }

    #[test]
    fn far_from_the_top_of_a_note_nothing_is_scanned() {
        let big = "word word\n".repeat(20_000);
        assert_eq!(typed(&big, big.len(), '('), None, "bails out instead of scanning the whole note");
        let small = "word word\n".repeat(100);
        assert!(typed(&small, small.len(), '(').is_some());
        let long_line = "a ".repeat(40_000);
        assert_eq!(typed(&long_line, long_line.len(), '('), None, "a huge single line is not scanned either");
    }

    #[test]
    fn typst_has_no_backtick_or_dollar_pairs() {
        assert_eq!(Closers::default().on_text("", 0..0, '$', false), None);
        assert_eq!(Closers::default().on_text("", 0..0, '(', false).map(|o| o.insert), Some("()".into()));
    }

    #[test]
    fn backspace_removes_only_our_empty_pair() {
        let mut c = Closers::default();
        let o = c.on_text("a  b", 2..2, '(', true).expect("pair");
        let t = o.apply("a  b");
        assert_eq!(t, "a () b");
        let o = c.on_backspace(&t, 3..3, true).expect("pair");
        assert_eq!(o.apply(&t), "a  b");
        assert_eq!(o.select, 2..2);
        // Typed by hand (not tracked): a normal Backspace.
        assert!(Closers::default().on_backspace("a()b", 2..2, true).is_none());
        let mut c = Closers::default();
        c.on_text("", 0..0, '(', true).expect("pair");
        assert!(c.on_backspace("(x)", 2..2, true).is_none());
        assert!(c.on_backspace("()", 0..2, true).is_none(), "a selection is deleted normally");
    }

    #[test]
    fn backspace_pair_delete_respects_code() {
        let mut c = Closers::default();
        c.at.push(2);
        c.len = 3;
        assert!(c.on_backspace("`()", 2..2, true).is_none(), "inside an inline code span");
    }
}
