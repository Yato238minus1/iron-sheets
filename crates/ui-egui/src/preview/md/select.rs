//! Text selection in the Markdown preview. The selection is two positions (block, segment, char), not widget
//! state, so it survives blocks scrolling out of the visible range and a drag can run past the viewport.
use super::layout::*;
use egui::epaint::text::Galley;
use egui::{PointerButton, Pos2, Rect, Response, Ui, pos2};
use iron_sheets_md::model::*;
use std::ops::Range;
use std::sync::Arc;

/// `seg` numbers the galleys of a block in reading order (a table cell is `row * cols + col`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub blk: usize,
    pub seg: usize,
    pub ch: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub anchor: Pos,
    pub cursor: Pos,
}

impl Sel {
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.cursor { (self.anchor, self.cursor) } else { (self.cursor, self.anchor) }
    }
}

pub struct Seg<'a> {
    pub galley: &'a Galley,
    pub slots: &'a [Slot],
    /// Top-left of the galley.
    pub pos: Pos2,
    /// Text copied between this segment and the next one.
    pub sep: &'static str,
}

/// The selectable galleys of a block (none for math, images, rules) with the body at (`bx`, `by`).
/// shortcut: tables and code blocks in a horizontal scroll area are hit-tested as if not scrolled.
pub fn segs(l: &BlockLayout, bx: f32, by: f32) -> Vec<Seg<'_>> {
    match &l.body {
        Body::Text { tb, .. } => vec![Seg { galley: &tb.galley, slots: &tb.slots, pos: pos2(bx, by), sep: "\n" }],
        Body::Code { galley, .. } => vec![Seg { galley, slots: &[], pos: pos2(bx + CODE_PAD, by + CODE_PAD), sep: "\n" }],
        Body::Table(t) => {
            let n = t.cols.len();
            let mut v = Vec::with_capacity(t.cells.len());
            let mut y = by;
            for (r, rh) in t.rows_h.iter().enumerate() {
                let mut x = bx;
                for c in 0..n {
                    let tb = &t.cells[r * n + c];
                    let pos = pos2(x + TABLE_PAD_X + cell_dx(t, c, tb), y + TABLE_PAD_Y);
                    v.push(Seg { galley: &tb.galley, slots: &tb.slots, pos, sep: if c + 1 == n { "\n" } else { "\t" } });
                    x += t.cols[c];
                }
                y += rh;
            }
            v
        }
        Body::Diagram(d) => d.texts.iter().map(|(p, tb)| Seg { galley: &tb.galley, slots: &tb.slots, pos: pos2(bx + p.x, by + p.y), sep: "\n" }).collect(),
        // The text of the embed's blocks (nested embeds included); its title and "show more" row are chrome.
        // Mirrors `paint_embed`.
        Body::Embed(e) => {
            let x = bx + EMBED_PAD;
            let y0 = by + EMBED_PAD + e.title.size().y + 4.0;
            e.items.iter().flat_map(|(_, y, il)| segs(il, x + il.indent, y0 + y + il.head_h)).collect()
        }
        _ => vec![],
    }
}

/// The number of segments [`segs`] returns for `l`, without building them.
pub fn seg_count(l: &BlockLayout) -> usize {
    match &l.body {
        Body::Text { .. } | Body::Code { .. } => 1,
        Body::Table(t) => t.cells.len(),
        Body::Diagram(d) => d.texts.len(),
        Body::Embed(e) => e.items.iter().map(|(_, _, il)| seg_count(il)).sum(),
        _ => 0,
    }
}

/// Chars of segment (`blk`, `seg`) inside the ordered selection `lo..hi`.
pub fn seg_range(lo: Pos, hi: Pos, blk: usize, seg: usize) -> Option<Range<usize>> {
    let k = (blk, seg);
    if k < (lo.blk, lo.seg) || k > (hi.blk, hi.seg) {
        return None;
    }
    let a = if k == (lo.blk, lo.seg) { lo.ch } else { 0 };
    let b = if k == (hi.blk, hi.seg) { hi.ch } else { usize::MAX };
    (a < b).then_some(a..b)
}

/// Highlight rectangles (galley space) of the chars `r`, one per row.
pub fn sel_rects<'a>(g: &'a Galley, r: &'a Range<usize>) -> impl Iterator<Item = Rect> + 'a {
    let mut base = 0usize;
    g.rows.iter().filter_map(move |row| {
        let start = base;
        let n = row.char_count_including_newline().0;
        base += n;
        let (a, b) = (r.start.max(start), r.end.min(start + n));
        if a >= b {
            return None;
        }
        let x = |c: usize| row.pos.x + row.glyphs.get(c - start).map_or(row.size.x, |gl| gl.pos.x);
        Some(Rect::from_min_max(pos2(x(a), row.pos.y), pos2(x(b), row.pos.y + row.size.y)))
    })
}

/// The word (or run of spaces or punctuation) around char `ch`, as a char range.
pub fn word_range(text: &str, ch: usize) -> Range<usize> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let n = text.chars().count();
    if n == 0 {
        return 0..0;
    }
    let target = ch.min(n - 1);
    let (mut start, mut end) = (0, n);
    let mut hit = false;
    let mut k_prev = u8::MAX; // class of no character yet
    for (i, c) in text.chars().enumerate() {
        let k = class(c);
        if k != k_prev {
            if hit {
                end = i;
                break;
            }
            start = i;
        }
        k_prev = k;
        hit |= i == target;
    }
    start..end
}

/// Chars `r` of a galley text; a slot (math, image) gives its source text.
pub fn slice_text(text: &str, slots: &[Slot], r: &Range<usize>) -> String {
    let mut out = String::new();
    for (i, c) in text.chars().enumerate().skip(r.start).take(r.end.saturating_sub(r.start)) {
        match slots.iter().find(|s| s.ch == i) {
            Some(s) => out.push_str(&s.text),
            None => out.push(c),
        }
    }
    out
}

fn inl_text(inl: &Inlines) -> String {
    let mut out = String::new();
    let mut objs = inl.objects.iter();
    for (k, part) in inl.text.split(OBJ).enumerate() {
        if let Some(o) = if k > 0 { objs.next() } else { None } {
            out.push_str(&obj_text(&o.kind));
        }
        out.push_str(part);
    }
    out
}

/// Plain text of a whole block, straight from the model (no layout needed).
pub fn block_text(b: &Block) -> String {
    match &b.kind {
        BlockKind::Paragraph(i) | BlockKind::Heading { content: i, .. } => inl_text(i),
        BlockKind::Code { text, .. } | BlockKind::Html { text } | BlockKind::FrontMatter { text } => text.trim_end_matches('\n').to_string(),
        BlockKind::Typst { source } => source.clone(),
        BlockKind::Math { source } => format!("$${source}$$"),
        BlockKind::Table { header, rows, .. } => {
            let row = |r: &Vec<Inlines>| r.iter().map(inl_text).collect::<Vec<_>>().join("\t");
            std::iter::once(header).chain(rows.iter()).map(row).collect::<Vec<_>>().join("\n")
        }
        BlockKind::Rule => "---".into(),
        BlockKind::Image { alt, src, .. } => (if alt.is_empty() { src } else { alt }).clone(),
        BlockKind::Embed { target } => format!("![[{target}]]"),
    }
}

/// The selected text: blocks joined with a blank line. The two end blocks use their layout (partial ranges);
/// without one (evicted from the cache) they contribute whole.
pub fn selected_text(doc: &Document, sel: &Sel, mut layout: impl FnMut(usize) -> Option<Arc<BlockLayout>>) -> String {
    let (lo, hi) = sel.ordered();
    let mut parts: Vec<String> = Vec::new();
    for i in lo.blk..=hi.blk.min(doc.blocks.len().saturating_sub(1)) {
        let Some(b) = doc.blocks.get(i) else { break };
        let l = if i == lo.blk || i == hi.blk || matches!(b.kind, BlockKind::Embed { .. }) { layout(i) } else { None };
        let segs = l.as_ref().map(|l| segs(l, 0.0, 0.0)).unwrap_or_default();
        let t = if segs.is_empty() {
            block_text(b)
        } else {
            let (mut s, mut sep) = (String::new(), "");
            for (k, sg) in segs.iter().enumerate() {
                let Some(r) = seg_range(lo, hi, i, k) else { continue };
                s.push_str(sep);
                s.push_str(&slice_text(sg.galley.text(), sg.slots, &r));
                sep = sg.sep;
            }
            s
        };
        if !t.is_empty() {
            parts.push(t);
        }
    }
    parts.join("\n\n")
}

/// Where the document sits on screen.
pub struct Geo<'a> {
    pub doc: &'a Document,
    pub tops: &'a [f32],
    pub origin: Pos2,
    /// Left edge of the block column.
    pub x0: f32,
}

type Layouts<'a> = &'a mut dyn FnMut(usize) -> Option<Arc<BlockLayout>>;

fn block_at(g: &Geo, y: f32) -> usize {
    g.tops.partition_point(|t| g.origin.y + *t <= y).saturating_sub(1).min(g.doc.blocks.len().saturating_sub(1))
}

fn hit(g: &Geo, layout: Layouts, p: Pos2) -> Option<Pos> {
    if g.doc.blocks.is_empty() {
        return None;
    }
    let i = block_at(g, p.y);
    let top = g.origin.y + *g.tops.get(i)?;
    let l = layout(i)?;
    let segs = segs(&l, g.x0 + l.indent, top + l.head_h);
    let dist = |s: &Seg| {
        let r = Rect::from_min_size(s.pos, s.galley.size());
        let (dx, dy) = ((r.min.x - p.x).max(p.x - r.max.x).max(0.0), (r.min.y - p.y).max(p.y - r.max.y).max(0.0));
        dx * dx + dy * dy
    };
    let Some((k, s)) = segs.iter().enumerate().min_by(|a, b| dist(a.1).total_cmp(&dist(b.1))) else {
        // Math, image, rule: the upper half is "before", the lower half "after" the block.
        return Some(Pos { blk: i, seg: 0, ch: (p.y > top + l.height / 2.0) as usize });
    };
    Some(Pos { blk: i, seg: k, ch: s.galley.cursor_from_pos(p - s.pos).index.0 })
}

/// What the interaction asks the preview to do.
#[derive(Default)]
pub struct Out {
    /// Source line to show (Alt+click).
    pub line: Option<u32>,
    /// Scroll offset to go to (drag past the viewport edge).
    pub scroll: Option<f32>,
    /// Source line of the block under a right click.
    pub ctx_line: Option<u32>,
}

/// Mouse handling on the response covering the viewport.
pub fn handle(ui: &Ui, bg: &Response, sel: &mut Option<Sel>, g: &Geo, layout: Layouts, off: f32) -> Out {
    let mut out = Out::default();
    let (shift, alt, origin) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt, i.pointer.press_origin()));
    let ptr = bg.interact_pointer_pos();
    let line_at = |p: Pos2| g.doc.blocks.get(block_at(g, p.y)).map(|b| b.lines.start);
    if bg.secondary_clicked() {
        out.ctx_line = ptr.and_then(line_at);
    }
    if bg.drag_started_by(PointerButton::Primary) {
        if let Some(p) = origin.and_then(|o| hit(g, layout, o)) {
            match sel {
                Some(s) if shift => s.cursor = p,
                _ => *sel = Some(Sel { anchor: p, cursor: p }),
            }
        }
    } else if bg.dragged_by(PointerButton::Primary) {
        let (Some(p), Some(s)) = (ptr, sel.as_mut()) else { return out };
        let clip = ui.clip_rect();
        if let Some(h) = hit(g, layout, pos2(p.x, p.y.clamp(clip.min.y + 1.0, clip.max.y - 1.0))) {
            s.cursor = h;
        }
        let over = if p.y < clip.min.y { p.y - clip.min.y } else { (p.y - clip.max.y).max(0.0) };
        if over != 0.0 {
            out.scroll = Some((off + over.signum() * (over.abs() * 0.5).clamp(4.0, 40.0)).max(0.0));
            ui.ctx().request_repaint();
        }
    } else if bg.triple_clicked() || bg.double_clicked() {
        let Some(h) = ptr.and_then(|p| hit(g, layout, p)) else { return out };
        let n = |s: &Seg| s.galley.text().chars().count();
        let range = layout(h.blk).and_then(|l| {
            let segs = segs(&l, 0.0, 0.0);
            let s = segs.get(h.seg)?;
            Some(if bg.triple_clicked() { 0..n(s) } else { word_range(s.galley.text(), h.ch) })
        });
        let r = range.unwrap_or(0..1);
        *sel = Some(Sel { anchor: Pos { ch: r.start, ..h }, cursor: Pos { ch: r.end, ..h } });
    } else if bg.clicked() {
        match (ptr, sel.as_mut()) {
            (Some(p), _) if alt => out.line = line_at(p),
            (Some(p), Some(s)) if shift => {
                if let Some(h) = hit(g, layout, p) {
                    s.cursor = h;
                }
            }
            _ => *sel = None,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(blk: usize, seg: usize, ch: usize) -> Pos {
        Pos { blk, seg, ch }
    }

    #[test]
    fn ranges_inside_selection() {
        let (lo, hi) = Sel { anchor: p(3, 0, 5), cursor: p(1, 2, 4) }.ordered();
        assert_eq!((lo, hi), (p(1, 2, 4), p(3, 0, 5)));
        assert_eq!(seg_range(lo, hi, 1, 2), Some(4..usize::MAX));
        assert_eq!(seg_range(lo, hi, 1, 1), None);
        assert_eq!(seg_range(lo, hi, 2, 0), Some(0..usize::MAX));
        assert_eq!(seg_range(lo, hi, 3, 0), Some(0..5));
        assert_eq!(seg_range(lo, hi, 3, 1), None);
        assert_eq!(seg_range(p(2, 0, 3), p(2, 0, 3), 2, 0), None);
    }

    #[test]
    fn words() {
        assert_eq!(word_range("hello big_world!", 7), 6..15);
        assert_eq!(word_range("hello big", 5), 5..6);
        assert_eq!(word_range("ab", 99), 0..2);
        assert_eq!(word_range("", 0), 0..0);
    }

    #[test]
    fn slots_give_their_source() {
        let slot = Slot { ch: 2, w: 1.0, h: 1.0, baseline: 1.0, row_l: 1.0, row_a: 1.0, content: SlotContent::Image("a.png".into()), text: "$x^2$".into() };
        assert_eq!(slice_text("a \u{A0}b", &[slot], &(1..4)), " $x^2$b");
    }

    #[test]
    fn block_text_from_model() {
        let d = iron_sheets_md::parse("# Title\n\nSome $x$ and **bold** ![pic](a.png)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n$$\ny\n$$\n");
        let t: Vec<String> = d.blocks.iter().map(block_text).collect();
        assert_eq!(t[0], "Title");
        assert_eq!(t[1], "Some $x$ and bold pic");
        assert_eq!(t[2], "a\tb\n1\t2");
        assert_eq!(t[3], "$$y$$");
        let all = Sel { anchor: p(0, 0, 0), cursor: p(3, 0, 1) };
        assert_eq!(selected_text(&d, &all, |_| None), t.join("\n\n"));
    }
}
