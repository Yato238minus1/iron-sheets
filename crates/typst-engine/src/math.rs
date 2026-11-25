//! Rendering of Typst math snippets to transparent RGBA images.

use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use typst::WorldExt;
use typst::diag::{Severity, SourceDiagnostic, Warned};
use typst::utils::Scalar;
use typst_layout::PagedDocument;
use typst_render::RenderOptions;

use crate::shared::Shared;
use crate::world::MathWorld;
use crate::{MathImage, panic_message};

/// An owned math request.
#[derive(Clone, Debug)]
pub struct MathJob {
    pub source: String,
    pub display: bool,
    pub font_size_pt: f32,
    pub color: [u8; 4],
    pub pixels_per_pt: f32,
    /// `source` is LaTeX; converted by [`render_one`]/[`render_batch`] before compiling.
    pub latex: bool,
}

impl MathJob {
    fn size(&self) -> f32 {
        if self.font_size_pt.is_finite() && self.font_size_pt > 0.0 { self.font_size_pt.clamp(1.0, 2000.0) } else { 11.0 }
    }

    fn ppp(&self) -> f64 {
        if self.pixels_per_pt.is_finite() && self.pixels_per_pt > 0.0 { f64::from(self.pixels_per_pt.clamp(0.05, 16.0)) } else { 2.0 }
    }

    fn set_text(&self) -> String {
        let [r, g, b, a] = self.color;
        // `bounds` edges make the (inline) equation's layout box the real ink
        // box instead of clamping it to the cap height / baseline.
        format!("#set text(size: {}pt, fill: rgb({r}, {g}, {b}, {a}), top-edge: \"bounds\", bottom-edge: \"bounds\")", self.size())
    }

    /// Safety margin (pt) around the content, so that anti-aliasing and
    /// overhangs are not clipped before we crop to the ink bounds.
    fn pad(&self) -> f32 {
        2.0
    }

    /// The paragraph: a zero-width, invisible marker (whose text item sits on
    /// the line's baseline) followed by the equation.
    fn paragraph(&self) -> String {
        let body = self.source.trim();
        let marker = format!("#box(width: 0pt, text(size: {MARKER_PT}pt, fill: rgb(0, 0, 0, 0))[{MARKER}])");
        if self.display {
            // An inline equation in display style: same layout as a block
            // equation, but sized and aligned like inline content.
            format!("{marker}#math.equation(block: false, math.display(${body}$.body))\n")
        } else {
            format!("{marker}${body}$\n")
        }
    }

    fn single_source(&self) -> String {
        format!("#set page(width: auto, height: auto, margin: {}pt, fill: none)\n{}\n{}", self.pad(), self.set_text(), self.paragraph())
    }
}

/// The invisible marker: a transparent, tiny "x" whose text item sits on the
/// line's baseline.
const MARKER: &str = "x";
const MARKER_PT: f64 = 0.25;

/// Formats the first error (plus its hints) of a failed compilation.
fn describe(errors: &[SourceDiagnostic]) -> String {
    let Some(first) = errors.iter().find(|d| d.severity == Severity::Error).or(errors.first()) else {
        return "unknown error".into();
    };
    let mut out = first.message.to_string();
    for hint in &first.hints {
        out.push_str("\nhint: ");
        out.push_str(&hint.v);
    }
    out
}

fn compile_world(world: &MathWorld) -> Result<PagedDocument, Vec<SourceDiagnostic>> {
    let Warned { output, .. } = typst::compile::<PagedDocument>(world);
    output.map_err(|e| e.into_iter().collect())
}

/// Finds the invisible marker and returns its absolute baseline y (pt).
fn find_marker(frame: &typst::layout::Frame, origin_y: f64) -> Option<f64> {
    use typst::layout::FrameItem;
    for (pos, item) in frame.items() {
        let y = origin_y + pos.y.to_pt();
        match item {
            FrameItem::Group(g) => {
                if let Some(found) = find_marker(&g.frame, y) {
                    return Some(found);
                }
            }
            FrameItem::Text(t) if t.text == MARKER && (t.size.to_pt() - MARKER_PT).abs() < 1e-6 => {
                return Some(y);
            }
            _ => {}
        }
    }
    None
}

/// Bounding box (x0, y0, x1, y1, exclusive end) of pixels with non-zero alpha.
fn ink_bounds(data: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        let row = &data[y * w * 4..(y + 1) * w * 4];
        let mut first = None;
        let mut last = 0;
        for x in 0..w {
            if row[x * 4 + 3] != 0 {
                if first.is_none() {
                    first = Some(x);
                }
                last = x;
            }
        }
        if let Some(first) = first {
            x0 = x0.min(first);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

fn render_page(doc: &PagedDocument, index: usize, job: &MathJob, key: u64) -> Option<MathImage> {
    let page = doc.pages().get(index)?;
    let ppp = job.ppp();
    let opts = RenderOptions { pixel_per_pt: Scalar::new(ppp), render_bleed: false };
    let pixmap = typst_render::render(page, &opts);
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let pad = f64::from(job.pad());
    let size = page.frame.size();
    // The layout box of the equation, in pixels.
    let lx0 = (pad * ppp).floor().max(0.0) as usize;
    let lx1 = (((size.x.to_pt() - pad) * ppp).ceil().max(0.0) as usize).min(w);
    let ly0 = (pad * ppp).floor().max(0.0) as usize;
    let ly1 = (((size.y.to_pt() - pad) * ppp).ceil().max(0.0) as usize).min(h);
    let (mut x0, mut y0, mut x1, mut y1) = (lx0.min(w), ly0.min(h), lx1.max(lx0.min(w)), ly1.max(ly0.min(h)));
    if let Some((ix0, iy0, ix1, iy1)) = ink_bounds(pixmap.data(), w, h) {
        if x1 == x0 {
            (x0, x1) = (ix0, ix1);
        }
        (x0, y0, x1, y1) = (x0.min(ix0), y0.min(iy0), x1.max(ix1), y1.max(iy1));
        if y1 <= y0 {
            (y0, y1) = (iy0, iy1);
        }
    }
    if x1 <= x0 || y1 <= y0 {
        // Empty formula: a 1x1 transparent image.
        (x0, y0, x1, y1) = (0, 0, 1.min(w), 1.min(h));
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(cw * ch * 4);
    for y in y0..y1 {
        rgba.extend_from_slice(&pixmap.data()[(y * w + x0) * 4..(y * w + x1) * 4]);
    }
    let baseline_abs = find_marker(&page.frame, 0.0).unwrap_or(size.y.to_pt() - pad);
    Some(MathImage {
        key,
        width: cw as u32,
        height: ch as u32,
        rgba,
        size_pt: [(cw as f64 / ppp) as f32, (ch as f64 / ppp) as f32],
        baseline_pt: (baseline_abs - y0 as f64 / ppp).max(0.0) as f32,
    })
}

impl MathJob {
    /// The job with Typst source: LaTeX is converted (only here, so nothing runs in Typst mode).
    fn prepared(&self) -> Result<MathJob, String> {
        if !self.latex {
            return Ok(self.clone());
        }
        let source = crate::latex::latex_to_typst(&self.source)?;
        Ok(MathJob { source, latex: false, ..self.clone() })
    }
}

/// One compile per formula.
pub fn render_one(shared: &Arc<Shared>, job: &MathJob, key: u64) -> Result<MathImage, String> {
    if job.latex {
        return render_one_typst(shared, &job.prepared()?, key);
    }
    render_one_typst(shared, job, key)
}

fn render_one_typst(shared: &Arc<Shared>, job: &MathJob, key: u64) -> Result<MathImage, String> {
    let world = MathWorld::new(shared.clone(), job.single_source());
    let result = catch_unwind(AssertUnwindSafe(|| match compile_world(&world) {
        Ok(doc) => render_page(&doc, 0, job, key).ok_or_else(|| "empty document".to_string()),
        Err(errors) => Err(describe(&errors)),
    }));
    result.unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&*p))))
}

/// Several formulas as pages of one document. Errors are isolated per
/// formula: failing formulas are located by span, dropped, and the rest is
/// compiled again.
pub fn render_batch(shared: &Arc<Shared>, jobs: &[(MathJob, u64)]) -> Vec<Result<MathImage, String>> {
    if !jobs.iter().any(|(j, _)| j.latex) {
        return render_batch_typst(shared, jobs);
    }
    // Convert LaTeX first; conversion errors are per-formula results.
    let mut out: Vec<Option<Result<MathImage, String>>> = vec![None; jobs.len()];
    let mut ok: Vec<(MathJob, u64)> = Vec::with_capacity(jobs.len());
    let mut idx: Vec<usize> = Vec::with_capacity(jobs.len());
    for (i, (job, key)) in jobs.iter().enumerate() {
        match job.prepared() {
            Ok(j) => {
                ok.push((j, *key));
                idx.push(i);
            }
            Err(e) => out[i] = Some(Err(e)),
        }
    }
    for (i, r) in idx.into_iter().zip(render_batch_typst(shared, &ok)) {
        out[i] = Some(r);
    }
    out.into_iter().map(|r| r.unwrap_or_else(|| Err("not rendered".to_string()))).collect()
}

fn render_batch_typst(shared: &Arc<Shared>, jobs: &[(MathJob, u64)]) -> Vec<Result<MathImage, String>> {
    let mut results: Vec<Option<Result<MathImage, String>>> = vec![None; jobs.len()];
    let mut todo: Vec<usize> = (0..jobs.len()).collect();
    for _attempt in 0..3 {
        if todo.is_empty() {
            break;
        }
        if todo.len() == 1 {
            let i = todo[0];
            results[i] = Some(render_one_typst(shared, &jobs[i].0, jobs[i].1));
            todo.clear();
            break;
        }
        // Build the document and remember each formula's byte range.
        let mut text = String::new();
        let mut ranges: Vec<Range<usize>> = Vec::with_capacity(todo.len());
        for &i in &todo {
            let job = &jobs[i].0;
            let start = text.len();
            text.push_str(&format!("#page(width: auto, height: auto, margin: {}pt, fill: none)[{}\n{}]\n", job.pad(), job.set_text(), job.paragraph()));
            ranges.push(start..text.len());
        }
        let world = MathWorld::new(shared.clone(), text);
        let outcome = catch_unwind(AssertUnwindSafe(|| compile_world(&world)));
        match outcome {
            Ok(Ok(doc)) if doc.pages().len() == todo.len() => {
                for (page, &i) in todo.iter().enumerate() {
                    results[i] = Some(render_page(&doc, page, &jobs[i].0, jobs[i].1).ok_or_else(|| "empty document".to_string()));
                }
                todo.clear();
            }
            Ok(Err(errors)) => {
                // Attribute every error to a formula.
                let mut failed: Vec<(usize, String)> = Vec::new();
                for err in errors.iter().filter(|d| d.severity == Severity::Error) {
                    let Some(range) = world.range(err.span) else { continue };
                    if let Some(pos) = ranges.iter().position(|r| r.start <= range.start && range.start < r.end)
                        && !failed.iter().any(|(p, _)| *p == pos)
                    {
                        failed.push((pos, describe(std::slice::from_ref(err))));
                    }
                }
                if failed.is_empty() {
                    // Cannot attribute: fall back to one compile each.
                    for &i in &todo {
                        results[i] = Some(render_one_typst(shared, &jobs[i].0, jobs[i].1));
                    }
                    todo.clear();
                } else {
                    for (pos, message) in &failed {
                        results[todo[*pos]] = Some(Err(message.clone()));
                    }
                    let drop: Vec<usize> = failed.iter().map(|(p, _)| todo[*p]).collect();
                    todo.retain(|i| !drop.contains(i));
                }
            }
            _ => {
                for &i in &todo {
                    results[i] = Some(render_one_typst(shared, &jobs[i].0, jobs[i].1));
                }
                todo.clear();
            }
        }
    }
    for &i in &todo {
        results[i] = Some(render_one_typst(shared, &jobs[i].0, jobs[i].1));
    }
    results.into_iter().map(|r| r.unwrap_or_else(|| Err("not rendered".to_string()))).collect()
}
