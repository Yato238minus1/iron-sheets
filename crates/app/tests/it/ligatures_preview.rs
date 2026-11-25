//! Ligatures module in the Markdown preview: arrows in prose show as Unicode glyphs (display only), code, links and
//! URLs stay as typed, copying gives the typed characters, module off or "Ligatures in preview" off shows no glyph.
//! Screenshots: `ligp_on`, `ligp_off`, `ligp_code_font`.
use crate::common::*;
use egui::{Event, Modifiers, PointerButton, Pos2, pos2};
use egui_kittest::Harness;
use iron_sheets::command::Command;
#[cfg(feature = "font-lilex")]
use iron_sheets::settings;
use iron_sheets::settings::{ITEMS, Section};
use iron_sheets_core::config::ViewMode;

type H = Harness<'static, Slot>;

const NOTE: &str = "a -> b => c != d <=> e <= f >= g\n\nspan `x -> y` end and **`p => q`**\n\n```\nf -> g\n```\n\n[a->b](https://x.test/a->b) and https://x.test/p->q\n";

fn setup(lilex: bool) -> (Fx, H) {
    let fx = Fx::new();
    let p = fx.notes().join("LigP.md");
    std::fs::write(&p, NOTE).expect("write note");
    let mut o = fx.opts(vec![fx.notes()]);
    if lilex {
        o.config.editor_font = "lilex".into();
    }
    // Short frames: egui's double-click delay is 0.3 s.
    let mut h = harness_dt(o, 0.05);
    {
        let st = &mut h.state_mut().app().state;
        st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
        st.commands.push(Command::SetViewMode(ViewMode::Preview));
    }
    settle(&mut h);
    settle(&mut h);
    (fx, h)
}

fn texts(s: &egui::Shape, out: &mut String) {
    match s {
        egui::Shape::Text(t) => {
            out.push_str(t.galley.text());
            out.push('\n');
        }
        egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
        _ => {}
    }
}

/// All text painted in the last frame.
fn shown(h: &mut H) -> String {
    let mut out = String::new();
    h.output().shapes.iter().for_each(|c| texts(&c.shape, &mut out));
    out
}

#[cfg(feature = "font-lilex")]
/// The layout sections of all text painted in the last frame (a split code span has one section per piece).
fn sections(h: &mut H) -> Vec<String> {
    fn walk(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Text(t) => out.extend(t.galley.job.sections.iter().map(|x| t.galley.job.text[x.byte_range.start.0..x.byte_range.end.0].to_string())),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    h.output().shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

#[cfg(feature = "font-lilex")]
fn config(h: &mut H, f: impl FnOnce(&mut iron_sheets_core::config::AppConfig)) {
    let st = &mut h.state_mut().app().state;
    f(&mut st.config);
    settings::apply(st, false);
    settle(h);
}

fn block_rect(h: &mut H, i: usize) -> egui::Rect {
    h.state_mut().app().state.ui.md_preview.block_rects.iter().find(|(k, _)| *k == i).map(|(_, r)| *r).unwrap_or_else(|| panic!("block {i} not painted"))
}

fn drag(h: &mut H, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=4 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
}

fn copied(h: &mut H) -> Option<String> {
    h.event(Event::Copy);
    h.step();
    h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    })
}

// the harness panics on a missing glyph; only the Typst fonts cover the arrows
#[cfg(feature = "typst")]
#[test]
fn prose_shows_glyphs_and_the_rest_stays_as_typed() {
    let (_fx, mut h) = setup(false);
    let t = shown(&mut h);
    assert!(t.contains("a → b ⇒ c ≠ d ⇔ e ≤ f ≥ g"), "{t}");
    assert!(t.contains("x -> y") && t.contains("p => q"), "code spans stay as typed: {t}");
    assert!(t.contains("f -> g"), "code blocks stay as typed: {t}");
    assert!(t.contains("a->b") && t.contains("https://x.test/p->q"), "links and URLs stay as typed: {t}");
    shot(&mut h, "ligp_on");
}

// the harness panics on a missing glyph; only the Typst fonts cover the arrows
#[cfg(feature = "typst")]
#[test]
fn copying_a_preview_selection_gives_the_typed_characters() {
    let (_fx, mut h) = setup(false);
    let r = block_rect(&mut h, 0);
    drag(&mut h, pos2(r.min.x + 1.0, r.center().y), pos2(r.max.x - 1.0, r.center().y));
    let t = copied(&mut h).expect("copy output");
    assert!(t.starts_with("a -> b => c != d <=> e <= f >= g"), "{t:?}");
    assert!(!t.contains('→'), "{t:?}");
}

#[test]
#[cfg(feature = "font-lilex")]
fn module_off_or_preview_option_off_shows_no_glyph() {
    let (_fx, mut h) = setup(true);
    assert!(shown(&mut h).contains("a → b"));
    config(&mut h, |c| c.ligatures_in_preview = false);
    let t = shown(&mut h);
    assert!(t.contains("a -> b => c != d <=> e <= f >= g") && !t.contains('→'), "{t}");
    shot(&mut h, "ligp_off");
    config(&mut h, |c| c.ligatures_in_preview = true);
    assert!(shown(&mut h).contains("a → b"));
    // The module off: no glyph, and the editor stays literal although the font ligates.
    config(&mut h, |c| c.module_ligatures = false);
    let t = shown(&mut h);
    assert!(t.contains("a -> b") && !t.contains('→'), "{t}");
    let th = &h.state_mut().app().state.ui.theme;
    assert!(th.editor.font_ligates && !th.editor.ligatures && !th.preview.subs);
    config(&mut h, |c| c.module_ligatures = true);
    assert!(shown(&mut h).contains("a → b"));
}

#[test]
#[cfg(feature = "font-lilex")]
fn preview_code_uses_the_editor_font_only_when_it_may_ligate() {
    let (_fx, mut h) = setup(true);
    // Bold code uses the editor face: literal (split between `=` and `>`) unless the module and "in code" are on.
    let split = |h: &mut H| sections(h).iter().any(|s| s.ends_with('=') && s != "=") && sections(h).iter().any(|s| s.starts_with('>') && s != ">");
    assert!(split(&mut h), "default: bold code stays literal");
    config(&mut h, |c| c.ligatures_in_preview = false);
    assert!(split(&mut h));
    config(&mut h, |c| c.module_ligatures = false);
    assert!(split(&mut h), "module off: bold code stays literal");
    config(&mut h, |c| c.module_ligatures = true);
    shot(&mut h, "ligp_code_split");
    config(&mut h, |c| c.ligatures_in_code = true);
    assert!(sections(&mut h).iter().any(|s| s == "p => q"), "in code on: one section, the font ligates");
    config(&mut h, |c| c.ligatures_in_code = false);
    let fam = |h: &mut H| h.state_mut().app().state.ui.theme.preview.code_family.clone();
    assert_eq!(fam(&mut h), egui::FontFamily::Monospace, "default: code is literal");
    config(&mut h, |c| c.ligatures_in_code = true);
    assert_eq!(fam(&mut h), egui::FontFamily::Name("editor".into()));
    shot(&mut h, "ligp_code_font");
    config(&mut h, |c| c.module_ligatures = false);
    assert_eq!(fam(&mut h), egui::FontFamily::Monospace);
    let t = shown(&mut h);
    assert!(t.contains("x -> y") && t.contains("f -> g"), "{t}");
}

#[test]
fn the_module_and_its_options_sit_together_in_the_modules_section() {
    let labels: Vec<&str> = ITEMS.iter().filter(|i| i.label.trim_start().starts_with("Ligatures")).map(|i| i.label.trim()).collect();
    assert_eq!(labels, ["Ligatures", "Ligatures in code", "Ligatures in preview"]);
    assert!(ITEMS.iter().filter(|i| i.label.contains("Ligatures")).all(|i| i.section == Section::Modules));
}
