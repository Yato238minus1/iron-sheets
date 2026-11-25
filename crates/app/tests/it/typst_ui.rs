use crate::common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

const DOC: &str = "#set page(width: 12cm, height: 7cm, margin: 1cm)\n= Page one\nSome text on the first page.\n\nMore text here.\n#pagebreak()\n= Page two\n$ sum_(i=1)^n i = (n(n+1))/2 $\n\nA paragraph.\n#pagebreak()\n= Page three\nLast page with *bold* and _italic_.\n";

/// Step frames until `f` holds or the timeout passes.
fn wait(h: &mut H, secs: u64, mut f: impl FnMut(&mut H) -> bool) -> bool {
    let t0 = Instant::now();
    loop {
        h.step();
        if f(h) {
            return true;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn setup() -> (Fx, H) {
    let fx = Fx::new();
    std::fs::write(fx.notes().join("Doc.typ"), DOC).expect("write doc");
    let h = harness(fx.opts(vec![fx.notes()]));
    // The symbol grid includes symbols bundled fonts lack; app draws tofu in production.
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    (fx, h)
}

fn open_doc(h: &mut H) {
    h.get_all_by_label("Doc.typ").next().expect("Doc.typ in tree").click();
    settle(h);
}

fn snapshot_pages(h: &mut H) -> Option<(usize, bool, usize)> {
    let st = &h.state_mut().app().state;
    let id = st.tabs.active_tab()?.id;
    st.typst.document(id).map(|s| (s.pages.len(), s.ok, s.diagnostics.len()))
}

fn textures(h: &mut H) -> usize {
    h.state_mut().app().state.ui.typst_preview.texture_count()
}

#[test]
fn three_pages_render_and_scroll_map() {
    let (_fx, mut h) = setup();
    let t0 = Instant::now();
    open_doc(&mut h);
    assert!(wait(&mut h, 60, |h| snapshot_pages(h).is_some_and(|s| s.0 == 3 && s.1)), "three pages compiled");
    let compiled = t0.elapsed();
    assert!(wait(&mut h, 60, |h| textures(h) >= 3), "visible page textures are uploaded");
    eprintln!("open -> compiled {compiled:?}, -> first texture {:?}", t0.elapsed());
    shot(&mut h, "typst-01-three-pages");
    // Scroll map: non-empty, monotonic, spans the content.
    let st = &mut h.state_mut().app().state;
    let tab = st.tabs.active_tab().expect("tab");
    let map = &tab.preview_scroll.map;
    assert!(!map.is_empty());
    let n_lines = DOC.lines().count() as u32 + 1;
    let mut last = -1.0f32;
    for l in 0..=n_lines {
        let y = map.y_for_line(l as f32);
        assert!(y >= last, "monotonic at line {l}: {y} < {last}");
        last = y;
    }
    assert!(map.y_for_line(n_lines as f32) > map.y_for_line(0.0) + 100.0);
    assert!(tab.preview_scroll.content_h > tab.preview_scroll.viewport_h, "three pages scroll");
}

#[test]
fn error_keeps_old_pages_and_diagnostic_click_moves_cursor() {
    let (fx, mut h) = setup();
    open_doc(&mut h);
    assert!(wait(&mut h, 60, |h| snapshot_pages(h).is_some_and(|s| s.0 == 3 && s.1) && textures(h) >= 3));
    let path = fx.notes().join("Doc.typ");
    {
        let st = &mut h.state_mut().app().state;
        let end = st.docs.get(&path).expect("doc").text.len();
        st.commands.push(Command::ApplyEdit { path: path.clone(), range: end..end, insert: "\n#unknownfn(1)\n".into(), select: None });
    }
    assert!(wait(&mut h, 60, |h| snapshot_pages(h).is_some_and(|s| !s.1 && s.2 > 0)), "compile fails with a diagnostic");
    settle(&mut h);
    assert_eq!(snapshot_pages(&mut h).map(|s| s.0), Some(3), "old pages are kept");
    assert!(textures(&mut h) >= 1, "old textures still drawn");
    h.get_by_label_contains("compile failed");
    shot(&mut h, "typst-02-error-keeps-pages");
    h.get_by_label_contains("unknown variable").click();
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    let text = &st.docs.get(&path).expect("doc").text;
    let line_start = text.find("#unknownfn").expect("inserted");
    let line_end = line_start + "#unknownfn(1)".len();
    let cur = st.tabs.active_tab().and_then(|t| t.cursor.clone()).expect("cursor");
    assert!((line_start..=line_end).contains(&cur.start), "cursor {cur:?} not on the error line {line_start}..{line_end}");
}

fn drag(h: &mut H, pts: &[egui::Pos2]) {
    use egui::{Event, Modifiers, PointerButton};
    h.event(Event::PointerMoved(pts[0]));
    h.step();
    h.event(Event::PointerButton { pos: pts[0], button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for p in &pts[1..] {
        h.event(Event::PointerMoved(*p));
        h.step();
    }
    let last = *pts.last().expect("points");
    h.event(Event::PointerButton { pos: last, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
}

fn line(a: egui::Pos2, b: egui::Pos2, n: usize) -> Vec<egui::Pos2> {
    (0..=n).map(|i| a + (b - a) * (i as f32 / n as f32)).collect()
}

fn open_symbols(h: &mut H) -> egui::Rect {
    h.state_mut().app().state.commands.push(Command::SetSidebarView(iron_sheets::state::SidebarView::Symbols));
    assert!(wait(h, 120, |h| h.state_mut().app().state.ui.symbols.recognizer_ready()), "recognizer built");
    settle(h);
    h.state_mut().app().state.ui.symbols.canvas_rect.expect("canvas drawn")
}

#[test]
fn draw_plus_finds_plus() {
    let (_fx, mut h) = setup();
    open_doc(&mut h);
    let r = open_symbols(&mut h);
    let c = r.center();
    let s = r.width() * 0.3;
    drag(&mut h, &line(c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0), 12));
    drag(&mut h, &line(c - egui::vec2(0.0, s), c + egui::vec2(0.0, s), 12));
    let ok = wait(&mut h, 20, |h| h.state_mut().app().state.ui.symbols.candidate_names().iter().any(|n| n == "plus"));
    let names = h.state_mut().app().state.ui.symbols.candidate_names();
    shot(&mut h, "typst-03-draw-plus");
    assert!(ok, "plus among candidates: {names:?}");
}

#[test]
fn draw_arrow_and_insert() {
    let (fx, mut h) = setup();
    open_doc(&mut h);
    let r = open_symbols(&mut h);
    let c = r.center();
    let s = r.width() * 0.32;
    let (l, rt) = (c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0));
    drag(&mut h, &line(l, rt, 14));
    let mut head = line(rt + egui::vec2(-s * 0.5, -s * 0.5), rt, 8);
    head.extend(line(rt, rt + egui::vec2(-s * 0.5, s * 0.5), 8));
    drag(&mut h, &head);
    let ok = wait(&mut h, 20, |h| h.state_mut().app().state.ui.symbols.candidate_names().iter().any(|n| n == "arrow.r"));
    let names = h.state_mut().app().state.ui.symbols.candidate_names();
    shot(&mut h, "typst-04-draw-arrow");
    assert!(ok, "arrow.r among candidates: {names:?}");
    // Click the arrow glyph: Auto mode in a .typ note outside math inserts `#sym.name`.
    h.get_all_by_label("\u{2192}").next().expect("arrow button").click();
    settle(&mut h);
    let path = fx.notes().join("Doc.typ");
    let st = &mut h.state_mut().app().state;
    let t = &st.docs.get(&path).expect("doc").text;
    assert!(t.contains("#sym.arrow"), "inserted: {:?} cands {:?}", &t[t.len().saturating_sub(40)..], st.ui.symbols.candidate_names());
}

#[test]
fn search_arrow_lists_matches() {
    let (_fx, mut h) = setup();
    open_doc(&mut h);
    h.state_mut().app().state.commands.push(Command::SetSidebarView(iron_sheets::state::SidebarView::Symbols));
    settle(&mut h);
    h.get_by_role(Role::TextInput).click();
    settle(&mut h);
    h.get_by_role(Role::TextInput).type_text("arrow");
    settle(&mut h);
    let names = h.state_mut().app().state.ui.symbols.filtered_names();
    let q = h.state_mut().app().state.ui.symbols.query.clone();
    let tail = {
        let st = &h.state_mut().app().state;
        let t = &st.docs.get(&_fx.notes().join("Doc.typ")).expect("doc").text;
        t[t.len().saturating_sub(30)..].to_string()
    };
    let q = format!("{q} / doc end {tail:?}");
    assert!(names.len() > 10 && names[0].contains("arrow"), "q={q:?} {:?}", &names[..names.len().min(5)]);
    assert!(names.iter().any(|n| n == "arrow.r"));
    shot(&mut h, "typst-05-search-arrow");
}

#[test]
fn compiling_indicator_shows_while_the_preview_is_behind() {
    let (fx, mut h) = setup();
    open_doc(&mut h);
    assert!(wait(&mut h, 60, |h| snapshot_pages(h).is_some_and(|s| s.0 == 3 && s.1) && textures(h) >= 3));
    assert!(h.query_by_label("compiling...").is_none(), "idle preview shows no indicator");
    let path = fx.notes().join("Doc.typ");
    {
        let st = &mut h.state_mut().app().state;
        let end = st.docs.get(&path).expect("doc").text.len();
        st.commands.push(Command::ApplyEdit { path, range: end..end, insert: "\nMore.\n".into(), select: None });
    }
    // The edit is fed after the typing debounce; from then until its compile is shown the indicator is on.
    assert!(wait(&mut h, 60, |h| h.query_by_label("compiling...").is_some()), "indicator while the edit is not compiled yet");
    shot(&mut h, "typst-02-compiling");
    assert!(wait(&mut h, 60, |h| h.query_by_label("compiling...").is_none()), "indicator goes away when the compile is shown");
}
