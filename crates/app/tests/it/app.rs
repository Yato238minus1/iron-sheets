use crate::common::*;
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable;
use iron_sheets_core::config::ViewMode;
use std::time::Duration;

fn click_label(h: &mut egui_kittest::Harness<'static, Slot>, label: &str) {
    h.get_all_by_label(label).next().expect("label present").click();
    settle(h);
}

#[test]
fn starts_and_shows_tree_with_two_roots() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes(), fx.journal()]));
    h.get_by_label("notes");
    h.get_by_label("journal");
    h.get_by_label("Welcome.md");
    assert_eq!(h.state_mut().app().state.workspace.roots.len(), 2);
    // A fresh start is one empty tab: no document, no editor, no preview.
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 1);
    assert!(st.tabs.tabs[0].is_empty() && st.tabs.active_path().is_none());
    shot(&mut h, "01-start-two-roots");
}

#[test]
fn click_opens_file_in_tab_and_navigation_replaces() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Welcome.md");
    {
        let st = &h.state_mut().app().state;
        assert_eq!(st.tabs.tabs.len(), 1);
        assert_eq!(st.tabs.active_path(), Some(fx.notes().join("Welcome.md").as_path()));
    }
    shot(&mut h, "02-file-open-split");
    click_label(&mut h, "Ideas.md");
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 1, "plain click replaces the active tab");
    assert_eq!(st.tabs.tabs[0].history.len(), 2);
}

#[test]
fn typing_marks_dirty_and_autosave_writes() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    // Long enough that a loaded machine cannot autosave before the dirty check below.
    h.state_mut().app().state.config.autosave_delay_ms = 1000;
    click_label(&mut h, "Welcome.md");
    h.get_by_role(Role::MultilineTextInput).type_text("ZZZ");
    settle(&mut h);
    let path = fx.notes().join("Welcome.md");
    {
        let st = &h.state_mut().app().state;
        assert!(st.docs.get(&path).expect("doc").is_dirty());
    }
    shot(&mut h, "03-dirty");
    std::thread::sleep(Duration::from_millis(1100));
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert!(!st.docs.get(&path).expect("doc").is_dirty());
    assert!(std::fs::read_to_string(&path).expect("read").contains("ZZZ"));
}

#[test]
fn ctrl_w_closes_and_ctrl_shift_t_reopens() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Welcome.md");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    settle(&mut h);
    // Closing the last tab leaves one empty tab, never none.
    assert!(h.state_mut().app().state.tabs.tabs.len() == 1 && h.state_mut().app().state.tabs.active_is_empty());
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::T);
    settle(&mut h);
    // The reopened note overwrites the empty tab.
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 1);
    assert!(st.tabs.active_path().is_some_and(|p| p.ends_with("Welcome.md")));
}

static CAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(3);

#[test]
fn session_restore_installs_only_the_active_file() {
    let fx = Fx::new();
    {
        let mut h = harness(fx.opts(vec![fx.notes()]));
        for f in ["Welcome.md", "Ideas.md", "Reading.md"] {
            let st = &mut h.state_mut().app().state;
            st.commands.push(iron_sheets::command::Command::OpenPath { path: fx.notes().join(f), new_tab: true, line: None });
            settle(&mut h);
        }
        h.state_mut().app().on_quit();
    }
    // Unsettled harness: the prefetch of never-shown tabs starts after frame 3, so count exactly three frames first.
    let mut o = fx.opts(vec![]);
    o.workspace = Some("test".into());
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1100.0, 720.0)).with_accessibility_check(false).build_ui_state(
        |ui, slot: &mut Slot| {
            if slot.app.is_none() {
                if let Some(o) = slot.opts.take() {
                    slot.app = Some(iron_sheets::App::new(ui.ctx(), o));
                }
            }
            // One step runs several frames: stop the app at the frame cap instead.
            if let Some(a) = slot.app.as_mut().filter(|a| a.frames < CAP.load(std::sync::atomic::Ordering::Relaxed)) {
                a.frame(ui);
            }
        },
        Slot { opts: Some(o), app: None },
    );
    for _ in 0..4 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(h.state_mut().app().frames, 3);
    let st = &h.state_mut().app().state;
    assert_eq!(st.tabs.tabs.len(), 3);
    assert!(st.docs.loads <= 1, "through frame 3 at most the active file is read: {}", st.docs.loads);
    assert!(st.docs.prefetch.counters().0 <= 1, "no background read of other tabs before frame 3: {:?}", st.docs.prefetch.counters());
    CAP.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
    // Afterwards a bounded background prefetch may read the other tabs, but never installs them.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
        let st = &h.state().app.as_ref().expect("app").state;
        if st.tabs.tabs.iter().filter(|t| st.docs.get(&t.path).is_none()).all(|t| st.docs.prefetch.has(&t.path)) {
            break;
        }
    }
    let st = &h.state_mut().app().state;
    let reads = st.docs.prefetch.counters().0;
    assert!(reads <= st.tabs.tabs.len().min(16), "bounded prefetch reads: {reads}");
    assert!(st.docs.loads <= 1 && st.docs.len() == 1, "only the shown tab's text is installed (loads {}, docs {})", st.docs.loads, st.docs.len());
}

#[test]
fn view_modes_and_divider() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Welcome.md");
    assert_eq!(h.state_mut().app().state.tabs.tabs[0].view, ViewMode::Split);
    h.get_by_label("Preview").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.tabs[0].view, ViewMode::Preview);
    shot(&mut h, "04-preview-only");
    h.get_by_label("Source").click();
    settle(&mut h);
    assert_eq!(h.state_mut().app().state.tabs.tabs[0].view, ViewMode::Source);
    shot(&mut h, "05-source-only");
    h.get_by_label("Split").click();
    settle(&mut h);
    let r = h.get_by_label("Split divider").rect();
    let (from, to) = (r.center(), egui::pos2(r.center().x - 150.0, r.center().y));
    h.event(egui::Event::PointerMoved(from));
    h.step();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.step();
    h.event(egui::Event::PointerMoved(to));
    h.step();
    h.event(egui::Event::PointerMoved(to));
    h.step();
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    settle(&mut h);
    let ratio = h.state_mut().app().state.tabs.tabs[0].split_ratio;
    assert!(ratio < 0.45, "divider dragged left, ratio {ratio}");
    shot(&mut h, "06-split-dragged");
}

#[test]
fn typst_and_image_files_open() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.journal()]));
    click_label(&mut h, "Report.typ");
    shot(&mut h, "07-typst-file");
    click_label(&mut h, "picture.png");
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(20));
        h.step();
    }
    shot(&mut h, "08-image-file");
    assert_eq!(h.state_mut().app().state.tabs.tabs.len(), 1);
}

#[test]
fn new_note_prompt_creates_file() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    h.state_mut().app().state.commands.push(iron_sheets::command::Command::NewNote { dir: None });
    settle(&mut h);
    shot(&mut h, "09-new-note-prompt");
    h.get_by_label("OK").click();
    settle(&mut h);
    assert!(fx.notes().join("Untitled.md").exists());
    assert_eq!(h.state_mut().app().state.tabs.tabs.len(), 1);
}

#[test]
fn external_change_reloads_clean_doc() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Welcome.md");
    let path = fx.notes().join("Welcome.md");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "# Changed outside\n").expect("write");
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        h.step();
        if h.state_mut().app().state.docs.get(&path).is_some_and(|d| d.text.contains("outside")) {
            return;
        }
    }
    panic!("external change was not picked up");
}
