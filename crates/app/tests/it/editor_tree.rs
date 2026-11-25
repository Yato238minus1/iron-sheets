//! Deleting text in a big note must leave the rows of the layout in order right away, without a scroll.
use crate::common::*;
use egui::Key;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

type H = egui_kittest::Harness<'static, Slot>;

fn note(kb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < kb * 1024 {
        s.push_str("Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Duis autem vel eum iriure dolor in hendrerit in vulputate velit esse molestie consequat, vel illum dolore eu feugiat nulla facilisis at vero eros et accumsan et iusto odio dignissim qui blandit praesent luptatum zzril delenit augue duis dolore te feugait nulla facilisi. Lorem ipsum dolor sit amet, consectetuer adipiscing elit, sed diam nonummy nibh euismod tincidunt ut laoreet dolore magna aliquam erat volutpat. Ut wisi enim ad minim veniam, quis nostrud exerci tation ullamcorper suscipit lobortis nisl ut aliquip ex ea commodo consequat. Duis autem vel eum iriure dolor in hendrerit in vulputate velit esse molestie consequat, vel illum dolore eu feugiat nulla facilisis at vero eros et accumsan et iusto odio dignissim qui blandit praesent luptatum zzril delenit augue duis dolore te feugait nulla facilisi. Nam liber tempor cum soluta nobis eleifend option congue nihil imperdiet doming id quod mazim placerat facer possim assum. Lorem ipsum dolor sit amet, consectetuer adipiscing elit, sed diam nonummy nibh euismod tincidunt ut laoreet dolore magna aliquam erat volutpat. Ut wisi enim ad minim veniam, quis nostrud exerci tation ullamcorper suscipit lobortis nisl ut aliquip ex ea commodo consequat.  Duis autem vel eum iriure dolor in hendrerit in vulputate velit esse molestie consequat, vel illum dolore eu feugiat nulla facilisis. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, At accusam aliquyam diam diam dolore dolores duo eirmod eos erat, et nonumy sed tempor et et invidunt justo labore Stet clita ea et gubergren, kasd magna no rebum. sanctus sea sed takimata ut vero voluptua. est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat. Consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Lorem ipsum dolor sit amet, consetetur sadipscing elitr, sed diam nonumy eirmod tempor invidunt ut labore et dolore magna aliquyam erat, sed diam voluptua. At vero eos et accusam et justo duo dolores et ea rebum. Stet clita kasd gubergren, no sea takimata sanctus est Lorem ipsum dolor sit amet. Duis autem vel eum iriure dolor in hendrerit in vulputate velit esse molestie consequat, vel illum dolore eu feugiat nulla facilisis at vero eros et accumsan et iusto odio dignissim qui blandit praesent luptatum zzril delenit augue duis dolore te feugait nulla facilisi. Lorem ipsum dolor sit amet, consectetuer adipiscing elit, sed diam nonummy nibh euismod tincidunt ut laoreet dolore magna aliquam erat volutpat. Ut wisi enim ad minim veniam, quis nostrud exerci tation ullamcorper suscipit lobortis nisl ut aliquip ex ea commodo consequat. Duis autem vel eum iriure dolor in hendrerit in vulputate velit esse molestie consequat, vel illum dolore eu feugiat nulla facilisis at vero eros et accumsan et iusto odio dignissim qui blandit praesent luptatum zzril delenit augue duis dolore te feugait nulla facilisi. Nam liber tempor cum soluta nobis eleifend option congue nihil imperdiet doming id quod mazim placerat facer possim assum. Lorem ipsum dolor sit amet, consectetuer adipiscing elit, sed diam nonummy nibh euismod tincidunt ut laoreet dolore magna aliquam erat volutpat. Ut wisi enim ad minim veniam, quis nostrud exerci tation ullamcorper suscipit lobortis nisl ut aliquip ex ea commodo consequat. Duis autem vel eum iriure dolor in");
        s.push_str(&format!("## Heading {i}\n\nParagraph {i}: lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore.\n\n- item {i}\n- [ ] task {i}\n\n"));
        i += 1;
    }
    s
}

fn open(kb: usize, wrap: bool, ppp: f32) -> (Fx, H, String) {
    let fx = Fx::new();
    let text = note(kb);
    let path = fx.notes().join("Big.md");
    std::fs::write(&path, &text).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.ctx.set_pixels_per_point(ppp);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    let st = &mut h.state_mut().app().state;
    st.config.editor_wrap = wrap;
    st.commands.push(Command::OpenPath { path, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);
    (fx, h, text)
}

fn overlapping(h: &mut H) -> usize {
    let st = &mut h.state_mut().app().state;
    let id = st.tabs.active_tab().expect("tab").id;
    st.ui.editor.stats(id).expect("stats").overlapping
}

fn select(h: &mut H, r: std::ops::Range<usize>) {
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").pending_cursor = Some(r);
    settle(h);
}

fn text_len(h: &mut H) -> usize {
    h.state_mut().app().state.docs.iter().next().map_or(0, |d| d.text.len())
}

/// The key is handled in the next frame; returns how many bytes it removed.
fn press(h: &mut H, k: Key) -> usize {
    let before = text_len(h);
    h.key_press(k);
    h.step();
    before - text_len(h)
}

fn check(kb: usize, wrap: bool, ppp: f32) {
    let (_fx, mut h, _) = open(kb, wrap, ppp);
    // A big selection (a third of the note) deleted from the middle, then from the top.
    for from in [0.4, 0.0] {
        let t = h.state_mut().app().state.docs.iter().next().map(|d| d.text.clone()).expect("doc");
        let a = (0..=(t.len() as f64 * from) as usize).rev().find(|&i| t.is_char_boundary(i)).unwrap_or(0);
        let b = (0..=a + t.len() / 3).rev().find(|&i| t.is_char_boundary(i)).unwrap_or(a);
        select(&mut h, a..b);
        assert_eq!(press(&mut h, Key::Backspace), b - a, "the selection was deleted");
        for _ in 0..4 {
            assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap}: after deleting a third at {from}");
            h.step();
        }
    }
    for part in [1usize, 3, 5, 7, 9] {
        let text_now = h.state_mut().app().state.docs.iter().next().map(|d| d.text.clone()).expect("doc");
        let mid = text_now.len() * part / 10;
        let at = text_now[..mid].rfind("\n\n").expect("blank line") + 2;
        // Several lines selected, then deleted.
        let end = at + text_now[at..].match_indices('\n').nth(6).expect("lines").0;
        select(&mut h, at..end);
        assert!(press(&mut h, Key::Delete) > 0, "the selection was deleted");
        assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap} at {part}/10: after deleting a selection");
        for _ in 0..3 {
            h.step();
            assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap} at {part}/10: a frame after deleting a selection");
        }
        // Backspace across line joins.
        let t = h.state_mut().app().state.docs.iter().next().map(|d| d.text.clone()).expect("doc");
        let nl = at + t[at..].find('\n').expect("newline") + 1;
        select(&mut h, nl..nl);
        for _ in 0..30 {
            assert_eq!(press(&mut h, Key::Backspace), 1, "a line break was deleted");
            assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap} at {part}/10: after backspace");
            h.step();
            assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap} at {part}/10: a frame after backspace");
        }
    }
}

#[test]
fn delete_in_40kb() {
    for wrap in [true, false] {
        check(40, wrap, 1.0);
        check(40, wrap, 1.5);
        // A zoom that makes the row height a non-integer number of pixels (see `Info::next`).
        check(40, wrap, 1.1);
    }
}

#[test]
fn delete_in_200kb() {
    check(200, true, 1.0);
    check(200, true, 1.1);
}

#[test]
fn delete_in_1mb() {
    check(1024, true, 1.0);
}

fn wheel(dy: f32) -> egui::Event {
    egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    }
}

/// Random deletions with the view scrolled away from the caret in between.
fn fuzz(kb: usize, wrap: bool) {
    let (_fx, mut h, _) = open(kb, wrap, 1.0);
    let mut seed = 7u64;
    let mut rnd = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n.max(1)
    };
    h.event(egui::Event::PointerMoved(egui::pos2(700.0, 400.0)));
    for step in 0..120 {
        let t = h.state_mut().app().state.docs.iter().next().map(|d| d.text.clone()).expect("doc");
        let a = (0..=rnd(t.len())).rev().find(|&i| t.is_char_boundary(i)).unwrap_or(0);
        let b = (0..=(a + [0, 1, 40, 300, 3000][rnd(5)]).min(t.len())).rev().find(|&i| t.is_char_boundary(i)).unwrap_or(a);
        select(&mut h, a..b);
        if rnd(2) == 0 {
            for _ in 0..rnd(4) {
                h.event(wheel([-900.0, 900.0, -200.0, 5000.0][rnd(4)]));
                h.step();
            }
        }
        press(&mut h, Key::Backspace);
        for k in 0..3 {
            assert_eq!(overlapping(&mut h), 0, "{kb} kB wrap={wrap} step {step}: frame {k} after deleting {a}..{b}");
            h.step();
        }
    }
}

#[test]
fn fuzz_deletions_in_40kb() {
    fuzz(40, true);
}

#[test]
fn fuzz_deletions_in_400kb() {
    fuzz(400, false);
}
