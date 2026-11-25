//! Sanitized, deterministic pure-Rust SVG emitter for Excalidraw scenes.

use super::extract::EmbeddedFileRef;
use super::schema::{ExcalidrawElement, ExcalidrawScene, FontFamily};
use super::{ExcalidrawError, MAX_SVG_BYTES};
use rustc_hash::FxHashMap;
use std::fmt::Write;

const COORD_LIMIT: f64 = 1_000_000.0;
const SCENE_PADDING: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub dark: bool,
    pub roughness: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { dark: false, roughness: true }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedSvg {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    pub element_count: usize,
    pub background_color: String,
    pub embedded_files: Vec<EmbeddedFileRef>,
}

/// Deterministic SplitMix64 PRNG keyed by `element.seed`.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Returns a deterministic float in `[-1.0, 1.0]`.
    fn signed(&mut self) -> f64 {
        let u = (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64);
        u * 2.0 - 1.0
    }
}

fn clamp_coord(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v.clamp(-COORD_LIMIT, COORD_LIMIT))
    } else {
        None
    }
}

fn is_renderable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "rectangle" | "ellipse" | "diamond" | "line" | "arrow" | "freedraw" | "text" | "frame" | "magicframe" | "image"
    )
}

#[derive(Clone, Copy)]
struct Bounds {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

    fn include_point(&mut self, x: f64, y: f64) {
        if x < self.min_x {
            self.min_x = x;
        }
        if y < self.min_y {
            self.min_y = y;
        }
        if x > self.max_x {
            self.max_x = x;
        }
        if y > self.max_y {
            self.max_y = y;
        }
    }

    fn include_rotated_rect(&mut self, x: f64, y: f64, w: f64, h: f64, angle: f64, pad: f64) {
        let x0 = x - pad;
        let y0 = y - pad;
        let x1 = x + w + pad;
        let y1 = y + h + pad;
        if angle.abs() < 1e-6 {
            self.include_point(x0, y0);
            self.include_point(x1, y1);
            return;
        }
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let (s, c) = angle.sin_cos();
        for (px, py) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
            let dx = px - cx;
            let dy = py - cy;
            self.include_point(cx + dx * c - dy * s, cy + dx * s + dy * c);
        }
    }
}

fn normalized_rect(el: &ExcalidrawElement) -> Option<(f64, f64, f64, f64)> {
    let mut x = clamp_coord(el.x)?;
    let mut y = clamp_coord(el.y)?;
    let mut w = clamp_coord(el.width)?;
    let mut h = clamp_coord(el.height)?;
    if w < 0.0 {
        x += w;
        w = -w;
    }
    if h < 0.0 {
        y += h;
        h = -h;
    }
    Some((x, y, w, h))
}

fn point_bounds(el: &ExcalidrawElement, x: f64, y: f64, w: f64, h: f64) -> (f64, f64, f64, f64) {
    if el.points.is_empty() {
        return (x, y, w, h);
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &[px, py] in &el.points {
        if let (Some(px), Some(py)) = (clamp_coord(x + px), clamp_coord(y + py)) {
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
    }
    if min_x.is_finite() {
        (min_x, min_y, (max_x - min_x).max(1.0), (max_y - min_y).max(1.0))
    } else {
        (x, y, w, h)
    }
}

/// Converts an Excalidraw scene into a sanitized SVG string.
pub fn scene_to_svg(
    scene: &ExcalidrawScene,
    embedded_files: &[EmbeddedFileRef],
    opts: &RenderOptions,
) -> Result<RenderedSvg, ExcalidrawError> {
    let mut bounds = Bounds::empty();
    let mut visible_count = 0usize;

    for el in &scene.elements {
        if el.is_deleted || !is_renderable_kind(&el.kind) {
            continue;
        }
        let Some((x, y, w, h)) = normalized_rect(el) else {
            continue;
        };
        let angle = if el.angle.is_finite() { el.angle } else { 0.0 };
        let sw = if el.stroke_width.is_finite() { el.stroke_width.clamp(0.0, 64.0) } else { 1.0 };
        let pad = sw + 12.0;
        match el.kind.as_str() {
            "line" | "arrow" | "freedraw" => {
                let (bx, by, bw, bh) = point_bounds(el, x, y, w, h);
                bounds.include_rotated_rect(bx, by, bw, bh, angle, pad);
            }
            "frame" | "magicframe" => {
                // Leave headroom above top-left for frame title label.
                bounds.include_rotated_rect(x, y - 22.0, w, h + 22.0, angle, pad);
            }
            _ => {
                bounds.include_rotated_rect(x, y, w, h, angle, pad);
            }
        }
        visible_count += 1;
    }

    if visible_count == 0 || !bounds.min_x.is_finite() {
        return Err(ExcalidrawError::EmptyScene);
    }

    let vb_x = bounds.min_x - SCENE_PADDING;
    let vb_y = bounds.min_y - SCENE_PADDING;
    let vb_w = (bounds.max_x - bounds.min_x + SCENE_PADDING * 2.0).max(1.0);
    let vb_h = (bounds.max_y - bounds.min_y + SCENE_PADDING * 2.0).max(1.0);

    let bg = resolve_background(scene.app_state.view_background_color.as_deref(), opts.dark);

    let embed_map: FxHashMap<&str, &str> = embedded_files
        .iter()
        .map(|e| (e.file_id.as_str(), e.target.as_str()))
        .collect();

    // Collect frame clip rects so children referencing `frameId` clip cleanly.
    let mut frame_clips: FxHashMap<&str, (usize, f64, f64, f64, f64, f64)> = FxHashMap::default();
    for (idx, el) in scene.elements.iter().enumerate() {
        if !el.is_deleted
            && matches!(el.kind.as_str(), "frame" | "magicframe")
            && !el.id.is_empty()
            && let Some((x, y, w, h)) = normalized_rect(el)
        {
            let angle = if el.angle.is_finite() { el.angle } else { 0.0 };
            frame_clips.insert(el.id.as_str(), (idx, x, y, w, h, angle));
        }
    }

    let mut svg = String::with_capacity((visible_count * 256).clamp(1024, 256 * 1024));
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb_x:.2} {vb_y:.2} {vb_w:.2} {vb_h:.2}" width="{vb_w:.0}" height="{vb_h:.0}">"#,
    );

    if !frame_clips.is_empty() {
        svg.push_str("<defs>");
        for (idx, fx, fy, fw, fh, angle) in frame_clips.values() {
            let _ = write!(svg, r#"<clipPath id="frame-{idx}">"#);
            if angle.abs() > 1e-6 {
                let deg = angle.to_degrees();
                let cx = fx + fw * 0.5;
                let cy = fy + fh * 0.5;
                let _ = write!(
                    svg,
                    r#"<rect x="{fx:.2}" y="{fy:.2}" width="{fw:.2}" height="{fh:.2}" rx="8" transform="rotate({deg:.3} {cx:.2} {cy:.2})"/>"#,
                );
            } else {
                let _ = write!(svg, r#"<rect x="{fx:.2}" y="{fy:.2}" width="{fw:.2}" height="{fh:.2}" rx="8"/>"#);
            }
            svg.push_str("</clipPath>");
        }
        svg.push_str("</defs>");
    }

    let _ = write!(
        svg,
        r#"<rect x="{vb_x:.2}" y="{vb_y:.2}" width="{vb_w:.2}" height="{vb_h:.2}" fill="{bg}"/>"#,
    );

    for (idx, el) in scene.elements.iter().enumerate() {
        if svg.len() > MAX_SVG_BYTES {
            return Err(ExcalidrawError::SvgTooLarge);
        }
        if el.is_deleted || !is_renderable_kind(&el.kind) {
            continue;
        }
        let Some((x, y, w, h)) = normalized_rect(el) else {
            continue;
        };

        let frame_clip = el
            .frame_id
            .as_deref()
            .and_then(|fid| frame_clips.get(fid).map(|(fidx, ..)| *fidx));

        if let Some(fidx) = frame_clip {
            let _ = write!(svg, r#"<g clip-path="url(#frame-{fidx})">"#);
        }

        emit_element(&mut svg, idx, el, x, y, w, h, &bg, scene, &embed_map, opts);

        if frame_clip.is_some() {
            svg.push_str("</g>");
        }
    }

    svg.push_str("</svg>");
    if svg.len() > MAX_SVG_BYTES {
        return Err(ExcalidrawError::SvgTooLarge);
    }

    Ok(RenderedSvg {
        svg,
        width: vb_w as f32,
        height: vb_h as f32,
        element_count: visible_count,
        background_color: bg,
        embedded_files: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_element(
    svg: &mut String,
    idx: usize,
    el: &ExcalidrawElement,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    bg: &str,
    scene: &ExcalidrawScene,
    embed_map: &FxHashMap<&str, &str>,
    opts: &RenderOptions,
) {
    let opacity = (if el.opacity.is_finite() { el.opacity } else { 100.0 } / 100.0).clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let sw = if el.stroke_width.is_finite() { el.stroke_width.clamp(0.1, 64.0) } else { 1.0 };
    let stroke = adapt_color(&el.stroke_color, opts.dark).unwrap_or_else(|| if opts.dark { "#ededed".into() } else { "#1e1e1e".into() });
    let fill = adapt_color(&el.background_color, opts.dark);
    let angle = if el.angle.is_finite() { el.angle } else { 0.0 };
    let use_rough = opts.roughness && el.roughness.is_finite() && el.roughness > 0.0;
    let rough_amp = if use_rough { el.roughness.clamp(0.0, 2.5) * 1.15 } else { 0.0 };
    let dash = stroke_dasharray(&el.stroke_style, sw);

    let (cx, cy) = match el.kind.as_str() {
        "line" | "arrow" | "freedraw" => {
            let (bx, by, bw, bh) = point_bounds(el, x, y, w, h);
            (bx + bw * 0.5, by + bh * 0.5)
        }
        _ => (x + w * 0.5, y + h * 0.5),
    };

    let has_transform = angle.abs() > 1e-6;
    let has_opacity = (opacity - 1.0).abs() > 1e-3;
    if has_transform || has_opacity {
        svg.push_str("<g");
        if has_transform {
            let _ = write!(svg, r#" transform="rotate({:.3} {cx:.2} {cy:.2})""#, angle.to_degrees());
        }
        if has_opacity {
            let _ = write!(svg, r#" opacity="{opacity:.3}""#);
        }
        svg.push('>');
    }

    match el.kind.as_str() {
        "rectangle" => {
            let r = if el.roundness.is_some() {
                (w.min(h) * 0.25).min(32.0)
            } else {
                0.0
            };
            let shape_path = rect_path(x, y, w, h, r);
            emit_fill(svg, idx, &shape_path, x, y, w, h, fill.as_deref(), &el.fill_style, sw, el.seed, rough_amp);
            if use_rough && r <= 0.5 {
                let mut rng = Rng::new(el.seed);
                let p1 = rough_poly_path(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true, rough_amp, &mut rng);
                let p2 = rough_poly_path(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true, rough_amp * 0.85, &mut rng);
                emit_stroke_path(svg, &p1, &stroke, sw, dash, 1.0);
                emit_stroke_path(svg, &p2, &stroke, sw * 0.85, dash, 0.72);
            } else if use_rough {
                let mut rng = Rng::new(el.seed);
                let jx = rng.signed() * rough_amp * 0.5;
                let jy = rng.signed() * rough_amp * 0.5;
                let p2 = rect_path(x + jx, y + jy, w, h, r);
                emit_stroke_path(svg, &shape_path, &stroke, sw, dash, 1.0);
                emit_stroke_path(svg, &p2, &stroke, sw * 0.85, dash, 0.65);
            } else {
                emit_stroke_path(svg, &shape_path, &stroke, sw, dash, 1.0);
            }
        }
        "diamond" => {
            let pts = [(x + w * 0.5, y), (x + w, y + h * 0.5), (x + w * 0.5, y + h), (x, y + h * 0.5)];
            let shape_path = if el.roundness.is_some() {
                rounded_polygon_path(&pts, (w.min(h) * 0.1).min(16.0))
            } else {
                poly_path(&pts, true)
            };
            emit_fill(svg, idx, &shape_path, x, y, w, h, fill.as_deref(), &el.fill_style, sw, el.seed, rough_amp);
            if use_rough {
                let mut rng = Rng::new(el.seed);
                let p1 = rough_poly_path(&pts, true, rough_amp, &mut rng);
                let p2 = rough_poly_path(&pts, true, rough_amp * 0.85, &mut rng);
                emit_stroke_path(svg, &p1, &stroke, sw, dash, 1.0);
                emit_stroke_path(svg, &p2, &stroke, sw * 0.85, dash, 0.72);
            } else {
                emit_stroke_path(svg, &shape_path, &stroke, sw, dash, 1.0);
            }
        }
        "ellipse" => {
            let shape_path = ellipse_path(x + w * 0.5, y + h * 0.5, w * 0.5, h * 0.5, 0.0, &mut None);
            emit_fill(svg, idx, &shape_path, x, y, w, h, fill.as_deref(), &el.fill_style, sw, el.seed, rough_amp);
            if use_rough {
                let mut rng = Rng::new(el.seed);
                let p1 = ellipse_path(x + w * 0.5, y + h * 0.5, w * 0.5, h * 0.5, rough_amp, &mut Some(&mut rng));
                let p2 = ellipse_path(x + w * 0.5, y + h * 0.5, w * 0.5, h * 0.5, rough_amp * 0.9, &mut Some(&mut rng));
                emit_stroke_path(svg, &p1, &stroke, sw, dash, 1.0);
                emit_stroke_path(svg, &p2, &stroke, sw * 0.85, dash, 0.72);
            } else {
                emit_stroke_path(svg, &shape_path, &stroke, sw, dash, 1.0);
            }
        }
        "line" | "arrow" => {
            let pts: Vec<(f64, f64)> = if el.points.is_empty() {
                vec![(x, y), (x + w, y + h)]
            } else {
                el.points
                    .iter()
                    .filter_map(|&[px, py]| Some((clamp_coord(x + px)?, clamp_coord(y + py)?)))
                    .collect()
            };
            if pts.len() >= 2 {
                let is_closed = el.kind == "line"
                    && pts.len() >= 3
                    && (pts[0].0 - pts[pts.len() - 1].0).abs() < 0.5
                    && (pts[0].1 - pts[pts.len() - 1].1).abs() < 0.5;
                let curved = el.roundness.is_some() && pts.len() >= 3;
                let path = if curved {
                    smooth_curve_path(&pts, is_closed)
                } else {
                    poly_path(&pts, is_closed)
                };
                if is_closed {
                    let (bx, by, bw, bh) = point_bounds(el, x, y, w, h);
                    emit_fill(svg, idx, &path, bx, by, bw, bh, fill.as_deref(), &el.fill_style, sw, el.seed, rough_amp);
                }
                if use_rough && !curved {
                    let mut rng = Rng::new(el.seed);
                    let p1 = rough_poly_path(&pts, is_closed, rough_amp * 0.8, &mut rng);
                    let p2 = rough_poly_path(&pts, is_closed, rough_amp * 0.65, &mut rng);
                    emit_stroke_path(svg, &p1, &stroke, sw, dash, 1.0);
                    emit_stroke_path(svg, &p2, &stroke, sw * 0.85, dash, 0.7);
                } else {
                    emit_stroke_path(svg, &path, &stroke, sw, dash, 1.0);
                }

                if let Some(ah) = el.start_arrowhead.as_deref()
                    && ah != "none"
                {
                    let (p0, p1) = (pts[0], pts[1]);
                    emit_arrowhead(svg, ah, p1, p0, &stroke, bg, sw);
                }
                if let Some(ah) = el.end_arrowhead.as_deref()
                    && ah != "none"
                {
                    let n = pts.len();
                    emit_arrowhead(svg, ah, pts[n - 2], pts[n - 1], &stroke, bg, sw);
                }
            }
        }
        "freedraw" => {
            let pts: Vec<(f64, f64)> = el
                .points
                .iter()
                .filter_map(|&[px, py]| Some((clamp_coord(x + px)?, clamp_coord(y + py)?)))
                .collect();
            if pts.len() == 1 {
                let (px, py) = pts[0];
                let _ = write!(
                    svg,
                    r#"<circle cx="{px:.2}" cy="{py:.2}" r="{:.2}" fill="{stroke}"/>"#,
                    (sw * 0.6).max(1.0)
                );
            } else if pts.len() >= 2 {
                let path = smooth_curve_path(&pts, false);
                let _ = write!(
                    svg,
                    r#"<path d="{path}" fill="none" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linecap="round" stroke-linejoin="round"/>"#,
                );
            }
        }
        "text" => {
            emit_text(svg, el, x, y, w, h, &stroke);
        }
        "frame" | "magicframe" => {
            let frame_stroke = if opts.dark { "#6c6c75" } else { "#b8b8c0" };
            let _ = write!(
                svg,
                r#"<rect x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" rx="8" fill="none" stroke="{frame_stroke}" stroke-width="1.5" stroke-dasharray="6 4"/>"#,
            );
            let label = el.name.as_deref().unwrap_or("Frame");
            let mut esc = String::new();
            xml_escape(label, &mut esc);
            let label_color = if opts.dark { "#b0b0ba" } else { "#6b6b76" };
            let _ = write!(
                svg,
                r#"<text x="{x:.2}" y="{:.2}" fill="{label_color}" font-family="Helvetica, Arial, Liberation Sans, DejaVu Sans, Noto Sans, sans-serif" font-size="13">{esc}</text>"#,
                y - 6.0
            );
        }
        "image" => {
            let href = el.file_id.as_deref().and_then(|fid| {
                if let Some(f) = scene.files.get(fid)
                    && is_safe_raster_data_url(&f.data_url)
                {
                    return Some(f.data_url.clone());
                }
                if embed_map.contains_key(fid) {
                    let mut esc_id = String::new();
                    xml_escape(fid, &mut esc_id);
                    return Some(format!("vault-embed://{esc_id}"));
                }
                None
            });
            if let Some(href) = href {
                let mut esc_href = String::new();
                xml_escape(&href, &mut esc_href);
                let _ = write!(
                    svg,
                    r#"<image href="{esc_href}" x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" preserveAspectRatio="none"/>"#,
                );
            } else {
                let ph_stroke = if opts.dark { "#55555e" } else { "#b0b0b8" };
                let ph_fill = if opts.dark { "#232329" } else { "#f3f3f6" };
                let _ = write!(
                    svg,
                    r#"<rect x="{x:.2}" y="{y:.2}" width="{w:.2}" height="{h:.2}" fill="{ph_fill}" stroke="{ph_stroke}" stroke-width="1.2" stroke-dasharray="4 3"/>"#,
                );
                let _ = write!(
                    svg,
                    r#"<path d="M {x:.2} {y:.2} L {:.2} {:.2} M {:.2} {y:.2} L {x:.2} {:.2}" stroke="{ph_stroke}" stroke-width="1"/>"#,
                    x + w,
                    y + h,
                    x + w,
                    y + h
                );
            }
        }
        _ => {}
    }

    if has_transform || has_opacity {
        svg.push_str("</g>");
    }
}

fn emit_stroke_path(svg: &mut String, d: &str, stroke: &str, sw: f64, dash: Option<&str>, op: f64) {
    let _ = write!(
        svg,
        r#"<path d="{d}" fill="none" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linecap="round" stroke-linejoin="round""#,
    );
    if let Some(da) = dash {
        let _ = write!(svg, r#" stroke-dasharray="{da}""#);
    }
    if op < 0.999 {
        let _ = write!(svg, r#" stroke-opacity="{op:.2}""#);
    }
    svg.push_str("/>");
}

#[allow(clippy::too_many_arguments)]
fn emit_fill(
    svg: &mut String,
    idx: usize,
    shape_path: &str,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    fill: Option<&str>,
    fill_style: &str,
    sw: f64,
    seed: u64,
    rough_amp: f64,
) {
    let Some(color) = fill else { return };
    match fill_style {
        "hachure" | "cross-hatch" | "zigzag" => {
            let _ = write!(
                svg,
                r#"<defs><clipPath id="fill-{idx}"><path d="{shape_path}"/></clipPath></defs>"#,
            );
            let mut hatch = String::new();
            let gap = (sw * 3.5 + 6.0).clamp(5.0, 24.0);
            let mut rng = Rng::new(seed ^ 0x55aa_55aa);
            let span = w + h;
            if fill_style == "zigzag" {
                let mut t = -h;
                let mut forward = true;
                while t <= span {
                    let (ax, ay) = (x + t, y);
                    let (bx, by) = (x + t - h, y + h);
                    if forward {
                        let _ = write!(hatch, "M {ax:.1} {ay:.1} L {bx:.1} {by:.1} ");
                    } else {
                        let _ = write!(hatch, "L {ax:.1} {ay:.1} L {bx:.1} {by:.1} ");
                    }
                    forward = !forward;
                    t += gap;
                }
            } else {
                let mut t = -h;
                while t <= span {
                    let j0 = if rough_amp > 0.0 { rng.signed() * rough_amp * 0.6 } else { 0.0 };
                    let j1 = if rough_amp > 0.0 { rng.signed() * rough_amp * 0.6 } else { 0.0 };
                    let _ = write!(
                        hatch,
                        "M {:.1} {:.1} L {:.1} {:.1} ",
                        x + t + j0,
                        y,
                        x + t - h + j1,
                        y + h
                    );
                    t += gap;
                }
                if fill_style == "cross-hatch" {
                    let mut t2 = -h;
                    while t2 <= span {
                        let j0 = if rough_amp > 0.0 { rng.signed() * rough_amp * 0.6 } else { 0.0 };
                        let j1 = if rough_amp > 0.0 { rng.signed() * rough_amp * 0.6 } else { 0.0 };
                        let _ = write!(
                            hatch,
                            "M {:.1} {:.1} L {:.1} {:.1} ",
                            x + t2 + j0,
                            y + h,
                            x + t2 - h + j1,
                            y
                        );
                        t2 += gap;
                    }
                }
            }
            let hatch_sw = (sw * 0.65).clamp(0.8, 4.0);
            let _ = write!(
                svg,
                r#"<g clip-path="url(#fill-{idx})"><path d="{hatch}" fill="none" stroke="{color}" stroke-width="{hatch_sw:.2}" stroke-linecap="round"/></g>"#,
            );
        }
        _ => {
            let _ = write!(svg, r#"<path d="{shape_path}" fill="{color}" stroke="none"/>"#);
        }
    }
}

fn emit_arrowhead(svg: &mut String, kind: &str, from: (f64, f64), tip: (f64, f64), stroke: &str, bg: &str, sw: f64) {
    let dx = tip.0 - from.0;
    let dy = tip.1 - from.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let nx = -uy;
    let ny = ux;
    let size = (10.0 + sw * 3.0).min(len * 0.9);
    match kind {
        "arrow" => {
            let spread = size * 0.45;
            let p1 = (tip.0 - ux * size + nx * spread, tip.1 - uy * size + ny * spread);
            let p2 = (tip.0 - ux * size - nx * spread, tip.1 - uy * size - ny * spread);
            let _ = write!(
                svg,
                r#"<path d="M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2}" fill="none" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linecap="round" stroke-linejoin="round"/>"#,
                p1.0, p1.1, tip.0, tip.1, p2.0, p2.1
            );
        }
        "triangle" | "triangle_outline" => {
            let spread = size * 0.45;
            let p1 = (tip.0 - ux * size + nx * spread, tip.1 - uy * size + ny * spread);
            let p2 = (tip.0 - ux * size - nx * spread, tip.1 - uy * size - ny * spread);
            let fill = if kind == "triangle" { stroke } else { bg };
            let _ = write!(
                svg,
                r#"<path d="M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z" fill="{fill}" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linejoin="round"/>"#,
                tip.0, tip.1, p1.0, p1.1, p2.0, p2.1
            );
        }
        "bar" => {
            let half = size * 0.5;
            let p1 = (tip.0 + nx * half, tip.1 + ny * half);
            let p2 = (tip.0 - nx * half, tip.1 - ny * half);
            let _ = write!(
                svg,
                r#"<path d="M {:.2} {:.2} L {:.2} {:.2}" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linecap="round"/>"#,
                p1.0, p1.1, p2.0, p2.1
            );
        }
        "dot" | "circle" | "circle_outline" => {
            let r = (size * 0.35).max(2.5);
            let cx = tip.0 - ux * r;
            let cy = tip.1 - uy * r;
            let fill = if kind == "circle_outline" { bg } else { stroke };
            let _ = write!(
                svg,
                r#"<circle cx="{cx:.2}" cy="{cy:.2}" r="{r:.2}" fill="{fill}" stroke="{stroke}" stroke-width="{sw:.2}"/>"#,
            );
        }
        "diamond" | "diamond_outline" => {
            let half = size * 0.42;
            let mid = (tip.0 - ux * size * 0.6, tip.1 - uy * size * 0.6);
            let back = (tip.0 - ux * size * 1.2, tip.1 - uy * size * 1.2);
            let left = (mid.0 + nx * half, mid.1 + ny * half);
            let right = (mid.0 - nx * half, mid.1 - ny * half);
            let fill = if kind == "diamond_outline" { bg } else { stroke };
            let _ = write!(
                svg,
                r#"<path d="M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2} Z" fill="{fill}" stroke="{stroke}" stroke-width="{sw:.2}" stroke-linejoin="round"/>"#,
                tip.0, tip.1, left.0, left.1, back.0, back.1, right.0, right.1
            );
        }
        _ => {}
    }
}

fn emit_text(svg: &mut String, el: &ExcalidrawElement, x: f64, y: f64, w: f64, h: f64, color: &str) {
    if el.text.is_empty() {
        return;
    }
    let font_size = if el.font_size.is_finite() { el.font_size.clamp(4.0, 400.0) } else { 20.0 };
    let line_height = if el.line_height.is_finite() { el.line_height.clamp(0.8, 3.0) } else { 1.25 };
    let line_h = font_size * line_height;
    let font_family = match &el.font_family {
        FontFamily::Numeric(1 | 5 | 8) => "Virgil, Comic Sans MS, Liberation Sans, DejaVu Sans, Noto Sans, cursive, sans-serif",
        FontFamily::Numeric(3) => "Cascadia Code, JetBrains Mono, Fira Code, Liberation Mono, DejaVu Sans Mono, Noto Sans Mono, monospace",
        FontFamily::Numeric(_) => "Helvetica, Arial, Liberation Sans, DejaVu Sans, Noto Sans, sans-serif",
        FontFamily::Named(n) => {
            let lower = n.to_ascii_lowercase();
            if lower.contains("mono") || lower.contains("code") || lower.contains("cascadia") {
                "Cascadia Code, JetBrains Mono, Liberation Mono, DejaVu Sans Mono, monospace"
            } else if lower.contains("virgil") || lower.contains("excalifont") || lower.contains("comic") {
                "Virgil, Comic Sans MS, Liberation Sans, DejaVu Sans, cursive, sans-serif"
            } else {
                "Helvetica, Arial, Liberation Sans, DejaVu Sans, sans-serif"
            }
        }
    };
    let (anchor, tx) = match el.text_align.as_str() {
        "center" => ("middle", x + w * 0.5),
        "right" => ("end", x + w),
        _ => ("start", x),
    };
    let lines: Vec<&str> = el.text.split('\n').collect();
    let total_h = lines.len() as f64 * line_h;
    let start_y = match el.vertical_align.as_str() {
        "middle" => y + ((h - total_h) * 0.5).max(0.0) + font_size * 0.85,
        "bottom" => y + (h - total_h).max(0.0) + font_size * 0.85,
        _ => y + font_size * 0.85,
    };

    let mut esc = String::new();
    for (i, line) in lines.iter().enumerate() {
        let ly = start_y + i as f64 * line_h;
        esc.clear();
        xml_escape(line, &mut esc);
        let _ = write!(
            svg,
            r#"<text x="{tx:.2}" y="{ly:.2}" fill="{color}" font-family="{font_family}" font-size="{font_size:.2}" text-anchor="{anchor}" xml:space="preserve">{esc}</text>"#,
        );
    }
}

fn rect_path(x: f64, y: f64, w: f64, h: f64, r: f64) -> String {
    if r <= 0.1 || w <= 0.0 || h <= 0.0 {
        return format!(
            "M {x:.2} {y:.2} L {:.2} {y:.2} L {:.2} {:.2} L {x:.2} {:.2} Z",
            x + w,
            x + w,
            y + h,
            y + h
        );
    }
    let r = r.min(w * 0.5).min(h * 0.5);
    format!(
        "M {:.2} {y:.2} L {:.2} {y:.2} Q {:.2} {y:.2} {:.2} {:.2} L {:.2} {:.2} Q {:.2} {:.2} {:.2} {:.2} L {:.2} {:.2} Q {x:.2} {:.2} {x:.2} {:.2} L {x:.2} {:.2} Q {x:.2} {y:.2} {:.2} {y:.2} Z",
        x + r,
        x + w - r,
        x + w,
        x + w,
        y + r,
        x + w,
        y + h - r,
        x + w,
        y + h,
        x + w - r,
        y + h,
        x + r,
        y + h,
        y + h,
        y + h - r,
        y + r,
        x + r,
    )
}

fn poly_path(pts: &[(f64, f64)], closed: bool) -> String {
    let mut s = String::new();
    for (i, &(px, py)) in pts.iter().enumerate() {
        if i == 0 {
            let _ = write!(s, "M {px:.2} {py:.2}");
        } else {
            let _ = write!(s, " L {px:.2} {py:.2}");
        }
    }
    if closed {
        s.push_str(" Z");
    }
    s
}

fn rounded_polygon_path(pts: &[(f64, f64)], radius: f64) -> String {
    if pts.len() < 3 || radius <= 0.1 {
        return poly_path(pts, true);
    }
    let n = pts.len();
    let mut s = String::new();
    for i in 0..n {
        let prev = pts[(i + n - 1) % n];
        let curr = pts[i];
        let next = pts[(i + 1) % n];
        let v1 = (prev.0 - curr.0, prev.1 - curr.1);
        let v2 = (next.0 - curr.0, next.1 - curr.1);
        let l1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
        let l2 = (v2.0 * v2.0 + v2.1 * v2.1).sqrt();
        let r = radius.min(l1 * 0.45).min(l2 * 0.45);
        if l1 < 1e-3 || l2 < 1e-3 {
            continue;
        }
        let a = (curr.0 + v1.0 * (r / l1), curr.1 + v1.1 * (r / l1));
        let b = (curr.0 + v2.0 * (r / l2), curr.1 + v2.1 * (r / l2));
        if i == 0 {
            let _ = write!(s, "M {:.2} {:.2}", a.0, a.1);
        } else {
            let _ = write!(s, " L {:.2} {:.2}", a.0, a.1);
        }
        let _ = write!(s, " Q {:.2} {:.2} {:.2} {:.2}", curr.0, curr.1, b.0, b.1);
    }
    s.push_str(" Z");
    s
}

fn smooth_curve_path(pts: &[(f64, f64)], closed: bool) -> String {
    if pts.len() < 3 {
        return poly_path(pts, closed);
    }
    let mut s = String::new();
    let _ = write!(s, "M {:.2} {:.2}", pts[0].0, pts[0].1);
    for i in 1..pts.len() - 1 {
        let mid_x = (pts[i].0 + pts[i + 1].0) * 0.5;
        let mid_y = (pts[i].1 + pts[i + 1].1) * 0.5;
        let _ = write!(s, " Q {:.2} {:.2} {mid_x:.2} {mid_y:.2}", pts[i].0, pts[i].1);
    }
    let last = pts[pts.len() - 1];
    let _ = write!(s, " L {:.2} {:.2}", last.0, last.1);
    if closed {
        s.push_str(" Z");
    }
    s
}

fn rough_poly_path(pts: &[(f64, f64)], closed: bool, amp: f64, rng: &mut Rng) -> String {
    if pts.len() < 2 {
        return String::new();
    }
    let mut s = String::new();
    let count = if closed { pts.len() } else { pts.len() - 1 };
    let first_x = pts[0].0 + rng.signed() * amp * 0.5;
    let first_y = pts[0].1 + rng.signed() * amp * 0.5;
    let _ = write!(s, "M {first_x:.2} {first_y:.2}");
    for i in 0..count {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        let mx = (a.0 + b.0) * 0.5 + rng.signed() * amp;
        let my = (a.1 + b.1) * 0.5 + rng.signed() * amp;
        let bx = b.0 + rng.signed() * amp * 0.45;
        let by = b.1 + rng.signed() * amp * 0.45;
        let _ = write!(s, " Q {mx:.2} {my:.2} {bx:.2} {by:.2}");
    }
    s
}

fn ellipse_path(cx: f64, cy: f64, rx: f64, ry: f64, amp: f64, rng: &mut Option<&mut Rng>) -> String {
    const K: f64 = 0.5522847498;
    let mut j = |scale: f64| -> f64 {
        match rng.as_mut() {
            Some(r) => r.signed() * amp * scale,
            None => 0.0,
        }
    };
    let rx = rx.max(0.5);
    let ry = ry.max(0.5);
    let ox = rx * K;
    let oy = ry * K;
    let (top_x, top_y) = (cx + j(0.4), cy - ry + j(0.4));
    let (right_x, right_y) = (cx + rx + j(0.4), cy + j(0.4));
    let (bot_x, bot_y) = (cx + j(0.4), cy + ry + j(0.4));
    let (left_x, left_y) = (cx - rx + j(0.4), cy + j(0.4));
    format!(
        "M {top_x:.2} {top_y:.2} C {:.2} {:.2} {:.2} {:.2} {right_x:.2} {right_y:.2} C {:.2} {:.2} {:.2} {:.2} {bot_x:.2} {bot_y:.2} C {:.2} {:.2} {:.2} {:.2} {left_x:.2} {left_y:.2} C {:.2} {:.2} {:.2} {:.2} {top_x:.2} {top_y:.2}",
        cx + ox + j(0.6),
        cy - ry + j(0.6),
        cx + rx + j(0.6),
        cy - oy + j(0.6),
        cx + rx + j(0.6),
        cy + oy + j(0.6),
        cx + ox + j(0.6),
        cy + ry + j(0.6),
        cx - ox + j(0.6),
        cy + ry + j(0.6),
        cx - rx + j(0.6),
        cy + oy + j(0.6),
        cx - rx + j(0.6),
        cy - oy + j(0.6),
        cx - ox + j(0.6),
        cy - ry + j(0.6),
    )
}

fn stroke_dasharray(style: &str, sw: f64) -> Option<&'static str> {
    match style {
        "dashed" => Some(if sw > 2.5 { "12 10" } else { "8 6" }),
        "dotted" => Some(if sw > 2.5 { "2 6" } else { "1.5 4.5" }),
        _ => None,
    }
}

fn xml_escape(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
}

fn is_safe_raster_data_url(url: &str) -> bool {
    let u = url.trim();
    if u.len() > 16 * 1024 * 1024 {
        return false;
    }
    let lower_prefix_ok = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/jpg;base64,",
        "data:image/webp;base64,",
        "data:image/gif;base64,",
    ]
    .iter()
    .any(|p| u.len() >= p.len() && u.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()));
    if !lower_prefix_ok {
        return false;
    }
    !u.bytes().any(|b| matches!(b, b'<' | b'>' | b'"' | b'\'' | b'&' | 0..=0x1f))
}

fn resolve_background(raw: Option<&str>, dark: bool) -> String {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return if dark { "#121212".into() } else { "#ffffff".into() };
    };
    if raw.eq_ignore_ascii_case("transparent") || raw.eq_ignore_ascii_case("none") {
        return if dark { "#121212".into() } else { "#ffffff".into() };
    }
    if dark && (raw.eq_ignore_ascii_case("#ffffff") || raw.eq_ignore_ascii_case("#fff") || raw.eq_ignore_ascii_case("white")) {
        return "#121212".into();
    }
    adapt_color(raw, dark).unwrap_or_else(|| if dark { "#121212".into() } else { "#ffffff".into() })
}

fn sanitize_color(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        if matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(s.to_ascii_lowercase());
        }
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if (lower.starts_with("rgb(") || lower.starts_with("rgba(") || lower.starts_with("hsl(") || lower.starts_with("hsla("))
        && lower.ends_with(')')
        && lower.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'(' | b')' | b',' | b'.' | b'%' | b' '))
    {
        return Some(lower);
    }
    if s.len() <= 24 && s.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Some(lower);
    }
    None
}

fn adapt_color(raw: &str, dark: bool) -> Option<String> {
    let clean = sanitize_color(raw)?;
    if !dark {
        return Some(clean);
    }
    let Some((r, g, b, a)) = parse_rgb(&clean) else {
        return Some(clean);
    };
    // Excalidraw dark theme filter: invert(93%) + hue-rotate(180deg) in sRGB.
    let inv = |c: f64| c * (1.0 - 0.93) + (255.0 - c) * 0.93;
    let (ir, ig, ib) = (inv(r as f64), inv(g as f64), inv(b as f64));
    // Standard CSS hue-rotate(180deg) matrix:
    let nr = (-0.574 * ir + 1.430 * ig + 0.144 * ib).clamp(0.0, 255.0).round() as u8;
    let ng = (0.426 * ir + 0.430 * ig + 0.144 * ib).clamp(0.0, 255.0).round() as u8;
    let nb = (0.426 * ir + 1.430 * ig - 0.856 * ib).clamp(0.0, 255.0).round() as u8;
    if (a - 1.0).abs() < 1e-3 {
        Some(format!("#{nr:02x}{ng:02x}{nb:02x}"))
    } else {
        Some(format!("rgba({nr}, {ng}, {nb}, {a:.3})"))
    }
}

fn parse_rgb(s: &str) -> Option<(u8, u8, u8, f64)> {
    if let Some(hex) = s.strip_prefix('#') {
        let bytes = hex.as_bytes();
        let nibble = |b: u8| -> Option<u8> {
            match b {
                b'0'..=b'9' => Some(b - b'0'),
                b'a'..=b'f' => Some(b - b'a' + 10),
                _ => None,
            }
        };
        let byte = |a: u8, b: u8| -> Option<u8> { Some((nibble(a)? << 4) | nibble(b)?) };
        return match bytes.len() {
            3 => Some((nibble(bytes[0])? * 17, nibble(bytes[1])? * 17, nibble(bytes[2])? * 17, 1.0)),
            4 => Some((
                nibble(bytes[0])? * 17,
                nibble(bytes[1])? * 17,
                nibble(bytes[2])? * 17,
                (nibble(bytes[3])? * 17) as f64 / 255.0,
            )),
            6 => Some((byte(bytes[0], bytes[1])?, byte(bytes[2], bytes[3])?, byte(bytes[4], bytes[5])?, 1.0)),
            8 => Some((
                byte(bytes[0], bytes[1])?,
                byte(bytes[2], bytes[3])?,
                byte(bytes[4], bytes[5])?,
                byte(bytes[6], bytes[7])? as f64 / 255.0,
            )),
            _ => None,
        };
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let p: Vec<&str> = inner.split(',').collect();
        if p.len() == 3 {
            return Some((
                p[0].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                p[1].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                p[2].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                1.0,
            ));
        }
    }
    if let Some(inner) = s.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
        let p: Vec<&str> = inner.split(',').collect();
        if p.len() == 4 {
            return Some((
                p[0].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                p[1].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                p[2].trim().parse::<f64>().ok()?.clamp(0.0, 255.0) as u8,
                p[3].trim().parse::<f64>().ok()?.clamp(0.0, 1.0),
            ));
        }
    }
    match s {
        "black" => Some((0, 0, 0, 1.0)),
        "white" => Some((255, 255, 255, 1.0)),
        "red" => Some((255, 0, 0, 1.0)),
        "green" => Some((0, 128, 0, 1.0)),
        "blue" => Some((0, 0, 255, 1.0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excalidraw::render_to_svg;

    #[test]
    fn emits_all_element_types_dark_light_deterministic_and_sanitized() {
        let json = r##"{
            "type": "excalidraw",
            "version": 2,
            "elements": [
                {"id": "f1", "type": "frame", "x": 0, "y": 0, "width": 500, "height": 400, "name": "System"},
                {"id": "r1", "type": "rectangle", "x": 20, "y": 30, "width": 100, "height": 50, "strokeColor": "#1e1e1e", "backgroundColor": "#a5d8ff", "fillStyle": "hachure", "roundness": {"type": 3}, "seed": 11, "frameId": "f1"},
                {"id": "e1", "type": "ellipse", "x": 150, "y": 30, "width": 80, "height": 50, "strokeColor": "#e03131", "backgroundColor": "#ffc9c9", "fillStyle": "cross-hatch", "seed": 22},
                {"id": "d1", "type": "diamond", "x": 260, "y": 30, "width": 90, "height": 70, "strokeColor": "#2f9e44", "backgroundColor": "#b2f2bb", "fillStyle": "zigzag", "seed": 33},
                {"id": "a1", "type": "arrow", "x": 120, "y": 55, "width": 30, "height": 0, "points": [[0,0],[15,10],[30,0]], "roundness": {"type": 2}, "startArrowhead": "dot", "endArrowhead": "triangle", "seed": 44},
                {"id": "l1", "type": "line", "x": 20, "y": 140, "width": 100, "height": 20, "points": [[0,0],[50,20],[100,0],[0,0]], "backgroundColor": "#ffec99", "fillStyle": "solid", "seed": 55},
                {"id": "fd1", "type": "freedraw", "x": 160, "y": 140, "width": 40, "height": 20, "points": [[0,0],[10,5],[25,15],[40,2]], "strokeWidth": 3, "seed": 66},
                {"id": "t1", "type": "text", "x": 25, "y": 40, "width": 90, "height": 30, "text": "<script>alert(1)</script>\nLine 2", "textAlign": "center", "verticalAlign": "middle", "fontFamily": 3},
                {"id": "img_bad", "type": "image", "x": 380, "y": 30, "width": 60, "height": 60, "fileId": "evil"},
                {"id": "img_ok", "type": "image", "x": 380, "y": 110, "width": 60, "height": 60, "fileId": "good"}
            ],
            "files": {
                "evil": {"id": "evil", "mimeType": "image/svg+xml", "dataURL": "https://evil.example/x.png"},
                "good": {"id": "good", "mimeType": "image/png", "dataURL": "data:image/png;base64,iVBORw0KGgo="}
            },
            "appState": {"viewBackgroundColor": "#ffffff"}
        }"##;

        let light1 = render_to_svg(json, &RenderOptions { dark: false, roughness: true }).unwrap();
        let light2 = render_to_svg(json, &RenderOptions { dark: false, roughness: true }).unwrap();
        assert_eq!(light1.svg, light2.svg);
        assert_eq!(light1.element_count, 10);
        assert_eq!(light1.background_color, "#ffffff");
        assert!(!light1.svg.contains("<script>"));
        assert!(light1.svg.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!light1.svg.contains("https://evil.example"));
        assert!(light1.svg.contains("data:image/png;base64,iVBORw0KGgo="));
        assert!(light1.svg.contains("clip-path=\"url(#frame-0)\""));

        let dark = render_to_svg(json, &RenderOptions { dark: true, roughness: true }).unwrap();
        assert_eq!(dark.background_color, "#121212");
        assert_ne!(light1.svg, dark.svg);
    }

    #[test]
    fn empty_and_extreme_coords_and_xss_color() {
        let empty = r#"{"elements": [{"type": "rectangle", "isDeleted": true}]}"#;
        assert_eq!(render_to_svg(empty, &RenderOptions::default()).unwrap_err(), ExcalidrawError::EmptyScene);

        let extreme = r##"{"elements": [
            {"type": "rectangle", "x": -10, "y": -20, "width": -50, "height": -30, "strokeColor": "\"><script>", "backgroundColor": "url(http://evil)"}
        ]}"##;
        let out = render_to_svg(extreme, &RenderOptions::default()).unwrap();
        assert!(!out.svg.contains("<script>"));
        assert!(!out.svg.contains("http://evil"));
    }
}
