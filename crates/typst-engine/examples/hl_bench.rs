//! Highlighter measurements: `cargo run --release -p iron-sheets-typst --example hl_bench`
use std::time::Instant;

const CHUNK: &str = "= Heading {n}\n\nSome *strong* and _emph_ text with `raw` and a #link(\"https://x.org\")[link], a label <lab-{n}> and a ref @lab-{n}.\n\n- item one\n- item two with $a^2 + b^2 = c^2$\n+ numbered\n\n#let f(x, y: 2) = {\n  let z = x + y * 3\n  if z > 4 { [big #z] } else { [small] }\n}\n\n#f(3, y: 5)\n\n$ integral_0^oo e^(-x^2) dif x = sqrt(pi)/2 $\n\n```rust\nfn main() { println!(\"{}\", {n}); }\n```\n\n/ Term: a description list item\n\n// a comment\n/* block\ncomment */\n\nPlain paragraph text, long enough to be realistic: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor {n}.\n\n";

fn corpus(size: usize) -> String {
    let mut s = String::new();
    let mut n = 0;
    while s.len() < size {
        s.push_str(&CHUNK.replace("{n}", &n.to_string()));
        n += 1;
    }
    s
}

fn med(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let text = corpus(100_000);
    let mut out = Vec::new();
    let mut v = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        iron_sheets_typst::highlight(&text, &mut out);
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!("full highlight {} bytes: median {:.3} ms ({} spans)", text.len(), med(v), out.len());
    let mut v = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        std::hint::black_box(typst_syntax::parse(&text));
        v.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!("parse only: median {:.3} ms", med(v));
    let t = Instant::now();
    let mut inc = iron_sheets_typst::IncrementalTypstHighlighter::new();
    inc.update(&text);
    println!("first incremental update (cold): {:.3} ms", t.elapsed().as_secs_f64() * 1e3);
    let mut text = text;
    let (mut full, mut incr, mut redone) = (Vec::new(), Vec::new(), Vec::new());
    let mut seed = 0x9E3779B97F4A7C15u64;
    let anchors: Vec<usize> = text.match_indices("lorem ipsum dolor sit amet").map(|(k, m)| k + m.len()).collect();
    let _ = anchors;
    for _ in 0..400 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        // type in prose: right after a random "lorem ipsum dolor sit amet" (keeps the text error-free)
        let occ: Vec<usize> = text.match_indices("lorem ipsum dolor sit amet").map(|(k, m)| k + m.len()).collect();
        let at = occ[(seed >> 8) as usize % occ.len()];
        text.insert(at, 'x');
        let t = Instant::now();
        iron_sheets_typst::highlight(&text, &mut out);
        full.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        inc.update(&text);
        incr.push(t.elapsed().as_secs_f64() * 1e3);
        redone.push(inc.children_redone() as f64);
    }
    println!(
        "per keystroke in prose: full {:.3} ms, incremental {:.3} ms (median root children redone {}, incremental updates {}/400)",
        med(full),
        med(incr),
        med(redone),
        inc.incremental_updates()
    );
}
