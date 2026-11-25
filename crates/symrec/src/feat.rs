//! Shape normalisation and directional-element features.

use crate::Stroke;

/// Tunable parameters (defaults are the measured optimum).
#[derive(Debug, Clone)]
pub struct Config {
    /// Aspect ratios below this are left alone (lines stay lines).
    pub rho: f32,
    /// Exponent for aspect compression above `rho` (1 = keep aspect, 0 = stretch to square).
    pub gamma: f32,
    /// Orientation bins over 0..180 degrees.
    pub bins: usize,
    /// Grid sizes of the feature pyramid.
    pub grids: Vec<usize>,
    /// Resampling step (fraction of the long side).
    pub step: f32,
    /// Half window (in samples) for tangent estimation.
    pub tangent: usize,
    /// Smoothing passes of the resampled polyline.
    pub smooth: usize,
    /// Spatial blur passes ([1 2 1] kernel) on feature grids.
    pub blur: usize,
    /// Weight of a dot (as equivalent stroke length).
    pub dot_weight: f32,
    /// Strokes shorter than this fraction of the long side are dots.
    pub dot_len: f32,
    /// Glyph raster size in pixels (long side) used for templates.
    pub raster: f32,
    /// Spur pruning length in raster pixels.
    pub spur: usize,
    /// Solid components with area/perimeter/length above this are outlined.
    pub blob_q: f32,
    /// Round components below this fraction of the glyph size are dots.
    pub dot_frac: f32,
    /// Round glyphs below this many em are dots.
    pub dot_em: f32,
    /// Shortlist size for the chamfer stage (0 disables it).
    pub shortlist: usize,
    /// Chamfer: orientation mismatch penalty (adjacent class), distance cap.
    pub c_pen: f32,
    pub c_cap: f32,
    pub c_rounds: usize,
    /// Rounds applied to every shortlisted candidate, then `c_rounds` more for the best `c_refine`.
    pub c_rounds_a: usize,
    pub c_refine: usize,
    pub c_step: f32,
    pub c_reg_scale: f32,
    pub c_reg_shear: f32,
    pub c_reg_rot: f32,
    pub c_reg_shift: f32,
    /// Final score = cosine - c_weight * chamfer cost.
    pub c_weight: f32,
    /// Minimum similarity score required for a candidate to be returned (0.0 disables filtering).
    pub min_score: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rho: 0.15,
            gamma: 0.5,
            bins: 8,
            grids: vec![8],
            step: 1.0 / 48.0,
            tangent: 3,
            smooth: 1,
            blur: 0,
            dot_weight: 0.08,
            dot_len: 0.04,
            raster: 100.0,
            spur: 4,
            blob_q: 0.12,
            dot_frac: 0.12,
            dot_em: 0.3,
            shortlist: 40,
            c_pen: 0.03,
            c_cap: 0.35,
            c_rounds: 2,
            c_rounds_a: 1,
            c_refine: 15,
            c_step: 1.0 / 16.0,
            c_reg_scale: 0.05,
            c_reg_shear: 0.05,
            c_reg_rot: 0.05,
            c_reg_shift: 0.1,
            c_weight: 4.0,
            min_score: 0.46,
        }
    }
}

fn clean(strokes: &[Stroke]) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    for s in strokes {
        let mut v: Vec<[f64; 2]> = Vec::with_capacity(s.len());
        for p in s {
            if p[0].is_finite() && p[1].is_finite() {
                let q = [p[0] as f64, p[1] as f64];
                if v.last() != Some(&q) {
                    v.push(q);
                }
            }
        }
        if !v.is_empty() {
            out.push(v);
        }
    }
    out
}

fn path_len(v: &[[f64; 2]]) -> f64 {
    v.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum()
}

/// Normalises strokes into the unit square (centre 0.5,0.5). Dots become
/// one-point strokes. Returns `None` for empty/unusable input.
pub fn normalise(cfg: &Config, strokes: &[Stroke]) -> Option<Vec<Vec<[f32; 2]>>> {
    let mut st = clean(strokes);
    if st.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in st.iter().flatten() {
        for d in 0..2 {
            lo[d] = lo[d].min(p[d]);
            hi[d] = hi[d].max(p[d]);
        }
    }
    let ext = [hi[0] - lo[0], hi[1] - lo[1]];
    let long = ext[0].max(ext[1]);
    if !(long.is_finite()) || long <= 1e-9 * (lo[0].abs().max(lo[1].abs()).max(hi[0].abs()).max(hi[1].abs())).max(1e-30) {
        return Some(vec![vec![[0.5, 0.5]]]);
    }
    // Dots.
    for s in st.iter_mut() {
        if path_len(s) < cfg.dot_len as f64 * long {
            let n = s.len() as f64;
            let c = [s.iter().map(|p| p[0]).sum::<f64>() / n, s.iter().map(|p| p[1]).sum::<f64>() / n];
            *s = vec![c];
        }
    }
    let r = (ext[0].min(ext[1]) / long) as f32;
    let rp = if r <= cfg.rho { r } else { cfg.rho + (1.0 - cfg.rho) * ((r - cfg.rho) / (1.0 - cfg.rho)).powf(cfg.gamma) };
    let k = if r > 1e-6 { (rp / r) as f64 } else { 1.0 };
    let (kx, ky) = if ext[0] >= ext[1] { (1.0, k) } else { (k, 1.0) };
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    // long side maps to 1 (with a 6% margin on each side kept for the blur).
    let s = 1.0 / long;
    let out = st.iter().map(|v| v.iter().map(|p| [(0.5 + (p[0] - c[0]) * s * kx) as f32, (0.5 + (p[1] - c[1]) * s * ky) as f32]).collect()).collect();
    Some(out)
}

fn resample(v: &[[f32; 2]], step: f32) -> Vec<[f32; 2]> {
    let mut out = vec![v[0]];
    let mut acc = 0.0f32;
    for w in v.windows(2) {
        let (a, b) = (w[0], w[1]);
        let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        if l <= 0.0 {
            continue;
        }
        let mut pos = step - acc;
        while pos <= l {
            let t = pos / l;
            out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
            pos += step;
        }
        acc = l - (pos - step);
    }
    let last = v[v.len() - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

fn smooth(v: &mut [[f32; 2]], passes: usize) {
    for _ in 0..passes {
        if v.len() < 3 {
            return;
        }
        let o = v.to_vec();
        for i in 1..o.len() - 1 {
            for d in 0..2 {
                v[i][d] = 0.25 * o[i - 1][d] + 0.5 * o[i][d] + 0.25 * o[i + 1][d];
            }
        }
    }
}

/// A point sample of a stroke with its (unsigned, 0..pi) tangent angle; dots
/// have `theta == None`.
#[derive(Clone, Copy)]
pub struct Sample {
    pub p: [f32; 2],
    pub theta: Option<f32>,
}

/// Resamples normalised strokes at `step`, smoothed, with tangent angles.
pub fn samples(cfg: &Config, strokes: &[Vec<[f32; 2]>], step: f32) -> Vec<Sample> {
    let mut out = Vec::new();
    for s in strokes {
        if s.len() < 2 {
            out.push(Sample { p: s[0], theta: None });
            continue;
        }
        let mut pts = resample(s, step);
        smooth(&mut pts, cfg.smooth);
        let n = pts.len();
        let m = cfg.tangent;
        let mut last_theta = 0.0f32;
        for i in 0..n {
            let a = pts[i.saturating_sub(m)];
            let b = pts[(i + m).min(n - 1)];
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let theta = if dx * dx + dy * dy > 1e-12 {
                let t = dy.atan2(dx);
                let t = if t < 0.0 { t + std::f32::consts::PI } else { t };
                if t >= std::f32::consts::PI { 0.0 } else { t }
            } else {
                last_theta
            };
            last_theta = theta;
            out.push(Sample { p: pts[i], theta: Some(theta) });
        }
    }
    out
}

/// Feature vector of normalised strokes: for each grid level the unsigned
/// orientation histograms (soft-binned in angle and position), sqrt-compressed
/// and L2-normalised; levels concatenated.
pub fn features(cfg: &Config, strokes: &[Vec<[f32; 2]>]) -> Vec<f32> {
    let k = cfg.bins;
    let mut levels: Vec<Vec<f32>> = cfg.grids.iter().map(|&g| vec![0.0; g * g * k]).collect();
    let splat = |levels: &mut Vec<Vec<f32>>, p: [f32; 2], theta: Option<f32>, wgt: f32| {
        for (li, &g) in cfg.grids.iter().enumerate() {
            let u = (p[0] * g as f32 - 0.5).clamp(0.0, (g - 1) as f32);
            let v = (p[1] * g as f32 - 0.5).clamp(0.0, (g - 1) as f32);
            let (i0, j0) = (u.floor() as usize, v.floor() as usize);
            let (fu, fv) = (u - i0 as f32, v - j0 as f32);
            let (i1, j1) = ((i0 + 1).min(g - 1), (j0 + 1).min(g - 1));
            let mut ob: [(usize, f32); 2] = [(0, 0.0); 2];
            let mut nb = 0;
            if let Some(t) = theta {
                let b = t / std::f32::consts::PI * k as f32;
                let b0 = b.floor();
                let f = b - b0;
                let b0 = (b0 as isize).rem_euclid(k as isize) as usize;
                ob[0] = (b0, 1.0 - f);
                ob[1] = ((b0 + 1) % k, f);
                nb = 2;
            }
            let cells = [(i0, j0, (1.0 - fu) * (1.0 - fv)), (i1, j0, fu * (1.0 - fv)), (i0, j1, (1.0 - fu) * fv), (i1, j1, fu * fv)];
            for (ci, cj, cw) in cells {
                let base = (cj * g + ci) * k;
                if nb == 0 {
                    for b in 0..k {
                        levels[li][base + b] += wgt * cw / k as f32;
                    }
                } else {
                    for &(b, bw) in &ob[..nb] {
                        levels[li][base + b] += wgt * cw * bw;
                    }
                }
            }
        }
    };
    for sm in samples(cfg, strokes, cfg.step) {
        match sm.theta {
            Some(_) => splat(&mut levels, sm.p, sm.theta, cfg.step),
            None => splat(&mut levels, sm.p, None, cfg.dot_weight),
        }
    }
    let mut out = Vec::new();
    for (li, &g) in cfg.grids.iter().enumerate() {
        let lv = &mut levels[li];
        for _ in 0..cfg.blur {
            blur(lv, g, k);
        }
        let mut nrm = 0.0f32;
        for v in lv.iter_mut() {
            *v = v.sqrt();
            nrm += *v * *v;
        }
        let inv = if nrm > 0.0 { 1.0 / nrm.sqrt() } else { 0.0 };
        let inv = inv / (cfg.grids.len() as f32).sqrt();
        out.extend(lv.iter().map(|v| v * inv));
    }
    out
}

fn blur(l: &mut [f32], g: usize, k: usize) {
    let o = l.to_vec();
    for j in 0..g {
        for i in 0..g {
            for b in 0..k {
                let mut s = 0.0;
                let mut w = 0.0;
                for dj in -1i32..=1 {
                    for di in -1i32..=1 {
                        let (x, y) = (i as i32 + di, j as i32 + dj);
                        if x < 0 || y < 0 || x >= g as i32 || y >= g as i32 {
                            continue;
                        }
                        let kw = (2 - di.abs()) as f32 * (2 - dj.abs()) as f32;
                        s += kw * o[(y as usize * g + x as usize) * k + b];
                        w += kw;
                    }
                }
                l[(j * g + i) * k + b] = s / w;
            }
        }
    }
}
