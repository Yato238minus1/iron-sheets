//! The inline `$...$` rule (see `iron_sheets_md::math`): preview parser and editor highlighter agree.
mod common;
use common::*;
use iron_sheets_md::*;

/// Math sources of all paragraphs' inline formulas, in order.
fn inline_math(text: &str) -> Vec<String> {
    let d = parse_checked(text);
    let mut v = Vec::new();
    for b in &d.blocks {
        if let BlockKind::Paragraph(i) = &b.kind {
            for o in &i.objects {
                if let ObjectKind::Math { source } = &o.kind {
                    v.push(source.clone());
                }
            }
        }
    }
    v
}

/// Slices the highlighter marks as math.
fn hl_math(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    highlight(text, &mut out);
    out.iter().filter(|s| s.tok == Tok::Math).map(|s| text[s.range.clone()].to_owned()).collect()
}

const CASES: &[(&str, &[&str])] = &[
    ("strict $x+y$ ok", &["x+y"]),
    ("space before closer $x + y $ ok", &["x + y"]),
    ("typst style $ x + y $ ok", &["x + y"]),
    ("space after opener $ x+y$ ok", &["x+y"]),
    ("at line start $x + y $ and end $ a^2 $", &["x + y", "a^2"]),
    ("$ x $", &["x"]),
    ("costs $5 and $10", &[]),
    ("$5 – $10 range", &[]),
    ("a $ 5 here", &[]),
    ("lone $ sign", &[]),
    ("echo $HOME and $PATH", &[]),
    ("price $ 5 and $ 10", &[]),
    ("escaped \\$x + y \\$ text", &[]),
    ("code `$x + y $` span", &[]),
    ("double ``a $x + y $`` span", &[]),
    ("two $a + b $ and $ c + d $ end", &["a + b", "c + d"]),
    ("after failure $5 and $ x + y $", &["x + y"]),
    ("$$ x + y $$ display inline", &["x + y"]),
];

#[test]
fn rule_cases_in_preview() {
    for (text, want) in CASES {
        // `$$ ... $$` in the middle of a paragraph is an inline display formula.
        assert_eq!(inline_math(text), *want, "{text:?}");
    }
}

#[test]
fn highlighter_agrees_with_preview() {
    for (text, want) in CASES {
        let hl: Vec<String> = hl_math(text);
        assert_eq!(hl.len(), want.len(), "{text:?}: {hl:?}");
        for (h, w) in hl.iter().zip(*want) {
            assert_eq!(h.trim_matches('$').trim(), *w, "{text:?}");
        }
    }
}

#[test]
fn soft_line_break_inside_formula() {
    let t = "first $x +\ny $ rest\n";
    assert_eq!(inline_math(t), ["x + y"]);
    // blank line breaks it
    assert!(inline_math("first $x +\n\ny $ rest\n").is_empty());
}

#[test]
fn fenced_code_keeps_dollars() {
    let d = parse_checked("```\n$x + y $ and $ z $\n```\n");
    assert_eq!(d.blocks[0].kind, BlockKind::Code { lang: "".into(), text: "$x + y $ and $ z $".into() });
}

#[test]
fn text_without_mask_leaks() {
    // Masked bytes never reach the model, also when the formula is not accepted by comrak.
    for t in ["> $ x + y $\n", "- $x + y $\n", "| a | $ x + y $ |\n|---|---|\n| 1 | 2 |\n", "# H $ x + y $\n"] {
        let d = parse_checked(t);
        let s = format!("{d:?}");
        assert!(!s.contains("\\u{1}"), "{t:?}: {s}");
    }
}

#[test]
fn display_block_unchanged() {
    let d = parse_checked("$$ x + y $$\n\n$$\na\n$$\n");
    assert_eq!(d.blocks[0].kind, BlockKind::Math { source: "x + y".into() });
}

/// Masking replaces single bytes: block line ranges must equal those of the strict twin.
#[test]
fn line_mapping_is_unchanged() {
    let relaxed = "# T $ a + b $\n\n- item $x + y $\n  - nested $ q + r $ more\n\n> quote $ a - b $\n> second $c + d $\n\n```\n$ code $\n```\n\npara $ x\n= y $\nend\n\n| a | b |\n|---|---|\n| $ x + y $ | 2 |\n\n- [ ] task $ t + u $\n\nlast $v + w $\n";
    // Same bytes, strict form: the blanks next to the delimiters become letters.
    let mut strict = String::new();
    let b = relaxed.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        let next_dollar = b.get(i + 1) == Some(&b'$');
        let prev_dollar = i > 0 && b[i - 1] == b'$';
        if c == b' ' && (next_dollar || prev_dollar) && !relaxed[..i].ends_with("```\n") {
            strict.push('q');
        } else {
            strict.push(c as char);
        }
    }
    let (a, s) = (parse_checked(relaxed), parse_checked(&strict));
    assert_eq!(a.line_count, s.line_count);
    let la: Vec<_> = a.blocks.iter().map(|b| b.lines.clone()).collect();
    let ls: Vec<_> = s.blocks.iter().map(|b| b.lines.clone()).collect();
    assert_eq!(la, ls);
    // and the formulas are found
    assert!(inline_math(relaxed).len() >= 7, "{:?}", inline_math(relaxed));
}

#[test]
fn linear_time() {
    let text = "$ a + b $ and $c $ and 5$ 6$ $\n".repeat(20_000);
    let t = std::time::Instant::now();
    let d = parse(&text);
    let ms = t.elapsed().as_millis();
    assert!(!d.blocks.is_empty());
    assert!(ms < 1500, "{ms} ms");
}
