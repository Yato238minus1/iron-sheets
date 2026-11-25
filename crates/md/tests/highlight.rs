mod common;
use common::*;
use iron_sheets_md::*;

fn hl(text: &str) -> Vec<HlSpan> {
    let mut out = Vec::new();
    highlight(text, &mut out);
    check_cov(text, &out);
    out
}

/// The coverage invariant: sorted, contiguous, covering, on char boundaries, no empty spans.
fn check_cov(text: &str, spans: &[HlSpan]) {
    let mut at = 0;
    for s in spans {
        assert_eq!(s.range.start, at, "gap/overlap in {text:?}: {spans:?}");
        assert!(s.range.end > s.range.start, "empty span in {text:?}");
        assert!(text.is_char_boundary(s.range.start) && text.is_char_boundary(s.range.end), "{text:?}");
        at = s.range.end;
    }
    assert_eq!(at, text.len(), "must cover {text:?}: {spans:?}");
}

/// `(slice, tok)` pairs with `Text` filtered out.
fn marked(text: &str) -> Vec<(&str, Tok)> {
    hl(text).iter().filter(|s| s.tok != Tok::Text).map(|s| (&text[s.range.clone()], s.tok)).collect()
}

fn count_chars(m: &[(&str, Tok)], tok: Tok, c: char) -> usize {
    m.iter().filter(|(_, t)| *t == tok).map(|(s, _)| s.matches(c).count()).sum()
}

fn has(text: &str, slice: &str, tok: Tok) -> bool {
    marked(text).iter().any(|(s, t)| *s == slice && *t == tok)
}

#[test]
fn empty_and_tiny() {
    assert!(hl("").is_empty());
    for t in ["a", "\n", "\r\n", "#", "*", "`", "[", "![", "<", "$", "%", "|", "\\", "-", "1.", ">", "---", "```"] {
        hl(t);
    }
}

#[test]
fn headings() {
    let m = marked("# Title\n## Two ##\n###### Six\n####### seven\n#hashtag\n");
    assert!(m.contains(&("#", Tok::HeadingMark)));
    assert!(m.contains(&("##", Tok::HeadingMark)));
    assert!(m.contains(&("######", Tok::HeadingMark)));
    let spans = hl("# Title\n");
    assert_eq!(spans[0], HlSpan { range: 0..1, tok: Tok::HeadingMark });
    assert_eq!(spans[1].tok, Tok::Heading(1));
    // not headings
    assert!(!marked("####### seven\n").iter().any(|(_, t)| *t == Tok::HeadingMark));
    assert!(has("#hashtag\n", "#hashtag", Tok::Tag));
    // closing hashes
    let m = marked("## Two ##\n");
    assert_eq!(m.iter().filter(|(_, t)| *t == Tok::HeadingMark).count(), 2);
    // inline styles inside headings
    assert!(has("# a **b**\n", "b", Tok::Bold));
    // setext underline
    assert!(has("Title\n=====\n", "=====\n", Tok::HeadingMark));
}

#[test]
fn emphasis() {
    assert!(has("a *b* c", "b", Tok::Italic));
    assert!(has("a _b_ c", "b", Tok::Italic));
    assert!(has("a **b** c", "b", Tok::Bold));
    assert!(has("a __b__ c", "b", Tok::Bold));
    assert!(has("a ***b*** c", "b", Tok::BoldItalic));
    assert!(has("a **b *c* d** e", "c", Tok::BoldItalic));
    assert!(has("a **b *c* d** e", " d", Tok::Bold));
    assert!(has("~~gone~~", "gone", Tok::Strike));
    assert!(has("==hot==", "hot", Tok::Highlight));
    assert!(has("**é日本**", "é日本", Tok::Bold));
    // markers are punctuation
    assert!(has("*a*", "*", Tok::MarkupPunct));
    // not emphasis
    assert!(marked("snake_case_word and 2 * 3 * 4\n").is_empty());
    assert!(marked("a * b\n").is_empty());
    assert!(marked("unclosed *star and **bold\n").is_empty());
    assert!(marked("a\\*b\\*\n").iter().all(|(_, t)| *t == Tok::Escape));
}

#[test]
fn code() {
    assert!(has("a `b` c", "`b`", Tok::Code));
    assert!(has("a ``b ` c`` d", "``b ` c``", Tok::Code));
    assert!(has("a ```b``` d", "```b```", Tok::Code));
    assert!(marked("a `unclosed\n").is_empty());
    // markup inside code is not interpreted
    assert_eq!(marked("`*a*`"), vec![("`*a*`", Tok::Code)]);
    assert!(has("x `a` y `b` z", "`b`", Tok::Code));
}

#[test]
fn fences() {
    let t = "```rust\nlet x = *1*;\n```\nafter *em*\n";
    let m = marked(t);
    assert_eq!(m[0], ("```rust\n", Tok::CodeFence));
    assert_eq!(m[1], ("let x = *1*;\n", Tok::CodeBlock));
    assert_eq!(m[2], ("```\n", Tok::CodeFence));
    assert!(has(t, "em", Tok::Italic));
    // longer fence needs a matching closer
    let t = "````\n```\nstill code\n````\ntext\n";
    let m = marked(t);
    assert_eq!(m.len(), 3);
    assert_eq!(m[1].1, Tok::CodeBlock);
    assert!(m[1].0.contains("still code"));
    // tilde fence and mismatched char
    let t = "~~~\n```\nx\n~~~\n";
    assert_eq!(marked(t).len(), 3);
    // unclosed fence runs to the end
    let t = "```\nabc\ndef";
    let s = hl(t);
    assert_eq!(s.last().unwrap().tok, Tok::CodeBlock);
    // backtick info string may not contain a backtick (that is inline code)
    assert!(has("```not a fence```\n", "```not a fence```", Tok::Code));
    // indented fence in a list
    let t = "- item\n  ```py\n  x = 1\n  ```\n- next\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::CodeBlock && s.contains("x = 1")));
    assert!(m.iter().filter(|(_, t)| *t == Tok::ListMark).count() == 2);
    // fence inside a quote
    let t = "> ```\n> code\n> ```\nplain\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::CodeBlock && s.contains("code")));
    assert_eq!(count_chars(&m, Tok::QuoteMark, '>'), 3);
}

#[test]
fn links_images_urls() {
    let m = marked("[text](http://a.b/c_d \"title\") and ![alt](img.png)\n");
    assert!(m.contains(&("text", Tok::LinkText)));
    assert!(m.contains(&("http://a.b/c_d", Tok::LinkUrl)));
    assert!(m.contains(&("![alt](img.png)", Tok::Image)));
    assert!(has("[a][ref]", "ref", Tok::LinkUrl));
    assert!(has("[a](b(c)d)", "b(c)d", Tok::LinkUrl));
    assert!(has("see https://x.y/z. next", "https://x.y/z", Tok::LinkUrl));
    assert!(has("(https://x.y/z) next", "https://x.y/z", Tok::LinkUrl));
    assert!(has("<https://x.y>", "<https://x.y>", Tok::LinkUrl));
    assert!(has("[![b](i.svg)](http://x)", "http://x", Tok::LinkUrl));
    assert!(marked("[not a link] text\n").is_empty());
    assert!(marked("[unclosed(\n").is_empty());
}

#[test]
fn wikilinks_tags_footnotes() {
    assert!(has("a [[Note|Alias]] b", "[[Note|Alias]]", Tok::WikiLink));
    assert!(has("a [[Note#^blk]] b", "[[Note#^blk]]", Tok::WikiLink));
    assert!(has("![[pic.png]]", "![[pic.png]]", Tok::Image));
    assert!(marked("a [[unclosed b\n").is_empty());
    assert!(has("x #tag/sub y", "#tag/sub", Tok::Tag));
    assert!(has("#start", "#start", Tok::Tag));
    assert!(!has("a#b", "#b", Tok::Tag));
    assert!(!has("issue #123 here", "#123", Tok::Tag));
    assert!(has("note[^1] text", "[^1]", Tok::Footnote));
    assert!(has("[^note]: text", "[^note]:", Tok::Footnote));
}

#[test]
fn lists_tasks_quotes_rules() {
    let m = marked("- a\n* b\n+ c\n1. d\n12) e\n  - nested\n");
    assert_eq!(m.iter().filter(|(_, t)| *t == Tok::ListMark).count(), 6);
    assert!(m.contains(&("12)", Tok::ListMark)));
    let m = marked("- [ ] open\n- [x] done\n* [X] big\n- [y] no\n");
    assert_eq!(m.iter().filter(|(_, t)| *t == Tok::TaskMark).count(), 3);
    let m = marked("> a\n>> b\n> - [ ] c\n");
    assert_eq!(count_chars(&m, Tok::QuoteMark, '>'), 4);
    assert!(m.contains(&("[ ]", Tok::TaskMark)));
    for r in ["---\n", "***\n", "___\n", "- - -\n", " * * *\n", "-----\n"] {
        let t = format!("para\n\n{r}");
        assert!(marked(&t).iter().any(|(_, t)| *t == Tok::Rule), "{r:?}");
    }
    assert!(!marked("**bold**\n").iter().any(|(_, t)| *t == Tok::Rule));
    // "- " in the middle of a line is not a list marker
    assert!(!marked("a - b\n").iter().any(|(_, t)| *t == Tok::ListMark));
}

#[test]
fn front_matter() {
    let t = "---\ntitle: x\n---\n# H\n";
    let m = marked(t);
    assert_eq!(m[0], ("---\ntitle: x\n---\n", Tok::FrontMatter));
    // unclosed => rule, not front matter
    assert!(!marked("---\nfoo\n").iter().any(|(_, t)| *t == Tok::FrontMatter));
    // only at the very start
    assert!(!marked("\n---\ntitle: x\n---\n").iter().any(|(_, t)| *t == Tok::FrontMatter));
    // also with `...` closer and CRLF
    assert_eq!(marked("---\r\na: 1\r\n...\r\n")[0].1, Tok::FrontMatter);
}

#[test]
fn math() {
    assert!(has("a $x^2$ b", "$x^2$", Tok::Math));
    assert!(has("a $$x$$ b", "$$x$$", Tok::Math));
    assert!(marked("costs $5 and $6 total\n").is_empty());
    assert!(marked("lone $ dollar\n").is_empty());
    let t = "$$\n\\frac{a}{*b*}\n$$\nafter *i*\n";
    let m = marked(t);
    assert_eq!(m[0].1, Tok::Math);
    assert!(m[0].0.starts_with("$$\n") && m[0].0.ends_with("$$\n"));
    assert!(has(t, "i", Tok::Italic));
}

#[test]
fn html_and_comments() {
    assert!(has("a <b>x</b> c", "<b>", Tok::Html));
    assert!(has("<img src=\"a\">", "<img src=\"a\">", Tok::Html));
    assert!(has("x <!-- c --> y", "<!-- c -->", Tok::Comment));
    assert!(marked("1 < 2 and 3 > 2\n").is_empty());
    // multi-line html comment
    let t = "a <!-- start\n**not bold**\nend --> *it*\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::Comment && s.contains("not bold")));
    assert!(!m.iter().any(|(_, t)| *t == Tok::Bold));
    assert!(has(t, "it", Tok::Italic));
    // percent comments, inline and block
    assert!(has("a %%hidden%% b", "%%hidden%%", Tok::Comment));
    let t = "%%\nblock *x*\n%%\n*after*\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::Comment && s.contains("block *x*")));
    assert!(has(t, "after", Tok::Italic));
    // unclosed comment swallows the rest, never panics
    hl("<!-- never closed\nmore\n");
    hl("%% never closed\nmore\n");
}

#[test]
fn escapes() {
    let m = marked("a \\* b \\_ \\\\ \\a end\n");
    assert_eq!(m.iter().filter(|(_, t)| *t == Tok::Escape).count(), 3);
}

#[test]
fn tables() {
    let t = "| a | b |\n|---|:-:|\n| 1 | `x|y` |\n\nplain | text\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::TableSep && s.contains("---")));
    assert!(count_chars(&m, Tok::TableSep, '|') >= 8);
    // pipe in plain text after the table is untouched
    let spans = hl(t);
    let last = spans.last().unwrap();
    assert_eq!(last.tok, Tok::Text);
    // header without leading pipe
    let t = "a | b\n--|--\n1 | 2\n";
    assert!(count_chars(&marked(t), Tok::TableSep, '|') >= 3);
    // a pipe alone in prose is not a table
    assert!(marked("a | b\n").is_empty());
}

#[test]
fn crlf_input() {
    let t = "# H\r\n- [ ] a\r\n```\r\ncode\r\n```\r\n";
    let m = marked(t);
    assert!(m.iter().any(|(s, t)| *t == Tok::CodeBlock && s.contains("code")));
    assert!(m.iter().any(|(_, t)| *t == Tok::TaskMark));
}

#[test]
fn rich_sample_is_stable_and_covering() {
    let a = hl(RICH);
    let b = hl(RICH);
    assert_eq!(a, b);
    // reuses the buffer: clears first
    let mut out = vec![HlSpan { range: 0..1, tok: Tok::Bold }; 3];
    highlight("x", &mut out);
    assert_eq!(out, vec![HlSpan { range: 0..1, tok: Tok::Text }]);
}

#[test]
fn rich_prefixes() {
    for (i, _) in RICH.char_indices() {
        hl(&RICH[..i]);
    }
    hl(RICH);
}

#[test]
fn byte_soup() {
    let mut rng = Rng(0xdeadbeefcafef00d);
    let iters: usize = std::env::var("MD_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    for _ in 0..iters {
        let len = (rng.next() % 500) as usize;
        let s = soup(&mut rng, len);
        hl(&s);
    }
    for _ in 0..1000 {
        let len = (rng.next() % 300) as usize;
        let s: String = (0..len).map(|_| (rng.next() % 128) as u8 as char).collect();
        hl(&s);
    }
}

#[test]
fn pathological_lines_are_not_quadratic() {
    let n = 200_000;
    for unit in ["*a ", "` ", "[", "<", "$a ", "[a](", "_a ", "~", "==", "%", "![", "\\", "| "] {
        let s = unit.repeat(n);
        let t = std::time::Instant::now();
        hl(&s);
        let el = t.elapsed();
        assert!(el.as_millis() < 1500, "{unit:?} took {el:?}");
    }
    // deep emphasis nesting
    let s = format!("{}x{}", "*a ".repeat(50_000), " b*".repeat(50_000));
    hl(&s);
}

/// A closer inside a code span must not close emphasis opened outside it (bug: strike from "~700" to "~30").
#[test]
fn closers_inside_code_spans_are_ignored() {
    let line = "up to ~700 KB with it (`builder.rs:556`). Selection `text_selection/visuals.rs:~30`). Typical notes";
    assert!(marked(line).iter().all(|(_, t)| *t != Tok::Strike), "{:?}", marked(line));
    for (src, tok) in
        [("*a `b*` c", Tok::Italic), ("**a `b**` c", Tok::Bold), ("_a `b_` c", Tok::Italic), ("==a `b==` c", Tok::Highlight), ("~~a `b~~` c", Tok::Strike)]
    {
        assert!(marked(src).iter().all(|(_, t)| *t != tok), "{src}: {:?}", marked(src));
    }
    // A real closer after the code span still works, and the span keeps its Code token.
    let m = marked("~~a `b~~` c~~ d");
    assert!(m.contains(&("~~a `b~~` c~~", Tok::Strike)) || m.iter().any(|(s, t)| *t == Tok::Strike && s.ends_with("c")), "{m:?}");
    assert!(m.iter().any(|(s, t)| *s == "`b~~`" && *t == Tok::Code), "{m:?}");
    // Unmatched backtick is literal: the closer after it still counts.
    assert!(marked("*a ` b*").iter().any(|(_, t)| *t == Tok::Italic));
}

/// Same rules as the preview (comrak) for a table of one-line cases: does the line contain strike / italic / bold?
#[test]
fn emphasis_matches_comrak() {
    use comrak::{Arena, Options, nodes::NodeValue, parse_document};
    let cases = [
        "~x~",
        "~~x~~",
        "a ~ b ~ c",
        "~a ~b",
        "~a b~",
        "*x*",
        "**x**",
        "***x***",
        "a * b * c",
        "_x_",
        "snake_case_name",
        "a_b_ c",
        "_a b_c",
        "==x==",
        "a == b == c",
        "*a `b*` c",
        "~a `b~` c",
        "~a $b~$ c",
        "x ~700 `y:~30` z",
        "*a\\*b*",
        "~~a~~b~~",
    ];
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.highlight = true;
    o.extension.math_dollars = true;
    for src in cases {
        let arena = Arena::new();
        let root = parse_document(&arena, src, &o);
        let (mut st, mut em, mut sg) = (false, false, false);
        for n in root.descendants() {
            match n.data().value {
                NodeValue::Strikethrough => st = true,
                NodeValue::Emph => em = true,
                NodeValue::Strong => sg = true,
                _ => {}
            }
        }
        let m = marked(src);
        let any = |t: Tok| m.iter().any(|(_, x)| *x == t);
        assert_eq!(any(Tok::Strike), st, "strike {src:?}: {m:?}");
        let bi = any(Tok::BoldItalic);
        assert_eq!(any(Tok::Italic) || bi, em, "italic {src:?}: {m:?}");
        assert_eq!(any(Tok::Bold) || bi, sg, "bold {src:?}: {m:?}");
    }
}
