#![allow(dead_code)]
use iron_sheets_md::*;

/// A document exercising most features.
pub const RICH: &str = include_str!("rich.md");

pub fn inlines_of(b: &Block) -> Option<&Inlines> {
    match &b.kind {
        BlockKind::Paragraph(i) | BlockKind::Heading { content: i, .. } => Some(i),
        _ => None,
    }
}

pub fn check_inlines(i: &Inlines) {
    let mut at = 0u32;
    for s in &i.spans {
        assert_eq!(s.range.start, at, "spans must be contiguous: {i:?}");
        assert!(s.range.end > s.range.start, "empty span: {i:?}");
        assert!(i.text.is_char_boundary(s.range.start as usize), "{i:?}");
        assert!(i.text.is_char_boundary(s.range.end as usize), "{i:?}");
        if let Some(l) = s.link {
            assert!((l as usize) < i.links.len(), "{i:?}");
        }
        at = s.range.end;
    }
    assert_eq!(at as usize, i.text.len(), "spans must cover text: {i:?}");
    for o in &i.objects {
        assert_eq!(i.text[o.at as usize..].chars().next(), Some('\u{FFFC}'), "{i:?}");
    }
    assert!(i.text.matches('\u{FFFC}').count() >= i.objects.len(), "objects need placeholders");
}

/// Structural invariants every parse result must satisfy.
pub fn check_doc(text: &str, doc: &Document) {
    let mut prev_end = 0;
    for b in &doc.blocks {
        assert!(b.lines.start >= prev_end, "overlap/unsorted: {:?} after end {prev_end}", b.lines);
        assert!(b.lines.end >= b.lines.start);
        assert!(b.lines.end <= doc.line_count, "{:?} > {}", b.lines, doc.line_count);
        prev_end = b.lines.end;
        match &b.kind {
            BlockKind::Paragraph(i) | BlockKind::Heading { content: i, .. } => check_inlines(i),
            BlockKind::Table { header, rows, .. } => header.iter().chain(rows.iter().flatten()).for_each(check_inlines),
            _ => {}
        }
        for c in &b.containers {
            if let ContainerKind::ListItem { task: Some(t), .. } = &c.kind {
                let o = t.offset as usize;
                assert!(text.is_char_boundary(o));
                assert_eq!(text.as_bytes()[o - 1], b'[', "task offset {o} in {text:?}");
            }
        }
    }
    assert_eq!(doc.line_count as usize, text.matches('\n').count() + 1);
}

pub fn parse_checked(text: &str) -> Document {
    let d = parse(text);
    check_doc(text, &d);
    d
}

pub fn para(doc: &Document, i: usize) -> &Inlines {
    match &doc.blocks[i].kind {
        BlockKind::Paragraph(x) => x,
        k => panic!("block {i} is not a paragraph: {k:?}"),
    }
}

/// Deterministic xorshift for fuzz-style tests.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Random valid-UTF-8 soup biased towards Markdown-significant characters.
pub fn soup(rng: &mut Rng, len: usize) -> String {
    const PIECES: &[&str] = &[
        "#",
        "##",
        " ",
        "  ",
        "\n",
        "\n\n",
        "\t",
        "*",
        "**",
        "_",
        "__",
        "~~",
        "==",
        "`",
        "```",
        "~~~",
        "[",
        "]",
        "(",
        ")",
        "![",
        "[[",
        "]]",
        "|",
        "-",
        "- ",
        "- [ ] ",
        "1. ",
        "> ",
        ">",
        "<",
        ">",
        "<b>",
        "</b>",
        "<!--",
        "-->",
        "$",
        "$$",
        "%%",
        "^",
        "\\",
        "&amp;",
        ":",
        ":rocket:",
        "[^1]",
        "[^1]: ",
        "---",
        "***",
        "http://x.y",
        "a",
        "word",
        "é",
        "日本",
        "😀",
        "\u{FFFC}",
        "\r",
        "\u{0}",
        "#tag",
        "[!NOTE]",
        "{",
        "}",
        "=",
        "+",
        "!",
        "<img src=a>",
        "<details>",
        "</div>",
        "@",
    ];
    let mut s = String::new();
    while s.len() < len {
        s.push_str(PIECES[(rng.next() % PIECES.len() as u64) as usize]);
    }
    s
}
