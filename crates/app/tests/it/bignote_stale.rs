//! A note that is closed, changed on disk and opened again must not show the preview of the old text.
//! Also tests the >150ms "Updating preview…" staleness cue without layout shift (IS-10).
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

#[test]
fn reopened_note_is_parsed_again() {
    let fx = Fx::new();
    let p = fx.notes().join("Reopen.md");
    std::fs::write(&p, "# Old heading\n\n[old](https://old.test)\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    assert!(h.state_mut().app().state.ui.md_preview.hits.iter().any(|(_, l)| l == "link:https://old.test"), "old text shown first");
    h.state_mut().app().state.commands.push(Command::CloseActiveTab);
    settle(&mut h);
    std::fs::write(&p, "# New heading\n\n[new](https://new.test)\n").expect("write");
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p.clone(), new_tab: true, line: None });
    settle(&mut h);
    let st = &mut h.state_mut().app().state;
    assert_eq!(st.docs.get(&p).map(|d| d.text.clone()).as_deref(), Some("# New heading\n\n[new](https://new.test)\n"));
    // The shown preview is of the new text, whether its parse came from the background open or the preview itself.
    let hits: Vec<&str> = st.ui.md_preview.hits.iter().map(|(_, l)| l.as_str()).collect();
    assert!(hits.contains(&"link:https://new.test") && !hits.contains(&"link:https://old.test"), "{hits:?}");
}

#[test]
fn stale_big_note_shows_updating_overlay_without_layout_shift() {
    let fx = Fx::new();
    let big = fx.notes().join("BigStale.md");
    let small = fx.notes().join("SmallClean.md");
    let big_body = "# Big Note\n\n".to_string() + &"Paragraph with **markdown** text and items.\n\n".repeat(5200);
    assert!(big_body.len() > 210 * 1024);
    std::fs::write(&big, &big_body).expect("write big");
    std::fs::write(&small, "# Small\n\nShort text.\n").expect("write small");

    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: small.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: small, range: 0..0, insert: "Edit\n".into(), select: None });
    settle(&mut h);
    assert!(h.query_by_label_contains("Updating preview").is_none(), "small notes never show stale overlay");

    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: big.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    let tops_before = h.state_mut().app().state.ui.md_preview.tops_of(&big).expect("tops").to_vec();

    h.state_mut().app().state.ui.md_preview.set_parse_paused(true);
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: big.clone(), range: 0..0, insert: "Top ".into(), select: None });
    h.step();
    // Wait past DEBOUNCE (150ms) so update_parse submits to the paused worker.
    std::thread::sleep(Duration::from_millis(180));
    h.step();
    // Wait past STALE_CUE_AFTER (150ms) so the "Updating preview…" overlay appears.
    std::thread::sleep(Duration::from_millis(180));
    h.step();
    h.step();

    assert!(h.query_by_label_contains("Updating preview").is_some(), "stale cue must appear after 150ms");
    let tops_during = h.state_mut().app().state.ui.md_preview.tops_of(&big).expect("tops").to_vec();
    assert_eq!(tops_before, tops_during, "overlay must not shift preview block layout");
    {
        let st = &mut h.state_mut().app().state;
        iron_sheets::perf::collect_inflight(st);
        assert_eq!(st.perf.inflight.iter().find(|(k, _)| *k == "md parses").map(|(_, v)| *v), Some(1));
    }
    shot(&mut h, "md-updating-cue");

    h.state_mut().app().state.ui.md_preview.set_parse_paused(false);
    let t0 = std::time::Instant::now();
    while h.state_mut().app().state.ui.md_preview.in_flight_parses() > 0 && t0.elapsed() < Duration::from_secs(5) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&mut h);
    assert!(h.query_by_label_contains("Updating preview").is_none(), "cue clears after parse finishes");
    {
        let st = &mut h.state_mut().app().state;
        iron_sheets::perf::collect_inflight(st);
        assert_eq!(st.perf.inflight.iter().find(|(k, _)| *k == "md parses").map(|(_, v)| *v), Some(0));
    }
    h.step();
    assert!(!wants_immediate_repaint(&h));
}
