//! IS-23: an open note deleted on disk gets a cue and is not recreated by autosave; a changed clean note reloads in place.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use std::time::Duration;

fn click_label(h: &mut egui_kittest::Harness<'static, Slot>, label: &str) {
    h.get_all_by_label(label).next().expect("label present").click();
    settle(h);
}

fn steps(h: &mut egui_kittest::Harness<'static, Slot>, mut until: impl FnMut(&mut egui_kittest::Harness<'static, Slot>) -> bool) -> bool {
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
        if until(h) {
            return true;
        }
    }
    false
}

#[test]
fn deleted_open_note_shows_cue_and_is_not_recreated_until_save_again() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Welcome.md");
    let path = fx.notes().join("Welcome.md");
    std::fs::remove_file(&path).expect("remove");
    let p = path.clone();
    assert!(steps(&mut h, |h| h.state_mut().app().state.docs.get(&p).is_some_and(|d| d.deleted)), "deletion noticed");
    h.step();
    h.get_by_label("Deleted on disk");
    // Edit: autosave (50 ms) must not bring the file back.
    h.state_mut().app().state.docs.get_mut(&path).expect("doc").apply_edit(0..0, "typed ");
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(60));
        h.step();
    }
    assert!(!path.exists(), "autosave must not recreate a deleted file");
    let s = shot(&mut h, "deleted_open");
    eprintln!("screenshot {}", s.display());
    h.get_by_label("Save again").click();
    let p = path.clone();
    assert!(steps(&mut h, |_| p.exists()), "Save again writes the file");
    assert!(std::fs::read_to_string(&path).expect("read").starts_with("typed "));
    h.step();
    assert!(!h.state_mut().app().state.docs.get(&path).expect("doc").deleted);
}

#[test]
fn changed_clean_note_reloads_and_keeps_the_scroll_offset() {
    let fx = Fx::new();
    let path = fx.notes().join("Long.md");
    let body = |tag: &str| (0..400).map(|i| format!("line {i} {tag}\n")).collect::<String>();
    std::fs::write(&path, body("a")).expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(20));
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    settle(&mut h);
    h.state_mut().app().state.tabs.active_tab_mut().expect("tab").editor_scroll.force = Some(900.0);
    h.step();
    h.step();
    let before = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.offset;
    assert!(before > 100.0, "scrolled: {before}");
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(&path, body("b")).expect("write");
    let p = path.clone();
    assert!(steps(&mut h, |h| h.state_mut().app().state.docs.get(&p).is_some_and(|d| d.text.contains("line 0 b"))), "reloaded");
    h.step();
    let after = h.state_mut().app().state.tabs.active_tab().expect("tab").editor_scroll.offset;
    assert_eq!(before, after, "reload keeps the editor scroll offset");
}

fn open_deleted(fx: &Fx, rel: &str) -> (egui_kittest::Harness<'static, Slot>, std::path::PathBuf) {
    let path = fx.notes().join(rel);
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    settle(&mut h);
    h.state_mut().app().state.docs.get_mut(&path).expect("doc").apply_edit(0..0, "typed ");
    h.state_mut().app().state.docs.get_mut(&path).expect("doc").deleted = true; // as `handle_fs` sets it
    (h, path)
}

#[test]
fn closing_a_dirty_deleted_note_asks_instead_of_recreating() {
    let fx = Fx::new();
    let (mut h, path) = open_deleted(&fx, "Welcome.md");
    std::fs::remove_file(&path).expect("remove");
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::CloseActiveTab);
    settle(&mut h);
    assert!(h.state_mut().app().state.dialogs.current.is_some(), "Unsaved dialog shown");
    assert!(!path.exists(), "close must not recreate the file");
}

#[test]
fn ctrl_s_recreates_a_deleted_note_and_clears_the_cue() {
    let fx = Fx::new();
    let (mut h, path) = open_deleted(&fx, "Welcome.md");
    std::fs::remove_file(&path).expect("remove");
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::Save);
    assert!(steps(&mut h, |_| path.exists()), "file recreated");
    assert!(steps(&mut h, |h| !h.state_mut().app().state.docs.get(&path).expect("doc").deleted), "cue cleared");
}

#[test]
fn save_again_keeps_the_cue_when_the_write_fails() {
    let fx = Fx::new();
    let (mut h, path) = open_deleted(&fx, "Projects/Plan.md");
    std::fs::remove_dir_all(fx.notes().join("Projects")).expect("remove dir");
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::SaveAgain(path.clone()));
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
    }
    assert!(!path.exists());
    assert!(h.state_mut().app().state.docs.get(&path).expect("doc").deleted, "cue stays after a failed write");
}
