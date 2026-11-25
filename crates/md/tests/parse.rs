mod common;
use common::*;
use iron_sheets_md::*;

fn kinds(doc: &Document) -> Vec<&'static str> {
    doc.blocks
        .iter()
        .map(|b| match &b.kind {
            BlockKind::Paragraph(_) => "p",
            BlockKind::Heading { .. } => "h",
            BlockKind::Code { .. } => "code",
            BlockKind::Math { .. } => "math",
            BlockKind::Typst { .. } => "typst",
            BlockKind::Table { .. } => "table",
            BlockKind::Rule => "rule",
            BlockKind::Image { .. } => "img",
            BlockKind::Embed { .. } => "embed",
            BlockKind::Html { .. } => "html",
            BlockKind::FrontMatter { .. } => "fm",
        })
        .collect()
}

fn styles_of(i: &Inlines, s: Style) -> Vec<&str> {
    i.spans.iter().filter(|sp| sp.style.contains(s)).map(|sp| &i.text[sp.range.start as usize..sp.range.end as usize]).collect()
}

#[test]
fn rich_document_invariants() {
    let d = parse_checked(RICH);
    assert!(d.blocks.len() > 30);
}

#[test]
fn empty_and_trivial() {
    for t in ["", "\n", "\n\n\n", " ", "a", "a\n"] {
        parse_checked(t);
    }
    assert_eq!(parse("").line_count, 1);
    assert_eq!(parse("a\n").line_count, 2);
    assert!(parse("").blocks.is_empty());
}

#[test]
fn headings_and_outline() {
    let t = "# One\n\ntext\n\nTwo\n---\n\n### Three `code` and *em*\n\n# One\n";
    let d = parse_checked(t);
    let o: Vec<_> = d.outline().collect();
    assert_eq!(o, vec![(1, "One", 0), (2, "Two", 4), (3, "Three code and em", 7), (1, "One", 9)]);
    // slugs: GitHub style, duplicates numbered
    let slugs: Vec<_> = d
        .blocks
        .iter()
        .filter_map(|b| match &b.kind {
            BlockKind::Heading { slug, .. } => Some(slug.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(slugs, ["one", "two", "three-code-and-em", "one-1"]);
    assert_eq!(d.find_heading("two"), Some(2));
    assert_eq!(d.find_heading("TWO"), Some(2));
    assert_eq!(d.find_heading("three-code-and-em"), Some(3));
    assert_eq!(d.find_heading("Three code and em"), Some(3));
    assert_eq!(d.find_heading("One"), Some(0));
    assert_eq!(d.find_heading("one-1"), Some(4));
    assert_eq!(d.find_heading("nope"), None);
}

#[test]
fn block_at_line_binary_search() {
    let t = "# A\n\npara\nlines\n\n```\ncode\n```\n";
    let d = parse_checked(t);
    assert_eq!(d.block_at_line(0), Some(0));
    assert_eq!(d.block_at_line(1), None);
    assert_eq!(d.block_at_line(2), Some(1));
    assert_eq!(d.block_at_line(3), Some(1));
    assert_eq!(d.block_at_line(4), None);
    assert_eq!(d.block_at_line(5), Some(2));
    assert_eq!(d.block_at_line(7), Some(2));
    assert_eq!(d.block_at_line(99), None);
    for l in 0..d.line_count {
        if let Some(i) = d.block_at_line(l) {
            assert!(d.blocks[i].lines.contains(&l));
        }
    }
}

#[test]
fn front_matter() {
    let d = parse_checked("---\ntitle: x\ntags: [a]\n---\n\n# H\n");
    assert_eq!(kinds(&d), ["fm", "h"]);
    assert_eq!(d.blocks[0].lines, 0..4);
    match &d.blocks[0].kind {
        BlockKind::FrontMatter { text } => assert_eq!(text, "title: x\ntags: [a]"),
        _ => panic!(),
    }
    // a lone --- later is a rule
    let d = parse_checked("a\n\n---\n\nb\n");
    assert_eq!(kinds(&d), ["p", "rule", "p"]);
}

#[test]
fn inline_styles() {
    let d = parse_checked(
        "a **b** *c* ~~d~~ `e` ==f== <u>g</u> <sub>h</sub> <sup>i</sup> <kbd>j</kbd> <mark>k</mark> <s>l</s> <i>m</i> <em>n</em> <strong>o</strong> <code>p</code>\n",
    );
    let i = para(&d, 0);
    assert_eq!(styles_of(i, Style::BOLD), ["b", "o"]);
    assert_eq!(styles_of(i, Style::ITALIC), ["c", "m", "n"]);
    assert_eq!(styles_of(i, Style::STRIKE), ["d", "l"]);
    assert_eq!(styles_of(i, Style::CODE), ["e", "p"]);
    assert_eq!(styles_of(i, Style::HIGHLIGHT), ["f", "k"]);
    assert_eq!(styles_of(i, Style::UNDERLINE), ["g"]);
    assert_eq!(styles_of(i, Style::SUB), ["h"]);
    assert_eq!(styles_of(i, Style::SUP), ["i"]);
    assert_eq!(styles_of(i, Style::KBD), ["j"]);
}

#[test]
fn nested_styles_combine() {
    let d = parse_checked("***both*** and **bold *and italic* more**\n");
    let i = para(&d, 0);
    assert_eq!(styles_of(i, Style::BOLD | Style::ITALIC), ["both", "and italic"]);
    assert_eq!(styles_of(i, Style::BOLD), ["both", "bold ", "and italic", " more"]);
}

#[test]
fn breaks() {
    let d = parse_checked("soft\nbreak  \nhard\\\nslash<br>html<br/>x\n");
    assert_eq!(para(&d, 0).text, "soft break\nhard\nslash\nhtml\nx");
}

#[test]
fn html_inline_rules() {
    let d =
        parse_checked("a <span class=x>keep</span> <!-- gone --> <unknown>inner</unknown> <a href=\"http://q\">lnk</a> <img src=\"p.png\" alt=\"A\"> end\n");
    let i = para(&d, 0);
    assert!(i.text.starts_with("a keep  inner lnk "), "{:?}", i.text);
    assert_eq!(i.links.len(), 1);
    assert_eq!(i.links[0].target, LinkTarget::Url("http://q".into()));
    assert_eq!(i.objects.len(), 1);
    assert!(matches!(&i.objects[0].kind, ObjectKind::Image { src, alt, wiki: false, .. } if src == "p.png" && alt == "A"));
}

#[test]
fn links_images_autolinks() {
    let d = parse_checked("[t](http://a \"T\") ![alt *x*](i.png \"it\") https://b.c/d me@x.org <http://z.y> [ref][r]\n\n[r]: http://ref \"RT\"\n");
    let i = para(&d, 0);
    assert_eq!(i.links[0], Link { target: LinkTarget::Url("http://a".into()), title: Some("T".into()) });
    assert_eq!(i.objects[0].kind, ObjectKind::Image { src: "i.png".into(), alt: "alt x".into(), title: Some("it".into()), wiki: false });
    let urls: Vec<_> = i
        .links
        .iter()
        .filter_map(|l| match &l.target {
            LinkTarget::Url(u) => Some(u.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(urls, ["http://a", "https://b.c/d", "mailto:me@x.org", "http://z.y", "http://ref"]);
    assert_eq!(i.links.last().unwrap().title.as_deref(), Some("RT"));
}

#[test]
fn image_in_link_stays_inline() {
    let d = parse_checked("[![badge](b.svg)](http://x)\n");
    assert_eq!(kinds(&d), ["p"]);
    let i = para(&d, 0);
    assert_eq!(i.objects.len(), 1);
    assert!(i.spans.iter().any(|s| s.link.is_some()));
}

#[test]
fn image_blocks() {
    let d = parse_checked("![alt](a.png \"T\")\n\n![[b.PNG]]\n\n![[c.png|300]]\n\n![[Note]]\n\n![[Note#Head]]\n\nx ![[d.png]] y\n");
    assert_eq!(kinds(&d), ["img", "img", "img", "embed", "embed", "p"]);
    assert_eq!(d.blocks[0].kind, BlockKind::Image { src: "a.png".into(), alt: "alt".into(), title: Some("T".into()), wiki: false });
    assert_eq!(d.blocks[1].kind, BlockKind::Image { src: "b.PNG".into(), alt: "".into(), title: None, wiki: true });
    assert!(matches!(&d.blocks[2].kind, BlockKind::Image { src, alt, wiki: true, .. } if src == "c.png" && alt == "300"));
    assert_eq!(d.blocks[3].kind, BlockKind::Embed { target: "Note".into() });
    assert_eq!(d.blocks[4].kind, BlockKind::Embed { target: "Note#Head".into() });
    assert!(matches!(&para(&d, 5).objects[0].kind, ObjectKind::Image { src, wiki: true, .. } if src == "d.png"));
}

#[test]
fn wikilinks() {
    let d = parse_checked("[[Note]] [[Note|Alias]] [[Note#Head]] [[#Only]] [[Note#^blk]] [[A#B|shown]]\n");
    let i = para(&d, 0);
    let w: Vec<_> = i
        .links
        .iter()
        .map(|l| match &l.target {
            LinkTarget::Wiki { note, heading, block } => (note.as_str(), heading.as_deref(), block.as_deref()),
            t => panic!("{t:?}"),
        })
        .collect();
    assert_eq!(
        w,
        [
            ("Note", None, None),
            ("Note", None, None),
            ("Note", Some("Head"), None),
            ("", Some("Only"), None),
            ("Note", None, Some("blk")),
            ("A", Some("B"), None)
        ]
    );
    assert_eq!(i.text, "Note Alias Note > Head Only Note shown");
}

#[test]
fn obsidian_comments_removed() {
    let d = parse_checked("keep %%hidden%% this\n\n%%\nblock\ncomment\n%%\n\nafter\n\n%%only%%\n");
    assert_eq!(kinds(&d), ["p", "p"]);
    assert_eq!(para(&d, 0).text, "keep  this");
    assert_eq!(para(&d, 1).text, "after");
}

#[test]
fn tags() {
    let d = parse_checked("#one and #two/sub and a#not and #2024 and `#code` and [x](#frag) #é日 end #tag-x_y.\n");
    let i = para(&d, 0);
    assert_eq!(styles_of(i, Style::TAG), ["#one", "#two/sub", "#é日", "#tag-x_y"]);
    let tags: Vec<_> = i
        .links
        .iter()
        .filter_map(|l| match &l.target {
            LinkTarget::Tag(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tags, ["one", "two/sub", "é日", "tag-x_y"]);
    // tag at start of a paragraph, and in a heading
    let d = parse_checked("#start of line\n\n# Head #h\n");
    assert_eq!(styles_of(para(&d, 0), Style::TAG), ["#start"]);
    match &d.blocks[1].kind {
        BlockKind::Heading { content, .. } => assert_eq!(styles_of(content, Style::TAG), ["#h"]),
        k => panic!("{k:?}"),
    }
}

#[test]
fn block_id_is_dim() {
    let d = parse_checked("A sentence. ^abc-12\n\n^only\n\nno ^caret in middle here\n");
    assert_eq!(styles_of(para(&d, 0), Style::DIM), ["^abc-12"]);
    assert_eq!(styles_of(para(&d, 1), Style::DIM), ["^only"]);
    assert!(styles_of(para(&d, 2), Style::DIM).is_empty());
}

#[test]
fn footnotes() {
    let d = parse_checked("Text[^a] and[^b].\n\n[^a]: First\n    continued\n\n[^b]: Second\n\n    Para two\n");
    let i = para(&d, 0);
    assert_eq!(i.text, "Text[1] and[2].");
    assert_eq!(styles_of(i, Style::SUP), ["[1]", "[2]"]);
    assert_eq!(i.links[0].target, LinkTarget::Footnote("a".into()));
    assert_eq!(i.links[1].target, LinkTarget::Footnote("b".into()));
    let fn_blocks: Vec<_> = d.blocks.iter().filter(|b| b.containers.iter().any(|c| matches!(&c.kind, ContainerKind::Footnote { .. }))).collect();
    assert_eq!(fn_blocks.len(), 3);
    assert!(matches!(&fn_blocks[0].containers[0].kind, ContainerKind::Footnote { label } if label == "a"));
    assert!(fn_blocks[1].containers[0].first);
    assert!(!fn_blocks[2].containers[0].first);
    assert_eq!(fn_blocks[1].containers[0].id, fn_blocks[2].containers[0].id);
    // a reference without definition is just text
    let d = parse_checked("lonely[^zz] ref\n");
    assert_eq!(para(&d, 0).text, "lonely[^zz] ref");
}

#[test]
fn emoji_shortcodes() {
    let d = parse_checked("go :rocket: and :+1: not :nonexistent:\n");
    assert_eq!(para(&d, 0).text, "go 🚀 and 👍 not :nonexistent:");
}

#[test]
fn math() {
    let d = parse_checked("inline $a+b$ and $$c$$ here\n\n$$\nx^2\n$$\n\n$$y$$\n\n```math\ne=mc^2\n```\n\ncost $5 and $6\n");
    assert_eq!(kinds(&d), ["p", "math", "math", "math", "p"]);
    let i = para(&d, 0);
    assert_eq!(i.objects.len(), 2);
    assert_eq!(i.objects[0].kind, ObjectKind::Math { source: "a+b".into() });
    assert_eq!(i.objects[1].kind, ObjectKind::Math { source: "c".into() });
    assert_eq!(i.text, "inline \u{FFFC} and \u{FFFC} here");
    assert_eq!(d.blocks[1].kind, BlockKind::Math { source: "x^2".into() });
    assert_eq!(d.blocks[1].lines, 2..5);
    assert_eq!(d.blocks[2].kind, BlockKind::Math { source: "y".into() });
    assert_eq!(d.blocks[3].kind, BlockKind::Math { source: "e=mc^2".into() });
    assert!(para(&d, 4).objects.is_empty());
}

#[test]
fn code_blocks() {
    let d = parse_checked("```Rust ignore\nfn x() {}\n```\n\n```typst\n#x\n```\n\n```typ\n#y\n```\n\n    indented\n    two\n\n~~~\nunclosed\n");
    assert_eq!(kinds(&d), ["code", "typst", "code", "code", "code"]);
    assert_eq!(d.blocks[0].kind, BlockKind::Code { lang: "rust".into(), text: "fn x() {}".into() });
    assert_eq!(d.blocks[1].kind, BlockKind::Typst { source: "#x".into() });
    assert_eq!(d.blocks[2].kind, BlockKind::Code { lang: "typ".into(), text: "#y".into() });
    assert_eq!(d.blocks[3].kind, BlockKind::Code { lang: "".into(), text: "indented\ntwo".into() });
    assert_eq!(d.blocks[4].kind, BlockKind::Code { lang: "".into(), text: "unclosed".into() });
    assert_eq!(d.blocks[0].lines, 0..3);
    assert_eq!(d.blocks[4].lines, 15..17);
}

#[test]
fn tables() {
    let d = parse_checked("| a | `b` |  c |\n|:--|:-:|--:|\n| 1 | **2** | [[N]] |\n| x |\n\nafter\n");
    match &d.blocks[0].kind {
        BlockKind::Table { aligns, header, rows } => {
            assert_eq!(aligns, &[Align::Left, Align::Center, Align::Right]);
            assert_eq!(header.len(), 3);
            assert_eq!(header[1].spans[0].style, Style::CODE);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0][1].spans[0].style, Style::BOLD);
            assert!(matches!(rows[0][2].links[0].target, LinkTarget::Wiki { .. }));
        }
        k => panic!("{k:?}"),
    }
    assert_eq!(d.blocks[0].lines, 0..4);
    assert_eq!(d.blocks[1].lines, 5..6);
}

#[test]
fn lists_flatten() {
    let t = "- a\n  - b\n    more\n\n    para\n- c\n\n1. x\n2. y\n";
    let d = parse_checked(t);
    assert_eq!(kinds(&d), ["p", "p", "p", "p", "p", "p"]);
    // a
    assert_eq!(d.blocks[0].containers.len(), 1);
    assert!(d.blocks[0].containers[0].first);
    // b: nested
    assert_eq!(d.blocks[1].containers.len(), 2);
    assert!(d.blocks[1].containers[1].first);
    assert!(!d.blocks[1].containers[0].first);
    assert_eq!(d.blocks[1].containers[0].id, d.blocks[0].containers[0].id);
    // "para" is the second block of item b
    assert_eq!(d.blocks[2].containers[1].id, d.blocks[1].containers[1].id);
    assert!(!d.blocks[2].containers[1].first);
    // c is a new item
    assert_ne!(d.blocks[3].containers[0].id, d.blocks[0].containers[0].id);
    // ordered numbers
    let nums: Vec<_> = d.blocks[4..]
        .iter()
        .map(|b| match &b.containers[0].kind {
            ContainerKind::ListItem { ordered, .. } => *ordered,
            _ => panic!(),
        })
        .collect();
    assert_eq!(nums, [Some(1), Some(2)]);
    assert_eq!(d.blocks[1].spacing, Spacing::Normal); // loose
    let d = parse_checked("- a\n- b\n- c\n");
    assert_eq!(d.blocks[0].spacing, Spacing::None);
    assert_eq!(d.blocks[1].spacing, Spacing::Tight);
    assert_eq!(d.blocks[2].spacing, Spacing::Tight);
}

#[test]
fn ordered_start_number() {
    let d = parse_checked("7. a\n8. b\n");
    let n: Vec<_> = d
        .blocks
        .iter()
        .map(|b| match &b.containers[0].kind {
            ContainerKind::ListItem { ordered, .. } => *ordered,
            _ => None,
        })
        .collect();
    assert_eq!(n, [Some(7), Some(8)]);
}

#[test]
fn task_items() {
    let t = "- [ ] open\n- [x] done\n  > - [X] quoted é\n1. [ ] ordered\n";
    let d = parse_checked(t);
    let tasks: Vec<_> = d
        .blocks
        .iter()
        .flat_map(|b| b.containers.iter())
        .filter_map(|c| match &c.kind {
            ContainerKind::ListItem { task: Some(t), .. } if c.first => Some(*t),
            _ => None,
        })
        .collect();
    assert_eq!(tasks.len(), 4);
    assert_eq!(tasks[0], Task { checked: false, offset: 3 });
    assert_eq!(tasks[1], Task { checked: true, offset: 14 });
    assert!(tasks[2].checked);
    for t2 in &tasks {
        let c = t.as_bytes()[t2.offset as usize];
        assert!(matches!(c, b' ' | b'x' | b'X'), "offset {} -> {:?}", t2.offset, c as char);
    }
    assert_eq!(t.as_bytes()[tasks[2].offset as usize], b'X');
    // toggling through the edit helper round-trips
    let op = toggle_task(t, tasks[0].offset as usize).unwrap();
    assert_eq!(op.apply(t).lines().next().unwrap(), "- [x] open");
}

#[test]
fn quotes_and_alerts() {
    let d = parse_checked("> a\n> > b\nlazy\n\n> [!NOTE]\n> n\n\n> [!warning] Careful!\n> w\n\n> [!faq]- Q?\n> a\n\n> [!TIP]\n");
    assert_eq!(d.blocks[0].containers.len(), 1);
    assert_eq!(d.blocks[0].containers[0].kind, ContainerKind::Quote);
    assert_eq!(d.blocks[1].containers.len(), 2);
    assert_eq!(para(&d, 1).text, "b lazy");
    assert_eq!(d.blocks[2].containers[0].kind, ContainerKind::Alert { kind: AlertKind::Note, title: None });
    assert_eq!(d.blocks[3].containers[0].kind, ContainerKind::Alert { kind: AlertKind::Warning, title: Some("Careful!".into()) });
    assert_eq!(d.blocks[4].containers[0].kind, ContainerKind::Alert { kind: AlertKind::Other("faq".into()), title: Some("Q?".into()) });
    // alert without a body still yields a (empty) block so it is drawn
    assert_eq!(d.blocks[5].containers[0].kind, ContainerKind::Alert { kind: AlertKind::Tip, title: None });
    assert!(para(&d, 5).text.is_empty());
}

#[test]
fn empty_list_items_are_kept() {
    let d = parse_checked("- \n- x\n");
    assert_eq!(d.blocks.len(), 2);
    assert!(para(&d, 0).is_empty());
}

#[test]
fn html_blocks() {
    let t = "<details>\n<summary>Click</summary>\n\nhidden\n\n</details>\n\n<div align=\"center\">\n<img src=\"logo.png\" alt=\"Logo\">\n</div>\n\n<!-- c -->\n\n<p align=\"center\">\n  <a href=\"http://x\"><img src=\"b.svg\"></a>\n  <b>bold</b> text\n</p>\n\n<table><tr><td>x</td></tr></table>\n\n<h2 align=center>Title</h2>\n\n<hr>\n\n<br>\n";
    let d = parse_checked(t);
    assert_eq!(kinds(&d), ["p", "p", "img", "p", "html", "h", "rule"], "{d:#?}");
    assert_eq!(para(&d, 0).text, "Click");
    assert_eq!(styles_of(para(&d, 0), Style::BOLD), ["Click"]);
    assert_eq!(para(&d, 1).text, "hidden");
    assert!(matches!(&d.blocks[2].kind, BlockKind::Image { src, alt, .. } if src == "logo.png" && alt == "Logo"));
    let i = para(&d, 3);
    assert_eq!(i.objects.len(), 1);
    assert!(i.text.ends_with("bold text"), "{:?}", i.text);
    assert!(matches!(&d.blocks[4].kind, BlockKind::Html { text } if text.starts_with("<table>")));
    match &d.blocks[5].kind {
        BlockKind::Heading { level, content, slug } => {
            assert_eq!((*level, content.text.as_str(), slug.as_str()), (2, "Title", "title"));
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn html_block_summary_line_range() {
    let d = parse_checked("<details>\n<summary>S</summary>\n</details>\n");
    assert_eq!(kinds(&d), ["p"]);
    assert_eq!(d.blocks[0].lines, 1..2);
}

#[test]
fn line_ranges_tricky() {
    let t = "- a\n  - b\n    > q\n    > r\n\n    ```\n    c\n    ```\n  lazy\n- d\n\ntext\nlazy\n> q1\nlazy q\n\nSetext\n---\n\n|a|b|\n|-|-|\n|1|2|\n\n<div>\n<span>x</span>\n</div>\npara\n";
    let d = parse_checked(t);
    let lines: Vec<&str> = t.split('\n').collect();
    let rng: Vec<(String, String)> =
        d.blocks.iter().map(|b| (lines[b.lines.start as usize].trim().to_string(), lines[b.lines.end as usize - 1].trim().to_string())).collect();
    let expect = [
        ("- a", "- a"),
        ("- b", "- b"),
        ("> q", "> r"),
        ("```", "```"),
        ("lazy", "lazy"),
        ("- d", "- d"),
        ("text", "lazy"),
        ("> q1", "lazy q"),
        ("Setext", "---"),
        ("|a|b|", "|1|2|"),
        ("<span>x</span>", "<span>x</span>"),
        ("para", "para"),
    ];
    for (i, (a, b)) in rng.iter().enumerate() {
        assert_eq!((a.as_str(), b.as_str()), expect[i], "block {i}");
    }
}

#[test]
fn hashes_are_positional_free() {
    let a = parse_checked("para one\n\n- item *x*\n- [ ] task\n\n| a |\n|---|\n| b |\n");
    let b = parse_checked("INSERTED\n\nlines\n\n\npara one\n\n- item *x*\n- [ ] task\n\n| a |\n|---|\n| b |\n");
    let ha: Vec<u64> = a.blocks.iter().map(|b| b.hash).collect();
    let hb: Vec<u64> = b.blocks.iter().map(|b| b.hash).collect();
    assert_eq!(&hb[hb.len() - ha.len()..], &ha[..], "hashes must survive edits elsewhere");
    // different content => different hash
    let c = parse_checked("para two\n");
    assert_ne!(c.blocks[0].hash, ha[0]);
    // task state matters, task offset does not
    let t1 = parse_checked("- [ ] x\n");
    let t2 = parse_checked("\n\n\n- [ ] x\n");
    let t3 = parse_checked("- [x] x\n");
    assert_eq!(t1.blocks[0].hash, t2.blocks[0].hash);
    assert_ne!(t1.blocks[0].hash, t3.blocks[0].hash);
    // container structure matters
    let q = parse_checked("> para one\n");
    assert_ne!(q.blocks[0].hash, ha[0]);
    // heading slug (position dependent: duplicates) does not
    let h1 = parse_checked("# Same\n");
    let h2 = parse_checked("# Same\n\n# Same\n");
    assert_eq!(h1.blocks[0].hash, h2.blocks[1].hash);
}

#[test]
fn container_ids_unique() {
    let d = parse_checked("- a\n- b\n\n> q\n\n> r\n");
    let mut ids: Vec<u32> = d.blocks.iter().flat_map(|b| b.containers.iter().map(|c| c.id)).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4);
}

#[test]
fn spacing_rules() {
    let d = parse_checked("# H\n\npara\n\npara2\n\n## H2\n\nafter\n");
    let s: Vec<_> = d.blocks.iter().map(|b| b.spacing).collect();
    assert_eq!(s, [Spacing::None, Spacing::Loose, Spacing::Normal, Spacing::Loose, Spacing::Loose]);
}

#[test]
fn deep_nesting_does_not_overflow() {
    let t = format!("{}x\n", "> ".repeat(500));
    parse_checked(&t);
    let t = format!("{}x\n", "- ".repeat(200));
    parse_checked(&t);
    let t = format!("{}x{}\n", "*a ".repeat(3000), " b*".repeat(3000));
    parse_checked(&t);
    let t = format!("{}\n", "[".repeat(20000));
    parse_checked(&t);
}

#[test]
fn rich_prefixes_never_panic() {
    // every char-boundary prefix of the rich sample
    for (i, _) in RICH.char_indices() {
        let p = &RICH[..i];
        let d = parse(p);
        check_doc(p, &d);
    }
}

#[test]
fn byte_soup_never_panics() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let iters: usize = std::env::var("MD_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    for n in 0..iters {
        let len = 1 + (rng.next() % 400) as usize;
        let s = soup(&mut rng, len);
        let d = parse(&s);
        check_doc(&s, &d);
        let _ = n;
    }
    // raw random bytes that happen to be valid UTF-8 (ASCII range)
    for _ in 0..500 {
        let len = (rng.next() % 300) as usize;
        let s: String = (0..len).map(|_| (rng.next() % 128) as u8 as char).collect();
        let d = parse(&s);
        check_doc(&s, &d);
    }
}
