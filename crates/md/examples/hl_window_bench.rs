//! Cost of the window highlighter on big notes: cold build, keystroke update, one paragraph.
//! `cargo run --release -p iron-sheets-md --example hl_window_bench`
use iron_sheets_md::{IncrementalHighlighter, WindowHighlighter, highlight};
use std::time::Instant;

const UNIT: &str = "## Section\n\nParagraph: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, [[Ideas]] and `code`, with inline math $x^2 + y$ and a [link](https://example.org).\n\n- item one\n- [ ] task\n\n```rust\nfn main() { println!(\"x\"); }\n```\n\n> quote with a little more text in it\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n";

fn main() {
    for mb in [1usize, 5, 10] {
        let mut text = String::new();
        while text.len() < mb << 20 {
            text.push_str(UNIT);
        }
        let mut spans = Vec::new();
        let t = Instant::now();
        highlight(&text, &mut spans);
        let full = t.elapsed();
        let t = Instant::now();
        let mut inc = IncrementalHighlighter::new();
        inc.update(&text);
        let inc_cold = t.elapsed();
        let t = Instant::now();
        let mut w = WindowHighlighter::new();
        w.update(None, &text);
        let cold = t.elapsed();
        // Keystrokes in the middle.
        let mut mid = text.len() / 2;
        while !text.is_char_boundary(mid) || text.as_bytes()[mid] != b' ' {
            mid += 1;
        }
        let mut times = Vec::new();
        for _ in 0..50 {
            let old = text.clone();
            text.insert(mid, 'a');
            let t = Instant::now();
            w.update(Some(&old), &text);
            times.push(t.elapsed());
        }
        times.sort();
        // One paragraph in the middle.
        let ls = text[..mid].rfind('\n').map_or(0, |i| i + 1);
        let le = text[mid..].find('\n').map_or(text.len(), |i| mid + i);
        let mut toks = Vec::new();
        let t = Instant::now();
        for _ in 0..1000 {
            w.tokens(&text, ls, le, &mut toks);
        }
        let para = t.elapsed() / 1000;
        println!(
            "{mb} MB: full highlight {:.1} ms, incremental cold {:.1} ms, window cold {:.2} ms ({} regions), keystroke update median {:.3} ms, one paragraph {:.1} us",
            full.as_secs_f64() * 1e3,
            inc_cold.as_secs_f64() * 1e3,
            cold.as_secs_f64() * 1e3,
            w.region_count(),
            times[25].as_secs_f64() * 1e3,
            para.as_secs_f64() * 1e6
        );
    }
}
