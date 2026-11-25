use crate::common::*;
use egui::{OrderedViewportIdMap, ViewportCommand, ViewportId};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use iron_sheets_core::config::AppConfig;

type H = Harness<'static, Slot>;

fn app(on: bool, fx: &Fx) -> H {
    let mut o = fx.opts(vec![fx.notes()]);
    o.config = AppConfig { custom_title_bar: on, autosave: false, ..o.config };
    harness(o)
}

fn commands(h: &H) -> Vec<ViewportCommand> {
    let m: &OrderedViewportIdMap<_> = &h.output().viewport_output;
    m.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

/// Click `label` and return the viewport commands of the frame that handled the click.
fn click(h: &mut H, label: &str) -> Vec<ViewportCommand> {
    h.get_by_label(label).click();
    h.step();
    commands(h)
}

#[test]
fn buttons_exist_only_when_enabled() {
    let fx = Fx::new();
    let h = app(false, &fx);
    assert!(h.query_by_label("Close").is_none() && h.query_by_label("Minimize").is_none() && h.query_by_label("Maximize").is_none());
    let mut h = app(true, &fx);
    h.get_by_label("Minimize");
    h.get_by_label("Maximize");
    h.get_by_label("Close");
    shot(&mut h, "title_bar_on");
}

#[test]
fn buttons_emit_viewport_commands() {
    let fx = Fx::new();
    let mut h = app(true, &fx);
    assert!(click(&mut h, "Minimize").iter().any(|c| matches!(c, ViewportCommand::Minimized(true))));
    assert!(click(&mut h, "Maximize").iter().any(|c| matches!(c, ViewportCommand::Maximized(true))));
}

#[test]
fn close_with_unsaved_note_asks_first() {
    let fx = Fx::new();
    let mut h = app(true, &fx);
    h.get_all_by_label("Welcome.md").next().expect("label present").click();
    settle(&mut h);
    let path = fx.notes().join("Welcome.md");
    h.state_mut().app().state.docs.get_mut(&path).expect("doc").apply_edit(0..0, "typed ");
    h.get_by_label("Close").click();
    settle(&mut h);
    assert!(h.state_mut().app().state.dialogs.current.is_some(), "unsaved-changes dialog");
    assert!(!h.state_mut().app().state.quit_ok);
}

#[test]
fn close_without_changes_quits() {
    let fx = Fx::new();
    let mut h = app(true, &fx);
    h.get_by_label("Close").click();
    settle(&mut h);
    assert!(h.state_mut().app().state.quit_ok);
}
