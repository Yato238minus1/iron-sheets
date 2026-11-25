//! Own per-paragraph galley cache with viewport-first layout.
//!
//! egui lays a job out paragraph by paragraph and caches each by hash, but it evicts every entry that was
//! not used during the last frame, and a whole-job hit costs a job clone plus a hash on every frame. The
//! editor memoizes its galley per document version, so idle frames never call egui, and the paragraph
//! galleys are kept here instead. A keystroke then lays out only the changed paragraph and concatenates
//! the rest (`Galley::concat`, O(rows)).
//!
//! Only the paragraphs near the viewport (and near the caret) are shaped. Every other paragraph is a
//! placeholder galley with the right number of chars per row and an estimated row count (monospace font:
//! `chars * advance / wrap width`), so cursor movement, selection and the scroll range keep working over the
//! whole text. A placeholder is replaced by the real layout as soon as the viewport gets near it; the caller
//! keeps the viewport anchored with `Layout::shift` when those replacements change the height above it.
use egui::emath::{GuiRounding, NumExt};
use egui::epaint::text::{Glyph, PlacedRow, Row, RowVisuals};
use egui::text::{ByteIndex, LayoutJob, LayoutSection};
use egui::{Galley, Rect};
use rustc_hash::FxHashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Source of the sections of a paragraph for big notes: the layout job then holds only the text, and the
/// highlighting of a paragraph is produced when the paragraph is laid out.
pub trait Sections {
    /// Format of unhighlighted text (also used to measure the font).
    fn base_format(&self) -> egui::text::TextFormat;
    /// Sections of the paragraph `start..end` of `text`, byte ranges relative to `start`.
    fn sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>);
    /// The last `sections` call used code colours that were still pending (the paragraph is styled anew when they arrive).
    fn pending(&self) -> bool {
        false
    }
}

/// Real paragraphs farther than this many viewport heights from the view go back to placeholders when idle.
const KEEP_SCREENS: f32 = 5.0;

/// What has to be exact: the viewport (galley y range) and the paragraphs of the caret / selection ends.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub y: f32,
    pub h: f32,
    /// Byte offsets (selection ends): their paragraphs and neighbours are exact.
    pub carets: [Option<usize>; 2],
    /// Byte offset the view is about to jump to (find, link, go to line): a whole viewport around it is exact.
    pub target: Option<usize>,
}

impl View {
    /// Everything exact (tests, small texts).
    pub fn all() -> View {
        View { y: 0.0, h: 1e9, carets: [None; 2], target: None }
    }
}

pub struct Layout {
    pub galley: Arc<Galley>,
    /// How far the content the viewport was anchored on moved (new y - old y). Add it to the scroll offset.
    pub shift: f32,
}

struct Para {
    start: usize,
    end: usize,
    sec: (usize, usize),
    key: u64,
    real: bool,
    /// Styled with pending code colours: the only kind `restyle` has to redo.
    pending: bool,
}

/// Everything needed to swap placeholders for real paragraphs without laying out or hashing again.
struct Prog {
    job: Arc<LayoutJob>,
    paras: Vec<Para>,
    children: Vec<Arc<Galley>>,
    /// Last row, width and row count of every child.
    info: Vec<Info>,
    /// Top of every paragraph in the current galley.
    ys: Vec<f32>,
    missing: usize,
    /// Sections come from a `Sections` source, not from `job`; paragraph keys are computed when a paragraph is laid out.
    big: bool,
    /// The galley made from `children` the last time, and the child index range changed since (inclusive): the next
    /// galley is made by patching that one when the caller let go of it (see `finish`).
    prev: Option<Arc<Galley>>,
    span: Option<(usize, usize)>,
    patches: u32,
    /// Where the idle sweep that demotes far paragraphs (`LineCache::sweep`) continues.
    sweep: usize,
}

#[derive(Clone, Copy)]
struct Info {
    /// Top and height of the child's last row: where the next child starts (see `Info::next`).
    top: f32,
    bot: f32,
    w: f32,
    rows: u32,
}

fn info_of(g: &Galley) -> Info {
    let w = g.rows.iter().map(|r| r.pos.x + r.size.x).fold(0.0, f32::max);
    let (top, bot) = g.rows.last().map_or((0.0, 0.0), |r| (r.pos.y, r.size.y));
    Info { top, bot, w, rows: g.rows.len() as u32 }
}

impl Info {
    /// Where the child after this one starts, given this child's start `y`: `Galley::concat` offsets the next galley by the
    /// bottom of the rows so far, and the last row's top is rounded to pixels on the way. Summing rect heights instead drifts
    /// away from it (a placeholder's height is not a whole number of pixels), and a galley patched with those positions then
    /// has rows that overlap or leave gaps where it meets the rows that were concatenated.
    fn next(&self, y: f32, ppp: f32) -> f32 {
        if self.rows == 0 { y } else { (y + self.top).round_to_pixels(ppp) + self.bot }
    }
}

impl Prog {
    fn mark(&mut self, i: usize) {
        self.span = Some(match self.span {
            Some((a, b)) => (a.min(i), b.max(i)),
            None => (i, i),
        });
    }
}

/// Glyph advance and row height of the editor font, plus a galley to clone placeholders from.
struct Metrics {
    template: Galley,
    row: Row,
    glyph: Glyph,
    adv: f32,
    row_h: f32,
}

#[derive(Default)]
pub struct LineCache {
    /// Paragraph hash -> (galley, generation of the last `layout` call that used it).
    map: FxHashMap<u64, (Arc<Galley>, u32)>,
    ppp: u32,
    generation: u32,
    prog: Option<Prog>,
    metrics: Option<Metrics>,
    /// (chars, trailing newline, wrap bits) -> placeholder paragraph.
    holders: FxHashMap<(u32, bool, u32), Arc<Galley>>,
    /// glyph count -> placeholder row.
    rows: FxHashMap<u32, Arc<Row>>,
    /// Galleys made by patching instead of `Galley::concat` (diagnostics and tests).
    patched: std::cell::Cell<usize>,
    /// Serial of the last galley handed out (from a global counter, so it stays unique when the cache is reset): callers
    /// compare it instead of the galley's address, which the allocator can reuse after the old galley was freed.
    serial: std::cell::Cell<u64>,
    /// (words, chars) change of the text since the previous `layout_big`, when that call edited the previous layout.
    delta: Option<(isize, isize)>,
}

/// Same split rule as epaint's `layout_each_paragraph_individually`; returns the paragraph byte ranges.
fn paragraphs(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= text.len() {
            return None;
        }
        let mut end = text[start..].find('\n').map_or(text.len(), |i| start + i);
        if end == text.len() - 1 && text.ends_with('\n') {
            end += 1;
        }
        let r = (start, end);
        start = end + 1;
        Some(r)
    })
}

/// Cache key of a paragraph of a big note: text, wrap width and sections (as in `LineCache::layout`).
fn big_key(text: &str, wrap: f32, secs: &[LayoutSection]) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    text.hash(&mut h);
    wrap.to_bits().hash(&mut h);
    for s in secs {
        s.leading_space.to_bits().hash(&mut h);
        s.byte_range.start.hash(&mut h);
        s.byte_range.end.hash(&mut h);
        s.format.hash(&mut h);
    }
    h.finish()
}

impl LineCache {
    /// Lays `job` out; paragraphs near `view` are exact, the others placeholders unless cached.
    pub fn layout(&mut self, ui: &egui::Ui, mut job: LayoutJob, view: &View) -> Layout {
        let ppp = ui.pixels_per_point();
        if self.ppp != ppp.to_bits() {
            self.ppp = ppp.to_bits();
            self.map.clear();
            self.prog = None;
            self.reset_metrics();
        }
        if !job.text.contains('\n') || !job.break_on_newline || job.wrap.max_rows != usize::MAX {
            self.prog = None;
            self.bump();
            return Layout { galley: ui.fonts_mut(|f| f.layout_job(job)), shift: 0.0 };
        }
        if job.wrap.max_width.is_finite() {
            job.wrap.max_width = job.wrap.max_width.round();
        }
        let old = self.prog.take();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let mut paras: Vec<Para> = Vec::with_capacity(old.as_ref().map_or(256, |o| o.paras.len()));
        let mut first_sec = 0;
        for (start, end) in paragraphs(&job.text) {
            // Sections overlapping this paragraph: `first_sec..last_sec`.
            while first_sec < job.sections.len() && job.sections[first_sec].byte_range.end <= ByteIndex(start) {
                first_sec += 1;
            }
            let mut last_sec = first_sec;
            while last_sec < job.sections.len() && ByteIndex(end) >= job.sections[last_sec].byte_range.start {
                last_sec += 1;
            }
            let mut h = rustc_hash::FxHasher::default();
            job.text[start..end].hash(&mut h);
            job.wrap.max_width.to_bits().hash(&mut h);
            if start == 0 {
                job.first_row_min_height.to_bits().hash(&mut h);
            }
            for s in &job.sections[first_sec..last_sec] {
                // The rebased section's fields, hashed without cloning the format.
                (if ByteIndex(start) <= s.byte_range.start { s.leading_space } else { 0.0 }).to_bits().hash(&mut h);
                s.byte_range.start.saturating_sub(start).hash(&mut h);
                (s.byte_range.end.min(ByteIndex(end))).saturating_sub(start).hash(&mut h);
                s.format.hash(&mut h);
            }
            let key = h.finish();
            let real = match self.map.get_mut(&key) {
                Some((_, used)) => {
                    *used = generation;
                    true
                }
                None => false,
            };
            paras.push(Para { start, end, sec: (first_sec, last_sec), key, real, pending: false });
        }
        if self.metrics.is_none() {
            self.make_metrics(ui, job.sections.first().map(|s| s.format.clone()).unwrap_or_default());
        }
        let wrap = job.wrap.max_width;
        let mut children: Vec<Arc<Galley>> = Vec::with_capacity(paras.len());
        let mut missing = 0;
        for p in &paras {
            if p.real {
                children.push(self.map[&p.key].0.clone());
            } else {
                missing += 1;
                children.push(self.holder(&job.text[p.start..p.end], wrap));
            }
        }
        let job = Arc::new(job);
        let info = children.iter().map(|g| info_of(g)).collect();
        let mut prog = Prog { job, paras, children, info, ys: Vec::new(), missing, big: false, prev: None, span: None, patches: 0, sweep: 0 };
        self.compute_ys(&mut prog);
        let all = View::all();
        self.fill_window(ui, &mut prog, if self.metrics.is_some() { view } else { &all }, generation, None);
        let galley = Arc::new(Galley::concat(prog.job.clone(), &prog.children, ppp));
        self.bump();
        // Keep the viewport on the same content: find where the paragraph at the old view top went.
        let mut shift = 0.0;
        if let Some(old) = old.filter(|o| o.job.text == prog.job.text && !o.ys.is_empty()) {
            let i = old.ys.partition_point(|&y| y <= view.y).saturating_sub(1);
            if let Ok(n) = prog.paras.binary_search_by_key(&old.paras[i].start, |p| p.start) {
                shift = prog.ys[n] + (view.y - old.ys[i]) - view.y;
            }
        }
        // Drop paragraphs that no current paragraph used (an edit replaces one entry; this keeps the map bounded).
        if self.map.len() > prog.paras.len() + 256 {
            self.map.retain(|_, (_, used)| *used == generation);
        }
        self.prog = Some(prog);
        Layout { galley, shift }
    }

    /// The galley of `prog.children`. `Galley::concat` costs O(rows) (about 4x a plain copy of the rows); when only a few
    /// children changed since the last galley, that galley is patched instead: the rows of the changed span are rebuilt and
    /// the rows below are moved. It is patched in place when the caller has let go of it, else a copy of it is (egui's
    /// `TextEdit` still holds the previous galley while it asks for the new one, so that is the usual case while typing).
    /// `shift_n` is how many children the changed span gained.
    fn finish(&self, prog: &mut Prog, shift_n: isize) -> Arc<Galley> {
        self.bump();
        let ppp = self.ppp_f32();
        fn full(prog: &mut Prog, ppp: f32) -> Arc<Galley> {
            let g = Arc::new(Galley::concat(prog.job.clone(), &prog.children, ppp));
            prog.prev = prog.big.then(|| g.clone());
            prog.span = None;
            prog.patches = 0;
            g
        }
        let (Some((lo, hi)), Some(prev)) = (prog.span.take(), prog.prev.take()) else { return full(prog, ppp) };
        let n = prog.children.len();
        if !prog.big || prog.patches >= 48 || hi >= n || hi as isize - shift_n < lo as isize {
            return full(prog, ppp);
        }
        let mut g = Arc::try_unwrap(prev).unwrap_or_else(|a| (*a).clone());
        let before: usize = prog.info[..lo].iter().map(|i| i.rows as usize).sum();
        let after: usize = prog.info[hi + 1..].iter().map(|i| i.rows as usize).sum();
        let Some(old_span) = g.rows.len().checked_sub(before + after) else { return full(prog, ppp) };
        let mut rows: Vec<PlacedRow> = Vec::with_capacity(prog.info[lo..=hi].iter().map(|i| i.rows as usize).sum());
        let (mut vertices, mut indices) = (0, 0);
        for i in lo..=hi {
            let c = &prog.children[i];
            let y = prog.ys[i];
            let last_child = i + 1 == n;
            for (k, pr) in c.rows.iter().enumerate() {
                let pos = (pr.pos + y * egui::Vec2::Y).round_to_pixels(ppp);
                rows.push(PlacedRow { pos, row: Arc::clone(&pr.row), ends_with_newline: pr.ends_with_newline || (!last_child && k + 1 == c.rows.len()) });
            }
            vertices += c.num_vertices;
            indices += c.num_indices;
        }
        let new_rows = rows.len();
        // Rows below the span move by the change in height.
        let dy = if after > 0 {
            let old_y = g.rows[before + old_span].pos.y;
            let new_y = egui::pos2(0.0, prog.ys[hi + 1]).round_to_pixels(ppp).y;
            new_y - old_y
        } else {
            0.0
        };
        g.rows.splice(before..before + old_span, rows);
        if dy != 0.0 {
            for r in &mut g.rows[before + new_rows..] {
                r.pos = egui::pos2(r.pos.x, r.pos.y + dy).round_to_pixels(ppp);
            }
        }
        let total_h = prog.info[n - 1].next(prog.ys[n - 1], ppp);
        let width = prog.info.iter().map(|i| i.w).fold(0.0, f32::max);
        let mut rect = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(width, total_h));
        let exceeded = rect.width() > prog.job.wrap.max_width + 1.0;
        rect = rect.round_ui();
        if !exceeded {
            rect.max.x = rect.max.x.at_most(rect.min.x + prog.job.wrap.max_width).floor_ui();
        }
        g.rect = rect;
        g.job = prog.job.clone();
        g.elided = false;
        // Only used as bounds and capacity hints: never smaller than the truth.
        g.num_vertices += vertices;
        g.num_indices += indices;
        prog.patches += 1;
        self.patched.set(self.patched.get() + 1);
        let g = Arc::new(g);
        prog.prev = Some(g.clone());
        g
    }

    fn bump(&self) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.serial.set(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    }

    /// The text of the current big layout (what a `WindowHighlighter` has to be given as the previous text).
    pub fn text(&self) -> Option<&str> {
        self.prog.as_ref().filter(|p| p.big).map(|p| p.job.text.as_str())
    }

    /// The word/char count change the last `layout_big` call saw (`None`: it had no previous text to compare with).
    pub fn take_delta(&mut self) -> Option<(isize, isize)> {
        self.delta.take()
    }

    /// Identifies the last galley handed out (changes with every new one).
    pub fn serial(&self) -> u64 {
        self.serial.get()
    }

    /// The galley of the current big layout (to put back after it was let go of for patching).
    pub fn current(&self) -> Option<Arc<Galley>> {
        self.prog.as_ref().and_then(|p| p.prev.clone())
    }

    /// Makes the paragraphs near `view` exact on the current layout. `None` when they already are.
    #[cfg(test)]
    pub fn refocus(&mut self, ui: &egui::Ui, view: &View) -> Option<Layout> {
        self.refocus_with(ui, view, None)
    }

    /// `refocus` for a layout made by `layout_big` (`src` supplies the sections).
    pub fn refocus_with(&mut self, ui: &egui::Ui, view: &View, src: Option<&dyn Sections>) -> Option<Layout> {
        let mut prog = self.prog.take()?;
        if self.paras_to_fill(&prog, view).is_empty() {
            self.prog = Some(prog);
            return None;
        }
        let anchor = Self::anchor(&prog, view);
        let generation = self.generation;
        self.fill_window(ui, &mut prog, view, generation, src);
        let galley = self.finish(&mut prog, 0);
        let shift = Self::shift_of(&prog, view, anchor);
        self.prog = Some(prog);
        Some(Layout { galley, shift })
    }

    /// Placeholders within `screens` viewport heights around `view`: how many are left to lay out.
    pub fn prefetch_pending(&self, view: &View, screens: f32) -> usize {
        self.prog.as_ref().map_or(0, |p| self.prefetch_todo(p, view, screens).len())
    }

    fn prefetch_todo(&self, prog: &Prog, view: &View, screens: f32) -> Vec<usize> {
        let n = prog.paras.len();
        if n == 0 || prog.missing == 0 || self.metrics.is_none() {
            return Vec::new();
        }
        let at = |y: f32| prog.ys.partition_point(|&v| v <= y).saturating_sub(1).min(n - 1);
        let h = view.h.max(1.0);
        let (lo, hi) = (at(view.y - screens * h), at(view.y + (screens + 1.0) * h));
        let mid = at(view.y + 0.5 * h);
        let mut v: Vec<usize> = (lo..=hi).filter(|&i| !prog.paras[i].real).collect();
        // Nearest the viewport first, below before above at equal distance.
        v.sort_by_key(|&i| (i.abs_diff(mid), i < mid));
        v
    }

    /// Idle work: lays out placeholders around `view` (nearest first) until `budget` is used up. Returns the new layout when
    /// something was laid out, and whether placeholders within `screens` viewport heights are still left.
    pub fn prefetch(&mut self, ui: &egui::Ui, view: &View, src: Option<&dyn Sections>, screens: f32, budget: Duration) -> (Option<Layout>, bool) {
        let Some(mut prog) = self.prog.take() else { return (None, false) };
        if prog.big && src.is_none() {
            self.prog = Some(prog);
            return (None, false);
        }
        let todo = self.prefetch_todo(&prog, view, screens);
        let t0 = Instant::now();
        // The paragraph at the view top and its y before anything moves.
        let anchor = Self::anchor(&prog, view);
        let generation = self.generation;
        let mut done = 0;
        for &i in &todo {
            self.fill_one(ui, &mut prog, i, generation, src);
            done += 1;
            if t0.elapsed() > budget {
                break;
            }
        }
        // Nothing left to lay out: free what is far away.
        let (demoted, more_sweep) = if done == todo.len() && prog.big { self.sweep(&mut prog, view, t0, budget) } else { (false, false) };
        if done == 0 && !demoted {
            self.prog = Some(prog);
            return (None, false);
        }
        self.compute_ys(&mut prog);
        let galley = self.finish(&mut prog, 0);
        let shift = Self::shift_of(&prog, view, anchor);
        self.prog = Some(prog);
        (Some(Layout { galley, shift }), done < todo.len() || more_sweep)
    }

    /// Index and y of the paragraph at the top of `view`, before the layout changes.
    fn anchor(prog: &Prog, view: &View) -> (usize, f32) {
        let i = prog.ys.partition_point(|&y| y <= view.y).saturating_sub(1);
        (i, prog.ys.get(i).copied().unwrap_or(0.0))
    }

    /// How far the content at the old view top moved.
    fn shift_of(prog: &Prog, view: &View, (i, y): (usize, f32)) -> f32 {
        prog.ys.get(i).map_or(0.0, |&ny| ny + (view.y - y) - view.y)
    }

    /// Paragraph index range that stays exact around `view`: `KEEP_SCREENS` viewport heights each way, and the caret paragraphs.
    fn kept(prog: &Prog, view: &View) -> (usize, usize, [Option<usize>; 2]) {
        let n = prog.paras.len();
        let at = |y: f32| prog.ys.partition_point(|&v| v <= y).saturating_sub(1).min(n.saturating_sub(1));
        let h = view.h.max(1.0);
        let para_at = |b: usize| prog.paras.partition_point(|p| p.start <= b).saturating_sub(1);
        let carets = view.carets.map(|c| c.or(view.target).map(para_at));
        (at(view.y - KEEP_SCREENS * h), at(view.y + (KEEP_SCREENS + 1.0) * h), carets)
    }

    fn is_kept(&(a, b, carets): &(usize, usize, [Option<usize>; 2]), i: usize) -> bool {
        (a..=b).contains(&i) || carets.iter().flatten().any(|&c| i.abs_diff(c) <= 2)
    }

    /// Turns real paragraphs far from `view` back into placeholders, so that scrolling through a big note does not keep every
    /// paragraph laid out in memory. Continues where the last call stopped; returns (anything demoted, more to look at).
    fn sweep(&mut self, prog: &mut Prog, view: &View, t0: Instant, budget: Duration) -> (bool, bool) {
        let n = prog.paras.len();
        let keep = Self::kept(prog, view);
        // Cheap exit: not much more laid out than the part that stays.
        if n - prog.missing <= keep.1.saturating_sub(keep.0) + 512 {
            prog.sweep = 0;
            return (false, false);
        }
        let mut i = prog.sweep.min(n);
        let mut any = false;
        while i < n {
            if prog.paras[i].real && !Self::is_kept(&keep, i) {
                self.demote(prog, i);
                any = true;
            }
            i += 1;
            if i.is_multiple_of(256) && t0.elapsed() > budget {
                break;
            }
        }
        prog.sweep = if i >= n { 0 } else { i };
        (any, i < n)
    }

    /// Paragraph `i` becomes an estimated placeholder again and its galley is let go of.
    fn demote(&mut self, prog: &mut Prog, i: usize) {
        let (start, end) = (prog.paras[i].start, prog.paras[i].end);
        self.map.remove(&prog.paras[i].key);
        let h = self.holder(&prog.job.text[start..end], prog.job.wrap.max_width);
        prog.info[i] = info_of(&h);
        prog.children[i] = h;
        prog.paras[i].real = false;
        prog.missing += 1;
        prog.mark(i);
    }

    /// Makes every exact paragraph that was styled with pending code colours a placeholder again, so the next `refocus`
    /// styles the ones in view anew (the colours of code blocks arrived).
    pub fn restyle(&mut self) {
        let Some(mut prog) = self.prog.take() else { return };
        for i in 0..prog.paras.len() {
            if prog.paras[i].real && prog.paras[i].pending {
                self.demote(&mut prog, i);
            }
        }
        self.compute_ys(&mut prog);
        self.prog = Some(prog);
    }

    /// True when a paragraph near `view` is still a placeholder.
    pub fn needs_focus(&self, view: &View) -> bool {
        self.prog.as_ref().is_some_and(|p| !self.paras_to_fill(p, view).is_empty())
    }

    pub fn patched(&self) -> usize {
        self.patched.get()
    }

    pub fn placeholders(&self) -> usize {
        self.prog.as_ref().map_or(0, |p| p.missing)
    }

    fn ppp_f32(&self) -> f32 {
        f32::from_bits(self.ppp)
    }

    fn reset_metrics(&mut self) {
        self.metrics = None;
        self.holders.clear();
        self.rows.clear();
        self.prog = None;
    }

    fn make_metrics(&mut self, ui: &egui::Ui, fmt: egui::text::TextFormat) {
        let g = ui.fonts_mut(|f| f.layout_job(LayoutJob::single_section("m".repeat(16), fmt)));
        let Some(pr) = g.rows.first() else { return };
        let row = (*pr.row).clone();
        let Some(glyph) = row.glyphs.first().copied() else { return };
        let adv = if row.glyphs.len() > 1 {
            (row.glyphs[row.glyphs.len() - 1].pos.x - row.glyphs[0].pos.x) / (row.glyphs.len() - 1) as f32
        } else {
            glyph.advance_width
        };
        let row_h = row.size.y.max(1.0);
        self.metrics = Some(Metrics { template: (*g).clone(), row, glyph, adv: adv.max(0.5), row_h });
    }

    /// Placeholder galley for a paragraph with the same char count as `text` (a trailing `\n` is the last paragraph's).
    fn holder(&mut self, text: &str, wrap: f32) -> Arc<Galley> {
        let trailing = text.ends_with('\n');
        let n = text.chars().count() - trailing as usize;
        let key = (n as u32, trailing, wrap.to_bits());
        if let Some(g) = self.holders.get(&key) {
            return g.clone();
        }
        let Some(m) = &self.metrics else { return self.dummy() };
        let cols = if wrap.is_finite() { ((wrap / m.adv).floor() as usize).max(1) } else { usize::MAX };
        let row_h = m.row_h;
        let mut lens: Vec<u32> = Vec::new();
        let mut left = n;
        while left > cols {
            lens.push(cols as u32);
            left -= cols;
        }
        lens.push(left as u32);
        let mut rows: Vec<PlacedRow> = Vec::with_capacity(lens.len() + trailing as usize);
        let mut width = 0.0f32;
        for (i, &k) in lens.iter().enumerate() {
            let Some(row) = self.row_of(k) else { return self.dummy() };
            width = width.max(row.size.x);
            rows.push(PlacedRow { pos: egui::pos2(0.0, i as f32 * row_h), row, ends_with_newline: trailing && i + 1 == lens.len() });
        }
        if trailing {
            let Some(row) = self.row_of(0) else { return self.dummy() };
            rows.push(PlacedRow { pos: egui::pos2(0.0, rows.len() as f32 * row_h), row, ends_with_newline: false });
        }
        let Some(m) = &self.metrics else { return self.dummy() };
        let mut g = m.template.clone();
        g.rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, rows.len() as f32 * row_h));
        g.rows = rows;
        g.mesh_bounds = Rect::NOTHING;
        g.num_vertices = 0;
        g.num_indices = 0;
        let g = Arc::new(g);
        self.holders.insert(key, g.clone());
        g
    }

    /// Empty galley used only when the font metrics could not be measured (then everything is laid out exactly).
    fn dummy(&self) -> Arc<Galley> {
        Arc::new(Galley::concat(Arc::new(LayoutJob::default()), &[], self.ppp_f32()))
    }

    fn row_of(&mut self, k: u32) -> Option<Arc<Row>> {
        if let Some(r) = self.rows.get(&k) {
            return Some(r.clone());
        }
        let m = self.metrics.as_ref()?;
        let mut row = m.row.clone();
        row.glyphs = (0..k)
            .map(|i| {
                let mut g = m.glyph;
                g.pos = egui::pos2(i as f32 * m.adv, m.glyph.pos.y);
                g.advance_width = m.adv;
                g
            })
            .collect();
        // Already rounded: `Galley::concat` would otherwise deep-clone every shared row to round it.
        row.size = egui::vec2(k as f32 * m.adv, m.row_h).round_ui();
        row.visuals = RowVisuals::default();
        let r = Arc::new(row);
        self.rows.insert(k, r.clone());
        Some(r)
    }

    /// Brings `prog.ys` (top of every paragraph) up to date. After an edit only the paragraphs from the first changed
    /// one are summed again, up to the point where the positions equal the old ones (typing usually changes no height).
    fn compute_ys(&self, prog: &mut Prog) {
        let n = prog.info.len();
        let (lo, hi) = match prog.span {
            Some(span) if prog.ys.len() == n => span,
            None if prog.ys.len() == n => return,
            _ => {
                prog.ys.clear();
                prog.ys.resize(n, 0.0);
                (0, n)
            }
        };
        let ppp = self.ppp_f32();
        let mut y = if lo == 0 { 0.0 } else { prog.info[lo - 1].next(prog.ys[lo - 1], ppp) };
        for j in lo..n {
            if j > hi && prog.ys[j] == y {
                break;
            }
            prog.ys[j] = y;
            y = prog.info[j].next(y, ppp);
        }
    }

    /// Paragraph indices (sorted, unique) that must be exact for `view` and are placeholders now.
    fn paras_to_fill(&self, prog: &Prog, view: &View) -> Vec<usize> {
        let n = prog.paras.len();
        if n == 0 {
            return Vec::new();
        }
        let at = |y: f32| prog.ys.partition_point(|&v| v <= y).saturating_sub(1).min(n - 1);
        let h = view.h.max(1.0);
        let para_at = |b: usize| prog.paras.partition_point(|p| p.start <= b).saturating_sub(1).min(n - 1);
        let mut ranges: Vec<(usize, usize)> = vec![(at(view.y - 0.3 * h), at(view.y + 1.0 * h))];
        for c in view.carets.into_iter().flatten() {
            let i = para_at(c);
            ranges.push((i.saturating_sub(2), (i + 2).min(n - 1)));
        }
        if let Some(t) = view.target {
            let i = para_at(t);
            ranges.push((at(prog.ys[i] - 0.5 * h), at(prog.ys[i] + 0.5 * h).max(i)));
        }
        let mut out = Vec::new();
        for (a, b) in ranges {
            for i in a..=b {
                if !prog.paras[i].real {
                    out.push(i);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Lays out the paragraphs `paras_to_fill` names. The y positions move while this runs, so it repeats
    /// (bounded) until the window is exact.
    fn fill_window(&mut self, ui: &egui::Ui, prog: &mut Prog, view: &View, generation: u32, src: Option<&dyn Sections>) {
        if prog.big && src.is_none() {
            return;
        }
        for _ in 0..4 {
            let todo = self.paras_to_fill(prog, view);
            if todo.is_empty() {
                break;
            }
            for i in todo {
                self.fill_one(ui, prog, i, generation, src);
            }
            self.compute_ys(prog);
        }
    }

    fn fill_one(&mut self, ui: &egui::Ui, prog: &mut Prog, i: usize, generation: u32, src: Option<&dyn Sections>) {
        let g = match src.filter(|_| prog.big) {
            Some(src) => {
                let mut secs = Vec::new();
                let (start, end) = (prog.paras[i].start, prog.paras[i].end);
                src.sections(&prog.job.text, start, end, &mut secs);
                prog.paras[i].pending = src.pending();
                if secs.is_empty() {
                    secs.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(0), format: src.base_format() });
                }
                let key = big_key(&prog.job.text[start..end], prog.job.wrap.max_width, &secs);
                prog.paras[i].key = key;
                match self.map.get_mut(&key) {
                    Some((g, used)) => {
                        *used = generation;
                        g.clone()
                    }
                    None => {
                        let pj = LayoutJob {
                            text: prog.job.text[start..end].to_owned(),
                            wrap: prog.job.wrap.clone(),
                            sections: secs,
                            break_on_newline: prog.job.break_on_newline,
                            halign: prog.job.halign,
                            justify: prog.job.justify,
                            first_row_min_height: 0.0,
                            round_output_to_gui: prog.job.round_output_to_gui,
                            keep_trailing_whitespace: prog.job.keep_trailing_whitespace,
                        };
                        let g = ui.fonts_mut(|f| f.layout_job(pj));
                        self.map.insert(key, (g.clone(), generation));
                        g
                    }
                }
            }
            None => {
                let g = self.lay_para(ui, &prog.job, &prog.paras[i]);
                self.map.insert(prog.paras[i].key, (g.clone(), generation));
                g
            }
        };
        prog.info[i] = info_of(&g);
        prog.children[i] = g;
        prog.mark(i);
        prog.paras[i].real = true;
        prog.missing -= 1;
    }

    /// Lays a big note out (see `Sections`): the job is the text alone, paragraph keys and sections are made
    /// per paragraph when it is laid out. `dirty` is the byte range whose highlighting may have changed since the
    /// last call; the previous layout is reused (edited in place) for everything outside the edit.
    pub fn layout_big(&mut self, ui: &egui::Ui, text: &str, wrap: f32, src: &dyn Sections, dirty: Range<usize>, view: &View) -> Layout {
        let ppp = ui.pixels_per_point();
        if self.ppp != ppp.to_bits() {
            self.ppp = ppp.to_bits();
            self.map.clear();
            self.prog = None;
            self.reset_metrics();
        }
        let wrap = if wrap.is_finite() { wrap.round() } else { wrap };
        let old = self.prog.take();
        self.delta = None;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let n = text.len();
        let base = src.base_format();
        let fresh_job = || {
            let mut job = LayoutJob::default();
            job.wrap.max_width = wrap;
            job.text = text.to_owned();
            job.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(n), format: base.clone() });
            Arc::new(job)
        };
        if self.metrics.is_none() {
            self.make_metrics(ui, base.clone());
        }
        // Where the viewport is anchored in the old layout: paragraph index and its y.
        let anchor = old.as_ref().filter(|o| !o.ys.is_empty()).map(|o| {
            let i = o.ys.partition_point(|&y| y <= view.y).saturating_sub(1);
            (i, o.ys[i], o.paras[i].start)
        });
        let reusable = old.as_ref().is_some_and(|o| o.big && o.job.wrap.max_width.to_bits() == wrap.to_bits() && n > 0 && !o.paras.is_empty());
        let same_text = !reusable && old.as_ref().is_some_and(|o| o.job.text == text);
        // (old paragraph index range replaced, number of new paragraphs) when the old layout was reused.
        let mut replaced: Option<(usize, usize, usize)> = None;
        let mut shift_n = 0isize;
        let mut prog;
        let job: Arc<LayoutJob>;
        match old {
            Some(mut o) if reusable => {
                let old_len = o.job.text.len();
                let (p, s) = iron_sheets_md::diff_texts(&o.job.text, text);
                self.delta = Some(iron_sheets_app_core::docs::count_delta(&o.job.text, text, p, s));
                job = fresh_job();
                let delta = n as isize - old_len as isize;
                let old_end = old_len - s;
                let np = o.paras.len();
                let mut i0 = o.paras.partition_point(|q| q.start <= p).saturating_sub(1);
                let i1 = o.paras.partition_point(|q| q.start <= old_end).saturating_sub(1).max(i0);
                let to_end = i1 + 1 >= np;
                if to_end {
                    // The last paragraph owns the final line break, so the one before it can change too.
                    i0 = i0.saturating_sub(1);
                }
                let s0 = o.paras[i0].start;
                let e1 = if to_end { n } else { (o.paras[i1].end as isize + delta) as usize };
                let mut fresh: Vec<Para> = Vec::new();
                if to_end {
                    for (a, b) in paragraphs(&text[s0..e1]) {
                        fresh.push(Para { start: s0 + a, end: s0 + b, sec: (0, 0), key: 0, real: false, pending: false });
                    }
                } else {
                    // `e1` is the position of the line break that ends the last paragraph: the text before it is split
                    // at every line break and its tail (possibly empty) is a paragraph too.
                    let slice = &text[s0..e1];
                    let mut a = 0;
                    loop {
                        match slice[a..].find('\n') {
                            Some(i) => {
                                fresh.push(Para { start: s0 + a, end: s0 + a + i, sec: (0, 0), key: 0, real: false, pending: false });
                                a += i + 1;
                            }
                            None => {
                                fresh.push(Para { start: s0 + a, end: e1, sec: (0, 0), key: 0, real: false, pending: false });
                                break;
                            }
                        }
                    }
                }
                let fresh_n = fresh.len();
                let holders: Vec<Arc<Galley>> = fresh.iter().map(|q| self.holder(&text[q.start..q.end], wrap)).collect();
                let infos: Vec<Info> = holders.iter().map(|g| info_of(g)).collect();
                let removed_missing = o.paras[i0..=i1].iter().filter(|q| !q.real).count();
                if o.ys.len() == np {
                    o.ys.splice(i0..=i1, std::iter::repeat_n(0.0, fresh_n));
                }
                o.paras.splice(i0..=i1, fresh);
                o.children.splice(i0..=i1, holders);
                o.info.splice(i0..=i1, infos);
                o.missing = o.missing - removed_missing + fresh_n;
                if delta != 0 {
                    for q in &mut o.paras[i0 + fresh_n..] {
                        q.start = (q.start as isize + delta) as usize;
                        q.end = (q.end as isize + delta) as usize;
                    }
                }
                o.job = job.clone();
                o.span = None;
                o.mark(i0);
                o.mark(i0 + fresh_n - 1);
                replaced = Some((i0, i1, fresh_n));
                shift_n = fresh_n as isize - (i1 + 1 - i0) as isize;
                prog = o;
            }
            _ => {
                let mut paras: Vec<Para> = Vec::with_capacity(n / 40 + 8);
                let mut children = Vec::with_capacity(paras.capacity());
                for (start, end) in paragraphs(text) {
                    children.push(self.holder(&text[start..end], wrap));
                    paras.push(Para { start, end, sec: (0, 0), key: 0, real: false, pending: false });
                }
                let missing = paras.len();
                let info = children.iter().map(|g| info_of(g)).collect();
                job = fresh_job();
                prog = Prog { job: job.clone(), paras, children, info, ys: Vec::new(), missing, big: true, prev: None, span: None, patches: 0, sweep: 0 };
            }
        }
        self.compute_ys(&mut prog);
        // Paragraphs outside the edit whose highlighting changed (a fence opened above them, ...) are placeholders again;
        // the far ones without looking (they are laid out when they come near), so a change that reaches to the end of the
        // note costs per paragraph that is still real, not per paragraph.
        if let (false, Some((r0, _, fresh_n))) = (dirty.is_empty(), replaced) {
            let keep = Self::kept(&prog, view);
            let j0 = prog.paras.partition_point(|q| q.end < dirty.start);
            let mut secs = Vec::new();
            let mut changed = false;
            for j in j0..prog.paras.len() {
                if prog.paras[j].start >= dirty.end {
                    break;
                }
                if (j >= r0 && j < r0 + fresh_n) || !prog.paras[j].real {
                    continue;
                }
                let (start, end) = (prog.paras[j].start, prog.paras[j].end);
                let stale = !Self::is_kept(&keep, j) || {
                    secs.clear();
                    src.sections(text, start, end, &mut secs);
                    if secs.is_empty() {
                        secs.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(0), format: src.base_format() });
                    }
                    big_key(&text[start..end], wrap, &secs) != prog.paras[j].key
                };
                if stale {
                    self.demote(&mut prog, j);
                    changed = true;
                }
            }
            if changed {
                self.compute_ys(&mut prog);
            }
        }
        let all = View::all();
        self.fill_window(ui, &mut prog, if self.metrics.is_some() { view } else { &all }, generation, Some(src));
        let galley = self.finish(&mut prog, shift_n);
        // Keep the viewport on the same content: where did the paragraph at the old view top go?
        let mut shift = 0.0;
        if let Some((i, y, start)) = anchor {
            let new_i = match replaced {
                Some((i0, i1, fresh_n)) => {
                    if i < i0 {
                        Some(i)
                    } else if i > i1 {
                        Some(i + fresh_n - (i1 + 1 - i0))
                    } else {
                        None
                    }
                }
                None if same_text => prog.paras.binary_search_by_key(&start, |q| q.start).ok(),
                None => None,
            };
            if let Some(ni) = new_i.filter(|&ni| ni < prog.ys.len()) {
                shift = prog.ys[ni] + (view.y - y) - view.y;
            }
        }
        // Keep the paragraph map bounded: only what the current layout uses survives.
        let live = prog.paras.len() - prog.missing;
        if self.map.len() > live + 1024 {
            let keep: rustc_hash::FxHashSet<u64> = prog.paras.iter().filter(|q| q.real).map(|q| q.key).collect();
            self.map.retain(|k, _| keep.contains(k));
        }
        self.prog = Some(prog);
        Layout { galley, shift }
    }

    fn lay_para(&self, ui: &egui::Ui, job: &LayoutJob, p: &Para) -> Arc<Galley> {
        let (start, end) = (p.start, p.end);
        let rebased = |s: &LayoutSection| LayoutSection {
            leading_space: if ByteIndex(start) <= s.byte_range.start { s.leading_space } else { 0.0 },
            byte_range: s.byte_range.start.saturating_sub(start)..(s.byte_range.end.min(ByteIndex(end))).saturating_sub(start),
            format: s.format.clone(),
        };
        let pj = LayoutJob {
            text: job.text[start..end].to_owned(),
            wrap: job.wrap.clone(),
            sections: job.sections[p.sec.0..p.sec.1].iter().map(rebased).collect(),
            break_on_newline: job.break_on_newline,
            halign: job.halign,
            justify: job.justify,
            first_row_min_height: if start == 0 { job.first_row_min_height } else { 0.0 },
            round_output_to_gui: job.round_output_to_gui,
            keep_trailing_whitespace: job.keep_trailing_whitespace,
        };
        ui.fonts_mut(|f| f.layout_job(pj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::text::TextFormat;
    use egui::{Color32, FontId};

    fn job(text: &str) -> LayoutJob {
        let mut j = LayoutJob::default();
        j.wrap.max_width = 120.0;
        let a = TextFormat::simple(FontId::monospace(13.0), Color32::WHITE);
        let b = TextFormat::simple(FontId::monospace(13.0), Color32::RED);
        for (i, piece) in text.split_inclusive('\n').enumerate() {
            j.append(piece, 0.0, if i % 2 == 0 { a.clone() } else { b.clone() });
        }
        if text.is_empty() {
            j.append("", 0.0, a);
        }
        j
    }

    #[test]
    fn matches_egui_layout() {
        let ctx = egui::Context::default();
        let mut cache = LineCache::default();
        let texts = ["", "one", "a\n", "a\n\n", "\n", "\n\nx", "a\nb\nc", "a long line that wraps around the width\nshort\n\nend\n", "x\n\n\n"];
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            for round in 0..2 {
                for t in texts {
                    let mine = cache.layout(ui, job(t), &View::all()).galley;
                    let theirs = ui.fonts_mut(|f| f.layout_job(job(t)));
                    assert_eq!(mine.rows.len(), theirs.rows.len(), "{t:?} round {round}");
                    for (a, b) in mine.rows.iter().zip(&theirs.rows) {
                        assert_eq!(a.pos, b.pos, "{t:?}");
                        assert_eq!(a.ends_with_newline, b.ends_with_newline, "{t:?}");
                    }
                    assert_eq!(mine.rect, theirs.rect, "{t:?}");
                    assert_eq!(mine.text(), theirs.text());
                }
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn serial_changes_with_every_galley() {
        run(|ui| {
            let mut cache = LineCache::default();
            let mut seen = vec![cache.serial()];
            for t in ["a\nb\n", "a\nb\n", "a\nc\n"] {
                cache.layout(ui, job(t), &View::all());
                seen.push(cache.serial());
            }
            seen.dedup();
            assert_eq!(seen.len(), 4, "each layout gets its own serial");
        });
    }

    fn run<R>(f: impl FnOnce(&egui::Ui) -> R) -> R {
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))), ..Default::default() };
        let mut f = Some(f);
        let mut res = None;
        let mut out = ctx.run_ui(input, |ui| {
            if let Some(f) = f.take() {
                res = Some(f(ui));
            }
        });
        out.textures_delta.clear();
        res.expect("ran")
    }

    struct Plain;
    impl Sections for Plain {
        fn base_format(&self) -> TextFormat {
            TextFormat::simple(FontId::monospace(13.0), Color32::WHITE)
        }
        fn sections(&self, _text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
            out.clear();
            out.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(end - start), format: self.base_format() });
        }
    }

    /// A source whose paragraphs are all styled with pending colours.
    struct Marked;
    impl Sections for Marked {
        fn base_format(&self) -> TextFormat {
            Plain.base_format()
        }
        fn sections(&self, t: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
            Plain.sections(t, start, end, out);
        }
        fn pending(&self) -> bool {
            true
        }
    }

    #[test]
    fn restyle_redoes_only_paragraphs_styled_with_pending_colours() {
        run(|ui| {
            let text = "plain a\n\nplain b\n";
            let view = View::all();
            // Everything styled by a source that says "not pending": restyle leaves it exact.
            let mut cache = LineCache::default();
            cache.layout_big(ui, text, 300.0, &Plain, 0..0, &view);
            cache.refocus_with(ui, &view, Some(&Plain));
            assert_eq!(cache.placeholders(), 0);
            cache.restyle();
            assert_eq!(cache.placeholders(), 0, "nothing was pending");
            // Styled while pending: restyle makes them placeholders again.
            let mut cache = LineCache::default();
            cache.layout_big(ui, text, 300.0, &Marked, 0..0, &view);
            cache.refocus_with(ui, &view, Some(&Marked));
            assert_eq!(cache.placeholders(), 0);
            cache.restyle();
            assert!(cache.placeholders() > 0, "pending paragraphs are styled anew");
        });
    }

    #[test]
    fn prefetch_lays_out_a_window_around_the_view_and_then_has_nothing_left() {
        run(|ui| {
            let text = doc(4000);
            let mut cache = LineCache::default();
            let view = View { y: 5000.0, h: 400.0, carets: [None; 2], target: None };
            let mut l = cache.layout_big(ui, &text, 300.0, &Plain, 0..0, &view);
            let p0 = cache.placeholders();
            assert!(p0 > 3000, "{p0}");
            let mut rounds = 0;
            loop {
                let (n, more) = cache.prefetch(ui, &view, Some(&Plain), 3.0, Duration::from_secs(5));
                if let Some(n) = n {
                    l = n;
                }
                rounds += 1;
                if !more || rounds > 20 {
                    break;
                }
            }
            let p1 = cache.placeholders();
            assert!(p1 < p0 && p1 > 3000, "window laid out, the rest still estimated: {p0} -> {p1}");
            assert_eq!(cache.prefetch_pending(&view, 3.0), 0);
            let (n, more) = cache.prefetch(ui, &view, Some(&Plain), 3.0, Duration::from_secs(5));
            assert!(n.is_none() && !more, "nothing left to do");
            drop(l);
        });
    }

    fn doc(n: usize) -> String {
        let mut t = String::new();
        for i in 0..n {
            match i % 4 {
                0 => t.push_str(&format!("## Heading {i}\n")),
                1 => t.push_str(&format!("A long paragraph number {i} that has to wrap around the width of the editor a few times, with words.\n")),
                2 => t.push('\n'),
                _ => t.push_str(&format!("- item {i}\n")),
            }
        }
        t
    }

    fn chars_of(g: &Galley) -> usize {
        g.rows.iter().map(|r| r.char_count_including_newline().0).sum()
    }

    #[test]
    fn placeholders_keep_chars_and_newlines() {
        run(|ui| {
            for text in [doc(400), format!("{}tail without newline", doc(400)), format!("{}\n\n", doc(40))] {
                let mut cache = LineCache::default();
                let view = View { y: 0.0, h: 200.0, carets: [None; 2], target: None };
                let l = cache.layout(ui, job(&text), &view);
                let exact = LineCache::default().layout(ui, job(&text), &View::all()).galley;
                if text.len() > 5000 {
                    assert!(cache.placeholders() > 100, "most paragraphs are placeholders");
                }
                assert_eq!(chars_of(&l.galley), text.chars().count());
                assert_eq!(l.galley.text(), text);
                let nl = |g: &Galley| g.rows.iter().filter(|r| r.ends_with_newline).count();
                assert_eq!(nl(&l.galley), nl(&exact));
                assert!(!l.galley.rows.is_empty());
                // The first screen is exact.
                for (a, b) in l.galley.rows.iter().zip(&exact.rows).take(10) {
                    assert_eq!(a.pos, b.pos);
                    assert_eq!(a.row.glyphs.len(), b.row.glyphs.len());
                }
                // y is monotonic.
                assert!(l.galley.rows.windows(2).all(|w| w[1].pos.y >= w[0].pos.y));
            }
        });
    }

    #[test]
    fn no_wrap_placeholders_have_exact_height() {
        run(|ui| {
            let text = doc(400);
            let mut j = job(&text);
            j.wrap.max_width = f32::INFINITY;
            let mut cache = LineCache::default();
            let l = cache.layout(ui, j.clone(), &View { y: 0.0, h: 100.0, carets: [None; 2], target: None });
            let exact = LineCache::default().layout(ui, j, &View::all()).galley;
            assert!(cache.placeholders() > 100);
            assert_eq!(l.galley.rows.len(), exact.rows.len());
            assert!((l.galley.rect.height() - exact.rect.height()).abs() < 1.0, "{} vs {}", l.galley.rect.height(), exact.rect.height());
        });
    }

    #[test]
    fn refocus_lays_out_the_new_view_and_keeps_anchor() {
        run(|ui| {
            let text = doc(2000);
            let mut cache = LineCache::default();
            let top = View { y: 0.0, h: 300.0, carets: [None; 2], target: None };
            let l0 = cache.layout(ui, job(&text), &top);
            let p0 = cache.placeholders();
            assert!(cache.refocus(ui, &top).is_none(), "nothing to do at the same view");
            let y = l0.galley.rect.height() * 0.6;
            let far = View { y, h: 300.0, carets: [None; 2], target: None };
            assert!(cache.needs_focus(&far));
            let l1 = cache.refocus(ui, &far).expect("layout near the new view");
            assert!(cache.placeholders() < p0);
            assert!(!cache.needs_focus(&far));
            assert_eq!(chars_of(&l1.galley), text.chars().count());
            // The rows in view now equal the exact layout's rows (same text, so same glyph counts).
            let exact = LineCache::default().layout(ui, job(&text), &View::all()).galley;
            // Anchor: the paragraph that was at `y` is at `y + shift` now; walking the rows from the top by chars finds the same row.
            let mut pos = 0usize;
            let mut found = None;
            for r in &l1.galley.rows {
                if r.pos.y >= y + l1.shift - 0.5 && found.is_none() {
                    found = Some(pos);
                }
                pos += r.char_count_including_newline().0;
            }
            assert!(found.is_some());
            // Paragraph-level equality of the exact window against the full layout.
            let rows_exact: Vec<usize> = exact.rows.iter().map(|r| r.row.glyphs.len()).collect();
            let start = l1.galley.rows.partition_point(|r| r.pos.y < y + l1.shift);
            let want: Vec<usize> = l1.galley.rows[start..start + 5].iter().map(|r| r.row.glyphs.len()).collect();
            assert!(rows_exact.windows(5).any(|w| w == want.as_slice()), "rows near the view are real rows");
        });
    }

    #[test]
    fn carets_and_targets_are_exact() {
        run(|ui| {
            let text = doc(2000);
            let mut cache = LineCache::default();
            let end = text.len();
            let v = View { y: 0.0, h: 200.0, carets: [Some(end), Some(end)], target: None };
            cache.layout(ui, job(&text), &v);
            assert!(!cache.needs_focus(&v), "the paragraphs at the caret are exact");
            let mid = text.len() / 2;
            let v2 = View { y: 0.0, h: 200.0, carets: [None; 2], target: Some(mid) };
            assert!(cache.needs_focus(&v2));
            cache.refocus(ui, &v2).expect("target laid out");
            assert!(!cache.needs_focus(&v2));
        });
    }

    #[test]
    fn rewrap_keeps_the_anchor() {
        run(|ui| {
            let text = doc(2000);
            let mut cache = LineCache::default();
            let mut j = job(&text);
            j.wrap.max_width = 300.0;
            let g = cache.layout(ui, j, &View::all()).galley;
            let y = g.rect.height() * 0.5;
            let mut j2 = job(&text);
            j2.wrap.max_width = 200.0;
            let l = cache.layout(ui, j2, &View { y, h: 300.0, carets: [None; 2], target: None });
            // A narrower pane wraps to more rows, so what was at `y` is lower now.
            assert!(l.shift > 0.0, "shift {}", l.shift);
        });
    }

    /// Idle layout of a whole note, then back to the top: the paragraphs far from the view go back to placeholders.
    #[test]
    fn far_paragraphs_are_demoted_when_idle() {
        run(|ui| {
            let text = doc(8000);
            let mut cache = LineCache::default();
            let view_at = |y: f32| View { y, h: 400.0, carets: [None; 2], target: None };
            let mut l = cache.layout_big(ui, &text, 300.0, &Plain, 0..0, &view_at(0.0));
            let total_h = l.galley.rect.height();
            let paras = cache.prog.as_ref().map_or(0, |p| p.paras.len());
            // Scroll end to end, idle at every stop.
            for k in 0..=20 {
                let view = view_at(total_h * k as f32 / 20.0);
                if let Some(n) = cache.refocus_with(ui, &view, Some(&Plain)) {
                    l = n;
                }
                for _ in 0..40 {
                    let (n, more) = cache.prefetch(ui, &view, Some(&Plain), 3.0, Duration::from_secs(5));
                    l = n.unwrap_or(l);
                    if !more {
                        break;
                    }
                }
            }
            let real = paras - cache.placeholders();
            assert!(real < paras / 4, "only the part around the last view stays laid out: {real} of {paras}");
            // A caret far from the view keeps its paragraphs laid out through the sweep.
            let end = View { carets: [Some(0), None], ..view_at(total_h) };
            cache.refocus_with(ui, &end, Some(&Plain));
            for _ in 0..40 {
                let (n, more) = cache.prefetch(ui, &end, Some(&Plain), 3.0, Duration::from_secs(5));
                l = n.unwrap_or(l);
                if !more {
                    break;
                }
            }
            assert!(!cache.needs_focus(&end), "the caret paragraphs stay exact");
            let view = view_at(0.0);
            assert!(cache.needs_focus(&view), "the top was demoted while the view was at the end");
            let n = cache.refocus_with(ui, &view, Some(&Plain)).expect("laid out again");
            assert!(!cache.needs_focus(&view));
            assert_eq!(chars_of(&n.galley), text.chars().count());
            drop(l);
        });
    }

    /// Deleting text (a selection over several lines, a backspace over a line break) keeps the rows of the patched galley in order.
    #[test]
    fn deletions_keep_rows_in_order() {
        run(|ui| {
            let mut seed = 12345u64;
            let mut rnd = |n: usize| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((seed >> 33) as usize) % n.max(1)
            };
            for wrap in [300.0, f32::INFINITY] {
                let mut text = doc(3000);
                let mut cache = LineCache::default();
                let mut view = View { y: 0.0, h: 400.0, carets: [None; 2], target: None };
                cache.layout_big(ui, &text, wrap, &Plain, 0..0, &view);
                for step in 0..300 {
                    let at = (0..=rnd(text.len())).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
                    let len = if step % 3 == 0 { 1 } else { rnd(600) };
                    let end = (at + len).min(text.len());
                    let end = (end..=text.len()).find(|&i| text.is_char_boundary(i)).unwrap_or(text.len());
                    text.replace_range(at..end, "");
                    view.y = rnd(20_000) as f32;
                    view.carets = [Some(at.min(text.len())), None];
                    let mut l = cache.layout_big(ui, &text, wrap, &Plain, 0..0, &view);
                    if step % 2 == 0 {
                        if let Some(n) = cache.refocus_with(ui, &view, Some(&Plain)) {
                            l = n;
                        }
                    }
                    let bad = l.galley.rows.windows(2).position(|w| (w[1].pos.y - (w[0].pos.y + w[0].row.size.y)).abs() > 1.0);
                    assert!(bad.is_none(), "wrap {wrap} step {step}: rows overlap at {bad:?} after deleting {at}..{end}");
                    assert_eq!(chars_of(&l.galley), text.chars().count(), "step {step}");
                }
            }
        });
    }
}
