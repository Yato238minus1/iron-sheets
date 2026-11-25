//! Syntax highlighting for the editor: `Tok` / `TypTok` spans -> `LayoutJob` sections.
//!
//! Contract: sections never span a line break (egui caches layout per paragraph only then);
//! texts above `PLAIN_ABOVE` bytes use the plain per-line layout.
use crate::theme::{EditorStyle, families};
use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontFamily, FontId, Stroke};
use iron_sheets_app_core::services::{CodeClass, CodeHighlighter, CodeHl};
use iron_sheets_core::FileKind;
use iron_sheets_md::highlight::{IncrementalHighlighter, Tok, WindowHighlighter};
use std::cell::RefCell;

pub const PLAIN_ABOVE: usize = iron_sheets_app_core::settings_model::DEFAULT_HIGHLIGHT_LIMIT;

/// Size above which highlighting is skipped (setting `highlight_limit_kb`; the value lives in the core).
pub fn limit() -> usize {
    #[cfg(test)]
    if let Some(l) = TEST_LIMIT.with(|c| c.get()) {
        return l;
    }
    iron_sheets_app_core::settings_model::highlight_limit()
}

// Tests run in parallel: a test that lowers the limit must not change it for the others, so the override is per thread.
#[cfg(test)]
thread_local!(static TEST_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) });

/// Highlighter caches kept per thread, one per recently shown text. `update` diffs the text against the cached
/// one, so the right entry is found by similarity and no key from the caller is needed.
const SLOTS: usize = 4;

struct Slot<H> {
    hl: H,
    used: u64,
}

struct Cache<H> {
    slots: Vec<Slot<H>>,
    clock: u64,
}

impl<H: Default> Cache<H> {
    const fn new() -> Self {
        Cache { slots: Vec::new(), clock: 0 }
    }

    /// The cache whose text is most similar to `text` (`sim` = shared prefix + suffix bytes); a new or the
    /// least recently used one when nothing is similar enough.
    fn pick_idx(&mut self, text_len: usize, sim: impl Fn(&H) -> usize) -> (usize, u64, &mut H) {
        self.clock += 1;
        let best = self.slots.iter().enumerate().map(|(i, s)| (sim(&s.hl), i)).max();
        let i = match best {
            Some((score, i)) if score * 2 >= text_len => i,
            _ if self.slots.len() < SLOTS => {
                self.slots.push(Slot { hl: H::default(), used: 0 });
                self.slots.len() - 1
            }
            _ => self.slots.iter().enumerate().min_by_key(|(_, s)| s.used).map_or(0, |(i, _)| i),
        };
        let stamp = self.clock;
        let slot = &mut self.slots[i];
        slot.used = stamp;
        (i, stamp, &mut slot.hl)
    }

    fn pick(&mut self, text_len: usize, sim: impl Fn(&H) -> usize) -> &mut H {
        self.pick_idx(text_len, sim).2
    }
}

#[derive(Default)]
struct MdEntry {
    hl: IncrementalHighlighter,
    /// `(line_byte_start, span_start_idx)` per line, plus sentinel `(text.len(), spans.len())`.
    lines: Vec<(u32, u32)>,
    fm_end: usize,
}

#[cfg(feature = "typst")]
#[derive(Default)]
struct TypEntry {
    hl: iron_sheets_typst::IncrementalTypstHighlighter,
    /// `(line_byte_start, span_start_idx)` per line, plus sentinel `(text.len(), spans.len())`.
    lines: Vec<(u32, u32)>,
    /// Per-line span hashes for bounding `dirty` after incremental Typst updates.
    hashes: Vec<u64>,
}

thread_local! {
    static FORMATS: RefCell<Option<(EditorStyle, std::rc::Rc<Formats>)>> = const { RefCell::new(None) };
    static MD_CACHE: RefCell<Cache<MdEntry>> = const { RefCell::new(Cache::new()) };
    #[cfg(feature = "typst")]
    static TYP_CACHE: RefCell<Cache<TypEntry>> = const { RefCell::new(Cache::new()) };
}

/// Format table indices.
#[derive(Clone, Copy)]
enum F {
    Text,
    Marker,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    Bold,
    Italic,
    BoldItalic,
    Strike,
    Code,
    CodeFence,
    Link,
    WikiLink,
    ListMark,
    Quote,
    Comment,
    CommentItalic,
    Math,
    Tag,
    Highlight,
    Keyword,
    Str,
    Number,
    Error,
    Operator,
    Func,
    /// The code block colours, in `CodeClass` order.
    /// Code text without a class (only with the code highlighter on): the normal text colour, as in the preview.
    CodePlain,
    CodeComment,
    CodeStr,
    CodeNum,
    CodeKw,
    CodeFunc,
    CodeType,
    N,
}

fn code_slot(c: CodeClass) -> F {
    match c {
        CodeClass::Comment => F::CodeComment,
        CodeClass::Str => F::CodeStr,
        CodeClass::Num => F::CodeNum,
        CodeClass::Kw => F::CodeKw,
        CodeClass::Func => F::CodeFunc,
        CodeClass::Type => F::CodeType,
    }
}

/// Format table by slot, plus what must stay literal. `active` is false when no splitting can matter (the font has no
/// ligatures, or everything may ligate).
struct Formats {
    slots: Vec<TextFormat>,
    active: bool,
    /// Ligatures off: every section is literal; otherwise only those with `CODE_MARK`.
    all_literal: bool,
}

/// Code slots (spans, fences, block text) carry this `expand_bg`; no other slot does (their default is 1.0).
const CODE_MARK: f32 = 0.0;

/// ASCII characters that programming fonts (Lilex) join into ligatures: `->` `<=>` `!=` `|>` `(*` `::` ...
const LIGA: &[u8] = b"-<>=!:|/*&+.~#?_%^$\\@;()[]{}";

/// Byte offsets inside `range` (exclusive of both ends) between two adjacent ligature characters of `text`.
/// Starting a new section there keeps the font from joining them: runs of 2+ get one section per character.
pub(crate) fn split_points(text: &str, range: std::ops::Range<usize>) -> Vec<usize> {
    let b = text.as_bytes();
    (range.start + 1..range.end).filter(|&p| LIGA.contains(&b[p - 1]) && LIGA.contains(&b[p])).collect()
}

impl Formats {
    /// Sections of this format must stay literal.
    fn literal(&self, f: &TextFormat) -> bool {
        self.all_literal || f.expand_bg == CODE_MARK
    }
}

/// Splits the sections that must stay literal between adjacent ligature characters (after merging, so nothing
/// merges back). `text` is what the section ranges index. Display only: the text is untouched.
fn literalize(text: &str, secs: &mut Vec<LayoutSection>, fmts: &Formats) {
    if !fmts.active {
        return;
    }
    let splits: Vec<(usize, Vec<usize>)> = secs
        .iter()
        .enumerate()
        .filter(|(_, s)| fmts.literal(&s.format))
        .map(|(i, s)| (i, split_points(text, s.byte_range.start.0..s.byte_range.end.0)))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    if splits.is_empty() {
        return;
    }
    // Move the sections over; only the split ones are cloned (once per piece).
    let old = std::mem::take(secs);
    secs.reserve(old.len() + splits.iter().map(|(_, p)| p.len()).sum::<usize>());
    let mut next = splits.into_iter().peekable();
    for (i, s) in old.into_iter().enumerate() {
        let Some((_, pts)) = next.next_if(|(k, _)| *k == i) else {
            secs.push(s);
            continue;
        };
        let mut from = s.byte_range.start.0;
        for p in pts.into_iter().chain([s.byte_range.end.0]) {
            secs.push(LayoutSection { byte_range: ByteIndex(from)..ByteIndex(p), ..s.clone() });
            from = p;
        }
    }
}

/// The format table for `style`, rebuilt only when the style changes.
fn formats(style: &EditorStyle) -> std::rc::Rc<Formats> {
    FORMATS.with(|c| {
        let mut c = c.borrow_mut();
        match &*c {
            Some((s, f)) if s == style => f.clone(),
            _ => {
                let f = std::rc::Rc::new(Formats::new(style));
                *c = Some((style.clone(), f.clone()));
                f
            }
        }
    })
}

impl Formats {
    fn new(style: &EditorStyle) -> Formats {
        let fam = families();
        let size = style.font_id.size;
        let font = |f: FontFamily| FontId::new(size, f);
        let plain = |color: Color32| TextFormat::simple(style.font_id.clone(), color);
        let with = |f: FontFamily, color: Color32| TextFormat::simple(font(f), color);
        let mut v = vec![plain(style.text); F::N as usize];
        let mut set = |i: F, t: TextFormat| v[i as usize] = t;
        set(F::Marker, plain(style.marker));
        for which in [F::H1, F::H2, F::H3, F::H4, F::H5, F::H6] {
            set(which, with(fam.mono_bold.clone(), style.heading));
        }
        set(F::Bold, with(fam.mono_bold.clone(), style.strong));
        set(F::Italic, with(fam.mono_italic.clone(), style.emphasis));
        set(F::BoldItalic, with(fam.mono_bold_italic.clone(), style.strong));
        let mut strike = plain(style.marker);
        strike.strikethrough = Stroke::new(1.0, style.marker);
        set(F::Strike, strike);
        let mut code = plain(style.code);
        code.background = style.code_bg;
        // The default expands the background by 1 px: the next row's fill then covers the descender of `_` above it.
        code.expand_bg = CODE_MARK;
        set(F::Code, code.clone());
        let mut fence = plain(style.marker);
        fence.background = style.code_bg;
        fence.expand_bg = CODE_MARK;
        set(F::CodeFence, fence);
        set(F::Link, plain(style.link));
        let mut wiki = plain(style.link);
        wiki.underline = Stroke::new(1.0, style.link);
        set(F::WikiLink, wiki);
        set(F::ListMark, with(fam.mono_bold.clone(), style.keyword));
        set(F::Quote, plain(style.quote));
        set(F::Comment, plain(style.comment));
        set(F::CommentItalic, with(fam.mono_italic.clone(), style.comment));
        set(F::Math, plain(style.math));
        set(F::Tag, plain(style.tag));
        let mut hl = plain(style.text);
        hl.background = style.selection_bg;
        set(F::Highlight, hl);
        set(F::Keyword, plain(style.keyword));
        set(F::Str, plain(style.string));
        set(F::Number, plain(style.number));
        let mut err = plain(Color32::from_rgb(220, 70, 70));
        err.underline = Stroke::new(1.0, Color32::from_rgb(220, 70, 70));
        set(F::Error, err);
        set(F::Operator, plain(style.marker));
        set(F::Func, plain(style.link));
        for (slot, color) in [
            (F::CodePlain, style.text),
            (F::CodeComment, style.comment),
            (F::CodeStr, style.string),
            (F::CodeNum, style.number),
            (F::CodeKw, style.keyword),
            (F::CodeFunc, style.link),
            (F::CodeType, style.tag),
        ] {
            let mut f = code.clone();
            f.color = color;
            set(slot, f);
        }
        Formats { slots: v, active: style.font_ligates && !(style.ligatures && style.ligatures_in_code), all_literal: !style.ligatures }
    }
}

fn md_slot(tok: Tok) -> F {
    match tok {
        Tok::Text => F::Text,
        Tok::HeadingMark => F::Marker,
        Tok::Heading(n) => match n {
            1 => F::H1,
            2 => F::H2,
            3 => F::H3,
            4 => F::H4,
            5 => F::H5,
            _ => F::H6,
        },
        Tok::Bold => F::Bold,
        Tok::Italic => F::Italic,
        Tok::BoldItalic => F::BoldItalic,
        Tok::Strike => F::Strike,
        Tok::Code => F::Code,
        Tok::CodeFence => F::CodeFence,
        Tok::CodeBlock => F::Code,
        Tok::LinkText | Tok::Image | Tok::Footnote => F::Link,
        Tok::LinkUrl => F::Marker,
        Tok::WikiLink => F::WikiLink,
        Tok::ListMark | Tok::TaskMark => F::ListMark,
        Tok::QuoteMark => F::Quote,
        Tok::Rule | Tok::Escape | Tok::TableSep | Tok::MarkupPunct => F::Marker,
        Tok::Html | Tok::FrontMatter => F::Comment,
        Tok::Comment => F::CommentItalic,
        Tok::Math => F::Math,
        Tok::Tag => F::Tag,
        Tok::Highlight => F::Highlight,
    }
}

/// `toks` with the code lines of fenced blocks split at the highlighter's `spans` (sorted, disjoint).
fn with_code(toks: &[(u32, Tok)], spans: &[(usize, usize, Option<CodeClass>)]) -> Vec<(u32, F)> {
    let mut out = Vec::with_capacity(toks.len() + spans.len() * 2);
    let (mut at, mut si) = (0usize, 0usize);
    for &(len, tok) in toks {
        let end = at + len as usize;
        if tok != Tok::CodeBlock {
            out.push((len, md_slot(tok)));
            at = end;
            continue;
        }
        let mut p = at;
        while p < end {
            while si < spans.len() && spans[si].1 <= p {
                si += 1;
            }
            match spans.get(si) {
                Some(&(a, b, class)) if a < end => {
                    if a > p {
                        out.push(((a - p) as u32, F::Code));
                        p = a;
                    }
                    let e = b.min(end);
                    out.push(((e - p) as u32, class.map_or(F::CodePlain, code_slot)));
                    p = e;
                }
                _ => {
                    out.push(((end - p) as u32, F::Code));
                    p = end;
                }
            }
        }
        at = end;
    }
    out
}

/// Builds the job: one section per token (equal neighbouring formats merged inside a line), none spanning a
/// line break. `tokens` are `(length, format slot)` and tile `text`.
fn build_job(text: &str, wrap_width: f32, fmts: &Formats, tokens: impl Iterator<Item = (u32, F)>, n_tokens: usize) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    job.text.push_str(text);
    job.sections.reserve(n_tokens);
    let bytes = text.as_bytes();
    let mut at = 0usize;
    let mut last: Option<F> = None;
    for (len, slot) in tokens {
        let end = at + len as usize;
        if len == 0 {
            continue;
        }
        match job.sections.last_mut() {
            Some(sec) if last.is_some_and(|l| l as u8 == slot as u8) && bytes[at - 1] != b'\n' => sec.byte_range.end = ByteIndex(end),
            _ => job.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(at)..ByteIndex(end), format: fmts.slots[slot as usize].clone() }),
        }
        last = Some(slot);
        at = end;
    }
    job
}

fn plain_job(text: &str, style: &EditorStyle, wrap_width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let fmt = TextFormat::simple(style.font_id.clone(), style.text);
    if text.len() > limit() {
        // One section per line is still required (paragraph caching), but nothing else is computed.
        job.text.reserve(text.len());
    }
    for line in text.split_inclusive('\n') {
        job.append(line, 0.0, fmt.clone());
    }
    if text.is_empty() {
        job.append("", 0.0, fmt);
    }
    literalize(&job.text, &mut job.sections, &formats(style));
    job
}

pub fn big_partial() -> bool {
    iron_sheets_app_core::docs::highlight_big_partial()
}

/// True when `text` is laid out through the big-note path (no whole-text job).
pub fn is_big(text_len: usize) -> bool {
    text_len > limit()
}

/// Which kinds get a highlighter in the big-note path (Markdown only: a Typst parse is whole-tree, see the README).
pub fn big_highlighted(kind: FileKind) -> bool {
    big_partial() && kind == FileKind::Markdown
}

/// Multi-line exact notes at or above 4 KB use `LineCache::layout_big` with `IncrementalSource` so keystrokes
/// only shape the edited/visible paragraphs instead of rebuilding a whole-note `LayoutJob`.
pub(super) fn is_incremental_exact(text: &str, kind: FileKind) -> bool {
    text.len() >= 4096
        && text.len() <= limit()
        && match kind {
            FileKind::Markdown => true,
            #[cfg(feature = "typst")]
            FileKind::Typst => true,
            _ => false,
        }
        && text.contains('\n')
}

fn common_prefix_boundary(a: &str, b: &str) -> usize {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let n = ab.len().min(bb.len());
    let mut p = 0usize;
    while p + 4096 <= n && ab[p..p + 4096] == bb[p..p + 4096] {
        p += 4096;
    }
    while p + 64 <= n && ab[p..p + 64] == bb[p..p + 64] {
        p += 64;
    }
    while p < n && ab[p] == bb[p] {
        p += 1;
    }
    if p == ab.len() && p == bb.len() {
        return p;
    }
    while p > 0 && !(a.is_char_boundary(p) && b.is_char_boundary(p)) {
        p -= 1;
    }
    p
}

fn leading_fm_end(toks: &[(u32, Tok)]) -> usize {
    let mut end = 0usize;
    for &(len, tok) in toks {
        if tok != Tok::FrontMatter {
            break;
        }
        end += len as usize;
    }
    end
}

impl MdEntry {
    fn rebuild_lines(&mut self, text_len: usize) {
        self.lines.clear();
        let mut sidx = 0u32;
        self.hl.for_each_line(|r, spans| {
            self.lines.push((r.start as u32, sidx));
            sidx += spans.len() as u32;
        });
        self.lines.push((text_len as u32, sidx));
        self.fm_end = leading_fm_end(self.hl.tokens());
    }

    fn update(&mut self, text: &str) -> std::ops::Range<usize> {
        if self.lines.is_empty() {
            self.hl.update(text);
            self.rebuild_lines(text.len());
            return 0..text.len();
        }
        let old = self.hl.text();
        let p = common_prefix_boundary(old, text);
        if p == old.len() && p == text.len() {
            return 0..0;
        }
        let a0 = self.lines[1..].partition_point(|&(end, _)| (end as usize) <= p);
        let old_fm = self.fm_end;
        self.hl.update(text);
        let redone = self.hl.lines_redone();
        self.rebuild_lines(text.len());
        if redone == 0 {
            return 0..0;
        }
        let dirty_line = if old_fm != self.fm_end { 0 } else { a0.saturating_sub(1) };
        let last_idx = self.lines.len().saturating_sub(1);
        let mut start_line = dirty_line.min(last_idx);
        let mut end_line = (dirty_line + redone).min(last_idx);
        // Changing a fence opener, body line, or closer can alter `codehl` colors for the entire block even when
        // `IncrementalHighlighter` stops after 1-2 lines. Expand `dirty` across any touched fenced block.
        let spans = self.hl.tokens();
        let line_has = |idx: usize, tok: Tok| -> bool {
            let (a, b) = (self.lines[idx].1 as usize, self.lines[idx + 1].1 as usize);
            spans[a..b].iter().any(|&(_, t)| t == tok)
        };
        if start_line < last_idx && line_has(start_line, Tok::CodeBlock) {
            while start_line > 0 && line_has(start_line - 1, Tok::CodeBlock) {
                start_line -= 1;
            }
            if start_line > 0 && line_has(start_line - 1, Tok::CodeFence) {
                start_line -= 1;
            }
        }
        if end_line > start_line
            && end_line <= last_idx
            && (line_has(end_line - 1, Tok::CodeBlock) || line_has(end_line - 1, Tok::CodeFence))
        {
            while end_line < last_idx && line_has(end_line, Tok::CodeBlock) {
                end_line += 1;
            }
            if end_line < last_idx && line_has(end_line, Tok::CodeFence) {
                end_line += 1;
            }
        }
        self.lines[start_line].0 as usize..self.lines[end_line].0 as usize
    }
}

#[cfg(feature = "typst")]
impl TypEntry {
    fn rebuild_lines(&mut self, text: &str) {
        self.lines.clear();
        self.hashes.clear();
        if text.is_empty() {
            self.lines.push((0, 0));
            return;
        }
        let bytes = text.as_bytes();
        let tokens = self.hl.tokens();
        let mut at = 0usize;
        let mut line_start = 0u32;
        let mut span_start = 0u32;
        let mut h = 0x517cc1b727220a95u64;
        for (i, &(len, tok)) in tokens.iter().enumerate() {
            if len == 0 {
                continue;
            }
            h = h.rotate_left(5) ^ (((len as u64) << 8) | (tok as u64));
            h = h.wrapping_mul(0x9e3779b97f4a7c15);
            at += len as usize;
            if bytes[at - 1] == b'\n' {
                self.lines.push((line_start, span_start));
                self.hashes.push(h);
                line_start = at as u32;
                span_start = (i + 1) as u32;
                h = 0x517cc1b727220a95u64;
            }
        }
        if (line_start as usize) < at {
            self.lines.push((line_start, span_start));
            self.hashes.push(h);
        }
        self.lines.push((at as u32, tokens.len() as u32));
    }

    fn update(&mut self, text: &str) -> std::ops::Range<usize> {
        if self.lines.is_empty() {
            self.hl.update(text);
            self.rebuild_lines(text);
            return 0..text.len();
        }
        let old = self.hl.text();
        let (p, s) = iron_sheets_md::diff_texts(old, text);
        if p == old.len() && p == text.len() {
            return 0..0;
        }
        let old_inc = self.hl.incremental_updates();
        let old_hashes = std::mem::take(&mut self.hashes);
        self.hl.update(text);
        self.rebuild_lines(text);
        if self.hl.incremental_updates() == old_inc {
            // Non-incremental rebuild (syntax diagnostics or inconsistent root children): any line may have changed.
            return 0..text.len();
        }
        let n_old = old_hashes.len();
        let n_new = self.hashes.len();
        if n_old == 0 || n_new == 0 {
            return 0..text.len();
        }
        let max_pfx = self.lines[1..].partition_point(|&(end, _)| (end as usize) <= p).min(n_old).min(n_new);
        let min_sfx_byte = text.len() - s;
        let sfx_line = self.lines[..n_new].partition_point(|&(start, _)| (start as usize) < min_sfx_byte);
        let max_sfx = n_new.saturating_sub(sfx_line).min(n_old.saturating_sub(max_pfx));
        let pfx = (0..max_pfx).take_while(|&i| old_hashes[i] == self.hashes[i]).count();
        let sfx = (0..max_sfx).take_while(|&k| old_hashes[n_old - 1 - k] == self.hashes[n_new - 1 - k]).count();
        let start_line = pfx.min(n_new);
        let end_line = (n_new - sfx).max(start_line).min(n_new);
        self.lines[start_line].0 as usize..self.lines[end_line].0 as usize
    }
}

/// Sections for the paragraphs of a big note, from a `WindowHighlighter` (or plain when `hl` is `None`).
pub struct BigSource<'a> {
    hl: Option<&'a WindowHighlighter>,
    fmts: std::rc::Rc<Formats>,
    plain: TextFormat,
    /// Language colours of fenced blocks; `memo` keeps the spans of the block the last code line was in.
    code: Option<&'a dyn CodeHighlighter>,
    memo: std::cell::RefCell<Option<Memo>>,
    /// The last `sections` call used a block whose colours were still pending.
    pending: std::cell::Cell<bool>,
}

type CodeSpan = (usize, usize, Option<CodeClass>);
/// Spans of one fenced block: its body range, the spans, and whether any of them is still a pending result (stand-in colours).
type Memo = (std::ops::Range<usize>, Vec<CodeSpan>, bool);

/// `block_spans` for a block of `text`; the flag tells that `code` answered `Pending` for it.
fn block_memo(code: &dyn CodeHighlighter, text: &str, opener: std::ops::Range<usize>, body: std::ops::Range<usize>) -> Memo {
    let pending = std::cell::Cell::new(false);
    let spans = super::codehl::block_spans(text, opener, body.clone(), |l, b| {
        let r = code.get(l, b);
        pending.set(pending.get() || matches!(r, CodeHl::Pending(_)));
        r
    });
    (body, spans, pending.get())
}

pub fn big_source<'a>(hl: Option<&'a WindowHighlighter>, style: &EditorStyle) -> BigSource<'a> {
    BigSource { hl, fmts: formats(style), plain: TextFormat::simple(style.font_id.clone(), style.text), code: None, memo: Default::default(), pending: Default::default() }
}

impl<'a> BigSource<'a> {
    /// Colours the code blocks of Markdown paragraphs with `code` (only blocks of the paragraphs asked for are requested).
    pub fn with_code(mut self, code: Option<&'a dyn CodeHighlighter>) -> Self {
        self.code = code;
        self
    }

    /// Code spans of the fenced block that has the line starting at `ls` in its body, shifted by `-ls`.
    fn code_spans(&self, code: &dyn CodeHighlighter, hl: &WindowHighlighter, text: &str, ls: usize, end: usize) -> Vec<CodeSpan> {
        let mut memo = self.memo.borrow_mut();
        if !memo.as_ref().is_some_and(|(r, ..)| r.contains(&ls)) {
            *memo = hl.fence_block(text, ls).map(|(op, body)| block_memo(code, text, op, body));
        }
        let Some((_, spans, pending)) = memo.as_ref() else { return Vec::new() };
        self.pending.set(self.pending.get() || *pending);
        let i = spans.partition_point(|s| s.1 <= ls);
        spans[i..].iter().take_while(|s| s.0 < end).map(|&(a, b, c)| (a - ls.min(a), b - ls, c)).collect()
    }
}

#[inline]
fn push_section(out: &mut Vec<LayoutSection>, last: &mut Option<F>, a: usize, b: usize, slot: F, fmts: &Formats) {
    if a >= b {
        return;
    }
    match out.last_mut() {
        Some(sec) if last.is_some_and(|l| l as u8 == slot as u8) => sec.byte_range.end = ByteIndex(b),
        _ => out.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(a)..ByteIndex(b),
            format: fmts.slots[slot as usize].clone(),
        }),
    }
    *last = Some(slot);
}

fn line_slice(lines: &[(u32, u32)], start: usize) -> Option<(usize, usize, usize, usize)> {
    let num_lines = lines.len().saturating_sub(1);
    if num_lines == 0 {
        return None;
    }
    let li = lines[..num_lines].partition_point(|&(ls, _)| (ls as usize) <= start).saturating_sub(1);
    Some((li, lines[li].0 as usize, lines[li].1 as usize, lines[li + 1].1 as usize))
}

fn emit_line_sections<T: Copy>(line_start: usize, start: usize, end: usize, spans: &[(u32, T)], slot_of: fn(T) -> F, fmts: &Formats, out: &mut Vec<LayoutSection>) {
    let mut at = line_start;
    if start == end {
        // An empty line: egui takes the height from the (empty) section of its line break.
        for &(len, tok) in spans {
            let span_end = at + len as usize;
            if span_end > start {
                out.push(LayoutSection {
                    leading_space: 0.0,
                    byte_range: ByteIndex(0)..ByteIndex(0),
                    format: fmts.slots[slot_of(tok) as usize].clone(),
                });
                break;
            }
            at = span_end;
        }
        return;
    }
    let mut last: Option<F> = None;
    for &(len, tok) in spans {
        if len == 0 {
            continue;
        }
        let span_end = at + len as usize;
        if span_end <= start {
            at = span_end;
            continue;
        }
        if at >= end {
            break;
        }
        let a = at.max(start) - start;
        let b = span_end.min(end) - start;
        push_section(out, &mut last, a, b, slot_of(tok), fmts);
        at = span_end;
    }
}

fn emit_code_sections(start: usize, end: usize, code_spans: &[CodeSpan], fmts: &Formats, out: &mut Vec<LayoutSection>) {
    if start == end {
        let idx = code_spans.partition_point(|s| s.1 <= start);
        let slot = match code_spans.get(idx) {
            Some(&(a, b, class)) if a <= start && start < b => class.map_or(F::CodePlain, code_slot),
            _ => F::Code,
        };
        out.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(0), format: fmts.slots[slot as usize].clone() });
        return;
    }
    let mut p = start;
    let mut si = code_spans.partition_point(|s| s.1 <= p);
    let mut last: Option<F> = None;
    while p < end {
        match code_spans.get(si) {
            Some(&(a, b, class)) if a < end => {
                if a > p {
                    push_section(out, &mut last, p - start, a - start, F::Code, fmts);
                    p = a;
                }
                let e = b.min(end);
                push_section(out, &mut last, p - start, e - start, class.map_or(F::CodePlain, code_slot), fmts);
                p = e;
                if b <= end {
                    si += 1;
                }
            }
            _ => {
                push_section(out, &mut last, p - start, end - start, F::Code, fmts);
                p = end;
            }
        }
    }
}

/// Finds the enclosing `(opener, body)` byte ranges for an unquoted, unindented Markdown fenced code block.
fn md_fence_block(text: &str, lines: &[(u32, u32)], spans: &[(u32, Tok)], li: usize) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let is_plain_code_line = |idx: usize| -> bool {
        let (a, b) = (lines[idx].1 as usize, lines[idx + 1].1 as usize);
        matches!(&spans[a..b], [(_, Tok::CodeBlock)])
    };
    if !is_plain_code_line(li) {
        return None;
    }
    let mut lo = li;
    while lo > 0 && is_plain_code_line(lo - 1) {
        lo -= 1;
    }
    if lo == 0 {
        return None;
    }
    let op_idx = lo - 1;
    let (op_a, op_b) = (lines[op_idx].1 as usize, lines[op_idx + 1].1 as usize);
    let opener_start = lines[op_idx].0 as usize;
    let opener_end = lines[lo].0 as usize;
    if !matches!(&spans[op_a..op_b], [(_, Tok::CodeFence)]) || !matches!(text.as_bytes().get(opener_start), Some(b'`' | b'~')) {
        return None;
    }
    let num_lines = lines.len().saturating_sub(1);
    let mut hi = li + 1;
    while hi < num_lines && is_plain_code_line(hi) {
        hi += 1;
    }
    Some((opener_start..opener_end, opener_end..lines[hi].0 as usize))
}

/// Sections for the paragraphs of an exact-highlighted note, backed by `MD_CACHE` or `TYP_CACHE`.
pub(super) struct IncrementalSource<'a> {
    kind: FileKind,
    fmts: std::rc::Rc<Formats>,
    plain: TextFormat,
    code: Option<&'a dyn CodeHighlighter>,
    memo: std::cell::RefCell<Option<Memo>>,
    pending: std::cell::Cell<bool>,
    /// `(slot_idx, cache_stamp)` verified to hold `text` (avoids re-diffing across paragraphs in a frame).
    slot: std::cell::Cell<Option<(usize, u64)>>,
}

pub(super) fn incremental_source<'a>(
    text: &str,
    kind: FileKind,
    style: &EditorStyle,
    code: Option<&'a dyn CodeHighlighter>,
) -> (IncrementalSource<'a>, std::ops::Range<usize>) {
    let src = incremental_source_lazy(kind, style, code);
    let dirty = match kind {
        FileKind::Markdown => MD_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            let (idx, stamp, entry) = cache.pick_idx(text.len(), |e| e.hl.similarity(text));
            let dirty = entry.update(text);
            src.slot.set(Some((idx, stamp)));
            dirty
        }),
        #[cfg(feature = "typst")]
        FileKind::Typst => TYP_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            let (idx, stamp, entry) = cache.pick_idx(text.len(), |e| e.hl.similarity(text));
            let dirty = entry.update(text);
            src.slot.set(Some((idx, stamp)));
            dirty
        }),
        _ => 0..0,
    };
    (src, dirty)
}

pub(super) fn incremental_source_lazy<'a>(kind: FileKind, style: &EditorStyle, code: Option<&'a dyn CodeHighlighter>) -> IncrementalSource<'a> {
    IncrementalSource {
        kind,
        fmts: formats(style),
        plain: TextFormat::simple(style.font_id.clone(), style.text),
        code: code.filter(|_| kind == FileKind::Markdown),
        memo: Default::default(),
        pending: Default::default(),
        slot: std::cell::Cell::new(None),
    }
}

fn with_md_slot<R>(text: &str, slot_cell: &std::cell::Cell<Option<(usize, u64)>>, f: impl FnOnce(&MdEntry) -> R) -> R {
    MD_CACHE.with(|cell| {
        if let Some((idx, stamp)) = slot_cell.get() {
            let cache = cell.borrow();
            if let Some(s) = cache.slots.get(idx)
                && s.used == stamp
                && !s.hl.lines.is_empty()
                && s.hl.hl.text().len() == text.len()
            {
                return f(&s.hl);
            }
        }
        let mut cache = cell.borrow_mut();
        let (idx, stamp, entry) = cache.pick_idx(text.len(), |e| e.hl.similarity(text));
        entry.update(text);
        slot_cell.set(Some((idx, stamp)));
        f(entry)
    })
}

#[cfg(feature = "typst")]
fn with_typ_slot<R>(text: &str, slot_cell: &std::cell::Cell<Option<(usize, u64)>>, f: impl FnOnce(&TypEntry) -> R) -> R {
    TYP_CACHE.with(|cell| {
        if let Some((idx, stamp)) = slot_cell.get() {
            let cache = cell.borrow();
            if let Some(s) = cache.slots.get(idx)
                && s.used == stamp
                && !s.hl.lines.is_empty()
                && s.hl.hl.text().len() == text.len()
            {
                return f(&s.hl);
            }
        }
        let mut cache = cell.borrow_mut();
        let (idx, stamp, entry) = cache.pick_idx(text.len(), |e| e.hl.similarity(text));
        entry.update(text);
        slot_cell.set(Some((idx, stamp)));
        f(entry)
    })
}

impl super::paragraphs::Sections for IncrementalSource<'_> {
    fn base_format(&self) -> TextFormat {
        self.plain.clone()
    }

    fn pending(&self) -> bool {
        self.pending.get()
    }

    fn sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
        self.pending.set(false);
        self.raw_sections(text, start, end, out);
        literalize(&text[start..end], out, &self.fmts);
    }
}

impl IncrementalSource<'_> {
    fn raw_sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
        out.clear();
        if text.is_empty() {
            return;
        }
        match self.kind {
            FileKind::Markdown => with_md_slot(text, &self.slot, |entry| {
                let Some((li, line_start, sp0, sp1)) = line_slice(&entry.lines, start) else { return };
                let spans = entry.hl.tokens();
                let line_spans = &spans[sp0..sp1];
                if let Some(code) = self.code.filter(|_| line_spans.iter().any(|&(_, t)| t == Tok::CodeBlock)) {
                    let mut memo = self.memo.borrow_mut();
                    if !memo.as_ref().is_some_and(|(r, ..)| r.contains(&line_start)) {
                        *memo = md_fence_block(text, &entry.lines, spans, li).map(|(op, body)| block_memo(code, text, op, body));
                    }
                    if let Some((_, code_spans, pending)) = memo.as_ref().filter(|(_, s, _)| !s.is_empty()) {
                        self.pending.set(self.pending.get() || *pending);
                        emit_code_sections(start, end, code_spans, &self.fmts, out);
                        return;
                    }
                }
                emit_line_sections(line_start, start, end, line_spans, md_slot, &self.fmts, out);
            }),
            #[cfg(feature = "typst")]
            FileKind::Typst => with_typ_slot(text, &self.slot, |entry| {
                let Some((_, line_start, sp0, sp1)) = line_slice(&entry.lines, start) else { return };
                let line_spans = &entry.hl.tokens()[sp0..sp1];
                emit_line_sections(line_start, start, end, line_spans, typ_slot, &self.fmts, out);
            }),
            _ => out.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(end - start), format: self.plain.clone() }),
        }
    }
}

impl super::paragraphs::Sections for BigSource<'_> {
    fn base_format(&self) -> TextFormat {
        self.plain.clone()
    }

    fn pending(&self) -> bool {
        self.pending.get()
    }

    fn sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
        self.pending.set(false);
        self.raw_sections(text, start, end, out);
        literalize(&text[start..end], out, &self.fmts);
    }
}

impl BigSource<'_> {
    fn raw_sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<LayoutSection>) {
        out.clear();
        let Some(hl) = self.hl else {
            out.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(end - start), format: self.plain.clone() });
            return;
        };
        let mut toks = Vec::new();
        hl.tokens(text, start, end, &mut toks);
        let mut at = start;
        let mut last: Option<F> = None;
        if start == end {
            // An empty line: egui takes the height from the (empty) section of its line break.
            if let Some(&(_, tok)) = toks.first() {
                out.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(0)..ByteIndex(0), format: self.fmts.slots[md_slot(tok) as usize].clone() });
            }
            return;
        }
        let toks: Vec<(u32, F)> = match self.code.filter(|_| toks.iter().any(|t| t.1 == Tok::CodeBlock)) {
            Some(code) => with_code(&toks, &self.code_spans(code, hl, text, start, end)),
            None => toks.iter().map(|&(l, t)| (l, md_slot(t))).collect(),
        };
        for (len, slot) in toks {
            if at >= end {
                break;
            }
            let e = (at + len as usize).min(end);
            match out.last_mut() {
                Some(sec) if last.is_some_and(|l| l as u8 == slot as u8) => sec.byte_range.end = ByteIndex(e - start),
                _ => out.push(LayoutSection {
                    leading_space: 0.0,
                    byte_range: ByteIndex(at - start)..ByteIndex(e - start),
                    format: self.fmts.slots[slot as usize].clone(),
                }),
            }
            last = Some(slot);
            at += len as usize;
        }
    }
}

pub fn layout(text: &str, kind: FileKind, style: &EditorStyle, wrap_width: f32) -> LayoutJob {
    layout_with(text, kind, style, wrap_width, None)
}

/// `layout`, with the fenced code blocks of a Markdown text coloured by `code` (see `codehl`).
pub fn layout_with(text: &str, kind: FileKind, style: &EditorStyle, wrap_width: f32, code: Option<&dyn CodeHighlighter>) -> LayoutJob {
    if text.len() > limit() || text.is_empty() {
        return plain_job(text, style, wrap_width);
    }
    let job = match kind {
        FileKind::Markdown => MD_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            let slot = cache.pick(text.len(), |e| e.hl.similarity(text));
            let old_len = slot.hl.text().len();
            slot.hl.update(text);
            if slot.hl.lines_redone() > 0 || slot.hl.text().len() != old_len {
                slot.lines.clear();
            }
            let toks = slot.hl.tokens();
            let spans = code.map(|c| super::codehl::spans(text, toks, |lang, body| c.get(lang, body))).unwrap_or_default();
            if spans.is_empty() {
                build_job(text, wrap_width, &formats(style), toks.iter().map(|&(l, t)| (l, md_slot(t))), toks.len())
            } else {
                let toks = with_code(toks, &spans);
                build_job(text, wrap_width, &formats(style), toks.iter().copied(), toks.len())
            }
        }),
        #[cfg(feature = "typst")]
        FileKind::Typst => TYP_CACHE.with(|cell| {
            let mut cache = cell.borrow_mut();
            let slot = cache.pick(text.len(), |e| e.hl.similarity(text));
            let old_len = slot.hl.text().len();
            let old_inc = slot.hl.incremental_updates();
            slot.hl.update(text);
            if slot.hl.children_redone() > 0 || slot.hl.incremental_updates() != old_inc || slot.hl.text().len() != old_len {
                slot.lines.clear();
                slot.hashes.clear();
            }
            let toks = slot.hl.tokens();
            build_job(text, wrap_width, &formats(style), toks.iter().map(|&(l, t)| (l, typ_slot(t))), toks.len())
        }),
        _ => return plain_job(text, style, wrap_width),
    };
    if job.sections.is_empty() {
        return plain_job(text, style, wrap_width);
    }
    let mut job = job;
    literalize(&job.text, &mut job.sections, &formats(style));
    job
}

#[cfg(feature = "typst")]
fn typ_slot(tok: iron_sheets_typst::TypTok) -> F {
    use iron_sheets_typst::TypTok as T;
    match tok {
        T::Text => F::Text,
        T::Comment => F::Comment,
        T::Punct | T::Escape => F::Marker,
        T::Strong => F::Bold,
        T::Emph => F::Italic,
        T::Link => F::WikiLink,
        T::Raw => F::Code,
        T::Label | T::Ref => F::Tag,
        T::Heading => F::H1,
        T::ListMarker => F::ListMark,
        T::MathDelim | T::MathOp | T::Interpolated => F::Math,
        T::Keyword => F::Keyword,
        T::Operator => F::Operator,
        T::Number => F::Number,
        T::String => F::Str,
        T::Function => F::Func,
        T::Error => F::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_sheets_app_core::theme_model::editor_colors;

    fn style() -> EditorStyle {
        EditorStyle::from_model(&editor_colors(false, 14.0))
    }

    /// Format of every byte, from the sections.
    fn per_byte(job: &LayoutJob) -> Vec<TextFormat> {
        let mut v = Vec::with_capacity(job.text.len());
        for s in &job.sections {
            for _ in s.byte_range.start.0..s.byte_range.end.0 {
                v.push(s.format.clone());
            }
        }
        v
    }

    fn check_job(job: &LayoutJob, text: &str) {
        assert_eq!(job.text, text);
        let mut at = 0;
        for s in &job.sections {
            assert_eq!(s.byte_range.start.0, at, "sections tile the text");
            assert!(s.byte_range.end.0 > at);
            let piece = &text.as_bytes()[at..s.byte_range.end.0];
            assert!(!piece[..piece.len() - 1].contains(&b'\n'), "a section spans a line break: {piece:?}");
            at = s.byte_range.end.0;
        }
        assert_eq!(at, text.len());
    }

    fn rich() -> String {
        let mut t = String::new();
        for i in 0..40 {
            t.push_str(&format!("# H{i}\n\nText *b* `c` [[w]] $x$\n\n```rs\nlet a = 1;\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [ ] t\n> q\n\n"));
        }
        t
    }

    /// The layout of an edited text (incremental) equals the layout of the same text highlighted from scratch.
    #[test]
    fn markdown_incremental_layout_equals_fresh() {
        let st = style();
        let mut text = rich();
        let mut seed = 12345u64;
        for i in 0..120 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let mut at = (seed >> 33) as usize % (text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            text.insert_str(at, ["a", "\n", "```\n", "$$", "---\n", "|", "*"][(seed >> 20) as usize % 7]);
            let job = layout(&text, FileKind::Markdown, &st, 500.0);
            check_job(&job, &text);
            // fresh: a text no cache matches closely (reversed order of calls through a different cache slot count is
            // not possible, so compare against the reference highlighter directly)
            let mut spans = Vec::new();
            iron_sheets_md::highlight(&text, &mut spans);
            let fm = Formats::new(&st);
            let mut want: Vec<TextFormat> = Vec::new();
            for sp in &spans {
                for _ in sp.range.clone() {
                    want.push(fm.slots[md_slot(sp.tok) as usize].clone());
                }
            }
            assert!(per_byte(&job) == want, "edit {i}");
        }
    }

    fn same_galley(a: &egui::Galley, b: &egui::Galley, ctx: &str) {
        assert_eq!(a.rows.len(), b.rows.len(), "{ctx}: row count");
        assert_eq!(a.rect, b.rect, "{ctx}: rect");
        for (i, (x, y)) in a.rows.iter().zip(&b.rows).enumerate() {
            assert_eq!(x.pos, y.pos, "{ctx}: row {i} pos");
            assert_eq!(x.ends_with_newline, y.ends_with_newline, "{ctx}: row {i}");
            assert_eq!(x.row.glyphs.len(), y.row.glyphs.len(), "{ctx}: row {i} glyphs");
            let col = |r: &egui::epaint::text::PlacedRow| r.row.visuals.mesh.vertices.iter().map(|v| (v.pos, v.color)).collect::<Vec<_>>();
            assert!(col(x) == col(y), "{ctx}: row {i} colours differ");
        }
    }

    /// The big-note path (paragraphs highlighted on demand from block states, layout reused across edits) lays out exactly
    /// what the whole-note job does, for random edits and random viewports.
    #[test]
    fn big_path_equals_whole_note_layout() {
        use super::super::paragraphs::{LineCache, View};
        let st = style();
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            for seed0 in [777u64, 4242, 99] {
                let mut text = rich();
                let mut hl = WindowHighlighter::new();
                let mut big = super::super::paragraphs::LineCache::default();
                let mut seed = seed0;
                for i in 0..150 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let mut at = (seed >> 33) as usize % (text.len() + 1);
                    while !text.is_char_boundary(at) {
                        at -= 1;
                    }
                    match (seed >> 12) % 5 {
                        0 => {
                            let mut end = (at + (seed >> 40) as usize % 200).min(text.len());
                            while !text.is_char_boundary(end) {
                                end += 1;
                            }
                            text.replace_range(at..end, "");
                        }
                        _ => text.insert_str(at, ["a", "\n", "```\n", "$$", "---\n", "|", "*", "<!--", "-->\n", "\n\n"][(seed >> 20) as usize % 10]),
                    }
                    let wrap = if i % 40 == 39 { 300.0 } else { 500.0 };
                    let dirty = hl.update(big.text(), &text);
                    let src = big_source(Some(&hl), &st);
                    // A random viewport: only part is exact; then make everything exact and compare.
                    let view = View { y: ((seed >> 5) % 4000) as f32, h: 500.0, carets: [Some(at.min(text.len())), None], target: None };
                    let partial = big.layout_big(ui, &text, wrap, &src, dirty, &view).galley;
                    let bad = partial.rows.windows(2).position(|w| (w[1].pos.y - (w[0].pos.y + w[0].row.size.y)).abs() > 1.0);
                    assert!(bad.is_none(), "seed {seed0} edit {i}: partial layout has a gap or overlap at row {bad:?}");
                    let l = big.refocus_with(ui, &View::all(), Some(&src));
                    let mine = match l {
                        Some(l) => l.galley,
                        None => big.layout_big(ui, &text, wrap, &src, 0..0, &View::all()).galley,
                    };
                    let job = layout(&text, FileKind::Markdown, &st, wrap);
                    let want = LineCache::default().layout(ui, job, &View::all()).galley;
                    same_galley(&mine, &want, &format!("seed {seed0} edit {i}"));
                }
            }
        });
        out.textures_delta.clear();
    }

    /// `cargo test -p iron-sheets-ui-egui --release --lib big_layout_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn big_layout_bench() {
        use super::super::paragraphs::{LineCache, View};
        use iron_sheets_app_core::platform::Waker;
        struct NoWake;
        impl Waker for NoWake {
            fn wake(&self) {}
        }
        let shared = crate::preview::markdown::codehl::SyntectHighlighter::shared(&(std::sync::Arc::new(NoWake) as std::sync::Arc<dyn Waker>));
        let st = style();
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            for mb in [1usize, 5] {
              for on in [false, true] {
                let code: Option<&dyn CodeHighlighter> = on.then_some(&*shared as &dyn CodeHighlighter);
                let mut text = String::new();
                while text.len() < mb << 20 {
                    text.push_str(&rich());
                }
                let mut hl = WindowHighlighter::new();
                let mut big = LineCache::default();
                let t = std::time::Instant::now();
                let dirty = hl.update(None, &text);
                let t_hl = t.elapsed();
                let src = big_source(Some(&hl), &st).with_code(code);
                let view = View { y: 100_000.0, h: 700.0, carets: [Some(text.len() / 2), None], target: None };
                let t = std::time::Instant::now();
                big.layout_big(ui, &text, 800.0, &src, dirty, &view);
                let t_open = t.elapsed();
                let mut times = Vec::new();
                let mut at = text.len() / 2;
                while !text.is_char_boundary(at) || text.as_bytes()[at] != b' ' {
                    at += 1;
                }
                for _ in 0..30 {
                    text.insert(at, 'a');
                    let t = std::time::Instant::now();
                    let dirty = hl.update(big.text(), &text);
                    let src = big_source(Some(&hl), &st).with_code(code);
                    big.layout_big(ui, &text, 800.0, &src, dirty, &view);
                    times.push(t.elapsed());
                }
                times.sort();
                // Scroll: each step moves the view by 3 screens and makes the new window exact.
                let mut scroll = Vec::new();
                for k in 0..30 {
                    let v = View { y: 120_000.0 + k as f32 * 2100.0, h: 700.0, carets: [None, None], target: None };
                    let src = big_source(Some(&hl), &st).with_code(code);
                    let t = std::time::Instant::now();
                    big.refocus_with(ui, &v, Some(&src));
                    scroll.push(t.elapsed());
                }
                scroll.sort();
                eprintln!(
                    "BIGLAYOUT {mb} MB code {on}: highlighter cold {:.1} ms, first layout {:.1} ms, keystroke (update + layout) median {:.2} ms, scroll step median {:.2} ms",
                    t_hl.as_secs_f64() * 1e3,
                    t_open.as_secs_f64() * 1e3,
                    times[15].as_secs_f64() * 1e3,
                    scroll[15].as_secs_f64() * 1e3
                );
              }
            }
        });
        out.textures_delta.clear();
    }

    #[cfg(feature = "typst")]
    #[test]
    fn typst_layout_equals_reference() {
        let st = style();
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!("= H{i}\n\nSome *strong* #f(1)[x] $a^2$\n\n```rust\nfn x() {{}}\n```\n\n#let y = {{\n  1 + 2\n}}\n\n"));
        }
        let mut seed = 99u64;
        for i in 0..60 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let mut at = (seed >> 33) as usize % (text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            text.insert_str(at, ["a", "\n", "#", "(", " ", "*"][(seed >> 20) as usize % 6]);
            let job = layout(&text, FileKind::Typst, &st, 500.0);
            check_job(&job, &text);
            let mut spans = Vec::new();
            iron_sheets_typst::highlight(&text, &mut spans);
            let fm = Formats::new(&st);
            let mut want: Vec<TextFormat> = Vec::new();
            for (r, tok) in &spans {
                for _ in r.clone() {
                    want.push(fm.slots[typ_slot(*tok) as usize].clone());
                }
            }
            assert!(per_byte(&job) == want, "edit {i}");
        }
    }

    #[test]
    fn code_blocks_get_their_class_colours_and_sections_still_tile() {
        use iron_sheets_app_core::services::{CodeHl, CodeRun, NoHighlight};
        struct Fake;
        impl CodeHighlighter for Fake {
            fn get(&self, _: &str, body: &str) -> CodeHl {
                // everything up to the first space (it may cross the line break) is a keyword
                let end = body.find(' ').unwrap_or(body.len()) as u32;
                CodeHl::Runs(vec![CodeRun { start: 0, end, class: CodeClass::Kw }].into())
            }
        }
        let st = style();
        let text = "a\n```rs\nfn\né x\n```\n";
        let job = layout_with(text, FileKind::Markdown, &st, 500.0, Some(&Fake));
        check_job(&job, text);
        let at = |b: usize| job.sections.iter().find(|s| s.byte_range.contains(&ByteIndex(b))).map(|s| s.format.color).unwrap_or_default();
        let (fn_at, x_at) = (text.find("fn").unwrap_or(0), text.find("x\n").unwrap_or(0));
        assert_eq!(at(fn_at), st.keyword);
        assert_eq!(at(x_at), st.text, "gaps between runs use the plain text colour");
        let t2 = "```rs\nplain\n```\n```zz\nplain\n```\n";
        let j2 = layout_with(t2, FileKind::Markdown, &st, 500.0, Some(&NoHighlight));
        assert!(j2.sections.iter().filter(|s| s.format.background != egui::Color32::TRANSPARENT).all(|s| s.format.expand_bg == 0.0));
        let at2 = |b: usize| j2.sections.iter().find(|s| s.byte_range.contains(&ByteIndex(b))).map(|s| s.format.color).unwrap_or_default();
        assert_eq!(at2(7), st.text, "block without runs uses the plain text colour");
        assert_eq!(at(0), st.text);
        // without a highlighter nothing changes
        assert!(per_byte(&layout(text, FileKind::Markdown, &st, 500.0)) != per_byte(&job));
    }

    #[test]
    fn two_texts_keep_their_own_cache_and_limit_still_applies() {
        let st = style();
        let a = rich();
        let b = "# other\n\ntext\n".repeat(50);
        for _ in 0..3 {
            check_job(&layout(&a, FileKind::Markdown, &st, 400.0), &a);
            check_job(&layout(&b, FileKind::Markdown, &st, 400.0), &b);
        }
        TEST_LIMIT.with(|c| c.set(Some(10)));
        let j = layout(&a, FileKind::Markdown, &st, 400.0);
        TEST_LIMIT.with(|c| c.set(None));
        assert!(j.sections.iter().all(|s| s.format.color == st.text));
    }

    fn para_ranges(text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < text.len() {
            let mut end = text[start..].find('\n').map_or(text.len(), |i| start + i);
            if end == text.len() - 1 && text.ends_with('\n') {
                end += 1;
            }
            out.push((start, end));
            start = end + 1;
        }
        out
    }

    fn job_line_sections(job: &LayoutJob, start: usize, end: usize) -> Vec<(std::ops::Range<usize>, TextFormat)> {
        if start == end {
            for s in &job.sections {
                if s.byte_range.start.0 <= start && start < s.byte_range.end.0 {
                    return vec![(0..0, s.format.clone())];
                }
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for s in &job.sections {
            if s.byte_range.end.0 <= start || s.byte_range.start.0 >= end {
                continue;
            }
            let a = s.byte_range.start.0.max(start) - start;
            let b = s.byte_range.end.0.min(end) - start;
            if a < b {
                out.push((a..b, s.format.clone()));
            }
        }
        out
    }

    fn assert_sections_match_layout_with(text: &str, kind: FileKind, st: &EditorStyle, code: Option<&dyn CodeHighlighter>, ctx: &str) {
        use super::super::paragraphs::Sections;
        let want_job = layout_with(text, kind, st, 500.0, code);
        let (eager, _) = incremental_source(text, kind, st, code);
        let lazy = incremental_source_lazy(kind, st, code);
        let mut out = Vec::new();
        for (i, (start, end)) in para_ranges(text).into_iter().enumerate() {
            let want = job_line_sections(&want_job, start, end);
            for (mode, src) in [("eager", &eager), ("lazy", &lazy)] {
                src.sections(text, start, end, &mut out);
                let got: Vec<_> = out.iter().map(|s| (s.byte_range.start.0..s.byte_range.end.0, s.format.clone())).collect();
                assert_eq!(got, want, "{ctx} ({mode}) line {i} ({start}..{end})");
            }
        }
    }

    #[test]
    fn incremental_source_sections_match_layout_with() {
        use iron_sheets_app_core::services::{CodeHl, CodeRun, NoHighlight};
        struct MultiLineCode;
        impl CodeHighlighter for MultiLineCode {
            fn get(&self, lang: &str, body: &str) -> CodeHl {
                if lang == "plain" {
                    return CodeHl::Plain;
                }
                let mid = body.len() / 2;
                let mid = (mid..=body.len()).find(|&i| body.is_char_boundary(i)).unwrap_or(body.len());
                CodeHl::Runs(vec![CodeRun { start: 0, end: mid as u32, class: CodeClass::Kw }, CodeRun { start: mid as u32, end: body.len() as u32, class: CodeClass::Comment }].into())
            }
        }
        let st = style();
        let md_cases = [
            "---\ntitle: Demo\n---\n\n# H1\n\n**b** *i* ~~s~~ `c` [[Wiki|a]] $x^2$\n\n```rs\nfn a() {}\n\nfn b() {}\n```\n\n> ```rs\n> quoted\n> ```\n\n  ```rs\n  indented\n  ```\n\n```plain\nplain\n```\n\n```rs\n```\n\n<!--\nmulti\n-->\n\n%%\nhidden\n%%\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n",
            "a\r\n```rs\r\nlet x = 1;\r\n\r\n```\r\ntail without nl",
            "```rs\nunclosed at eof\nline 2\n",
            "a\n\n",
        ];
        for (i, md) in md_cases.iter().enumerate() {
            assert_sections_match_layout_with(md, FileKind::Markdown, &st, None, &format!("md plain {i}"));
            assert_sections_match_layout_with(md, FileKind::Markdown, &st, Some(&MultiLineCode), &format!("md codehl {i}"));
            assert_sections_match_layout_with(md, FileKind::Markdown, &st, Some(&NoHighlight), &format!("md nohl {i}"));
        }

        #[cfg(feature = "typst")]
        {
            let typ_cases = [
                "= Heading\n\nSome *strong* and _emph_, `raw`, $a^2 + b^2$.\n\n#let f(x) = {\n  /* multi\n\n  comment */\n  x + 1\n}\n\n```rust\nfn main() {}\n```\n\n@ref <lab>\n\n",
                "// single\n#let unclosed = (\n  1 + 2\n",
                "a\n\n",
            ];
            for (i, typ) in typ_cases.iter().enumerate() {
                assert_sections_match_layout_with(typ, FileKind::Typst, &st, None, &format!("typ {i}"));
            }
        }
    }

    #[test]
    fn incremental_source_layout_big_equals_whole_note_across_edits() {
        use super::super::paragraphs::{LineCache, View};
        use iron_sheets_app_core::services::{CodeHl, CodeRun};
        struct Fake;
        impl CodeHighlighter for Fake {
            fn get(&self, _: &str, body: &str) -> CodeHl {
                let end = body.find(' ').unwrap_or(body.len()) as u32;
                CodeHl::Runs(vec![CodeRun { start: 0, end, class: CodeClass::Kw }].into())
            }
        }
        let st = style();
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            for (use_code, seed0) in [(false, 777u64), (true, 4242u64)] {
                let code: Option<&dyn CodeHighlighter> = use_code.then_some(&Fake as &dyn CodeHighlighter);
                let mut text = format!("---\ntitle: Test\n---\n\n{}", rich());
                let mut cache = LineCache::default();
                let mut seed = seed0;
                for i in 0..120 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let mut at = (seed >> 33) as usize % (text.len() + 1);
                    while !text.is_char_boundary(at) {
                        at -= 1;
                    }
                    match (seed >> 12) % 5 {
                        0 => {
                            let mut end = (at + (seed >> 40) as usize % 200).min(text.len());
                            while !text.is_char_boundary(end) {
                                end += 1;
                            }
                            text.replace_range(at..end, "");
                        }
                        _ => text.insert_str(at, ["a", "\n", "```\n", "$$", "---\n", "|", "*", "<!--", "-->\n", "\n\n"][(seed >> 20) as usize % 10]),
                    }
                    let wrap = if i % 40 == 39 { 300.0 } else { 500.0 };
                    let (src, dirty) = incremental_source(&text, FileKind::Markdown, &st, code);
                    let view = View { y: ((seed >> 5) % 4000) as f32, h: 500.0, carets: [Some(at.min(text.len())), None], target: None };
                    let partial = cache.layout_big(ui, &text, wrap, &src, dirty, &view).galley;
                    let bad = partial.rows.windows(2).position(|w| (w[1].pos.y - (w[0].pos.y + w[0].row.size.y)).abs() > 1.0);
                    assert!(bad.is_none(), "code={use_code} seed={seed0} edit={i}: partial layout overlap at {bad:?}");
                    let lazy = incremental_source_lazy(FileKind::Markdown, &st, code);
                    let mine = match cache.refocus_with(ui, &View::all(), Some(&lazy)) {
                        Some(l) => l.galley,
                        None => cache.layout_big(ui, &text, wrap, &lazy, 0..0, &View::all()).galley,
                    };
                    let job = layout_with(&text, FileKind::Markdown, &st, wrap, code);
                    let want = LineCache::default().layout(ui, job, &View::all()).galley;
                    same_galley(&mine, &want, &format!("md incremental code={use_code} seed={seed0} edit={i}"));
                }
            }
        });
        out.textures_delta.clear();
    }

    #[cfg(feature = "typst")]
    #[test]
    fn incremental_source_typst_layout_big_equals_whole_note_across_edits() {
        use super::super::paragraphs::{LineCache, View};
        let st = style();
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0))), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            let mut text = String::new();
            for i in 0..40 {
                text.push_str(&format!("= H{i}\n\nSome *strong* #f(1)[x] $a^2$\n\n```rust\nfn x() {{}}\n```\n\n#let y = {{\n  /* c */\n  1 + 2\n}}\n\n"));
            }
            let mut cache = LineCache::default();
            let mut seed = 1337u64;
            for i in 0..60 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let mut at = (seed >> 33) as usize % (text.len() + 1);
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                match (seed >> 12) % 5 {
                    0 => {
                        let mut end = (at + (seed >> 40) as usize % 80).min(text.len());
                        while !text.is_char_boundary(end) {
                            end += 1;
                        }
                        text.replace_range(at..end, "");
                    }
                    _ => text.insert_str(at, ["a", "\n", "#", "(", "/*", "*/", "```\n", "*"][(seed >> 20) as usize % 8]),
                }
                let wrap = if i % 25 == 24 { 320.0 } else { 500.0 };
                let (src, dirty) = incremental_source(&text, FileKind::Typst, &st, None);
                let view = View { y: ((seed >> 5) % 3000) as f32, h: 500.0, carets: [Some(at.min(text.len())), None], target: None };
                let partial = cache.layout_big(ui, &text, wrap, &src, dirty, &view).galley;
                let bad = partial.rows.windows(2).position(|w| (w[1].pos.y - (w[0].pos.y + w[0].row.size.y)).abs() > 1.0);
                assert!(bad.is_none(), "typst edit={i}: partial layout overlap at {bad:?}");
                let lazy = incremental_source_lazy(FileKind::Typst, &st, None);
                let mine = match cache.refocus_with(ui, &View::all(), Some(&lazy)) {
                    Some(l) => l.galley,
                    None => cache.layout_big(ui, &text, wrap, &lazy, 0..0, &View::all()).galley,
                };
                let job = layout(&text, FileKind::Typst, &st, wrap);
                let want = LineCache::default().layout(ui, job, &View::all()).galley;
                same_galley(&mine, &want, &format!("typ incremental edit={i}"));
            }
        });
        out.textures_delta.clear();
    }

    #[test]
    fn split_points_only_between_ligature_characters() {
        assert_eq!(split_points("a -> b", 0..6), vec![3]);
        assert_eq!(split_points("a<=>b", 0..5), vec![2, 3]);
        assert!(split_points("a - b >", 0..7).is_empty(), "single characters stay");
        assert_eq!(split_points("->=", 1..3), vec![2], "range ends are not split");
        assert_eq!(split_points("é->", 0..4), vec![3], "non-ASCII neighbours are fine");
        assert!(split_points("", 0..0).is_empty());
    }

    fn sections_of(text: &str, st: &EditorStyle) -> Vec<(String, bool)> {
        let job = layout(text, FileKind::Markdown, st, 800.0);
        check_job(&job, text);
        job.sections.iter().map(|s| (text[s.byte_range.start.0..s.byte_range.end.0].to_string(), s.format.background != Color32::TRANSPARENT)).collect()
    }

    #[test]
    fn literal_code_splits_only_code_and_never_merges_back() {
        let mut st = style();
        st.font_ligates = true;
        let text = "a -> b `x->y` \n\n```\n=>\n```\n";
        let has = |v: &[(String, bool)], piece: &str| v.iter().any(|(t, _)| t == piece);
        // Default: prose keeps `->` together, code is split.
        let v = sections_of(text, &st);
        assert!(has(&v, "a -> b "), "{v:?}");
        assert!(has(&v, "`x-") && has(&v, ">y`") && has(&v, "=") && has(&v, ">\n"), "{v:?}");
        // In code too: nothing is split.
        st.ligatures_in_code = true;
        let v = sections_of(text, &st);
        assert!(has(&v, "`x->y`") && has(&v, "=>\n"), "{v:?}");
        // Off: prose is split as well.
        st.ligatures = false;
        st.ligatures_in_code = true;
        let v = sections_of(text, &st);
        assert!(has(&v, "a -") && has(&v, "> b "), "{v:?}");
        assert!(v.len() > 8);
        // A theme without a code background still marks code.
        let mut st = style();
        st.font_ligates = true;
        st.code_bg = Color32::TRANSPARENT;
        let v = sections_of(text, &st);
        assert!(has(&v, "a -> b ") && has(&v, "`x-") && has(&v, ">y`"), "{v:?}");
    }

    #[test]
    fn fonts_without_ligatures_are_never_split() {
        let mut st = style();
        assert!(!st.font_ligates);
        let text = "a -> b `x->y` <=>\n\n```\n=>\n```\n";
        let base = sections_of(text, &st);
        st.ligatures = false;
        assert_eq!(sections_of(text, &st), base);
    }

    /// With Lilex the arrow really is drawn as a ligature (other glyph bitmaps than the literal split), one glyph per char.
    #[test]
    #[cfg(feature = "font-lilex")]
    fn lilex_arrow_is_a_ligature_unless_split() {
        let ctx = egui::Context::default();
        let choice = crate::theme::FontChoice { editor: "lilex".into(), ..Default::default() };
        assert!(crate::theme::install_fonts(&ctx, &choice).is_empty());
        let fam = FontFamily::Name("editor".into());
        let fmt = TextFormat::simple(FontId::new(40.0, fam), Color32::WHITE);
        let text = "a -> b";
        let one = LayoutJob::single_section(text.into(), fmt.clone());
        let mut split = LayoutJob { text: text.into(), ..Default::default() };
        for (a, b) in [(0, 3), (3, 4), (4, 6)] {
            split.sections.push(LayoutSection { leading_space: 0.0, byte_range: ByteIndex(a)..ByteIndex(b), format: fmt.clone() });
        }
        assert_eq!(split.sections.len(), 3);
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 200.0))), ..Default::default() };
        let lay = |ui: &mut egui::Ui| {
            let (g1, g2) = ui.fonts_mut(|f| (f.layout_job(one.clone()), f.layout_job(split.clone())));
            let a = &g1.rows[0].glyphs;
            let b = &g2.rows[0].glyphs;
            assert_eq!(a.len(), text.chars().count(), "one glyph per char");
            assert_eq!(b.len(), a.len());
            let xs: Vec<f32> = a.iter().map(|g| g.pos.x).collect();
            assert!(xs.windows(2).all(|w| w[0] < w[1]), "caret positions grow: {xs:?}");
            let (l, r) = (&a[2..4], &b[2..4]);
            assert_ne!([l[0].uv_rect.size, l[1].uv_rect.size], [r[0].uv_rect.size, r[1].uv_rect.size], "the ligature draws other shapes");
            assert_eq!(a[0].uv_rect.size, b[0].uv_rect.size, "`a` is the same");
        };
        // Fonts set with `set_fonts` apply at the start of the next pass.
        ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
        ctx.run_ui(input, lay).textures_delta.clear();
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use iron_sheets_app_core::theme_model::editor_colors;
    use std::sync::Arc;
    use std::time::Instant;

    fn md_note() -> String {
        let mut s = String::new();
        let mut i = 0;
        while s.len() < 100 * 1024 {
            s.push_str(&format!("## Section {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, with inline math $x^{i} + y$ and a [link](https://example.org).\n\n- item one {i}\n- [ ] task {i}\n\n> quote {i} with a little more text in it\n\n```rust\nfn f{i}() {{ let a = {i}; }}\n```\n\n"));
            i += 1;
        }
        s
    }

    #[cfg(feature = "typst")]
    fn typ_note() -> String {
        let mut s = String::new();
        let mut i = 0;
        while s.len() < 100 * 1024 {
            s.push_str(&format!("= Heading {i}\n\nSome *strong* and _emph_ text with `raw` and a #link(\"https://x.org\")[link], a label <lab-{i}> and a ref @lab-{i}.\n\n- item one\n- item two with $a^2 + b^2 = c^2$\n\n#let f(x, y: 2) = {{\n  let z = x + y * 3\n  if z > 4 {{ [big #z] }} else {{ [small] }}\n}}\n\n#f(3, y: 5)\n\nPlain paragraph text, long enough to be realistic: lorem ipsum dolor sit amet {i}.\n\n"));
            i += 1;
        }
        s
    }

    fn med(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    /// Cost of the literal splitting: sections and job build time (no shaping) of a 100 KB note, and one shaped paragraph.
    #[test]
    fn ligature_split_cost() {
        let text = md_note().replace("`code`,", "`a->b => c`, | --- | -->").replace("quote", "quote ->");
        let para = "Paragraph: a -> b => c != d with `x->y` and [[Ideas]] https://example.org/a-b, again -> and => here.";
        let ctx = egui::Context::default();
        let choice = crate::theme::FontChoice { editor: "lilex".into(), ..Default::default() };
        crate::theme::install_fonts(&ctx, &choice);
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))), ..Default::default() };
        ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
        let mut shaped = [0.0f64; 3];
        let modes = [("all ligatures", true, true), ("default (not in code)", true, false), ("off", false, false)];
        for (k, (label, lig, code)) in modes.into_iter().enumerate() {
            let mut st = EditorStyle::from_model(&editor_colors(false, 14.0));
            (st.ligatures, st.ligatures_in_code, st.font_ligates) = (lig, code, true);
            st.font_id.family = FontFamily::Name("editor".into());
            let mut v = Vec::new();
            let mut n = 0;
            for _ in 0..30 {
                let t = Instant::now();
                let j = layout(&text, FileKind::Markdown, &st, 800.0);
                v.push(t.elapsed().as_secs_f64() * 1e3);
                n = j.sections.len();
            }
            let mut sh = Vec::new();
            ctx.run_ui(input.clone(), |ui| {
                for i in 0..200 {
                    let job = layout(&format!("{para} {i}"), FileKind::Markdown, &st, 800.0);
                    let t = Instant::now();
                    std::hint::black_box(ui.fonts_mut(|f| f.layout_job(job)));
                    sh.push(t.elapsed().as_secs_f64() * 1e3);
                }
            })
            .textures_delta
            .clear();
            shaped[k] = med(sh);
            eprintln!("LIG {label}: {n} sections, job build median {:.3} ms, shaped paragraph median {:.4} ms", med(v), shaped[k]);
        }
    }

    fn run(label: &str, kind: FileKind, mut text: String, anchor: &str) {
        let st = EditorStyle::from_model(&editor_colors(false, 14.0));
        let t = Instant::now();
        let job = layout(&text, kind, &st, 800.0);
        let open = t.elapsed().as_secs_f64() * 1e3;
        let sections = job.sections.len();
        let mut v = Vec::new();
        let mut seed = 0x9E3779B97F4A7C15u64;
        for _ in 0..300 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            // type inside prose: after the anchor text at a random occurrence
            let occ: Vec<usize> = text.match_indices(anchor).map(|(k, m)| k + m.len()).collect();
            let at = occ[(seed >> 8) as usize % occ.len()];
            text.insert(at, 'x');
            let t = Instant::now();
            let j = layout(&text, kind, &st, 800.0);
            v.push(t.elapsed().as_secs_f64() * 1e3);
            std::hint::black_box(j);
        }
        eprintln!("HLBENCH {label}: {} bytes, {sections} sections; first layout {open:.3} ms; per keystroke layout median {:.3} ms", text.len(), med(v));
    }

    #[test]
    #[ignore]
    fn layout_bench() {
        run("markdown", FileKind::Markdown, md_note(), "Lorem ipsum dolor sit amet, ");
        #[cfg(feature = "typst")]
        run("typst", FileKind::Typst, typ_note(), "lorem ipsum dolor sit amet");
    }

    /// Keystroke layout with and without the code highlighter (20 rust blocks in 100 KB).
    /// `cargo test -p iron-sheets-ui-egui --release --lib codehl_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn codehl_bench() {
        use iron_sheets_app_core::platform::Waker;
        use iron_sheets_app_core::services::CodeHighlighter;
        struct NoWake;
        impl Waker for NoWake {
            fn wake(&self) {}
        }
        let note = {
            let mut s = md_note();
            for i in 0..20 {
                let at = s.len() / 21 * (i + 1);
                let at = s[at..].find("\n\n").map_or(s.len(), |k| at + k + 2);
                s.insert_str(at, "```rust\nfn main() {\n    let s = \"hi\"; // c\n    println!(\"{}\", 42);\n}\n```\n\n");
            }
            s
        };
        let hl = crate::preview::markdown::codehl::SyntectHighlighter::shared(&(Arc::new(NoWake) as Arc<dyn Waker>));
        let st = EditorStyle::from_model(&editor_colors(false, 14.0));
        let _ = layout_with(&note, FileKind::Markdown, &st, 800.0, Some(&*hl));
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for (label, code) in [("off", None), ("on", Some(&*hl as &dyn CodeHighlighter))] {
            let mut text = note.clone();
            let mut v = Vec::new();
            for _ in 0..300 {
                let at = text.find("Lorem ipsum").map_or(0, |k| k + 5);
                text.insert(at, 'x');
                let t = Instant::now();
                std::hint::black_box(layout_with(&text, FileKind::Markdown, &st, 800.0, code));
                v.push(t.elapsed().as_secs_f64() * 1e3);
            }
            eprintln!("CODEHL keystroke layout {label}: median {:.3} ms", med(v));
        }
    }
}
