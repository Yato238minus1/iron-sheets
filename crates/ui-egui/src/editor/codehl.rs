//! Language colours of fenced code blocks in the source view: the runs the preview's highlighter gives for a block
//! (key: language + body text, so a block coloured in one view is free in the other) as absolute byte spans.
use iron_sheets_app_core::services::{CodeClass, CodeHl};
use iron_sheets_md::highlight::Tok;
use std::ops::Range;

/// `(start, end, class)` byte spans, sorted and disjoint, that tile the bodies of the fenced blocks of `text` (`toks` tile
/// it); `None` is plain code text (gaps between runs, blocks without runs). `get` is the highlighter lookup
/// `(info string, body)`. Only blocks whose fences sit at the start of a line and whose lines are all plain code lines
/// count (a quoted or indented fence has a body that differs from the block the preview sees).
pub fn spans(text: &str, toks: &[(u32, Tok)], mut get: impl FnMut(&str, &str) -> CodeHl) -> Vec<(usize, usize, Option<CodeClass>)> {
    let mut out = Vec::new();
    let (mut at, mut i) = (0usize, 0usize);
    while i < toks.len() {
        let (start, tok) = (at, toks[i].1);
        at += toks[i].0 as usize;
        i += 1;
        // An indented closing fence is one token that starts at the line start, with its indentation: not an opener.
        if tok != Tok::CodeFence || (start > 0 && text.as_bytes()[start - 1] != b'\n') || !text[start..].starts_with(['`', '~']) {
            continue;
        }
        let info = text[start..at].trim_start_matches(['`', '~']).trim();
        let body_start = at;
        while i < toks.len() && toks[i].1 == Tok::CodeBlock {
            at += toks[i].0 as usize;
            i += 1;
        }
        let body = &text[body_start..at];
        let closed = i < toks.len() && toks[i].1 == Tok::CodeFence;
        if closed {
            at += toks[i].0 as usize;
            i += 1;
        }
        if !closed && i < toks.len() {
            continue;
        }
        // comrak's block text has no final line break
        let body = body.strip_suffix('\n').unwrap_or(body);
        if body.is_empty() {
            continue;
        }
        tile(body_start, body, if info.is_empty() { CodeHl::Plain } else { get(info, body) }, &mut out);
    }
    out
}

/// Spans for one block body starting at `body_start` (`body` has no final line break), see `spans`.
fn tile(body_start: usize, body: &str, runs: CodeHl, out: &mut Vec<(usize, usize, Option<CodeClass>)>) {
    let runs = match &runs {
        CodeHl::Runs(r) | CodeHl::Pending(Some(r)) => &r[..],
        _ => &[],
    };
    let mut pos = 0;
    for r in runs {
        let (a, b) = (r.start as usize, (r.end as usize).min(body.len()));
        if a >= pos && a < b && body.is_char_boundary(a) && body.is_char_boundary(b) {
            if a > pos {
                out.push((body_start + pos, body_start + a, None));
            }
            out.push((body_start + a, body_start + b, Some(r.class)));
            pos = b;
        }
    }
    if pos < body.len() {
        out.push((body_start + pos, body_start + body.len(), None));
    }
}

/// `spans` for the single block with info line `opener` and body `body` (byte ranges of `text`, from
/// `WindowHighlighter::fence_block`), for big notes.
pub fn block_spans(text: &str, opener: Range<usize>, body: Range<usize>, get: impl FnOnce(&str, &str) -> CodeHl) -> Vec<(usize, usize, Option<CodeClass>)> {
    let info = text[opener].trim_start_matches(['`', '~']).trim();
    // comrak's block text has no final line break
    let b = &text[body.clone()];
    let b = b.strip_suffix('\n').unwrap_or(b);
    let mut out = Vec::new();
    if !b.is_empty() {
        tile(body.start, b, if info.is_empty() { CodeHl::Plain } else { get(info, b) }, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_sheets_app_core::services::CodeRun;
    use iron_sheets_md::highlight::IncrementalHighlighter;

    fn toks(text: &str) -> Vec<(u32, Tok)> {
        let mut h = IncrementalHighlighter::new();
        h.update(text);
        h.tokens().to_vec()
    }

    /// Runs: the first word of the body is a keyword.
    fn kw_first_word(_lang: &str, body: &str) -> CodeHl {
        let end = body.find(' ').unwrap_or(body.len()) as u32;
        CodeHl::Runs(vec![CodeRun { start: 0, end, class: CodeClass::Kw }, CodeRun { start: end + 1, end: body.len() as u32, class: CodeClass::Str }].into())
    }

    fn texts<'a>(text: &'a str, sp: &[(usize, usize, Option<CodeClass>)]) -> Vec<&'a str> {
        sp.iter().map(|&(a, b, _)| &text[a..b]).collect()
    }

    #[test]
    fn spans_lie_inside_the_fence_body() {
        let t = "# h\n\n```rs\nfn é 1\n```\n\nfn x y\n";
        let sp = spans(t, &toks(t), kw_first_word);
        assert_eq!(texts(t, &sp), ["fn", " ", "é 1"]);
        assert_eq!(sp[1].2, None);
    }

    #[test]
    fn crlf_and_unclosed_and_unusable_fences() {
        let t = "```rs\r\nlet a\r\nb\r\n```\r\n";
        assert_eq!(texts(t, &spans(t, &toks(t), kw_first_word)), ["let", " ", "a\r\nb\r"]);
        let t = "```rs\nlet a";
        assert_eq!(texts(t, &spans(t, &toks(t), kw_first_word)), ["let", " ", "a"]);
        for t in ["> ```rs\n> let a\n> ```\n", "- x\n  ```rs\n  let a\n  ```\n", "```rs\n```\n"] {
            assert!(spans(t, &toks(t), kw_first_word).is_empty(), "{t:?}");
        }
    }

    #[test]
    fn indented_closing_fence_is_not_an_opener() {
        let t = "  ```rs\n  x\n  ```\n```rs\nfn a\n```\n";
        assert_eq!(texts(t, &spans(t, &toks(t), kw_first_word)), ["fn", " ", "a"]);
    }

    #[test]
    fn pending_uses_previous_runs_and_plain_none() {
        let t = "```rs\nab cd\n```\n";
        let runs: std::sync::Arc<[CodeRun]> = vec![CodeRun { start: 0, end: 2, class: CodeClass::Num }].into();
        assert_eq!(spans(t, &toks(t), |_, _| CodeHl::Pending(Some(runs.clone()))).len(), 2);
        // no runs: the whole body is plain code text
        for hl in [CodeHl::Pending(None), CodeHl::Plain] {
            assert_eq!(spans(t, &toks(t), |_, _| hl.clone()), [(6, 11, None)]);
        }
    }
}
