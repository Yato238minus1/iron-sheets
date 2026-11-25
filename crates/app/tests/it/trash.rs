//! "Move to trash" dialog: the "Do not show again" checkbox skips later confirmations.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets::command::Command;

#[test]
fn do_not_show_again_skips_the_dialog_next_time() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    // Missing files: the move fails with a status error, so nothing lands in the OS trash.
    let (a, b) = (fx.notes().join("Gone1.md"), fx.notes().join("Gone2.md"));
    h.state_mut().app().state.commands.push(Command::Trash { path: a.clone() });
    settle(&mut h);
    shot(&mut h, "trash_dialog");
    h.get_by_label("Do not show again").click();
    h.step();
    h.get_by_role_and_label(egui::Role::Button, "Move to trash").click();
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert!(!st.config.confirm_trash, "choice stored in the config");
    assert!(st.dialogs.current.is_none());
    h.state_mut().app().state.commands.push(Command::Trash { path: b });
    settle(&mut h);
    let st = &h.state_mut().app().state;
    assert!(st.dialogs.current.is_none(), "no dialog the second time");
    assert!(st.status.current().is_some_and(|m| m.text.starts_with("Cannot move to trash")), "the move was attempted");
}
