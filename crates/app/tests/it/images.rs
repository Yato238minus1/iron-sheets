//! Image support: the image window from the preview, pasting images into the editor.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use iron_sheets::command::Command;
use iron_sheets_core::config::ViewMode;

type H = Harness<'static, Slot>;

fn prev(h: &mut H) -> &mut iron_sheets::preview::markdown::MarkdownPreview {
    &mut h.state_mut().app().state.ui.md_preview
}

fn click(h: &mut H, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
}

fn open_note(fx: &Fx, text: &str, mode: ViewMode) -> H {
    let p = fx.notes().join("Host.md");
    std::fs::write(&p, text).expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    let st = &mut h.state_mut().app().state;
    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
    st.commands.push(Command::SetViewMode(mode));
    settle(&mut h);
    h
}

/// Steps until `cond` holds (background decoding), at most about 4 s.
fn wait(h: &mut H, what: &str, cond: impl Fn(&mut H) -> bool) {
    for _ in 0..800 {
        h.step();
        if cond(h) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("timeout: {what}");
}

fn image_rect(h: &mut H) -> Option<egui::Rect> {
    prev(h).hits.iter().find(|(_, l)| l == "image").map(|(r, _)| *r)
}

fn hit(h: &mut H, label: &str) -> Option<egui::Rect> {
    prev(h).hits.iter().find(|(_, l)| l == label).map(|(r, _)| *r)
}

/// Hovers `r` so its view buttons show, then clicks the button `label`.
fn press_button(h: &mut H, r: egui::Rect, label: &str) {
    h.event(Event::PointerMoved(r.center()));
    h.step();
    let b = hit(h, label).expect(label);
    click(h, b.center());
}

#[test]
fn image_buttons_open_fullscreen_and_window_and_a_plain_click_opens_nothing() {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy");
    let mut h = open_note(&fx, "# T\n\n![[pic.png]]\n", ViewMode::Preview);
    wait(&mut h, "image painted", |h| image_rect(h).is_some());
    let r = image_rect(&mut h).expect("rect");
    // A click on the picture itself opens nothing; hovering shows the two buttons.
    click(&mut h, r.center());
    assert!(prev(&mut h).image_full.is_none() && prev(&mut h).image_win.is_none(), "plain click opens nothing");
    assert!(hit(&mut h, "view-window").is_some() && hit(&mut h, "view-fullscreen").is_some(), "buttons on hover");
    shot(&mut h, "images-buttons");
    // Fullscreen, closed by Esc.
    press_button(&mut h, r, "view-fullscreen");
    assert!(prev(&mut h).image_full.is_some() && prev(&mut h).image_win.is_none(), "fullscreen opened");
    h.step(); // the overlay is drawn from the frame after the click
    shot(&mut h, "images-fullscreen");
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();
    assert!(prev(&mut h).image_full.is_none(), "Esc closes fullscreen");
    // The window.
    press_button(&mut h, r, "view-window");
    assert!(prev(&mut h).image_win.is_some() && prev(&mut h).image_full.is_none(), "window opened");
    // Wheel over the window zooms; double-click resets. The embedded window is centred on the app window.
    let c = h.ctx.content_rect().center();
    h.event(Event::PointerMoved(c));
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 200.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    });
    for _ in 0..12 {
        h.step();
    }
    let z = prev(&mut h).image_win.as_ref().map(|v| v.view.zoom).expect("open");
    assert!(z > 1.05, "zoomed: {z}");
    shot(&mut h, "images-window-zoomed");
    click(&mut h, c);
    click(&mut h, c);
    assert!(prev(&mut h).image_win.as_ref().is_some_and(|v| (v.view.zoom - 1.0).abs() < 1e-4), "reset by double-click");
    h.key_press(egui::Key::Escape);
    h.step();
    h.step();
    assert!(prev(&mut h).image_win.is_none(), "Esc closes");
}

#[test]
fn pasting_an_image_writes_the_file_inserts_the_embed_and_shows_in_the_preview() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    let fx = Fx::new();
    let note = fx.notes().join("Host.md");
    std::fs::write(&note, "# T\n\nbefore\n").expect("write");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    h.state_mut().app().state.indexer.wait_idle(std::time::Duration::from_secs(10));
    h.get_all_by_label("Host.md").next().expect("in tree").click();
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    let img = egui::ColorImage::filled([8, 6], egui::Color32::from_rgb(200, 40, 40));
    h.event(Event::PasteImage(std::sync::Arc::new(img)));
    wait(&mut h, "embed inserted", |h| h.state_mut().app().state.docs.get(&note).is_some_and(|d| d.text.contains("![[Pasted image ")));
    let text = h.state_mut().app().state.docs.get(&note).expect("doc").text.clone();
    let name = text.split("![[").nth(1).and_then(|s| s.split("]]").next()).expect("name").to_string();
    assert!(fx.notes().join("assets").join(&name).is_file(), "written to <note dir>/assets: {name}");
    h.state_mut().app().state.commands.push(Command::SetViewMode(ViewMode::Preview));
    wait(&mut h, "pasted image resolves in the preview", |h| prev(h).hits.iter().any(|(_, l)| l == "slot"));
    shot(&mut h, "images-pasted");
    // Inline images have the buttons too; a plain click opens nothing.
    let r = prev(&mut h).hits.iter().find(|(_, l)| l == "slot").map(|(r, _)| *r).expect("slot");
    click(&mut h, r.center());
    assert!(prev(&mut h).image_win.is_none() && prev(&mut h).image_full.is_none());
    press_button(&mut h, r, "view-window");
    assert!(prev(&mut h).image_win.is_some());
}

#[test]
fn pasting_into_an_untitled_note_is_refused() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    let fx = Fx::new();
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    h.state_mut().app().state.commands.push(Command::StartUntitled { text: String::new() });
    settle(&mut h);
    h.get_by_role(Role::MultilineTextInput).click();
    settle(&mut h);
    h.event(Event::PasteImage(std::sync::Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::RED))));
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert!(st.status.msgs.iter().any(|m| m.text.contains("Save the note first")));
    assert!(!fx.notes().join("assets").exists() && !fx.home().join("assets").exists());
}

#[test]
fn inline_image_buttons_work_in_a_paragraph_with_a_link() {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy");
    let mut h = open_note(&fx, "# T\n\nSee [[Ideas]] and ![[pic.png]] here.\n", ViewMode::Preview);
    wait(&mut h, "inline image painted", |h| hit(h, "slot").is_some());
    let r = hit(&mut h, "slot").expect("slot");
    // Over the picture the pointer stays an arrow, not a text caret; over the words it is a caret.
    h.event(Event::PointerMoved(egui::pos2(r.center().x, r.max.y - 2.0)));
    h.step();
    h.step();
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::Default, "arrow over the image");
    h.event(Event::PointerMoved(egui::pos2(r.max.x + 20.0, r.center().y)));
    h.step();
    h.step();
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::Text, "caret over the text");
    press_button(&mut h, r, "view-fullscreen");
    assert!(prev(&mut h).image_full.is_some(), "the button wins over the paragraph's link area");
}

#[test]
fn an_inline_image_leaves_no_empty_band_below_it_and_no_caret_beside_short_rows() {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy");
    // The image line continues the list item (no blank line), so the picture is inline in the item's text.
    let mut h = open_note(&fx, "- item\n![[pic.png]]\n\nafter\n", ViewMode::Preview);
    wait(&mut h, "inline image painted", |h| hit(h, "slot").is_some());
    let img = hit(&mut h, "slot").expect("slot");
    let next = prev(&mut h).block_rects.iter().find(|(i, _)| *i == 1).map(|(_, r)| *r).expect("next block");
    shot(&mut h, "images-inline-in-list");
    let gap = next.min.y - img.max.y;
    assert!(gap < img.height() / 3.0, "empty band below the image: {gap} (image {img:?}, next block {next:?})");
    // Right of the short row "after": not over text, so no caret.
    h.event(Event::PointerMoved(egui::pos2(next.max.x - 10.0, next.center().y)));
    h.step();
    h.step();
    assert_ne!(h.output().platform_output.cursor_icon, egui::CursorIcon::Text, "no caret beside a short row");
    h.event(Event::PointerMoved(egui::pos2(next.min.x + 8.0, next.center().y)));
    h.step();
    h.step();
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::Text, "caret over the text");
}

#[test]
fn a_linked_inline_image_has_no_view_buttons() {
    let fx = Fx::new();
    std::fs::copy(fx.journal().join("picture.png"), fx.notes().join("pic.png")).expect("copy");
    let mut h = open_note(&fx, "# T\n\nSee [![x](pic.png)](Ideas.md) here.\n", ViewMode::Preview);
    wait(&mut h, "inline image painted", |h| hit(h, "slot").is_some());
    let r = hit(&mut h, "slot").expect("slot");
    h.event(Event::PointerMoved(r.center()));
    h.step();
    h.step();
    assert!(hit(&mut h, "view-window").is_none(), "the link owns the click");
    assert_eq!(h.output().platform_output.cursor_icon, egui::CursorIcon::PointingHand);
}

#[test]
fn image_preview_error_retry_and_inflight_counter() {
    use egui_kittest::kittest::Queryable;
    let fx = Fx::new();
    let bad = fx.notes().join("bad.png");
    std::fs::write(&bad, b"not a valid png").expect("write bad png");
    let mut h = harness_dt(fx.opts(vec![fx.notes()]), 0.05);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: bad.clone(), new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    wait(&mut h, "image error shown", |h| h.query_by_label("Retry").is_some());
    settle(&mut h);
    assert!(h.query_by_label_contains("Cannot show image:").is_some());
    shot(&mut h, "images-preview-error");

    std::fs::copy(fx.journal().join("picture.png"), &bad).expect("copy valid png");
    h.get_by_label("Retry").click();
    wait(&mut h, "retry succeeded", |h| h.query_by_label("Retry").is_none());
    settle(&mut h);
    shot(&mut h, "images-preview-retried");
    {
        let st = &mut h.state_mut().app().state;
        iron_sheets::perf::collect_inflight(st);
        assert_eq!(st.ui.image_preview.in_flight(), 0);
        assert!(st.perf.inflight.iter().any(|(k, v)| *k == "image/mermaid decodes" && *v == 0));
    }
    settle(&mut h);
    h.step();
    assert!(!wants_immediate_repaint(&h), "idle image preview must not request immediate repaint");
}
