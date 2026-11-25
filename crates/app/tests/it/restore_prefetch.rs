//! Restored tabs that are not shown yet are read in the background one by one, so switching to them shows no skeleton.
use crate::common::*;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

#[test]
fn restored_tabs_are_prefetched_in_the_background() {
    let fx = Fx::new();
    let names = ["T0.md", "T1.md", "T2.md", "T3.md"];
    for n in names {
        std::fs::write(fx.notes().join(n), format!("# {n}\n\n{}", "lorem ipsum dolor sit amet\n".repeat(60_000))).expect("write");
    }
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        for n in names {
            h.state_mut().app().state.commands.push(Command::OpenPath { path: fx.notes().join(n), new_tab: true, line: None });
            settle(&mut h);
        }
        h.state_mut().app().on_quit();
    }
    let mut o = fx.opts(vec![]);
    o.workspace = Some("test".into());
    let mut h = harness(o);
    settle(&mut h);
    // Idle frames: the worker reads the other tabs, one at a time.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
        let st = &h.state().app.as_ref().expect("app").state;
        if st.tabs.tabs.iter().filter(|t| st.docs.get(&t.path).is_none()).all(|t| st.docs.prefetch.has(&t.path)) {
            break;
        }
    }
    let n = h.state().app.as_ref().expect("app").state.tabs.tabs.len();
    assert_eq!(n, 4);
    let mut skeleton_frames = 0;
    for i in 0..n {
        h.state_mut().app().state.commands.push(Command::ActivateTab(i));
        h.step();
        let st = &h.state().app.as_ref().expect("app").state;
        if st.is_active_tab_loading() {
            skeleton_frames += 1;
        }
        settle(&mut h);
    }
    eprintln!("skeleton frames on first switch to each restored tab: {skeleton_frames}");
    assert_eq!(skeleton_frames, 0);
}
