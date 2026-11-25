mod common;
use common::*;
use iron_sheets_symrec::{Config, Recognizer, find_math_font, handwriting_aliases};

fn rec_all() -> (Recognizer, Vec<(u32, char)>) {
    let syms = symbols();
    let r = Recognizer::build(&[math_font()], with_aliases(&syms));
    (r, syms)
}

#[test]
fn send_sync() {
    fn is<T: Send + Sync>() {}
    is::<Recognizer>();
}

#[test]
fn degenerate_inputs_do_not_panic() {
    let r = Recognizer::build(&[math_font()], [(0, '+'), (1, '-'), (2, '|'), (3, 'α')]);
    assert_eq!(r.len(), 4);
    assert!(!r.is_empty());
    assert!(r.recognize(&[], 5).is_empty());
    assert!(r.recognize(&[vec![]], 5).is_empty());
    let _ = r.recognize(&[vec![[5.0, 5.0]]], 5);
    let _ = r.recognize(&[vec![[5.0, 5.0], [5.0, 5.0]]], 5);
    let line = r.recognize(&[vec![[0.0, 0.0], [100.0, 0.0]]], 3);
    assert_eq!(line[0].id, 1, "horizontal line is the minus");
    let _ = r.recognize(&[vec![[0.0, 0.0], [100.0, 100.0]]], 3);
    let bad = [vec![[f32::NAN, 1.0], [f32::INFINITY, 2.0], [3.0, f32::NEG_INFINITY]], vec![[1e30, 1e30], [-1e30, 5.0]]];
    let _ = r.recognize(&bad, 5);
    let _ = r.recognize(&[vec![[f32::NAN, f32::NAN]]], 5);
    assert!(r.recognize(&[vec![[0.0, 0.0], [1.0, 1.0]]], 0).is_empty());
    assert!(r.recognize(&[vec![[0.0, 0.0], [1.0, 1.0]]], 2).len() <= 2);
    let e = Recognizer::build(&[], [(0, 'a')]);
    assert!(e.is_empty() && e.recognize(&[vec![[0.0, 0.0], [1.0, 1.0]]], 3).is_empty());
    let e = Recognizer::build(&[&[1, 2, 3][..]], [(0, 'a')]);
    assert_eq!(e.len(), 0);
}

#[test]
fn math_font_found_and_aliases() {
    let fonts: Vec<&'static [u8]> = typst_assets::fonts().collect();
    assert!(find_math_font(&fonts).is_some());
    assert_eq!(handwriting_aliases('α').next(), Some('\u{1D6FC}'));
    assert_eq!(handwriting_aliases('ω').next(), Some('\u{1D714}'));
    assert_eq!(handwriting_aliases('+').count(), 0);
}

#[test]
fn ids_unique_in_results() {
    let (r, _) = rec_all();
    let hits = r.recognize(&[vec![[0.0, 0.0], [50.0, 60.0], [100.0, 0.0]]], 30);
    let mut ids: Vec<u32> = hits.iter().map(|m| m.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), hits.len());
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
}

/// Accuracy floors (measured: see README of the report); skipped without system fonts.
#[test]
fn accuracy_floors() {
    let fonts = query_fonts();
    if fonts.is_empty() {
        eprintln!("no system fonts, skipping");
        return;
    }
    let syms = symbols();
    let math = math_font();
    let cfg = Config::default();
    let r = Recognizer::build(&[math], with_aliases(&syms));
    let acc = equivalence(math, &syms, 0.8);
    for qf in &fonts {
        let o = |d| Opts { distort: d, trials: 1, italic_greek_only: false, seed: 7 };
        let p = evaluate(&r, &cfg, &qf.data, &syms, &acc, o(false)).pct();
        let d = evaluate(&r, &cfg, &qf.data, &syms, &acc, o(true)).pct();
        eprintln!("{} plain {:?} dist {:?}", qf.name, p, d);
        assert!(p.1 >= 83.0 && p.2 >= 86.0, "{} plain {p:?}", qf.name);
        assert!(d.0 >= 53.0 && d.1 >= 77.0 && d.2 >= 82.0, "{} dist {d:?}", qf.name);
    }
}

#[test]
fn chaotic_scribble_returns_empty_and_real_symbols_pass() {
    let (r, _) = rec_all();

    let minus = vec![vec![[10.0, 50.0], [90.0, 50.0]]];
    let plus = vec![vec![[20.0, 50.0], [80.0, 50.0]], vec![[50.0, 20.0], [50.0, 80.0]]];
    // Dense multi-loop chaotic scribble covering the canvas.
    let mut scribble = Vec::new();
    let mut pts = Vec::new();
    for i in 0..80 {
        let t = i as f32 * 0.45;
        let x = 50.0 + 45.0 * (t * 1.7).sin() + 12.0 * (t * 5.3).cos();
        let y = 50.0 + 45.0 * (t * 2.3).cos() + 12.0 * (t * 6.1).sin();
        pts.push([x, y]);
    }
    scribble.push(pts);
    // Criss-cross dense scribble.
    let mut dense = scribble.clone();
    for k in 0..6 {
        let p = 10.0 + k as f32 * 16.0;
        dense.push(vec![[p, 5.0], [95.0 - p, 95.0]]);
        dense.push(vec![[5.0, p], [95.0, 95.0 - p]]);
    }

    assert!(!r.recognize(&minus, 5).is_empty(), "minus must still match");
    assert!(!r.recognize(&plus, 5).is_empty(), "plus must still match");
    assert!(r.recognize(&scribble, 5).is_empty(), "chaotic scribble should return empty");
    assert!(r.recognize(&dense, 5).is_empty(), "dense criss-cross scribble should return empty");
}
