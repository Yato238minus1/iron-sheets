use crate::common::*;
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use std::path::PathBuf;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn setup(name: &str, text: &str) -> (Fx, H, PathBuf) {
    let fx = Fx::new();
    let path = fx.notes().join(name);
    std::fs::write(&path, text).expect("write note");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.indexer.wait_idle(Duration::from_secs(10));
    h.get_all_by_label(name).next().expect("file in tree").click();
    settle(&mut h);
    (fx, h, path)
}

fn text(h: &mut H, p: &std::path::Path) -> String {
    h.state_mut().app().state.docs.get(p).expect("doc").text.clone()
}

fn select(h: &mut H, range: std::ops::Range<usize>) {
    let st = &mut h.state_mut().app().state;
    if let Some(t) = st.tabs.active_tab_mut() {
        t.pending_cursor = Some(range);
    }
    settle(h);
}

fn key(h: &mut H, k: Key) {
    h.key_press(k);
    settle(h);
}

fn key_mod(h: &mut H, m: Modifiers, k: Key) {
    h.key_press_modifiers(m, k);
    settle(h);
}

#[test]
fn enter_continues_list_and_empty_item_removes_marker() {
    let (_fx, mut h, p) = setup("T.md", "");
    h.get_by_role(Role::MultilineTextInput).type_text("- a");
    settle(&mut h);
    key(&mut h, Key::Enter);
    assert_eq!(text(&mut h, &p), "- a\n- ");
    key(&mut h, Key::Enter);
    let t = text(&mut h, &p);
    assert!(!t.contains("- \n") && !t.ends_with("- "), "marker removed: {t:?}");
    assert!(t.starts_with("- a\n"), "{t:?}");
}

#[test]
fn tab_indents_and_ctrl_shift_b_wraps() {
    let (_fx, mut h, p) = setup("T.md", "- a\n");
    select(&mut h, 3..3);
    key(&mut h, Key::Tab);
    let t = text(&mut h, &p);
    assert!(t.starts_with(' ') || t.starts_with('\t'), "indented: {t:?}");
    assert!(t.contains("- a"), "{t:?}");
    key_mod(&mut h, Modifiers::SHIFT, Key::Tab);
    assert_eq!(text(&mut h, &p), "- a\n", "shift+tab outdents");

    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::ApplyEdit { path: p.clone(), range: 0..4, insert: "hello world".into(), select: Some(0..5) });
    }
    settle(&mut h);
    key_mod(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::B);
    assert_eq!(text(&mut h, &p), "**hello** world");
    key_mod(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::B);
    assert_eq!(text(&mut h, &p), "hello world", "second Ctrl+Shift+B unwraps");
}

#[test]
fn wikilink_autocomplete_inserts_link() {
    let (_fx, mut h, p) = setup("T.md", "");
    h.get_by_role(Role::MultilineTextInput).type_text("[[Id");
    settle(&mut h);
    assert!(h.get_all_by_label("Ideas").count() >= 1, "popup lists Ideas");
    shot(&mut h, "editor-02-wiki-popup");
    key(&mut h, Key::Enter);
    assert_eq!(text(&mut h, &p), "[[Ideas]]");
    // Cursor sits after `]]`: typing continues there.
    h.get_by_role(Role::MultilineTextInput).type_text("!");
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "[[Ideas]]!");
}

#[test]
fn find_navigates_and_replaces() {
    let (_fx, mut h, p) = setup("T.md", "foo bar foo baz foo\n");
    h.state_mut().app().state.find_request = true;
    settle(&mut h);
    let q = h.get_all_by_role(Role::TextInput).next().expect("find field");
    q.type_text("foo");
    settle(&mut h);
    h.get_by_label("1/3");
    shot(&mut h, "editor-03-find");
    key(&mut h, Key::Enter);
    h.get_by_label("2/3");
    key_mod(&mut h, Modifiers::SHIFT, Key::Enter);
    h.get_by_label("1/3");
    key(&mut h, Key::F3);
    h.get_by_label("2/3");
    // Ctrl+F / find_request starts in compact Find-only mode without Replace controls.
    assert!(h.query_by_label("Replace all").is_none());
    h.get_by_label("Replace…").click();
    settle(&mut h);
    // Replace all in one edit.
    let r = h.get_all_by_role(Role::TextInput).nth(1).expect("replace field");
    r.click();
    settle(&mut h);
    h.get_all_by_role(Role::TextInput).nth(1).expect("replace field").type_text("X");
    settle(&mut h);
    h.get_by_label("Replace all").click();
    settle(&mut h);
    assert_eq!(text(&mut h, &p), "X bar X baz X\n");
    // Escape closes the bar.
    key(&mut h, Key::Escape);
    assert!(h.query_by_label("Replace all").is_none());
}

#[test]
fn highlighted_screenshots() {
    let md = "---\ntitle: Demo\n---\n# Heading one\n\nSome **bold**, *italic*, ~~strike~~ and `code` with a [link](https://example.org).\nA wiki link [[Ideas|alias]] and a #tag, math $a^2+b^2$.\n\n> quote\n\n- [ ] task\n- item\n\n```rust\nfn main() {}\n```\n\n<!-- comment -->\n";
    let (_fx, mut h, _p) = setup("Demo.md", md);
    shot(&mut h, "editor-01-highlight-md");
    let typ = "= Heading\n#let x = 3\nSome *strong* and _emph_ text, $x^2 + y$ and #strong[call].\n// comment\n#let s = \"string\"\n";
    let (_fx2, mut h2, _p2) = setup("Demo.typ", typ);
    shot(&mut h2, "editor-04-highlight-typ");
}

#[test]
fn layout_time_100kb() {
    let block = "# Heading\n\nSome **bold** and *italic* text with `code`, a [[Wiki|alias]] and [l](http://x.y).\n\n- item $a^2$\n- item two\n\n```rs\nlet x = 1;\n```\n\n";
    let mut s = String::new();
    while s.len() < 100 * 1024 {
        s.push_str(block);
    }
    let style = iron_sheets::theme::Theme::default().editor;
    let md = || iron_sheets::editor::highlight::layout(&s, iron_sheets_core::FileKind::Markdown, &style, 800.0);
    drop(md());
    let t = std::time::Instant::now();
    let n = 10;
    for _ in 0..n {
        std::hint::black_box(md());
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("layout() 100 KB markdown: {per:.2} ms ({} bytes)", s.len());
}
