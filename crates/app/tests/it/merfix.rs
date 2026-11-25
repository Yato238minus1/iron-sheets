//! Mermaid text overlay alignment and blank renders.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use egui_kittest::{Harness, kittest::Queryable};
use iron_sheets::command::Command;
use iron_sheets_core::config::{ThemePref, ViewMode};
use std::time::Duration;

type H = Harness<'static, Slot>;

fn open(src: &str) -> (Fx, H) {
    open_ppp(src, 1.0)
}

fn open_ppp(src: &str, ppp: f32) -> (Fx, H) {
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, format!("# D\n\n```mermaid\n{src}```\n\nafter\n")).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Dark;
    iron_sheets::theme::set_row_recording(true);
    let mut h =
        Harness::builder().with_size(egui::vec2(1100.0, 720.0)).with_pixels_per_point(ppp).with_step_dt(0.05).with_accessibility_check(false).build_ui_state(
            |ui, slot: &mut Slot| {
                if slot.app.is_none()
                    && let Some(o) = slot.opts.take()
                {
                    slot.app = Some(iron_sheets::App::new(ui.ctx(), o));
                }
                if let Some(a) = slot.app.as_mut() {
                    a.frame(ui);
                }
            },
            Slot { opts: Some(o), app: None },
        );
    for _ in 0..6 {
        h.step();
    }
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait(&mut h, "diagram painted", |h| hit(h, "diagram").is_some());
    (fx, h)
}

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn hit(h: &mut H, label: &str) -> Option<Rect> {
    prev(h).hits.iter().find(|(_, l)| l == label).map(|(r, _)| *r)
}

fn wait(h: &mut H, what: &str, cond: impl Fn(&mut H) -> bool) {
    for _ in 0..800 {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timeout: {what}");
}

fn run(h: &mut H, ms: u64) {
    for _ in 0..ms / 20 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn click(h: &mut H, p: Pos2) {
    h.event(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
}

#[test]
fn fullscreen_selection_is_on_the_text() {
    let (_fx, mut h) = open("graph TD\n  A-->B\n");
    let pic = hit(&mut h, "diagram").expect("rect");
    click(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    click(&mut h, full.center());
    run(&mut h, 500);
    let rects = prev(&mut h).diagram_full.as_ref().expect("open").text_rects().to_vec();
    let first = rects[0];
    click(&mut h, first.center());
    click(&mut h, first.center());
    shot(&mut h, "merfix-fullscreen-sel");
}

#[test]
fn fullscreen_zoomed_at_1_5x() {
    let (_fx, mut h) = open_ppp("graph TD\n  A-->B\n", 1.5);
    run(&mut h, 200);
    let pic = hit(&mut h, "diagram").expect("rect");
    click(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    click(&mut h, full.center());
    run(&mut h, 500);
    assert!(prev(&mut h).diagram_full.is_some(), "open");
    for _ in 0..3 {
        h.get_by_label("Zoom in").click();
        run(&mut h, 100);
    }
    run(&mut h, 1500);
    let rects = prev(&mut h).diagram_full.as_ref().expect("open").text_rects().to_vec();
    click(&mut h, rects[0].center());
    click(&mut h, rects[0].center());
    shot(&mut h, "merfix-zoomed-hidpi");
}

fn edit(h: &mut H, p: &std::path::Path, from: &str, to: &str) {
    let d = h.state_mut().app().state.docs.get_mut(p).expect("doc");
    let at = d.text.find(from).expect("text");
    assert!(d.apply_edit(at..at + from.len(), to));
}

#[test]
fn identical_diagrams_survive_editing_one() {
    let d = "```mermaid\ngraph TD\n  A-->B\n```\n\n";
    let fx = Fx::new();
    let p = fx.notes().join("M.md");
    std::fs::write(&p, format!("# D\n\n{d}{d}after\n")).expect("write");
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = ThemePref::Dark;
    let mut h = harness_dt(o, 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Preview));
    run(&mut h, 800);
    edit(&mut h, &p, "A-->B", "A-->B-->C");
    run(&mut h, 1500);
    assert!(hit(&mut h, "diagram").is_some());
    shot(&mut h, "merfix-dup-after");
}

#[test]
fn window_selection_is_on_the_text() {
    let (_fx, mut h) = open("graph TD\n  A-->B\n");
    let pic = hit(&mut h, "diagram").expect("rect");
    click(&mut h, pic.center());
    let win = hit(&mut h, "view-window").expect("button");
    click(&mut h, win.center());
    run(&mut h, 600);
    let rects = prev(&mut h).diagram_win.as_ref().expect("open").text_rects().to_vec();
    click(&mut h, rects[0].center());
    click(&mut h, rects[0].center());
    shot(&mut h, "merfix-window-sel");
}

fn drag(h: &mut H, from: Pos2, steps: &[Pos2]) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for p in steps {
        h.event(Event::PointerMoved(*p));
        h.step();
    }
    let last = *steps.last().expect("steps");
    h.event(Event::PointerButton { pos: last, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
}

fn open_full() -> (Fx, H) {
    let (fx, mut h) = open("graph TD\n  A-->B\n");
    let pic = hit(&mut h, "diagram").expect("rect");
    click(&mut h, pic.center());
    let full = hit(&mut h, "view-fullscreen").expect("button");
    click(&mut h, full.center());
    run(&mut h, 500);
    (fx, h)
}

#[test]
fn a_pan_that_started_on_empty_space_survives_crossing_text() {
    let (_fx, mut h) = open_full();
    let t = prev(&mut h).diagram_full.as_ref().expect("open").text_rects()[0];
    let y = t.center().y;
    // Press right of the label and drag right: the label moves under the (fixed) press point.
    let a = Pos2::new(t.max.x + 40.0, y);
    let (b, c) = (Pos2::new(a.x + 40.0 + t.width() / 2.0, y), Pos2::new(a.x + 150.0, y));
    let p0 = prev(&mut h).diagram_full.as_ref().expect("open").view.pan;
    // Two big steps; after the first the label lies under the press point.
    drag(&mut h, a, &[b, c]);
    let v = prev(&mut h).diagram_full.as_ref().expect("open");
    assert!(((v.view.pan - p0).x - (c.x - a.x)).abs() < 1.0, "pan follows the whole drag: {:?}", v.view.pan - p0);
    assert!(v.selected().is_empty(), "no selection");
}

#[test]
fn a_press_on_text_selects_and_does_not_pan() {
    let (_fx, mut h) = open_full();
    let t = prev(&mut h).diagram_full.as_ref().expect("open").text_rects()[0];
    let p0 = prev(&mut h).diagram_full.as_ref().expect("open").view.pan;
    drag(&mut h, t.left_center() + egui::vec2(2.0, 0.0), &[t.center(), t.right_center() - egui::vec2(2.0, 0.0)]);
    let v = prev(&mut h).diagram_full.as_ref().expect("open");
    assert_eq!(v.view.pan, p0);
    assert_eq!(v.selected(), "A");
}
