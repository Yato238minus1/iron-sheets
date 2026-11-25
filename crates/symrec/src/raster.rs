//! Glyph outlines: flattening and a small anti-aliased scanline filler.

use ttf_parser::{Face, GlyphId, OutlineBuilder};

pub type Poly = Vec<[f32; 2]>;

struct Flatten {
    polys: Vec<Poly>,
    cur: Poly,
    last: [f32; 2],
}

impl Flatten {
    fn flush(&mut self) {
        if self.cur.len() >= 3 {
            self.polys.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
}

impl OutlineBuilder for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.last = [x, -y];
        self.cur.push(self.last);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.last = [x, -y];
        self.cur.push(self.last);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = self.last;
        let (c, p1) = ([x1, -y1], [x, -y]);
        const N: usize = 8;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let u = 1.0 - t;
            self.cur.push([u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p1[0], u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p1[1]]);
        }
        self.last = p1;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = self.last;
        let (c1, c2, p1) = ([x1, -y1], [x2, -y2], [x, -y]);
        const N: usize = 10;
        for i in 1..=N {
            let t = i as f32 / N as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.cur.push([a * p0[0] + b * c1[0] + c * c2[0] + d * p1[0], a * p0[1] + b * c1[1] + c * c2[1] + d * p1[1]]);
        }
        self.last = p1;
    }
    fn close(&mut self) {
        self.flush();
    }
}

/// Flattened outline of a glyph, y pointing down, font units.
pub fn glyph_polys(face: &Face, gid: GlyphId) -> Option<Vec<Poly>> {
    let mut b = Flatten { polys: Vec::new(), cur: Vec::new(), last: [0.0; 2] };
    face.outline_glyph(gid, &mut b)?;
    b.flush();
    if b.polys.is_empty() { None } else { Some(b.polys) }
}

/// Binary raster (`coverage >= 0.5`) of polygons already in pixel coordinates,
/// non-zero winding rule, 4 sub-scanlines per pixel with exact x coverage.
pub fn fill(polys: &[Poly], w: usize, h: usize) -> Vec<bool> {
    const SS: usize = 4;
    let mut rows: Vec<Vec<(f32, i8)>> = vec![Vec::new(); h * SS];
    for p in polys {
        let n = p.len();
        for i in 0..n {
            let (a, b) = (p[i], p[(i + 1) % n]);
            if a[1] == b[1] {
                continue;
            }
            let (dir, lo, hi) = if b[1] > a[1] { (1i8, a, b) } else { (-1i8, b, a) };
            let k0 = ((lo[1] * SS as f32 - 0.5).ceil().max(0.0)) as usize;
            let k1f = (hi[1] * SS as f32 - 0.5).ceil();
            if k1f <= 0.0 {
                continue;
            }
            let k1 = (k1f as usize).min(h * SS);
            #[allow(clippy::needless_range_loop)]
            for k in k0..k1 {
                let y = (k as f32 + 0.5) / SS as f32;
                let t = (y - lo[1]) / (hi[1] - lo[1]);
                rows[k].push((lo[0] + t * (hi[0] - lo[0]), dir));
            }
        }
    }
    let mut cov = vec![0f32; w * h];
    for (k, r) in rows.iter_mut().enumerate() {
        if r.is_empty() {
            continue;
        }
        r.sort_by(|a, b| a.0.total_cmp(&b.0));
        let row = &mut cov[(k / SS) * w..(k / SS + 1) * w];
        let mut wind = 0i32;
        for i in 0..r.len() - 1 {
            wind += r[i].1 as i32;
            if wind != 0 {
                add_span(row, r[i].0, r[i + 1].0);
            }
        }
    }
    cov.iter().map(|&c| c >= 0.5 * SS as f32).collect()
}

fn add_span(row: &mut [f32], xa: f32, xb: f32) {
    let w = row.len() as f32;
    let (xa, xb) = (xa.clamp(0.0, w), xb.clamp(0.0, w));
    if xb <= xa {
        return;
    }
    let (ia, ib) = (xa.floor() as usize, (xb.floor() as usize).min(row.len() - 1));
    if ia == ib {
        row[ia] += xb - xa;
    } else {
        row[ia] += ia as f32 + 1.0 - xa;
        for c in row.iter_mut().take(ib).skip(ia + 1) {
            *c += 1.0;
        }
        row[ib] += xb - ib as f32;
    }
}

/// Bounding box of polygons: `(min, max)`.
pub fn bounds(polys: &[Poly]) -> ([f32; 2], [f32; 2]) {
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    for p in polys.iter().flatten() {
        for d in 0..2 {
            lo[d] = lo[d].min(p[d]);
            hi[d] = hi[d].max(p[d]);
        }
    }
    (lo, hi)
}

/// Rasterises `polys` so the longer side measures `size` pixels with `pad`
/// pixels of margin. Returns `(bitmap, w, h, scale)`.
pub fn rasterise(polys: &[Poly], size: f32, pad: usize) -> (Vec<bool>, usize, usize, f32) {
    let (lo, hi) = bounds(polys);
    let ext = [hi[0] - lo[0], hi[1] - lo[1]];
    let long = ext[0].max(ext[1]).max(1e-3);
    let scale = size / long;
    let w = (ext[0] * scale).ceil() as usize + 2 * pad + 1;
    let h = (ext[1] * scale).ceil() as usize + 2 * pad + 1;
    let moved: Vec<Poly> =
        polys.iter().map(|p| p.iter().map(|q| [(q[0] - lo[0]) * scale + pad as f32, (q[1] - lo[1]) * scale + pad as f32]).collect()).collect();
    (fill(&moved, w, h), w, h, scale)
}
