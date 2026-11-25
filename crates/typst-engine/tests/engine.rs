use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use iron_sheets_typst::{DocSnapshot, Engine, EngineConfig, MathRequest, MathState, PageState, TypTok, highlight, symbols};

struct Harness {
    engine: Engine,
    count: Arc<(Mutex<u64>, Condvar)>,
    notified: Arc<AtomicUsize>,
}

fn harness(system_fonts: bool) -> Harness {
    let count = Arc::new((Mutex::new(0u64), Condvar::new()));
    let notified = Arc::new(AtomicUsize::new(0));
    let (c, n) = (count.clone(), notified.clone());
    let engine = Engine::new(EngineConfig {
        system_fonts,
        notify: Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            *c.0.lock().unwrap() += 1;
            c.1.notify_all();
        }),
    });
    Harness { engine, count, notified }
}

impl Harness {
    fn wait<T>(&self, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for the engine");
            let guard = self.count.0.lock().unwrap();
            let seen = *guard;
            let _ = self.count.1.wait_timeout_while(guard, Duration::from_millis(200), |c| *c == seen).unwrap();
        }
    }

    fn math(&self, source: &str, display: bool) -> MathState {
        self.wait(|| match self.engine.math(&MathRequest { source, display, font_size_pt: 14.0, color: [0, 0, 0, 255], pixels_per_pt: 2.0, latex: false }) {
            MathState::Pending => None,
            s => Some(s),
        })
    }

    fn doc(&self, id: u64, version: u64) -> Arc<DocSnapshot> {
        self.wait(|| self.engine.document(id).filter(|s| s.version >= version))
    }
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iron-sheets-typst-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn engine_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Engine>();
    check::<Arc<Engine>>();
}

#[test]
fn engine_new_does_not_start_anything() {
    let h = harness(true);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(h.notified.load(Ordering::SeqCst), 0);
}

#[test]
fn math_valid_is_ready() {
    let h = harness(false);
    let MathState::Ready(img) = h.math("x_i^2", false) else { panic!("expected Ready") };
    assert!(img.width > 0 && img.height > 0);
    assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    assert!(img.rgba.chunks(4).any(|p| p[3] != 0), "image must contain ink");
    assert!(img.rgba.chunks(4).any(|p| p[3] == 0), "background must be transparent");
    // 2 px per pt.
    assert!((img.size_pt[0] * 2.0 - img.width as f32).abs() <= 1.0);
    assert!((img.size_pt[1] * 2.0 - img.height as f32).abs() <= 1.0);
}

#[test]
fn math_baseline_is_inside_image_and_below_the_top() {
    let h = harness(false);
    for (src, display) in [("x_i^2", false), ("sum_(k=0)^n k", false), ("sum_(k=0)^n k", true), ("a", false)] {
        let MathState::Ready(img) = h.math(src, display) else { panic!("{src}: expected Ready") };
        assert!(img.baseline_pt > 0.0, "{src}: baseline {}", img.baseline_pt);
        assert!(img.baseline_pt <= img.size_pt[1] + 0.01, "{src}: baseline {} height {}", img.baseline_pt, img.size_pt[1]);
    }
    // A subscript hangs below the baseline, so the baseline is above the bottom.
    let MathState::Ready(img) = h.math("x_i", false) else { panic!() };
    assert!(img.baseline_pt < img.size_pt[1] - 1.0);
}

#[test]
fn math_color_is_applied() {
    let h = harness(false);
    let state = h.wait(|| {
        match h.engine.math(&MathRequest { source: "x", display: false, font_size_pt: 20.0, color: [255, 0, 0, 255], pixels_per_pt: 2.0, latex: false }) {
            MathState::Pending => None,
            s => Some(s),
        }
    });
    let MathState::Ready(img) = state else { panic!() };
    let opaque = img.rgba.chunks(4).filter(|p| p[3] == 255).collect::<Vec<_>>();
    assert!(!opaque.is_empty());
    assert!(opaque.iter().all(|p| p[0] == 255 && p[1] == 0 && p[2] == 0));
}

#[test]
fn math_invalid_is_error_with_message() {
    let h = harness(false);
    match h.math("x_", false) {
        MathState::Error(msg) => assert!(!msg.is_empty()),
        other => panic!("expected Error, got {other:?}"),
    }
    match h.math("#unknown_function(1)", false) {
        MathState::Error(msg) => assert!(!msg.is_empty()),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn math_cannot_read_files() {
    let h = harness(false);
    match h.math("#read(\"/etc/passwd\")", false) {
        MathState::Error(_) => {}
        other => panic!("file access must be denied, got {other:?}"),
    }
}

#[test]
fn math_is_cached() {
    let h = harness(false);
    let MathState::Ready(a) = h.math("alpha + beta", true) else { panic!() };
    let before = h.notified.load(Ordering::SeqCst);
    let MathState::Ready(b) = h.math("alpha + beta", true) else { panic!() };
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.key, b.key);
    assert_eq!(before, h.notified.load(Ordering::SeqCst));
}

#[test]
fn math_many_formulas_with_errors_isolated() {
    let h = harness(false);
    let sources: Vec<String> = (0..60).map(|i| if i % 10 == 3 { format!("x_{i} + #undefined_{i}") } else { format!("x_{i}^2 + y") }).collect();
    for (i, s) in sources.iter().enumerate() {
        let state = h.math(s, i % 2 == 0);
        if i % 10 == 3 {
            assert!(matches!(state, MathState::Error(_)), "{i}");
        } else {
            assert!(matches!(state, MathState::Ready(_)), "{i}");
        }
    }
}

const DOC: &str = "#set page(width: 200pt, height: 150pt, margin: 10pt)\n= Title\n\nHello *world* $x^2$.\n\n#pagebreak()\n\nSecond page.\n";

#[test]
fn document_snapshot_has_pages() {
    let h = harness(false);
    h.engine.set_document(7, None, None, DOC.to_string(), 1);
    let snap = h.doc(7, 1);
    assert!(snap.ok, "{:?}", snap.diagnostics);
    assert_eq!(snap.version, 1);
    assert_eq!(snap.pages.len(), 2);
    assert!((snap.pages[0][0] - 200.0).abs() < 0.01 && (snap.pages[0][1] - 150.0).abs() < 0.01);
    assert!(snap.compile_ms > 0.0);
    assert!(!snap.anchors.is_empty());
}

#[test]
fn document_error_has_correct_line_and_keeps_last_good_pages() {
    let h = harness(false);
    h.engine.set_document(1, None, None, DOC.to_string(), 1);
    assert!(h.doc(1, 1).ok);
    let bad = "#set page(width: 200pt, height: 150pt)\nfine\n\n#let x = 1 +\n= Heading\n#unknown-fn()\n";
    h.engine.set_document(1, None, None, bad.to_string(), 2);
    let snap = h.doc(1, 2);
    assert!(!snap.ok);
    let err = snap.diagnostics.iter().find(|d| d.error).expect("an error");
    assert!(err.file.is_none());
    assert!(!err.message.is_empty());
    // The first problem is on line 3 (0-based) or the next one: `#let x = 1 +`.
    assert!(err.line == 3 || err.line == 4, "line {}", err.line);
    // Pages come from the last good compile.
    assert_eq!(snap.pages.len(), 2);

    let unknown = "line 0\nline 1\n#nope()\n";
    h.engine.set_document(2, None, None, unknown.to_string(), 1);
    let snap = h.doc(2, 1);
    let err = snap.diagnostics.iter().find(|d| d.error).unwrap();
    assert_eq!(err.line, 2);
    assert_eq!(err.column, 1);
    assert!(snap.pages.is_empty());
}

#[test]
fn document_warnings_are_reported() {
    let h = harness(false);
    h.engine.set_document(1, None, None, "#set text(font: \"Definitely Not A Font\")\nhi".to_string(), 1);
    let snap = h.doc(1, 1);
    assert!(snap.ok);
    assert!(snap.diagnostics.iter().any(|d| !d.error), "{:?}", snap.diagnostics);
}

#[test]
fn anchors_are_sorted_and_monotonic() {
    let h = harness(false);
    let mut text = String::from("#set page(width: 300pt, height: 200pt, margin: 12pt)\n");
    for i in 0..60 {
        text.push_str(&format!("Line number {i} of the text with some words.\n\n"));
    }
    h.engine.set_document(1, None, None, text, 1);
    let snap = h.doc(1, 1);
    assert!(snap.ok);
    assert!(snap.pages.len() >= 3, "pages {}", snap.pages.len());
    assert!(snap.anchors.len() > 30);
    for w in snap.anchors.windows(2) {
        assert!(w[0].line < w[1].line, "{:?}", w);
        assert!((w[0].page, w[0].y_pt) <= (w[1].page, w[1].y_pt), "{:?}", w);
    }
    let last = snap.anchors.last().unwrap();
    assert!(last.page as usize == snap.pages.len() - 1);
    assert!(snap.anchors.iter().all(|a| a.y_pt >= 0.0 && a.y_pt <= snap.pages[a.page as usize][1]));
}

#[test]
fn latest_wins_under_rapid_set_document() {
    let h = harness(false);
    // A document that takes a little while to compile.
    let make = |n: u64| {
        let mut s = String::from("#set page(width: 300pt, height: 200pt)\n");
        for i in 0..150 {
            s.push_str(&format!("Paragraph {i} version {n}: $sum_(k=0)^{i} k^2 = frac({i}, 2)$ and more text to lay out here.\n\n"));
        }
        s
    };
    h.engine.set_document(1, None, None, make(0), 0);
    h.doc(1, 0);
    let total = 100u64;
    for v in 1..=total {
        h.engine.set_document(1, None, None, make(v), v);
    }
    let mut seen = std::collections::BTreeSet::new();
    let last = h.wait(|| {
        let s = h.engine.document(1).unwrap();
        seen.insert(s.version);
        (s.version == total).then_some(s)
    });
    assert_eq!(last.version, total);
    assert!(seen.len() < 20, "compiled too many stale versions: {seen:?}");
}

#[test]
fn page_rendering_and_stale_while_revalidate() {
    let h = harness(false);
    h.engine.set_document(1, None, None, DOC.to_string(), 1);
    h.doc(1, 1);
    assert!(matches!(h.engine.page(1, 5, 2.0), PageState::Pending), "out of range page");
    let img = h.wait(|| match h.engine.page(1, 0, 2.0) {
        PageState::Ready(i) => Some(i),
        PageState::Pending => None,
    });
    assert_eq!((img.width, img.height), (400, 300));
    assert_eq!(img.version, 1);
    assert_eq!(img.page, 0);
    // Opaque white page background.
    assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255]);
    assert!(img.rgba.chunks(4).any(|p| p[0] < 128), "text ink present");

    // A new version: until it is rendered, the old image keeps being returned.
    h.engine.set_document(1, None, None, DOC.replace("Hello", "Howdy"), 2);
    h.doc(1, 2);
    match h.engine.page(1, 0, 2.0) {
        PageState::Ready(i) => assert!(i.version == 1 || i.version == 2),
        PageState::Pending => panic!("must not flicker to Pending"),
    }
    let fresh = h.wait(|| match h.engine.page(1, 0, 2.0) {
        PageState::Ready(i) if i.version == 2 => Some(i),
        _ => None,
    });
    assert_ne!(fresh.rgba, img.rgba);
    h.engine.close_document(1);
    assert!(h.engine.document(1).is_none());
    assert!(matches!(h.engine.page(1, 0, 2.0), PageState::Pending));
}

#[test]
fn click_jumps_to_source_line() {
    let h = harness(false);
    let text = "#set page(width: 200pt, height: 100pt, margin: 10pt)\nfirst line\n\nsecond paragraph here\n";
    h.engine.set_document(1, None, None, text.to_string(), 1);
    h.doc(1, 1);
    // Find the second paragraph through the anchors.
    let snap = h.engine.document(1).unwrap();
    let a = snap.anchors.iter().find(|a| a.line == 3).expect("anchor for line 3");
    h.engine.jump_from_click(1, a.page, 20.0, a.y_pt + 5.0);
    let (line, _col) = h.wait(|| h.engine.take_jump(1));
    assert_eq!(line, 3);
    assert!(h.engine.take_jump(1).is_none());
}

#[cfg(feature = "pdf")]
#[test]
fn pdf_export() {
    let h = harness(false);
    let dir = tmp_dir("pdf");
    // Nothing compiled yet -> error.
    h.engine.export_pdf(9, dir.join("none.pdf"));
    assert!(h.wait(|| h.engine.take_export_result(9)).is_err());
    h.engine.set_document(1, None, None, DOC.to_string(), 1);
    h.doc(1, 1);
    let out = dir.join("out.pdf");
    h.engine.export_pdf(1, out.clone());
    let result = h.wait(|| h.engine.take_export_result(1));
    assert_eq!(result.unwrap(), out);
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
}

#[cfg(not(feature = "packages"))]
#[test]
fn missing_preview_package_is_a_diagnostic_without_downloader() {
    let h = harness(false);
    h.engine.set_document(1, None, None, "#import \"@preview/no-such-pkg:9.9.9\": x".to_string(), 1);
    let snap = h.doc(1, 1);
    assert!(!snap.ok);
    assert!(snap.diagnostics.iter().any(|d| d.message.contains("not compiled in")), "{:?}", snap.diagnostics);
}

#[test]
fn project_files_are_read_and_reread_when_changed() {
    let h = harness(false);
    let dir = tmp_dir("files");
    std::fs::write(dir.join("part.typ"), "Included text ONE").unwrap();
    let main = dir.join("main.typ");
    let text = "#set page(width: 200pt, height: 100pt)\n#include \"part.typ\"\n";
    h.engine.set_document(1, Some(main.clone()), Some(dir.clone()), text.to_string(), 1);
    let snap = h.doc(1, 1);
    assert!(snap.ok, "{:?}", snap.diagnostics);
    let first = h.wait(|| match h.engine.page(1, 0, 1.0) {
        PageState::Ready(i) => Some(i),
        _ => None,
    });
    // Change the included file (different length so the stamp differs even
    // on coarse timestamps).
    std::fs::write(dir.join("part.typ"), "Included text TWO, now much longer than before").unwrap();
    h.engine.set_document(1, Some(main.clone()), Some(dir.clone()), text.to_string(), 2);
    h.doc(1, 2);
    let second = h.wait(|| match h.engine.page(1, 0, 1.0) {
        PageState::Ready(i) if i.version == 2 => Some(i),
        _ => None,
    });
    assert_ne!(first.rgba, second.rgba);

    // A missing file is an error located at the include, naming the file.
    std::fs::remove_file(dir.join("part.typ")).unwrap();
    h.engine.set_document(1, Some(main), Some(dir.clone()), text.to_string(), 3);
    let snap = h.doc(1, 3);
    assert!(!snap.ok);
    let err = snap.diagnostics.iter().find(|d| d.error).unwrap();
    assert_eq!(err.line, 1);
    assert!(err.message.contains("file not found") || err.message.contains("part.typ"), "{}", err.message);
}

#[test]
fn errors_in_other_files_carry_the_file() {
    let h = harness(false);
    let dir = tmp_dir("otherfile");
    std::fs::write(dir.join("bad.typ"), "ok\n#nope()\n").unwrap();
    let main = dir.join("main.typ");
    h.engine.set_document(1, Some(main), Some(dir.clone()), "#include \"bad.typ\"".to_string(), 1);
    let snap = h.doc(1, 1);
    let err = snap.diagnostics.iter().find(|d| d.error).unwrap();
    assert_eq!(err.file.as_deref(), Some(dir.join("bad.typ").as_path()));
    assert_eq!(err.line, 1);
}

#[test]
fn works_with_system_fonts_enabled() {
    let h = harness(true);
    h.engine.set_document(1, None, None, DOC.to_string(), 1);
    assert!(h.doc(1, 1).ok);
    assert!(matches!(h.math("a + b", false), MathState::Ready(_)));
}

#[test]
fn symbols_table() {
    let syms = symbols();
    assert!(syms.len() > 500, "{}", syms.len());
    for name in ["alpha", "arrow.r", "sum", "arrow.r.long", "eq.not"] {
        assert!(syms.iter().any(|s| s.name == name), "missing {name}");
    }
    let alpha = syms.iter().find(|s| s.name == "alpha").unwrap();
    assert_eq!(alpha.ch, Some('α'));
    assert_eq!(alpha.value, "α");
    let sum = syms.iter().find(|s| s.name == "sum").unwrap();
    assert_eq!(sum.ch, Some('∑'));
    // Unique names.
    let mut names: Vec<_> = syms.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    let n = names.len();
    names.dedup();
    assert_eq!(n, names.len(), "duplicate symbol names");
}

fn check_coverage(text: &str) {
    let mut out = vec![(0..0, TypTok::Text)];
    highlight(text, &mut out);
    let mut pos = 0;
    for (range, _) in &out {
        assert_eq!(range.start, pos, "gap or overlap in {text:?}");
        assert!(range.end > range.start, "empty range in {text:?}");
        assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
        pos = range.end;
    }
    assert_eq!(pos, text.len(), "not covering {text:?}");
}

#[test]
fn highlight_covers_text_exactly() {
    for text in [
        "",
        "plain",
        "= Heading\n*strong* _emph_ `raw` <label> @ref https://x.y\n",
        "#let f(x) = x + 1 // comment\n#f(2) /* block */ \"str\" 12pt\n",
        "$ sum_(i=0)^n x_i dot \"text\" = alpha $ and $x$\n",
        "- item\n+ num\n/ Term: desc\n",
        "unterminated $ math and #( paren and \"string",
        "ünïcödé 日本語 émoji 😀 *bold 😀* \\u{1F600}\r\nCRLF\r\n",
        "```rust\nfn main() {}\n```",
    ] {
        check_coverage(text);
    }
    let mut out = Vec::new();
    highlight("= Hi *x*", &mut out);
    assert!(out.iter().any(|(_, t)| *t == TypTok::Heading));
    assert!(out.iter().any(|(_, t)| *t == TypTok::Strong));
    highlight("#let x = 1", &mut out);
    assert!(out.iter().any(|(_, t)| *t == TypTok::Keyword));
    assert!(out.iter().any(|(_, t)| *t == TypTok::Number));
    // The output vec is cleared first.
    highlight("", &mut out);
    assert!(out.is_empty());
}
