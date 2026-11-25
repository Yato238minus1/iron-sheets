//! Compiling documents: world setup, diagnostics, scroll anchors.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use rustc_hash::FxHashMap;
use typst::World;
use typst::WorldExt;
use typst::diag::{Severity, SourceDiagnostic, Warned};
use typst::layout::{Frame, FrameItem, Point, Transform};
use typst::syntax::{DiagSpan, DiagSpanKind, FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst_layout::PagedDocument;

use crate::shared::Shared;
use crate::world::{DocFiles, DocWorld};
use crate::{Anchor, Diagnostic};

/// What the editor handed us for one compile.
pub struct Input {
    pub path: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub text: String,
    pub version: u64,
}

/// The result of the last successful compile of a document.
pub struct Good {
    pub version: u64,
    /// Internal counter, increments with every successful compile.
    pub generation: u64,
    pub doc: Arc<PagedDocument>,
    pub world: Arc<DocWorld>,
    pub pages: Vec<[f32; 2]>,
    pub anchors: Vec<Anchor>,
}

/// Document, world, page sizes and anchors of a successful compile.
pub type GoodParts = (Arc<PagedDocument>, Arc<DocWorld>, Vec<[f32; 2]>, Vec<Anchor>);

pub struct Outcome {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub good: Option<GoodParts>,
}

const MAX_DIAGNOSTICS: usize = 200;

static UNTITLED: LazyLock<FileId> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    let path = VirtualPath::new("untitled.typ").expect("constant path is valid");
    FileId::unique(RootedPath::new(VirtualRoot::Project, path))
});

/// Determines the main file id and the effective project root.
fn main_id(path: &Option<PathBuf>, root: &Option<PathBuf>) -> (FileId, Option<PathBuf>) {
    let Some(path) = path else {
        return (*UNTITLED, root.clone());
    };
    let effective = root.clone().or_else(|| path.parent().map(Path::to_path_buf));
    if let Some(root) = &effective
        && let Ok(vpath) = VirtualPath::virtualize(root, path)
    {
        return (RootedPath::new(VirtualRoot::Project, vpath).intern(), effective);
    }
    // The file lies outside the given root: use its own folder.
    let parent = path.parent().map(Path::to_path_buf);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("untitled.typ");
    match VirtualPath::new(name) {
        Ok(vpath) => (RootedPath::new(VirtualRoot::Project, vpath).intern(), parent),
        Err(_) => (*UNTITLED, parent),
    }
}

/// Compiles `input`. `main_source` is the engine-owned persistent main source,
/// edited in place for incremental parsing.
pub fn compile(shared: &Arc<Shared>, input: &Input, files: Arc<DocFiles>, main_source: &mut Option<Source>, epoch: u64) -> Outcome {
    let (id, root) = main_id(&input.path, &input.root);
    let source = match main_source {
        Some(source) if source.id() == id => {
            source.replace(&input.text);
            source.clone()
        }
        _ => {
            let source = Source::new(id, input.text.clone());
            *main_source = Some(source.clone());
            source
        }
    };
    let fonts = shared.fonts();
    let world = Arc::new(DocWorld::new(shared.clone(), fonts, source, root, files, epoch));

    let Warned { output, warnings } = typst::compile::<PagedDocument>(&*world);
    let mut diagnostics = Vec::new();
    let ok;
    let mut good = None;
    match output {
        Ok(doc) => {
            ok = true;
            let pages = doc
                .pages()
                .iter()
                .map(|p| {
                    let s = p.frame.size();
                    [s.x.to_pt() as f32, s.y.to_pt() as f32]
                })
                .collect();
            let anchors = build_anchors(&doc, world.main_source());
            good = Some((Arc::new(doc), world.clone(), pages, anchors));
        }
        Err(errors) => {
            ok = false;
            for d in errors.iter().take(MAX_DIAGNOSTICS) {
                diagnostics.push(convert(&world, d));
            }
        }
    }
    for d in warnings.iter() {
        if diagnostics.len() >= MAX_DIAGNOSTICS {
            break;
        }
        diagnostics.push(convert(&world, d));
    }
    Outcome { ok, diagnostics, good }
}

fn convert(world: &DocWorld, d: &SourceDiagnostic) -> Diagnostic {
    let mut file = None;
    let (mut line, mut column) = (0, 0);
    if let Some(id) = d.span.id() {
        if let Some(range) = world.range(d.span)
            && let Ok(source) = world.source(id)
            && let Some((l, c)) = source.lines().byte_to_line_column(range.start)
        {
            line = l as u32;
            column = c as u32;
        }
        if id != world.main() {
            file = Some(world.display_path(id));
        }
    }
    Diagnostic {
        error: d.severity == Severity::Error,
        message: d.message.to_string(),
        hints: d.hints.iter().map(|h| h.v.to_string()).collect(),
        file,
        line,
        column,
    }
}

// ---------------------------------------------------------------------------
// Anchors
// ---------------------------------------------------------------------------

struct Collector<'a> {
    main: &'a Source,
    page: u32,
    /// line -> (page, y) of its first appearance in layout order.
    best: FxHashMap<u32, (u32, f32)>,
}

impl Collector<'_> {
    fn walk(&mut self, frame: &Frame, ts: Transform) {
        for (pos, item) in frame.items() {
            match item {
                FrameItem::Group(group) => {
                    let inner = ts.pre_concat(Transform::translate(pos.x, pos.y).pre_concat(group.transform));
                    self.walk(&group.frame, inner);
                }
                FrameItem::Text(text) => {
                    let origin = Point::zero().transform(ts.pre_concat(Transform::translate(pos.x, pos.y)));
                    let ascent = text.font.metrics().ascender.at(text.size);
                    let y = (origin.y - ascent).to_pt().max(0.0) as f32;
                    let mut last = None;
                    for glyph in &text.glyphs {
                        let span = glyph.span.0;
                        if last == Some(span) {
                            continue;
                        }
                        last = Some(span);
                        let DiagSpanKind::Number { id, num, .. } = DiagSpan::from_span(span, None).get() else {
                            continue;
                        };
                        if id != self.main.id() {
                            continue;
                        }
                        let Some(range) = self.main.range(num, None) else { continue };
                        let Some(line) = self.main.lines().byte_to_line(range.start) else {
                            continue;
                        };
                        let key = (self.page, y);
                        let entry = self.best.entry(line as u32).or_insert(key);
                        if key.0 < entry.0 || (key.0 == entry.0 && key.1 < entry.1) {
                            *entry = key;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Builds the line -> position table by walking the page frames, then keeps
/// the longest subsequence (by line) that is non-decreasing in (page, y) so
/// that floating content cannot break monotonic scroll mapping.
pub fn build_anchors(doc: &PagedDocument, main: &Source) -> Vec<Anchor> {
    let mut collector = Collector { main, page: 0, best: FxHashMap::default() };
    for (index, page) in doc.pages().iter().enumerate() {
        collector.page = index as u32;
        collector.walk(&page.frame, Transform::identity());
    }
    let mut all: Vec<Anchor> = collector.best.into_iter().map(|(line, (page, y_pt))| Anchor { line, page, y_pt }).collect();
    all.sort_by_key(|a| a.line);
    longest_non_decreasing(all)
}

fn key(a: &Anchor) -> f64 {
    f64::from(a.page) * 1.0e6 + f64::from(a.y_pt)
}

fn longest_non_decreasing(items: Vec<Anchor>) -> Vec<Anchor> {
    if items.is_empty() {
        return items;
    }
    // Patience sorting with back-pointers.
    let mut tails: Vec<usize> = Vec::new(); // index of the smallest tail per length
    let mut prev: Vec<Option<usize>> = vec![None; items.len()];
    for i in 0..items.len() {
        let k = key(&items[i]);
        let pos = tails.partition_point(|&t| key(&items[t]) <= k);
        if pos > 0 {
            prev[i] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(items[i].clone());
        cur = prev[i];
    }
    out.reverse();
    out
}
