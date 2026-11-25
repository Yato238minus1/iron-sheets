//! Syntax highlighting of Typst source, on top of `typst_syntax::highlight`.

use std::ops::Range;

use typst_syntax::{LinkedNode, Source, Tag, highlight as tag_of, parse};

use crate::TypTok;

fn map(tag: Tag) -> TypTok {
    match tag {
        Tag::Comment => TypTok::Comment,
        Tag::Punctuation => TypTok::Punct,
        Tag::Escape => TypTok::Escape,
        Tag::Strong => TypTok::Strong,
        Tag::Emph => TypTok::Emph,
        Tag::Link => TypTok::Link,
        Tag::Raw => TypTok::Raw,
        Tag::Label => TypTok::Label,
        Tag::Ref => TypTok::Ref,
        Tag::Heading => TypTok::Heading,
        Tag::ListMarker | Tag::ListTerm => TypTok::ListMarker,
        Tag::MathDelimiter | Tag::MathGroupingParens => TypTok::MathDelim,
        Tag::MathOperator => TypTok::MathOp,
        Tag::Keyword => TypTok::Keyword,
        Tag::Operator => TypTok::Operator,
        Tag::Number => TypTok::Number,
        Tag::String => TypTok::String,
        Tag::Function => TypTok::Function,
        Tag::Interpolated => TypTok::Interpolated,
        Tag::Error => TypTok::Error,
    }
}

fn push(out: &mut Vec<(Range<usize>, TypTok)>, range: Range<usize>, tok: TypTok) {
    if range.is_empty() {
        return;
    }
    if let Some((last, last_tok)) = out.last_mut()
        && *last_tok == tok
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    out.push((range, tok));
}

fn walk(node: &LinkedNode, inherited: TypTok, out: &mut Vec<(Range<usize>, TypTok)>) {
    let tok = tag_of(node).map(map).unwrap_or(inherited);
    let mut any = false;
    for child in node.children() {
        any = true;
        walk(&child, tok, out);
    }
    if !any {
        push(out, node.range(), tok);
    }
}

/// Highlights `text` into `out` (cleared first): sorted, contiguous ranges
/// covering `text` exactly. Adjacent ranges of the same kind are merged.
pub fn highlight(text: &str, out: &mut Vec<(Range<usize>, TypTok)>) {
    out.clear();
    if text.is_empty() {
        return;
    }
    let root = parse(text);
    walk(&LinkedNode::new(&root), TypTok::Text, out);
    // Fast path: well-formed trees cover `0..text.len()` contiguously with adjacent equal tokens merged.
    let mut pos = 0;
    if out.iter().all(|(r, _)| {
        let ok = r.start == pos;
        pos = r.end;
        ok
    }) && pos == text.len()
    {
        return;
    }
    // Defensive: make the coverage invariant hold even if the tree were to
    // leave gaps (it does not for well-formed trees).
    let mut fixed: Vec<(Range<usize>, TypTok)> = Vec::with_capacity(out.len());
    pos = 0;
    for (range, tok) in out.drain(..) {
        if range.start > pos {
            push(&mut fixed, pos..range.start, TypTok::Text);
        }
        pos = range.end;
        push(&mut fixed, range, tok);
    }
    if pos < text.len() {
        push(&mut fixed, pos..text.len(), TypTok::Text);
    }
    *out = fixed;
}

// ---------------------------------------------------------------------------------------------
// Incremental highlighting
// ---------------------------------------------------------------------------------------------

/// Root children on each side of an edit that are highlighted again: a leaf's tag depends on its
/// parent kinds and on the two leaves before / one after it (`typst_syntax::highlight`), and every
/// root child holds at least one leaf.
const MARGIN: usize = 3;

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    const CHUNK: usize = 4096;
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + CHUNK <= n && a[i..i + CHUNK] == b[i..i + CHUNK] {
        i += CHUNK;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn common_suffix(a: &[u8], b: &[u8], max: usize) -> usize {
    const CHUNK: usize = 4096;
    let (la, lb) = (a.len(), b.len());
    let n = max.min(la).min(lb);
    let mut i = 0;
    while i + CHUNK <= n && a[la - i - CHUNK..la - i] == b[lb - i - CHUNK..lb - i] {
        i += CHUNK;
    }
    while i < n && a[la - 1 - i] == b[lb - 1 - i] {
        i += 1;
    }
    i
}

/// Typst highlighter that keeps the syntax tree (incrementally reparsed with `Source::edit`) and the
/// spans of every top-level node. After an edit only the root children around the reparsed range
/// (plus a margin) are walked again. The result equals [`highlight`] (adjacent equal tokens merged).
///
/// `Source::edit` is not always identical to a fresh parse when the text has syntax errors (measured:
/// about 0.4% of random edits on garbage text, none among edits whose old and new trees were free of
/// errors). So a text whose reparsed tree has any diagnostic is parsed again from scratch: such texts
/// cost the same as `highlight`, error-free ones are incremental.
#[derive(Default)]
pub struct IncrementalTypstHighlighter {
    source: Option<Source>,
    /// `(bytes, spans)` per root child.
    children: Vec<(u32, u32)>,
    /// `(length, token)`; no span contains a line break except as its last byte.
    spans: Vec<(u32, TypTok)>,
    redone: usize,
    incremental: u64,
}

impl IncrementalTypstHighlighter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the cached spans describe.
    pub fn text(&self) -> &str {
        self.source.as_ref().map_or("", |s| s.text())
    }

    /// Number of root children the last `update` walked again.
    pub fn children_redone(&self) -> usize {
        self.redone
    }

    /// Number of updates that took the incremental path (the others parsed the whole text).
    pub fn incremental_updates(&self) -> u64 {
        self.incremental
    }

    /// `(length, token)` spans in text order. A span never contains a line break except at its end.
    pub fn tokens(&self) -> &[(u32, TypTok)] {
        &self.spans
    }

    /// Length of the common prefix and suffix with `new` (to pick the best of several caches).
    pub fn similarity(&self, new: &str) -> usize {
        let (p, s) = self.diff(new);
        p + s
    }

    fn diff(&self, new: &str) -> (usize, usize) {
        let old = self.text();
        let (ob, nb) = (old.as_bytes(), new.as_bytes());
        let mut p = common_prefix(ob, nb);
        while p > 0 && !(new.is_char_boundary(p) && old.is_char_boundary(p)) {
            p -= 1;
        }
        let mut s = common_suffix(ob, nb, ob.len().min(nb.len()) - p);
        while s > 0 && !(new.is_char_boundary(nb.len() - s) && old.is_char_boundary(ob.len() - s)) {
            s -= 1;
        }
        (p, s)
    }

    /// Walks root children `range` of the tree, appending their spans.
    fn walk_children(
        root: &typst_syntax::SyntaxNode,
        text: &str,
        range: std::ops::Range<usize>,
        children: &mut Vec<(u32, u32)>,
        spans: &mut Vec<(u32, TypTok)>,
    ) {
        let node = LinkedNode::new(root);
        let mut tmp: Vec<(Range<usize>, TypTok)> = Vec::new();
        for child in node.children().skip(range.start).take(range.len()) {
            tmp.clear();
            walk(&child, TypTok::Text, &mut tmp);
            let before = spans.len();
            let mut covered = 0usize;
            let base = child.offset();
            for (r, tok) in tmp.iter() {
                // Splice-free coverage: the leaves of a node tile it exactly.
                let (a, b) = (r.start, r.end);
                debug_assert_eq!(a - base, covered);
                covered = b - base;
                for piece in text[a..b].split_inclusive('\n') {
                    spans.push((piece.len() as u32, *tok));
                }
            }
            if covered != child.len() {
                // Defensive (not expected): pad so the cached lengths stay exact.
                spans.push(((child.len() - covered) as u32, TypTok::Text));
            }
            children.push((child.len() as u32, (spans.len() - before) as u32));
        }
    }

    fn rebuild(&mut self, new: &str) {
        let source = Source::detached(new);
        let mut children = Vec::new();
        let mut spans = Vec::new();
        let root = source.root();
        let n = root.children().len();
        Self::walk_children(root, source.text(), 0..n, &mut children, &mut spans);
        self.redone = n;
        self.children = children;
        self.spans = spans;
        self.source = Some(source);
    }

    /// Brings the cached tree and spans up to date with `new`.
    pub fn update(&mut self, new: &str) {
        let Some(source) = self.source.as_mut() else {
            self.rebuild(new);
            return;
        };
        let old_len = source.text().len();
        let (p, s) = {
            let (ob, nb) = (source.text().as_bytes(), new.as_bytes());
            let mut p = common_prefix(ob, nb);
            if p == old_len && p == nb.len() {
                self.redone = 0;
                return;
            }
            while p > 0 && !(new.is_char_boundary(p) && source.text().is_char_boundary(p)) {
                p -= 1;
            }
            let mut s = common_suffix(ob, nb, ob.len().min(nb.len()) - p);
            while s > 0 && !(new.is_char_boundary(nb.len() - s) && source.text().is_char_boundary(ob.len() - s)) {
                s -= 1;
            }
            (p, s)
        };
        let n = new.len();
        let reparsed = source.edit(p..old_len - s, &new[p..n - s]);
        let root = source.root();
        if root.diagnosis().either() {
            self.rebuild(new);
            return;
        }
        let kids = root.children().as_slice();
        // Dirty root children: those touching the reparsed range, plus the margin.
        let mut first = kids.len();
        let mut last_excl = kids.len();
        let mut off = 0usize;
        for (i, k) in kids.iter().enumerate() {
            if first == kids.len() && off + k.len() >= reparsed.start {
                first = i;
            }
            if off > reparsed.end {
                last_excl = i;
                break;
            }
            off += k.len();
        }
        let a = first.saturating_sub(MARGIN);
        let b = (last_excl + MARGIN).min(kids.len()).max(a);
        let tail = kids.len() - b;
        // The cached children outside `a..b` must be the ones outside it in the new tree.
        let old_n = self.children.len();
        let consistent = a <= old_n && tail <= old_n - a && {
            let head_bytes: usize = self.children[..a].iter().map(|c| c.0 as usize).sum();
            let tail_bytes: usize = self.children[old_n - tail..].iter().map(|c| c.0 as usize).sum();
            let new_head: usize = kids[..a].iter().map(|k| k.len()).sum();
            let new_tail: usize = kids[b..].iter().map(|k| k.len()).sum();
            head_bytes == new_head && tail_bytes == new_tail
        };
        if !consistent {
            self.rebuild(new);
            return;
        }
        let mut mid_children = Vec::new();
        let mut mid_spans = Vec::new();
        Self::walk_children(root, source.text(), a..b, &mut mid_children, &mut mid_spans);
        let old_b = old_n - tail;
        let sa: usize = self.children[..a].iter().map(|c| c.1 as usize).sum();
        let sb: usize = sa + self.children[a..old_b].iter().map(|c| c.1 as usize).sum::<usize>();
        self.redone = mid_children.len();
        self.incremental += 1;
        self.children.splice(a..old_b, mid_children);
        self.spans.splice(sa..sb, mid_spans);
    }

    /// The spans as [`highlight`] returns them (adjacent equal tokens merged).
    pub fn spans(&self, out: &mut Vec<(Range<usize>, TypTok)>) {
        out.clear();
        let mut at = 0usize;
        for &(len, tok) in &self.spans {
            push(out, at..at + len as usize, tok);
            at += len as usize;
        }
    }
}
