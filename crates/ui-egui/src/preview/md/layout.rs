//! Block layout for the Markdown preview: turns one parsed block into galleys and geometry.
//! Everything here is position independent so layouts can be cached by block hash.
use super::diagram;
use super::embed::{EmbedCache, Mode, Part, parse_target, slice};
#[cfg(feature = "excalidraw")]
use super::images::ExcalidrawEmbedJob;
use super::images::{ImageCache, ImgInfo};
use super::mermaid;
use crate::services::{CodeHighlighter, CodeHl, CodeRun, MathBitmap, MathRenderer, MathResult};
use crate::theme::{PreviewStyle, families};
use egui::epaint::text::Galley;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, Context, FontFamily, FontId, Rect, Stroke, Vec2, pos2, vec2};
use iron_sheets_app_core::docs::DocStore;
use iron_sheets_core::FileKind;
use iron_sheets_core::index::Index;
use iron_sheets_md::model::*;
use parking_lot::RwLock;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const NBSP: char = '\u{A0}';
pub const OBJ: char = '\u{FFFC}';
pub const CODE_PAD: f32 = 8.0;
pub const TABLE_PAD_X: f32 = 8.0;
pub const TABLE_PAD_Y: f32 = 4.0;
const TABLE_COL_CAP: f32 = 320.0;

/// Everything layout needs from the outside world.
pub struct Env<'a> {
    pub ctx: &'a Context,
    pub style: &'a PreviewStyle,
    pub math: &'a dyn MathRenderer,
    pub hl: &'a dyn CodeHighlighter,
    pub index: &'a Arc<RwLock<Index>>,
    pub note: &'a Path,
    pub images: &'a mut ImageCache,
    pub mermaid: bool,
    /// "Re-render on save" and the note has unsaved changes: edited diagrams keep their last render.
    pub mermaid_hold: bool,
    /// Index of the block being laid out among the note's Mermaid blocks (see `mermaid_ordinal`).
    pub mermaid_ordinal: u32,
    pub nbsp_ratio: f32,
    pub ppp: f32,
    pub embeds: &'a mut EmbedCache,
    pub open: &'a DocStore,
    /// The notes this layout is embedded in, outermost first (empty for the note itself).
    pub stack: Vec<PathBuf>,
    /// Blocks the embed being laid out may still add (shared with its nested embeds): depth times width must not
    /// multiply into a huge layout.
    pub budget: &'a mut u32,
}

#[derive(Debug, Clone)]
pub enum LinkAction {
    Target(LinkTarget),
    /// Hover text only (math errors).
    Tip,
}

#[derive(Debug, Clone)]
pub struct LinkSpan {
    /// Char indices in the galley text.
    pub chars: Range<usize>,
    pub action: LinkAction,
    pub tip: String,
}

#[derive(Debug, Clone)]
pub enum SlotContent {
    Math(Arc<MathBitmap>),
    Image(PathBuf),
    #[cfg(feature = "excalidraw")]
    Excalidraw { tex_key: PathBuf, target: PathBuf },
    /// Nothing to paint: a displayed glyph that stands for typed source (`→` for `->`), see [`arrows`].
    Text,
}

/// A reserved gap (NBSP) in a galley that is painted over with a texture.
#[derive(Debug, Clone)]
pub struct Slot {
    pub ch: usize,
    pub w: f32,
    pub h: f32,
    /// Distance from the top of the object to its baseline.
    pub baseline: f32,
    /// Height and baseline-in-row of the surrounding text (for baseline alignment).
    pub row_l: f32,
    pub row_a: f32,
    pub content: SlotContent,
    /// Plain text of the object for copying (`$source$`, alt text).
    pub text: String,
}

impl Slot {
    /// The char `ch` of the galley shows `text` (the typed source) in another form.
    fn typed(ch: usize, text: &str) -> Slot {
        Slot { ch, w: 0.0, h: 0.0, baseline: 0.0, row_l: 0.0, row_a: 0.0, content: SlotContent::Text, text: text.into() }
    }
}

pub fn obj_text(k: &ObjectKind) -> String {
    match k {
        ObjectKind::Math { source } => format!("${source}$"),
        ObjectKind::Image { src, alt, .. } => (if alt.is_empty() { src } else { alt }).clone(),
    }
}

pub struct TextBox {
    pub galley: Arc<Galley>,
    pub links: Vec<LinkSpan>,
    pub slots: Vec<Slot>,
    /// Keycap boxes (`<kbd>`), one per row of a run, relative to the galley origin; painted behind the text.
    pub kbds: Vec<Kbd>,
}

#[derive(Debug, Clone)]
pub struct Kbd {
    pub rect: Rect,
    pub fill: Color32,
    pub border: Color32,
}

/// Space kept free on each side of a keycap by the section after / at it.
const KBD_LEAD: f32 = 6.0;
/// How far the box reaches past the glyphs sideways.
const KBD_PAD: f32 = 4.0;

/// One box per row for every char range in `runs`, from the glyph metrics of the laid-out galley.
fn kbd_boxes(g: &Galley, runs: &[(Range<usize>, Color32, Color32)]) -> Vec<Kbd> {
    let mut out = Vec::new();
    let mut base = 0usize;
    for row in &g.rows {
        for (r, fill, border) in runs {
            let mut rect: Option<Rect> = None;
            for (k, gl) in row.glyphs.iter().enumerate() {
                if !r.contains(&(base + k)) {
                    continue;
                }
                let top = row.pos.y + gl.pos.y - gl.font_ascent - 1.0;
                let b = Rect::from_min_size(pos2(row.pos.x + gl.pos.x, top), vec2(gl.advance_width, gl.font_height + 2.0));
                rect = Some(rect.map_or(b, |u| u.union(b)));
            }
            if let Some(rect) = rect {
                // The pad must not stick out of the text box where the keycap starts or ends a wrapped row.
                let r = rect.expand2(vec2(KBD_PAD, 0.0));
                let rect = Rect::from_x_y_ranges(r.min.x.max(0.0)..=r.max.x.min(g.size().x), r.y_range());
                out.push(Kbd { rect, fill: *fill, border: *border });
            }
        }
        base += row.char_count_including_newline().0;
    }
    out
}

impl TextBox {
    pub fn size(&self) -> Vec2 {
        self.galley.size()
    }
}

pub struct TableLayout {
    pub cols: Vec<f32>,
    pub rows_h: Vec<f32>,
    /// Row-major, header row first, `cols.len()` per row.
    pub cells: Vec<TextBox>,
    pub aligns: Vec<Align>,
    pub total_w: f32,
}

pub enum Body {
    Text { tb: TextBox, rule_below: bool },
    Code { galley: Arc<Galley>, lang: String, text_w: f32 },
    Table(TableLayout),
    Rule,
    Math { bm: Arc<MathBitmap>, w: f32, h: f32 },
    Image { path: PathBuf, size: Vec2 },
    Diagram(Box<DiagramBody>),
    #[cfg(feature = "excalidraw")]
    Excalidraw(Box<ExcalidrawBody>),
    Embed(Box<EmbedLayout>),
}

/// An embedded Excalidraw drawing in a Markdown note.
#[cfg(feature = "excalidraw")]
pub struct ExcalidrawBody {
    pub target: PathBuf,
    pub path: PathBuf,
    pub slot: u64,
    pub key: PathBuf,
    pub size: Vec2,
    pub err: Option<Arc<Galley>>,
}

/// A rendered Mermaid diagram: the picture, invisible text boxes over it (position relative to the picture) and the
/// error of a newer source that failed, shown below the last good picture.
pub struct DiagramBody {
    pub path: PathBuf,
    /// The block's slot in the image cache (what the viewers follow).
    pub slot: u64,
    /// The slot's key when this was laid out: a cached layout is stale once the slot moved on (see `diagrams_current`).
    pub key: PathBuf,
    pub size: Vec2,
    pub texts: Vec<(Vec2, TextBox)>,
    /// Node boxes of the linked labels (relative to the picture) with the label's index in `texts`.
    pub shapes: Vec<(Rect, usize)>,
    pub err: Option<Arc<Galley>>,
}

/// Position of block `i` among the Mermaid blocks of `doc`: the identity of a diagram while its source is edited.
pub fn mermaid_ordinal(doc: &Document, i: usize) -> u32 {
    let is = |b: &Block| matches!(&b.kind, BlockKind::Code { lang, .. } if lang.eq_ignore_ascii_case("mermaid"));
    if doc.blocks.get(i).is_some_and(is) { doc.blocks[..i].iter().filter(|b| is(b)).count() as u32 } else { 0 }
}

pub const EMBED_PAD: f32 = 8.0;
/// An inline embed stops adding blocks once it is this tall, so a huge note costs a bounded layout.
const EMBED_MAX_H: f32 = 420.0;
/// Blocks one embed, nested embeds included, may lay out.
const EMBED_MAX_BLOCKS: u32 = 64;

/// A framed box with the embedded blocks. `items` are (block index in `doc`, top below the title row, layout).
pub struct EmbedLayout {
    pub doc: Arc<Document>,
    /// The embedded note (links and tasks inside it belong to it, not to the host).
    pub path: Arc<PathBuf>,
    pub title: TextBox,
    pub items: Vec<(usize, f32, BlockLayout)>,
    /// "Show more" row below the items when the note is longer than the cap.
    pub more: Option<TextBox>,
    /// Y of `more` below the title row.
    pub more_y: f32,
    /// Block hash of a collapsible embed (the title arrow toggles it).
    pub toggle: Option<u64>,
}

pub struct BlockLayout {
    /// Total height: alert titles plus body.
    pub height: f32,
    pub head_h: f32,
    pub indent: f32,
    pub body_h: f32,
    pub first_line_h: f32,
    pub titles: Vec<(usize, Arc<Galley>, Color32)>,
    pub body: Body,
    /// Waiting for math or image information: layout again once it arrives.
    pub pending: bool,
}

impl BlockLayout {
    /// Whether every diagram in this layout is still its slot's current one. A layout cached by content outlives the
    /// slot's edits: after an edit and a revert, the cached layout would paint a picture the edit already released.
    pub fn diagrams_current(&self, images: &ImageCache) -> bool {
        match &self.body {
            Body::Diagram(d) => images.slot_key(d.slot) == Some(d.key.as_path()),
            #[cfg(feature = "excalidraw")]
            Body::Excalidraw(e) => images.slot_key(e.slot) == Some(e.key.as_path()),
            Body::Embed(e) => e.items.iter().all(|(_, _, l)| l.diagrams_current(images)),
            _ => true,
        }
    }
}

/// Horizontal offset of a table cell's text inside its column, from the column alignment.
pub fn cell_dx(t: &TableLayout, c: usize, tb: &TextBox) -> f32 {
    let inner = t.cols[c] - 2.0 * TABLE_PAD_X;
    match t.aligns[c] {
        Align::Center => ((inner - tb.size().x) / 2.0).max(0.0),
        Align::Max => (inner - tb.size().x).max(0.0),
        _ => 0.0,
    }
}

pub fn container_indent(c: &Container) -> f32 {
    match &c.kind {
        ContainerKind::Quote | ContainerKind::Alert { .. } => 18.0,
        ContainerKind::ListItem { ordered: Some(_), .. } => 28.0,
        ContainerKind::ListItem { .. } => 24.0,
        ContainerKind::Footnote { .. } => 28.0,
    }
}

pub fn total_indent(cs: &[Container]) -> f32 {
    cs.iter().map(container_indent).sum()
}

pub fn alert_color(kind: &AlertKind, dark: bool) -> Color32 {
    let (l, d) = match kind {
        AlertKind::Note => ([9, 105, 218], [88, 166, 255]),
        AlertKind::Tip => ([26, 127, 55], [63, 185, 80]),
        AlertKind::Important => ([130, 80, 223], [188, 140, 255]),
        AlertKind::Warning => ([154, 103, 0], [210, 153, 34]),
        AlertKind::Caution => ([207, 34, 46], [248, 81, 73]),
        AlertKind::Other(_) => ([90, 100, 120], [140, 150, 170]),
    };
    let v = if dark { d } else { l };
    Color32::from_rgb(v[0], v[1], v[2])
}

pub fn alert_title(kind: &AlertKind, title: &Option<String>) -> String {
    if let Some(t) = title.as_ref().filter(|t| !t.is_empty()) {
        return t.clone();
    }
    let name = match kind {
        AlertKind::Note => "Note",
        AlertKind::Tip => "Tip",
        AlertKind::Important => "Important",
        AlertKind::Warning => "Warning",
        AlertKind::Caution => "Caution",
        AlertKind::Other(o) => o.as_str(),
    };
    let mut cs = name.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

fn heading_scale(level: u8) -> f32 {
    [2.0, 1.6, 1.3, 1.15, 1.0, 0.9][(level.clamp(1, 6) - 1) as usize]
}

fn color_rgba(c: Color32) -> [u8; 4] {
    c.to_srgba_unmultiplied()
}

fn dim(c: Color32, dark: bool) -> Color32 {
    let t = if dark { Color32::from_gray(40) } else { Color32::from_gray(255) };
    c.lerp_to_gamma(t, 0.45)
}

pub struct TextOpts {
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Color32,
    /// `f32::INFINITY` for no wrapping.
    pub width: f32,
    /// Allow math objects and images.
    pub objects: bool,
}

fn family(bold: bool, italic: bool) -> FontFamily {
    let f = families();
    match (bold, italic) {
        (false, false) => f.preview,
        (true, false) => f.preview_bold,
        (false, true) => f.preview_italic,
        (true, true) => f.preview_bold_italic,
    }
}

fn mono_family(style: &PreviewStyle, bold: bool, italic: bool) -> FontFamily {
    let f = families();
    match (bold, italic) {
        (false, false) => style.code_family.clone(),
        (true, false) => f.mono_bold,
        (false, true) => f.mono_italic,
        (true, true) => f.mono_bold_italic,
    }
}

pub fn layout_job(ctx: &Context, job: LayoutJob) -> Arc<Galley> {
    ctx.fonts_mut(|f| f.layout_job(job))
}

/// Longest first. `<=`/`>=` are in: in prose they are comparisons. `==` and `...` stay as typed.
const ARROWS: [(&str, &str); 8] = [("<=>", "⇔"), ("<->", "↔"), ("->", "→"), ("<-", "←"), ("=>", "⇒"), ("<=", "≤"), (">=", "≥"), ("!=", "≠")];

/// Arrow and operator sequences of `text` with the glyph that replaces each (byte range, glyph). A sequence touching
/// another operator character (`-->`, `==>`, `!==`) stays as typed.
pub fn arrows(text: &str) -> Vec<(Range<usize>, &'static str)> {
    const OPS: &[u8] = b"-<>=!|~";
    let b = text.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let hit = if OPS.contains(&b[i]) { ARROWS.iter().find(|(p, _)| text[i..].starts_with(p)) } else { None };
        match hit {
            Some((p, g)) if (i == 0 || !OPS.contains(&b[i - 1])) && b.get(i + p.len()).is_none_or(|n| !OPS.contains(n)) => {
                out.push((i..i + p.len(), *g));
                i += p.len();
            }
            _ => i += 1,
        }
    }
    out
}

fn push(job: &mut LayoutJob, text: &str, fmt: TextFormat, chars: &mut usize) {
    if text.is_empty() {
        return;
    }
    *chars += text.chars().count();
    job.append(text, 0.0, fmt);
}

/// What `build_text` is filling: the job, the galley char count and the slots for substituted glyphs.
struct Out<'a> {
    job: &'a mut LayoutJob,
    chars: &'a mut usize,
    slots: &'a mut Vec<Slot>,
}

impl Out<'_> {
    fn text(&mut self, text: &str, fmt: &TextFormat) {
        push(self.job, text, fmt.clone(), self.chars);
    }
}

/// Appends the text of one span. Code in an editor face that must stay literal is split between adjacent ligature
/// characters (sections pushed directly: `LayoutJob::append` would merge equal formats back); prose (`subs`) shows
/// arrows as glyphs, each with a slot that gives the typed characters back on copy.
fn push_seg(env: &Env, out: &mut Out, text: &str, fmt: &TextFormat, sp: &Span, subs: bool) {
    if sp.style.contains(Style::CODE) || sp.style.contains(Style::KBD) {
        // Plain Monospace (DejaVu) has no ligatures; only the editor family and its bold/italic faces can ligate.
        if !env.style.split_code || fmt.font_id.family == FontFamily::Monospace {
            return out.text(text, fmt);
        }
        let mut at = 0;
        for p in crate::editor::highlight::split_points(text, 0..text.len()).into_iter().chain([text.len()]) {
            let start = out.job.text.len();
            out.job.text.push_str(&text[at..p]);
            out.job.sections.push(egui::text::LayoutSection {
                leading_space: 0.0,
                byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(out.job.text.len()),
                format: fmt.clone(),
            });
            *out.chars += text[at..p].chars().count();
            at = p;
        }
    } else if subs && sp.link.is_none() && !sp.style.contains(Style::DIM) {
        let mut at = 0;
        for (r, g) in arrows(text) {
            out.text(&text[at..r.start], fmt);
            out.slots.push(Slot::typed(*out.chars, &text[r.clone()]));
            out.text(g, fmt);
            at = r.end;
        }
        out.text(&text[at..], fmt);
    } else {
        out.text(text, fmt);
    }
}

fn span_format(env: &Env, style: Style, o: &TextOpts, link: Option<(&LinkTarget, bool)>) -> TextFormat {
    let st = env.style;
    let bold = o.bold || style.contains(Style::BOLD);
    let italic = o.italic || style.contains(Style::ITALIC);
    let mut size = o.size;
    let mut color = o.color;
    let mut f = TextFormat { font_id: FontId::new(size, family(bold, italic)), color, valign: Align::Center, ..Default::default() };
    if style.contains(Style::CODE) {
        size = o.size * 0.9;
        color = if st.dark { Color32::from_rgb(235, 150, 165) } else { Color32::from_rgb(170, 40, 70) };
        f.font_id = FontId::new(size, mono_family(st, bold, italic));
        f.background = st.code_bg;
        f.expand_bg = 1.0;
    }
    if style.contains(Style::KBD) {
        size = o.size * 0.88;
        f.font_id = FontId::new(size, mono_family(st, bold, false));
    }
    if style.contains(Style::HIGHLIGHT) {
        f.background = if st.dark { Color32::from_rgb(110, 95, 20) } else { Color32::from_rgb(255, 240, 130) };
    }
    if style.contains(Style::TAG) {
        color = st.link;
        f.background = if st.dark { Color32::from_rgb(40, 55, 80) } else { Color32::from_rgb(225, 235, 250) };
    }
    if style.contains(Style::DIM) {
        color = dim(color, st.dark);
    }
    if style.contains(Style::SUP) || style.contains(Style::SUB) {
        size *= 0.75;
        f.font_id = FontId::new(size, f.font_id.family.clone());
        f.valign = if style.contains(Style::SUP) { Align::Min } else { Align::Max };
    }
    if style.contains(Style::STRIKE) {
        f.strikethrough = Stroke::new(1.0, color);
    }
    if style.contains(Style::UNDERLINE) {
        f.underline = Stroke::new(1.0, color);
    }
    if let Some((t, unresolved)) = link {
        // Unresolved wiki links stay in the (AA-contrast) link colour and are italic: not colour alone.
        color = st.link;
        if unresolved && matches!(t, LinkTarget::Wiki { .. }) {
            f.italics = true;
        }
        f.underline = Stroke::new(1.0, color);
    }
    f.color = color;
    f
}

/// (row height, baseline y inside the row) of plain text at `size`.
fn probe(env: &Env, size: f32) -> (f32, f32) {
    let job = LayoutJob::single_section("x".into(), TextFormat::simple(FontId::new(size, family(false, false)), Color32::WHITE));
    let g = layout_job(env.ctx, job);
    match g.rows.first() {
        Some(r) => (r.size.y, r.glyphs.first().map_or(r.size.y * 0.8, |gl| gl.pos.y)),
        None => (size * 1.2, size),
    }
}

/// Appends a gap of `w` x `h` whose baseline sits `b` below its top.
fn push_gap(job: &mut LayoutJob, env: &Env, o: &TextOpts, w: f32, h: f32, b: f32, chars: &mut usize) -> (f32, f32) {
    let (l, a) = probe(env, o.size);
    // Row height such that the object (b above the baseline, h - b below) fits when text is centred.
    let rh = l.max(l + 2.0 * (b - a)).max(2.0 * (h - b) + 2.0 * a - l);
    let ratio = env.nbsp_ratio.max(0.05);
    let fmt = TextFormat {
        font_id: FontId::new((w / ratio).max(0.5), family(false, false)),
        color: Color32::TRANSPARENT,
        line_height: Some(rh),
        valign: Align::Center,
        ..Default::default()
    };
    job.append(&NBSP.to_string(), 0.0, fmt);
    *chars += 1;
    (l, a)
}

/// Resolve an image reference to a local file; `None` for remote URLs.
pub fn resolve_image(env: &Env, src: &str, wiki: bool) -> Option<PathBuf> {
    let s = src.trim();
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("data:") {
        return None;
    }
    if wiki {
        let r = env.index.try_read().and_then(|ix| ix.resolve_file(s, Some(env.note)));
        if let Some(p) = r {
            return Some(p);
        }
    }
    let dec = percent_decode(s.trim_start_matches('<').trim_end_matches('>'));
    let rel = Path::new(&dec);
    let base = env.note.parent().unwrap_or_else(|| Path::new(""));
    Some(base.join(rel))
}

pub fn percent_decode(s: &str) -> String {
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
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn wiki_resolved(env: &Env, note: &str) -> bool {
    if note.is_empty() {
        return true;
    }
    match env.index.try_read() {
        Some(ix) => ix.resolve(note, Some(env.note)).is_some(),
        None => true,
    }
}

/// Build one galley from inline content.
pub fn build_text(env: &mut Env, inl: &Inlines, o: &TextOpts, pending: &mut bool) -> TextBox {
    let subs = env.style.subs;
    build_text_with(env, inl, o, pending, subs)
}

/// `subs`: show arrows as glyphs (see [`push_seg`]).
fn build_text_with(env: &mut Env, inl: &Inlines, o: &TextOpts, pending: &mut bool, subs: bool) -> TextBox {
    let mut job = LayoutJob::default();
    job.wrap.max_width = o.width;
    let mut chars = 0usize;
    let mut links: Vec<LinkSpan> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    // (byte range in the job text, char range) of each keycap.
    let mut kbd_runs: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut oi = 0usize;
    let text = inl.text.as_str();
    let mut spans: &[Span] = &inl.spans;
    let whole;
    if spans.is_empty() && !text.is_empty() {
        whole = [Span { range: 0..text.len() as u32, style: Style::EMPTY, link: None }];
        spans = &whole;
    }
    for sp in spans {
        let (s, e) = (sp.range.start as usize, sp.range.end as usize);
        if e > text.len() || s > e {
            continue;
        }
        let link = sp.link.and_then(|i| inl.links.get(i as usize));
        let unresolved = match link.map(|l| &l.target) {
            Some(LinkTarget::Wiki { note, .. }) => !wiki_resolved(env, note),
            _ => false,
        };
        let fmt = span_format(env, sp.style, o, link.map(|l| (&l.target, unresolved)));
        let (start_chars, start_bytes) = (chars, job.text.len());
        let mut pos = s;
        while pos < e {
            // Next object inside this span.
            while oi < inl.objects.len() && (inl.objects[oi].at as usize) < pos {
                oi += 1;
            }
            let obj = inl.objects.get(oi).filter(|ob| (ob.at as usize) < e && o.objects);
            let seg_end = obj.map_or(e, |ob| ob.at as usize);
            if seg_end > pos {
                push_seg(env, &mut Out { job: &mut job, chars: &mut chars, slots: &mut slots }, &text[pos..seg_end], &fmt, sp, subs);
            }
            let Some(ob) = obj else { break };
            pos = ob.at as usize + OBJ.len_utf8();
            oi += 1;
            match &ob.kind {
                ObjectKind::Math { source } => {
                    let pt = o.size * 0.75;
                    match env.math.math(source, false, pt, color_rgba(fmt.color), env.ppp * 4.0 / 3.0) {
                        MathResult::Ready(bm) => {
                            let (w, h, b) = (bm.size_pt[0] * 4.0 / 3.0, bm.size_pt[1] * 4.0 / 3.0, bm.baseline_pt * 4.0 / 3.0);
                            let at = chars;
                            let (l, a) = push_gap(&mut job, env, o, w, h, b, &mut chars);
                            slots.push(Slot { ch: at, w, h, baseline: b, row_l: l, row_a: a, content: SlotContent::Math(bm), text: obj_text(&ob.kind) });
                        }
                        MathResult::Error(e) => {
                            let mut f = fmt.clone();
                            f.color = Color32::from_rgb(220, 70, 70);
                            let a = chars;
                            push(&mut job, source, f, &mut chars);
                            links.push(LinkSpan { chars: a..chars, action: LinkAction::Tip, tip: e.to_string() });
                        }
                        MathResult::Pending => {
                            // A gap of about the final size, drawn empty until the formula arrives.
                            *pending = true;
                            let (w, h) = math_pending_size(source, o.size);
                            let at = chars;
                            let (l, a) = push_gap(&mut job, env, o, w, h, h * 0.8, &mut chars);
                            slots.push(Slot { ch: at, w, h, baseline: h * 0.8, row_l: l, row_a: a, content: SlotContent::Image(PathBuf::new()), text: obj_text(&ob.kind) });
                        }
                        _ => {
                            let mut f = span_format(env, Style::CODE, o, None);
                            f.color = env.style.text;
                            push(&mut job, source, f, &mut chars);
                        }
                    }
                }
                ObjectKind::Image { src, alt, wiki, .. } => {
                    let local = resolve_image(env, src, *wiki);
                    // shortcut: `is_file` stats the disk on the UI thread (once per re-layout of this block, layouts are cached), upgrade to an index lookup if slow network shares show up in frame times.
                    #[cfg(feature = "excalidraw")]
                    let is_exc = local
                        .as_ref()
                        .is_some_and(|p| FileKind::from_path(p) == FileKind::Excalidraw && (p.is_file() || env.open.get(p).is_some()));
                    #[cfg(feature = "excalidraw")]
                    if is_exc && let Some(target_path) = local {
                        use std::hash::{Hash, Hasher};
                        let stamp = env.embeds.stamp_of(&target_path, env.open);
                        let (dark, ppp, bucket) = (env.style.dark, env.ppp, 320.0_f32);
                        let mut kh = rustc_hash::FxHasher::default();
                        (&target_path, stamp, dark, bucket as u32, ppp.to_bits()).hash(&mut kh);
                        let key = PathBuf::from(format!("excalidraw:{:x}", kh.finish()));
                        let mut sh = rustc_hash::FxHasher::default();
                        (env.note, &target_path, chars, !env.stack.is_empty()).hash(&mut sh);
                        let slot = sh.finish();
                        let inline_source = env.open.get(&target_path).map(|d| d.text.clone());
                        let m = env.images.excalidraw(&key, slot, false, || ExcalidrawEmbedJob {
                            path: target_path.clone(),
                            source: inline_source,
                            dark,
                            w_pts: bucket,
                            ppp,
                            px_w: None,
                            index: env.index.clone(),
                        });
                        *pending |= m.pending;
                        match m.info {
                            ImgInfo::Known([iw, ih]) => {
                                let maxw = o.width.clamp(20.0, 900.0);
                                let k = (maxw / iw as f32).min(400.0 / ih as f32).min(1.0);
                                let (w, h) = (iw as f32 * k, ih as f32 * k);
                                let at = chars;
                                let (l, a) = probe(env, o.size);
                                let b = (h - l) / 2.0 + a;
                                push_gap(&mut job, env, o, w, h, b, &mut chars);
                                slots.push(Slot {
                                    ch: at,
                                    w,
                                    h,
                                    baseline: b,
                                    row_l: l,
                                    row_a: a,
                                    content: SlotContent::Excalidraw { tex_key: m.path, target: target_path },
                                    text: obj_text(&ob.kind),
                                });
                            }
                            ImgInfo::Unknown => {
                                *pending = true;
                                push(&mut job, if alt.is_empty() { "drawing" } else { alt }, fmt.clone(), &mut chars);
                            }
                            ImgInfo::Failed(_) => {
                                let mut f = fmt.clone();
                                f.color = env.style.link;
                                f.underline = Stroke::new(1.0, f.color);
                                let a = chars;
                                push(&mut job, if alt.is_empty() { src } else { alt }, f, &mut chars);
                                links.push(LinkSpan { chars: a..chars, action: LinkAction::Target(LinkTarget::Url(src.clone())), tip: src.clone() });
                            }
                        }
                        pos = seg_end;
                        oi += 1;
                        continue;
                    }
                    let info = local.as_ref().map(|p| env.images.info(p));
                    match (local, info) {
                        (Some(p), Some(ImgInfo::Known([iw, ih]))) => {
                            let maxw = o.width.clamp(20.0, 900.0);
                            let k = (maxw / iw as f32).min(400.0 / ih as f32).min(1.0);
                            let (w, h) = (iw as f32 * k, ih as f32 * k);
                            let at = chars;
                            // Centred on the text (CSS `vertical-align: middle`): sitting on the baseline would make the
                            // centred row reserve about the image's height again as empty space below it.
                            let (l, a) = probe(env, o.size);
                            let b = (h - l) / 2.0 + a;
                            push_gap(&mut job, env, o, w, h, b, &mut chars);
                            slots.push(Slot { ch: at, w, h, baseline: b, row_l: l, row_a: a, content: SlotContent::Image(p), text: obj_text(&ob.kind) });
                        }
                        // Size not known yet: only a network path or special file (the decode worker reads it).
                        (Some(_), Some(ImgInfo::Unknown)) => {
                            *pending = true;
                            push(&mut job, if alt.is_empty() { "image" } else { alt }, fmt.clone(), &mut chars);
                        }
                        _ => {
                            // Remote or broken: the alt text as a link.
                            let mut f = fmt.clone();
                            f.color = env.style.link;
                            f.underline = Stroke::new(1.0, f.color);
                            let a = chars;
                            push(&mut job, if alt.is_empty() { src } else { alt }, f, &mut chars);
                            links.push(LinkSpan { chars: a..chars, action: LinkAction::Target(LinkTarget::Url(src.clone())), tip: src.clone() });
                        }
                    }
                }
            }
        }
        if sp.style.contains(Style::KBD) && chars > start_chars {
            kbd_runs.push((start_bytes..job.text.len(), start_chars..chars));
        }
        if let Some(l) = link {
            if chars > start_chars {
                let tip = match &l.target {
                    LinkTarget::Url(u) => u.clone(),
                    LinkTarget::Wiki { note, heading, .. } => {
                        let mut t = note.clone();
                        if let Some(h) = heading {
                            t.push('#');
                            t.push_str(h);
                        }
                        if unresolved { format!("{t} (not found)") } else { t }
                    }
                    LinkTarget::Footnote(f) => format!("Footnote {f}"),
                    LinkTarget::Tag(t) => format!("#{t}"),
                };
                links.push(LinkSpan { chars: start_chars..chars, action: LinkAction::Target(l.target.clone()), tip });
            }
        }
    }
    if job.text.is_empty() {
        job.append(" ", 0.0, TextFormat::simple(FontId::new(o.size, family(o.bold, o.italic)), o.color));
    }
    // Air around each keycap: its first section and the one after it.
    for (b, _) in &kbd_runs {
        for sec in job.sections.iter_mut().filter(|sec| sec.byte_range.start.0 == b.start || sec.byte_range.start.0 == b.end) {
            sec.leading_space = KBD_LEAD;
        }
    }
    let st = env.style;
    let (fill, border) = (if st.dark { Color32::from_gray(60) } else { Color32::from_gray(240) }, st.rule);
    let runs: Vec<_> = kbd_runs.into_iter().map(|(_, c)| (c, fill, border)).collect();
    let galley = layout_job(env.ctx, job);
    let kbds = kbd_boxes(&galley, &runs);
    TextBox { galley, links, slots, kbds }
}

fn mono_galley(env: &Env, text: &str, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    let text = if text.is_empty() { " " } else { text };
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat { font_id: FontId::new(size, env.style.code_family.clone()), color, ..Default::default() });
    job.wrap.max_width = width;
    layout_job(env.ctx, job)
}

fn plain_text(env: &mut Env, s: &str, o: &TextOpts) -> TextBox {
    let inl = Inlines { text: s.to_string(), spans: vec![], links: vec![], objects: vec![] };
    let mut p = false;
    build_text(env, &inl, o, &mut p)
}

/// Layout `doc.blocks[i]` for a column `width` wide.
pub fn layout_block(env: &mut Env, b: &Block, width: f32) -> BlockLayout {
    let st = env.style;
    let indent = total_indent(&b.containers);
    let w = (width - indent).max(60.0);
    let mut pending = false;
    let mut head_h = 0.0;
    let mut titles = Vec::new();
    for (k, c) in b.containers.iter().enumerate() {
        if let (true, ContainerKind::Alert { kind, title }) = (c.first, &c.kind) {
            let col = alert_color(kind, st.dark);
            let tb =
                plain_text(env, &alert_title(kind, title), &TextOpts { size: st.body_size, bold: true, italic: false, color: col, width: w, objects: false });
            head_h += tb.galley.size().y + 4.0;
            titles.push((k, tb.galley.clone(), col));
        }
    }
    let size = st.body_size;
    let base = TextOpts { size, bold: false, italic: false, color: st.text, width: w, objects: true };
    let mut first_line_h = size * 1.3;
    let (body, body_h) = match &b.kind {
        BlockKind::Paragraph(inl) => {
            let tb = build_text(env, inl, &base, &mut pending);
            first_line_h = tb.galley.rows.first().map_or(first_line_h, |r| r.size.y);
            let h = tb.size().y;
            (Body::Text { tb, rule_below: false }, h)
        }
        BlockKind::Heading { level, content, .. } => {
            let o = TextOpts { size: size * heading_scale(*level), bold: true, color: st.heading, ..base };
            let tb = build_text(env, content, &o, &mut pending);
            first_line_h = tb.galley.rows.first().map_or(first_line_h, |r| r.size.y);
            let rule = *level <= 2;
            let h = tb.size().y + if rule { 8.0 } else { 0.0 };
            (Body::Text { tb, rule_below: rule }, h)
        }
        BlockKind::Code { lang, text } if env.mermaid && lang.eq_ignore_ascii_case("mermaid") => {
            let (body, h, err) = mermaid_body(env, text, w, &mut pending);
            if let Some(e) = err {
                let note = format!("Mermaid diagram could not be rendered: {}", mermaid::short(&e));
                let o = TextOpts { size: st.body_size * 0.9, bold: false, italic: false, color: Color32::from_rgb(220, 70, 70), width: w, objects: false };
                let g = plain_text(env, &note, &o).galley;
                head_h += g.size().y + 4.0;
                titles.push((usize::MAX, g, o.color));
            }
            (body, h)
        }
        BlockKind::Code { lang, text } => {
            let runs = match env.hl.get(lang, text) {
                CodeHl::Runs(r) => Some(r),
                CodeHl::Pending(prev) => {
                    pending = true;
                    prev
                }
                CodeHl::Plain => None,
            };
            code_body(env, text, lang, st.text, w, runs.as_deref())
        }
        BlockKind::Typst { source } => code_body(env, source, "typst", st.text, w, None),
        BlockKind::Html { text } => code_body(env, text, "html", dim(st.text, st.dark), w, None),
        BlockKind::FrontMatter { text } => code_body(env, text, "front matter", dim(st.text, st.dark), w, None),
        BlockKind::Math { source } => {
            let pt = size * 0.75 * 1.2;
            match env.math.math(source, true, pt, color_rgba(st.text), env.ppp * 4.0 / 3.0) {
                MathResult::Ready(bm) => {
                    let (mw, mh) = (bm.size_pt[0] * 4.0 / 3.0, bm.size_pt[1] * 4.0 / 3.0);
                    (Body::Math { bm, w: mw, h: mh }, mh + 8.0)
                }
                MathResult::Error(e) => {
                    let o = TextOpts { color: Color32::from_rgb(220, 70, 70), ..base };
                    // The first line of the message sits under the red source (inline math keeps the tooltip only: no reflow in a line).
                    let msg = mermaid::short(&e);
                    let text = format!("{source}\n{msg}");
                    let cut = source.len() as u32 + 1;
                    let spans = vec![
                        Span { range: 0..cut, style: Style::EMPTY, link: None },
                        Span { range: cut..text.len() as u32, style: Style::ITALIC, link: None },
                    ];
                    let mut p = false;
                    let mut tb = build_text_with(env, &Inlines { text, spans, links: vec![], objects: vec![] }, &o, &mut p, false);
                    tb.links.push(LinkSpan { chars: 0..source.chars().count(), action: LinkAction::Tip, tip: e.to_string() });
                    let h = tb.size().y;
                    (Body::Text { tb, rule_below: false }, h)
                }
                MathResult::Pending => {
                    // A box of about the final size (the formula is not drawn yet), not the source: the block must not jump.
                    pending = true;
                    let h = math_pending_height(source, size);
                    (Body::Image { path: PathBuf::new(), size: Vec2::new(w.min(240.0), h) }, h + 8.0)
                }
                _ => code_body(env, source, "math", st.text, w, None),
            }
        }
        BlockKind::Table { aligns, header, rows } => table_body(env, aligns, header, rows, &base, &mut pending),
        BlockKind::Rule => (Body::Rule, 10.0),
        BlockKind::Image { src, alt, wiki, .. } => image_block(env, b, ImageRef { src, alt, wiki: *wiki }, w, &base, &mut pending),
        BlockKind::Embed { target } if is_image_name(target) => image_block(env, b, ImageRef { src: target, alt: target, wiki: true }, w, &base, &mut pending),
        BlockKind::Embed { target } => embed_body(env, b, target, w, &base, &mut pending),
    };
    BlockLayout { height: head_h + body_h, head_h, indent, body_h, first_line_h, titles, body, pending }
}

/// Height of a display formula that is still rendering: taller for the usual tall constructs, per source line.
/// Measured on rendered formulas: `a^2 + b^2` is about 1.8 em, a sum with limits and a fraction about 3.9 em (the box adds 8 px around it).
fn math_pending_height(source: &str, size: f32) -> f32 {
    const TALL: [&str; 10] = ["sum", "int", "frac", "/", "lim", "prod", "mat", "binom", "root", "cases"];
    let lines = source.lines().filter(|l| !l.trim().is_empty()).count().max(1) as f32;
    let em = if TALL.iter().any(|t| source.contains(t)) { 3.8 } else { 1.8 };
    lines * em * size
}

/// Size of an inline formula that is still rendering: about half an em per visible character.
fn math_pending_size(source: &str, size: f32) -> (f32, f32) {
    let chars = source.chars().filter(|c| !c.is_whitespace()).count() as f32;
    ((chars * 0.5).max(1.0) * size, size * 1.125)
}

/// The diagram as an image block (an estimated-size box while it renders). On failure the source as code and the reason.
fn mermaid_body(env: &mut Env, src: &str, w: f32, pending: &mut bool) -> (Body, f32, Option<String>) {
    use std::hash::{Hash, Hasher};
    // Cache key: source, theme, column width bucket, pixel density.
    let (dark, ppp, bucket) = (env.style.dark, env.ppp, (w / 64.0).ceil() * 64.0);
    let mut h = rustc_hash::FxHasher::default();
    (src, dark, bucket as u32, ppp.to_bits()).hash(&mut h);
    let path = PathBuf::from(format!("mermaid:{:x}", h.finish()));
    let slot = {
        let mut h = rustc_hash::FxHasher::default();
        // An embed lays the note out at another width (another key): its own slot, or the two would release each other's render.
        (env.note, env.mermaid_ordinal, !env.stack.is_empty()).hash(&mut h);
        h.finish()
    };
    let m = env.images.mermaid(&path, slot, env.mermaid_hold, || mermaid::Job { source: src.to_string(), dark, w_pts: bucket, ppp, px_w: None });
    *pending |= m.pending;
    match m.info {
        ImgInfo::Known([iw, ih]) => {
            let k = (w / iw as f32).min(1.0);
            let size = Vec2::new(iw as f32 * k, ih as f32 * k);
            let (mut texts, mut shapes, mut h) = (Vec::new(), Vec::new(), size.y);
            if let Some(t) = env.images.texts(&m.path) {
                let sc = size.x / t.size[0].max(1.0);
                texts = diagram::label_boxes(env.ctx, &t, sc).into_iter().map(|(p, tb)| (p.to_vec2(), tb)).collect();
                shapes = diagram::label_shapes(&t, sc);
            }
            let err = m.err.map(|e| {
                let f = FontId::new(env.style.body_size * 0.9, families().preview);
                let mut job = LayoutJob::single_section(
                    format!("Mermaid diagram could not be rendered: {}", mermaid::short(&e)),
                    TextFormat { font_id: f, color: Color32::from_rgb(220, 70, 70), ..Default::default() },
                );
                job.wrap.max_width = w;
                let g = layout_job(env.ctx, job);
                h += g.size().y + 4.0;
                g
            });
            let key = env.images.slot_key(slot).map(Path::to_path_buf).unwrap_or_default();
            (Body::Diagram(Box::new(DiagramBody { path: m.path, slot, key, size, texts, shapes, err })), h, None)
        }
        // A diagram that has not rendered before: an estimate until it does.
        ImgInfo::Unknown => {
            *pending = true;
            (Body::Image { path, size: Vec2::new(w.min(200.0), 80.0) }, 80.0, None)
        }
        ImgInfo::Failed(e) => {
            let (body, h) = code_body(env, src, "mermaid", env.style.text, w, None);
            (body, h, Some(e))
        }
    }
}

/// `runs` colour parts of `text` (see `CodeRun`); the gaps and everything without runs use `color`.
fn code_body(env: &Env, text: &str, lang: &str, color: Color32, _w: f32, runs: Option<&[CodeRun]>) -> (Body, f32) {
    let size = env.style.body_size * 0.9;
    let text = text.trim_end_matches('\n');
    let g = match runs.filter(|r| !r.is_empty() && !text.is_empty()) {
        None => mono_galley(env, text, size, color, f32::INFINITY),
        Some(runs) => {
            let font_id = FontId::new(size, env.style.code_family.clone());
            let mut job = LayoutJob { wrap: egui::text::TextWrapping { max_width: f32::INFINITY, ..Default::default() }, ..Default::default() };
            let mut pos = 0;
            for r in runs {
                let (a, b) = (r.start as usize, (r.end as usize).min(text.len()));
                let Some(s) = text.get(a..b).filter(|_| a >= pos) else { continue };
                if let Some(gap) = text.get(pos..a).filter(|g| !g.is_empty()) {
                    job.append(gap, 0.0, TextFormat { font_id: font_id.clone(), color, ..Default::default() });
                }
                job.append(s, 0.0, TextFormat { font_id: font_id.clone(), color: env.style.code_color(r.class), ..Default::default() });
                pos = b;
            }
            if let Some(rest) = text.get(pos..).filter(|g| !g.is_empty()) {
                job.append(rest, 0.0, TextFormat { font_id, color, ..Default::default() });
            }
            layout_job(env.ctx, job)
        }
    };
    let h = g.size().y + 2.0 * CODE_PAD;
    let text_w = g.size().x;
    (Body::Code { galley: g, lang: lang.to_string(), text_w }, h)
}

fn table_body(
    env: &mut Env,
    aligns: &[iron_sheets_md::model::Align],
    header: &[Inlines],
    rows: &[Vec<Inlines>],
    base: &TextOpts,
    pending: &mut bool,
) -> (Body, f32) {
    let ncols = rows.iter().map(|r| r.len()).chain([header.len(), aligns.len()]).max().unwrap_or(0).max(1);
    let empty = Inlines::default();
    let all: Vec<(&Inlines, bool)> = (0..=rows.len())
        .flat_map(|r| (0..ncols).map(move |c| (r, c)))
        .map(|(r, c)| if r == 0 { (header.get(c).unwrap_or(&empty), true) } else { (rows[r - 1].get(c).unwrap_or(&empty), false) })
        .collect();
    // Natural widths, no wrapping.
    let mut cols = vec![40.0f32; ncols];
    let o_nowrap = |bold| TextOpts { width: f32::INFINITY, bold, size: base.size, ..*base };
    let mut natural: Vec<f32> = Vec::with_capacity(all.len());
    for (i, (inl, bold)) in all.iter().enumerate() {
        let tb = build_text(env, inl, &o_nowrap(*bold), pending);
        let w = tb.size().x + 2.0 * TABLE_PAD_X;
        natural.push(w);
        cols[i % ncols] = cols[i % ncols].max(w);
    }
    for c in cols.iter_mut() {
        *c = c.min(TABLE_COL_CAP + 2.0 * TABLE_PAD_X).ceil();
    }
    let mut cells = Vec::with_capacity(all.len());
    let mut rows_h = vec![0.0f32; rows.len() + 1];
    for (i, (inl, bold)) in all.iter().enumerate() {
        let (r, c) = (i / ncols, i % ncols);
        let o = TextOpts { width: cols[c] - 2.0 * TABLE_PAD_X, bold: *bold, ..*base };
        let tb = build_text(env, inl, &o, pending);
        rows_h[r] = rows_h[r].max(tb.size().y + 2.0 * TABLE_PAD_Y);
        cells.push(tb);
    }
    let al: Vec<Align> = (0..ncols)
        .map(|c| match aligns.get(c).copied().unwrap_or(iron_sheets_md::model::Align::None) {
            iron_sheets_md::model::Align::Center => Align::Center,
            iron_sheets_md::model::Align::Right => Align::Max,
            _ => Align::Min,
        })
        .collect();
    let total_w: f32 = cols.iter().sum();
    let h: f32 = rows_h.iter().sum();
    (Body::Table(TableLayout { cols, rows_h, cells, aligns: al, total_w }), h)
}

/// Rough height before a block has been laid out.
pub fn estimate(b: &Block, width: f32, size: f32) -> f32 {
    let w = (width - total_indent(&b.containers)).max(60.0);
    let text_h = |chars: usize, sz: f32| {
        let per_line = (w / (sz * 0.5)).max(8.0);
        ((chars as f32 / per_line).ceil().max(1.0)) * sz * 1.25
    };
    let head: f32 = b.containers.iter().filter(|c| c.first && matches!(c.kind, ContainerKind::Alert { .. })).count() as f32 * (size * 1.25 + 4.0);
    head + match &b.kind {
        BlockKind::Paragraph(i) => text_h(i.text.len(), size),
        BlockKind::Heading { level, content, .. } => text_h(content.text.len(), size * heading_scale(*level)) + if *level <= 2 { 8.0 } else { 0.0 },
        BlockKind::Code { text, .. } | BlockKind::Html { text } | BlockKind::FrontMatter { text } => {
            (text.matches('\n').count() as f32 + 1.0) * size * 0.9 * 1.25 + 2.0 * CODE_PAD
        }
        BlockKind::Typst { source } | BlockKind::Math { source } => (source.matches('\n').count() as f32 + 1.0) * size * 1.25 + 2.0 * CODE_PAD,
        BlockKind::Table { rows, .. } => (rows.len() as f32 + 1.0) * (size * 1.25 + 2.0 * TABLE_PAD_Y),
        BlockKind::Rule => 10.0,
        BlockKind::Image { .. } => 120.0,
        BlockKind::Embed { .. } => size * 3.0,
    }
}

/// Document-order gap above a block.
pub fn gap(sp: Spacing, size: f32) -> f32 {
    match sp {
        Spacing::None => 0.0,
        Spacing::Tight => size * 0.2,
        Spacing::Normal => size * 0.7,
        Spacing::Loose => size * 1.1,
    }
}

fn is_image_name(s: &str) -> bool {
    let l = s.split('|').next().unwrap_or(s).to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"].iter().any(|e| l.ends_with(e))
}

#[cfg(feature = "excalidraw")]
fn excalidraw_body(
    env: &mut Env,
    b: &Block,
    target_path: PathBuf,
    want: Option<(f32, Option<f32>)>,
    w: f32,
    pending: &mut bool,
) -> (Body, f32) {
    use std::hash::{Hash, Hasher};
    if env.stack.is_empty() {
        env.embeds.register(b, Some(&target_path), env.open);
    }
    let stamp = env.embeds.stamp_of(&target_path, env.open);
    let (dark, ppp, bucket) = (env.style.dark, env.ppp, (w / 64.0).ceil() * 64.0);
    let mut kh = rustc_hash::FxHasher::default();
    (&target_path, stamp, dark, bucket as u32, ppp.to_bits()).hash(&mut kh);
    let key = PathBuf::from(format!("excalidraw:{:x}", kh.finish()));
    let slot = {
        let mut sh = rustc_hash::FxHasher::default();
        (env.note, b.lines.start, &target_path, !env.stack.is_empty()).hash(&mut sh);
        sh.finish()
    };
    let inline_source = env.open.get(&target_path).map(|d| d.text.clone());
    let m = env.images.excalidraw(&key, slot, false, || ExcalidrawEmbedJob {
        path: target_path.clone(),
        source: inline_source,
        dark,
        w_pts: bucket,
        ppp,
        px_w: None,
        index: env.index.clone(),
    });
    *pending |= m.pending;
    let slot_key = env.images.slot_key(slot).map(Path::to_path_buf).unwrap_or_else(|| key.clone());
    let err_galley = |msg: &str| {
        let f = FontId::new(env.style.body_size * 0.9, families().preview);
        let mut job = LayoutJob::single_section(
            format!("Excalidraw drawing could not be rendered: {}", mermaid::short(msg)),
            TextFormat { font_id: f, color: Color32::from_rgb(220, 70, 70), ..Default::default() },
        );
        job.wrap.max_width = w;
        layout_job(env.ctx, job)
    };
    match m.info {
        ImgInfo::Known([iw, ih]) => {
            let (mut sw, mut sh) = match want {
                Some((tw, Some(th))) => (tw, th),
                Some((tw, None)) => (tw, ih as f32 * tw / iw.max(1) as f32),
                None => {
                    let k = (w / iw.max(1) as f32).min(1.0);
                    (iw as f32 * k, ih as f32 * k)
                }
            };
            if sw > w {
                sh *= w / sw;
                sw = w;
            }
            let size = Vec2::new(sw.max(1.0), sh.max(1.0));
            let mut h = size.y;
            let err = m.err.map(|e| {
                let g = err_galley(&e);
                h += g.size().y + 4.0;
                g
            });
            (
                Body::Excalidraw(Box::new(ExcalidrawBody { target: target_path, path: m.path, slot, key: slot_key, size, err })),
                h,
            )
        }
        ImgInfo::Unknown => {
            *pending = true;
            let size = Vec2::new(w.min(320.0), 140.0);
            (
                Body::Excalidraw(Box::new(ExcalidrawBody { target: target_path, path: m.path, slot, key: slot_key, size, err: None })),
                size.y,
            )
        }
        ImgInfo::Failed(e) => {
            let g = err_galley(&e);
            let h = (g.size().y + 12.0).max(32.0);
            (
                Body::Excalidraw(Box::new(ExcalidrawBody {
                    target: target_path,
                    path: m.path,
                    slot,
                    key: slot_key,
                    size: Vec2::new(w.min(320.0), 0.0),
                    err: Some(g),
                })),
                h,
            )
        }
    }
}

struct ImageRef<'a> {
    src: &'a str,
    alt: &'a str,
    wiki: bool,
}

#[cfg_attr(not(feature = "excalidraw"), allow(unused_variables))]
fn image_block(env: &mut Env, b: &Block, img: ImageRef<'_>, w: f32, base: &TextOpts, pending: &mut bool) -> (Body, f32) {
    let (src, alt, wiki) = (img.src, img.alt, img.wiki);
    let st = env.style;
    let local = resolve_image(env, src, wiki);
    // `![[pic.png|200]]` and `|200x100`: the alias is the size.
    let want = if wiki { wiki_size(alt) } else { None };
    let alt = if want.is_some() { "" } else { alt };
    // shortcut: `is_file` stats the disk on the UI thread (once per re-layout of this block, layouts are cached), upgrade to an index lookup if slow network shares show up in frame times.
    #[cfg(feature = "excalidraw")]
    if let Some(p) = local.as_ref().filter(|p| FileKind::from_path(p) == FileKind::Excalidraw && (p.is_file() || env.open.get(p).is_some())) {
        return excalidraw_body(env, b, p.clone(), want, w, pending);
    }
    match local.as_ref().map(|p| (p, env.images.info(p))) {
        Some((p, ImgInfo::Known([iw, ih]))) => {
            let (mut sw, mut sh) = match want {
                Some((tw, Some(th))) => (tw, th),
                Some((tw, None)) => (tw, ih as f32 * tw / iw as f32),
                None => {
                    let k = (w / iw as f32).min(1.0);
                    (iw as f32 * k, ih as f32 * k)
                }
            };
            if sw > w {
                sh *= w / sw;
                sw = w;
            }
            (Body::Image { path: p.clone(), size: Vec2::new(sw, sh) }, sh)
        }
        // Size not known yet: only a network path or special file (the decode worker reads it).
        Some((p, ImgInfo::Unknown)) => {
            *pending = true;
            (Body::Image { path: p.clone(), size: Vec2::new(w.min(200.0), 80.0) }, 80.0)
        }
        _ => {
            // Remote or unreadable: the alt text as a link.
            let label = if alt.is_empty() { src } else { alt };
            let mut tb = plain_text(env, label, &TextOpts { color: st.link, ..*base });
            tb.links.push(LinkSpan { chars: 0..label.chars().count(), action: LinkAction::Target(LinkTarget::Url(src.to_string())), tip: src.to_string() });
            let h = tb.size().y;
            (Body::Text { tb, rule_below: false }, h)
        }
    }
}

/// `200` or `200x100` (the alias of an image embed) as (width, height).
fn wiki_size(alt: &str) -> Option<(f32, Option<f32>)> {
    let (w, h) = match alt.trim().split_once('x') {
        Some((w, h)) => (w, Some(h)),
        None => (alt.trim(), None),
    };
    let w: f32 = w.parse().ok().filter(|w| *w > 0.0)?;
    let h = match h {
        Some(h) => Some(h.parse::<f32>().ok().filter(|h| *h > 0.0)?),
        None => None,
    };
    Some((w, h))
}

/// One text row ("↪ name") that opens `action`, or plain text without one.
fn row_tb(env: &mut Env, label: &str, action: Option<LinkTarget>, base: &TextOpts) -> TextBox {
    let color = if action.is_some() { env.style.link } else { dim(env.style.text, env.style.dark) };
    let mut tb = plain_text(env, label, &TextOpts { color, ..*base });
    if let Some(a) = action {
        tb.links.push(LinkSpan { chars: 2..label.chars().count(), tip: label.chars().skip(2).collect(), action: LinkAction::Target(a) });
    }
    tb
}

fn embed_row(env: &mut Env, label: &str, action: Option<LinkTarget>, base: &TextOpts) -> (Body, f32) {
    let tb = row_tb(env, label, action, base);
    let h = tb.size().y;
    (Body::Text { tb, rule_below: false }, h)
}

fn embed_body(env: &mut Env, b: &Block, target: &str, w: f32, base: &TextOpts, pending: &mut bool) -> (Body, f32) {
    let t = parse_target(target);
    let wiki = |t: &super::embed::Target| LinkTarget::Wiki {
        note: t.name.clone(),
        heading: if let Part::Heading(h) = &t.part { Some(h.clone()) } else { None },
        block: if let Part::Block(b) = &t.part { Some(b.clone()) } else { None },
    };
    let found = env.index.try_read().and_then(|ix| {
        let m = ix.resolve(&t.name, Some(env.note)).and_then(|id| ix.get(id))?;
        let k = FileKind::from_path(&m.path);
        matches!(k, FileKind::Markdown | FileKind::Excalidraw).then(|| (m.path.clone(), k))
    });
    if env.stack.is_empty() {
        env.embeds.register(b, found.as_ref().map(|(p, _)| p.as_path()), env.open);
        *env.budget = EMBED_MAX_BLOCKS;
    }
    #[cfg(feature = "excalidraw")]
    if let Some((path, FileKind::Excalidraw)) = found {
        let want = target.split_once('|').and_then(|(_, a)| wiki_size(a));
        return excalidraw_body(env, b, path, want, w, pending);
    }
    let Some((path, _)) = found else {
        // A PDF or other file: a link that opens it.
        let file = env.index.try_read().and_then(|ix| ix.resolve_file(&t.name, Some(env.note)));
        let action = file.map_or_else(|| wiki(&t), |p| LinkTarget::Url(p.to_string_lossy().into_owned()));
        return embed_row(env, &format!("\u{BB} {}", t.name), Some(action), base);
    };
    if path == env.note || env.stack.contains(&path) {
        return embed_row(env, "(embed cycle)", None, base);
    }
    let link = |env: &mut Env| embed_row(env, &format!("\u{BB} {target}"), Some(wiki(&t)), base);
    if env.embeds.mode == Mode::Link || env.stack.len() as u32 >= env.embeds.max_depth {
        return link(env);
    }
    let Some(edoc) = env.embeds.doc(&path, env.open) else { return link(env) };
    let range = slice(&edoc, &t.part);
    if range.is_empty() {
        return embed_row(env, &format!("\u{BB} {target} (not found)"), Some(wiki(&t)), base);
    }
    let collapsible = env.embeds.mode == Mode::Collapsed;
    let expanded = !collapsible || env.embeds.expanded.contains(&b.hash);
    let arrow = if !collapsible {
        '\u{BB}'
    } else if expanded {
        '-'
    } else {
        '+'
    };
    let inner_w = (w - 2.0 * EMBED_PAD).max(60.0);
    let mut title = plain_text(env, &format!("{arrow} {target}"), &TextOpts { color: env.style.link, width: inner_w, ..*base });
    title.links.push(LinkSpan { chars: 2..target.chars().count() + 2, action: LinkAction::Target(wiki(&t)), tip: target.to_string() });
    let mut items = Vec::new();
    let (mut y, mut more) = (0.0f32, None);
    let mut more_y = 0.0;
    if expanded {
        let mut sub_stack = env.stack.clone();
        sub_stack.push(env.note.to_path_buf());
        let mut sub = Env {
            ctx: env.ctx,
            style: env.style,
            math: env.math,
            hl: env.hl,
            index: env.index,
            note: &path,
            images: &mut *env.images,
            mermaid: env.mermaid,
            // shortcut: embedded notes always re-render while typing, upgrade when embeds need the setting too.
            mermaid_hold: false,
            mermaid_ordinal: 0,
            nbsp_ratio: env.nbsp_ratio,
            ppp: env.ppp,
            embeds: &mut *env.embeds,
            open: env.open,
            stack: sub_stack,
            budget: &mut *env.budget,
        };
        for k in range.clone() {
            if y > EMBED_MAX_H || *sub.budget == 0 {
                break;
            }
            *sub.budget -= 1;
            if k > range.start {
                y += gap_at(&edoc, k, env.style.body_size);
            }
            sub.mermaid_ordinal = mermaid_ordinal(&edoc, k);
            let l = layout_block(&mut sub, &edoc.blocks[k], inner_w);
            *pending |= l.pending;
            let h = l.height;
            items.push((k, y, l));
            y += h;
        }
        if items.last().is_some_and(|(k, ..)| *k + 1 < range.end) {
            more_y = y + 4.0;
            let tb = row_tb(env, "\u{BB} Show more in the note", Some(wiki(&t)), base);
            y = more_y + tb.size().y;
            more = Some(tb);
        }
    }
    let title_h = title.size().y;
    let body = if expanded { 4.0 + y } else { 0.0 };
    let height = 2.0 * EMBED_PAD + title_h + body;
    let toggle = collapsible.then_some(b.hash);
    (Body::Embed(Box::new(EmbedLayout { doc: edoc, path: Arc::new(path), title, items, more, more_y, toggle })), height)
}

/// Gap above block `i`: a "tight" gap only applies between list items.
pub fn gap_at(doc: &Document, i: usize, size: f32) -> f32 {
    if i == 0 {
        return 0.0;
    }
    let sp = doc.blocks[i].spacing;
    let sp = if sp == Spacing::Tight && !doc.blocks[i - 1].containers.iter().any(|c| matches!(c.kind, ContainerKind::ListItem { .. })) {
        Spacing::Normal
    } else {
        sp
    };
    gap(sp, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(t: &str) -> String {
        let (mut out, mut at) = (String::new(), 0);
        for (r, g) in arrows(t) {
            out.push_str(&t[at..r.start]);
            out.push_str(g);
            at = r.end;
        }
        out + &t[at..]
    }

    #[test]
    fn arrows_take_the_longest_match_and_leave_the_rest() {
        assert_eq!(conv("a -> b <- c => d <=> e <-> f"), "a → b ← c ⇒ d ⇔ e ↔ f");
        assert_eq!(conv("a <= b >= c != d"), "a ≤ b ≥ c ≠ d");
        assert_eq!(conv("a == b ... c"), "a == b ... c");
        assert_eq!(conv("x->y"), "x→y");
        // Touching another operator character: as typed.
        assert_eq!(conv("--> ==> !== <-- <!-- -->"), "--> ==> !== <-- <!-- -->");
        assert_eq!(conv("-> ->"), "→ →");
        assert_eq!(conv("héllo → ü->"), "héllo → ü→");
    }

    #[test]
    fn a_substituted_char_gives_its_source_back() {
        let t = "a -> b != c";
        let slots: Vec<Slot> = {
            let (mut n, mut v) = (0, Vec::new());
            for (r, _) in arrows(t) {
                v.push(Slot::typed(t[..r.start].chars().count() - n, &t[r.clone()]));
                n += r.len() - 1;
            }
            v
        };
        let shown = conv(t);
        assert_eq!(shown, "a → b ≠ c");
        assert_eq!(super::super::select::slice_text(&shown, &slots, &(0..shown.chars().count())), t);
        assert_eq!(super::super::select::slice_text(&shown, &slots, &(2..5)), "-> b");
    }
}
