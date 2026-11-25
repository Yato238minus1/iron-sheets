#![allow(dead_code)]
use iron_sheets_symrec::sim::{self, Distortion, Rng};
use iron_sheets_symrec::{Config, Recognizer, handwriting_aliases};
use std::collections::{HashMap, HashSet};

pub fn math_font() -> &'static [u8] {
    let fonts: Vec<&'static [u8]> = typst_assets::fonts().collect();
    iron_sheets_symrec::find_math_font(&fonts).expect("math font")
}

/// Single-codepoint symbols of Typst's `sym` module as `(id, char)`.
pub fn symbols() -> Vec<(u32, char)> {
    let mut out = Vec::new();
    fn walk(m: codex::Module, out: &mut Vec<(u32, char)>) {
        for (_, b) in m.iter() {
            match b.def {
                codex::Def::Module(m) => walk(m, out),
                codex::Def::Symbol(s) => {
                    for (_, v, dep) in s.variants() {
                        let mut it = v.chars();
                        if let (Some(c), None) = (it.next(), it.next())
                            && dep.is_none()
                            && !c.is_whitespace()
                        {
                            let id = out.len() as u32;
                            out.push((id, c));
                        }
                    }
                }
            }
        }
    }
    walk(codex::SYM, &mut out);
    out
}

/// Symbols plus handwriting aliases.
pub fn with_aliases(syms: &[(u32, char)]) -> Vec<(u32, char)> {
    let mut v = syms.to_vec();
    for &(id, c) in syms {
        for a in handwriting_aliases(c) {
            v.push((id, a));
        }
    }
    v
}

pub struct QueryFont {
    pub name: &'static str,
    pub data: Vec<u8>,
}

pub fn query_fonts() -> Vec<QueryFont> {
    [
        ("NotoSansMath", "/usr/share/fonts/noto/NotoSansMath-Regular.ttf"),
        ("DejaVuMathTeXGyre", "/usr/share/fonts/TTF/DejaVuMathTeXGyre.ttf"),
        ("DejaVuSans", "/usr/share/fonts/TTF/DejaVuSans.ttf"),
    ]
    .into_iter()
    .filter_map(|(name, p)| std::fs::read(p).ok().map(|data| QueryFont { name, data }))
    .collect()
}

/// For each template id the set of ids whose NCM glyphs are near-identical
/// (IoU of uniformly-fitted 28x28 fills >= `tau`); includes itself.
pub fn equivalence(math: &[u8], syms: &[(u32, char)], tau: f32) -> HashMap<u32, Vec<u32>> {
    let face = ttf_parser::Face::parse(math, 0).unwrap();
    const N: usize = 28;
    let bm: Vec<(u32, Vec<u64>)> = syms
        .iter()
        .filter_map(|&(id, c)| {
            let b = sim::bitmap(&face, c, N)?;
            let mut w = vec![0u64; (N * N).div_ceil(64)];
            for (i, &x) in b.iter().enumerate() {
                if x {
                    w[i / 64] |= 1 << (i % 64);
                }
            }
            Some((id, w))
        })
        .collect();
    let mut acc: HashMap<u32, Vec<u32>> = HashMap::new();
    for (id, _) in &bm {
        acc.entry(*id).or_default().push(*id);
    }
    for i in 0..bm.len() {
        for j in 0..bm.len() {
            if i == j || bm[i].0 == bm[j].0 {
                continue;
            }
            let (mut inter, mut uni) = (0u32, 0u32);
            for (a, b) in bm[i].1.iter().zip(&bm[j].1) {
                inter += (a & b).count_ones();
                uni += (a | b).count_ones();
            }
            if uni > 0 && inter as f32 / uni as f32 >= tau {
                acc.get_mut(&bm[i].0).unwrap().push(bm[j].0);
            }
        }
    }
    acc
}

#[derive(Debug, Default, Clone)]
pub struct Score {
    pub n: usize,
    pub top1: usize,
    pub top5: usize,
    pub top10: usize,
    pub top50: usize,
    pub rec_time: std::time::Duration,
    pub rec_max: std::time::Duration,
    /// (id, char, rank) of queries missing the top 10.
    pub misses: Vec<(u32, char, Vec<u32>)>,
}

impl Score {
    pub fn pct(&self) -> (f32, f32, f32) {
        let n = self.n.max(1) as f32;
        (100.0 * self.top1 as f32 / n, 100.0 * self.top5 as f32 / n, 100.0 * self.top10 as f32 / n)
    }
}

#[derive(Clone, Copy)]
pub struct Opts {
    pub distort: bool,
    pub trials: usize,
    /// Query the math-italic form of lowercase Greek (only Greek symbols).
    pub italic_greek_only: bool,
    pub seed: u64,
}

pub fn evaluate(rec: &Recognizer, cfg: &Config, qdata: &[u8], syms: &[(u32, char)], accept: &HashMap<u32, Vec<u32>>, o: Opts) -> Score {
    let Ok(qface) = ttf_parser::Face::parse(qdata, 0) else { return Score::default() };
    let have: HashSet<u32> = rec.ids().iter().copied().collect();
    let mut sc = Score::default();
    for &(id, c0) in syms {
        if !have.contains(&id) {
            continue;
        }
        let c = if o.italic_greek_only {
            match handwriting_aliases(c0).next() {
                Some(a) => a,
                None => continue,
            }
        } else {
            c0
        };
        if qface.glyph_index(c).is_none() {
            continue;
        }
        let Some(strokes) = sim::glyph_strokes(&qface, c, cfg) else { continue };
        let ok = accept.get(&id).cloned().unwrap_or_else(|| vec![id]);
        let trials = if o.distort { o.trials } else { 1 };
        for t in 0..trials {
            let q = if o.distort {
                let mut rng = Rng::new(o.seed ^ ((id as u64) << 8) ^ t as u64);
                sim::distort(&strokes, &mut rng, &Distortion::default())
            } else {
                sim::plain(&strokes)
            };
            let t0 = std::time::Instant::now();
            let hits = rec.recognize(&q, 50);
            let dt = t0.elapsed();
            sc.rec_time += dt;
            sc.rec_max = sc.rec_max.max(dt);
            let rank = hits.iter().position(|m| ok.contains(&m.id));
            sc.n += 1;
            if rank.is_some() {
                sc.top50 += 1;
            }
            match rank {
                Some(0) => {
                    sc.top1 += 1;
                    sc.top5 += 1;
                    sc.top10 += 1;
                }
                Some(r) if r < 5 => {
                    sc.top5 += 1;
                    sc.top10 += 1;
                }
                Some(r) if r < 10 => sc.top10 += 1,
                _ => sc.misses.push((id, c0, hits.iter().take(3).map(|m| m.id).collect())),
            }
        }
    }
    sc
}
