//! IS-21/IS-22: an expanded empty folder says so, an unreadable folder says why, and a workspace without folders explains itself.
use crate::common::*;
use egui_kittest::kittest::Queryable;
use iron_sheets_core::tree::RowNote;

fn notes_of(h: &mut egui_kittest::Harness<'static, Slot>) -> Vec<RowNote> {
    h.state_mut().app().state.tree.rows().iter().filter_map(|r| r.note.clone()).collect()
}

fn click_label(h: &mut egui_kittest::Harness<'static, Slot>, label: &str) {
    h.get_all_by_label(label).next().expect("label present").click();
    settle(h);
}

#[test]
fn empty_folder_says_so() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.notes().join("Hollow")).expect("mkdir");
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Hollow");
    h.step();
    let notes = notes_of(&mut h);
    assert_eq!(notes, vec![RowNote::Empty]);
    eprintln!("screenshot {}", shot(&mut h, "tree_empty_folder").display());
}

#[cfg(unix)]
#[test]
fn unreadable_folder_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new();
    let locked = fx.notes().join("Locked");
    std::fs::create_dir_all(&locked).expect("mkdir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(locked.clone());
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("skipped: running as root");
        return;
    }
    let mut h = harness(fx.opts(vec![fx.notes()]));
    click_label(&mut h, "Locked");
    h.step();
    let notes = notes_of(&mut h);
    assert!(matches!(notes.as_slice(), [RowNote::Unreadable(e)] if !e.is_empty()), "{notes:?}");
    eprintln!("screenshot {}", shot(&mut h, "tree_unreadable_folder").display());
}

#[test]
fn no_roots_explains_and_offers_add() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![]));
    h.get_by_label("No folders in this workspace yet.");
    assert!(h.get_all_by_label("Add folder").count() >= 1);
    eprintln!("screenshot {}", shot(&mut h, "tree_no_roots").display());
}
