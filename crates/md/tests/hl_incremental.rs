//! Property test: the incremental highlighter equals the full one after every random edit.
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

fn merged(text: &str) -> Vec<HlSpan> {
    let mut v = Vec::new();
    highlight(text, &mut v);
    v
}

fn check(inc: &mut IncrementalHighlighter, text: &str, ctx: &str) {
    inc.update(text);
    assert_eq!(inc.text(), text, "{ctx}");
    let mut got = Vec::new();
    inc.spans(&mut got);
    let want = merged(text);
    if got != want {
        let at = got.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
        panic!("{ctx}: spans differ at {at}\n got  {:?}\n want {:?}\ntext {text:?}", got.get(at), want.get(at));
    }
    // per-line spans cover each line exactly
    inc.for_each_line(|r, spans| {
        assert_eq!(spans.iter().map(|s| s.0 as usize).sum::<usize>(), r.len(), "{ctx}");
        assert!(r.is_empty() || text[r.clone()].ends_with('\n') || r.end == text.len());
    });
}

fn corpus(size: usize) -> String {
    let mut s = String::new();
    while s.len() < size {
        s.push_str(RICH);
    }
    s
}

fn run(mut text: String, edits: usize, seed: u64, small_only: bool) {
    let mut rng = Rng(seed);
    let mut inc = IncrementalHighlighter::new();
    check(&mut inc, &text, "initial");
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
                // delete a bigger range (a few lines)
                let mut end = (at + rng.below(300)).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                text.replace_range(at..end, "");
            }
            _ => {
                // paste a chunk of the text elsewhere
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
        check(&mut inc, &text, &format!("edit {i}"));
    }
}

#[test]
fn random_edits_rich_note() {
    run(RICH.to_string(), 3000, 0x9E3779B97F4A7C15, false);
}

#[test]
fn random_edits_big_note() {
    run(corpus(60_000), 1500, 0xDEADBEEF12345, true);
}

#[test]
fn random_edits_front_matter() {
    let t = format!("---\ntitle: x\ntags: [a]\n---\n{}", &RICH[..RICH.len().min(3000)]);
    run(t, 3000, 42, false);
    run("---\n".to_string(), 1000, 7, false);
    run(String::new(), 1000, 11, false);
}

#[test]
fn random_edits_tables_and_fences() {
    let t =
        "| a | b |\n|---|---|\n| 1 | 2 |\n\ntext\n\n```rust\nlet x = 1;\n```\n\n$$\nx^2\n$$\n\n<!-- c\nd -->\n\n%% e\nf %%\n\n> ```\n> q\n> ```\n".repeat(8);
    run(t, 4000, 99, false);
}

#[test]
fn typing_redoes_few_lines() {
    let mut text = corpus(100_000);
    let mut inc = IncrementalHighlighter::new();
    inc.update(&text);
    let total = text.lines().count();
    let mut worst = 0;
    for k in 0..50 {
        let mut at = text.len() / 2 + k * 7;
        while !text.is_char_boundary(at) {
            at += 1;
        }
        text.insert(at, 'x');
        inc.update(&text);
        worst = worst.max(inc.lines_redone());
    }
    assert!(worst < 12, "redone {worst} of {total} lines");
}

#[test]
fn fence_open_and_close_reach_the_end() {
    let mut text = corpus(20_000);
    let mut inc = IncrementalHighlighter::new();
    inc.update(&text);
    text.insert_str(100, "\n```\n");
    check(&mut inc, &text, "open fence");
    text.replace_range(100..105, "");
    check(&mut inc, &text, "close fence");
}

#[test]
fn random_edits_many_seeds() {
    for seed in 1..=25u64 {
        run(RICH.to_string(), 600, seed.wrapping_mul(0x2545F4914F6CDD1D) | 1, false);
    }
}
