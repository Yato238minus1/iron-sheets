//! Right-click menu of the source editor (modelled on Obsidian's): clipboard, Format, links.
use super::{Link, apply, link_at};
use crate::command::Command;
use crate::keymap::format_shortcut;
use crate::state::AppState;
use iron_sheets_core::FileKind;
use iron_sheets_md::edit::{self, EditOp};
use std::ops::Range;

/// `[[sel]]`, the caret inside an empty pair or after the link.
fn wiki_op(text: &str, sel: Range<usize>) -> EditOp {
    let inner = &text[sel.clone()];
    let at = if sel.is_empty() { sel.start + 2 } else { sel.start + 4 + inner.len() };
    EditOp { range: sel, insert: format!("[[{inner}]]"), select: at..at }
}

/// Byte offset of the character index `c` (only on a right-click, so the plain scan is fine).
fn byte_of(text: &str, c: usize) -> usize {
    text.char_indices().nth(c).map_or(text.len(), |(i, _)| i)
}

/// Selection the editor had when the secondary button went down (egui collapses it on any press).
pub(crate) fn press_sel_id() -> egui::Id {
    egui::Id::NULL.with("ctxmenu_press_sel")
}

/// Set by a right-click: the caret it moves must not recentre the view.
pub(crate) fn no_scroll_id() -> egui::Id {
    egui::Id::NULL.with("ctxmenu_no_scroll")
}

/// Opens the menu on a secondary click of the editor and fills it. `at` remembers where the click was.
pub fn show(ui: &mut egui::Ui, state: &mut AppState, ti: usize, out: &super::EditorFrame, at: &mut usize) {
    // Nothing to do in the usual frame: no click, no open menu.
    if !out.response.secondary_clicked() && !out.response.context_menu_opened() {
        return;
    }
    let Some(tab) = state.tabs.tabs.get(ti) else { return };
    let Some(doc) = state.docs.get(&tab.path) else { return };
    let (kind, editable, path) = (doc.kind, !doc.read_only, tab.path.clone());
    if out.response.secondary_clicked()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        // Below the text or right of a line the position resolves to the end of that line / of the text.
        let c = out.galley.cursor_from_pos((p - out.galley_pos.to_vec2()).to_vec2()).index.0;
        *at = byte_of(&doc.text, c);
        // A click inside the selection keeps it (egui already collapsed it on the press, so it is put back).
        let before = ui.data(|d| d.get_temp::<Option<Range<usize>>>(press_sel_id())).flatten();
        let keep = before.filter(|r| !r.is_empty() && r.contains(at));
        state.tabs.tabs[ti].pending_cursor = Some(keep.unwrap_or(*at..*at));
        ui.data_mut(|d| d.insert_temp(no_scroll_id(), true));
    }
    let Some(tab) = state.tabs.tabs.get(ti) else { return };
    let Some(doc) = state.docs.get(&path) else { return };
    let len = doc.text.len();
    let sel = tab.cursor.clone().map_or(len..len, |r| crate::editor::floor_boundary(&doc.text, r.start)..crate::editor::floor_boundary(&doc.text, r.end));
    let link = link_at(&doc.text, *at);
    let markdown = kind == FileKind::Markdown;
    let hint = |id: &str| state.keymap.editor_bindings(id).first().map(format_shortcut).unwrap_or_default();
    let item = |ui: &mut egui::Ui, on: bool, label: &str, short: String| {
        let b = egui::Button::new(label).shortcut_text(short);
        let r = ui.add_enabled(on, b).clicked();
        if r {
            ui.close();
        }
        r
    };
    let mut op: Option<EditOp> = None;
    let (mut select_all, mut paste, mut copy, mut cut) = (false, false, false, false);
    let mut open: Option<bool> = None;
    crate::anim::context_menu(&out.response, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        let has_sel = !sel.is_empty();
        cut = item(ui, editable && has_sel, "Cut", "Ctrl+X".into());
        copy = item(ui, has_sel, "Copy", "Ctrl+C".into());
        paste = item(ui, editable, "Paste", "Ctrl+V".into());
        // The clipboard is plain text here, so this is the same as Paste.
        paste |= item(ui, editable, "Paste as plain text", "Ctrl+Shift+V".into());
        select_all = item(ui, true, "Select all", "Ctrl+A".into());
        if markdown && editable {
            ui.separator();
            ui.menu_button("Format", |ui| {
                for (label, id, marker) in [
                    ("Bold", "editor.bold", "**"),
                    ("Italic", "editor.italic", "*"),
                    ("Strikethrough", "", "~~"),
                    ("Code", "editor.code", "`"),
                    ("Math", "", "$"),
                    ("Highlight", "", "=="),
                ] {
                    if item(ui, true, label, hint(id)) {
                        op = Some(edit::toggle_wrap(&doc.text, sel.clone(), marker));
                    }
                }
            });
            if item(ui, true, "Insert link", hint("editor.link")) {
                op = Some(edit::insert_link(&doc.text, sel.clone()));
            }
            if item(ui, true, "Insert wiki link", String::new()) {
                op = Some(wiki_op(&doc.text, sel.clone()));
            }
        }
        if link.is_some() {
            ui.separator();
            if item(ui, true, "Open link", String::new()) {
                open = Some(false);
            }
            if item(ui, true, "Open in new tab", String::new()) {
                open = Some(true);
            }
        }
    });
    let len = doc.text.len();
    if cut || copy {
        ui.ctx().copy_text(doc.text.get(sel.clone()).unwrap_or_default().to_owned());
    }
    if cut {
        op = Some(EditOp { range: sel.clone(), insert: String::new(), select: sel.start..sel.start });
    }
    let acted = cut || copy || paste || select_all || op.is_some();
    if let Some(op) = op {
        apply(state, ti, op);
    }
    if select_all {
        state.tabs.tabs[ti].pending_cursor = Some(0..len);
    }
    if paste {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
    }
    if let (Some(new_tab), Some(link)) = (open, link) {
        state.commands.push(match link {
            Link::Url(u) => Command::OpenUrl(u),
            Link::Wiki { note, heading } => {
                let note = if note.is_empty() { path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { note };
                Command::OpenLink { note, heading, from: Some(path), new_tab }
            }
        });
    }
    if acted {
        ui.memory_mut(|m| m.request_focus(super::editor_id(&state.tabs.tabs[ti])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_op_places_the_caret() {
        assert_eq!(wiki_op("ab", 1..1).select, 3..3);
        let op = wiki_op("a bc d", 2..4);
        assert_eq!((op.insert.as_str(), op.select), ("[[bc]]", 8..8));
    }
}
