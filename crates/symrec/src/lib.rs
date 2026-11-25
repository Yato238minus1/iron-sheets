//! Offline hand-drawn symbol recognizer.
//!
//! Templates are built from font glyph outlines; a drawing (a list of pen
//! strokes) is ranked against all templates. No network, no model files.
//!
//! Public contract (the app depends on exactly this):
//!
//! ```ignore
//! let rec = Recognizer::build(&[font_bytes], symbols); // (id, char) pairs, ids may repeat
//! let hits: Vec<Match> = rec.recognize(&strokes, 20);   // best first
//! ```

mod chamfer;
mod feat;
mod raster;
pub mod sim;
mod skeleton;

pub use feat::Config;

/// One pen stroke: points in canvas coordinates (x right, y down), any scale.
pub type Stroke = Vec<[f32; 2]>;

/// A ranked candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    /// Caller-supplied symbol id.
    pub id: u32,
    /// Similarity in `0.0..=1.0`, higher is better.
    pub score: f32,
}

/// Glyph-template recognizer.
pub struct Recognizer {
    cfg: Config,
    dim: usize,
    /// Flat template features, `dim` floats each.
    feats: Vec<f32>,
    /// Template index -> slot (dense symbol index).
    slot: Vec<u32>,
    /// Slot -> caller id.
    ids: Vec<u32>,
    /// Oriented sample points of all templates, `pt_off[i]..pt_off[i+1]` per template.
    pts: Vec<chamfer::Pt>,
    pt_off: Vec<usize>,
    /// Distance maps, `(K+1) * G * G` bytes per template.
    maps: Vec<u8>,
}

/// Centre-line strokes (raster pixel coordinates, y down) of a glyph, one
/// polyline per skeleton edge.
pub(crate) fn glyph_skeleton(face: &ttf_parser::Face, ch: char, cfg: &Config) -> Option<Vec<Stroke>> {
    let gid = face.glyph_index(ch)?;
    let polys = raster::glyph_polys(face, gid)?;
    let (img, w, h, scale) = raster::rasterise(&polys, cfg.raster, 3);
    let rules = skeleton::BlobRules { q: cfg.blob_q, dot_frac: cfg.dot_frac, dot_px: cfg.dot_em * face.units_per_em() as f32 * scale };
    let st = skeleton::trace(&img, w, h, cfg.spur, 1.2, &rules);
    if st.is_empty() { None } else { Some(st) }
}

/// Picks the math font (one with a `MATH` table, preferring a regular, non
/// bold, non italic face whose name mentions "Math") out of a font list.
pub fn find_math_font<'a>(fonts: &[&'a [u8]]) -> Option<&'a [u8]> {
    let mut best: Option<(i32, &'a [u8])> = None;
    for &data in fonts {
        let Ok(face) = ttf_parser::Face::parse(data, 0) else { continue };
        if face.tables().math.is_none() {
            continue;
        }
        let mut score = 0;
        let names: Vec<String> = face.names().into_iter().filter_map(|n| n.to_string()).collect();
        if names.iter().any(|n| n.contains("Math")) {
            score += 4;
        }
        if names.iter().any(|n| n == "Regular") {
            score += 2;
        }
        if face.is_bold() {
            score -= 3;
        }
        if face.is_italic() {
            score -= 3;
        }
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, data));
        }
    }
    best.map(|b| b.1)
}

/// Shapes people actually draw for `ch` when they differ from the font's
/// upright glyph: lowercase Greek is written like the math-italic letters.
/// Yields nothing for other characters.
pub fn handwriting_aliases(ch: char) -> impl Iterator<Item = char> {
    let italic = match ch {
        '\u{3B1}'..='\u{3C9}' if ch != '\u{3C2}' => char::from_u32(0x1D6FC + (ch as u32 - 0x3B1)),
        '\u{3C2}' => char::from_u32(0x1D6FC + (0x3C2 - 0x3B1)),
        '\u{2202}' => char::from_u32(0x1D715),
        '\u{3F5}' => char::from_u32(0x1D716),
        '\u{3D1}' => char::from_u32(0x1D717),
        '\u{3F0}' => char::from_u32(0x1D718),
        '\u{3D5}' => char::from_u32(0x1D719),
        '\u{3F1}' => char::from_u32(0x1D71A),
        '\u{3D6}' => char::from_u32(0x1D71B),
        _ => None,
    };
    italic.into_iter()
}

impl Recognizer {
    /// Builds templates for `symbols` using the first font in `fonts` that has
    /// a glyph for each char. The same id may appear with several chars
    /// (alternative shapes); the best-scoring shape counts.
    pub fn build(fonts: &[&[u8]], symbols: impl IntoIterator<Item = (u32, char)>) -> Self {
        Self::build_with(fonts, symbols, Config::default())
    }

    /// Like [`Recognizer::build`] with explicit parameters.
    pub fn build_with(fonts: &[&[u8]], symbols: impl IntoIterator<Item = (u32, char)>, cfg: Config) -> Self {
        let faces: Vec<ttf_parser::Face> = fonts.iter().filter_map(|d| ttf_parser::Face::parse(d, 0).ok()).collect();
        let mut feats = Vec::new();
        let mut slot = Vec::new();
        let mut ids: Vec<u32> = Vec::new();
        let mut slot_of = std::collections::HashMap::new();
        let mut dim = 0;
        let mut pts = Vec::new();
        let mut pt_off = vec![0usize];
        let mut maps = Vec::new();
        for (id, ch) in symbols {
            let Some(st) = faces.iter().find_map(|f| glyph_skeleton(f, ch, &cfg)) else { continue };
            let Some(norm) = feat::normalise(&cfg, &st) else { continue };
            let f = feat::features(&cfg, &norm);
            dim = f.len();
            feats.extend_from_slice(&f);
            if cfg.shortlist > 0 {
                let p = chamfer::points(&feat::samples(&cfg, &norm, cfg.c_step));
                maps.extend_from_slice(&chamfer::build_maps(&p, cfg.c_pen));
                pts.extend_from_slice(&p);
                pt_off.push(pts.len());
            }
            let s = *slot_of.entry(id).or_insert_with(|| {
                ids.push(id);
                (ids.len() - 1) as u32
            });
            slot.push(s);
        }
        Recognizer { cfg, dim, feats, slot, ids, pts, pt_off, maps }
    }

    /// Number of symbol ids that got at least one template.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True if no templates were built.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Ranks symbols for a drawing; at most `k` results, best first, one per id.
    pub fn recognize(&self, strokes: &[Stroke], k: usize) -> Vec<Match> {
        if k == 0 || self.is_empty() {
            return Vec::new();
        }
        let Some(norm) = feat::normalise(&self.cfg, strokes) else { return Vec::new() };
        let q = feat::features(&self.cfg, &norm);
        let nt = self.slot.len();
        let mut cos = vec![0f32; nt];
        for (t, f) in self.feats.chunks_exact(self.dim).enumerate() {
            cos[t] = f.iter().zip(&q).map(|(a, b)| a * b).sum();
        }
        let mut final_score = cos.clone();
        let mut excess_len_pen = 0.0f32;
        if self.cfg.shortlist > 0 {
            // Shortlist by best template per id.
            let mut order: Vec<usize> = (0..nt).collect();
            order.sort_by(|&a, &b| cos[b].total_cmp(&cos[a]));
            let mut seen = vec![false; self.ids.len()];
            let mut cand = Vec::new();
            for &t in &order {
                let s = self.slot[t] as usize;
                if !seen[s] {
                    seen[s] = true;
                    cand.push(t);
                    if cand.len() >= self.cfg.shortlist {
                        break;
                    }
                }
            }
            let qp = chamfer::points(&feat::samples(&self.cfg, &norm, self.cfg.c_step));
            // Real math glyphs stay well under ~200 resampled points (`136` max across benchmark fonts);
            // dense space-filling scribbles exceed that and are penalized against matching dense glyphs.
            if qp.len() > 200 {
                excess_len_pen = (qp.len() - 200) as f32 * 0.002;
            }
            let qmaps = chamfer::build_maps(&qp, self.cfg.c_pen);
            let msz = (chamfer::K + 1) * chamfer::G * chamfer::G;
            let cfg = &self.cfg;
            let mut st: Vec<(usize, chamfer::Align)> = cand
                .iter()
                .map(|&t| {
                    let tp = &self.pts[self.pt_off[t]..self.pt_off[t + 1]];
                    let tm = &self.maps[t * msz..(t + 1) * msz];
                    let mut al = chamfer::Align::start(cfg, &qp, &qmaps, tp, tm);
                    al.run(cfg, cfg.c_rounds_a, &qp, &qmaps, tp, tm);
                    (t, al)
                })
                .collect();
            let sc = |t: usize, al: &chamfer::Align| 1.0 + cos[t] - cfg.c_weight * al.cost;
            if cfg.c_refine < st.len() {
                st.sort_by(|a, b| sc(b.0, &b.1).total_cmp(&sc(a.0, &a.1)));
                for (t, al) in st.iter_mut().take(cfg.c_refine) {
                    let tp = &self.pts[self.pt_off[*t]..self.pt_off[*t + 1]];
                    al.run(cfg, cfg.c_rounds, &qp, &qmaps, tp, &self.maps[*t * msz..(*t + 1) * msz]);
                }
            } else {
                for (t, al) in st.iter_mut() {
                    let tp = &self.pts[self.pt_off[*t]..self.pt_off[*t + 1]];
                    al.run(cfg, cfg.c_rounds, &qp, &qmaps, tp, &self.maps[*t * msz..(*t + 1) * msz]);
                }
            }
            for (i, (t, al)) in st.iter().enumerate() {
                // Candidates that were not refined keep a lower bonus so refined ones rank first.
                let bonus = if cfg.c_refine < st.len() && i >= cfg.c_refine { -1.0 } else { 0.0 };
                final_score[*t] = sc(*t, al) + bonus;
            }
        }
        let mut best = vec![f32::MIN; self.ids.len()];
        for (&slot_id, &score) in self.slot.iter().zip(&final_score) {
            let slot = slot_id as usize;
            if score > best[slot] {
                best[slot] = score;
            }
        }
        let mut order: Vec<usize> = (0..best.len()).collect();
        order.sort_by(|&a, &b| best[b].total_cmp(&best[a]).then(self.ids[a].cmp(&self.ids[b])));
        let to_score = |b: f32| {
            let base = if self.cfg.shortlist > 0 { b * 0.5 } else { b };
            (base - excess_len_pen).clamp(0.0, 1.0)
        };
        if let Some(&first) = order.first()
            && to_score(best[first]) < self.cfg.min_score
        {
            return Vec::new();
        }
        order
            .into_iter()
            .take(k)
            .map(|i| Match { id: self.ids[i], score: to_score(best[i]) })
            .collect()
    }
}

impl Recognizer {
    /// Ids that have at least one template, in build order.
    pub fn ids(&self) -> &[u32] {
        &self.ids
    }
}
