use crate::common::*;
use iron_sheets::command::Command;
use std::time::Duration;

fn open(h: &mut egui_kittest::Harness<'static, Slot>, p: std::path::PathBuf) {
    h.state_mut().app().state.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    settle(h);
}

#[test]
fn restored_cursor_inside_multibyte_char_does_not_panic() {
    let fx = Fx::new();
    let f = fx.notes().join("Cjk.md");
    std::fs::write(&f, "abcdefgh\n").expect("write");
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        open(&mut h, f.clone());
        h.state_mut().app().state.tabs.tabs[0].cursor = Some(5..5);
        h.state_mut().app().on_quit();
    }
    // The file changed while the app was closed: byte 5 is now inside the second byte of a char.
    std::fs::write(&f, "abcd\u{e9}\u{e9}\u{e9}\n").expect("write");
    let mut o = fx.opts(vec![]);
    o.workspace = Some("test".into());
    let mut h = harness(o);
    settle(&mut h);
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 1);
}

#[test]
fn autosave_does_not_overwrite_external_change() {
    let fx = Fx::new();
    let f = fx.notes().join("Welcome.md");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open(&mut h, f.clone());
    {
        let st = &mut h.state_mut().app().state;
        let d = st.docs.get_mut(&f).expect("doc");
        assert!(d.apply_edit(0..0, "MINE"));
        d.external_change = true;
    }
    std::fs::write(&f, "THEIRS").expect("write");
    std::thread::sleep(Duration::from_millis(300));
    settle(&mut h);
    assert_eq!(std::fs::read_to_string(&f).expect("read"), "THEIRS");
    assert!(h.state_mut().app().state.docs.get(&f).expect("doc").is_dirty());
}

#[test]
fn folder_rename_keeps_unsaved_edits() {
    let fx = Fx::new();
    let dir = fx.notes().join("sub");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let f = dir.join("a.md");
    std::fs::write(&f, "one\n").expect("write");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open(&mut h, f.clone());
    {
        let st = &mut h.state_mut().app().state;
        st.config.autosave = false;
        assert!(st.docs.get_mut(&f).expect("doc").apply_edit(0..0, "EDIT"));
        st.commands.push(Command::PromptSubmit(iron_sheets::dialogs::PromptAction::Rename { path: dir.clone() }, "sub2".into()));
    }
    settle(&mut h);
    let to = fx.notes().join("sub2").join("a.md");
    let st = &h.state_mut().app().state;
    let d = st.docs.get(&to).expect("doc re-keyed");
    assert!(d.is_dirty() && d.text.starts_with("EDIT"));
    assert!(st.docs.get(&f).is_none());
    assert_eq!(st.tabs.tabs[0].path, to);
}

#[test]
fn discard_switches_workspace() {
    let fx = Fx::new();
    let f = fx.notes().join("Welcome.md");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    open(&mut h, f.clone());
    {
        let st = &mut h.state_mut().app().state;
        st.config.autosave = false;
        assert!(st.docs.get_mut(&f).expect("doc").apply_edit(0..0, "X"));
        st.commands.push(Command::SwitchWorkspaceForce("Default".into()));
    }
    settle(&mut h);
    assert!(h.state_mut().app().state.docs.dirty_paths().is_empty());
}
