use crate::common::*;
use egui::{Event, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SHOWCASE: &str = "---\ntitle: Showcase\n---\n\n# Heading one\n\n## Heading two\n\n### Heading three\n\nA paragraph with **bold**, *italic*, ~~strike~~, `inline code`, ==highlight==, H~2~O, x^2^, <kbd>Ctrl</kbd>, #tag and a [web link](https://example.org) plus [[Ideas]] and [[Missing Note]] and a footnote[^1].\nSecond line of the same paragraph, soft broken.\n\n- bullet one\n- bullet two\n  - nested bullet\n    - deeper\n\n1. first\n2. second\n\n- [ ] open task\n- [x] done task\n\n> A quote\n>\n> spanning two paragraphs\n\n> [!NOTE]\n> A GitHub alert\n\n> [!WARNING] Custom title\n> Careful now\n\n```rust\nfn main() {\n    println!(\"a very long line of code that is certainly wider than half of the preview pane so a scroll bar appears\");\n}\n```\n\n| Left | Center | Right |\n|:-----|:------:|------:|\n| a | b | c |\n| longer cell | [[Plan]] | 3 |\n\n---\n\nInline math $a^2 + b^2 = c^2$ in a sentence.\n\n$$\\int_0^1 x dx$$\n\n![pic](../journal/picture.png)\n\n![[picture.png]]\n\n<div>raw html</div>\n\n[^1]: The footnote text.\n";

fn open(fx: &Fx, file: &str, view: ViewMode) -> Harness<'static, Slot> {
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let p = fx.notes().join(file);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(view));
    }
    settle(&mut h);
    h
}

fn write(fx: &Fx, name: &str, text: &str) -> PathBuf {
    let p = fx.notes().join(name);
    std::fs::write(&p, text).expect("write");
    p
}

fn prev<'a>(h: &'a mut Harness<'static, Slot>) -> &'a mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn click(h: &mut Harness<'static, Slot>, pos: egui::Pos2, mods: Modifiers) {
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: mods });
    h.step();
    h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: mods });
    settle(h);
}

fn find_hit(h: &mut Harness<'static, Slot>, prefix: &str) -> egui::Rect {
    prev(h).hits.iter().find(|(_, l)| l.starts_with(prefix)).map(|(r, _)| *r).unwrap_or_else(|| panic!("no hit {prefix}: {:?}", prev(h).hits))
}

fn wait_idle(h: &mut Harness<'static, Slot>) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        h.step();
        if prev(h).unlaid == 0 && prev(h).parse_count > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(h);
}

#[test]
fn showcase_renders_all_block_kinds() {
    let fx = Fx::new();
    write(&fx, "Showcase.md", SHOWCASE);
    let mut h = open(&fx, "Showcase.md", ViewMode::Preview);
    wait_idle(&mut h);
    // Math may need a moment.
    for _ in 0..200 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
        if cfg!(not(feature = "typst")) || prev(&mut h).textures_painted >= 2 {
            break;
        }
    }
    let p = shot(&mut h, "md-showcase-top");
    eprintln!("screenshot: {}", p.display());
    assert!(prev(&mut h).laid_total > 10);
    assert!(prev(&mut h).hits.iter().any(|(_, l)| l.starts_with("link:")));
    // Scroll down and shoot the second half.
    h.event(Event::PointerMoved(pos2(600.0, 400.0)));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -700.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    for _ in 0..30 {
        h.step();
    }
    let p = shot(&mut h, "md-showcase-bottom");
    eprintln!("screenshot: {}", p.display());
}

#[test]
fn task_checkbox_toggles_text() {
    let fx = Fx::new();
    let mut h = open(&fx, "Welcome.md", ViewMode::Preview);
    let r = find_hit(&mut h, "task:");
    click(&mut h, r.center(), Modifiers::NONE);
    let path = fx.notes().join("Welcome.md");
    let text = h.state_mut().app().state.docs.get(&path).expect("doc").text.clone();
    assert!(text.contains("- [x] task one"), "{text}");
    shot(&mut h, "md-task-toggled");
}

#[test]
fn wiki_link_click_opens_target() {
    let fx = Fx::new();
    let mut h = open(&fx, "Welcome.md", ViewMode::Preview);
    let r = find_hit(&mut h, "link:Ideas");
    click(&mut h, r.center(), Modifiers::NONE);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.active_path(), Some(fx.notes().join("Ideas.md").as_path()));
}

fn long_note(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("## Section {i}\n\nParagraph number {i} with some text to wrap around the pane a few times, **bold**, `code` and a [link](https://example.org/{i}). More words follow here to make it longer and longer.\n\n"));
    }
    s
}

#[test]
fn sync_scroll_both_directions() {
    let fx = Fx::new();
    write(&fx, "Long.md", &long_note(150));
    let mut h = open(&fx, "Long.md", ViewMode::Split);
    wait_idle(&mut h);
    let off = |h: &mut Harness<'static, Slot>| {
        let t = h.state_mut().app().state.tabs.active_tab().expect("tab");
        (t.editor_scroll.offset, t.preview_scroll.offset, t.preview_scroll.content_h)
    };
    let (e0, p0, ch) = off(&mut h);
    assert!(ch > 3000.0, "content height {ch}");
    assert!(e0 < 1.0 && p0 < 1.0);
    // Wheel over the editor (left half) moves the preview.
    h.event(Event::PointerMoved(pos2(480.0, 400.0)));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -1500.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    for _ in 0..40 {
        h.step();
    }
    let (e1, p1, _) = off(&mut h);
    eprintln!("after editor wheel: editor {e1} preview {p1}");
    assert!(e1 > 500.0 && p1 > 200.0, "editor {e1} preview {p1}");
    // Wheel over the preview (right half) moves the editor back.
    h.event(Event::PointerMoved(pos2(950.0, 400.0)));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 800.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    for _ in 0..40 {
        h.step();
    }
    let (e2, p2, _) = off(&mut h);
    eprintln!("after preview wheel: editor {e2} preview {p2}");
    assert!(p2 < p1 - 200.0 && e2 < e1 - 200.0, "editor {e2} preview {p2}");
    shot(&mut h, "md-sync-split");
}

#[cfg(feature = "typst")]
#[test]
fn math_renders_an_image() {
    let fx = Fx::new();
    write(&fx, "TypMath.md", "# Math\n\nInline $a^2 + b^2 = c^2$ and\n\n$$ integral_0^1 x dif x $$\n");
    let mut h = open(&fx, "TypMath.md", ViewMode::Preview);
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(30) && prev(&mut h).textures_painted < 2 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    settle(&mut h);
    let n = prev(&mut h).textures_painted;
    let p = shot(&mut h, "md-math");
    eprintln!("screenshot: {} textures {n}", p.display());
    assert!(n >= 2, "inline and display formula painted, got {n}");
}

#[cfg(not(feature = "typst"))]
#[test]
fn math_shows_as_code_without_typst() {
    let fx = Fx::new();
    let mut h = open(&fx, "Math.md", ViewMode::Preview);
    wait_idle(&mut h);
    assert_eq!(prev(&mut h).textures_painted, 0);
    let p = shot(&mut h, "md-math-nofeature");
    eprintln!("screenshot: {}", p.display());
}

#[test]
fn edit_relayouts_only_changed_blocks() {
    let fx = Fx::new();
    let p = write(&fx, "Long.md", &long_note(300));
    let mut h = open(&fx, "Long.md", ViewMode::Preview);
    wait_idle(&mut h);
    assert_eq!(prev(&mut h).unlaid, 0);
    let before = prev(&mut h).parse_count;
    let at = h.state_mut().app().state.docs.get(&p).expect("doc").text.find("Paragraph number 2 ").expect("pos");
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p, range: at..at, insert: "Z".into(), select: None });
    let l0 = prev(&mut h).laid_total;
    h.step();
    h.step();
    // Notes above ~24 KB are parsed on a worker thread: wait until the new parse landed and was laid out.
    for _ in 0..200 {
        if prev(&mut h).parse_count > before {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.step();
    }
    h.step();
    let (laid, parses) = (prev(&mut h).laid_total - l0, prev(&mut h).parse_count - before);
    eprintln!("blocks laid out after one-char edit: {laid}, parses {parses}");
    assert!(parses >= 1);
    assert!((1..=3).contains(&laid), "laid {laid}");
    settle(&mut h);
    // Run the edit frame once more in isolation to count exactly that frame.
    let at2 = h.state_mut().app().state.docs.get(&fx.notes().join("Long.md")).expect("doc").text.find("Paragraph number 3 ").expect("pos");
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: fx.notes().join("Long.md"), range: at2..at2, insert: "Y".into(), select: None });
    let l0 = prev(&mut h).laid_total;
    let parses_before = prev(&mut h).parse_count;
    h.step(); // executes the command
    h.step(); // preview sees the new version (or starts the background parse)
    for _ in 0..200 {
        if prev(&mut h).parse_count > parses_before {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.step();
    }
    let laid = prev(&mut h).laid_total - l0;
    eprintln!("second edit, laid in frame: {laid}");
    assert!(laid <= 3, "laid {laid}");
}

/// Numbers for the report (run with --nocapture). Thresholds are loose: this runs in the dev profile.
#[test]
fn perf_2000_blocks() {
    let fx = Fx::new();
    let mut s = String::new();
    for i in 0..600 {
        s.push_str(&format!("## Heading {i}\n\nParagraph {i}: Lorem ipsum dolor sit amet, **consectetur** adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua, [[Ideas]] and `code` ut enim ad minim veniam quis nostrud exercitation ullamco.\n\n- item one {i}\n- item two\n\n```rust\nlet x = {i};\n```\n\n> quote number {i} with a little more text to make the block longer than a line of text\n\n"));
    }
    eprintln!("note size: {} bytes", s.len());
    assert!(s.len() > 200 * 1024);
    let p = write(&fx, "Big.md", &s);
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    // Frames until the first preview frame with content.
    let mut first = Duration::ZERO;
    let t = Instant::now();
    let mut n = 0;
    while t.elapsed() < Duration::from_secs(20) {
        let f = Instant::now();
        h.step();
        let d = f.elapsed();
        if prev(&mut h).laid_this_frame > 0 && first == Duration::ZERO {
            first = d;
            break;
        }
        n += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
    let blocks = h.state_mut().app().state.ui.md_preview.laid_total;
    eprintln!("first preview frame with content: {first:?} (after {n} empty frames, {blocks} blocks laid)");
    wait_idle(&mut h);
    let parse = h.state_mut().app().state.perf.timers.get("md.parse").map(|t| t.last_ms);
    eprintln!("md.parse last_ms {parse:?}");
    // Idle frame.
    let mut idle = Vec::new();
    for _ in 0..30 {
        let f = Instant::now();
        h.step();
        idle.push(f.elapsed());
    }
    idle.sort();
    eprintln!("idle frame median {:?} max {:?}", idle[idle.len() / 2], idle[idle.len() - 1]);
    // Edit frames.
    let mut edit = Vec::new();
    for k in 0..10 {
        let at = h.state_mut().app().state.docs.get(&p).expect("doc").text.find("Paragraph 3:").expect("pos");
        h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p.clone(), range: at..at, insert: format!("{k}"), select: None });
        let before = prev(&mut h).parse_count;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(3) {
            let f = Instant::now();
            h.step();
            let d = f.elapsed();
            if prev(&mut h).parse_count > before {
                edit.push(d); // the frame that swaps in the new parse and lays out the changed block
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        settle(&mut h);
    }
    edit.sort();
    let st = &h.state_mut().app().state;
    let (pl, ll) = (st.perf.timers.get("md.parse").map(|t| t.last_ms), st.perf.timers.get("md.layout").map(|t| t.last_ms));
    eprintln!("edit frame median {:?} max {:?}; md.parse last {pl:?} ms, md.layout last {ll:?} ms", edit[edit.len() / 2], edit[edit.len() - 1]);
}

/// Inline math on a later row of a wrapped paragraph must sit on that row at fractional scaling too.
#[cfg(feature = "typst")]
#[test]
#[allow(clippy::collapsible_if)]
fn inline_math_sits_on_its_own_row_at_fractional_scale() {
    let w = "AWEIhufdbpsiöaeuzfhbgv<oawze ";
    let text = format!("# T\n\nPRENIS {}\n{}[xy](https://e.org)$M = sigma$\n{}\n", w.repeat(3), w.repeat(2), w.repeat(4));
    for (ppp, font) in [(1.0, 15.0), (1.25, 18.0), (1.5, 22.0), (1.75, 24.0), (1.33, 20.0)] {
        let fx = Fx::new();
        write(&fx, "Wrap.md", &text);
        let mut o = fx.opts(vec![fx.notes()]);
        o.config.preview_font_size = font;
        let mut h = Harness::builder().with_size(vec2(900.0, 700.0)).with_pixels_per_point(ppp).with_accessibility_check(false).build_ui_state(
            |ui, slot: &mut Slot| {
                if slot.app.is_none() {
                    if let Some(o) = slot.opts.take() {
                        slot.app = Some(iron_sheets::App::new(ui.ctx(), o));
                    }
                }
                if let Some(a) = slot.app.as_mut() {
                    a.frame(ui);
                }
            },
            Slot { opts: Some(o), app: None },
        );
        settle(&mut h);
        {
            let st = &mut h.state_mut().app().state;
            st.commands.push(Command::OpenPath { path: fx.notes().join("Wrap.md"), new_tab: true, line: None });
            st.commands.push(Command::SetViewMode(ViewMode::Preview));
        }
        settle(&mut h);
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(30) && prev(&mut h).textures_painted < 1 {
            h.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        settle(&mut h);
        let hits = prev(&mut h).hits.clone();
        let slot = hits.iter().find(|(_, l)| l == "slot").map(|(r, _)| *r).expect("math painted");
        // The link text sits right before the formula, on the row that holds the formula's gap glyph.
        let row = hits.iter().find(|(_, l)| l.starts_with("link:")).map(|(r, _)| *r).expect("link");
        if ppp == 1.5 {
            shot(&mut h, "md-math-wrap-ppp1.5");
        }
        let cy = slot.center().y;
        assert!(row.min.y <= cy && cy <= row.max.y, "ppp {ppp} font {font}: math centre {cy} outside the row {row:?} of the text before it");
    }
}

#[test]
fn kbd_renders_as_keycaps() {
    let fx = Fx::new();
    write(&fx, "Keys.md", "# Press <kbd>Ctrl</kbd>+<kbd>C</kbd> now\n\nCopy with <kbd>Ctrl</kbd>+<kbd>C</kbd> and paste with <kbd>Ctrl</kbd>+<kbd>V</kbd>, then go on.\n");
    let mut h = open(&fx, "Keys.md", ViewMode::Preview);
    wait_idle(&mut h);
    let p = shot(&mut h, "md-kbd");
    eprintln!("screenshot: {}", p.display());
    let keys = prev(&mut h).hits.iter().filter(|(_, l)| l == "kbd").count();
    assert_eq!(keys, 6, "one box per <kbd>");
}
