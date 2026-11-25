//! Property test: the incremental Typst highlighter equals the full one after every random edit.
use iron_sheets_typst::{IncrementalTypstHighlighter, TypTok, highlight};
use std::ops::Range;

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

const CHUNK: &str = "= Heading {n}\n\nSome *strong* and _emph_ text with `raw` and a #link(\"https://x.org\")[link], a label <lab-{n}> and a ref @lab-{n}.\n\n- item one\n- item two with $a^2 + b^2 = c^2$\n+ numbered\n\n#let f(x, y: 2) = {\n  let z = x + y * 3\n  if z > 4 { [big #z] } else { [small] }\n}\n\n#f(3, y: 5)\n#show heading: it => it.body\n#a.b.c(1)\n\n$ integral_0^oo e^(-x^2) dif x = sqrt(pi)/2 $\n\n```rust\nfn main() { println!(\"{}\", {n}); }\n```\n\n/ Term: a description list item\n\n// a comment\n/* block\ncomment */\n\nPlain paragraph text, long enough: lorem ipsum dolor sit amet {n}. é日本\n\n";

const PIECES: &[&str] = &[
    "a", " ", "\n", "\n\n", "#", "(", ")", "[", "]", "{", "}", "$", "$ ", "```", "`", "*", "_", "= ", "//", "/*", "*/", "<l>", "@r", "#let ", "f(", ".", "x.y",
    ":", ",", "\"", "\\", "- ", "+ ", "/ ", "#f[", "=", "é", "日本", "\r\n", "#show", "\t", "--", "..", "1", "0.5", "<", ">",
];

fn corpus(n: usize) -> String {
    (0..n).map(|i| CHUNK.replace("{n}", &i.to_string())).collect()
}

fn merged(text: &str) -> Vec<(Range<usize>, TypTok)> {
    let mut v = Vec::new();
    highlight(text, &mut v);
    v
}

fn check(inc: &mut IncrementalTypstHighlighter, text: &str, ctx: &str) {
    inc.update(text);
    assert_eq!(inc.text(), text, "{ctx}");
    let mut got = Vec::new();
    inc.spans(&mut got);
    let want = merged(text);
    if got != want {
        let at = got.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(got.len().min(want.len()));
        panic!("{ctx}: spans differ at {at}\n got  {:?}\n want {:?}\ntext {text:?}", got.get(at), want.get(at));
    }
    // no token holds a line break except as its last byte
    let mut at = 0;
    for &(len, _) in inc.tokens() {
        let piece = &text[at..at + len as usize];
        assert!(!piece.as_bytes()[..piece.len().saturating_sub(1)].contains(&b'\n'), "{ctx}: {piece:?}");
        at += len as usize;
    }
    assert_eq!(at, text.len(), "{ctx}");
}

fn run(mut text: String, edits: usize, seed: u64) {
    let mut rng = Rng(seed);
    let mut inc = IncrementalTypstHighlighter::new();
    check(&mut inc, &text, "initial");
    for i in 0..edits {
        let mut at = rng.below(text.len() + 1);
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        match rng.below(6) {
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
        check(&mut inc, &text, &format!("edit {i}"));
    }
}

#[test]
fn random_edits_small() {
    run(corpus(2), 3000, 0x9E3779B97F4A7C15);
}

#[test]
fn random_edits_medium() {
    run(corpus(12), 2000, 0xABCDEF12345);
}

#[test]
fn random_edits_from_empty() {
    run(String::new(), 1500, 5);
}

#[test]
fn typing_touches_few_children() {
    let mut text = corpus(60);
    let mut inc = IncrementalTypstHighlighter::new();
    inc.update(&text);
    let mut worst = 0;
    let at = text.match_indices("lorem ipsum dolor sit amet ").nth(30).map(|(k, m)| k + m.len()).unwrap_or(0);
    for k in 0..50 {
        text.insert(at + k, 'x');
        check(&mut inc, &text, "typing");
        worst = worst.max(inc.children_redone());
    }
    assert!(worst < 40, "redone {worst} root children");
}

/// Typing in the middle of prose keeps the tree error-free: those updates must take the incremental
/// path and still match.
#[test]
fn prose_typing_takes_the_incremental_path() {
    const SAFE: &[&str] = &["a", "b", "e", " ", "\n", "\n\n", "word ", "é", "日本", "x y "];
    let mut rng = Rng(77);
    let mut text = corpus(8);
    let mut inc = IncrementalTypstHighlighter::new();
    check(&mut inc, &text, "initial");
    for i in 0..3000 {
        let anchors: Vec<usize> = text.match_indices("lorem ipsum dolor sit amet ").map(|(k, m)| k + m.len()).collect();
        let at = anchors[rng.below(anchors.len())];
        let next = text[at..].chars().next();
        if rng.below(3) == 0 && next.is_some_and(|c| c.is_alphanumeric() || c == ' ') {
            text.replace_range(at..at + next.map_or(0, char::len_utf8), "");
        } else {
            text.insert_str(at, SAFE[rng.below(SAFE.len())]);
        }
        check(&mut inc, &text, &format!("prose {i}"));
    }
    assert!(inc.incremental_updates() > 1000, "only {} incremental updates", inc.incremental_updates());
}

#[test]
fn random_edits_many_seeds() {
    for seed in 1..=25u64 {
        run(corpus(3), 600, seed.wrapping_mul(0x2545F4914F6CDD1D) | 1);
    }
}

/// Diagnostic: is `Source::edit` itself identical to a fresh parse on these edits?
#[test]
fn reparse_vs_parse_diagnostic() {
    let mut diff = 0;
    let mut total = 0;
    let mut err_states = 0;
    for seed in 1..=25u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545F4914F6CDD1D) | 1);
        let mut text = corpus(3);
        let mut src = typst_syntax::Source::detached(text.clone());
        for _ in 0..600 {
            let mut at = rng.below(text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            match rng.below(6) {
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
            let was_err = src.root().diagnosis().either();
            src.replace(&text);
            let fresh = typst_syntax::parse(&text);
            if was_err || src.root().diagnosis().either() || fresh.diagnosis().either() {
                src = typst_syntax::Source::detached(text.clone());
                err_states += 1;
                continue;
            }
            total += 1;
            if !same_tree(src.root(), &fresh) {
                diff += 1;
                // a divergent tree stays divergent: restart from the fresh parse
                src = typst_syntax::Source::detached(text.clone());
            }
        }
    }
    eprintln!("reparse != parse in {diff} of {total} error-free edits ({err_states} edits involved errors)");
}

fn same_tree(a: &typst_syntax::SyntaxNode, b: &typst_syntax::SyntaxNode) -> bool {
    a.kind() == b.kind() && a.len() == b.len() && a.children().len() == b.children().len() && a.children().zip(b.children()).all(|(x, y)| same_tree(x, y))
}
