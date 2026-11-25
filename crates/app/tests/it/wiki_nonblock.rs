//! The wiki-link autocomplete must never make an editor frame wait for the index (a rescan holds its write lock).
use crate::common::*;
use iron_sheets::command::Command;
use std::time::{Duration, Instant};

#[test]
fn editor_frame_does_not_block_while_the_index_is_write_locked() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    let p = fx.notes().join("Welcome.md");
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(&mut h);
    let idx = h.state_mut().app().state.indexer.index();
    let held = std::thread::spawn(move || {
        let _g = idx.write();
        std::thread::sleep(Duration::from_millis(300));
    });
    std::thread::sleep(Duration::from_millis(30));
    let t = Instant::now();
    for _ in 0..3 {
        h.step();
    }
    let ms = t.elapsed().as_secs_f64() * 1e3;
    held.join().expect("lock thread");
    println!("3 frames under a held index write lock: {ms:.1} ms");
    assert!(ms < 150.0, "3 editor frames took {ms:.1} ms while the index was write-locked");
}
