use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use iron_sheets_md::{IncrementalHighlighter, WindowHighlighter, highlight, parse};
use std::time::Duration;

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

fn bench_md(c: &mut Criterion) {
    let text_a = corpus(96 * 1024);
    let mid = text_a[..text_a.len() / 2].rfind("Lorem ipsum").map_or(text_a.len() / 2, |p| p + 5);
    let mut text_b = text_a.clone();
    text_b.insert(mid, 'x');

    let mut parse_group = c.benchmark_group("md/parse");
    parse_group.sample_size(30);
    parse_group.warm_up_time(Duration::from_millis(500));
    parse_group.measurement_time(Duration::from_secs(2));
    parse_group.throughput(Throughput::Bytes(text_a.len() as u64));
    parse_group.bench_function("parse_96kb_gfm", |b| b.iter(|| black_box(parse(black_box(&text_a)))));
    parse_group.finish();

    let mut hl_group = c.benchmark_group("md/highlight");
    hl_group.warm_up_time(Duration::from_millis(500));
    hl_group.measurement_time(Duration::from_secs(2));
    hl_group.throughput(Throughput::Bytes(text_a.len() as u64));
    let mut spans = Vec::new();
    hl_group.bench_function("full_96kb", |b| {
        b.iter(|| {
            highlight(black_box(&text_a), &mut spans);
            black_box(spans.len())
        })
    });

    let mut inc = IncrementalHighlighter::new();
    inc.update(&text_a);
    let mut flip = true;
    hl_group.bench_function("incremental_keystroke_96kb", |b| {
        b.iter(|| {
            let next = if flip { &text_b } else { &text_a };
            flip = !flip;
            inc.update(black_box(next));
            black_box(inc.tokens().len())
        })
    });

    let mut win = WindowHighlighter::new();
    win.update(None, &text_a);
    let mut wflip = true;
    hl_group.bench_function("window_keystroke_96kb", |b| {
        b.iter(|| {
            let (old, new) = if wflip { (&text_a, &text_b) } else { (&text_b, &text_a) };
            wflip = !wflip;
            black_box(win.update(Some(old.as_str()), black_box(new)))
        })
    });
    hl_group.finish();

    win.update(None, &text_a);
    let ls = text_a[..mid].rfind('\n').map_or(0, |i| i + 1);
    let le = text_a[mid..].find('\n').map_or(text_a.len(), |i| mid + i);
    let mut toks = Vec::new();
    let mut para_group = c.benchmark_group("md/highlight_window");
    para_group.warm_up_time(Duration::from_millis(500));
    para_group.measurement_time(Duration::from_secs(2));
    para_group.throughput(Throughput::Bytes((le - ls) as u64));
    para_group.bench_function("tokens_paragraph", |b| {
        b.iter(|| {
            win.tokens(black_box(&text_a), ls, le, &mut toks);
            black_box(toks.len())
        })
    });
    para_group.finish();
}

criterion_group!(benches, bench_md);
criterion_main!(benches);
