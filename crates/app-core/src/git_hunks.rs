//! Hunk-level edits of a note against a text it differs from (`HEAD` or the index): revert as a buffer edit, stage and
//! unstage as new index blobs. Pure functions on text and bytes; the git calls are in `git.rs`.
//!
//! Lines are the diff's lines: the text split at `\n`, so a final newline leaves an empty last line. Revert works on the
//! LF buffer text. Stage and unstage work on raw bytes (CRLF and BOM kept as stored), taking the lines of the buffer in
//! the file's own style, which is what `git add -p` would store (no `core.autocrlf`, no attributes).
use crate::git::{DiffMark, diff_lines};
use iron_sheets_core::textio::{self, LineEnding};
use std::ops::Range;

/// The file's on-disk style, kept when the buffer is encoded for the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStyle {
    pub ending: LineEnding,
    pub bom: bool,
}

impl FileStyle {
    pub fn of(raw: &[u8]) -> FileStyle {
        let (ending, bom) = textio::detect_style(raw);
        FileStyle { ending, bom }
    }
}

pub const STALE: &str = "The note changed since this change was shown; try again";

/// Half-open line ranges that overlap; an empty range (a deletion between two lines) also touches the ends of the other.
fn touches(a: (u32, u32), b: (u32, u32)) -> bool {
    if a.0 == a.1 || b.0 == b.1 { a.0 <= b.1 && b.0 <= a.1 } else { a.0 < b.1 && b.0 < a.1 }
}

/// A line of a raw file: its bytes and the terminator it had (`None` for the last piece, which has none).
#[derive(Clone, Copy)]
struct Line<'a> {
    content: &'a [u8],
    term: Option<&'static [u8]>,
}

fn raw_lines(b: &[u8]) -> Vec<Line<'_>> {
    let mut pieces: Vec<&[u8]> = b.split(|&c| c == b'\n').collect();
    let last = pieces.pop().unwrap_or_default();
    let mut lines: Vec<Line> = pieces
        .into_iter()
        .map(|p| match p.strip_suffix(b"\r") {
            Some(c) => Line { content: c, term: Some(b"\r\n") },
            None => Line { content: p, term: Some(b"\n") },
        })
        .collect();
    lines.push(Line { content: last, term: None });
    lines
}

/// The file's bytes: every line but the last with its own terminator (`ending` for a line that had none, the old last one),
/// the last as it is. A line that was terminated and now ends the file loses its terminator, as in the file git would store.
fn join_raw(lines: &[Line], ending: LineEnding) -> Vec<u8> {
    let fallback: &[u8] = if ending == LineEnding::Crlf { b"\r\n" } else { b"\n" };
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        out.extend_from_slice(l.content);
        if i + 1 < lines.len() {
            out.extend_from_slice(l.term.unwrap_or(fallback));
        }
    }
    out
}

fn norm(raw: &[u8], what: &str) -> Result<String, String> {
    textio::decode_text_lf(raw.to_vec()).ok_or_else(|| format!("The {what} version is a binary file"))
}

/// The buffer edit (byte range, replacement) that puts the `old` lines of `mark` back, or `None` when `mark` is not a change
/// of `text` against `old` any more (the buffer moved on since the mark was computed).
pub fn revert_edit(text: &str, old: &str, mark: &DiffMark) -> Option<(Range<usize>, String)> {
    if !diff_lines(old, text).contains(mark) {
        return None;
    }
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let n = starts.len();
    let (s, e) = (mark.start as usize, mark.end as usize);
    let old_lines: Vec<&str> = old.split('\n').collect();
    let repl = old_lines.get(mark.old_start as usize..mark.old_end as usize)?.join("\n");
    let has_old = mark.old_end > mark.old_start;
    Some(match (s < e, has_old) {
        // Modified: the lines [s, e) become the old lines.
        (true, true) => (starts[s]..if e < n { starts[e] - 1 } else { text.len() }, repl),
        // Added: the lines go, with one separator.
        (true, false) if e < n => (starts[s]..starts[e], String::new()),
        (true, false) => (starts[s].saturating_sub(1)..text.len(), String::new()),
        // Deleted: the old lines come back before line `s`, or after the last one.
        (false, _) if s < n => (starts[s]..starts[s], repl + "\n"),
        (false, _) => (text.len()..text.len(), format!("\n{repl}")),
    })
}

/// The index blob after staging `mark`: `index` with every change of the buffer that touches the hunk taken over. `text` is the
/// buffer (LF); its lines are encoded in `style`.
///
/// `head` (when the file is in the last commit) lets it refuse a mark that is no change of `text` against `HEAD` any more.
pub fn stage_blob(index: &[u8], head: Option<&[u8]>, text: &str, style: FileStyle, mark: &DiffMark) -> Result<Vec<u8>, String> {
    if let Some(h) = head {
        if !diff_lines(&norm(h, "committed")?, text).contains(mark) {
            return Err(STALE.into());
        }
    }
    let index_norm = norm(index, "staged")?;
    let hunks: Vec<DiffMark> = diff_lines(&index_norm, text).into_iter().filter(|g| touches((g.start, g.end), (mark.start, mark.end))).collect();
    if hunks.is_empty() {
        return Err("That change is already staged".into());
    }
    let buffer = textio::encode_text(text, style.ending, style.bom);
    let (b_lines, mut lines) = (raw_lines(&buffer), raw_lines(index));
    if lines.len() != index_norm.split('\n').count() || b_lines.len() != text.split('\n').count() {
        return Err("Could not split the file into lines".into());
    }
    for g in hunks.iter().rev() {
        lines.splice(g.old_start as usize..g.old_end as usize, b_lines[g.start as usize..g.end as usize].iter().copied());
    }
    Ok(join_raw(&lines, style.ending))
}

/// The index blob after unstaging `mark` (a change of `text` against `head`): the lines of `head` go back into `index` at the
/// place the change has there. Refuses when the index does not hold the change (it differs from the buffer there).
pub fn unstage_blob(index: &[u8], head: &[u8], text: &str, mark: &DiffMark) -> Result<Vec<u8>, String> {
    let (index_norm, head_norm) = (norm(index, "staged")?, norm(head, "committed")?);
    if !diff_lines(&head_norm, text).contains(mark) {
        return Err(STALE.into());
    }
    let (s, e) = (mark.start, mark.end);
    let mut shift = 0i64; // lines the buffer has more than the index before the hunk
    for g in diff_lines(&index_norm, text) {
        if touches((g.start, g.end), (s, e)) {
            return Err("That change is not staged".into());
        }
        if g.end <= s {
            shift += i64::from(g.end - g.start) - i64::from(g.old_end - g.old_start);
        }
    }
    let (mut lines, head_lines) = (raw_lines(index), raw_lines(head));
    let at = i64::from(s) - shift;
    let (at, len) = (usize::try_from(at).map_err(|_| STALE)?, (e - s) as usize);
    if at + len > lines.len() || lines.len() != index_norm.split('\n').count() || head_lines.len() != head_norm.split('\n').count() {
        return Err("Could not split the file into lines".into());
    }
    lines.splice(at..at + len, head_lines[mark.old_start as usize..mark.old_end as usize].iter().copied());
    Ok(join_raw(&lines, FileStyle::of(index).ending))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edit: (Range<usize>, String)) -> String {
        let mut t = text.to_string();
        t.replace_range(edit.0, &edit.1);
        t
    }

    /// Reverting the last hunk again and again gives the old text back (all kinds, edges, no final newline).
    #[test]
    fn reverting_all_hunks_restores_the_old_text_for_every_shape() {
        // Every text of up to 4 lines out of a, b, c, with and without the final newline.
        let mut texts = vec![String::new()];
        let mut layer = vec![String::new()];
        for _ in 0..4 {
            layer = layer.iter().flat_map(|t| ["a", "b", "c"].map(|l| format!("{t}{l}\n"))).collect();
            texts.extend(layer.iter().cloned());
            texts.extend(layer.iter().map(|t| t.trim_end_matches('\n').to_string()));
        }
        let mut checked = 0;
        for old in &texts {
            for new in &texts {
                let mut cur = new.clone();
                // Reverting one hunk at a time (the diff of the result is recomputed, as the UI does).
                for _ in 0..10 {
                    let Some(m) = diff_lines(old, &cur).pop() else { break };
                    let Some(edit) = revert_edit(&cur, old, &m) else { panic!("{old:?} -> {cur:?}: {m:?}") };
                    cur = apply(&cur, edit);
                }
                assert_eq!(&cur, old, "{old:?} -> {new:?}");
                checked += 1;
            }
        }
        assert!(checked > 1000, "{checked}");
    }

    #[test]
    fn revert_edit_is_none_for_a_stale_mark() {
        let m = diff_lines("a\nb\n", "a\nX\n")[0];
        assert!(revert_edit("a\nX\n", "a\nb\n", &m).is_some());
        assert!(revert_edit("a\nX\nmore\n", "a\nb\n", &m).is_none(), "the buffer has another hunk now: the mark no longer matches");
        assert!(revert_edit("a\nb\n", "a\nb\n", &m).is_none());
    }

    #[test]
    fn revert_edit_exact_edits() {
        let edit = |old: &str, new: &str| revert_edit(new, old, &diff_lines(old, new)[0]).unwrap();
        assert_eq!(edit("a\nb\nc\n", "a\nX\nc\n"), (2..3, "b".into()));
        assert_eq!(edit("a\nb\n", "a\n"), (2..2, "b\n".into()), "deleted line comes back before the empty last line");
        assert_eq!(edit("a\nb", "a"), (1..1, "\nb".into()), "no final newline: separator first");
        assert_eq!(edit("a\n", "a\nx\n"), (2..4, String::new()));
        assert_eq!(edit("", "x\ny"), (0..3, String::new()));
    }

    fn marks_of(a: &str, b: &str) -> Vec<DiffMark> {
        diff_lines(a, b)
    }

    #[test]
    fn staging_composes_the_index_for_each_shape_and_keeps_crlf_and_bom() {
        let lf = FileStyle { ending: LineEnding::Lf, bom: false };
        // index == HEAD == "a\nb\nc\nd\n", buffer changes line 1 and 3: stage only the first hunk.
        let (idx, buf) = ("a\nb\nc\nd\n", "a\nB\nc\nD\n");
        let m = marks_of(idx, buf);
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &m[0]).unwrap(), b"a\nB\nc\nd\n");
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &m[1]).unwrap(), b"a\nb\nc\nD\n");
        // Start, end, deletion, no trailing newline.
        let (idx, buf) = ("a\nb\n", "x\na\nb\ny\n");
        let m = marks_of(idx, buf);
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &m[0]).unwrap(), b"x\na\nb\n");
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &m[1]).unwrap(), b"a\nb\ny\n");
        let (idx, buf) = ("a\nb\nc", "a\nc");
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &marks_of(idx, buf)[0]).unwrap(), b"a\nc");
        let (idx, buf) = ("a\nb\n", "a\nb");
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &marks_of(idx, buf)[0]).unwrap(), b"a\nb");
        // CRLF file: staged lines come with CRLF, untouched lines keep what the index had; BOM stays on line 0.
        let crlf = FileStyle { ending: LineEnding::Crlf, bom: true };
        let idx = b"\xEF\xBB\xBFa\r\nb\r\nc\r\n";
        let buf = "a\nB\nc\nd\n";
        let m = marks_of("a\nb\nc\n", buf);
        assert_eq!(stage_blob(idx, None, buf, crlf, &m[0]).unwrap(), b"\xEF\xBB\xBFa\r\nB\r\nc\r\n");
        assert_eq!(stage_blob(idx, None, buf, crlf, &m[1]).unwrap(), b"\xEF\xBB\xBFa\r\nb\r\nc\r\nd\r\n");
        // Terminators follow the lines: an unterminated last line that gets a successor takes the file's ending, and a
        // terminated one that ends the file loses it (what `git add -p` stores).
        let plain = FileStyle { ending: LineEnding::Crlf, bom: false };
        assert_eq!(stage_blob(b"a\r\nb", None, "a\nb\nc", plain, &marks_of("a\nb", "a\nb\nc")[0]).unwrap(), b"a\r\nb\r\nc");
        assert_eq!(stage_blob(b"a\r\nb\r\n", None, "a\nb", plain, &marks_of("a\nb\n", "a\nb")[0]).unwrap(), b"a\r\nb");
        // A change on line 0 of a BOM file takes the BOM along, as `git add -p` would.
        let buf = "A\nb\nc\n";
        assert_eq!(stage_blob(idx, None, buf, crlf, &marks_of("a\nb\nc\n", buf)[0]).unwrap(), b"\xEF\xBB\xBFA\r\nb\r\nc\r\n");
    }

    #[test]
    fn staging_what_is_staged_already_or_unrelated_is_refused() {
        let lf = FileStyle { ending: LineEnding::Lf, bom: false };
        let (idx, buf) = ("a\nB\nc\nd\n", "a\nB\nc\nD\n"); // line 1 is in the index already
        let head_marks = marks_of("a\nb\nc\nd\n", buf);
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &head_marks[0]), Err("That change is already staged".into()));
        assert_eq!(stage_blob(idx.as_bytes(), None, buf, lf, &head_marks[1]).unwrap(), b"a\nB\nc\nD\n");
        assert!(stage_blob(b"a\0b", None, "x", lf, &head_marks[0]).is_err(), "a binary index version is refused");
    }

    #[test]
    fn staging_refuses_a_mark_that_is_no_change_against_head_any_more() {
        let lf = FileStyle { ending: LineEnding::Lf, bom: false };
        let (head, buf) = ("a\nb\nc\n", "a\nB\nc\n");
        let m = marks_of(head, buf)[0];
        assert!(stage_blob(head.as_bytes(), Some(head.as_bytes()), buf, lf, &m).is_ok());
        assert_eq!(stage_blob(head.as_bytes(), Some(head.as_bytes()), "a\nB\nC\n", lf, &m), Err(STALE.into()), "the buffer moved on");
    }

    #[test]
    fn unstaging_takes_a_staged_hunk_back_and_refuses_an_unstaged_one() {
        let head = "a\nb\nc\nd\ne\n";
        let buf = "a\nB\nc\nd\nE\nf\n";
        let m = marks_of(head, buf); // b->B, e->E+f (one Modified [4,6))
        assert_eq!(m.len(), 2);
        // Index holds both changes: unstage the first, keep the second.
        assert_eq!(unstage_blob(buf.as_bytes(), head.as_bytes(), buf, &m[0]).unwrap(), b"a\nb\nc\nd\nE\nf\n");
        assert_eq!(unstage_blob(buf.as_bytes(), head.as_bytes(), buf, &m[1]).unwrap(), b"a\nB\nc\nd\ne\n");
        // The index holds only the second change: the first is not staged; the second's place has shifted by one line.
        let idx = "a\nb\nc\nd\nE\nf\n";
        assert_eq!(unstage_blob(idx.as_bytes(), head.as_bytes(), buf, &m[0]), Err("That change is not staged".into()));
        assert_eq!(unstage_blob(idx.as_bytes(), head.as_bytes(), buf, &m[1]).unwrap(), b"a\nb\nc\nd\ne\n");
        // The index holds a line the buffer dropped again (an earlier stage): the shift is accounted for.
        let idx = "x\ny\na\nb\nc\nd\nE\nf\n"; // two lines added in the index only; buffer lacks them
        let shifted = marks_of(idx, buf);
        assert!(shifted.iter().any(|g| g.kind == crate::git::DiffKind::Deleted));
        assert_eq!(unstage_blob(idx.as_bytes(), head.as_bytes(), buf, &m[1]).unwrap(), b"x\ny\na\nb\nc\nd\ne\n");
        // A stale mark is refused.
        assert_eq!(unstage_blob(buf.as_bytes(), head.as_bytes(), "a\nB\n", &m[0]), Err(STALE.into()));
        // CRLF: the HEAD lines come back as HEAD had them.
        let head = b"a\r\nb\r\n";
        let buf = "a\nX\n";
        let m = marks_of("a\nb\n", buf);
        assert_eq!(unstage_blob(b"a\r\nX\r\n", head, buf, &m[0]).unwrap(), b"a\r\nb\r\n");
    }
}
