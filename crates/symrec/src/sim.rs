//! Evaluation helpers: turning glyphs of any font into simulated, distorted
//! pen strokes. Not needed by the app.

use crate::{Config, Stroke, glyph_skeleton, raster};

/// Small deterministic xorshift generator.
#[derive(Clone)]
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03);
        for _ in 0..4 {
            r.next_u64();
        }
        r
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `0..1`.
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in `-a..a`.
    pub fn sym(&mut self, a: f32) -> f32 {
        (self.f32() * 2.0 - 1.0) * a
    }
    pub fn gauss(&mut self) -> f32 {
        let s: f32 = (0..6).map(|_| self.f32()).sum();
        (s - 3.0) * std::f32::consts::SQRT_2
    }
}

fn ends_dir(s: &[[f32; 2]], at_end: bool) -> [f32; 2] {
    let n = s.len();
    let (a, b) = if at_end { (s[n - 1], s[(n - 1).saturating_sub(3)]) } else { (s[0], s[3.min(n - 1)]) };
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l = (dx * dx + dy * dy).sqrt().max(1e-6);
    [dx / l, dy / l]
}

/// Joins skeleton edges meeting at junctions into longer strokes (as a pen
/// would draw them): at each shared endpoint the two most collinear edges are
/// merged.
fn join_edges(mut edges: Vec<Vec<[f32; 2]>>) -> Vec<Vec<[f32; 2]>> {
    loop {
        let mut best: Option<(f32, usize, bool, usize, bool)> = None;
        for i in 0..edges.len() {
            if edges[i].len() < 2 {
                continue;
            }
            for j in i + 1..edges.len() {
                if edges[j].len() < 2 {
                    continue;
                }
                for ei in [false, true] {
                    for ej in [false, true] {
                        let pi = if ei { edges[i][edges[i].len() - 1] } else { edges[i][0] };
                        let pj = if ej { edges[j][edges[j].len() - 1] } else { edges[j][0] };
                        if pi != pj {
                            continue;
                        }
                        let (a, b) = (ends_dir(&edges[i], ei), ends_dir(&edges[j], ej));
                        let dot = a[0] * b[0] + a[1] * b[1]; // -1 = perfectly straight through
                        if dot < -0.65 && best.is_none_or(|x| dot < x.0) {
                            best = Some((dot, i, ei, j, ej));
                        }
                    }
                }
            }
        }
        let Some((_, i, ei, j, ej)) = best else { break };
        let mut a = std::mem::take(&mut edges[i]);
        let mut b = std::mem::take(&mut edges[j]);
        if !ei {
            a.reverse();
        }
        if ej {
            b.reverse();
        }
        a.extend(b.into_iter().skip(1));
        edges[i] = a;
    }
    edges.retain(|e| !e.is_empty());
    edges
}

/// Pen strokes extracted from a glyph (centre-line, junction edges joined).
pub fn glyph_strokes(face: &ttf_parser::Face, ch: char, cfg: &Config) -> Option<Vec<Stroke>> {
    glyph_skeleton(face, ch, cfg).map(join_edges)
}

/// Filled glyph bitmap, `n x n`, uniformly scaled (long side = `n`) and centred.
pub fn bitmap(face: &ttf_parser::Face, ch: char, n: usize) -> Option<Vec<bool>> {
    let polys = raster::glyph_polys(face, face.glyph_index(ch)?)?;
    let (lo, hi) = raster::bounds(&polys);
    let ext = [hi[0] - lo[0], hi[1] - lo[1]];
    let sc = n as f32 / ext[0].max(ext[1]).max(1.0);
    let off = [(n as f32 - ext[0] * sc) / 2.0, (n as f32 - ext[1] * sc) / 2.0];
    let moved: Vec<raster::Poly> = polys.iter().map(|p| p.iter().map(|q| [(q[0] - lo[0]) * sc + off[0], (q[1] - lo[1]) * sc + off[1]]).collect()).collect();
    Some(raster::fill(&moved, n, n))
}

/// Distortion strength; `Distortion::default()` is the evaluation setting.
#[derive(Debug, Clone, Copy)]
pub struct Distortion {
    pub scale: f32,
    pub shear: f32,
    pub rot_deg: f32,
    pub jitter: f32,
    pub wobble: f32,
}

impl Default for Distortion {
    fn default() -> Self {
        Distortion { scale: 0.25, shear: 0.25, rot_deg: 8.0, jitter: 0.008, wobble: 0.015 }
    }
}

fn resample_random(s: &[[f32; 2]], rng: &mut Rng, size: f32) -> Vec<[f32; 2]> {
    let mut out = vec![s[0]];
    let mut step = size * (0.02 + 0.04 * rng.f32());
    let mut acc = 0.0f32;
    for w in s.windows(2) {
        let (a, b) = (w[0], w[1]);
        let l = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let mut pos = step - acc;
        while pos <= l && l > 0.0 {
            let t = pos / l;
            out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
            step = size * (0.015 + 0.05 * rng.f32());
            pos += step;
        }
        acc = l - (pos - step);
    }
    let last = s[s.len() - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// Applies seeded random distortions: anisotropic scale, shear, rotation,
/// point jitter, low-frequency wobble, random resampling, random stroke order
/// and direction. Output is in canvas-like coordinates (hundreds of units).
pub fn distort(strokes: &[Stroke], rng: &mut Rng, d: &Distortion) -> Vec<Stroke> {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in strokes.iter().flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let size = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
    let sx = 1.0 + rng.sym(d.scale);
    let sy = 1.0 + rng.sym(d.scale);
    let sh = rng.sym(d.shear);
    let th = rng.sym(d.rot_deg).to_radians();
    let (st, ct) = th.sin_cos();
    let canvas = 150.0 + 200.0 * rng.f32();
    let off = [100.0 + 100.0 * rng.f32(), 100.0 + 100.0 * rng.f32()];
    // Low-frequency wobble phases.
    let ph: [f32; 4] = std::array::from_fn(|_| rng.f32() * std::f32::consts::TAU);
    let fr: [f32; 2] = [2.0 + 3.0 * rng.f32(), 2.0 + 3.0 * rng.f32()];
    let mut out: Vec<Stroke> = Vec::new();
    for s in strokes {
        let s = if s.len() >= 2 { resample_random(s, rng, size) } else { s.clone() };
        let mut v: Stroke = s
            .iter()
            .map(|p| {
                let (x, y) = ((p[0] - c[0]) / size, (p[1] - c[1]) / size);
                let wob = [
                    d.wobble * (fr[0] * std::f32::consts::TAU * y + ph[0]).sin() + d.wobble * (fr[1] * std::f32::consts::TAU * x + ph[1]).sin(),
                    d.wobble * (fr[1] * std::f32::consts::TAU * y + ph[2]).sin() + d.wobble * (fr[0] * std::f32::consts::TAU * x + ph[3]).sin(),
                ];
                let (x, y) = (x * sx + sh * y * sy, y * sy);
                let (x, y) = (x * ct - y * st, x * st + y * ct);
                [off[0] + canvas * (x + wob[0] + d.jitter * rng.gauss()), off[1] + canvas * (y + wob[1] + d.jitter * rng.gauss())]
            })
            .collect();
        if rng.f32() < 0.5 {
            v.reverse();
        }
        out.push(v);
    }
    // Random order.
    for i in (1..out.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        out.swap(i, j);
    }
    out
}

/// Undistorted strokes scaled to canvas-like coordinates.
pub fn plain(strokes: &[Stroke]) -> Vec<Stroke> {
    strokes.iter().map(|s| s.iter().map(|p| [p[0] * 3.0 + 50.0, p[1] * 3.0 + 50.0]).collect()).collect()
}
