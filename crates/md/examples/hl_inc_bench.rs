//! Full vs incremental highlight per keystroke: `cargo run --release -p iron-sheets-md --example hl_inc_bench`

use std::time::Instant;

const CHUNK: &str = "## Section {n}\n\nParagraph {n}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor [[Ideas]] and `code`, with inline math $x^{n} + y$ and a [link](https://example.org).\n\n- item one {n}\n- [ ] task {n}\n\n> quote {n} with a little more text in it\n\n```rust\nfn f{n}() { let a = {n}; }\n```\n\n| a | b |\n|---|---|\n| {n} | 2 |\n\n";

fn med(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let mut text = String::new();
    let mut n = 0;
    while text.len() < 100 * 1024 {
        text.push_str(&CHUNK.replace("{n}", &n.to_string()));
        n += 1;
    }
    let mut spans = Vec::new();
    let t = Instant::now();
    iron_sheets_md::highlight(&text, &mut spans);
    println!("first full highlight ({} bytes): {:.3} ms", text.len(), t.elapsed().as_secs_f64() * 1e3);
    let mut inc = iron_sheets_md::IncrementalHighlighter::new();
    let t = Instant::now();
    inc.update(&text);
    println!("first incremental update (cold): {:.3} ms", t.elapsed().as_secs_f64() * 1e3);

    let (mut full, mut incr, mut redone) = (Vec::new(), Vec::new(), Vec::new());
    let mut seed = 0x9E3779B97F4A7C15u64;
    for _ in 0..400 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut at = (seed >> 8) as usize % text.len();
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.insert(at, 'x');
        let t = Instant::now();
        iron_sheets_md::highlight(&text, &mut spans);
        full.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        inc.update(&text);
        incr.push(t.elapsed().as_secs_f64() * 1e3);
        redone.push(inc.lines_redone() as f64);
    }
    println!(
        "per keystroke at random positions: full {:.3} ms, incremental {:.3} ms (median lines redone {}, max {})",
        med(full),
        med(incr),
        med(redone.clone()),
        redone.iter().cloned().fold(0.0, f64::max)
    );
}
