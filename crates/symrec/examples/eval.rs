//! `cargo run --release --example eval -- [key=value ...] [full]`
//! Keys: rho gamma bins grids(4,8) step tangent smooth blur dot_weight dot_len raster spur aliases(0/1)
#[path = "../tests/common/mod.rs"]
mod common;
use common::*;
use iron_sheets_symrec::{Config, Recognizer};
use std::time::Instant;

fn ascii(strokes: &[Vec<[f32; 2]>]) {
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in strokes.iter().flatten() {
        for d in 0..2 {
            lo[d] = lo[d].min(p[d]);
            hi[d] = hi[d].max(p[d]);
        }
    }
    let long = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
    let (w, h) = (((hi[0] - lo[0]) / long * 38.0) as usize + 1, ((hi[1] - lo[1]) / long * 19.0) as usize + 1);
    let mut g = vec![vec![' '; w + 1]; h + 1];
    for (si, s) in strokes.iter().enumerate() {
        let ch = (b'a' + (si % 26) as u8) as char;
        for seg in s.windows(2).chain(if s.len() == 1 { Some(&s[..]) } else { None }) {
            let (a, b) = (seg[0], *seg.last().unwrap());
            for t in 0..=40 {
                let t = t as f32 / 40.0;
                let x = ((a[0] + t * (b[0] - a[0]) - lo[0]) / long * 38.0) as usize;
                let y = ((a[1] + t * (b[1] - a[1]) - lo[1]) / long * 19.0) as usize;
                g[y][x] = ch;
            }
        }
    }
    for r in g {
        println!("{}", r.into_iter().collect::<String>());
    }
}

fn main() {
    if let Some(a) = std::env::args().find(|a| a.starts_with("show=")) {
        let fonts = query_fonts();
        let math = math_font();
        let cfg = Config::default();
        for c in a[5..].chars() {
            let mut all: Vec<(&str, &[u8])> = vec![("NCM", math)];
            for f in &fonts {
                all.push((f.name, &f.data));
            }
            for (n, d) in all {
                let face = ttf_parser::Face::parse(d, 0).unwrap();
                println!("--- {c} {n}");
                match iron_sheets_symrec::sim::glyph_strokes(&face, c, &cfg) {
                    Some(s) => ascii(&s),
                    None => println!("(none)"),
                }
            }
        }
        return;
    }
    let mut cfg = Config::default();
    let mut aliases = false;
    let mut full = false;
    let mut misses = false;
    for a in std::env::args().skip(1) {
        if a == "full" {
            full = true;
            continue;
        }
        if a == "misses" {
            misses = true;
            continue;
        }
        let Some((k, v)) = a.split_once('=') else { continue };
        match k {
            "rho" => cfg.rho = v.parse().unwrap(),
            "gamma" => cfg.gamma = v.parse().unwrap(),
            "bins" => cfg.bins = v.parse().unwrap(),
            "grids" => cfg.grids = v.split(',').map(|x| x.parse().unwrap()).collect(),
            "step" => cfg.step = v.parse().unwrap(),
            "tangent" => cfg.tangent = v.parse().unwrap(),
            "smooth" => cfg.smooth = v.parse().unwrap(),
            "blur" => cfg.blur = v.parse().unwrap(),
            "dot_weight" => cfg.dot_weight = v.parse().unwrap(),
            "dot_len" => cfg.dot_len = v.parse().unwrap(),
            "raster" => cfg.raster = v.parse().unwrap(),
            "blob_q" => cfg.blob_q = v.parse().unwrap(),
            "dot_frac" => cfg.dot_frac = v.parse().unwrap(),
            "dot_em" => cfg.dot_em = v.parse().unwrap(),
            "spur" => cfg.spur = v.parse().unwrap(),
            "shortlist" => cfg.shortlist = v.parse().unwrap(),
            "c_pen" => cfg.c_pen = v.parse().unwrap(),
            "c_cap" => cfg.c_cap = v.parse().unwrap(),
            "c_rounds_a" => cfg.c_rounds_a = v.parse().unwrap(),
            "c_refine" => cfg.c_refine = v.parse().unwrap(),
            "c_rounds" => cfg.c_rounds = v.parse().unwrap(),
            "c_step" => cfg.c_step = v.parse().unwrap(),
            "c_reg_scale" => cfg.c_reg_scale = v.parse().unwrap(),
            "c_reg_shear" => cfg.c_reg_shear = v.parse().unwrap(),
            "c_reg_rot" => cfg.c_reg_rot = v.parse().unwrap(),
            "c_reg_shift" => cfg.c_reg_shift = v.parse().unwrap(),
            "c_weight" => cfg.c_weight = v.parse().unwrap(),
            "aliases" => aliases = v == "1",
            _ => panic!("unknown key {k}"),
        }
    }
    let math = math_font();
    let syms = symbols();
    let build_syms = if aliases { with_aliases(&syms) } else { syms.clone() };
    let t = Instant::now();
    let rec = Recognizer::build_with(&[math], build_syms.iter().copied(), cfg.clone());
    let bt = t.elapsed();
    let acc = equivalence(math, &syms, 0.8);
    let avg: f32 = acc.values().map(|v| v.len() as f32).sum::<f32>() / acc.len() as f32;
    println!("ids {} build {:?} equiv-set {avg:.2} aliases={aliases} {:?}", rec.len(), bt, cfg);
    let mut tot = [0.0f32; 3];
    let mut cnt = 0.0;
    for qf in query_fonts() {
        let plain = evaluate(&rec, &cfg, &qf.data, &syms, &acc, Opts { distort: false, trials: 1, italic_greek_only: false, seed: 1 });
        let seeds = if full { 4 } else { 2 };
        let dist = evaluate(&rec, &cfg, &qf.data, &syms, &acc, Opts { distort: true, trials: seeds, italic_greek_only: false, seed: 7 });
        let g = evaluate(&rec, &cfg, &qf.data, &syms, &acc, Opts { distort: true, trials: seeds, italic_greek_only: true, seed: 7 });
        let f = |s: &Score| {
            let p = s.pct();
            format!("n={:4} {:5.1} {:5.1} {:5.1} (t50 {:5.1})", s.n, p.0, p.1, p.2, 100.0 * s.top50 as f32 / s.n.max(1) as f32)
        };
        println!("{:18} plain {} | dist {} | greek-italic dist {}", qf.name, f(&plain), f(&dist), f(&g));
        println!("  recognize avg {:?} max {:?}", dist.rec_time / dist.n.max(1) as u32, dist.rec_max);
        let p = dist.pct();
        tot[0] += p.0;
        tot[1] += p.1;
        tot[2] += p.2;
        cnt += 1.0;
        if misses {
            let m: Vec<String> =
                plain.misses.iter().map(|x| format!("{}->{}", x.1, x.2.iter().map(|i| syms[*i as usize].1.to_string()).collect::<String>())).collect();
            println!("  plain misses(top10): {}", m.join(" "));
        }
    }
    println!("MEAN dist top1/5/10: {:.1} {:.1} {:.1}", tot[0] / cnt, tot[1] / cnt, tot[2] / cnt);
}
