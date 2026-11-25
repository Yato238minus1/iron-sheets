//! Inline content builder (text + spans + links + objects) and the comrak inline walker.

use crate::html::{self, Tag};
use crate::model::*;
use comrak::nodes::{Node, NodeValue};

#[derive(Default)]
pub(crate) struct Builder {
    text: String,
    spans: Vec<Span>,
    links: Vec<Link>,
    objects: Vec<InlineObject>,
    node_cnt: [u16; Style::COUNT],
    html_cnt: [u16; Style::COUNT],
    cur: Style,
    link: Option<u16>,
    link_stack: Vec<Option<u16>>,
}

const ALL_STYLES: [Style; Style::COUNT] =
    [Style::BOLD, Style::ITALIC, Style::STRIKE, Style::CODE, Style::SUP, Style::SUB, Style::UNDERLINE, Style::HIGHLIGHT, Style::KBD, Style::TAG, Style::DIM];

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text_len(&self) -> usize {
        self.text.len()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn recompute(&mut self) {
        let mut s = Style::EMPTY;
        for (i, st) in ALL_STYLES.iter().enumerate() {
            if self.node_cnt[i] > 0 || self.html_cnt[i] > 0 {
                s |= *st;
            }
        }
        self.cur = s;
    }

    fn idx(s: Style) -> usize {
        s.bits().trailing_zeros() as usize
    }

    pub fn node_open(&mut self, s: Style) {
        let i = Self::idx(s);
        self.node_cnt[i] = self.node_cnt[i].saturating_add(1);
        self.recompute();
    }

    pub fn node_close(&mut self, s: Style) {
        let i = Self::idx(s);
        self.node_cnt[i] = self.node_cnt[i].saturating_sub(1);
        self.recompute();
    }

    pub fn html_open(&mut self, s: Style) {
        let i = Self::idx(s);
        self.html_cnt[i] = self.html_cnt[i].saturating_add(1);
        self.recompute();
    }

    pub fn html_close(&mut self, s: Style) {
        let i = Self::idx(s);
        self.html_cnt[i] = self.html_cnt[i].saturating_sub(1);
        self.recompute();
    }

    pub fn push_link(&mut self, target: LinkTarget, title: Option<String>) {
        self.link_stack.push(self.link);
        if self.links.len() < u16::MAX as usize {
            self.links.push(Link { target, title });
            self.link = Some((self.links.len() - 1) as u16);
        }
    }

    pub fn pop_link(&mut self) {
        if let Some(l) = self.link_stack.pop() {
            self.link = l;
        }
    }

    pub fn push_str(&mut self, s: &str) {
        self.push_str_with(s, Style::EMPTY);
    }

    pub fn push_str_with(&mut self, s: &str, extra: Style) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(&crate::math::unmask(s));
        self.extend_span(start, extra);
    }

    fn extend_span(&mut self, start: usize, extra: Style) {
        let end = self.text.len();
        let style = self.cur | extra;
        let (start, end) = (start as u32, end as u32);
        if let Some(last) = self.spans.last_mut()
            && last.style == style
            && last.link == self.link
            && last.range.end == start
        {
            last.range.end = end;
            return;
        }
        self.spans.push(Span { range: start..end, style, link: self.link });
    }

    pub fn push_object(&mut self, kind: ObjectKind) {
        let at = self.text.len() as u32;
        self.text.push('\u{FFFC}');
        self.extend_span(at as usize, Style::EMPTY);
        self.objects.push(InlineObject { at, kind });
    }

    /// Applies an HTML tag (inline mapping rules).
    pub fn html_tag(&mut self, tag: &Tag) {
        let style = match tag.name.as_str() {
            "b" | "strong" | "summary" => Style::BOLD,
            "i" | "em" | "cite" | "dfn" | "var" => Style::ITALIC,
            "s" | "del" | "strike" => Style::STRIKE,
            "u" | "ins" => Style::UNDERLINE,
            "mark" => Style::HIGHLIGHT,
            "kbd" => Style::KBD,
            "code" | "tt" | "samp" => Style::CODE,
            "sub" => Style::SUB,
            "sup" => Style::SUP,
            "br" => {
                if !tag.closing {
                    self.push_str("\n");
                }
                return;
            }
            "a" => {
                if tag.closing {
                    self.pop_link();
                } else {
                    match tag.attr("href") {
                        Some(h) if !h.is_empty() => {
                            self.push_link(LinkTarget::Url(h.to_owned()), tag.attr("title").filter(|t| !t.is_empty()).map(str::to_owned))
                        }
                        _ => self.link_stack.push(self.link),
                    }
                }
                return;
            }
            "img" => {
                if !tag.closing
                    && let Some(src) = tag.attr("src")
                {
                    self.push_object(ObjectKind::Image {
                        src: src.to_owned(),
                        alt: tag.attr("alt").unwrap_or("").to_owned(),
                        title: tag.attr("title").filter(|t| !t.is_empty()).map(str::to_owned),
                        wiki: false,
                    });
                }
                return;
            }
            _ => return,
        };
        if tag.closing {
            self.html_close(style);
        } else {
            self.html_open(style);
        }
    }

    pub fn finish(mut self, paragraph: bool) -> Inlines {
        self.post_process(paragraph);
        Inlines { text: self.text, spans: self.spans, links: self.links, objects: self.objects }
    }

    /// `#tags` and trailing `^blockid`.
    fn post_process(&mut self, paragraph: bool) {
        let mut ranges: Vec<(u32, u32, Style, Option<u16>)> = Vec::new();
        let mut tag_names: Vec<(usize, String)> = Vec::new();
        let text = self.text.as_str();
        let bytes = text.as_bytes();
        if memchr::memchr(b'#', bytes).is_some() {
            let mut from = 0;
            while let Some(p) = memchr::memchr(b'#', &bytes[from..]) {
                let i = from + p;
                from = i + 1;
                if i > 0 && !text[..i].chars().next_back().is_some_and(char::is_whitespace) {
                    continue;
                }
                let mut j = i + 1;
                for c in text[j..].chars() {
                    if c.is_alphanumeric() || matches!(c, '_' | '-' | '/') {
                        j += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if j == i + 1 || text[i + 1..j].bytes().all(|b| b.is_ascii_digit()) {
                    continue;
                }
                if !self.plain_range(i, j) {
                    continue;
                }
                tag_names.push((ranges.len(), text[i + 1..j].to_owned()));
                ranges.push((i as u32, j as u32, Style::TAG, None));
                from = j;
            }
        }
        if paragraph {
            let t = text.trim_end();
            let (start, tail) = match t.rfind(char::is_whitespace) {
                Some(p) => {
                    let ws = t[p..].chars().next().map_or(1, char::len_utf8);
                    (p + ws, &t[p + ws..])
                }
                None => (0, t),
            };
            if tail.len() > 1 && tail.starts_with('^') && tail[1..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && self.plain_range(start, t.len())
            {
                ranges.push((start as u32, t.len() as u32, Style::DIM, None));
            }
        }
        if ranges.is_empty() {
            return;
        }
        for (ri, name) in tag_names {
            if self.links.len() >= u16::MAX as usize {
                break;
            }
            self.links.push(Link { target: LinkTarget::Tag(name), title: None });
            ranges[ri].3 = Some((self.links.len() - 1) as u16);
        }
        let old = std::mem::take(&mut self.spans);
        self.spans = apply_ranges(old, &ranges);
    }

    /// True when `[a, b)` touches no code span and no link.
    fn plain_range(&self, a: usize, b: usize) -> bool {
        let (a, b) = (a as u32, b as u32);
        let first = self.spans.partition_point(|s| s.range.end <= a);
        self.spans[first..].iter().take_while(|s| s.range.start < b).all(|s| !s.style.contains(Style::CODE) && s.link.is_none())
    }
}

/// Splits `spans` at the (sorted, disjoint) `ranges` and adds their style/link.
fn apply_ranges(spans: Vec<Span>, ranges: &[(u32, u32, Style, Option<u16>)]) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len() + ranges.len() * 2);
    let mut ri = 0;
    for sp in spans {
        let mut start = sp.range.start;
        let end = sp.range.end;
        while start < end {
            while ri < ranges.len() && ranges[ri].1 <= start {
                ri += 1;
            }
            if ri == ranges.len() || ranges[ri].0 >= end {
                out.push(Span { range: start..end, style: sp.style, link: sp.link });
                break;
            }
            let r = ranges[ri];
            if r.0 > start {
                out.push(Span { range: start..r.0, style: sp.style, link: sp.link });
                start = r.0;
            }
            let e = r.1.min(end);
            out.push(Span { range: start..e, style: sp.style | r.2, link: r.3.or(sp.link) });
            start = e;
        }
    }
    out
}

/// Concatenated plain text of a node's descendants (alt texts, titles).
pub(crate) fn plain_text<'a>(node: Node<'a>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        match &d.data().value {
            NodeValue::Text(t) => s.push_str(t),
            NodeValue::Code(c) => s.push_str(&c.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => s.push(' '),
            NodeValue::ShortCode(sc) => s.push_str(&sc.emoji),
            _ => {}
        }
    }
    s
}

pub(crate) fn is_image_target(target: &str) -> bool {
    let name = target.split(['#', '|']).next().unwrap_or(target).trim();
    if cfg!(feature = "excalidraw") && name.len() > 14 && name.as_bytes()[name.len() - 14..].eq_ignore_ascii_case(b".excalidraw.md") {
        return true;
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            const EXTS: [&str; 9] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "ico"];
            EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)) || (cfg!(feature = "excalidraw") && ext.eq_ignore_ascii_case("excalidraw"))
        }
        _ => false,
    }
}

/// `Note#Heading` / `Note#^block` / `#Heading`.
pub(crate) fn split_wiki(url: &str) -> (String, Option<String>, Option<String>) {
    match url.split_once('#') {
        None => (url.trim().to_owned(), None, None),
        Some((note, rest)) => {
            let rest = rest.trim();
            if let Some(b) = rest.strip_prefix('^') {
                (note.trim().to_owned(), None, Some(b.to_owned()))
            } else if rest.is_empty() {
                (note.trim().to_owned(), None, None)
            } else {
                (note.trim().to_owned(), Some(rest.to_owned()), None)
            }
        }
    }
}

/// Builds the inlines of a Paragraph / Heading / TableCell node.
pub(crate) fn build<'a>(root: Node<'a>, paragraph: bool) -> Inlines {
    let mut b = Builder::new();
    walk(&mut b, root);
    b.finish(paragraph)
}

pub(crate) fn walk<'a>(b: &mut Builder, root: Node<'a>) {
    let Some(mut node) = root.first_child() else { return };
    let mut entering = true;
    loop {
        if entering {
            if enter(b, node) {
                if let Some(c) = node.first_child() {
                    node = c;
                    continue;
                }
                exit(b, node);
            }
        } else {
            exit(b, node);
        }
        if let Some(n) = node.next_sibling() {
            node = n;
            entering = true;
            continue;
        }
        match node.parent() {
            Some(p) if !p.same_node(root) => {
                node = p;
                entering = false;
            }
            _ => break,
        }
    }
}

fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

/// Returns true when the node's children must be visited (and `exit` called).
fn enter<'a>(b: &mut Builder, node: Node<'a>) -> bool {
    let data = node.data();
    match &data.value {
        NodeValue::Text(t) => b.push_str(t),
        NodeValue::SoftBreak => b.push_str(" "),
        NodeValue::LineBreak => b.push_str("\n"),
        NodeValue::Code(c) => b.push_str_with(&c.literal, Style::CODE),
        NodeValue::HtmlInline(h) => {
            if !h.starts_with("<!--")
                && let Some((tag, _)) = html::parse_tag(h)
            {
                b.html_tag(&tag);
            }
        }
        NodeValue::ShortCode(sc) => b.push_str(&sc.emoji),
        NodeValue::Math(m) => b.push_object(ObjectKind::Math { source: crate::math::unmask(&m.literal).trim().to_owned() }),
        NodeValue::Image(l) => {
            let alt = plain_text(node);
            b.push_object(ObjectKind::Image { src: l.url.clone(), alt, title: nonempty(&l.title), wiki: false });
        }
        NodeValue::FootnoteReference(f) => {
            b.push_link(LinkTarget::Footnote(f.name.clone()), None);
            let label = if f.ix > 0 { format!("[{}]", f.ix) } else { format!("[{}]", f.name) };
            b.push_str_with(&label, Style::SUP);
            b.pop_link();
        }
        NodeValue::WikiLink(w) => {
            let alias = plain_text(node);
            if w.embed && is_image_target(&w.url) {
                let (src, _, _) = split_wiki(&w.url);
                let src = src.split('|').next().unwrap_or("").to_owned();
                b.push_object(ObjectKind::Image { src, alt: if alias == w.url { String::new() } else { alias }, title: None, wiki: true });
            } else {
                let (note, heading, block) = split_wiki(&w.url);
                let shown = if alias != w.url {
                    alias
                } else {
                    match (&heading, note.is_empty()) {
                        (Some(h), true) => h.clone(),
                        (Some(h), false) => format!("{note} > {h}"),
                        (None, true) => block.clone().map(|x| format!("^{x}")).unwrap_or_default(),
                        (None, false) => note.clone(),
                    }
                };
                b.push_link(LinkTarget::Wiki { note, heading, block }, None);
                b.push_str(&shown);
                b.pop_link();
            }
            return false;
        }
        NodeValue::Link(l) => {
            b.push_link(LinkTarget::Url(l.url.clone()), nonempty(&l.title));
            return true;
        }
        NodeValue::Emph => {
            b.node_open(Style::ITALIC);
            return true;
        }
        NodeValue::Strong => {
            b.node_open(Style::BOLD);
            return true;
        }
        NodeValue::Strikethrough => {
            b.node_open(Style::STRIKE);
            return true;
        }
        NodeValue::Highlight => {
            b.node_open(Style::HIGHLIGHT);
            return true;
        }
        NodeValue::Underline | NodeValue::Insert => {
            b.node_open(Style::UNDERLINE);
            return true;
        }
        NodeValue::Superscript => {
            b.node_open(Style::SUP);
            return true;
        }
        NodeValue::Subscript => {
            b.node_open(Style::SUB);
            return true;
        }
        NodeValue::CommentInline(_) => {}
        _ => return true,
    }
    false
}

fn exit<'a>(b: &mut Builder, node: Node<'a>) {
    match &node.data().value {
        NodeValue::Link(_) => b.pop_link(),
        NodeValue::Emph => b.node_close(Style::ITALIC),
        NodeValue::Strong => b.node_close(Style::BOLD),
        NodeValue::Strikethrough => b.node_close(Style::STRIKE),
        NodeValue::Highlight => b.node_close(Style::HIGHLIGHT),
        NodeValue::Underline | NodeValue::Insert => b.node_close(Style::UNDERLINE),
        NodeValue::Superscript => b.node_close(Style::SUP),
        NodeValue::Subscript => b.node_close(Style::SUB),
        _ => {}
    }
}
