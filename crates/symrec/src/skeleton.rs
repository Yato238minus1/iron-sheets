//! Thinning (Zhang-Suen), spur pruning and tracing of a binary raster into
//! centre-line polylines.

const NB: [(i32, i32); 8] = [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)];

/// Up to eight neighbour coordinates without allocating.
struct Nbrs {
    v: [(i32, i32); 8],
    n: usize,
}

impl Nbrs {
    fn is_empty(&self) -> bool {
        self.n == 0
    }
}

impl IntoIterator for Nbrs {
    type Item = (i32, i32);
    type IntoIter = std::iter::Take<std::array::IntoIter<(i32, i32), 8>>;
    fn into_iter(self) -> Self::IntoIter {
        self.v.into_iter().take(self.n)
    }
}

struct Grid {
    w: usize,
    h: usize,
    d: Vec<u8>,
}

impl Grid {
    #[inline]
    fn get(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 { 0 } else { self.d[y as usize * self.w + x as usize] }
    }
    fn deg(&self, x: i32, y: i32) -> usize {
        NB.iter().filter(|(dx, dy)| self.get(x + dx, y + dy) != 0).count()
    }
    fn set(&mut self, x: i32, y: i32, v: u8) {
        self.d[y as usize * self.w + x as usize] = v;
    }
    fn neighbours(&self, x: i32, y: i32) -> Nbrs {
        let mut r = Nbrs { v: [(0, 0); 8], n: 0 };
        for (dx, dy) in NB {
            if self.get(x + dx, y + dy) != 0 {
                r.v[r.n] = (x + dx, y + dy);
                r.n += 1;
            }
        }
        r
    }
}

/// How to treat solid (non stroke-like) components.
#[derive(Debug, Clone, Copy)]
pub struct BlobRules {
    /// Components whose area / perimeter / long side exceeds this are outlined instead of thinned.
    pub q: f32,
    /// Round components shorter than this fraction of the longest component are dots.
    pub dot_frac: f32,
    /// Round components shorter than this many pixels are dots.
    pub dot_px: f32,
}

/// Replaces solid components by their outline, small round ones by a dot
/// (returned as points).
fn split_blobs(g: &mut Grid, r: &BlobRules) -> Vec<[f32; 2]> {
    let (w, h) = (g.w, g.h);
    let mut seen = vec![false; w * h];
    struct Comp {
        px: Vec<usize>,
        lo: [usize; 2],
        hi: [usize; 2],
    }
    let mut comps: Vec<Comp> = Vec::new();
    for start in 0..w * h {
        if g.d[start] == 0 || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut c = Comp { px: vec![start], lo: [w, h], hi: [0, 0] };
        let mut i = 0;
        while i < c.px.len() {
            let p = c.px[i];
            i += 1;
            let (x, y) = (p % w, p / w);
            c.lo = [c.lo[0].min(x), c.lo[1].min(y)];
            c.hi = [c.hi[0].max(x), c.hi[1].max(y)];
            for (dx, dy) in NB {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let q = ny as usize * w + nx as usize;
                if g.d[q] != 0 && !seen[q] {
                    seen[q] = true;
                    c.px.push(q);
                }
            }
        }
        comps.push(c);
    }
    let long_of = |c: &Comp| ((c.hi[0] - c.lo[0]).max(c.hi[1] - c.lo[1]) + 1) as f32;
    let overall = comps.iter().map(long_of).fold(1.0, f32::max);
    let mut dots = Vec::new();
    for c in &comps {
        let long = long_of(c);
        let short = ((c.hi[0] - c.lo[0]).min(c.hi[1] - c.lo[1]) + 1) as f32;
        let area = c.px.len() as f32;
        let fill = area / (long * short);
        if (long < r.dot_frac * overall || long < r.dot_px) && short / long > 0.5 && fill > 0.5 {
            let n = c.px.len() as f32;
            let sx: f32 = c.px.iter().map(|&p| (p % w) as f32 + 0.5).sum();
            let sy: f32 = c.px.iter().map(|&p| (p / w) as f32 + 0.5).sum();
            dots.push([sx / n, sy / n]);
            for &p in &c.px {
                g.d[p] = 0;
            }
            continue;
        }
        let is_border = |g: &Grid, p: usize| {
            let (x, y) = ((p % w) as i32, (p / w) as i32);
            [(0, -1), (1, 0), (0, 1), (-1, 0)].iter().any(|(dx, dy)| g.get(x + dx, y + dy) == 0)
        };
        let border: Vec<usize> = c.px.iter().copied().filter(|&p| is_border(g, p)).collect();
        if border.is_empty() {
            continue;
        }
        if area / border.len() as f32 / long > r.q {
            for &p in &c.px {
                g.d[p] = 0;
            }
            for &p in &border {
                g.d[p] = 1;
            }
        }
    }
    dots
}

fn thin(g: &mut Grid) {
    let w = g.w as isize;
    // N NE E SE S SW W NW as flat offsets.
    let off: [isize; 8] = [-w, -w + 1, 1, w + 1, w, w - 1, -1, -w - 1];
    let n = g.d.len();
    let mut inlist = vec![false; n];
    let mut cand: Vec<usize> = Vec::new();
    let border = |g: &Grid, i: usize| off.iter().any(|&o| g.d[(i as isize + o) as usize] == 0);
    for y in 1..g.h - 1 {
        for x in 1..g.w - 1 {
            let i = y * g.w + x;
            if g.d[i] != 0 && border(g, i) {
                cand.push(i);
                inlist[i] = true;
            }
        }
    }
    let mut del: Vec<usize> = Vec::new();
    let mut idle = 0;
    let mut step = 0;
    while idle < 2 && !cand.is_empty() {
        del.clear();
        for &i in &cand {
            if g.d[i] == 0 {
                continue;
            }
            let p: [u8; 8] = std::array::from_fn(|k| g.d[(i as isize + off[k]) as usize]);
            let b: u8 = p[0] + p[1] + p[2] + p[3] + p[4] + p[5] + p[6] + p[7];
            if !(2..=6).contains(&b) {
                continue;
            }
            let a = (0..8).filter(|&k| p[k] == 0 && p[(k + 1) % 8] != 0).count();
            if a != 1 {
                continue;
            }
            let ok = if step == 0 { p[0] * p[2] * p[4] == 0 && p[2] * p[4] * p[6] == 0 } else { p[0] * p[2] * p[6] == 0 && p[0] * p[4] * p[6] == 0 };
            if ok {
                del.push(i);
            }
        }
        if del.is_empty() {
            idle += 1;
        } else {
            idle = 0;
            for &i in &del {
                g.d[i] = 0;
            }
            for &i in &del {
                for &o in &off {
                    let j = (i as isize + o) as usize;
                    if g.d[j] != 0 && !inlist[j] {
                        inlist[j] = true;
                        cand.push(j);
                    }
                }
            }
        }
        // Drop deleted pixels from the list.
        cand.retain(|&i| {
            if g.d[i] == 0 {
                inlist[i] = false;
                false
            } else {
                true
            }
        });
        step ^= 1;
    }
}

/// Removes pixels that merely round a staircase corner.
fn clean_corners(g: &mut Grid, list: &[(i32, i32)]) {
    for _ in 0..3 {
        let mut any = false;
        {
            for &(x, y) in list {
                if g.get(x, y) == 0 {
                    continue;
                }
                let n = g.neighbours(x, y);
                if n.n == 2 {
                    let (a, b) = (n.v[0], n.v[1]);
                    if (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1 {
                        g.set(x, y, 0);
                        any = true;
                    }
                }
            }
        }
        if !any {
            break;
        }
    }
}

fn prune(g: &mut Grid, list: &[(i32, i32)], spur: usize) {
    for _ in 0..2 {
        let mut removed = false;
        {
            for &(x, y) in list {
                if g.get(x, y) == 0 || g.deg(x, y) != 1 {
                    continue;
                }
                let mut path = vec![(x, y)];
                let mut prev = (-1, -1);
                let mut cur = (x, y);
                let mut junction = false;
                for _ in 0..=spur {
                    let nn: Vec<_> = g.neighbours(cur.0, cur.1).into_iter().filter(|&q| q != prev && !path.contains(&q)).collect();
                    if nn.len() != 1 {
                        junction = nn.len() > 1;
                        break;
                    }
                    let nx = nn[0];
                    if g.deg(nx.0, nx.1) != 2 {
                        junction = true;
                        break;
                    }
                    prev = cur;
                    cur = nx;
                    path.push(cur);
                }
                if junction && path.len() < spur {
                    for &(px, py) in &path {
                        g.set(px, py, 0);
                    }
                    removed = true;
                }
            }
        }
        if !removed {
            break;
        }
        clean_corners(g, list);
    }
}

fn dp(pts: &[[f32; 2]], eps: f32, out: &mut Vec<[f32; 2]>) {
    // Douglas-Peucker; pushes all but the first point.
    let n = pts.len();
    if n < 2 {
        return;
    }
    let (a, b) = (pts[0], pts[n - 1]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let mut best = (0.0f32, 0usize);
    for (i, p) in pts.iter().enumerate().take(n - 1).skip(1) {
        let d = if len < 1e-6 { ((p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)).sqrt() } else { ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / len };
        if d > best.0 {
            best = (d, i);
        }
    }
    if best.0 > eps {
        dp(&pts[..=best.1], eps, out);
        dp(&pts[best.1..], eps, out);
    } else {
        out.push(b);
    }
}

pub fn simplify(pts: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    dp(pts, eps, &mut out);
    out
}

/// Thins a binary image and traces its skeleton into polylines (pixel
/// coordinates, pixel centres at +0.5). Edges of a junction share the
/// junction's exact coordinates.
pub fn trace(img: &[bool], w: usize, h: usize, spur: usize, eps: f32, blobs: &BlobRules) -> Vec<Vec<[f32; 2]>> {
    let mut g = Grid { w, h, d: img.iter().map(|&b| b as u8).collect() };
    for x in 0..w {
        g.d[x] = 0;
        g.d[(h - 1) * w + x] = 0;
    }
    for y in 0..h {
        g.d[y * w] = 0;
        g.d[y * w + w - 1] = 0;
    }
    let dots = split_blobs(&mut g, blobs);
    thin(&mut g);
    let list: Vec<(i32, i32)> = (0..h as i32).flat_map(|y| (0..w as i32).map(move |x| (x, y))).filter(|&(x, y)| g.get(x, y) != 0).collect();
    clean_corners(&mut g, &list);
    if spur > 0 {
        prune(&mut g, &list, spur);
    }
    let list: Vec<(i32, i32)> = list.into_iter().filter(|&(x, y)| g.get(x, y) != 0).collect();

    // Nodes: endpoints and clusters of junction pixels.
    let mut label = vec![-1i32; w * h];
    let mut cent: Vec<[f32; 2]> = Vec::new();
    for &(x, y) in &list {
        {
            let i = y as usize * w + x as usize;
            if g.d[i] == 0 || label[i] >= 0 {
                continue;
            }
            let dg = g.deg(x, y);
            if dg == 2 {
                continue;
            }
            let id = cent.len() as i32;
            let mut stack = vec![(x, y)];
            label[i] = id;
            let (mut sx, mut sy, mut c) = (0.0f32, 0.0f32, 0.0f32);
            while let Some((cx, cy)) = stack.pop() {
                sx += cx as f32 + 0.5;
                sy += cy as f32 + 0.5;
                c += 1.0;
                if dg < 3 {
                    continue;
                }
                for (nx, ny) in g.neighbours(cx, cy) {
                    let j = ny as usize * w + nx as usize;
                    if label[j] < 0 && g.deg(nx, ny) >= 3 {
                        label[j] = id;
                        stack.push((nx, ny));
                    }
                }
            }
            cent.push([sx / c, sy / c]);
        }
    }
    let mut visited = vec![false; w * h];
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let idx = |x: i32, y: i32| y as usize * w + x as usize;
    for &(x, y) in &list {
        {
            let i = idx(x, y);
            if g.d[i] == 0 || label[i] < 0 {
                continue;
            }
            let from = label[i];
            let nbs = g.neighbours(x, y);
            if nbs.is_empty() {
                out.push(vec![cent[from as usize]]);
                continue;
            }
            for (qx, qy) in nbs {
                let qi = idx(qx, qy);
                if label[qi] == from {
                    continue;
                }
                if label[qi] >= 0 {
                    if from < label[qi] {
                        out.push(vec![cent[from as usize], cent[label[qi] as usize]]);
                    }
                    continue;
                }
                if visited[qi] {
                    continue;
                }
                let mut pts = vec![cent[from as usize]];
                let mut prev = (x, y);
                let mut cur = (qx, qy);
                loop {
                    visited[idx(cur.0, cur.1)] = true;
                    pts.push([cur.0 as f32 + 0.5, cur.1 as f32 + 0.5]);
                    let next = g.neighbours(cur.0, cur.1).into_iter().find(|&q| q != prev);
                    let Some(nx) = next else { break };
                    let ni = idx(nx.0, nx.1);
                    if label[ni] >= 0 {
                        pts.push(cent[label[ni] as usize]);
                        break;
                    }
                    if visited[ni] {
                        break;
                    }
                    prev = cur;
                    cur = nx;
                }
                out.push(simplify(&pts, eps));
            }
        }
    }
    out.extend(dots.into_iter().map(|d| vec![d]));
    // Pure loops.
    for &(x, y) in &list {
        {
            let i = idx(x, y);
            if g.d[i] == 0 || label[i] >= 0 || visited[i] {
                continue;
            }
            let start = (x, y);
            let mut pts = Vec::new();
            let mut prev = (-1, -1);
            let mut cur = start;
            loop {
                visited[idx(cur.0, cur.1)] = true;
                pts.push([cur.0 as f32 + 0.5, cur.1 as f32 + 0.5]);
                let next = g.neighbours(cur.0, cur.1).into_iter().find(|&q| q != prev && !visited[idx(q.0, q.1)]);
                let Some(nx) = next else { break };
                prev = cur;
                cur = nx;
            }
            pts.push(pts[0]);
            out.push(simplify(&pts, eps));
        }
    }
    out
}
