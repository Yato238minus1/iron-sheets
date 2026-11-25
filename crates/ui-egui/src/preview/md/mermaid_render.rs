//! Mermaid source -> SVG (mermaid-rs-renderer) -> pixels (resvg). Runs on a worker thread of `ImageCache`.
//! Feature `mermaid`; the types it returns live in `mermaid.rs` so the image cache and viewers compile without it.
use super::mermaid::{DiagramText, Fail, Job, Label, Link, Rendered, fail};
use super::svgfonts::fonts;
use egui::ColorImage;
use resvg::tiny_skia::{Pixmap, Transform};
use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// A layout that has not finished by now is abandoned (its thread keeps running until it ends by itself).
const TIMEOUT: Duration = Duration::from_secs(10);
/// Same cap as ordinary images.
const MAX_SIDE: f32 = 2048.0;
/// Zoomed raster: side and pixel count caps (8192 x 4096 x 4 bytes = 128 MB).
const ZOOM_SIDE: f32 = 8192.0;
const ZOOM_PIXELS: f32 = 32.0 * 1024.0 * 1024.0;
const MAX_LABELS: usize = 600;
/// Deep recursion in the layout fails one diagram here, not the process.
const STACK: usize = 16 << 20;
const MAX_STUCK: isize = 2;
const MAX_BYTES: usize = 64 * 1024;
const MAX_LINES: usize = 2000;

/// Render threads that timed out and have not ended yet (signed: a thread may end before its timeout is counted).
static STUCK: AtomicIsize = AtomicIsize::new(0);
const RUNNING: u8 = 0;
const ENDED: u8 = 1;
const TIMED_OUT: u8 = 2;

/// Nothing in a diagram may read files or data URLs (`<image href>` in a label).
fn usvg_options(fonts: Arc<resvg::usvg::fontdb::Database>) -> resvg::usvg::Options<'static> {
    let href = resvg::usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) };
    resvg::usvg::Options { fontdb: fonts, image_href_resolver: href, ..Default::default() }
}

/// Normal form for comparing a node label with the text of its `<text>`.
fn norm(s: &str) -> String {
    let s = s.replace("<br/>", " ").replace("<br />", " ").replace("<br>", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Only http(s), mailto and scheme-less (relative) targets are followed.
fn safe_url(u: &str) -> bool {
    let u = u.trim();
    match u.split_once(':') {
        Some((scheme, _)) if scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)) && !scheme.is_empty() => {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "mailto")
        }
        _ => !u.is_empty(),
    }
}

/// A node's normalised label, its link and its box in SVG units.
type NodeLink = (String, Link, Option<egui::Rect>);

/// Links of the nodes by normalised label: `click` directives and Obsidian's `internal-link` class.
/// shortcut: the link is attached to the `<text>` whose text equals the node label; the node box comes from the layout.
fn node_links(g: &mermaid_rs_renderer::ir::Graph, layout: &mermaid_rs_renderer::layout::Layout) -> Vec<NodeLink> {
    let mut v = Vec::new();
    for (id, node) in &g.nodes {
        let label = norm(&node.label);
        let shape = layout.nodes.get(id).map(|n| egui::Rect::from_min_size(egui::pos2(n.x, n.y), egui::vec2(n.width, n.height)));
        if let Some(l) = g.node_links.get(id) {
            if safe_url(&l.url) {
                v.push((label, Link::Url(l.url.trim().to_string()), shape));
            }
        } else if g.node_classes.get(id).is_some_and(|c| c.iter().any(|c| c == "internal-link")) && !label.is_empty() {
            v.push((label.clone(), Link::Note(label), shape));
        }
    }
    v
}

fn collect_labels(g: &resvg::usvg::Group, links: &[NodeLink], out: &mut Vec<Label>) {
    for n in g.children() {
        match n {
            resvg::usvg::Node::Group(g) => collect_labels(g, links, out),
            resvg::usvg::Node::Text(t) if out.len() < MAX_LABELS => {
                let lines: Vec<&str> = t.chunks().iter().map(|c| c.text()).collect();
                let text = lines.join("\n");
                let size = t.chunks().first().and_then(|c| c.spans().first()).map_or(12.0, |s| s.font_size().get());
                let b = t.abs_bounding_box();
                if text.trim().is_empty() || !(b.width() > 0.0 && b.height() > 0.0) {
                    continue;
                }
                let (link, node) = match links.iter().find(|(l, ..)| *l == norm(&text)) {
                    Some((_, l, r)) => (Some(l.clone()), *r),
                    None => (None, None),
                };
                let rect = egui::Rect::from_min_size(egui::pos2(b.x(), b.y()), egui::vec2(b.width(), b.height()));
                // The node box only counts when it really encloses the text (other diagram kinds lay out differently).
                let shape = node.filter(|n| n.expand(1.0).contains_rect(rect));
                out.push(Label { rect, text, size, link, shape });
            }
            _ => {}
        }
    }
}

/// Text of the tree in reading order: rows (quantised to the font size) top to bottom, then left to right.
fn labels(tree: &resvg::usvg::Tree, links: &[NodeLink]) -> Vec<Label> {
    let mut v = Vec::new();
    collect_labels(tree.root(), links, &mut v);
    let row = |l: &Label| ((l.rect.center().y / (l.size * 1.2).max(1.0)).round() as i64, l.rect.min.x);
    v.sort_by(|a, b| {
        let (ra, rb) = (row(a), row(b));
        ra.0.cmp(&rb.0).then(ra.1.total_cmp(&rb.1))
    });
    v
}

/// Bounds of everything drawn except a background that fills the whole canvas.
fn content_bounds(tree: &resvg::usvg::Tree) -> Option<resvg::usvg::Rect> {
    let (w, h) = (tree.size().width(), tree.size().height());
    let fills_canvas = |b: resvg::usvg::Rect| b.x() <= 0.5 && b.y() <= 0.5 && b.right() >= w - 0.5 && b.bottom() >= h - 0.5;
    let mut it = tree.root().children().iter().map(|n| n.abs_stroke_bounding_box()).filter(|b| !fills_canvas(*b));
    let first = it.next()?;
    Some(it.fold(first, |a, b| {
        resvg::usvg::Rect::from_ltrb(a.left().min(b.left()), a.top().min(b.top()), a.right().max(b.right()), a.bottom().max(b.bottom())).unwrap_or(a)
    }))
}

fn rasterise(svg: &str, fonts: Arc<resvg::usvg::fontdb::Database>, job: &Job, links: &[NodeLink]) -> Result<Rendered, String> {
    let opts = usvg_options(fonts);
    let tree = resvg::usvg::Tree::from_str(svg, &opts).map_err(|e| e.to_string())?;
    // mermaid-rs-renderer pads the far sides twice as much as the near ones: crop to the content with the near padding all round.
    let (ox, oy, nw, nh) = match content_bounds(&tree) {
        Some(b) => {
            let pad = b.x().min(b.y()).max(0.0);
            (b.x() - pad, b.y() - pad, (b.width() + 2.0 * pad).max(1.0), (b.height() + 2.0 * pad).max(1.0))
        }
        None => (0.0, 0.0, tree.size().width().max(1.0), tree.size().height().max(1.0)),
    };
    let scale = match job.px_w {
        Some(px) => (px / nw).min(ZOOM_SIDE / nw).min(ZOOM_SIDE / nh).min((ZOOM_PIXELS / (nw * nh)).sqrt()),
        None => ((job.w_pts / nw).min(1.0) * job.ppp).min(MAX_SIDE / nw).min(MAX_SIDE / nh),
    };
    let (pw, ph) = ((nw * scale).ceil() as u32, (nh * scale).ceil() as u32);
    let mut pm = Pixmap::new(pw.max(1), ph.max(1)).ok_or("diagram too large")?;
    resvg::render(&tree, Transform::from_translate(-ox, -oy).post_scale(scale, scale), &mut pm.as_mut());
    let image = ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
    let d = egui::vec2(-ox, -oy);
    let mut labels = labels(&tree, links);
    for l in &mut labels {
        l.rect = l.rect.translate(d);
        l.shape = l.shape.map(|r| r.translate(d));
    }
    let text = Arc::new(DiagramText { size: [nw, nh], labels });
    Ok(Rendered { dims: [nw.ceil() as u32, nh.ceil() as u32], image, text })
}

fn render_now(job: &Job, fonts: Arc<resvg::usvg::fontdb::Database>) -> Result<Rendered, String> {
    let theme = if job.dark { mermaid_rs_renderer::Theme::dark() } else { mermaid_rs_renderer::Theme::modern() };
    let opts = mermaid_rs_renderer::RenderOptions { theme, ..Default::default() };
    let parsed = mermaid_rs_renderer::parse_mermaid_strict(&job.source).map_err(|e| e.to_string())?;
    let layout = mermaid_rs_renderer::compute_layout(&parsed.graph, &opts.theme, &opts.layout);
    let svg = mermaid_rs_renderer::render_svg(&layout, &opts.theme, &opts.layout);
    rasterise(&svg, fonts, job, &node_links(&parsed.graph, &layout))
}

/// `render_now` with panics and hangs turned into errors. The first call loads the system fonts before the
/// timeout starts. Threads that outlive their timeout are counted; at `MAX_STUCK` new renders fail fast (and are retried later, so other notes recover once a thread ends).
pub fn render(job: &Job) -> Result<Rendered, Fail> {
    if job.source.len() > MAX_BYTES || job.source.lines().count() > MAX_LINES {
        return Err(fail(format!("source too large (limit {} kB, {MAX_LINES} lines)", MAX_BYTES / 1024)));
    }
    let transient = |msg: &str| Fail { msg: msg.into(), transient: true };
    let fonts = fonts();
    if STUCK.load(Ordering::SeqCst) >= MAX_STUCK {
        return Err(transient("too many diagrams still rendering"));
    }
    let (tx, rx) = mpsc::channel();
    let state = Arc::new(AtomicU8::new(RUNNING));
    let (job, st) = (job.clone(), state.clone());
    let spawned = std::thread::Builder::new().name("iron-sheets-mermaid".into()).stack_size(STACK).spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_now(&job, fonts))).unwrap_or_else(|_| Err("the renderer crashed".into()));
        let _ = tx.send(r);
        if st.swap(ENDED, Ordering::SeqCst) == TIMED_OUT {
            STUCK.fetch_sub(1, Ordering::SeqCst);
        }
    });
    if spawned.is_err() {
        return Err(transient("could not start the render thread"));
    }
    match rx.recv_timeout(TIMEOUT) {
        Ok(r) => r.map_err(fail),
        Err(_) => {
            if state.compare_exchange(RUNNING, TIMED_OUT, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                STUCK.fetch_add(1, Ordering::SeqCst);
            }
            // Final for this source: a retry would pin another thread with the same hang (the crate has no cancel hook).
            Err(fail("timed out"))
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn job(s: &str) -> Job {
        Job { source: s.into(), dark: true, w_pts: 600.0, ppp: 2.0, px_w: None }
    }

    #[test]
    fn flowchart_renders_and_scales() {
        let r = render(&job("flowchart LR\n A-->B")).unwrap();
        assert!(r.dims[0] > 20 && r.image.size[0] as u32 >= r.dims[0]);
    }

    #[test]
    fn wide_diagram_shrinks_to_column() {
        let mut j = job("flowchart LR\n A-->B-->C-->D-->E-->F-->G-->H-->I-->J");
        j.w_pts = 100.0;
        j.ppp = 1.0;
        let r = render(&j).unwrap();
        assert!(r.dims[0] > 100 && r.image.size[0] <= 101);
    }

    #[test]
    fn text_is_extracted_in_reading_order_with_links() {
        let src = "flowchart TD\n  A[Start] --> B[Docs]\n  B --> C[My Note]\n  click B \"https://example.com\"\n  class C internal-link;\n";
        let t = render(&job(src)).unwrap().text;
        let texts: Vec<&str> = t.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["Start", "Docs", "My Note"], "{texts:?}");
        assert!(t.labels[0].link.is_none());
        assert_eq!(t.labels[1].link, Some(Link::Url("https://example.com".into())));
        assert_eq!(t.labels[2].link, Some(Link::Note("My Note".into())));
        // A linked label carries its whole node, which is larger than the text; plain labels carry none.
        let shape = t.labels[1].shape.expect("node shape");
        assert!(shape.contains_rect(t.labels[1].rect) && shape.width() > t.labels[1].rect.width() + 4.0, "{shape:?}");
        assert!(t.labels[0].shape.is_none());
        assert!(t.labels.iter().all(|l| l.rect.width() > 0.0 && l.rect.max.x <= t.size[0] + 1.0));
    }

    #[test]
    fn only_safe_link_targets_are_followed() {
        assert!(safe_url("https://a.b") && safe_url("mailto:a@b") && safe_url("Notes/Plan.md") && safe_url("#head"));
        assert!(!safe_url("javascript:alert(1)") && !safe_url("file:///etc/passwd") && !safe_url(""));
        let src = "flowchart TD\n  A[Evil]\n  click A \"javascript:alert(1)\"\n";
        assert!(render(&job(src)).unwrap().text.labels.iter().all(|l| l.link.is_none()));
    }

    #[test]
    fn zoomed_raster_is_sharper_and_capped() {
        let mut j = job("flowchart LR\n A-->B");
        let base = render(&j).unwrap();
        j.px_w = Some(base.image.size[0] as f32 * 3.0);
        let z = render(&j).unwrap();
        assert!(z.image.size[0] > base.image.size[0] * 2 && z.dims == base.dims);
        j.px_w = Some(1e6);
        let big = render(&j).unwrap();
        assert!(big.image.size[0] <= 8192 && big.image.size[1] <= 8192);
    }

    #[test]
    fn picture_is_centred_with_even_margins() {
        for src in ["flowchart TB\n    A-->B", "flowchart LR\n A-->B", "flowchart TD\n  A[Start] --> B[Docs]"] {
            let img = render(&job(src)).unwrap().image;
            let [w, h] = img.size;
            let bg = img.pixels[0];
            let used = |x: usize| (0..h).any(|y| img.pixels[y * w + x] != bg);
            let left = (0..w).find(|&x| used(x)).unwrap();
            let right = w - 1 - (0..w).rev().find(|&x| used(x)).unwrap();
            let rows = |y: usize| (0..w).any(|x| img.pixels[y * w + x] != bg);
            let top = (0..h).find(|&y| rows(y)).unwrap();
            let bottom = h - 1 - (0..h).rev().find(|&y| rows(y)).unwrap();
            assert!(left.abs_diff(right) <= 2 && top.abs_diff(bottom) <= 2, "{src:?}: l={left} r={right} t={top} b={bottom}");
        }
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(!render(&job("not a diagram at all")).unwrap_err().transient);
    }

    #[test]
    fn oversized_source_is_refused() {
        let e = render(&job(&"A-->B\n".repeat(MAX_LINES + 1))).unwrap_err();
        assert!(e.msg.contains("too large") && !e.transient);
    }

    #[test]
    fn diagram_cannot_read_files() {
        let dir = std::env::temp_dir().join(format!("iron-sheets-mermaid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("p.png");
        Pixmap::new(4, 4).unwrap().save_png(&png).unwrap();
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><image href="{}" width="4" height="4"/></svg>"#, png.display());
        let default = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
        assert!(!default.root().children().is_empty(), "control: the default resolver loads the file");
        assert!(resvg::usvg::Tree::from_str(&svg, &usvg_options(fonts())).unwrap().root().children().is_empty());
        assert!(rasterise(&svg, fonts(), &job(""), &[]).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn label_boxes_lie_on_the_drawn_text() {
        for src in ["graph TD\n A-->B\n", "graph LR\n A-->B\n", "sequenceDiagram\n Alice->>Bob: Hi\n"] {
            let mut j = job(src);
            j.ppp = 1.0;
            let r = render(&j).unwrap();
            let (sx, sy) = (r.image.size[0] as f32 / r.text.size[0], r.image.size[1] as f32 / r.text.size[1]);
            assert!(!r.text.labels.is_empty());
            for l in &r.text.labels {
                // Centre of the bright (text coloured) pixels inside the box.
                let (mut n, mut cx, mut cy) = (0.0, 0.0, 0.0);
                for y in (l.rect.min.y * sy) as usize..=((l.rect.max.y * sy) as usize).min(r.image.size[1] - 1) {
                    for x in (l.rect.min.x * sx) as usize..=((l.rect.max.x * sx) as usize).min(r.image.size[0] - 1) {
                        let c = r.image.pixels[y * r.image.size[0] + x];
                        if c.r() > 180 && c.g() > 180 {
                            (n, cx, cy) = (n + 1.0, cx + x as f32, cy + y as f32);
                        }
                    }
                }
                assert!(n > 5.0, "{src:?}: no text pixels in {:?}", l.rect);
                let (dx, dy) = ((cx / n / sx - l.rect.center().x).abs(), (cy / n / sy - l.rect.center().y).abs());
                assert!(dx < 5.0 && dy < 5.0, "{src:?} {:?}: ink off the box by ({dx}, {dy})", l.text);
            }
        }
    }

    #[test]
    fn a_missing_trailing_newline_changes_nothing() {
        let texts = |s: &str| render(&job(s)).unwrap().text.labels.iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        for b in ["graph TD\n A[Start]-->B[End]", "flowchart LR\n A-->B\n B-->C[Done]"] {
            assert_eq!(texts(b), texts(&format!("{b}\n")), "{b:?}");
        }
    }
}

