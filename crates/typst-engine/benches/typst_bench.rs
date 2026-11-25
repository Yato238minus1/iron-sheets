use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use iron_sheets_typst::{IncrementalTypstHighlighter, highlight};
use std::time::Duration;

const CHUNK: &str = "= Heading {n}\n\nSome *strong* and _emph_ text with `raw` and a #link(\"https://x.org\")[link], a label <lab-{n}> and a ref @lab-{n}.\n\n- item one\n- item two with $a^2 + b^2 = c^2$\n+ numbered\n\n#let f(x, y: 2) = {\n  let z = x + y * 3\n  if z > 4 { [big #z] } else { [small] }\n}\n\n#f(3, y: 5)\n\n$ integral_0^oo e^(-x^2) dif x = sqrt(pi)/2 $\n\n```rust\nfn main() { println!(\"{}\", {n}); }\n```\n\n/ Term: a description list item\n\n// a comment\n/* block\ncomment */\n\nPlain paragraph text, long enough to be realistic: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor {n}.\n\n";

fn corpus(size: usize) -> String {
    let mut s = String::with_capacity(size + 1024);
    let mut n = 0usize;
    while s.len() < size {
        s.push_str(&CHUNK.replace("{n}", &n.to_string()));
        n += 1;
    }
    s
}

fn bench_typst_highlight(c: &mut Criterion) {
    let text_a = corpus(100_000);
    let mid = text_a[..text_a.len() / 2].rfind("lorem ipsum dolor sit amet").map_or(text_a.len() / 2, |p| p + "lorem ipsum dolor sit amet".len());
    let mut text_b = text_a.clone();
    text_b.insert(mid, 'x');

    let mut group = c.benchmark_group("typst/highlight");
    group.sample_size(30);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.throughput(Throughput::Bytes(text_a.len() as u64));

    let mut out = Vec::new();
    group.bench_function("full_100kb", |b| {
        b.iter(|| {
            highlight(black_box(&text_a), &mut out);
            black_box(out.len())
        })
    });

    let mut inc = IncrementalTypstHighlighter::new();
    inc.update(&text_a);
    let mut flip = true;
    group.bench_function("incremental_keystroke_100kb", |b| {
        b.iter(|| {
            let next = if flip { &text_b } else { &text_a };
            flip = !flip;
            inc.update(black_box(next));
            black_box(inc.tokens().len())
        })
    });

    group.finish();
}

criterion_group!(benches, bench_typst_highlight);
criterion_main!(benches);
