//! Fast single-pass scanners for wiki-links, links, headings, tags and front matter.
//! No Markdown parser; good enough for indexing. Line numbers are 0-based.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiTarget<'a> {
    pub note: &'a str,
    pub heading: Option<&'a str>,
    pub block: Option<&'a str>,
    pub alias: Option<&'a str>,
}

/// Parse the text between `[[` and `]]`: `Note#Heading|Alias`, `Note#^block`, `#Heading`.
pub fn parse_wikilink(inner: &str) -> WikiTarget<'_> {
    let (left, alias) = match inner.find('|') {
        Some(i) => (&inner[..i], Some(inner[i + 1..].trim()).filter(|s| !s.is_empty())),
        None => (inner, None),
    };
    let left = left.strip_suffix('\\').unwrap_or(left);
    let (note, frag) = match left.find('#') {
        Some(i) => (&left[..i], Some(&left[i + 1..])),
        None => (left, None),
    };
    let (mut heading, mut block) = (None, None);
    if let Some(f) = frag {
        let f = f.trim();
        if let Some(b) = f.strip_prefix('^') {
            block = Some(b.trim()).filter(|s| !s.is_empty());
        } else {
            heading = Some(f).filter(|s| !s.is_empty());
        }
    }
    WikiTarget { note: note.trim(), heading, block, alias }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    pub level: u8,
    pub text: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutLink {
    /// Note or file name/path as written (no `#fragment`, URL-decoded for Markdown links).
    pub target: String,
    pub line: u32,
    pub embed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteScan {
    pub title: Option<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<OutLink>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    /// Where each link's target sits in the text; filled only by `scan_markdown_spans` (parallel to `links`).
    #[serde(skip)]
    pub spans: Vec<LinkSpan>,
    #[serde(skip)]
    pub want_spans: bool,
}

/// Byte range of a link target inside its line (0-based line, range without `#fragment`, `?query` and brackets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub line: u32,
    pub start: usize,
    pub end: usize,
    /// `[[wiki]]` or `![[embed]]`, else a Markdown link; `<angle>` destinations are written raw, others percent-encoded.
    pub wiki: bool,
    pub angle: bool,
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn has_scheme(s: &str) -> bool {
    let mut it = s.char_indices();
    match it.next() {
        Some((_, c)) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for (i, c) in it {
        if c == ':' {
            return i >= 2; // single letter = Windows drive
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return false;
        }
    }
    false
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn parse_inline_list(v: &str) -> Vec<String> {
    let v = v.trim();
    let v = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(v);
    v.split(',').map(|x| unquote(x).trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Returns number of lines consumed by front matter (0 if none).
fn parse_front_matter(lines: &[&str], scan: &mut NoteScan) -> usize {
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return 0;
    }
    let Some(end) = lines.iter().skip(1).position(|l| matches!(l.trim_end(), "---" | "...")) else {
        return 0;
    };
    let end = end + 1;
    let mut title = None;
    let mut tags = Vec::new();
    let mut aliases = Vec::new();
    #[derive(PartialEq)]
    enum Key {
        None,
        Tags,
        Aliases,
        Other,
    }
    let mut cur = Key::None;
    for l in &lines[1..end] {
        let t = l.trim_end();
        let trimmed = t.trim_start();
        if trimmed.starts_with("- ") || trimmed == "-" {
            let item = unquote(trimmed[1..].trim()).to_string();
            match cur {
                Key::Tags => tags.push(item),
                Key::Aliases => aliases.push(item),
                _ => {}
            }
            continue;
        }
        if t.starts_with(' ') || t.starts_with('\t') {
            continue;
        }
        let Some((k, v)) = t.split_once(':') else {
            cur = Key::None;
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        cur = Key::Other;
        match k.as_str() {
            "title" => {
                let v = unquote(v);
                if !v.is_empty() {
                    title = Some(v.to_string());
                }
            }
            "tags" | "tag" => {
                cur = Key::Tags;
                tags.extend(parse_inline_list(v));
            }
            "aliases" | "alias" => {
                cur = Key::Aliases;
                aliases.extend(parse_inline_list(v));
            }
            _ => {}
        }
    }
    scan.title = title;
    for t in tags {
        push_unique(&mut scan.tags, t.trim_start_matches('#').to_string());
    }
    for a in aliases {
        push_unique(&mut scan.aliases, a);
    }
    end + 1
}

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let c = *t.as_bytes().first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == c).count();
    if n < 3 {
        return None;
    }
    if c == b'`' && t[n..].contains('`') {
        return None;
    }
    Some((c, n))
}

fn fence_close(line: &str, ch: u8, len: usize) -> bool {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return false;
    }
    let n = t.bytes().take_while(|&b| b == ch).count();
    n >= len && t[n..].trim().is_empty()
}

fn atx_heading(line: &str) -> Option<(u8, &str, usize)> {
    let t = line.trim_start_matches(' ');
    let indent = line.len() - t.len();
    if indent > 3 {
        return None;
    }
    let n = t.bytes().take_while(|&b| b == b'#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if !(rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')) {
        return None;
    }
    let mut text = rest.trim();
    let stripped = text.trim_end_matches('#');
    if stripped.len() != text.len() && (stripped.is_empty() || stripped.ends_with(' ') || stripped.ends_with('\t')) {
        text = stripped.trim_end();
    }
    Some((n as u8, text, indent + n))
}

fn setext_level(line: &str) -> Option<u8> {
    let t = line.trim_end();
    let t2 = t.trim_start_matches(' ');
    if t.len() - t2.len() > 3 || t2.is_empty() {
        return None;
    }
    if t2.bytes().all(|b| b == b'=') {
        Some(1)
    } else if t2.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

fn is_list_marker(t: &str) -> bool {
    let t = t.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let d = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    d > 0 && d < 10 && (t[d..].starts_with(". ") || t[d..].starts_with(") "))
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Scan one line of prose for links and tags.
fn scan_inline(line: &str, ln: u32, out: &mut NoteScan) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'`' => {
                let n = b[i..].iter().take_while(|&&c| c == b'`').count();
                // find closing run of exactly n backticks
                let mut j = i + n;
                let mut found = None;
                while j < b.len() {
                    if b[j] == b'`' {
                        let m = b[j..].iter().take_while(|&&c| c == b'`').count();
                        if m == n {
                            found = Some(j + m);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                i = found.unwrap_or(i + n);
            }
            b'!' if b.get(i + 1) == Some(&b'[') => {
                if b.get(i + 2) == Some(&b'[') {
                    i = wikilink(line, i + 3, ln, true, out).unwrap_or(i + 1);
                } else {
                    i = md_link(line, i + 1, ln, true, out).unwrap_or(i + 1);
                }
            }
            b'[' => {
                if b.get(i + 1) == Some(&b'[') {
                    i = wikilink(line, i + 2, ln, false, out).unwrap_or(i + 1);
                } else {
                    i = md_link(line, i, ln, false, out).unwrap_or(i + 1);
                }
            }
            b'#' => {
                let prev = line[..i].chars().next_back();
                let ok_prev = match prev {
                    None => true,
                    Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '{' | '"' | '\'' | ',' | ';'),
                };
                let mut end = i + 1;
                if ok_prev {
                    for (k, c) in line[i + 1..].char_indices() {
                        if is_tag_char(c) {
                            end = i + 1 + k + c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    let body = line[i + 1..end].trim_end_matches(['/', '-', '_']);
                    if !body.is_empty() && body.chars().any(|c| !c.is_ascii_digit()) {
                        push_unique(&mut out.tags, body.to_string());
                    }
                }
                i = end.max(i + 1);
            }
            _ => i += 1,
        }
    }
}

/// `start` is just after `[[`. Returns the index after `]]`.
fn wikilink(line: &str, start: usize, ln: u32, embed: bool, out: &mut NoteScan) -> Option<usize> {
    let rest = &line[start..];
    let end = rest.find("]]")?;
    let inner = &rest[..end];
    if inner.contains('[') || inner.trim().is_empty() {
        return None;
    }
    let t = parse_wikilink(inner);
    if !t.note.is_empty() {
        out.links.push(OutLink { target: t.note.to_string(), line: ln, embed });
        if out.want_spans {
            let mut k = inner.find(['#', '|']).unwrap_or(inner.len());
            if inner[..k].ends_with('\\') && inner[k..].starts_with('|') {
                k -= 1;
            }
            let raw = &inner[..k];
            let lead = raw.len() - raw.trim_start().len();
            out.spans.push(LinkSpan { line: ln, start: start + lead, end: start + raw.trim_end().len(), wiki: true, angle: false });
        }
    }
    Some(start + end + 2)
}

/// `open` points at `[`. Returns the index after the closing `)`.
fn md_link(line: &str, open: usize, ln: u32, embed: bool, out: &mut NoteScan) -> Option<usize> {
    let b = line.as_bytes();
    let mut depth = 0;
    let mut j = open;
    let mut close = None;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    let close = close?;
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let ds = close + 2;
    let mut k = ds;
    let dest: &str;
    if b.get(ds) == Some(&b'<') {
        let e = line[ds + 1..].find('>')?;
        dest = &line[ds + 1..ds + 1 + e];
        k = ds + 1 + e + 1;
    } else {
        let mut pd = 0;
        while k < b.len() {
            match b[k] {
                b'\\' => k += 1,
                b'(' => pd += 1,
                b')' => {
                    if pd == 0 {
                        break;
                    }
                    pd -= 1;
                }
                b' ' | b'\t' => break,
                _ => {}
            }
            k += 1;
        }
        dest = line.get(ds..k.min(b.len()))?;
    }
    // skip optional title up to ')'
    let end = line[k.min(line.len())..].find(')').map(|p| k + p + 1)?;
    let dest_start = if b.get(ds) == Some(&b'<') { ds + 1 } else { ds };
    let mut d = dest;
    if let Some(p) = d.find('#') {
        d = &d[..p];
    }
    if let Some(p) = d.find('?') {
        d = &d[..p];
    }
    if !d.is_empty() && !has_scheme(d) && !d.starts_with("//") {
        out.links.push(OutLink { target: percent_decode(d), line: ln, embed });
        if out.want_spans {
            out.spans.push(LinkSpan { line: ln, start: dest_start, end: dest_start + d.len(), wiki: false, angle: b.get(ds) == Some(&b'<') });
        }
    }
    Some(end)
}

pub fn scan_markdown(text: &str) -> NoteScan {
    scan_markdown_with(text, false)
}

/// Like `scan_markdown`, also recording the position of every link target (`NoteScan::spans`).
pub fn scan_markdown_spans(text: &str) -> NoteScan {
    scan_markdown_with(text, true)
}

fn scan_markdown_with(text: &str, want_spans: bool) -> NoteScan {
    let mut scan = NoteScan { want_spans, ..Default::default() };
    let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let start = parse_front_matter(&lines, &mut scan);
    let fm_title = scan.title.take();

    let mut fence: Option<(u8, usize)> = None;
    let mut para: Option<(usize, usize, u32)> = None; // line range in `lines` so far (for setext)
    let mut prev_blank = true;
    let mut in_indented = false;

    for (idx, line) in lines.iter().enumerate().skip(start) {
        let ln = idx as u32;
        if let Some((ch, len)) = fence {
            if fence_close(line, ch, len) {
                fence = None;
            }
            prev_blank = false;
            continue;
        }
        if line.trim().is_empty() {
            para = None;
            prev_blank = true;
            // blank lines do not end an indented code block
            continue;
        }
        let indented = line.starts_with("    ") || line.starts_with('\t');
        if indented && para.is_none() && (prev_blank || in_indented) && !is_list_marker(line) {
            in_indented = true;
            prev_blank = false;
            continue;
        }
        in_indented = false;
        prev_blank = false;
        if let Some(f) = fence_open(line) {
            fence = Some(f);
            para = None;
            continue;
        }
        if let Some((level, text, skip)) = atx_heading(line) {
            scan.headings.push(Heading { level, text: text.to_string(), line: ln });
            let from = scan.spans.len();
            scan_inline(&line[skip..], ln, &mut scan);
            for sp in &mut scan.spans[from..] {
                sp.start += skip;
                sp.end += skip;
            }
            para = None;
            continue;
        }
        if let Some(level) = setext_level(line) {
            if let Some((a, b, l)) = para.take() {
                let text = if a == b {
                    lines[a].trim().to_string()
                } else {
                    let cap: usize = lines[a..=b].iter().map(|s| s.trim().len() + 1).sum();
                    let mut s = String::with_capacity(cap);
                    for (i, line) in lines[a..=b].iter().enumerate() {
                        if i > 0 {
                            s.push(' ');
                        }
                        s.push_str(line.trim());
                    }
                    s
                };
                scan.headings.push(Heading { level, text, line: l });
                continue;
            }
        }
        let t = line.trim_start();
        let eligible = !(is_list_marker(line) || t.starts_with('>') || t.starts_with('|') || t.starts_with("<"));
        match (&mut para, eligible) {
            (Some((_, end, _)), true) => *end = idx,
            (None, true) => para = Some((idx, idx, ln)),
            (_, false) => para = None,
        }
        scan_inline(line, ln, &mut scan);
    }
    scan.title = fm_title.or_else(|| scan.headings.iter().find(|h| h.level == 1).map(|h| h.text.clone()));
    scan
}

pub fn scan_typst(text: &str) -> NoteScan {
    let mut scan = NoteScan::default();
    let mut in_raw = false;
    for (idx, line) in text.split('\n').enumerate() {
        let t = line.trim_start();
        if t.starts_with("```") {
            // an odd number of fences on a line toggles raw mode
            if t.matches("```").count() % 2 == 1 {
                in_raw = !in_raw;
            }
            continue;
        }
        if in_raw {
            continue;
        }
        let n = t.bytes().take_while(|&b| b == b'=').count();
        if n == 0 {
            continue;
        }
        let rest = &t[n..];
        if rest.starts_with([' ', '\t']) {
            let text = rest.trim().trim_end_matches('\r').to_string();
            scan.headings.push(Heading { level: n.min(255) as u8, text, line: idx as u32 });
        }
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilink_parsing() {
        let t = parse_wikilink("Note#Head|Alias");
        assert_eq!((t.note, t.heading, t.block, t.alias), ("Note", Some("Head"), None, Some("Alias")));
        let t = parse_wikilink("Note#^abc");
        assert_eq!((t.note, t.heading, t.block), ("Note", None, Some("abc")));
        let t = parse_wikilink("#Heading");
        assert_eq!((t.note, t.heading), ("", Some("Heading")));
        let t = parse_wikilink("dir/Note");
        assert_eq!((t.note, t.heading, t.alias), ("dir/Note", None, None));
        let t = parse_wikilink("Note\\|Alias");
        assert_eq!((t.note, t.alias), ("Note", Some("Alias")));
        assert_eq!(parse_wikilink("").note, "");
    }

    fn targets(s: &NoteScan) -> Vec<(&str, u32, bool)> {
        s.links.iter().map(|l| (l.target.as_str(), l.line, l.embed)).collect()
    }

    #[test]
    fn links_basic() {
        let s =
            scan_markdown("See [[A]] and [[b/B#h|x]].\n![[img.png]] [t](rel/My%20Note.md#x) [e](https://x.org/a.md) [m](mailto:a@b) ![i](p.png) [s](#same)");
        assert_eq!(targets(&s), vec![("A", 0, false), ("b/B", 0, false), ("img.png", 1, true), ("rel/My Note.md", 1, false), ("p.png", 1, true)]);
    }

    #[test]
    fn links_skip_code() {
        let t = "```rust\n[[InFence]] #nottag\n```\n\n    [[Indented]]\n\n`[[Inline]]` and ``a ` [[Inline2]]`` ok [[Real]]\n~~~\n[[Tilde]]\n~~~\n[[After]] \\[[Escaped]]";
        let s = scan_markdown(t);
        let names: Vec<_> = s.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(names, vec!["Real", "After"]);
        assert!(s.tags.is_empty());
    }

    #[test]
    fn nested_list_not_code() {
        let s = scan_markdown("- a\n\n    - [[Nested]]\n");
        assert_eq!(s.links.len(), 1);
    }

    #[test]
    fn headings_and_title() {
        let s = scan_markdown("# One #\n\n## Two ##  \n#NoHeading\nSetext\n===\n\nMulti\nline\n---\n####### seven\n    # indented\n### ");
        let h: Vec<_> = s.headings.iter().map(|h| (h.level, h.text.as_str(), h.line)).collect();
        assert_eq!(h, vec![(1, "One", 0), (2, "Two", 2), (1, "#NoHeading Setext", 3), (2, "Multi line", 7), (3, "", 12)]);
        assert_eq!(s.title.as_deref(), Some("One"));
        assert_eq!(s.tags, vec!["NoHeading"]);
    }

    #[test]
    fn tags() {
        let s = scan_markdown("# Heading\ntext #tag1 and #nested/tag, (#paren) not#this #123 #a-b_c.\nurl http://x.com/#frag [[N#Head]] `#code`");
        assert_eq!(s.tags, vec!["tag1", "nested/tag", "paren", "a-b_c"]);
        let s = scan_markdown("#日本語 x");
        assert_eq!(s.tags, vec!["日本語"]);
    }

    #[test]
    fn front_matter() {
        let t = "---\ntitle: \"My Title\"\ntags: [a, \"#b\"]\naliases:\n  - Foo\n  - 'Bar Baz'\nother: x\n---\n# Heading\n[[L]] #body\n";
        let s = scan_markdown(t);
        assert_eq!(s.title.as_deref(), Some("My Title"));
        assert_eq!(s.tags, vec!["a", "b", "body"]);
        assert_eq!(s.aliases, vec!["Foo", "Bar Baz"]);
        assert_eq!(s.headings[0].line, 8);
        assert_eq!(s.links[0].line, 9);
        let s = scan_markdown("---\ntags: x, y\nalias: Z\n---\n");
        assert_eq!((s.tags, s.aliases), (vec!["x".into(), "y".into()], vec!["Z".into()]));
        // unterminated front matter is ordinary text
        let s = scan_markdown("---\ntitle: nope\n# H");
        assert_eq!(s.title.as_deref(), Some("H"));
    }

    #[test]
    fn malformed_inputs() {
        for t in ["[[", "[[]]", "![[", "[a](", "[a](<b", "![", "[[a [b]]", "`", "``` ", "#", "\\", "[a]((((", "%", "[x](%zz%)", ""] {
            let _ = scan_markdown(t);
        }
        let s = scan_markdown("[x](%e4%bd%a0.md) [y](a%2)");
        assert_eq!(s.links[0].target, "你.md");
        assert_eq!(s.links[1].target, "a%2");
        let s = scan_markdown("a\r\n===\r\n[[X]]\r\n");
        assert_eq!(s.headings.len(), 1);
        assert_eq!(s.links.len(), 1);
    }

    #[test]
    fn typst() {
        let s = scan_typst("= Title\ntext\n== Sub *x*\n```\n= not\n```\n=nospace\n=== Deep\n#let x = 1");
        let h: Vec<_> = s.headings.iter().map(|h| (h.level, h.text.as_str(), h.line)).collect();
        assert_eq!(h, vec![(1, "Title", 0), (2, "Sub *x*", 2), (3, "Deep", 7)]);
        assert!(s.links.is_empty() && s.tags.is_empty());
    }
}
