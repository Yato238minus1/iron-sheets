//! Source editor: a monospace `TextEdit` in a scroll pane with a cached layout.
mod codehl;
pub mod features;
mod gutter;
pub mod highlight;
pub use iron_sheets_app_core::editor::{find_model, floor_boundary, wiki_model};
mod paragraphs;

use crate::command::Command;
use crate::scrollsync::ScrollMap;
use crate::scrollsync_egui::scroll_pane;
use crate::state::AppState;
use crate::theme::EditorStyle;
use egui::text::{ByteIndex, CharIndex};
use egui::text::{CCursor, CCursorRange};
use egui::{Galley, Id, TextBuffer, TextEdit};
use features::EditorFrame;
use iron_sheets_core::FileKind;
use iron_sheets_md::WindowHighlighter;
use rustc_hash::FxHashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

struct Cached {
    key: (u64, u32, u64, u8),
    galley: Arc<Galley>,
    lines: paragraphs::LineCache,
    /// Galley the scroll map was built for.
    map_for: u64,
    /// (nav gen, doc version, text len, wrap bits, theme generation, kind, highlight limit) the galley was made for: lets the
    /// pre-pass skip hashing the text on frames where nothing changed.
    ver: Option<(u64, u64, usize, u32, u64, u8, usize)>,
    /// Wrap width the `TextEdit` asked for in the last frame.
    wrap_used: f32,
    /// Block state of a big Markdown note (highlighting of the visible part only).
    big: Option<WindowHighlighter>,
    /// Layout path of the last layout: 0 whole-note job, 1 big and plain, 2 big and highlighted, 3 incremental exact.
    path: u8,
    /// Word/char count change between the text laid out when the frame began and the text laid out last (big notes).
    delta: Option<(isize, isize)>,
    /// `nav_gen` of the `TextEdit` whose undo history was capped for the note's size (each step is a copy of the text).
    undo_capped: Option<u64>,
    /// Code highlighter results the paragraphs of a big note were last styled with.
    code_epoch: u64,
    /// (doc version, line count) for the gutter width.
    line_count: Option<(u64, usize)>,
    /// Stand-in while `galley` is let go of so that the layout can patch it in place (big notes).
    empty: Arc<Galley>,
}

impl Cached {
    /// Lets go of the galley (the layout keeps its own handle) so a big layout can patch it instead of rebuilding it.
    fn release(&mut self) {
        if self.path != 0 {
            self.galley = self.empty.clone();
        }
    }

    /// Takes the layout's galley back after `release` when nothing new was made.
    fn restore(&mut self) {
        if self.path != 0 && Arc::ptr_eq(&self.galley, &self.empty) {
            if let Some(g) = self.lines.current() {
                self.galley = g;
            }
        }
    }
}

/// Layout state of a tab's editor, for tests and the perf overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutStats {
    pub rows: usize,
    pub chars: usize,
    /// Paragraphs that are still estimated placeholders.
    pub placeholders: usize,
    /// Galleys made by patching the previous one (big notes) instead of rebuilding it.
    pub patched: usize,
    /// Rows that start above the bottom of the row before them (a broken layout paints text over text).
    pub overlapping: usize,
}

#[derive(Default)]
pub struct EditorState {
    cache: FxHashMap<u64, Cached>,
    focused: Option<(u64, u64)>,
    last_sel: FxHashMap<u64, (usize, usize)>,
    /// Width of the pane minus the wrap width the `TextEdit` used, learned from the last frame (predicts the
    /// wrap width before the `TextEdit` runs).
    wrap_delta: f32,
    caret: crate::caret::Caret,
}

impl EditorState {
    /// The painted (smooth) caret, for tests.
    pub fn caret(&self) -> &crate::caret::Caret {
        &self.caret
    }

    /// Drop per-tab caches of tabs that no longer exist.
    pub fn prune(&mut self, tab_ids: &[u64]) {
        if self.cache.len() > tab_ids.len() || self.last_sel.len() > tab_ids.len() {
            self.cache.retain(|id, _| tab_ids.contains(id));
            self.last_sel.retain(|id, _| tab_ids.contains(id));
        }
    }

    pub fn cached_tabs(&self) -> usize {
        self.cache.len()
    }

    pub fn stats(&self, tab_id: u64) -> Option<LayoutStats> {
        let c = self.cache.get(&tab_id)?;
        Some(LayoutStats {
            rows: c.galley.rows.len(),
            chars: c.galley.rows.iter().map(|r| r.char_count_including_newline().0).sum(),
            placeholders: c.lines.placeholders(),
            patched: c.lines.patched(),
            overlapping: c.galley.rows.windows(2).filter(|w| (w[1].pos.y - (w[0].pos.y + w[0].row.size.y)).abs() > 1.0).count(),
        })
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    s.hash(&mut h);
    h.finish()
}

enum LazySections<'a> {
    Big(highlight::BigSource<'a>),
    Inc(highlight::IncrementalSource<'a>),
}

impl paragraphs::Sections for LazySections<'_> {
    fn base_format(&self) -> egui::text::TextFormat {
        match self {
            LazySections::Big(s) => s.base_format(),
            LazySections::Inc(s) => s.base_format(),
        }
    }

    fn sections(&self, text: &str, start: usize, end: usize, out: &mut Vec<egui::text::LayoutSection>) {
        match self {
            LazySections::Big(s) => s.sections(text, start, end, out),
            LazySections::Inc(s) => s.sections(text, start, end, out),
        }
    }

    fn pending(&self) -> bool {
        match self {
            LazySections::Big(s) => s.pending(),
            LazySections::Inc(s) => s.pending(),
        }
    }
}

fn lazy_sections<'a>(
    path: u8,
    big: Option<&'a WindowHighlighter>,
    kind: FileKind,
    style: &EditorStyle,
    code: Option<&'a dyn crate::services::CodeHighlighter>,
) -> Option<LazySections<'a>> {
    match path {
        1 | 2 => Some(LazySections::Big(highlight::big_source(big, style).with_code(code))),
        3 => Some(LazySections::Inc(highlight::incremental_source_lazy(kind, style, code))),
        _ => None,
    }
}

/// Lays the text out unless the cached galley already is for it. Returns the anchor shift when it did.
#[allow(clippy::too_many_arguments)]
fn relayout(
    entry: &mut Cached,
    ui: &egui::Ui,
    text: &str,
    wrap: f32,
    kind: FileKind,
    style: &EditorStyle,
    generation: u64,
    view: &paragraphs::View,
    code: Option<&crate::preview::markdown::codehl::SyntectHighlighter>,
    unmutated_pre_pass: bool,
) -> Option<f32> {
    // On `TextEdit::show`'s first (pre-edit) `layouter` call, `entry.ver` is already synchronized by the pre-pass
    // above, so if `FastBuf` has not mutated and non-text parameters match, skip hashing 100–360 KB.
    if unmutated_pre_pass
        && !text.is_empty()
        && entry.ver.is_some()
        && entry.key.1 == wrap.round().to_bits()
        && entry.key.2 == generation
        && entry.key.3 == kind as u8
    {
        return None;
    }
    let path = if text.is_empty() {
        0
    } else if highlight::is_big(text.len()) {
        if highlight::big_highlighted(kind) { 2 } else { 1 }
    } else if highlight::is_incremental_exact(text, kind) {
        3
    } else {
        0
    };
    let code = code.filter(|_| path == 0 || path == 2 || path == 3);
    // Big and exact-incremental notes re-section changed paragraphs via `LineCache::restyle`, not the whole key.
    let epoch = code.filter(|_| path == 0).map_or(0, |c| c.epoch());
    let key = (
        hash_str(text) ^ (highlight::limit() as u64).rotate_left(40) ^ (path as u64).rotate_left(56) ^ epoch.rotate_left(24),
        wrap.round().to_bits(),
        generation,
        kind as u8,
    );
    if entry.key == key {
        return None;
    }
    if entry.key.2 != generation || entry.path != path {
        // Paragraph galleys of an older generation may point into a reset font atlas.
        entry.lines = Default::default();
        entry.code_epoch = code.map_or(0, |c| c.epoch());
    }
    entry.path = path;
    let code_hl = code.map(|c| c as &dyn crate::services::CodeHighlighter);
    let l = if path == 0 {
        entry.big = None;
        let job = highlight::layout_with(text, kind, style, wrap, code_hl);
        entry.lines.layout(ui, job, view)
    } else if path == 3 {
        entry.big = None;
        entry.release();
        let (src, dirty) = highlight::incremental_source(text, kind, style, code_hl);
        entry.lines.layout_big(ui, text, wrap, &src, dirty, view)
    } else {
        let dirty = if path == 2 { entry.big.get_or_insert_with(Default::default).update(entry.lines.text(), text) } else { 0..0 };
        if path == 1 {
            entry.big = None;
        }
        entry.release();
        let src = highlight::big_source(entry.big.as_ref(), style).with_code(code_hl);
        entry.lines.layout_big(ui, text, wrap, &src, dirty, view)
    };
    entry.key = key;
    entry.galley = l.galley;
    // An edit made by the `TextEdit` reports its count change; anything the layout could not compare is recounted.
    match (entry.lines.take_delta(), entry.delta.as_mut()) {
        (Some(d), Some(acc)) if path != 0 => *acc = (acc.0 + d.0, acc.1 + d.1),
        _ => entry.delta = None,
    }
    Some(l.shift)
}

/// Idle layout of placeholders: this many viewport heights above and below the viewport, at most this long per frame.
const PREFETCH_SCREENS: f32 = 3.0;
const PREFETCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(2);

/// The scroll area stops about 50 px short of `content_h - viewport_h`; within this distance the pane counts as at the bottom.
const BOTTOM_SLACK: f32 = 60.0;

fn build_map(galley: &Galley) -> ScrollMap {
    let mut pts = Vec::with_capacity(galley.rows.len());
    let mut line = 0u32;
    let mut at_start = true;
    for r in &galley.rows {
        if at_start {
            pts.push((line as f32, r.pos.y));
            at_start = false;
        }
        if r.ends_with_newline {
            line += 1;
            at_start = true;
        }
    }
    ScrollMap::from_points(pts)
}

/// Byte offset of the `c`-th char (the text length when there are fewer). Whole blocks are skipped by counting char
/// starts, which vectorizes (about 10 GB/s) where `char_indices().nth` decodes every char.
fn char_to_byte(text: &str, c: usize) -> usize {
    const BLOCK: usize = 4096;
    let b = text.as_bytes();
    let is_start = |&x: &u8| (x as i8) >= -0x40;
    let (mut at, mut left) = (0, c);
    while at + BLOCK <= b.len() {
        // u8 partial sums (at most 255 per chunk) vectorize far better than `filter().count()`.
        let n: usize = b[at..at + BLOCK].chunks(255).map(|c| c.iter().map(|x| u8::from(is_start(x))).sum::<u8>() as usize).sum();
        if n > left {
            break;
        }
        left -= n;
        at += BLOCK;
    }
    for (i, x) in b[at..].iter().enumerate() {
        if is_start(x) {
            if left == 0 {
                return at + i;
            }
            left -= 1;
        }
    }
    b.len()
}

/// The range of `new` that differs from `old` (common prefix and suffix cut off, on char boundaries), `None` when equal.
/// Two linear scans over the bytes, memcmp speed.
fn changed_range(old: &str, new: &str) -> Option<std::ops::Range<usize>> {
    const CHUNK: usize = 4096;
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let min_len = a.len().min(b.len());
    let mut p = 0;
    while p + CHUNK <= min_len && a[p..p + CHUNK] == b[p..p + CHUNK] {
        p += CHUNK;
    }
    while p < min_len && a[p] == b[p] {
        p += 1;
    }
    if p == a.len() && p == b.len() {
        return None;
    }
    let max_s = min_len - p;
    let mut s = 0;
    while s + CHUNK <= max_s && a[a.len() - s - CHUNK..a.len() - s] == b[b.len() - s - CHUNK..b.len() - s] {
        s += CHUNK;
    }
    while s < max_s && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    Some(floor_boundary(new, p)..(b.len() - s..=b.len()).find(|&i| new.is_char_boundary(i)).unwrap_or(b.len()))
}

/// Number of `\n` in `text`, 8 to 10 GB/s (a plain `filter().count()` runs at about 3 GB/s).
fn count_newlines(text: &str) -> usize {
    // 255 is the most a u8 sum can hold.
    text.as_bytes().chunks(255).map(|c| c.iter().map(|&b| u8::from(b == b'\n')).sum::<u8>() as usize).sum()
}

/// A `String` as egui text buffer. egui converts char and byte indices by decoding every char before the index (about 4 ms
/// for a caret in the middle of 5 MB, several times per edit); these use `char_to_byte` and `str::chars().count()`.
struct FastBuf<'a>(&'a mut String, &'a std::cell::Cell<bool>);

impl TextBuffer for FastBuf<'_> {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.0
    }
    fn insert_text(&mut self, text: &str, at: CharIndex) -> usize {
        if !text.is_empty() {
            self.1.set(true);
            self.0.insert_str(char_to_byte(self.0, at.0), text);
        }
        text.chars().count()
    }
    fn delete_char_range(&mut self, r: std::ops::Range<CharIndex>) {
        // egui calls this with an empty range on every typed char (to drop an empty selection).
        if r.end.0 > r.start.0 {
            self.1.set(true);
            let a = char_to_byte(self.0, r.start.0);
            let b = a + char_to_byte(&self.0[a..], r.end.0 - r.start.0);
            self.0.drain(a..b);
        }
    }
    fn byte_index_from_char_index(&self, c: CharIndex) -> ByteIndex {
        ByteIndex(char_to_byte(self.0, c.0))
    }
    fn char_index_from_byte_index(&self, b: ByteIndex) -> CharIndex {
        let t = self.0.as_str();
        // Same as egui: a byte index that is past the end or inside a char counts all chars.
        CharIndex(if t.is_char_boundary(b.0) { t[..b.0].chars().count() } else { t.chars().count() })
    }
    fn char_range(&self, r: std::ops::Range<CharIndex>) -> &str {
        let a = char_to_byte(self.0, r.start.0);
        &self.0[a..a + char_to_byte(&self.0[a..], r.end.0.saturating_sub(r.start.0))]
    }
    fn clear(&mut self) {
        if !self.0.is_empty() {
            self.1.set(true);
            self.0.clear();
        }
    }
    fn replace_with(&mut self, text: &str) {
        self.1.set(true);
        text.clone_into(self.0);
    }
    fn take(&mut self) -> String {
        self.1.set(true);
        std::mem::take(self.0)
    }
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<FastBuf<'static>>()
    }
}

fn is_undo_redo(e: &egui::Event) -> bool {
    matches!(e, egui::Event::Key { key: egui::Key::Z | egui::Key::Y, pressed: true, modifiers, .. } if modifiers.command)
}

fn byte_to_char(text: &str, b: usize) -> usize {
    text[..floor_boundary(text, b)].chars().count()
}

/// egui's own undo history (default 100 steps) keeps a full copy of the text per step: about 64 MB at most, in coarse
/// size classes. The cap is set once per `TextEdit` (when it first exists, its history empty); replacing the undoer
/// later would wipe the history, so a note that grows or shrinks afterwards keeps the cap it started with.
const UNDO_STEPS: usize = 100;

fn undo_steps(text_len: usize) -> usize {
    if text_len < 655_360 {
        UNDO_STEPS
    } else if text_len < 2_097_152 {
        32
    } else if text_len < 8_388_608 {
        8
    } else {
        2
    }
}

/// Caps the undo history of the `TextEdit` `id` by the note's size, once (see `UNDO_STEPS`).
fn limit_undo(ctx: &egui::Context, id: Id, entry: &mut Cached, nav_gen: u64, text_len: usize) {
    if entry.undo_capped != Some(nav_gen)
        && let Some(mut st) = TextEdit::load_state(ctx, id)
    {
        let settings = egui::util::undoer::Settings { max_undos: undo_steps(text_len), ..Default::default() };
        st.set_undoer(egui::util::undoer::Undoer::with_settings(settings));
        st.store(ctx, id);
        entry.undo_capped = Some(nav_gen);
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
    let mut feats = std::mem::take(&mut state.ui.editor_features);
    feats.before(ui, state, tab_index);
    let frame = show_inner(ui, state, tab_index);
    if let Some(f) = frame {
        feats.after(ui, state, tab_index, &f);
        tree_drop(ui, state, tab_index, &f);
        #[cfg(feature = "git")]
        crate::git_hunk::after_editor(state, tab_index, &f);
    }
    state.ui.editor_features = feats;
}

/// A file dragged out of the tree over a writable Markdown source pane: a caret marks the char under the pointer, and
/// releasing inserts a link there (one undoable edit, caret behind it). Folders, `.typ` and read-only notes are no target.
fn tree_drop(ui: &mut egui::Ui, state: &mut AppState, ti: usize, f: &EditorFrame) {
    let Some(doc) = state.tabs.tabs.get(ti).and_then(|t| state.docs.get(&t.path)).filter(|d| d.kind == FileKind::Markdown && !d.read_only) else { return };
    let (path, area) = (doc.path.clone(), f.viewport);
    let ctx = ui.ctx().clone();
    let at = |p: egui::Pos2| f.galley.cursor_from_pos(p - f.galley_pos);
    if let Some(p) = ctx.pointer_latest_pos().filter(|p| area.contains(*p) && state.ui.files.dragged_file(&ctx)) {
        state.ui.files.accept_drop(&ctx);
        let r = f.galley.pos_from_cursor(at(p)).translate(f.galley_pos.to_vec2());
        ui.painter().line_segment([r.center_top(), r.center_bottom()], egui::Stroke::new(2.0, ui.visuals().selection.stroke.color));
    }
    let Some(p) = ctx.pointer_latest_pos() else { return };
    let Some(dropped) = state.ui.files.take_drop(&ctx, area) else { return };
    let link = state.indexer.index().try_read().map(|ix| wiki_model::dropped_link(&ix, &dropped));
    let Some(link) = link else { return state.status.error("The index is busy: drop the file again") };
    let Some(link) = link else {
        let name = dropped.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return state.status.error(format!("Cannot link \"{name}\": rename it without # | ^ [ ]"));
    };
    let b = state.docs.get(&path).map_or(0, |d| char_to_byte(&d.text, at(p).index.0));
    state.commands.push(Command::ApplyEdit { path, range: b..b, insert: link, select: None });
}

fn show_inner(ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) -> Option<EditorFrame> {
    let take_focus = std::mem::take(&mut state.focus_editor);
    #[cfg(feature = "codehl")]
    let code = state.modules.enabled(iron_sheets_app_core::modules::CODEHL).then(|| crate::preview::markdown::codehl::SyntectHighlighter::shared(&state.waker));
    #[cfg(not(feature = "codehl"))]
    let code: Option<std::sync::Arc<crate::preview::markdown::codehl::SyntectHighlighter>> = None;
    let code = code.as_deref();
    let wrap_on = state.config.editor_wrap;
    let show_numbers = state.config.show_line_numbers;
    let sync_on = state.config.sync_scroll;
    let AppState {
        docs,
        tabs,
        ui: ustate,
        #[cfg(feature = "git")]
        modules,
        ..
    } = state;
    let (theme, editor) = (&mut ustate.theme, &mut ustate.editor);
    let tab = tabs.tabs.get_mut(tab_index)?;
    let doc = docs.get_mut(&tab.path)?;
    let style: EditorStyle = theme.editor.clone();
    let generation = theme.generation;
    let id = Id::NULL.with(("editor", tab.id, tab.nav_gen));
    let tab_id = tab.id;
    let kind: FileKind = doc.kind;
    let code = code.filter(|_| kind == FileKind::Markdown);
    // Finished highlighter results change the layout: they are part of the layout keys.
    let epoch = code.map_or(0, |c| c.epoch());
    let read_only = doc.read_only;

    let pending_bytes = tab.pending_cursor.take();
    let pending = pending_bytes.as_ref().map(|r| (byte_to_char(&doc.text, r.start), byte_to_char(&doc.text, r.end)));
    if take_focus || editor.focused != Some((tab_id, tab.nav_gen)) {
        editor.focused = Some((tab_id, tab.nav_gen));
        if !read_only {
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
    let entry = editor.cache.entry(tab_id).or_insert_with(|| Cached {
        key: (0, 0, u64::MAX, 0),
        galley: ui.fonts_mut(|f| f.layout_job(Default::default())),
        lines: Default::default(),
        map_for: 0,
        ver: None,
        wrap_used: 0.0,
        big: None,
        path: 0,
        empty: ui.fonts_mut(|f| f.layout_job(Default::default())),
        delta: None,
        undo_capped: None,
        code_epoch: 0,
        line_count: None,
    });
    let mut scroll = std::mem::take(&mut tab.editor_scroll);

    // Pre-pass: lay out what the viewport and the caret need before the `TextEdit` runs, so that heights changing
    // above the viewport can be compensated in the scroll offset of this very frame.
    let avail_w = ui.available_width();
    let wrap_pred = if wrap_on { (avail_w - editor.wrap_delta).max(32.0) } else { f32::INFINITY };
    let view_h = if scroll.viewport_h > 1.0 { scroll.viewport_h } else { ui.available_height() };
    let view_y = scroll.force.unwrap_or(scroll.offset);
    let caret = |b: usize| Some(b.min(doc.text.len()));
    let carets = match &tab.cursor {
        Some(r) => [caret(r.start), caret(r.end)],
        None => [None; 2],
    };
    let target = pending_bytes.as_ref().and_then(|r| caret(r.start));
    let mut view = paragraphs::View { y: view_y, h: view_h, carets, target };
    let nav_gen = tab.nav_gen;
    let ver_of = |doc: &iron_sheets_app_core::docs::Doc, wrap: f32| {
        (
            nav_gen,
            doc.version,
            doc.text.len(),
            wrap.round().to_bits(),
            generation,
            kind as u8,
            (highlight::limit() | ((highlight::big_partial() as usize) << 62)) ^ (epoch as usize).rotate_left(20),
        )
    };
    let mut pin_bottom = false;
    // The source line -> y map serves the scroll sync and keeping the top line on a font zoom; it costs O(rows) per layout
    // change, so big notes only build it with a preview beside the editor (shortcut: no top-line anchor on zoom there).
    let want_map = tab.view == iron_sheets_core::config::ViewMode::Split || !highlight::is_big(doc.text.len());
    // In split view with scroll sync the preview and the editor negotiate the position; laying out paragraphs on the side
    // while they do changes the heights under that negotiation (the sync-to-bottom test), so idle layout waits.
    let follows_preview = sync_on && tab.view == iron_sheets_core::config::ViewMode::Split;
    // No input this frame: time for idle work (prefetching layouts).
    let busy = ui.input(|i| !i.events.is_empty() || i.smooth_scroll_delta != egui::Vec2::ZERO || i.pointer.is_moving());
    let code_hl = code.map(|c| c as &dyn crate::services::CodeHighlighter);
    if !doc.text.is_empty() {
        let ver = ver_of(doc, wrap_pred);
        if let Some(c) = code.filter(|_| entry.path == 2 || entry.path == 3)
            && std::mem::replace(&mut entry.code_epoch, c.epoch()) != c.epoch()
        {
            entry.lines.restyle();
        }
        let before = entry.lines.serial();
        let before_h = entry.galley.rect.height();
        let mut shift = 0.0;
        if entry.ver == Some(ver) {
            if entry.lines.needs_focus(&view) {
                entry.release();
                let src = lazy_sections(entry.path, entry.big.as_ref(), kind, &style, code_hl);
                let src = src.as_ref().map(|s| s as &dyn paragraphs::Sections);
                if let Some(l) = entry.lines.refocus_with(ui, &view, src) {
                    entry.galley = l.galley;
                    shift = l.shift;
                }
                entry.restore();
            }
        } else {
            shift = relayout(entry, ui, &doc.text, wrap_pred, kind, &style, generation, &view, code, false).unwrap_or(0.0);
            if entry.lines.needs_focus(&view) {
                entry.release();
                let src = lazy_sections(entry.path, entry.big.as_ref(), kind, &style, code_hl);
                let src = src.as_ref().map(|s| s as &dyn paragraphs::Sections);
                if let Some(l) = entry.lines.refocus_with(ui, &view, src) {
                    entry.galley = l.galley;
                    shift += l.shift;
                }
                entry.restore();
            }
            entry.ver = Some(ver);
        }
        if !busy && !follows_preview && entry.lines.serial() == before {
            // Idle: lay out the paragraphs around the viewport so scrolling meets exact rows.
            entry.release();
            let src = lazy_sections(entry.path, entry.big.as_ref(), kind, &style, code_hl);
            let src = src.as_ref().map(|s| s as &dyn paragraphs::Sections);
            let (l, more) = entry.lines.prefetch(ui, &view, src, PREFETCH_SCREENS, PREFETCH_BUDGET);
            if let Some(l) = l {
                entry.galley = l.galley;
                shift = l.shift;
            }
            entry.restore();
            if more {
                ui.ctx().request_repaint();
            }
        }
        if entry.lines.serial() != before {
            // Estimated heights were replaced by real ones (or the wrap width changed): keep the same content in view,
            // or, when the preview drives this pane, the same source line / edge.
            if want_map {
                entry.map_for = entry.lines.serial();
                scroll.map = build_map(&entry.galley);
            }
            let follow = if sync_on && tab.view == iron_sheets_core::config::ViewMode::Split { scroll.follow } else { None };
            // Scroll range of the new layout: the old one (it includes padding the galley does not) plus the height change.
            let new_max = (scroll.max_offset() + entry.galley.rect.height() - before_h).max(0.0);
            pin_bottom = follow.is_none() && scroll.content_h > 0.0 && scroll.max_offset() > 0.0 && scroll.offset >= scroll.max_offset() - BOTTOM_SLACK;
            let target = match follow {
                Some(f) => Some(scroll.follow_offset(f, new_max)),
                // At the old bottom: stay at the new one (the estimated range was too short or too long).
                None if scroll.content_h > 0.0 && scroll.offset >= scroll.max_offset() - BOTTOM_SLACK && scroll.max_offset() > 0.0 => Some(new_max),
                None if shift.abs() > 0.5 => Some((view_y + shift).max(0.0)),
                None => None,
            };
            if let Some(t) = target.filter(|t| (t - view_y).abs() > 0.5) {
                view.y = t;
                scroll.force = Some(t);
            }
        }
    } else {
        entry.ver = None;
    }
    // Gutter width (0 = no gutter, and none of its code runs). The line count is cached per document version.
    // shortcut: one newline count per edit while line numbers are on (~0.2 ms at 560 KB, ~3 ms at 10 MB, measured); the
    // widget edits without an edit range, so an incremental count needs the big-layout delta extended; upgrade for multi-MB notes.
    let numbers_w = if show_numbers {
        let (v, n) = match entry.line_count {
            Some((v, n)) if v == doc.version => (v, n),
            _ => (doc.version, count_newlines(&doc.text) + 1),
        };
        entry.line_count = Some((v, n));
        gutter::width(ui, &style.font_id, n)
    } else {
        0.0
    };
    #[cfg(not(feature = "git"))]
    let diff_marks: Arc<[gutter::DiffMark]> = Arc::from([]);
    #[cfg(feature = "git")]
    let diff_marks: Arc<[gutter::DiffMark]> = if let Some(git) = modules.get::<iron_sheets_app_core::git::GitModule>(iron_sheets_app_core::modules::GIT) {
        let (marks, repaint_after) =
            git.diff_marks(&tab.path, doc.version, doc.text.len(), doc.is_dirty(), doc.last_edit, || doc.text.clone());
        if let Some(d) = repaint_after {
            ui.ctx().request_repaint_after(d);
        }
        marks
    } else {
        Arc::from([])
    };
    let diff_w = if diff_marks.is_empty() { 0.0 } else { gutter::DIFF_W };
    let gutter_w = numbers_w + diff_w;
    let version_before = doc.version;
    entry.delta = Some((0, 0));
    let mutated = std::cell::Cell::new(false);
    let mut layouter = |ui: &egui::Ui, buf: &dyn TextBuffer, wrap: f32| -> Arc<Galley> {
        let wrap = if wrap_on { wrap } else { f32::INFINITY };
        entry.wrap_used = wrap;
        relayout(entry, ui, buf.as_str(), wrap, kind, &style, generation, &view, code, !mutated.get());
        entry.galley.clone()
    };

    let row_h = style.line_height.max(1.0);
    let rows = (ui.available_height() / row_h).ceil().max(1.0) as usize;
    // Undo and redo put the caret of the old snapshot back; the caret should be at the change instead.
    // A history step that only moved the caret changes no text: the same key is sent again next frame, so one press reaches a text change.
    // Keyed by tab and only replayed into this focused editor without a new undo/redo press of its own.
    let again_id = Id::NULL.with(("undo_again", tab_id));
    if let Some(Some(e)) = ui.data_mut(|d| d.remove_temp::<Option<egui::Event>>(again_id))
        && ui.memory(|m| m.has_focus(id))
        && !ui.input(|i| i.events.iter().any(is_undo_redo))
    {
        ui.input_mut(|i| i.events.push(e));
    }
    if ui.input(|i| i.pointer.secondary_pressed()) {
        let sel = tab.cursor.clone();
        ui.data_mut(|d| d.insert_temp(features::ctxmenu::press_sel_id(), sel));
    }
    let menu_caret = ui.data_mut(|d| d.remove_temp::<bool>(features::ctxmenu::no_scroll_id())).unwrap_or(false) && pending.is_some();
    let undo_ev = if read_only { None } else { ui.input(|i| i.events.iter().find(|e| is_undo_redo(e)).cloned()) };
    let before_undo = undo_ev.as_ref().map(|_| (doc.text.clone(), TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range())));
    let mut chain = false;
    let mut viewport = ui.max_rect();
    let hunk_click = std::cell::Cell::new(None);
    let mut buf = FastBuf(&mut doc.text, &mutated);
    // Smooth caret: egui's own is hidden (transparent, no blink wakeups) and `crate::caret` paints one after the pane.
    // Not while an IME composes: egui draws the preedit caret with the same stroke, so egui's own caret stays.
    let mut smooth = !read_only && crate::anim::enabled(crate::anim::Group::Caret) && ui.style().animation_time > 0.0;
    if smooth {
        ui.input(|i| editor.caret.track_ime(&i.events));
        smooth = !editor.caret.composing;
    }
    if smooth && pending.is_some() {
        editor.caret.jump();
    }
    let pane = scroll_pane(ui, ("editor_scroll", tab_id), &mut scroll, |ui| {
        viewport = ui.clip_rect();
        if smooth {
            let c = &mut ui.visuals_mut().text_cursor;
            (c.blink, c.stroke.color) = (false, egui::Color32::TRANSPARENT);
        }
        let edit = TextEdit::multiline(&mut buf)
            .id(id)
            .frame(egui::Frame::NONE)
            .desired_width(f32::INFINITY)
            .desired_rows(rows)
            .lock_focus(true)
            .cursor_at_end(false)
            .interactive(!read_only)
            .layouter(&mut layouter);
        let show_edit = |ui: &mut egui::Ui| {
            let out = if wrap_on {
                edit.show(ui)
            } else {
                egui::ScrollArea::horizontal().id_salt(("editor_hscroll", tab_id)).auto_shrink([false, true]).show(ui, |ui| edit.show(ui)).inner
            };
            crate::theme::name_field(&out.response, "Note editor");
            out
        };
        let out = if gutter_w > 0.0 {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let gutter_left = ui.cursor().min.x;
                let right = gutter_left + numbers_w - 6.0;
                let diff_x = gutter_left + numbers_w + 1.0;
                ui.allocate_space(egui::vec2(gutter_w, 0.0));
                let out = show_edit(ui);
                let nid = Id::NULL.with(("gutter_rows", tab_id));
                let mut numbers: gutter::Numbers = ui.data_mut(|d| d.remove_temp(nid)).unwrap_or_default();
                hunk_click.set(gutter::paint(
                    ui,
                    &out.galley,
                    out.galley_pos,
                    show_numbers.then_some((right, &style.font_id, ui.visuals().weak_text_color())),
                    (!diff_marks.is_empty()).then_some((diff_x, &diff_marks)),
                    version_before,
                    &mut numbers,
                ));
                ui.data_mut(|d| d.insert_temp(nid, numbers));
                out
            })
            .inner
        } else {
            show_edit(ui)
        };
        // After an undo or redo: collapsed at the end of re-inserted text, else at the spot of the removed text; the view moves only when that is off-screen.
        let undone = before_undo.as_ref().and_then(|(old, sel)| match changed_range(old, buf.0.as_str()) {
            Some(r) => {
                let c = byte_to_char(buf.0.as_str(), if r.is_empty() { r.start } else { r.end });
                Some(((c, c), None))
            }
            None => {
                chain = *sel != out.state.cursor.char_range();
                None
            }
        });
        if let Some(((a, b), align)) = pending.map(|p| (p, if menu_caret { None } else { Some(egui::Align::Center) })).or(undone) {
            let mut st = out.state.clone();
            st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
            st.store(ui.ctx(), out.response.id);
            let rect = out.galley.pos_from_cursor(CCursor::new(b)).translate(out.galley_pos.to_vec2());
            ui.scroll_to_rect(rect, align);
            ui.memory_mut(|m| m.request_focus(out.response.id));
        }
        out
    });
    tab.editor_scroll = scroll;
    if chain {
        ui.data_mut(|d| d.insert_temp(again_id, undo_ev));
        ui.ctx().request_repaint();
    }
    // Right after the first `show` of this `TextEdit`: its state exists and has no history yet.
    limit_undo(ui.ctx(), id, entry, nav_gen, doc.text.len());
    let changed = pane.response.changed();
    let galley = pane.galley.clone();
    // A drag-and-drop paste changes the text without a layout: then the counts are recounted in full and the next
    // frame lays the text out again instead of trusting the stale galley.
    let synced = !changed || entry.lines.text() == Some(doc.text.as_str());
    if changed {
        doc.touch();
        if let Some(d) = entry.delta.take().filter(|_| synced) {
            doc.add_count_delta(version_before, d);
        }
        #[cfg(feature = "git")]
        if let Some(git) = modules.get::<iron_sheets_app_core::git::GitModule>(iron_sheets_app_core::modules::GIT)
            && let Some(d) = git.diff_repaint_delay(&tab.path, doc.text.len(), doc.is_dirty(), doc.last_edit)
        {
            ui.ctx().request_repaint_after(d);
        }
    }
    if wrap_on && entry.wrap_used.is_finite() {
        editor.wrap_delta = avail_w - entry.wrap_used;
    }
    entry.ver = if synced { Some(ver_of(doc, entry.wrap_used)) } else { None };
    if !synced {
        ui.ctx().request_repaint();
    }
    // Was at the bottom and the range changed: the pre-pass estimate of the new range can be off by padding; settle on the real one.
    if pin_bottom && tab.editor_scroll.force.is_none() && tab.editor_scroll.offset < tab.editor_scroll.max_offset() {
        tab.editor_scroll.force = Some(tab.editor_scroll.max_offset());
        ui.ctx().request_repaint();
    }
    // The viewport may have moved during the frame (scroll_to, keys): paint what is exact now, finish next frame.
    let now = paragraphs::View { y: tab.editor_scroll.offset, h: view_h, carets: view.carets, target: None };
    if entry.lines.needs_focus(&now) || (busy && entry.lines.prefetch_pending(&now, PREFETCH_SCREENS) > 0) {
        ui.ctx().request_repaint();
    }
    if want_map && entry.lines.serial() != entry.map_for {
        entry.map_for = entry.lines.serial();
        tab.editor_scroll.map = build_map(&galley);
    }
    // Selection in bytes, recomputed only when it moved.
    if let Some(r) = pane.cursor_range {
        let r = r.as_sorted_char_range();
        let key = (r.start.0, r.end.0);
        if editor.last_sel.get(&tab_id) != Some(&key) || changed || tab.cursor.is_none() {
            editor.last_sel.insert(tab_id, key);
            let t = &doc.text;
            let a = char_to_byte(t, key.0);
            tab.cursor = Some(a..if key.1 == key.0 { a } else { char_to_byte(t, key.1) });
        }
    }
    if smooth {
        let focused = ui.memory(|m| m.has_focus(id)) && ui.input(|i| i.focused);
        let mut clip = viewport;
        clip.min.x += gutter_w; // a horizontal glide (wrap off) must not draw over the gutter
        let draw = crate::caret::Draw { range: pane.cursor_range.filter(|_| focused), key: (tab_id, tab.nav_gen), galley: &galley, galley_pos: pane.galley_pos, clip, changed, view_h, glide_ms: state.config.caret_glide_ms, trail_ms: if !state.config.professional && state.config.caret_trail { state.config.caret_trail_ms } else { 0 }, trail_opacity: state.config.caret_trail_opacity, trail_width: state.config.caret_trail_width };
        editor.caret.paint(ui, draw);
    } else {
        editor.caret.hide();
    }
    let cursor_rect = pane.cursor_range.map(|r| galley.pos_from_cursor(r.primary).translate(pane.galley_pos.to_vec2()));
    Some(EditorFrame { galley, galley_pos: pane.galley_pos, cursor_rect, response: pane.response.response.clone(), changed, viewport, hunk_click: hunk_click.get() })
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn lazy_sections_report_pending_code_colours() {
        use iron_sheets_app_core::services::{CodeHighlighter, CodeHl};
        use paragraphs::Sections;
        struct Waiting;
        impl CodeHighlighter for Waiting {
            fn get(&self, _: &str, _: &str) -> CodeHl {
                CodeHl::Pending(None)
            }
        }
        let style = EditorStyle::from_model(&iron_sheets_app_core::theme_model::editor_colors(false, 14.0));
        let text = "```rs\nfn a() {}\n```\n";
        let line = text.find("fn").unwrap_or(0);
        let mut out = Vec::new();
        // Path 3 (incremental); path 1 needs a WindowHighlighter (big notes) and forwards the same way.
        let src = lazy_sections(3, None, FileKind::Markdown, &style, Some(&Waiting)).expect("source");
        src.sections(text, line, line + "fn a() {}".len(), &mut out);
        assert!(src.pending(), "pending code colours must reach the paragraph cache");
    }

    #[test]
    fn changed_range_cuts_prefix_and_suffix() {
        assert_eq!(changed_range("abcdef", "abXYcdef"), Some(2..4));
        assert_eq!(changed_range("abXYcdef", "abcdef"), Some(2..2));
        assert_eq!(changed_range("same", "same"), None);
        assert_eq!(changed_range("aaa", "aaaa"), Some(3..4));
        // never inside a multibyte char
        let r = changed_range("é", "è").expect("differs");
        assert!("è".is_char_boundary(r.start) && "è".is_char_boundary(r.end));
    }

    #[test]
    fn count_newlines_matches_filter() {
        let t = "a\n\nbé\n".repeat(1000) + "tail";
        assert_eq!(count_newlines(&t), t.bytes().filter(|&b| b == b'\n').count());
        assert_eq!(count_newlines(""), 0);
        let big = "\n".repeat(1000);
        assert_eq!(count_newlines(&big), 1000);
    }

    #[test]
    fn fast_buf_matches_string_buffer() {
        // Several 4 KB blocks, so the block skip in `char_to_byte` runs more than once.
        let base = "aé字😀\nline two\n字字".repeat(1200);
        let n = base.chars().count();
        for (at, len) in [(0, 0), (5, 3), (4095, 2), (4097, 700), (n - 3, 3), (n, 0), (n + 5, 0)] {
            let (mut a, mut b) = (base.clone(), base.clone());
            let mutated = std::cell::Cell::new(false);
            let mut fast = FastBuf(&mut a, &mutated);
            let r = CharIndex(at)..CharIndex(at + len);
            assert_eq!(fast.char_range(CharIndex(at.min(n))..CharIndex((at + len).min(n))), b.char_range(CharIndex(at.min(n))..CharIndex((at + len).min(n))));
            fast.delete_char_range(r.clone());
            b.delete_char_range(r);
            assert_eq!(*fast.0, b, "delete {at}+{len}");
            assert_eq!(fast.insert_text("é😀x", CharIndex(at)), b.insert_text("é😀x", CharIndex(at)));
            assert_eq!(*fast.0, b, "insert {at}");
            for byte in [0, 1, 2, 7, b.len() / 2, b.len(), b.len() + 3] {
                assert_eq!(fast.char_index_from_byte_index(ByteIndex(byte)), b.char_index_from_byte_index(ByteIndex(byte)), "byte {byte}");
            }
            assert_eq!(fast.byte_index_from_char_index(CharIndex(at)), b.byte_index_from_char_index(CharIndex(at)));
        }
    }

    #[test]
    fn changed_range_on_5mb_is_fast() {
        let a = "line of a note with some words\n".repeat(170_000);
        let b = format!("{}X{}", &a[..2_000_000], &a[2_000_000..]);
        let t = std::time::Instant::now();
        let r = changed_range(&a, &b);
        eprintln!("changed_range 5 MB: {:?}", t.elapsed());
        assert_eq!(r, Some(2_000_000..2_000_001));
    }

    #[test]
    fn char_to_byte_matches_char_indices() {
        let mut t = String::new();
        for i in 0..6000 {
            t.push_str(["a", "é", "字", "😀", "\n"][i * 7 % 5]);
        }
        for c in (0..t.chars().count() + 3).step_by(7).chain([4095, 4096, 4097, 8191, 8192]) {
            assert_eq!(char_to_byte(&t, c), t.char_indices().nth(c).map_or(t.len(), |(i, _)| i), "char {c}");
        }
        assert_eq!(char_to_byte("", 0), 0);
    }

    #[test]
    fn floors_inside_multibyte() {
        let t = "aé字";
        assert_eq!(floor_boundary(t, 2), 1);
        assert_eq!(floor_boundary(t, 5), 3);
        assert_eq!(floor_boundary(t, 99), t.len());
        assert_eq!(byte_to_char(t, 2), 1);
    }

    struct TestDir(std::path::PathBuf);
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup_editor_test(name: &str, text: &str) -> (TestDir, egui::Context, AppState, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        struct NoWake;
        impl iron_sheets_app_core::platform::Waker for NoWake {
            fn wake(&self) {}
        }
        let dir = std::env::temp_dir().join(format!("iron-sheets-editor-test-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let root = dir.join("notes");
        std::fs::create_dir_all(&root).expect("mkdir");
        let path = root.join(name);
        std::fs::write(&path, text).expect("write");
        let mut opts = iron_sheets_app_core::state::AppOptions::new(iron_sheets_core::paths::AppDirs::at(&dir.join("home")));
        opts.config.restore_session = false;
        opts.config.autosave = false;
        opts.paths = vec![root];
        let mut state = crate::state::new_state(Arc::new(NoWake), opts);
        state.docs.open(&path).expect("open");
        state.tabs.open(&path, true, true, iron_sheets_core::config::ViewMode::Source);
        let ctx = egui::Context::default();
        ctx.disable_accesskit();
        (TestDir(dir), ctx, state, path)
    }

    fn run_editor_frame(ctx: &egui::Context, state: &mut AppState, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| show(ui, state, 0));
        });
        out.textures_delta.clear();
    }

    #[test]
    fn incremental_exact_preserves_counts_without_recounting() {
        let mut text = String::new();
        let mut i = 0usize;
        while text.len() < 100 * 1024 {
            text.push_str(&format!(
                "## Heading {i}\n\nParagraph {i}: lorem ipsum\u{a0}dolor → sit amet, **bold** `code`.\n\n```rust\nfn f{i}() {{ let a = {i}; }}\n```\n\n"
            ));
            i += 1;
        }
        let (_dir, ctx, mut state, path) = setup_editor_test("Note.md", &text);
        let mid = (0..text.len() / 2).rev().find(|&k| text.is_char_boundary(k)).unwrap_or(0);
        state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(mid..mid);
        for _ in 0..4 {
            run_editor_frame(&ctx, &mut state, vec![]);
        }
        let tab_id = state.tabs.active_tab().expect("tab").id;
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(3));
        assert!(state.ui.editor.stats(tab_id).expect("stats").placeholders > 500);
        let initial_counts = state.docs.get(&path).expect("doc").counts();
        assert_eq!(initial_counts, iron_sheets_app_core::docs::count_text(&text));
        let base_runs = state.docs.get(&path).expect("doc").full_count_runs();

        let edits = ["a", " ", "é", "\u{a0}", "  ", "→→", "x y"];
        for (step, ev) in edits.iter().enumerate() {
            run_editor_frame(&ctx, &mut state, vec![egui::Event::Text((*ev).into())]);
            if step % 3 == 1 {
                run_editor_frame(
                    &ctx,
                    &mut state,
                    vec![
                        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
                        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE },
                    ],
                );
            }
            if step % 3 == 2 {
                run_editor_frame(
                    &ctx,
                    &mut state,
                    vec![
                        egui::Event::Key { key: egui::Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
                        egui::Event::Key { key: egui::Key::Backspace, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE },
                    ],
                );
            }
            let doc = state.docs.get(&path).expect("doc");
            assert_eq!(doc.counts(), iron_sheets_app_core::docs::count_text(&doc.text), "step {step}");
            assert_eq!(doc.full_count_runs(), base_runs, "step {step}: full recount triggered");
            let st = state.ui.editor.stats(tab_id).expect("stats");
            assert_eq!(st.overlapping, 0, "step {step}: overlapping rows");
            assert_eq!(st.chars, doc.text.chars().count(), "step {step}: char count");
        }
        assert!(state.docs.get(&path).expect("doc").text.len() > text.len());
    }

    #[test]
    fn boundary_transitions_and_fence_open_close_stay_consistent() {
        let small = "# Small\n\nSome **prose** here.\n".to_string();
        let (_dir, ctx, mut state, path) = setup_editor_test("Trans.md", &small);
        for _ in 0..3 {
            run_editor_frame(&ctx, &mut state, vec![]);
        }
        let tab_id = state.tabs.active_tab().expect("tab").id;
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(0));

        // < 4 KB -> 4 KB..300 KB (incremental exact path = 3)
        let med_chunk = "Paragraph line with **bold** text and `code`.\n\n".repeat(160); // ~7.5 KB
        run_editor_frame(&ctx, &mut state, vec![egui::Event::Paste(med_chunk.clone())]);
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(3));
        {
            let doc = state.docs.get(&path).expect("doc");
            assert_eq!(doc.counts(), iron_sheets_app_core::docs::count_text(&doc.text));
            let st = state.ui.editor.stats(tab_id).expect("stats");
            assert_eq!(st.overlapping, 0);
            assert_eq!(st.chars, doc.text.chars().count());
        }

        // Open and close a fence inside path == 3
        let at = state.docs.get(&path).expect("doc").text.find("Paragraph line").expect("anchor");
        state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(at..at);
        run_editor_frame(&ctx, &mut state, vec![]);
        run_editor_frame(&ctx, &mut state, vec![egui::Event::Paste("```\n".to_string())]);
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").overlapping, 0);
        for _ in 0..4 {
            run_editor_frame(
                &ctx,
                &mut state,
                vec![
                    egui::Event::Key { key: egui::Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
                    egui::Event::Key { key: egui::Key::Backspace, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE },
                ],
            );
        }
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").overlapping, 0);

        // 4 KB..300 KB -> > 300 KB (big highlighted path = 2)
        let huge_chunk = med_chunk.repeat(45); // ~340 KB
        run_editor_frame(&ctx, &mut state, vec![egui::Event::Paste(huge_chunk)]);
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(2));
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").overlapping, 0);

        // > 300 KB -> 4 KB..300 KB -> < 4 KB -> empty -> 4 KB..300 KB
        let shrink_to_med = |state: &mut AppState, new_text: String| {
            let doc = state.docs.get_mut(&path).expect("doc");
            let len = doc.text.len();
            doc.apply_edit(0..len, &new_text);
        };
        shrink_to_med(&mut state, med_chunk.clone());
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(3));
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").overlapping, 0);

        shrink_to_med(&mut state, small);
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(0));
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").overlapping, 0);

        shrink_to_med(&mut state, String::new());
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(0));
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").chars, 0);

        run_editor_frame(&ctx, &mut state, vec![egui::Event::Paste(med_chunk)]);
        run_editor_frame(&ctx, &mut state, vec![]);
        assert_eq!(state.ui.editor.cache.get(&tab_id).map(|c| c.path), Some(3));
        let doc = state.docs.get(&path).expect("doc");
        assert_eq!(doc.counts(), iron_sheets_app_core::docs::count_text(&doc.text));
        assert_eq!(state.ui.editor.stats(tab_id).expect("stats").chars, doc.text.chars().count());
    }
}
