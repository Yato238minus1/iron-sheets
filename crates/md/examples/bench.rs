//! Throughput benchmark: `cargo run -p iron-sheets-md --example bench --release`.

use std::time::{Duration, Instant};

const CHUNKS: &[&str] = &[
    "# Section {n}: planning the [[Roadmap]] and #project/alpha\n\nThe quick brown fox jumps over the lazy dog, again and again; this is *emphasis*, **strong text**, `inline code` and a [link](https://example.com/path?q={n}) with ~~strike~~ and ==highlight==. Visit https://example.org/{n} for more. See also [[Note {n}|alias]] and [[Other#Heading]].\n\n",
    "## Notes {n}\n\n- first item with some text\n- [ ] open task {n}\n- [x] done task\n  - nested item with `code`\n  - another nested one\n1. ordered one\n2. ordered two\n\n",
    "> A quoted paragraph that goes on for a while, with **bold** words and a footnote reference[^{n}].\n> Second quoted line.\n\n[^{n}]: The footnote text for {n}.\n\n",
    "```rust\nfn main() {\n    let x = {n};\n    println!(\"{}\", x);\n}\n```\n\n",
    "| Name | Value | Notes |\n|:-----|------:|-------|\n| alpha | {n} | first row |\n| beta | 2 | `code` cell |\n| gamma | 3 | [[Link]] |\n\n",
    "Inline math $a^2 + b^2 = c^2$ in a sentence, then display math:\n\n$$\n\\int_0^{n} x\\,dx = \\frac{n^2}{2}\n$$\n\n",
    "> [!NOTE]\n> An alert body with text.\n\n![[diagram-{n}.png]]\n\n![alt text](images/pic{n}.png \"A title\")\n\n---\n\n",
    "A longer paragraph of plain prose to make the corpus realistic. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. {n}\n\n",
];

fn corpus(size: usize) -> String {
    let mut s = String::with_capacity(size + 1024);
    let mut n = 0usize;
    while s.len() < size {
        for c in CHUNKS {
            s.push_str(&c.replace("{n}", &n.to_string()));
            n += 1;
        }
    }
    s
}

fn measure<F: FnMut()>(mut f: F) -> f64 {
    f(); // warm up
    let t = Instant::now();
    let mut iters = 0u32;
    while t.elapsed() < Duration::from_millis(600) || iters < 3 {
        f();
        iters += 1;
    }
    t.elapsed().as_secs_f64() / iters as f64
}

fn main() {
    let mut spans = Vec::new();
    for (label, size) in [("10 KB", 10_000), ("100 KB", 100_000), ("1 MB", 1_000_000)] {
        let text = corpus(size);
        let mb = text.len() as f64 / 1e6;
        let doc = iron_sheets_md::parse(&text);
        let tp = measure(|| {
            std::hint::black_box(iron_sheets_md::parse(std::hint::black_box(&text)));
        });
        let th = measure(|| {
            iron_sheets_md::highlight(std::hint::black_box(&text), &mut spans);
        });
        println!(
            "{label:>7} ({} bytes, {} blocks): parse {:7.1} MB/s ({:.2} ms)   highlight {:7.1} MB/s ({:.2} ms, {} spans)",
            text.len(),
            doc.blocks.len(),
            mb / tp,
            tp * 1e3,
            mb / th,
            th * 1e3,
            spans.len()
        );
    }
}
