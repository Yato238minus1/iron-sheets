//! Editor highlighting after edits that open and close fenced code / raw blocks (screenshots for a human to look at).
use crate::common::*;
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn setup(name: &str, text: &str) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join(name);
    std::fs::write(&path, text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.state_mut().app().state.commands.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Source));
    settle(&mut h);
    (fx, h, path)
}

fn edit(h: &mut H, p: &std::path::Path, at: usize, remove: usize, insert: &str) {
    h.state_mut().app().state.commands.push(Command::ApplyEdit { path: p.to_path_buf(), range: at..at + remove, insert: insert.to_string(), select: None });
    settle(h);
}

const MD: &str = "---\ntitle: Demo\n---\n\n# Heading with **bold**\n\nA paragraph with `code`, a [[Wiki|alias]] and $x^2$.\n\n- [ ] task\n- item\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n\n> quote\n\nLast line with ==mark== and #tag.\n";

const TYP: &str = "= Heading\n\nSome *strong* and _emph_ text, `raw`, #link(\"https://x.org\")[a link] and $a^2 + b^2$.\n\n#let f(x) = {\n  x + 1\n}\n\n```rust\nfn main() {}\n```\n\n- item\n\nLast line @ref <lab>.\n";

#[test]
fn markdown_fence_open_close_screens() {
    let (_fx, mut h, p) = setup("Hl.md", MD);
    let before = shot(&mut h, "hl_md_0_initial");
    // open a fence right after the heading: everything below turns into code until the next fence line
    let at = MD.find("A paragraph").expect("anchor");
    edit(&mut h, &p, at, 0, "```\n");
    let opened = shot(&mut h, "hl_md_1_fence_opened");
    edit(&mut h, &p, at, 4, "");
    let closed = shot(&mut h, "hl_md_2_fence_closed");
    let text = h.state_mut().app().state.docs.get(&p).expect("doc").text.clone();
    assert_eq!(text, MD, "edits undone");
    for s in [before, opened, closed] {
        assert!(s.exists());
    }
}

#[test]
fn typst_raw_open_close_screens() {
    let (_fx, mut h, p) = setup("Hl.typ", TYP);
    shot(&mut h, "hl_typ_0_initial");
    let at = TYP.find("Some *strong*").expect("anchor");
    edit(&mut h, &p, at, 0, "```\n");
    shot(&mut h, "hl_typ_1_raw_opened");
    edit(&mut h, &p, at, 4, "");
    shot(&mut h, "hl_typ_2_raw_closed");
    let text = h.state_mut().app().state.docs.get(&p).expect("doc").text.clone();
    assert_eq!(text, TYP);
}
