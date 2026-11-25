use crate::common::*;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn setup(text: &str) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join("R.md");
    std::fs::write(&path, text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label("R.md").next().expect("file in tree").click();
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    settle(&mut h);
    h.get_by_label("Regular expression").click();
    settle(&mut h);
    (fx, h, path)
}

#[test]
fn regex_find_and_replace_all_with_groups() {
    let (_fx, mut h, p) = setup("k1=v1\nk2=v2\nplain\n");
    h.get_by_label("Find in note").click();
    h.get_by_label("Find in note").type_text(r"^(\w+)=(\w+)$");
    settle(&mut h);
    shot(&mut h, "find_regex_found");
    assert!(h.query_by_label("1/2").is_some(), "two matches, first current");
    h.get_by_label("Replace with").click();
    h.get_by_label("Replace with").type_text("$2=$1");
    settle(&mut h);
    shot(&mut h, "find_regex_ok");
    h.get_by_label("Replace all").click();
    settle(&mut h);
    let t = h.state_mut().app().state.docs.get(&p).expect("doc").text.clone();
    assert_eq!(t, "v1=k1\nv2=k2\nplain\n");
}

#[test]
fn invalid_regex_shows_the_error_state() {
    let (_fx, mut h, p) = setup("a (b\n");
    h.get_by_label("Find in note").click();
    h.get_by_label("Find in note").type_text("(b");
    settle(&mut h);
    shot(&mut h, "find_regex_error");
    assert!(h.query_by_label("bad regex").is_some());
    h.get_by_label("Replace all").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.docs.get(&p).expect("doc").text, "a (b\n", "nothing replaced");
}
