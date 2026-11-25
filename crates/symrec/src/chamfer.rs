//! Oriented chamfer matching with a small affine alignment search, used to
//! re-rank the shortlist of the feature stage.

use crate::feat::{Config, Sample};

/// Distance-map grid size and covered range (unit square plus margin).
pub const G: usize = 32;
const LO: f32 = -0.1;
const SPAN: f32 = 1.2;
/// Orientation classes (over 180 degrees).
pub const K: usize = 6;
/// Distance quantum of the stored u8 maps.
const Q: f32 = 0.004;

#[derive(Clone, Copy)]
pub struct Pt {
    pub p: [f32; 2],
    /// Orientation class, or `K` for dots.
    pub c: u8,
}

pub fn points(samples: &[Sample]) -> Vec<Pt> {
    samples
        .iter()
        .map(|s| Pt {
            p: s.p,
            c: match s.theta {
                Some(t) => ((t / std::f32::consts::PI * K as f32 + 0.5) as usize % K) as u8,
                None => K as u8,
            },
        })
        .collect()
}

/// `K + 1` distance maps (class maps and an orientation-free map), quantised.
pub fn build_maps(pts: &[Pt], pen: f32) -> Vec<u8> {
    let mut maps = vec![f32::INFINITY; (K + 1) * G * G];
    for q in pts {
        let x = (((q.p[0] - LO) / SPAN * G as f32).floor().clamp(0.0, (G - 1) as f32)) as usize;
        let y = (((q.p[1] - LO) / SPAN * G as f32).floor().clamp(0.0, (G - 1) as f32)) as usize;
        let cell = y * G + x;
        for m in 0..=K {
            let v = if m == K || q.c as usize == K {
                0.0
            } else {
                let d = (m as isize - q.c as isize).unsigned_abs();
                let d = d.min(K - d);
                if d == 0 {
                    0.0
                } else if d == 1 {
                    pen
                } else {
                    continue;
                }
            };
            let slot = &mut maps[m * G * G + cell];
            if v < *slot {
                *slot = v;
            }
        }
    }
    let cell = SPAN / G as f32;
    let (w1, w2) = (cell, cell * std::f32::consts::SQRT_2);
    for m in 0..=K {
        let d = &mut maps[m * G * G..(m + 1) * G * G];
        for y in 0..G {
            for x in 0..G {
                let mut v = d[y * G + x];
                if x > 0 {
                    v = v.min(d[y * G + x - 1] + w1);
                }
                if y > 0 {
                    v = v.min(d[(y - 1) * G + x] + w1);
                    if x > 0 {
                        v = v.min(d[(y - 1) * G + x - 1] + w2);
                    }
                    if x + 1 < G {
                        v = v.min(d[(y - 1) * G + x + 1] + w2);
                    }
                }
                d[y * G + x] = v;
            }
        }
        for y in (0..G).rev() {
            for x in (0..G).rev() {
                let mut v = d[y * G + x];
                if x + 1 < G {
                    v = v.min(d[y * G + x + 1] + w1);
                }
                if y + 1 < G {
                    v = v.min(d[(y + 1) * G + x] + w1);
                    if x + 1 < G {
                        v = v.min(d[(y + 1) * G + x + 1] + w2);
                    }
                    if x > 0 {
                        v = v.min(d[(y + 1) * G + x - 1] + w2);
                    }
                }
                d[y * G + x] = v;
            }
        }
    }
    maps.iter().map(|&v| (v / Q).min(255.0) as u8).collect()
}

#[inline]
fn lookup(maps: &[u8], class: u8, p: [f32; 2], cap: f32) -> f32 {
    let m = class as usize;
    let u = ((p[0] - LO) / SPAN * G as f32 - 0.5).clamp(0.0, (G - 1) as f32 - 1e-3);
    let v = ((p[1] - LO) / SPAN * G as f32 - 0.5).clamp(0.0, (G - 1) as f32 - 1e-3);
    let (i, j) = (u as usize, v as usize);
    let (fu, fv) = (u - i as f32, v - j as f32);
    let base = m * G * G + j * G + i;
    let a = maps[base] as f32;
    let b = maps[base + 1] as f32;
    let c = maps[base + G] as f32;
    let d = maps[base + G + 1] as f32;
    let val = ((a * (1.0 - fu) + b * fu) * (1.0 - fv) + (c * (1.0 - fu) + d * fu) * fv) * Q;
    // Outside the map area add the clamping distance.
    const E0: f32 = LO + 0.5 * SPAN / G as f32;
    const E1: f32 = LO + SPAN - 0.5 * SPAN / G as f32;
    let ox = (E0 - p[0]).max(p[0] - E1).max(0.0);
    let oy = (E0 - p[1]).max(p[1] - E1).max(0.0);
    (val + ox + oy).min(cap)
}

/// Affine alignment parameters.
#[derive(Clone, Copy, Default)]
struct Aff {
    tx: f32,
    ty: f32,
    sx: f32,
    sy: f32,
    sh: f32,
    rot: f32,
}

impl Aff {
    fn id() -> Self {
        Aff { sx: 1.0, sy: 1.0, ..Default::default() }
    }
    fn matrix(&self) -> [f32; 4] {
        let (s, c) = self.rot.sin_cos();
        // R * [[sx, sh*sy],[0, sy]]
        [c * self.sx, c * self.sh * self.sy - s * self.sy, s * self.sx, s * self.sh * self.sy + c * self.sy]
    }
    fn reg(&self, cfg: &Config) -> f32 {
        cfg.c_reg_scale * ((self.sx - 1.0).abs() + (self.sy - 1.0).abs())
            + cfg.c_reg_shear * self.sh.abs()
            + cfg.c_reg_rot * self.rot.abs()
            + cfg.c_reg_shift * (self.tx.abs() + self.ty.abs())
    }
}

fn cost(cfg: &Config, a: &Aff, q: &[Pt], qmaps: &[u8], t: &[Pt], tmaps: &[u8]) -> f32 {
    let m = a.matrix();
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-4 {
        return f32::INFINITY;
    }
    let inv = [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det];
    let cap = cfg.c_cap;
    let mut s1 = 0.0;
    for p in q {
        let (x, y) = (p.p[0] - 0.5, p.p[1] - 0.5);
        let r = [0.5 + m[0] * x + m[1] * y + a.tx, 0.5 + m[2] * x + m[3] * y + a.ty];
        s1 += lookup(tmaps, p.c, r, cap);
    }
    let mut s2 = 0.0;
    for p in t {
        let (x, y) = (p.p[0] - 0.5 - a.tx, p.p[1] - 0.5 - a.ty);
        let r = [0.5 + inv[0] * x + inv[1] * y, 0.5 + inv[2] * x + inv[3] * y];
        s2 += lookup(qmaps, p.c, r, cap);
    }
    s1 / q.len().max(1) as f32 + s2 / t.len().max(1) as f32 + a.reg(cfg)
}

/// Resumable alignment search state.
#[derive(Clone, Copy)]
pub struct Align {
    a: Aff,
    pub cost: f32,
    step: [f32; 6],
}

impl Align {
    pub fn start(cfg: &Config, q: &[Pt], qmaps: &[u8], t: &[Pt], tmaps: &[u8]) -> Self {
        let a = Aff::id();
        Align { a, cost: cost(cfg, &a, q, qmaps, t, tmaps), step: [0.04, 0.04, 0.12, 0.12, 0.12, 0.12] }
    }

    /// Runs `rounds` coordinate-descent rounds with halving step sizes.
    pub fn run(&mut self, cfg: &Config, rounds: usize, q: &[Pt], qmaps: &[u8], t: &[Pt], tmaps: &[u8]) {
        for _ in 0..rounds {
            for k in 0..6 {
                for dir in [-1.0f32, 1.0] {
                    let mut b = self.a;
                    let st = self.step[k];
                    match k {
                        0 => b.tx += dir * st,
                        1 => b.ty += dir * st,
                        2 => b.sx *= 1.0 + dir * st,
                        3 => b.sy *= 1.0 + dir * st,
                        4 => b.sh += dir * st,
                        _ => b.rot += dir * st,
                    }
                    let c = cost(cfg, &b, q, qmaps, t, tmaps);
                    if c < self.cost {
                        self.cost = c;
                        self.a = b;
                        break;
                    }
                }
            }
            for s in self.step.iter_mut() {
                *s *= 0.5;
            }
        }
    }
}
