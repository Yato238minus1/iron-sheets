//! Property test: windows of a big note highlighted through `WindowHighlighter` equal the full highlight.
mod common;
use common::RICH;
use iron_sheets_md::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    " ",
    "\n",
    "\n\n",
    "`",
    "```",
    "~~~",
    "$",
    "$$",
    "$$\n",
    "---",
    "---\n",
    "<div>",
    "</div>",
    "<!--",
    "-->",
    "%%",
    "*",
    "**",
    "_",
    "[",
    "]",
    "(",
    ")",
    "[[",
    "]]",
    "|",
    "|---|",
    "| a | b |\n",
    "> ",
    "- ",
    "- [ ] ",
    "1. ",
    "# ",
    "## ",
    "\r\n",
    "é",
    "日本",
    "#tag",
    "http://x.y",
    "\\",
    "==",
    "![",
    "\t",
    "...",
    "===",
];

fn per_byte(text: &str) -> Vec<Tok> {
    let mut v = Vec::new();
    highlight(text, &mut v);
    let mut out = vec![Tok::Text; text.len()];
    for s in v {
        for t in &mut out[s.range] {
            *t = s.tok;
        }
    }
    out
}

fn window_bytes(w: &WindowHighlighter, text: &str, start: usize, end: usize) -> (usize, Vec<Tok>) {
    let mut toks = Vec::new();
    w.tokens(text, start, end, &mut toks);
    let mut out = Vec::new();
    for (len, t) in toks {
        out.extend(std::iter::repeat_n(t, len as usize));
    }
    (start, out)
}

fn corpus(size: usize) -> String {
    let mut s = String::new();
    while s.len() < size {
        s.push_str(RICH);
    }
    s
}

fn check(w: &mut WindowHighlighter, prev: &Option<(String, Vec<Tok>)>, text: &str, rng: &mut Rng, ctx: &str) -> Vec<Tok> {
    let dirty = w.update(prev.as_ref().map(|p| p.0.as_str()), text);
    let want = per_byte(text);
    if let Some((old, oldhl)) = prev {
        let n = text.len();
        let delta = n as isize - old.len() as isize;
        let ob = old.as_bytes();
        let nb = text.as_bytes();
        let p = ob.iter().zip(nb).take_while(|(a, b)| a == b).count();
        let s = ob[p..].iter().rev().zip(nb[p..].iter().rev()).take_while(|(a, b)| a == b).count();
        for i in 0..dirty.start.min(n).min(p) {
            assert_eq!(oldhl[i], want[i], "{ctx}: byte {i} before dirty {dirty:?} changed");
        }
        for (i, &w) in want.iter().enumerate().skip(dirty.end.max(n - s)) {
            let o = (i as isize - delta) as usize;
            if oldhl[o] != w {
                let lo = p.saturating_sub(60);
                panic!(
                    "{ctx}: byte {i} after dirty {dirty:?} changed (edit p={p} s={s})\nOLD {:?}\nNEW {:?}",
                    &old[lo..(p + 90).min(old.len())],
                    &text[lo..(p + 90).min(n)]
                );
            }
        }
    }
    // Random windows: a few lines starting at a line start, plus the whole text now and then.
    let b = text.as_bytes();
    for k in 0..6 {
        let at = rng.below(text.len() + 1);
        let start = b[..at].iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        let mut end = start;
        for _ in 0..rng.below(if k == 0 { 4000 } else { 6 }) {
            match b[end..].iter().position(|&c| c == b'\n') {
                Some(i) if end + i + 1 < b.len() => end += i + 1,
                _ => break,
            }
        }
        // `end` is a line start; a paragraph end is the line break before it (or the text end).
        let pend = if end > start { end - 1 } else { b[end..].iter().position(|&c| c == b'\n').map_or(b.len(), |i| end + i) };
        let (s0, got) = window_bytes(w, text, start, pend);
        let upto = (s0 + got.len()).min(want.len());
        if got[..upto - s0] != want[s0..upto] {
            let d = (0..upto - s0).find(|&i| got[i] != want[s0 + i]).unwrap_or(0);
            let at = s0 + d;
            let lo = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let ctx_start = text[..lo.saturating_sub(1)].rfind('\n').map_or(0, |i| i + 1);
            panic!(
                "{ctx}: window {start}..{pend} differs at {at}: got {:?} want {:?}\ntext from previous line: {:?}",
                got[d],
                want[at],
                &text[ctx_start..(at + 40).min(text.len())]
            );
        }
        assert!(upto > pend || pend >= b.len() || got.len() > pend - start, "{ctx}: window too short");
    }
    want
}

fn run(mut text: String, edits: usize, seed: u64, small_only: bool) {
    let mut rng = Rng(seed);
    let mut w = WindowHighlighter::new();
    let mut prev: Option<(String, Vec<Tok>)> = None;
    let hl = check(&mut w, &prev, &text, &mut rng, "initial");
    prev = Some((text.clone(), hl));
    for i in 0..edits {
        let mut at = rng.below(text.len() + 1);
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        match rng.below(if small_only { 4 } else { 6 }) {
            0 | 1 => text.insert_str(at, PIECES[rng.below(PIECES.len())]),
            2 | 3 => {
                let mut end = (at + rng.below(6)).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                text.replace_range(at..end, "");
            }
            4 => {
                let mut end = (at + rng.below(300)).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                text.replace_range(at..end, "");
            }
            _ => {
                let mut a = rng.below(text.len() + 1);
                while !text.is_char_boundary(a) {
                    a -= 1;
                }
                let mut b = (a + rng.below(200)).min(text.len());
                while !text.is_char_boundary(b) {
                    b += 1;
                }
                let chunk = text[a..b].to_string();
                text.insert_str(at, &chunk);
            }
        }
        let hl = check(&mut w, &prev, &text, &mut rng, &format!("edit {i}"));
        prev = Some((text.clone(), hl));
    }
}

#[test]
fn windows_rich_note() {
    run(corpus(3_000), 2500, 7, false);
}

#[test]
fn windows_big_note() {
    run(corpus(60_000), 1200, 11, false);
}

#[test]
fn windows_many_seeds() {
    for seed in 1..=150u64 {
        run(corpus(1_500), 150, seed * 7919, seed % 2 == 0);
    }
}

#[test]
fn windows_front_matter_tables_fences() {
    run(format!("---\ntitle: x\n---\n{}", corpus(2_000)), 800, 3, false);
    run("| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n$$\nx\n$$\n<!--\nc\n-->\n%%\nz\n%%\n".repeat(30), 800, 5, true);
}

#[test]
fn unterminated_fence_reaches_the_end_and_closing_fixes_it() {
    let mut text = String::from("intro\n\n");
    for i in 0..200 {
        text.push_str(&format!("line {i} with **bold**\n"));
    }
    let mut w = WindowHighlighter::new();
    w.update(None, &text);
    let mut toks = Vec::new();
    let mid = text.find("line 100").unwrap();
    w.tokens(&text, mid, mid + 5, &mut toks);
    assert!(toks.iter().any(|t| t.1 == Tok::Bold));
    let old = text.clone();
    text.insert_str(0, "```\n");
    let d = w.update(Some(&old), &text);
    assert!(d.end >= text.len() - 30, "{d:?}");
    let mid = text.find("line 100").unwrap();
    w.tokens(&text, mid, mid + 5, &mut toks);
    assert!(toks.iter().all(|t| t.1 == Tok::CodeBlock), "{toks:?}");
}

#[test]
fn unterminated_front_matter_is_closed_and_reopened_by_edits() {
    let mut w = WindowHighlighter::default();
    let mut text = format!("---\ntitle: x\n{}", corpus(2_000));
    let steps = [
        ("", ""),
        ("title: x\n", "title: y\n"),
        ("title: y\n", "title: y\n---\n"),
        ("title: y\n---\n", "title: y\n--\n"),
        ("title: y\n--\n", "title: y\n--"),
        ("title: y\n--", "title: y\n---"),
        ("title: y\n---", "title: y\n- -"),
        ("title: y\n- -", "title: y\n..."),
    ];
    for (i, (from, to)) in steps.iter().enumerate() {
        let next = text.replacen(from, to, 1);
        let old = std::mem::replace(&mut text, next);
        w.update((i > 0).then_some(old.as_str()), &text);
        assert_eq!(window_bytes(&w, &text, 0, text.len()).1, per_byte(&text), "step {i}");
    }
}

#[test]
fn fence_block_finds_opener_and_body() {
    let t = "a\n\n```rust\nfn a()\n\nlet b\n```\n\n~~~\nx\n~~~\n> ```c\n> y\n> ```\n  ```sh\n  z\n  ```\n```py\nopen";
    let mut hl = WindowHighlighter::new();
    hl.update(None, t);
    let at = |s: &str| t.find(s).unwrap();
    let ls = |s: &str| t[..at(s)].rfind('\n').map_or(0, |p| p + 1);
    let (op, body) = hl.fence_block(t, ls("let b")).unwrap();
    assert_eq!((&t[op], &t[body]), ("```rust\n", "fn a()\n\nlet b\n"));
    assert_eq!(hl.fence_block(t, ls("fn a")).unwrap().1.start, at("fn a"));
    // blank body line, closing fence, text outside, quoted and indented fences
    assert!(hl.fence_block(t, at("\n\nlet") + 1).is_some());
    assert!(hl.fence_block(t, t.rfind("```\n\n~~~").unwrap()).is_none());
    assert!(hl.fence_block(t, 0).is_none());
    for s in ["> y", "  z"] {
        assert!(hl.fence_block(t, ls(s)).is_none(), "{s}");
    }
    // unclosed at the end of the note: body runs to the text end
    let (op, body) = hl.fence_block(t, ls("open")).unwrap();
    assert_eq!((&t[op], &t[body]), ("```py\n", "open"));
}

/// IS-12: an edit reports a changed range of a few lines, not the whole document.
#[test]
fn edit_in_a_big_note_rehighlights_only_its_block() {
    let mut text = String::new();
    let mut fence = (0, 0);
    let mut i = 0;
    while text.len() < 1_000_000 {
        if i == 6_000 {
            fence.0 = text.len();
            text.push_str("```rust\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n");
            fence.1 = text.len();
        }
        text.push_str(&format!("Line {i} with *emphasis*, `code` and [[Link {i}]] to read.\n"));
        i += 1;
    }
    let mut w = WindowHighlighter::new();
    w.update(None, &text);
    let lines = |t: &str, r: std::ops::Range<usize>| t[r].matches('\n').count();
    // Single character in the middle of a paragraph line.
    let at = text.len() / 2;
    let at = at + text[at..].find("emphasis").expect("word") + 2;
    let mut edited = text.clone();
    edited.insert(at, 'X');
    let r = w.update(Some(&text), &edited);
    eprintln!("paragraph edit: {} lines re-highlighted", lines(&edited, r.clone()));
    assert!(lines(&edited, r) <= 8);
    // Inside the fenced block: stays within it (plus the anchor line above).
    let text = edited;
    let inside = text[fence.0..fence.1].find("let b").expect("body") + fence.0 + 4;
    let mut edited = text.clone();
    edited.insert(inside, 'X');
    let r = w.update(Some(&text), &edited);
    eprintln!("fence edit: {:?} within {:?}", r, fence);
    let (a, b) = (r.start, r.end);
    assert!(a + 100 >= fence.0 && b <= fence.1 + 100, "range {a}..{b} stays near the block {fence:?}");
    assert!(lines(&edited, r) <= 8);
}
