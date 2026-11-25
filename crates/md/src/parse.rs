//! comrak -> flat block model.

use crate::html_block;
use crate::inline::{self, build, is_image_target, plain_text, split_wiki};
use crate::model::*;
use comrak::nodes::{AlertType, Node, NodeValue, Sourcepos, TableAlignment};
use comrak::{Anchorizer, Arena, Options};
use rustc_hash::FxHasher;
use smallvec::SmallVec;
use std::hash::{Hash, Hasher};

/// Parses `text` (LF line endings) into a flat [`Document`]. Never panics.
pub fn parse(text: &str) -> Document {
    let line_count = memchr::memchr_iter(b'\n', text.as_bytes()).count() as u32 + 1;
    // comrak asserts internally in a few corner cases; never let that reach the UI.
    // Relaxed `$ x $` formulas are masked for comrak (same length, same lines); see `math`.
    let masked = crate::math::mask_relaxed(text);
    let src = masked.as_deref().unwrap_or(text);
    let result = std::panic::catch_unwind(|| parse_inner(src));
    let mut blocks = result.unwrap_or_else(|_| fallback(text));
    fix_lines(&mut blocks, line_count);
    Document { blocks, line_count }
}

fn options() -> Options<'static> {
    let mut o = Options::default();
    let e = &mut o.extension;
    e.strikethrough = true;
    e.table = true;
    e.autolink = true;
    e.tasklist = true;
    e.footnotes = true;
    e.alerts = true;
    e.ofm_callouts = true;
    e.math_dollars = true;
    e.math_code = true;
    e.shortcodes = true;
    e.wikilinks_title_after_pipe = true;
    e.highlight = true;
    e.ofm_comments = true;
    e.front_matter_delimiter = Some("---".to_owned());
    o.parse.leave_footnote_definitions = true;
    o
}

fn parse_inner(text: &str) -> Vec<Block> {
    let arena = Arena::new();
    let root = comrak::parse_document(&arena, text, &options());
    let mut cx = Cx {
        text,
        blocks: Vec::new(),
        stack: Vec::new(),
        lists: Vec::new(),
        next_id: 0,
        anchorizer: Anchorizer::new(),
        line_starts: None,
        title_depth: 0,
        prev_heading: false,
    };
    cx.walk(root);
    cx.blocks
}

/// Plain paragraphs split on blank lines; used only if comrak panics.
fn fallback(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut start: Option<u32> = None;
    let mut buf = String::new();
    let flush = |start: &mut Option<u32>, end: u32, buf: &mut String, blocks: &mut Vec<Block>| {
        if let Some(s) = start.take() {
            let mut b = inline::Builder::new();
            b.push_str(buf.trim());
            let kind = BlockKind::Paragraph(b.finish(false));
            let spacing = if blocks.is_empty() { Spacing::None } else { Spacing::Normal };
            blocks.push(make_block(kind, s..end, SmallVec::new(), spacing));
            buf.clear();
        }
    };
    let mut n = 0u32;
    for (i, line) in text.split('\n').enumerate() {
        n = i as u32;
        if line.trim().is_empty() {
            flush(&mut start, n, &mut buf, &mut blocks);
        } else {
            if start.is_none() {
                start = Some(n);
            } else {
                buf.push(' ');
            }
            buf.push_str(line.trim());
        }
    }
    flush(&mut start, n + 1, &mut buf, &mut blocks);
    blocks
}

fn make_block(kind: BlockKind, lines: std::ops::Range<u32>, containers: SmallVec<[Container; 2]>, spacing: Spacing) -> Block {
    let mut h = FxHasher::default();
    match &kind {
        // the slug depends on earlier headings (duplicate suffixes): positional, not hashed
        BlockKind::Heading { level, content, .. } => {
            0xA5u8.hash(&mut h);
            level.hash(&mut h);
            content.hash(&mut h);
        }
        k => k.hash(&mut h),
    }
    containers.hash(&mut h);
    Block { kind, lines, containers, spacing, hash: h.finish() }
}

/// Sorted, non-overlapping, inside `[0, line_count)`.
fn fix_lines(blocks: &mut [Block], line_count: u32) {
    let mut prev_end = 0u32;
    for b in blocks {
        let s = b.lines.start.max(prev_end).min(line_count);
        let e = b.lines.end.max(s).min(line_count.max(s));
        b.lines = s..e;
        prev_end = e;
    }
}

struct Frame {
    kind: ContainerKind,
    id: u32,
    pending_first: bool,
    emitted: bool,
    tight: bool,
    line: u32,
}

struct ListState {
    ordered: bool,
    next: u32,
    tight: bool,
}

struct Cx<'t> {
    text: &'t str,
    blocks: Vec<Block>,
    stack: Vec<Frame>,
    lists: Vec<ListState>,
    next_id: u32,
    anchorizer: Anchorizer,
    line_starts: Option<Vec<u32>>,
    title_depth: u32,
    prev_heading: bool,
}

fn lines_of(sp: Sourcepos) -> std::ops::Range<u32> {
    let s = sp.start.line.saturating_sub(1) as u32;
    let e = (sp.end.line as u32).max(s + 1);
    s..e
}

impl<'t> Cx<'t> {
    fn walk<'a>(&mut self, root: Node<'a>) {
        let mut node = root;
        let mut entering = true;
        loop {
            if entering {
                if self.enter(node) {
                    if let Some(c) = node.first_child() {
                        node = c;
                        continue;
                    }
                    self.exit(node);
                }
            } else {
                self.exit(node);
            }
            if node.same_node(root) {
                break;
            }
            if let Some(n) = node.next_sibling() {
                node = n;
                entering = true;
                continue;
            }
            match node.parent() {
                Some(p) => {
                    node = p;
                    entering = false;
                }
                None => break,
            }
        }
    }

    fn push_frame(&mut self, kind: ContainerKind, tight: bool, sp: Sourcepos) {
        self.next_id += 1;
        self.stack.push(Frame { kind, id: self.next_id, pending_first: true, emitted: false, tight, line: sp.start.line.saturating_sub(1) as u32 });
    }

    fn emit(&mut self, kind: BlockKind, lines: std::ops::Range<u32>) {
        let mut containers: SmallVec<[Container; 2]> = SmallVec::new();
        for f in self.stack.iter_mut() {
            containers.push(Container { id: f.id, kind: f.kind.clone(), first: f.pending_first });
            f.pending_first = false;
            f.emitted = true;
        }
        let is_heading = matches!(kind, BlockKind::Heading { .. });
        let spacing = if self.blocks.is_empty() {
            Spacing::None
        } else if is_heading || self.prev_heading {
            Spacing::Loose
        } else if self.stack.last().is_some_and(|f| f.tight && matches!(f.kind, ContainerKind::ListItem { .. })) {
            Spacing::Tight
        } else {
            Spacing::Normal
        };
        self.prev_heading = is_heading;
        self.blocks.push(make_block(kind, lines, containers, spacing));
    }

    fn line_start(&mut self, line0: usize) -> Option<usize> {
        let text = self.text;
        let ls = self.line_starts.get_or_insert_with(|| {
            // comrak also treats a bare CR as a line break, so we do the same here
            let b = text.as_bytes();
            let mut v = vec![0u32];
            for p in memchr::memchr2_iter(b'\n', b'\r', b) {
                if b[p] == b'\r' && b.get(p + 1) == Some(&b'\n') {
                    continue;
                }
                v.push(p as u32 + 1);
            }
            v
        });
        ls.get(line0).map(|&x| x as usize)
    }

    fn task_offset(&mut self, sp: Sourcepos, item: Sourcepos) -> u32 {
        let valid = |t: &str, off: usize| {
            off > 0
                && t.is_char_boundary(off)
                && t.as_bytes().get(off - 1) == Some(&b'[')
                && t[off..].chars().next().is_some_and(|c| t.as_bytes().get(off + c.len_utf8()) == Some(&b']'))
        };
        if sp.start.line >= 1
            && let Some(ls) = self.line_start(sp.start.line - 1)
        {
            let off = ls + sp.start.column.saturating_sub(1);
            if valid(self.text, off) {
                return off as u32;
            }
        }
        // Fallback: first `[ ]`/`[x]` on the item's first line.
        if let Some(ls) = self.line_start(item.start.line.saturating_sub(1)) {
            let rest = &self.text[ls..];
            let line = rest.split(['\n', '\r']).next().unwrap_or("");
            for pat in ["[ ]", "[x]", "[X]"] {
                if let Some(p) = line.find(pat) {
                    return (ls + p + 1) as u32;
                }
            }
        }
        0
    }

    fn enter<'a>(&mut self, node: Node<'a>) -> bool {
        let sp;
        enum Act {
            Descend,
            Leaf,
        }
        let act = {
            let data = node.data();
            sp = data.sourcepos;
            match &data.value {
                NodeValue::Document => Act::Descend,
                NodeValue::FrontMatter(s) => {
                    let t = strip_front_matter(s);
                    self.emit(BlockKind::FrontMatter { text: t }, lines_of(sp));
                    Act::Leaf
                }
                NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                    self.push_frame(ContainerKind::Quote, false, sp);
                    Act::Descend
                }
                NodeValue::Alert(a) => {
                    let kind = match a.alert_type {
                        AlertType::Note => AlertKind::Note,
                        AlertType::Tip => AlertKind::Tip,
                        AlertType::Important => AlertKind::Important,
                        AlertType::Warning => AlertKind::Warning,
                        AlertType::Caution => AlertKind::Caution,
                    };
                    self.push_frame(ContainerKind::Alert { kind, title: a.title.clone() }, false, sp);
                    Act::Descend
                }
                NodeValue::Callout(c) => {
                    let kind = match c.callout_type.as_str() {
                        "note" => AlertKind::Note,
                        "tip" => AlertKind::Tip,
                        "important" => AlertKind::Important,
                        "warning" => AlertKind::Warning,
                        "caution" => AlertKind::Caution,
                        other => AlertKind::Other(other.to_owned()),
                    };
                    self.push_frame(ContainerKind::Alert { kind, title: None }, false, sp);
                    Act::Descend
                }
                NodeValue::CalloutTitle => {
                    self.title_depth += 1;
                    Act::Descend
                }
                NodeValue::List(l) => {
                    self.lists.push(ListState {
                        ordered: l.list_type == comrak::nodes::ListType::Ordered,
                        next: l.start.min(u32::MAX as usize) as u32,
                        tight: l.tight,
                    });
                    Act::Descend
                }
                NodeValue::Item(_) | NodeValue::TaskItem(_) => {
                    let (ordered, tight) = match self.lists.last_mut() {
                        Some(l) => {
                            let n = l.next;
                            l.next = l.next.saturating_add(1);
                            (l.ordered.then_some(n), l.tight)
                        }
                        None => (None, true),
                    };
                    let task = if let NodeValue::TaskItem(t) = &data.value {
                        let tsp = t.symbol_sourcepos;
                        Some((t.symbol.is_some(), tsp))
                    } else {
                        None
                    };
                    let task = task.map(|(checked, tsp)| Task { checked, offset: self.task_offset(tsp, sp) });
                    self.push_frame(ContainerKind::ListItem { ordered, task }, tight, sp);
                    Act::Descend
                }
                NodeValue::FootnoteDefinition(d) => {
                    self.push_frame(ContainerKind::Footnote { label: d.name.clone() }, false, sp);
                    Act::Descend
                }
                NodeValue::Paragraph => {
                    drop(data);
                    self.paragraph(node, sp);
                    Act::Leaf
                }
                NodeValue::Heading(h) => {
                    let level = h.level.clamp(1, 6);
                    drop(data);
                    let content = build(node, false);
                    let plain: String = content.text.replace('\u{FFFC}', "");
                    let slug = self.anchorizer.anchorize(&plain);
                    self.emit(BlockKind::Heading { level, content, slug }, lines_of(sp));
                    Act::Leaf
                }
                NodeValue::CodeBlock(cb) => {
                    let lang = cb.info.split_whitespace().next().unwrap_or("").to_lowercase();
                    let mut body = crate::math::unmask(&cb.literal).into_owned();
                    if body.ends_with('\n') {
                        body.pop();
                    }
                    let kind = match lang.as_str() {
                        "math" => BlockKind::Math { source: body },
                        "typst" => BlockKind::Typst { source: body },
                        _ => BlockKind::Code { lang, text: body },
                    };
                    self.emit(kind, lines_of(sp));
                    Act::Leaf
                }
                NodeValue::HtmlBlock(h) => {
                    let lit = crate::math::unmask(&h.literal).into_owned();
                    drop(data);
                    self.html_block(&lit, sp);
                    Act::Leaf
                }
                NodeValue::ThematicBreak => {
                    self.emit(BlockKind::Rule, lines_of(sp));
                    Act::Leaf
                }
                NodeValue::Table(t) => {
                    let aligns: Vec<Align> = t
                        .alignments
                        .iter()
                        .map(|a| match a {
                            TableAlignment::None => Align::None,
                            TableAlignment::Left => Align::Left,
                            TableAlignment::Center => Align::Center,
                            TableAlignment::Right => Align::Right,
                        })
                        .collect();
                    drop(data);
                    let mut header = Vec::new();
                    let mut rows = Vec::new();
                    for row in node.children() {
                        let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                        let cells: Vec<Inlines> = row.children().map(|c| build(c, false)).collect();
                        if is_header {
                            header = cells;
                        } else {
                            rows.push(cells);
                        }
                    }
                    self.emit(BlockKind::Table { aligns, header, rows }, lines_of(sp));
                    Act::Leaf
                }
                NodeValue::MultilineComment(_) => Act::Leaf,
                _ => Act::Descend,
            }
        };
        matches!(act, Act::Descend)
    }

    fn exit<'a>(&mut self, node: Node<'a>) {
        let is_frame = {
            let d = node.data();
            match &d.value {
                NodeValue::CalloutTitle => {
                    self.title_depth = self.title_depth.saturating_sub(1);
                    return;
                }
                NodeValue::List(_) => {
                    self.lists.pop();
                    return;
                }
                NodeValue::BlockQuote
                | NodeValue::MultilineBlockQuote(_)
                | NodeValue::Alert(_)
                | NodeValue::Callout(_)
                | NodeValue::Item(_)
                | NodeValue::TaskItem(_)
                | NodeValue::FootnoteDefinition(_) => true,
                _ => false,
            }
        };
        if is_frame {
            if let Some(f) = self.stack.last()
                && !f.emitted
            {
                let l = f.line;
                self.emit(BlockKind::Paragraph(Inlines::default()), l..l + 1);
            }
            self.stack.pop();
        }
    }

    fn paragraph<'a>(&mut self, node: Node<'a>, sp: Sourcepos) {
        let lines = lines_of(sp);
        // Callout title paragraph: becomes the container's title.
        if self.title_depth > 0 {
            let inl = build(node, false);
            let title = inl.text.replace('\u{FFFC}', "").trim().to_owned();
            if let Some(Frame { kind: ContainerKind::Alert { title: t, .. }, .. }) = self.stack.last_mut()
                && !title.is_empty()
            {
                *t = Some(title);
            }
            return;
        }
        // Special single-child paragraphs.
        if let Some(only) = node.first_child()
            && only.next_sibling().is_none()
        {
            let special = match &only.data().value {
                NodeValue::Image(l) => {
                    Some(BlockKind::Image { src: l.url.clone(), alt: plain_text(only), title: (!l.title.is_empty()).then(|| l.title.clone()), wiki: false })
                }
                NodeValue::WikiLink(w) if w.embed => {
                    if is_image_target(&w.url) {
                        let (src, _, _) = split_wiki(&w.url);
                        let alias = plain_text(only);
                        Some(BlockKind::Image { src, alt: if alias == w.url { String::new() } else { alias }, title: None, wiki: true })
                    } else {
                        Some(BlockKind::Embed { target: w.url.clone() })
                    }
                }
                NodeValue::Math(m) if m.dollar_math && m.display_math => Some(BlockKind::Math { source: crate::math::unmask(&m.literal).trim().to_owned() }),
                _ => None,
            };
            if let Some(k) = special {
                self.emit(k, lines);
                return;
            }
        }
        let inl = build(node, true);
        if inl.is_empty() {
            return; // e.g. only a %%comment%%
        }
        self.emit(BlockKind::Paragraph(inl), lines);
    }

    fn html_block(&mut self, literal: &str, sp: Sourcepos) {
        let lines = lines_of(sp);
        let pieces = html_block::simplify(literal, lines.start);
        match pieces {
            None => {
                let t = literal.trim_end_matches(['\n', '\r']).to_owned();
                self.emit(BlockKind::Html { text: t }, lines);
            }
            Some(pieces) => {
                for p in pieces {
                    let mut r = p.lines;
                    r.end = r.end.min(lines.end).max(r.start + 1);
                    match p.kind {
                        html_block::Piece::Rule => self.emit(BlockKind::Rule, r),
                        html_block::Piece::Content { inlines, heading } => {
                            if let Some(level) = heading {
                                let plain = inlines.text.replace('\u{FFFC}', "");
                                let slug = self.anchorizer.anchorize(&plain);
                                self.emit(BlockKind::Heading { level, content: inlines, slug }, r);
                            } else if inlines.objects.len() == 1
                                && inlines.text == "\u{FFFC}"
                                && inlines.spans.iter().all(|s| s.link.is_none())
                                && matches!(inlines.objects[0].kind, ObjectKind::Image { .. })
                            {
                                if let ObjectKind::Image { src, alt, title, wiki } = inlines.objects[0].kind.clone() {
                                    self.emit(BlockKind::Image { src, alt, title, wiki }, r);
                                }
                            } else {
                                self.emit(BlockKind::Paragraph(inlines), r);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// comrak hands us the front matter including both delimiter lines.
fn strip_front_matter(s: &str) -> String {
    let s = s.trim_end_matches(['\n', '\r']);
    let body = match s.split_once('\n') {
        Some((_, rest)) => rest,
        None => "",
    };
    let body = match body.rsplit_once('\n') {
        Some((b, last)) if last.trim_end() == "---" => b,
        None if body.trim_end() == "---" => "",
        _ => body,
    };
    body.to_owned()
}
