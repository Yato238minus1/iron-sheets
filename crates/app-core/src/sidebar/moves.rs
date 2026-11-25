//! Renaming and moving files and folders, with the links to them rewritten (see `iron_sheets_core::relink`).
use crate::command::Command;
use crate::state::{AppState, CoreMut};
use iron_sheets_core::ops;
use iron_sheets_core::relink::{self, Plan};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a move changed: sent back when the notes that were closed have been rewritten on a background thread.
pub struct Done {
    /// Start of the notice ("Moved a to b"), empty for a plain rename.
    prefix: String,
    undo: Option<Command>,
    links: usize,
    notes: usize,
    failed: Vec<(PathBuf, String)>,
    /// Closed notes written on the background thread (their paths after the move): open ones must catch up.
    written: Vec<PathBuf>,
    /// The index was still scanning when the plan was made: links may have been missed.
    incomplete: bool,
}

/// A move waiting for the answer to "Update links?", with the plan made for the question.
pub struct Pending {
    from: PathBuf,
    to: PathBuf,
    plan: Plan,
    incomplete: bool,
}

/// Byte range of `old` that `new` replaces, and the replacement: what is left after the common start and end.
fn changed_range<'a>(old: &str, new: &'a str) -> (Range<usize>, &'a str) {
    let mut p = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(p) || !new.is_char_boundary(p) {
        p -= 1;
    }
    let max = old.len().min(new.len()) - p;
    let mut s = old.bytes().rev().zip(new.bytes().rev()).take(max).take_while(|(a, b)| a == b).count();
    while !old.is_char_boundary(old.len() - s) || !new.is_char_boundary(new.len() - s) {
        s -= 1;
    }
    (p..old.len() - s, &new[p..new.len() - s])
}

const SKIPPED: &str = "a link was left unchanged: the new name has # | [ ] ^ or < >";

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Whether moving `from` into the folder `dir` is allowed: not onto its own parent, not into itself.
pub fn can_drop(from: &Path, dir: &Path) -> bool {
    from.parent() != Some(dir) && !dir.starts_with(from)
}

/// Move or rename `from` to `to` (both full paths). `links`: `Some` = the user decided about the links, `None` = decide by
/// the setting (and ask). `dnd`: a drag and drop move, answered with a notice that offers Undo.
pub fn mv<U>(state: &mut AppState<U>, from: PathBuf, to: PathBuf, links: Option<bool>, dnd: bool) {
    mv_inner(&mut state.core_mut(), from, to, links, dnd);
}

pub(crate) fn mv_inner(state: &mut CoreMut<'_>, from: PathBuf, to: PathBuf, links: Option<bool>, dnd: bool) {
    if from == to {
        return;
    }
    if to.starts_with(&from) {
        return state.status.error("Cannot move a folder into itself");
    }
    // The plan made for the question is the one the answer uses.
    let pending = state.pending_move.take().filter(|p| p.from == from && p.to == to && links.is_some());
    let (plan, incomplete) = match pending {
        Some(p) => (p.plan, p.incomplete),
        None => {
            let incomplete = state.indexer.status().scanning;
            let index = state.indexer.index();
            let g = index.read();
            (Plan::build(&g, &from, &to), incomplete)
        }
    };
    let update = match links {
        Some(b) => b,
        None => match state.config.update_links.as_str() {
            "never" => false,
            "always" => true,
            _ if plan.linking == 0 => true,
            _ => {
                let msg = format!("Update links in {}?", plural(plan.linking, "note"));
                let ok = Command::Move { from: from.clone(), to: to.clone(), links: Some(true), dnd };
                let alt = Command::Move { from: from.clone(), to: to.clone(), links: Some(false), dnd };
                *state.pending_move = Some(Pending { from, to, plan, incomplete });
                return state.dialogs.confirm_alt("Update links", msg, "Update", vec![ok], "Don't update", vec![alt]);
            }
        },
    };
    state.docs.wait_saved_under(&from);
    if let Err(e) = ops::rename(&from, &to) {
        return state.status.error(format!("Cannot move: {e}"));
    }
    state.docs.rename(&from, &to);
    state.tabs.rename_path(&from, &to);
    let parent = |p: &Path| p.parent().map(Path::to_path_buf).unwrap_or_default();
    state.tree.invalidate(&parent(&from));
    state.tree.invalidate(&parent(&to));
    state.indexer.file_changed(from.clone());
    state.indexer.file_changed(to.clone());
    state.modules.on_save(&from);
    state.modules.on_save(&to);
    let mut done = Done { prefix: String::new(), undo: None, links: 0, notes: 0, failed: Vec::new(), written: Vec::new(), incomplete };
    if dnd {
        state.tree.set_expanded(&parent(&to), true);
        state.tree.reveal(&to);
        *state.reveal_request = Some(to.clone());
        done.prefix = format!("Moved {} to {}", name_of(&from), name_of(&parent(&to)));
        done.undo = Some(Command::Move { from: to.clone(), to: from.clone(), links: Some(update), dnd: false });
    }
    let mut closed = Vec::new();
    if update {
        for old in &plan.notes {
            match state.docs.get_mut(&plan.new_of(old)) {
                // An open note is edited in its buffer: undoable, and unsaved changes stay.
                Some(doc) if !doc.read_only => {
                    let r = relink::rewrite(&doc.text, old, &plan);
                    if r.skipped > 0 {
                        done.failed.push((doc.path.clone(), SKIPPED.into()));
                    }
                    if let Some(text) = r.text {
                        let (range, insert) = changed_range(&doc.text, &text);
                        if doc.apply_edit(range, insert) {
                            done.links += r.links;
                            done.notes += 1;
                        } else {
                            done.failed.push((doc.path.clone(), "could not edit".into()));
                        }
                    }
                }
                Some(doc) => done.failed.push((doc.path.clone(), "read-only".into())),
                None => closed.push(old.clone()),
            }
        }
    }
    if closed.is_empty() {
        return finish(state, done);
    }
    let (tx, plan, waker) = (state.relink_tx.clone(), Arc::new(plan), state.waker.clone());
    std::thread::spawn(move || {
        for old in closed {
            match relink::rewrite_file(&old, &plan) {
                Ok((n, skipped)) => {
                    if skipped > 0 {
                        done.failed.push((plan.new_of(&old), SKIPPED.into()));
                    }
                    if n > 0 {
                        (done.links, done.notes) = (done.links + n, done.notes + 1);
                        done.written.push(plan.new_of(&old));
                    }
                }
                Err(e) => done.failed.push((plan.new_of(&old), e.to_string())),
            }
        }
        let _ = tx.send(done);
        waker.wake();
    });
}

/// Report finished moves; called every frame to pick up the background results.
pub fn poll<U>(state: &mut AppState<U>) {
    poll_inner(&mut state.core_mut());
}

fn poll_inner(state: &mut CoreMut<'_>) {
    while let Ok(mut done) = state.relink_rx.try_recv() {
        // A note opened while it was rewritten shows the old text: take the new one, unless it has unsaved edits.
        for p in std::mem::take(&mut done.written) {
            match state.docs.get(&p).map(|d| d.is_dirty()) {
                Some(false) => {
                    if let Err(e) = state.docs.reload(&p) {
                        done.failed.push((p, e.to_string()));
                    }
                }
                Some(true) => done.failed.push((p, "opened and edited meanwhile; its file changed on disk".into())),
                None => {}
            }
        }
        finish(state, done);
    }
}

fn finish(state: &mut CoreMut<'_>, done: Done) {
    if !done.failed.is_empty() {
        let list: Vec<String> = done.failed.iter().take(3).map(|(p, e)| format!("{} ({e})", name_of(p))).collect();
        let more = if done.failed.len() > 3 { ", ..." } else { "" };
        state.status.error(format!("Could not update links in {}: {}{more}", plural(done.failed.len(), "note"), list.join(", ")));
    }
    let unchecked = done.incomplete.then(|| "Links not checked: still indexing".to_string());
    let updated = (done.links > 0).then(|| format!("Updated {} in {}", plural(done.links, "link"), plural(done.notes, "note")));
    let text = [Some(done.prefix).filter(|p| !p.is_empty()), updated, unchecked].into_iter().flatten().collect::<Vec<_>>().join(". ");
    if !text.is_empty() {
        state.notify(text, done.undo.map(|c| ("Undo", c)), 8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_range_is_minimal_and_on_char_boundaries() {
        assert_eq!(changed_range("a [[old]] b", "a [[new]] b"), (4..7, "new"));
        assert_eq!(changed_range("x", "x"), (1..1, ""));
        assert_eq!(changed_range("aäb", "aöb"), (1..3, "ö"));
        assert_eq!(changed_range("abab", "ab"), (2..4, ""));
    }

    #[test]
    fn drop_rules() {
        let p = Path::new;
        assert!(!can_drop(p("/r/a/x.md"), p("/r/a")));
        assert!(!can_drop(p("/r/a"), p("/r/a")));
        assert!(!can_drop(p("/r/a"), p("/r/a/b")));
        assert!(can_drop(p("/r/a"), p("/r/ab")));
        assert!(can_drop(p("/r/a/x.md"), p("/r/b")));
    }

    fn pump(st: &mut AppState) {
        for _ in 0..200 {
            poll(st);
            if st.notice.is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn move_rewrites_open_and_closed_notes() {
        let files = [("old.md", "x"), ("open.md", "see [[old]]"), ("closed.md", "\u{feff}a [[old|al]]\r\n"), ("sub/keep.md", "none")];
        let (_t, mut st) = crate::test_support::state_with(&files);
        let (old, new) = (st.vault_path("old.md"), st.vault_path("sub/new.md"));
        st.config.update_links = "always".into();
        st.docs.open(&st.vault_path("open.md")).unwrap();
        mv(&mut st, old.clone(), new.clone(), None, true);
        assert!(new.exists() && !old.exists());
        pump(&mut st);
        let open = st.docs.get(&st.vault_path("open.md")).unwrap();
        assert_eq!(open.text, "see [[new]]");
        assert!(open.is_dirty(), "edited through the buffer, not on disk");
        assert_eq!(std::fs::read(st.vault_path("open.md")).unwrap(), b"see [[old]]");
        assert_eq!(std::fs::read(st.vault_path("closed.md")).unwrap(), b"\xEF\xBB\xBFa [[new|al]]\r\n");
        let n = st.notice.as_ref().unwrap();
        assert_eq!(n.text, "Moved old.md to sub. Updated 2 links in 2 notes");
        assert!(n.action.is_some());
    }

    #[test]
    fn ask_never_and_refused_moves() {
        let (_t, mut st) = crate::test_support::state_with(&[("old.md", "x"), ("a.md", "[[old]]"), ("d/x.md", "")]);
        let (old, new) = (st.vault_path("old.md"), st.vault_path("d/new.md"));
        mv(&mut st, old.clone(), new.clone(), None, false);
        assert!(matches!(st.dialogs.current, Some(crate::dialogs::Dialog::Confirm { alt: Some(_), .. })), "asks first");
        assert!(old.exists(), "nothing moved before the answer");
        st.dialogs.current = None;
        st.config.update_links = "never".into();
        mv(&mut st, old, new, None, false);
        assert_eq!(std::fs::read_to_string(st.vault_path("a.md")).unwrap(), "[[old]]");
        let d = st.vault_path("d");
        mv(&mut st, d.clone(), d.join("x.md"), None, false);
        assert!(d.exists());
    }

    #[test]
    fn unchecked_index_and_failures_are_reported() {
        let (_t, mut st) = crate::test_support::state_with(&[("a.md", "x")]);
        let done = Done {
            prefix: String::new(),
            undo: None,
            links: 0,
            notes: 0,
            failed: vec![(PathBuf::from("/v/n.md"), "boom".into())],
            written: vec![],
            incomplete: true,
        };
        finish(&mut st.core_mut(), done);
        assert_eq!(st.notice.as_ref().unwrap().text, "Links not checked: still indexing");
        assert!(st.status.current().unwrap().text.contains("n.md (boom)"));
    }

    #[test]
    fn open_note_is_reloaded_or_reported_after_a_background_rewrite() {
        let (_t, mut st) = crate::test_support::state_with(&[("old.md", "x"), ("c.md", "[[old]]"), ("d.md", "[[old]]")]);
        let (c, d) = (st.vault_path("c.md"), st.vault_path("d.md"));
        st.config.update_links = "always".into();
        let (old, new) = (st.vault_path("old.md"), st.vault_path("new.md"));
        mv(&mut st, old, new, None, false);
        pump(&mut st);
        // Opened after the rewrite (clean): sees the new text. Opened and edited before `poll` ran: reported.
        st.docs.open(&c).unwrap();
        st.docs.open(&d).unwrap();
        st.docs.get_mut(&d).unwrap().apply_edit(0..0, "z");
        let done = Done { prefix: String::new(), undo: None, links: 1, notes: 2, failed: vec![], written: vec![c.clone(), d.clone()], incomplete: false };
        std::fs::write(&c, "[[new]]").unwrap();
        st.relink_tx.send(done).unwrap();
        poll(&mut st);
        assert_eq!(st.docs.get(&c).unwrap().text, "[[new]]");
        assert!(st.docs.get(&d).unwrap().text.starts_with('z'), "unsaved edits are kept");
        assert!(st.status.current().unwrap().text.contains("d.md"));
    }

    #[test]
    fn answer_uses_the_plan_of_the_question_and_same_path_is_silent() {
        let (_t, mut st) = crate::test_support::state_with(&[("old.md", "x"), ("a.md", "[[old]]"), ("d/x.md", "")]);
        let (old, new) = (st.vault_path("old.md"), st.vault_path("d/new.md"));
        mv(&mut st, old.clone(), new.clone(), None, false);
        assert!(st.pending_move.is_some());
        mv(&mut st, old.clone(), new, Some(true), false);
        assert!(st.pending_move.is_none() && st.status.current().is_none());
        mv(&mut st, old.clone(), old, None, false);
        assert!(st.status.current().is_none());
    }
}
