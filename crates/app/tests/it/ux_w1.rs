//! UX wave 1: save-state badge and retry (IS-32), lazy-init cue (IS-02), debug overlay (IS-37), idle repaints (IS-14).
use crate::common::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

type H = Harness<'static, Slot>;

fn ready() -> (Fx, H) {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(20));
    settle(&mut h);
    (fx, h)
}

fn open(h: &mut H, p: std::path::PathBuf) {
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
}

/// Steps frames (with real time passing) until `f` holds or `secs` pass.
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
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn failed_save_is_a_retry_button_that_saves_again() {
    use std::os::unix::fs::PermissionsExt;
    let (fx, mut h) = ready();
    let p = fx.notes().join("Ideas.md");
    open(&mut h, p.clone());
    let set = |mode: u32| {
        for q in [&p, &fx.notes()] {
            std::fs::set_permissions(q, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }
    };
    set(0o555);
    // A read-only dir and file: skip when the process can write anyway (root).
    if std::fs::OpenOptions::new().append(true).open(&p).is_ok() {
        set(0o755);
        return;
    }
    h.state_mut().app().state.docs.get_mut(&p).expect("doc").apply_edit(0..0, "edit ");
    h.state_mut().app().state.commands.push(Command::Save);
    assert!(wait(&mut h, 10, |h| h.query_by_label_contains("Save failed").is_some()), "badge shows the failure");
    shot(&mut h, "ux-save-failed");
    set(0o755);
    h.get_by_label("Save failed – retry").click();
    assert!(wait(&mut h, 10, |h| h.query_by_label_contains("Save failed").is_none()), "the retry saved");
    assert!(std::fs::read_to_string(&p).expect("read").starts_with("edit "), "the edit reached the disk");
    assert!(h.query_all_by_label("Saved").next().is_some());
    shot(&mut h, "ux-save-retried");
}

#[cfg(feature = "typst")]
#[test]
fn typst_warm_up_is_reported_in_the_status_bar() {
    let (_fx, mut h) = ready();
    h.state_mut().app().state.typst.warm();
    let mut seen = false;
    let ok = wait(&mut h, 20, |h| {
        let now = h.query_by_label_contains("Loading Typst").is_some();
        if now && !seen {
            seen = true;
            shot(h, "ux-typst-warming");
        }
        seen && !now
    });
    assert!(seen, "the cue showed while Typst loaded");
    assert!(ok, "and went away when it was ready");
}

#[test]
fn overlay_shows_fps_and_what_is_in_flight() {
    let (fx, mut h) = ready();
    open(&mut h, fx.notes().join("Welcome.md"));
    h.state_mut().app().state.perf.overlay = true;
    h.step();
    h.step();
    h.get_by_label_contains("fps:");
    h.get_by_label_contains("in flight:");
    shot(&mut h, "ux-overlay");
}

#[test]
fn idle_app_asks_for_no_repaint() {
    let (fx, mut h) = ready();
    open(&mut h, fx.notes().join("Welcome.md"));
    #[cfg(feature = "typst")]
    open(&mut h, fx.journal().join("Report.typ"));
    #[cfg(feature = "typst")]
    {
        // Let the first compile and the page upload finish: that is work, not idle.
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            h.step();
            let st = &mut h.state_mut().app().state;
            if st.typst.compiles_pending() == 0 && st.typst.warming().is_none() && !wants_immediate_repaint(&h) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    settle(&mut h);
    // ~5 s of simulated time at 0.25 s per frame. The caret blink is egui's own `request_repaint_after` (never zero).
    let (mut immediate, mut min_delay) = (0, Duration::MAX);
    for _ in 0..20 {
        h.step();
        immediate += usize::from(wants_immediate_repaint(&h));
        for v in h.output().viewport_output.values() {
            min_delay = min_delay.min(v.repaint_delay);
        }
    }
    eprintln!("IDLE 20 frames: immediate repaint requests {immediate}, smallest repaint_after {min_delay:?}");
    assert_eq!(immediate, 0, "no immediate repaint while idle");
}
