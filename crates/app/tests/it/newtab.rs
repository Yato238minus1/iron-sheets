//! Themes (built-in file theme), professional mode and the new-tab page extras.
use crate::common::*;
use egui::{Role, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;
use iron_sheets::state::AppState;
use std::time::Duration;

type H = Harness<'static, Slot>;

fn st(h: &mut H) -> &mut AppState {
    &mut h.state_mut().app().state
}

fn ready(fx: &Fx, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) -> H {
    let mut o = fx.opts(vec![fx.notes()]);
    o.config.theme = iron_sheets_core::config::ThemePref::Dark;
    f(&mut o.config);
    let mut h = harness(o);
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    h.set_size(vec2(1000.0, 640.0));
    settle(&mut h);
    h
}

fn wait_frames(h: &mut H, n: usize) {
    for _ in 0..n {
        h.step();
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn quotes_file(fx: &Fx) -> String {
    let p = fx.journal().join("quotes.txt");
    std::fs::write(&p, "Only quote here — Tester\n").expect("write");
    p.display().to_string()
}

#[test]
fn newtab_shows_the_quote_and_the_background_image_and_professional_mode_hides_them() {
    let fx = Fx::new();
    let (q, img) = (quotes_file(&fx), fx.journal().join("picture.png").display().to_string());
    let mut h = ready(&fx, |c| {
        c.newtab_quotes_file = q;
        c.newtab_image = img.clone();
    });
    wait_frames(&mut h, 40);
    h.get_by_role_and_label(Role::Label, "Only quote here");
    h.get_by_role_and_label(Role::Label, "— Tester");
    let _ = shot(&mut h, "newtab_quote_image");
    assert!(st(&mut h).status.current().is_none(), "no error for a good image");

    st(&mut h).commands.push(Command::ToggleProfessional);
    wait_frames(&mut h, 4);
    assert!(h.query_by_label("Only quote here").is_none(), "professional mode hides the quote");
    assert_eq!(st(&mut h).config.newtab_image, img, "the stored setting is untouched");
    h.get_by_role_and_label(Role::Button, "Professional mode");
    let size = st(&mut h).config.editor_font_size;
    st(&mut h).commands.push(Command::FontZoom(1));
    wait_frames(&mut h, 3);
    assert_eq!(st(&mut h).config.editor_font_size, size, "font zoom is a no-op in professional mode");
    let _ = shot(&mut h, "newtab_professional");

    st(&mut h).commands.push(Command::ToggleProfessional);
    wait_frames(&mut h, 4);
    h.get_by_role_and_label(Role::Label, "Only quote here");
    assert!(!st(&mut h).config.professional);
}

#[test]
fn a_bad_background_image_reports_once() {
    let fx = Fx::new();
    let mut h = ready(&fx, |c| c.newtab_image = "/no/such/image.png".into());
    wait_frames(&mut h, 10);
    let n = st(&mut h).status.msgs.iter().filter(|m| m.error).count();
    assert_eq!(n, 1);
}

#[test]
fn builtin_theme_is_applied_and_a_bad_theme_file_falls_back_with_an_error() {
    let fx = Fx::new();
    let mut h = ready(&fx, |c| c.theme_file = "sepia".into());
    assert_eq!(st(&mut h).ui.theme.model.ui.edit_bg, iron_sheets::theme_model::Rgba::rgb(0xf4, 0xec, 0xd8));
    assert!(!st(&mut h).ui.theme.dark, "sepia is a light theme");
    let _ = shot(&mut h, "theme_sepia_newtab");

    let fx2 = Fx::new();
    let dir = iron_sheets_core::themes::themes_dir(&fx2.dirs());
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("broken.toml"), "name = \"x\"\nbase = = 1\n").expect("write");
    let mut h = ready(&fx2, |c| c.theme_file = "broken".into());
    let msg = st(&mut h).status.msgs.iter().find(|m| m.error).map(|m| m.text.clone()).unwrap_or_default();
    assert!(msg.contains("broken.toml") && msg.contains("line 2"), "{msg}");
    assert!(st(&mut h).ui.theme.dark, "fell back to the base theme (Dark here)");
}
