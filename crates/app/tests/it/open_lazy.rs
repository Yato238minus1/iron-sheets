//! Opening a note never reads it on the UI thread: the click frame only creates the tab.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

/// Big enough that a synchronous read would show in the click frame (debug build), small enough that four test
/// threads each holding the note several times (doc, prefetch, index) stay well under the test memory cap.
const BIG_MB: usize = 4;

fn big_note(mb: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < mb * 1024 * 1024 {
        s.push_str(&format!("## Section {i}\n\nParagraph {i} lorem ipsum [[Other]] dolor sit amet.\n\n"));
        i += 1;
    }
    s
}

struct Setup {
    _fx: Fx,
    h: egui_kittest::Harness<'static, Slot>,
    big: std::path::PathBuf,
}

fn setup() -> Setup {
    let fx = Fx::new();
    let big = fx.notes().join("Bigone.md");
    std::fs::write(&big, format!("# Bigone\n\n{}", big_note(BIG_MB))).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(20));
    settle(&mut h);
    Setup { _fx: fx, h, big }
}

fn click_frame(s: &mut Setup, cmd: Command) -> (usize, Duration) {
    s.h.state_mut().app().state.commands.push(cmd);
    let t = Instant::now();
    s.h.step();
    let d = t.elapsed();
    (s.h.state_mut().app().state.docs.sync_loads, d)
}

#[test]
fn open_path_reads_nothing_in_the_click_frame() {
    let mut s = setup();
    let p = s.big.clone();
    let (loads, d) = click_frame(&mut s, Command::OpenPath { path: p.clone(), new_tab: true, line: Some(40) });
    eprintln!("OpenPath {BIG_MB} MB: sync loads {loads}, click frame {d:?}");
    assert_eq!(loads, 0, "no synchronous read in the click frame");
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    assert!(st.docs.get(&p).is_some(), "the note arrived");
    assert_eq!(st.docs.sync_loads, 0);
    let t = st.tabs.active_tab().expect("tab");
    assert_eq!(t.path, p);
    assert!(t.pending_line.is_none(), "pending line was applied");
}

#[test]
fn wiki_link_reads_nothing_in_the_click_frame() {
    let mut s = setup();
    let (loads, d) = click_frame(&mut s, Command::OpenLink { note: "Bigone".into(), heading: None, from: None, new_tab: true });
    eprintln!("OpenLink {BIG_MB} MB: sync loads {loads}, click frame {d:?}");
    assert_eq!(loads, 0);
    settle(&mut s.h);
    let p = s.big.clone();
    assert!(s.h.state_mut().app().state.docs.get(&p).is_some());
}

#[test]
fn open_error_keeps_the_lazy_tab_in_a_failed_state() {
    let mut s = setup();
    let missing = s.big.with_file_name("Nope.md");
    click_frame(&mut s, Command::OpenPath { path: missing.clone(), new_tab: true, line: None });
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    let t = st.tabs.tabs.iter().find(|t| t.path == missing).expect("the tab stays");
    assert!(t.load_error.is_some(), "tab carries the reason");
}

#[test]
fn wiki_link_does_not_wait_for_a_busy_index() {
    let mut s = setup();
    let idx = s.h.state_mut().app().state.indexer.index();
    let guard = idx.write();
    s.h.state_mut().app().state.commands.push(Command::OpenLink { note: "Bigone".into(), heading: None, from: None, new_tab: true });
    let t = Instant::now();
    s.h.step();
    s.h.step();
    assert!(t.elapsed() < Duration::from_millis(150), "frames do not wait for the index lock: {:?}", t.elapsed());
    assert!(s.h.state_mut().app().state.tabs.find(&s.big).is_none(), "not opened while the index is busy");
    drop(guard);
    settle(&mut s.h);
    let big = s.big.clone();
    assert!(s.h.state_mut().app().state.docs.get(&big).is_some(), "retried once the index was free");
}

#[test]
fn wiki_link_never_blocks_a_frame_however_long_the_index_is_busy() {
    let mut s = setup();
    let idx = s.h.state_mut().app().state.indexer.index();
    let big = s.big.clone();
    let opener = std::thread::spawn({
        let idx = idx.clone();
        move || {
            let _g = idx.write();
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    std::thread::sleep(Duration::from_millis(50));
    s.h.state_mut().app().state.commands.push(Command::OpenLink { note: "Bigone".into(), heading: None, from: None, new_tab: true });
    let (start, mut worst, mut shot_taken) = (Instant::now(), Duration::ZERO, false);
    while s.h.state_mut().app().state.tabs.find(&big).is_none() && start.elapsed() < Duration::from_secs(5) {
        let t = Instant::now();
        s.h.step();
        worst = worst.max(t.elapsed());
        if !shot_taken && s.h.state_mut().app().state.pending_link.is_some() {
            s.h.get_by_label_contains("Opening link");
            shot(&mut s.h, "open-link-pending");
            shot_taken = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    opener.join().expect("join");
    eprintln!("link while index busy 500 ms: worst frame {worst:?}");
    assert!(shot_taken, "the click was pending, with a status cue");
    assert!(worst < Duration::from_millis(300), "no frame waited for the index lock: {worst:?}");
    assert!(s.h.state_mut().app().state.tabs.find(&big).is_some(), "the click is never lost: it opens once the lock is free");
    settle(&mut s.h);
    assert!(s.h.query_by_label_contains("Opening link").is_none(), "cue gone after opening");
}

#[test]
fn quick_open_says_it_is_indexing_while_the_index_is_busy() {
    let mut s = setup();
    let idx = s.h.state_mut().app().state.indexer.index();
    let guard = idx.write();
    {
        let st = &mut s.h.state_mut().app().state;
        st.indexer.rescan();
        st.commands.push(Command::QuickOpen);
    }
    s.h.step();
    s.h.step();
    let scanning = s.h.state_mut().app().state.indexer.status().scanning;
    assert!(scanning, "the rescan waits for the lock we hold");
    s.h.get_by_label_contains("Indexing...");
    shot(&mut s.h, "open-fast-palette-indexing");
    drop(guard);
    settle(&mut s.h);
}

#[test]
fn failed_replacing_open_keeps_the_previous_note() {
    let mut s = setup();
    let x = s._fx.notes().join("Ideas.md");
    click_frame(&mut s, Command::OpenPath { path: x.clone(), new_tab: true, line: None });
    settle(&mut s.h);
    let missing = s.big.with_file_name("Nope.md");
    // new_tab: false replaces the active clean tab before the load result is known.
    click_frame(&mut s, Command::OpenPath { path: missing.clone(), new_tab: false, line: None });
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    assert_eq!(st.tabs.active_path(), Some(x.as_path()), "the replaced note is back");
    assert!(st.tabs.find(&missing).is_none());
    assert!(st.status.msgs.last().is_some_and(|m| m.error && m.text.contains("Cannot open")), "an error is shown");
}

#[test]
fn a_new_link_click_drops_the_older_waiting_one() {
    let mut s = setup();
    let idx = s.h.state_mut().app().state.indexer.index();
    let guard = idx.write();
    s.h.state_mut().app().state.commands.push(Command::OpenLink { note: "Ideas".into(), heading: None, from: None, new_tab: true });
    s.h.step();
    drop(guard);
    // The index is free again: B opens at once, and A (still waiting) must not follow it.
    s.h.state_mut().app().state.commands.push(Command::OpenLink { note: "Bigone".into(), heading: None, from: None, new_tab: true });
    s.h.step();
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    let ideas = st.tabs.tabs.iter().any(|t| t.path.ends_with("Ideas.md"));
    assert!(!ideas, "only the last click opens");
    assert!(st.tabs.tabs.iter().any(|t| t.path.ends_with("Bigone.md")));
}

#[test]
fn switching_tabs_drops_a_waiting_link() {
    let mut s = setup();
    let idx = s.h.state_mut().app().state.indexer.index();
    let guard = idx.write();
    {
        let st = &mut s.h.state_mut().app().state;
        st.commands.push(Command::NewTab);
    }
    s.h.step();
    s.h.state_mut().app().state.commands.push(Command::OpenLink { note: "Ideas".into(), heading: None, from: None, new_tab: true });
    s.h.step();
    s.h.state_mut().app().state.commands.push(Command::NewTab);
    s.h.state_mut().app().state.commands.push(Command::ActivateTab(0));
    s.h.step();
    drop(guard);
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    assert!(st.pending_link.is_none());
    assert!(!st.tabs.tabs.iter().any(|t| t.path.ends_with("Ideas.md")), "the late link did not open");
}

#[test]
fn history_back_big_note_is_non_blocking_and_restores_cursor() {
    let mut s = setup();
    let big = s.big.clone();
    let ideas = s._fx.notes().join("Ideas.md");
    click_frame(&mut s, Command::OpenPath { path: big.clone(), new_tab: true, line: None });
    settle(&mut s.h);
    {
        let st = &mut s.h.state_mut().app().state;
        st.tabs.tabs[0].cursor = Some(25..25);
    }
    click_frame(&mut s, Command::OpenPath { path: ideas.clone(), new_tab: false, line: None });
    settle(&mut s.h);
    let sync_before = s.h.state_mut().app().state.docs.sync_loads;
    s.h.state_mut().app().state.docs.prefetch.set_open_paused(true);
    let (loads, _) = click_frame(&mut s, Command::HistoryBack);
    assert_eq!(loads, sync_before, "HistoryBack must not sync-load");
    assert!(s.h.state_mut().app().state.is_active_tab_loading(), "loading skeleton while prefetch paused");
    s.h.state_mut().app().state.docs.prefetch.set_open_paused(false);
    settle(&mut s.h);
    let st = &mut s.h.state_mut().app().state;
    assert_eq!(st.docs.sync_loads, sync_before);
    assert_eq!(st.tabs.active_path(), Some(big.as_path()));
    assert!(st.docs.get(&big).is_some());
    assert_eq!(st.tabs.tabs[0].cursor, Some(25..25), "cursor restored on history back");
}
