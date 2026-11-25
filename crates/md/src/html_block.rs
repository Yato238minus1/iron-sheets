//! Simplifies HTML blocks: tag-only wrappers vanish, `<summary>` becomes bold, `<img>` an
//! image, anything with tags we do not understand stays raw.

use crate::html::{decode_entities, parse_tag};
use crate::inline::Builder;
use crate::model::Inlines;
use std::ops::Range;

pub(crate) enum Piece {
    Rule,
    Content { inlines: Inlines, heading: Option<u8> },
}

pub(crate) struct Out {
    pub lines: Range<u32>,
    pub kind: Piece,
}

const KNOWN: &[&str] = &[
    "details",
    "summary",
    "div",
    "p",
    "center",
    "span",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "figure",
    "figcaption",
    "picture",
    "source",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "a",
    "img",
    "b",
    "strong",
    "i",
    "em",
    "s",
    "del",
    "strike",
    "u",
    "ins",
    "mark",
    "kbd",
    "sub",
    "sup",
    "code",
    "tt",
    "small",
    "big",
    "font",
    "abbr",
    "cite",
    "q",
    "dfn",
    "var",
    "samp",
    "label",
];

const BOUNDARY: &[&str] = &[
    "details",
    "summary",
    "div",
    "p",
    "center",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "figure",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

/// `None` means "keep as raw Html". `base` is the 0-based source line of the block's first line.
pub(crate) fn simplify(lit: &str, base: u32) -> Option<Vec<Out>> {
    struct St {
        b: Builder,
        first: Option<u32>,
        last: u32,
        heading: Option<u8>,
        out: Vec<Out>,
        base: u32,
    }
    impl St {
        fn flush(&mut self) {
            let b = std::mem::take(&mut self.b);
            let heading = self.heading.take();
            let first = self.first.take();
            if b.text_len() == 0 {
                return;
            }
            let inlines = b.finish(true);
            if inlines.objects.is_empty() && inlines.text.trim().is_empty() {
                return;
            }
            let f = first.unwrap_or(0);
            self.out.push(Out { lines: self.base + f..self.base + self.last.max(f) + 1, kind: Piece::Content { inlines, heading } });
        }
    }
    let mut st = St { b: Builder::new(), first: None, last: 0, heading: None, out: Vec::new(), base };
    let bytes = lit.as_bytes();
    let mut line = 0u32;
    let mut i = 0usize;
    let mut pending_space = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'<' {
            let rest = &lit[i..];
            if rest.starts_with("<!--") {
                let end = match rest.strip_prefix("<!--").unwrap_or("").find("-->") {
                    Some(p) => 4 + p + 3,
                    None => rest.len(),
                };
                line += memchr::memchr_iter(b'\n', &rest.as_bytes()[..end]).count() as u32;
                i += end;
                continue;
            }
            if rest.starts_with("<!") || rest.starts_with("<?") {
                return None;
            }
            if let Some((tag, len)) = parse_tag(rest) {
                if !KNOWN.contains(&tag.name.as_str()) {
                    return None;
                }
                let name = tag.name.as_str();
                let has_content = st.b.text_len() > 0;
                if (BOUNDARY.contains(&name) || name == "hr") && has_content {
                    if st.last < line {
                        st.flush();
                    } else {
                        pending_space = true;
                    }
                }
                if name == "hr" {
                    st.out.push(Out { lines: base + line..base + line + 1, kind: Piece::Rule });
                } else if let (Some(d), false) = (name.strip_prefix('h').and_then(|d| d.parse::<u8>().ok()), tag.closing) {
                    if (1..=6).contains(&d) && st.b.text_len() == 0 {
                        st.heading = Some(d);
                    }
                } else {
                    st.b.html_tag(&tag);
                    if name == "img" && !tag.closing && st.b.text_len() > 0 {
                        st.first.get_or_insert(line);
                        st.last = line;
                    }
                }
                line += memchr::memchr_iter(b'\n', &rest.as_bytes()[..len]).count() as u32;
                i += len;
                continue;
            }
        }
        if c.is_ascii_whitespace() {
            if c == b'\n' {
                line += 1;
            }
            pending_space = true;
            i += 1;
            continue;
        }
        // text run up to the next tag or whitespace
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] != b'<' && !bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let seg = decode_entities(&lit[i..j]);
        if pending_space && st.b.text_len() > 0 && !st.b.text().ends_with(['\n', ' ']) {
            st.b.push_str(" ");
        }
        pending_space = false;
        st.b.push_str(&seg);
        st.first.get_or_insert(line);
        st.last = line;
        i = j;
    }
    st.flush();
    Some(st.out)
}
