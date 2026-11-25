//! Symbol draw panel: no-match state and keyboard selection (IS-28/IS-29).
use crate::common::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

#[allow(dead_code)]
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

#[allow(dead_code)]
fn snapshot_pages(h: &mut H) -> Option<(usize, bool, usize)> {
    let st = &h.state_mut().app().state;
    let id = st.tabs.active_tab()?.id;
    st.typst.document(id).map(|s| (s.pages.len(), s.ok, s.diagnostics.len()))
}

#[allow(dead_code)]
fn textures(h: &mut H) -> usize {
    h.state_mut().app().state.ui.typst_preview.texture_count()
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
fn no_match_state_is_designed() {
    let (_fx, mut h) = setup();
    open_doc(&mut h);
    let r = open_symbols(&mut h);
    // Draw a dense multi-loop chaotic scribble that scores below symrec::Config::min_score.
    let mut pts = Vec::new();
    for i in 0..70 {
        let t = i as f32 * 0.45;
        let x = r.center().x + r.width() * 0.38 * (t * 1.7).sin() + r.width() * 0.08 * (t * 5.3).cos();
        let y = r.center().y + r.height() * 0.38 * (t * 2.3).cos() + r.height() * 0.08 * (t * 6.1).sin();
        pts.push(egui::pos2(x, y));
    }
    drag(&mut h, &pts);
    assert!(wait(&mut h, 20, |h| h.state_mut().app().state.ui.symbols.answered()), "recognizer answered scribble");
    settle(&mut h);
    assert!(h.state_mut().app().state.ui.symbols.candidate_names().is_empty(), "chaotic scribble must yield no candidates");
    shot(&mut h, "ux-w3-06-symbols-no-match");
    assert!(h.query_by_label_contains("No match. Try drawing larger or adding strokes.").is_some());
    assert!(h.query_by_label_contains("Thinking").is_none());
}

#[test]
fn symbol_search_grid_keyboard_nav() {
    let (fx, mut h) = setup();
    open_doc(&mut h);
    open_symbols(&mut h);
    let search = h.get_by_label("Search symbols by name");
    search.click();
    settle(&mut h);
    h.get_by_label("Search symbols by name").type_text("arrow");
    settle(&mut h);
    let filtered = h.state_mut().app().state.ui.symbols.filtered_names();
    assert!(filtered.len() >= 3, "filtered={filtered:?}");
    // Down moves focus into the virtualized symbol_grid, Right steps to the next column.
    h.key_press(egui::Key::ArrowDown);
    settle(&mut h);
    h.key_press(egui::Key::ArrowRight);
    settle(&mut h);
    shot(&mut h, "ux-w3-08-symbols-grid-keyboard");
    // Esc from the focused grid clears the query and returns focus to search.
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.state_mut().app().state.ui.symbols.query.is_empty(), "Escape clears symbol search query");
    // Re-filter, step into grid, and Enter inserts the selected symbol.
    h.get_by_label("Search symbols by name").type_text("arrow");
    settle(&mut h);
    h.key_press(egui::Key::ArrowDown);
    settle(&mut h);
    h.key_press(egui::Key::ArrowRight);
    settle(&mut h);
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let path = fx.notes().join("Doc.typ");
    let t = h.state_mut().app().state.docs.get(&path).expect("doc").text.clone();
    assert!(t.contains("#sym."), "expected inserted symbol in {t:?}");
}

#[test]
fn keyboard_selects_candidate_and_enter_inserts() {
    let (fx, mut h) = setup();
    open_doc(&mut h);
    let r = open_symbols(&mut h);
    let c = r.center();
    let s = r.width() * 0.3;
    drag(&mut h, &line(c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0), 12));
    drag(&mut h, &line(c - egui::vec2(0.0, s), c + egui::vec2(0.0, s), 12));
    // Both strokes answered (the answer to a stroke set resets the highlight to the first candidate).
    assert!(wait(&mut h, 20, |h| h.state_mut().app().state.ui.symbols.candidate_names().first().is_some_and(|n| n == "plus")));
    settle(&mut h);
    let names = h.state_mut().app().state.ui.symbols.candidate_names();
    // The canvas still has focus from drawing: Right moves to the second candidate, Enter inserts it.
    h.key_press(egui::Key::ArrowRight);
    settle(&mut h);
    shot(&mut h, "ux-w3-07-symbols-keyboard");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let path = fx.notes().join("Doc.typ");
    let st = &mut h.state_mut().app().state;
    let t = st.docs.get(&path).expect("doc").text.clone();
    assert!(t.contains(&format!("#sym.{}", names[1])), "inserted second candidate {:?}: {:?}", names[1], t);
}

#[test]
fn escape_clears_the_drawing() {
    let (_fx, mut h) = setup();
    open_doc(&mut h);
    let r = open_symbols(&mut h);
    let c = r.center();
    let s = r.width() * 0.3;
    drag(&mut h, &line(c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0), 12));
    assert!(wait(&mut h, 20, |h| !h.state_mut().app().state.ui.symbols.candidate_names().is_empty()));
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(h.state_mut().app().state.ui.symbols.candidate_names().is_empty());
}
