//! The smooth caret: glides after a move, fades while blinking, asks for repaints only while it moves or fades, and is
//! switched off by the master switch or its own group switch (then egui's own caret is the only one).
use crate::common::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn setup(f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> (Fx, H) {
    let fx = Fx::new();
    let mut o = fx.opts(vec![fx.notes()]);
    f(&mut o.config);
    let mut h = harness_dt(o, 1.0 / 60.0);
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label("Welcome.md").next().expect("file in tree").click();
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).click();
    run(&mut h, 30);
    (fx, h)
}

fn run(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn caret(h: &mut H) -> (egui::Pos2, bool, f32) {
    let c = h.state_mut().app().state.ui.editor.caret();
    (c.painted, c.animating, c.alpha)
}

fn repaint_delay(h: &H) -> Duration {
    h.output().viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX)
}

#[test]
fn caret_glides_after_a_move_then_repaints_stop_until_the_fade() {
    // `common::Fx::opts` sets `autosave_delay_ms = 50`; keep it above the 400 ms blink window so the wall-clock autosave timer does not mask the caret wakeup.
    let (_fx, mut h) = setup(|c| c.autosave_delay_ms = 500);
    let (x0, ..) = caret(&mut h);
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    h.step();
    let (x1, glide1, _) = caret(&mut h);
    assert!(glide1, "mid-glide on the first frame after the move");
    assert!(wants_immediate_repaint(&h), "a glide asks for a frame at once");
    h.step();
    let (x2, glide2, _) = caret(&mut h);
    assert!(glide2 && x2.x > x1.x, "still moving, to the right: {x1:?} -> {x2:?}");
    run(&mut h, 11); // glide 45 ms plus the 110 ms tail lag, then the wakeup before the fade
    let (x3, glide3, alpha) = caret(&mut h);
    assert!(!glide3 && x3.x > x2.x && x3.x > x0.x, "arrived: {x3:?}");
    assert_eq!(alpha, 1.0, "solid right after a move");
    assert!(!wants_immediate_repaint(&h), "nothing repaints between the fades");
    let d = repaint_delay(&h);
    assert!(d > Duration::from_millis(50) && d < Duration::from_millis(400), "one wakeup before the fade starts: {d:?}");
}

#[test]
fn blink_fades_and_repaints_every_frame_inside_a_fade() {
    let (_fx, mut h) = setup(|_| {});
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    run(&mut h, 10);
    let mut seen_mid = false;
    for _ in 0..90 {
        h.step();
        let (_, _, a) = caret(&mut h);
        if a > 0.2 && a < 0.8 {
            assert!(wants_immediate_repaint(&h), "fading repaints every frame");
            seen_mid = true;
        }
    }
    assert!(seen_mid, "a frame inside a fade was seen");
}

#[test]
fn blink_shots() {
    let (_fx, mut h) = setup(|_| {});
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    run(&mut h, 10);
    shot(&mut h, "caret_rest");
    for _ in 0..90 {
        h.step();
        if (0.2..0.8).contains(&caret(&mut h).2) {
            shot(&mut h, "caret_mid_fade");
            return;
        }
    }
    panic!("no frame inside a fade");
}

#[test]
fn idle_repaints_and_glide_frame_time() {
    for smooth in [false, true] {
        let (_fx, mut h) = setup(|c| c.anim_caret = smooth);
        h.get_by_role(Role::MultilineTextInput).type_text("X");
        // Background work started by the edit (preview, highlight) asks for a repaint when it lands; under a loaded
        // test run that can fall into the measured window. Give it wall-clock time to finish first.
        for _ in 0..60 {
            h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        // 3 simulated seconds (3 blink cycles) of idle: frames that asked for the next one at once (timed wakeups: 2 per cycle in both modes).
        let mut now = 0;
        for _ in 0..180 {
            h.step();
            now += wants_immediate_repaint(&h) as u32;
        }
        // Frame time of a glide: type a character per frame.
        let t = Instant::now();
        let mut n = 0;
        for _ in 0..30 {
            h.get_by_role(Role::MultilineTextInput).type_text("y");
            h.step();
            n += 1;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        eprintln!("CARET smooth={smooth}: idle 3 s: {now} of 180 frames repaint at once; typing+glide frame {ms:.2} ms");
        if smooth {
            assert!(now < 60, "only the fade windows repaint at once: {now} of 180");
        } else {
            assert_eq!(now, 0, "egui's caret never repaints at once");
        }
    }
}

#[test]
fn switches_off_leave_only_egui_s_caret() {
    for off in [0, 1] {
        let (_fx, mut h) = setup(|c| {
            if off == 0 { c.animations = false } else { c.anim_caret = false }
        });
        h.get_by_role(Role::MultilineTextInput).type_text("X");
        h.step();
        let (p, glide, alpha) = caret(&mut h);
        assert!(!glide && alpha == 0.0 && p == egui::Pos2::ZERO, "our caret never painted (off={off})");
        run(&mut h, 20);
        assert!(!wants_immediate_repaint(&h), "no extra repaint (off={off})");
    }
}

#[test]
fn ime_composition_leaves_the_caret_to_egui() {
    let (_fx, mut h) = setup(|_| {});
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    run(&mut h, 5);
    assert_eq!(caret(&mut h).2, 1.0, "ours is painted");
    h.event(egui::Event::Ime(egui::ImeEvent::Preedit { text: "ka".into(), active_range_chars: None }));
    run(&mut h, 3);
    assert_eq!(caret(&mut h).2, 0.0, "composing: ours is hidden, egui paints the preedit caret");
    h.event(egui::Event::Ime(egui::ImeEvent::Commit("ka".into())));
    run(&mut h, 3);
    assert_eq!(caret(&mut h).2, 1.0, "ours is back after the commit");
}

fn trail(h: &mut H) -> f32 {
    h.state_mut().app().state.ui.editor.caret().trail_len
}

fn glide_frames(h: &mut H) -> usize {
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    (0..60).take_while(|_| {
        h.step();
        caret(h).1
    }).count()
}

#[test]
fn trail_shows_mid_glide_only_and_setting_turns_it_off() {
    for on in [true, false] {
        let (_fx, mut h) = setup(|c| c.caret_trail = on);
        let mut mid = 0.0;
        h.key_press(egui::Key::ArrowLeft); // a short same-row move, inside the snap limit
        for _ in 0..4 {
            h.step();
            mid = trail(&mut h);
            if mid > 0.0 {
                if on {
                    shot(&mut h, "caret_trail_mid");
                }
                break;
            }
        }
        assert_eq!(mid > 0.0, on, "trail={on}: mid-glide trail_len {mid}");
        run(&mut h, 30);
        assert_eq!(trail(&mut h), 0.0, "collapsed after the glide");
    }
}

#[test]
fn caret_is_solid_on_the_frame_after_a_keystroke_and_glide_length_follows_the_setting() {
    let (_fx, mut h) = setup(|_| {});
    for _ in 0..80 {
        h.step();
        if caret(&mut h).2 < 1.0 {
            break;
        }
    }
    assert!(caret(&mut h).2 < 1.0, "blink is in a fade or hidden");
    h.get_by_role(Role::MultilineTextInput).type_text("X");
    h.step();
    assert_eq!(caret(&mut h).2, 1.0, "no fade-in after a keystroke");
    let (_fx2, mut slow) = setup(|c| c.caret_glide_ms = 200);
    let (_fx3, mut fast) = setup(|c| c.caret_glide_ms = 20);
    let (s, f) = (glide_frames(&mut slow), glide_frames(&mut fast));
    assert!(s > f + 5, "200 ms glides longer than 20 ms: {s} vs {f}");
}

/// Replace the note by `text` and park the caret with Ctrl+Home (a snap), then let everything settle.
fn fixture(h: &mut H, text: &str) {
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_role(Role::MultilineTextInput).type_text(text);
    run(h, 30);
}

/// Frames from a move until the caret stops asking for the next frame at once, the largest trail seen,
/// whether a trail was still alive after the head had arrived, and the mean frame time in ms.
fn smear_frames(h: &mut H, key: egui::Key) -> (usize, f32, bool, f64) {
    h.key_press(key);
    let (mut n, mut max, mut after_head) = (0, 0f32, false);
    let t = Instant::now();
    for _ in 0..60 {
        h.step();
        let (_, head, _) = caret(h);
        let tl = trail(h);
        max = max.max(tl);
        after_head |= !head && tl > 0.0;
        if !wants_immediate_repaint(h) {
            break;
        }
        n += 1;
    }
    (n, max, after_head, t.elapsed().as_secs_f64() * 1000.0 / (n.max(1)) as f64)
}

const LONG: &str = "0123456789 0123456789 0123456789 0123456789";

#[test]
fn smear_outlives_the_head_then_repaints_stop() {
    let (_fx, mut h) = setup(|_| {});
    fixture(&mut h, LONG);
    h.key_press(egui::Key::Home);
    run(&mut h, 30);
    let (n, max, after_head, _) = smear_frames(&mut h, egui::Key::End);
    assert!(max > 0.0 && after_head, "the tail still lags once the head has arrived");
    assert!((6..=14).contains(&n), "glide 45 ms + lag 110 ms is about 10 frames at 60 fps: {n}");
    assert_eq!(trail(&mut h), 0.0, "collapsed into the caret");
    assert!(!wants_immediate_repaint(&h), "repaints stopped once the tail arrived");
}

#[test]
fn trail_options_change_the_smear() {
    let frames = |f: fn(&mut iron_sheets_core::config::AppConfig)| {
        let (_fx, mut h) = setup(f);
        fixture(&mut h, LONG);
        h.key_press(egui::Key::Home);
        run(&mut h, 30);
        smear_frames(&mut h, egui::Key::End)
    };
    let default = frames(|_| {});
    let long = frames(|c| c.caret_trail_ms = 300);
    let zero = frames(|c| c.caret_trail_ms = 0);
    let off = frames(|c| c.caret_trail = false);
    assert!(long.0 > default.0 + 5, "300 ms lag repaints longer: {} vs {}", long.0, default.0);
    assert!(zero.1 == 0.0 && off.1 == 0.0, "length 0 and the switch both mean no trail");
    assert!(zero.0 <= 4 && off.0 <= 4, "without a trail only the glide repaints: {} {}", zero.0, off.0);
    let wide = frames(|c| c.caret_trail_width = 1.0);
    assert!(wide.1 > 0.0, "a full-width tail still draws");
    eprintln!("SMEAR frames repainted per move: old glide-only {} | default {} | 300 ms {} | trail off {}; frame ms default {:.2}, off {:.2}", off.0, default.0, long.0, off.0, default.3, off.3);
}

#[test]
fn smear_shots_same_row_and_across_rows() {
    let (_fx, mut h) = setup(|_| {});
    fixture(&mut h, LONG);
    h.key_press(egui::Key::Home);
    run(&mut h, 30);
    h.key_press(egui::Key::End);
    for _ in 0..4 {
        h.step();
    }
    assert!(trail(&mut h) > 0.0);
    shot(&mut h, "caret_smear_same_row");
    fixture(&mut h, &format!("ab\n{LONG}"));
    h.key_press(egui::Key::ArrowUp);
    for _ in 0..4 {
        h.step();
    }
    assert!(trail(&mut h) > 0.0, "one row up is smeared");
    shot(&mut h, "caret_smear_cross_row");
}

#[test]
fn moves_over_three_rows_have_no_smear_and_repaints_stop_after_the_glide() {
    let (_fx, mut h) = setup(|_| {});
    fixture(&mut h, &format!("{LONG}\n1\n2\n3\n4\n5\n6\nend"));
    let y0 = caret(&mut h).0.y;
    let pos = egui::pos2(400.0, 90.0); // the first row: a jump of 7 rows
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    let (mut max, mut n) = (0f32, 0);
    for _ in 0..30 {
        h.step();
        max = max.max(trail(&mut h));
        if !wants_immediate_repaint(&h) {
            break;
        }
        n += 1;
    }
    let dy = y0 - caret(&mut h).0.y;
    // Rows are ~16 points: the caret moved 4+ rows but inside a viewport, so this takes the "too many rows" branch, not a snap.
    assert!(dy > 3.0 * 16.0 && dy < 600.0, "caret moved over more than 3 rows within the viewport: {dy}");
    assert_eq!(max, 0.0, "no trail over 3 rows");
    assert!(n <= 5, "repaints stop right after the glide (about 3 frames), not after the tail lag: {n}");
}
