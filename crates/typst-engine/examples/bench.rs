//! Measurements for iron-sheets-typst.
//!
//!   cargo run -p iron-sheets-typst --example typst-bench            # everything
//!   cargo run -p iron-sheets-typst --example typst-bench -- cold [sys]   # one cold start (own process)
//!   cargo run --release -p iron-sheets-typst --example typst-bench -- latex  # LaTeX conversion + LaTeX-mode latency
//!
//! Outputs PNGs to target/agents/typst/out/ (or $BENCH_OUT).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use iron_sheets_typst::internals::{MathJob, Shared, render_batch, render_one};
use iron_sheets_typst::{Engine, EngineConfig, MathRequest, MathState, PageState, TypTok, highlight};

struct Waker {
    count: Mutex<u64>,
    cv: Condvar,
}

fn make_engine(system_fonts: bool) -> (Engine, Arc<Waker>) {
    let waker = Arc::new(Waker { count: Mutex::new(0), cv: Condvar::new() });
    let w = waker.clone();
    let engine = Engine::new(EngineConfig {
        system_fonts,
        notify: Arc::new(move || {
            *w.count.lock().unwrap() += 1;
            w.cv.notify_all();
        }),
    });
    (engine, waker)
}

/// Polls `f` until it returns Some, sleeping on the notify condvar.
fn wait_for<T>(waker: &Waker, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    let mut guard = waker.count.lock().unwrap();
    loop {
        drop(guard);
        if let Some(v) = f() {
            return Some(v);
        }
        guard = waker.count.lock().unwrap();
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        let seen = *guard;
        let (g, _) = waker
            .cv
            .wait_timeout_while(guard, (deadline - now).min(Duration::from_millis(2)), |c| *c == seen) // capped: a notify between `f()` and reading `seen` is lost
            .unwrap();
        guard = g;
    }
}

fn req(source: &str, display: bool) -> MathRequest<'_> {
    MathRequest { source, display, font_size_pt: 14.0, color: [0, 0, 0, 255], pixels_per_pt: 2.0, latex: false }
}

fn math_blocking(engine: &Engine, waker: &Waker, r: &MathRequest) -> MathState {
    wait_for(waker, Duration::from_secs(60), || match engine.math(r) {
        MathState::Pending => None,
        s => Some(s),
    })
    .unwrap_or(MathState::Pending)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn out_dir() -> PathBuf {
    let dir = std::env::var_os("BENCH_OUT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/agents/typst/out"));
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Composites premultiplied RGBA over a background and writes a PNG.
fn save_png(path: &Path, rgba: &[u8], w: u32, h: u32, bg: tiny_skia::Color, baseline_px: Option<u32>) {
    let Some(pm) = tiny_skia::PixmapRef::from_bytes(rgba, w, h) else { return };
    let Some(mut canvas) = tiny_skia::Pixmap::new(w, h) else { return };
    canvas.fill(bg);
    canvas.draw_pixmap(0, 0, pm, &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    if let Some(by) = baseline_px {
        let y = by.min(h - 1);
        for x in (0..w).step_by(2) {
            let i = ((y * w + x) * 4) as usize;
            canvas.data_mut()[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    canvas.save_png(path).unwrap();
}

const FORMULAS: [(&str, &str, bool); 3] =
    [("xi2", "x_i^2", false), ("sum", "sum_(k=0)^n k", false), ("integral", "integral_0^oo e^(-x^2) dif x = sqrt(pi) / 2", true)];

fn sample_doc(lines_per_section: usize) -> String {
    let mut s = String::from(
        "#set page(paper: \"a4\", margin: 2cm)\n#set text(size: 11pt)\n#set heading(numbering: \"1.\")\n#set par(justify: true)\n\n= Introduction\n",
    );
    for sec in 0..8 {
        s.push_str(&format!("\n== Section {sec}\n"));
        for i in 0..lines_per_section {
            s.push_str(&format!(
                "Paragraph {sec}.{i}: the quick brown fox jumps over the lazy dog, *strong text*, _emphasis_ and an inline formula $x_{i}^2 + y_{i}^2 = z_{i}^2$. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\n"
            ));
        }
        s.push_str(&format!("$ sum_(k=0)^n k = (n(n+1))/2 quad integral_0^{sec} x dif x $\n\n#table(columns: 3, [A], [B], [C], [1], [2], [{sec}])\n"));
    }
    s
}

fn cold(sys: bool, latex: bool) {
    let t0 = Instant::now();
    let (engine, waker) = make_engine(sys);
    let t_new = t0.elapsed();
    let src = if latex { "\\frac{x_i^2}{2}" } else { "frac(x_i^2, 2)" };
    let state = math_blocking(&engine, &waker, &MathRequest { latex, ..req(src, false) });
    let total = t0.elapsed();
    let ok = matches!(state, MathState::Ready(_));
    println!("COLD system_fonts={sys} new={:.3}ms first_math={:.1}ms ok={ok}", ms(t_new), ms(total));
}

fn run_cold() {
    let exe = std::env::current_exe().unwrap();
    for sys in [false, true] {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let out = Command::new(&exe).arg("cold").arg(if sys { "sys" } else { "nosys" }).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(v) = text.split("first_math=").nth(1).and_then(|s| s.split("ms").next()) {
                samples.push(v.parse::<f64>().unwrap_or(f64::NAN));
            }
        }
        println!("cold first math image (Engine::new -> Ready), system_fonts={sys}: median {:.1} ms (samples {:?})", median(&mut samples.clone()), samples);
    }
}

/// LaTeX mode: conversion cost per formula, cold first math (own process) and warm latency, vs Typst mode.
fn latex_bench() {
    use iron_sheets_typst::latex_to_typst;
    let formulas: Vec<String> = (0..300)
        .map(|i| match i % 5 {
            0 => format!("\\frac{{a_{i}}}{{b^{{{i}}}}} + \\sqrt{{x^2 + y_{i}^2}}"),
            1 => format!("\\sum_{{k=1}}^{{{i}}} \\alpha_k x^k \\leq \\int_0^\\infty e^{{-x}}\\,dx"),
            2 => format!("\\left( \\begin{{pmatrix}} a & {i} \\\\ c & d \\end{{pmatrix}} \\right) \\mathbb{{R}} \\cdot \\mathbf{{v}}"),
            3 => format!("\\begin{{aligned}} a &= b + {i} \\\\ &= \\text{{done}} \\end{{aligned}}"),
            _ => format!("x_{{i,j}}^{{{i}}} \\times \\binom{{n}}{{k}} \\neq \\partial_t u"),
        })
        .collect();
    let mut per = Vec::new();
    for round in 0..20 {
        let t = Instant::now();
        let mut n = 0usize;
        for f in &formulas {
            n += latex_to_typst(f).map(|s| s.len()).unwrap_or(0);
        }
        let _ = (round, n);
        per.push(t.elapsed().as_secs_f64() * 1e6 / formulas.len() as f64);
    }
    println!(
        "latex_to_typst: median {:.2} us per formula ({} distinct formulas, 20 rounds, min {:.2})",
        median(&mut per.clone()),
        formulas.len(),
        per.iter().cloned().fold(f64::MAX, f64::min)
    );
    let exe = std::env::current_exe().unwrap();
    for latex in [false, true] {
        let mut samples = Vec::new();
        for _ in 0..8 {
            let out = Command::new(&exe).arg("cold").arg("nosys").arg(if latex { "latex" } else { "typst" }).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(v) = text.split("first_math=").nth(1).and_then(|s| s.split("ms").next()) {
                samples.push(v.parse::<f64>().unwrap_or(f64::NAN));
            }
        }
        println!(
            "cold first math (Engine::new -> Ready), {} mode: median {:.1} ms (samples {:?})",
            if latex { "LaTeX" } else { "Typst" },
            median(&mut samples.clone()),
            samples
        );
    }
    let (engine, waker) = make_engine(false);
    math_blocking(&engine, &waker, &req("a+b", false));
    for latex in [false, true] {
        let mut v = Vec::new();
        for f in formulas.iter().take(60) {
            let src = if latex { f.clone() } else { latex_to_typst(f).unwrap_or_default() };
            let t = Instant::now();
            math_blocking(&engine, &waker, &MathRequest { latex, ..req(&src, false) });
            v.push(ms(t.elapsed()));
        }
        println!(
            "warm single formula, {} mode (source given as {}): median {:.2} ms",
            if latex { "LaTeX" } else { "Typst" },
            if latex { "LaTeX" } else { "pre-converted Typst" },
            median(&mut v)
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("cold") {
        cold(args.get(2).map(String::as_str) == Some("sys"), args.get(3).map(String::as_str) == Some("latex"));
        return;
    }
    if args.get(1).map(String::as_str) == Some("latex") {
        latex_bench();
        return;
    }
    let out = out_dir();
    println!("== cold start (fresh process each, 5 runs)");
    run_cold();

    let (engine, waker) = make_engine(false);
    // Warm everything up.
    math_blocking(&engine, &waker, &req("a+b", false));

    println!("\n== warm single formula (Engine::math request -> Ready), distinct formulas, median of 30");
    let mut v = Vec::new();
    for i in 0..30 {
        let src = format!("x_{i}^2 + sqrt({i}) = frac(a_{i}, b)");
        let t = Instant::now();
        math_blocking(&engine, &waker, &req(&src, false));
        v.push(ms(t.elapsed()));
    }
    println!(
        "inline: median {:.2} ms, min {:.2}, max {:.2}",
        median(&mut v.clone()),
        v.iter().cloned().fold(f64::MAX, f64::min),
        v.iter().cloned().fold(0.0, f64::max)
    );
    let t = Instant::now();
    let _ = engine.math(&req("x_1^2 + sqrt(1) = frac(a_1, b)", false));
    println!("cache hit lookup: {:.1} us", t.elapsed().as_secs_f64() * 1e6);

    {
        let shared = Shared::new();
        let t = Instant::now();
        shared.install_system_fonts(Shared::scan_system_fonts());
        println!(
            "\nsystem font scan (background thread in the engine, not on the first-math path): {:.0} ms, {} font families total",
            ms(t.elapsed()),
            shared.fonts().store.book().families().count()
        );
    }

    println!("\n== 200 distinct formulas: strategies (in-process, shared state warm)");
    let shared = Arc::new(Shared::new());
    let mk = |i: usize| MathJob {
        source: format!("sum_(k=0)^{i} k^2 + frac({i}, k+1) - alpha_{i} beta^2"),
        display: i.is_multiple_of(5),
        font_size_pt: 14.0,
        color: [0, 0, 0, 255],
        pixels_per_pt: 2.0,
        latex: false,
    };
    // warm
    let _ = render_one(&shared, &mk(9999), 0);
    let jobs: Vec<(MathJob, u64)> = (0..200).map(|i| (mk(i), i as u64)).collect();
    let t = Instant::now();
    for (j, k) in &jobs {
        let _ = render_one(&shared, j, *k);
    }
    println!("one compile per formula, 1 thread:   {:.0} ms", ms(t.elapsed()));
    for batch in [5usize, 10, 25, 50, 200] {
        let t = Instant::now();
        for chunk in jobs.chunks(batch) {
            let _ = render_batch(&shared, chunk);
        }
        println!("batched {batch:>3} per doc, 1 thread:      {:.0} ms", ms(t.elapsed()));
    }
    for threads in [2usize, 3, 4, 6] {
        let jobs = Arc::new(jobs.clone());
        let next = Arc::new(AtomicU64::new(0));
        let t = Instant::now();
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let (jobs, next, shared) = (jobs.clone(), next.clone(), shared.clone());
                std::thread::spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst) as usize;
                        if i >= jobs.len() {
                            break;
                        }
                        let _ = render_one(&shared, &jobs[i].0, jobs[i].1);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        println!("one compile per formula, {threads} threads: {:.0} ms", ms(t.elapsed()));
    }
    for (threads, batch) in [(3usize, 10usize), (3, 25), (4, 10), (4, 25)] {
        let chunks: Arc<Vec<Vec<(MathJob, u64)>>> = Arc::new(jobs.chunks(batch).map(|c| c.to_vec()).collect());
        let next = Arc::new(AtomicU64::new(0));
        let t = Instant::now();
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let (chunks, next, shared) = (chunks.clone(), next.clone(), shared.clone());
                std::thread::spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst) as usize;
                        if i >= chunks.len() {
                            break;
                        }
                        let _ = render_batch(&shared, &chunks[i]);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        println!("batched {batch:>2}, {threads} threads:            {:.0} ms", ms(t.elapsed()));
    }

    // End to end through the engine (whatever strategy it uses).
    {
        let (engine, waker) = make_engine(false);
        math_blocking(&engine, &waker, &req("a+b", false));
        let t = Instant::now();
        let srcs: Vec<String> = (0..200).map(|i| mk(1000 + i).source).collect();
        (0..200usize).for_each(|i| {
            engine.math(&MathRequest { display: i.is_multiple_of(5), ..req(&srcs[i], false) });
        });
        let done = wait_for(&waker, Duration::from_secs(120), || {
            let all = (0..200usize).all(|i| !matches!(engine.math(&MathRequest { display: i.is_multiple_of(5), ..req(&srcs[i], false) }), MathState::Pending));
            all.then_some(())
        });
        println!("Engine::math, 200 distinct formulas end-to-end: {:.0} ms (done={})", ms(t.elapsed()), done.is_some());
        // error isolation
        match math_blocking(&engine, &waker, &req("x_^ )(", false)) {
            MathState::Error(e) => println!("invalid formula -> Error({:?})", e.lines().next().unwrap_or("")),
            other => println!("invalid formula -> {other:?}"),
        }
    }

    println!("\n== formula PNGs -> {}", out.display());
    for (dark, color, bg) in
        [("light", [235u8, 235, 235, 255], tiny_skia::Color::from_rgba8(30, 30, 34, 255)), ("dark", [20u8, 20, 20, 255], tiny_skia::Color::WHITE)]
    {
        for (name, src, display) in FORMULAS {
            let r = MathRequest { color, pixels_per_pt: 4.0, ..req(src, display) };
            if let MathState::Ready(img) = math_blocking(&engine, &waker, &r) {
                save_png(&out.join(format!("math_{name}_{dark}text.png")), &img.rgba, img.width, img.height, bg, Some((img.baseline_pt * 4.0).round() as u32));
                println!("{name} ({dark} text): {}x{} px, size_pt {:?}, baseline_pt {:.2}", img.width, img.height, img.size_pt, img.baseline_pt);
            }
        }
    }

    println!("\n== documents");
    let (engine, waker) = make_engine(false);
    let text = sample_doc(3);
    let t = Instant::now();
    engine.set_document(1, None, None, text.clone(), 1);
    let snap = wait_for(&waker, Duration::from_secs(60), || engine.document(1).filter(|s| s.version == 1)).unwrap();
    println!(
        "cold compile (includes lazy init): {:.0} ms wall, compile {:.0} ms, ok={} pages={} anchors={} diags={}",
        ms(t.elapsed()),
        snap.compile_ms,
        snap.ok,
        snap.pages.len(),
        snap.anchors.len(),
        snap.diagnostics.len()
    );
    let t = Instant::now();
    engine.set_document(2, None, None, text.clone(), 1);
    let snap = wait_for(&waker, Duration::from_secs(60), || engine.document(2)).unwrap();
    println!("second document, same text (warm lib, comemo cached): {:.0} ms wall, compile {:.0} ms", ms(t.elapsed()), snap.compile_ms);
    let mut v = Vec::new();
    let mut walls = Vec::new();
    for ver in 2..22u64 {
        let mut edited = text.clone();
        // insert one character in the middle of the first paragraph block
        let at = edited.find("Paragraph 3.2").unwrap() + 11;
        edited.insert(at, if ver % 2 == 0 { 'X' } else { 'Y' });
        let t = Instant::now();
        engine.set_document(1, None, None, edited, ver);
        let snap = wait_for(&waker, Duration::from_secs(60), || engine.document(1).filter(|s| s.version == ver)).unwrap();
        walls.push(ms(t.elapsed()));
        v.push(snap.compile_ms as f64);
    }
    println!(
        "one-character edit recompile: compile median {:.0} ms (min {:.0}, max {:.0}); wall median {:.0} ms",
        median(&mut v.clone()),
        v.iter().cloned().fold(f64::MAX, f64::min),
        v.iter().cloned().fold(0.0, f64::max),
        median(&mut walls)
    );

    // page render
    let mut v = Vec::new();
    for page in 0..3 {
        let t = Instant::now();
        let img = wait_for(&waker, Duration::from_secs(60), || match engine.page(1, page, 2.0) {
            PageState::Ready(i) if i.version == 21 => Some(i),
            _ => None,
        });
        if let Some(img) = img {
            v.push(ms(t.elapsed()));
            if page == 0 {
                save_png(&out.join("doc_page1.png"), &img.rgba, img.width, img.height, tiny_skia::Color::WHITE, None);
                println!("page 1 PNG: {}x{} px", img.width, img.height);
            }
        }
    }
    println!("page render at 2.0 px/pt (request -> Ready, pages 1..3): {:?} ms", v.iter().map(|x| x.round()).collect::<Vec<_>>());
    // anchors check
    let snap = engine.document(1).unwrap();
    let sorted = snap.anchors.windows(2).all(|w| w[0].line < w[1].line && (w[0].page, w[0].y_pt) <= (w[1].page, w[1].y_pt));
    println!("anchors: {} entries, sorted & monotonic = {sorted}; first {:?}, last {:?}", snap.anchors.len(), snap.anchors.first(), snap.anchors.last());
    // pdf
    #[cfg(feature = "pdf")]
    {
        let path = out.join("doc.pdf");
        let t = Instant::now();
        engine.export_pdf(1, path.clone());
        let r = wait_for(&waker, Duration::from_secs(60), || engine.take_export_result(1));
        println!("PDF export: {:?} in {:.0} ms", r, ms(t.elapsed()));
    }

    println!("\n== highlight, 100 KB");
    let mut big = String::new();
    while big.len() < 100 * 1024 {
        big.push_str(&text);
    }
    big.truncate(100 * 1024);
    while !big.is_char_boundary(big.len()) {
        big.pop();
    }
    let mut toks = Vec::new();
    highlight(&big, &mut toks);
    let t = Instant::now();
    let n = 20;
    for _ in 0..n {
        highlight(&big, &mut toks);
    }
    let per = ms(t.elapsed()) / n as f64;
    let kinds: std::collections::BTreeSet<String> = toks.iter().map(|(_, k)| format!("{k:?}")).collect();
    println!("{} bytes: {:.2} ms per run = {:.0} MB/s, {} tokens, kinds {:?}", big.len(), per, big.len() as f64 / 1e6 / (per / 1000.0), toks.len(), kinds);
    let _ = TypTok::Text;
    println!("\nsymbols: {}", iron_sheets_typst::symbols().len());
    println!("\n== typst_assets::fonts() (index: family, style, weight)");
    for (i, info) in Shared::new().embedded_font_infos().iter().enumerate() {
        println!("{i:>2}: {} | {:?} | weight {} | stretch {:?}", info.family, info.variant.style, info.variant.weight.to_number(), info.variant.stretch);
    }
}
