//! Editor extras: formatting shortcuts, wiki-link autocomplete, find/replace, Ctrl+click links.
//! Hooks run around the `TextEdit`: `before` consumes keys, `after` paints and handles pointer input.
#[path = "ctxmenu.rs"]
pub(crate) mod ctxmenu;
#[path = "find.rs"]
mod find;
#[path = "paste_image.rs"]
mod paste_image;
#[path = "wiki.rs"]
mod wiki;

use crate::command::Command;
use crate::state::AppState;
use egui::{Galley, Id, Key, Modifiers, Pos2, Rect, Response, Ui};
use iron_sheets_app_core::editor::autoclose;
use iron_sheets_core::FileKind;
use iron_sheets_md::edit::{self, EditOp};
use std::ops::Range;
use std::sync::Arc;

#[derive(Default)]
pub struct EditorFeatures {
    find: find::Find,
    wiki: wiki::Wiki,
    pasting: paste_image::Pasting,
    /// Closers auto-close inserted, for the tab `closers_tab`.
    closers: autoclose::Closers,
    closers_tab: u64,
    /// Byte offset of the last right-click.
    menu_at: usize,
    /// The link found at (path, document version, char index): the lookup walks the text, so it is done once per pointer position.
    probe: Option<(std::path::PathBuf, u64, usize, Option<Link>)>,
    /// Last frame's layout (path, doc version, galley, galley pos): finds the link under a middle-click before the TextEdit.
    last_layout: Option<(std::path::PathBuf, u64, Arc<Galley>, Pos2)>,
}

pub struct EditorFrame {
    pub galley: Arc<Galley>,
    pub galley_pos: Pos2,
    pub cursor_rect: Option<Rect>,
    pub response: Response,
    pub changed: bool,
    /// The visible part of the scroll pane (what the pointer can point at).
    pub viewport: Rect,
    /// The change mark the pointer clicked in the gutter this frame (Git).
    pub hunk_click: Option<super::gutter::DiffMark>,
}

fn editor_id(tab: &crate::tabs::Tab) -> Id {
    Id::NULL.with(("editor", tab.id, tab.nav_gen))
}

/// Applies an edit like `Command::ApplyEdit` does: through `Doc::apply_edit`, cursor via `pending_cursor`.
/// Auto-close: a typed pairing char or Backspace between an empty pair becomes an edit; the key is consumed then.
/// Nothing happens while an IME is composing.
fn auto_close_op(ui: &Ui, closers: &mut autoclose::Closers, text: &str, sel: Range<usize>, markdown: bool) -> Option<EditOp> {
    ui.input_mut(|i| {
        if i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))) {
            return None;
        }
        if i.key_pressed(Key::Backspace) && i.modifiers.is_none() {
            let op = closers.on_backspace(text, sel, markdown)?;
            i.consume_key(Modifiers::NONE, Key::Backspace);
            return Some(op);
        }
        let mut typed = i.events.iter().enumerate().filter_map(|(n, e)| match e {
            egui::Event::Text(t) => Some((n, t)),
            _ => None,
        });
        let (n, t) = typed.next().filter(|_| typed.next().is_none())?;
        let mut chars = t.chars();
        let ch = chars.next().filter(|_| chars.next().is_none())?;
        let op = closers.on_text(text, sel, ch, markdown)?;
        i.events.remove(n);
        Some(op)
    })
}

/// A pasted image was written (or failed): insert `![[name]]` into the note it came from, wherever the caret is now.
fn finish_paste(state: &mut AppState, d: paste_image::Done) {
    let name = match d.res {
        Ok(n) => n,
        Err(e) => return state.status.error(format!("Could not save the pasted image: {e}")),
    };
    // Index and tree learn of the file now instead of when the watcher reports it.
    state.tree.invalidate(&d.dir);
    state.indexer.file_changed(d.dir.join(&name));
    let insert = format!("![[{name}]]");
    let Some(doc) = state.docs.get_mut(&d.note).filter(|doc| !doc.read_only) else {
        return state.status.error(format!("Saved {name}, but its note is gone or read-only: link not inserted"));
    };
    let range = if doc.version == d.version {
        d.caret
    } else {
        // The note changed meanwhile: insert (never replace) at the saved offset, or at the end of its line when that
        // is no longer a char boundary.
        let mut at = d.caret.start.min(doc.text.len());
        if !doc.text.is_char_boundary(at) {
            at += doc.text.as_bytes()[at..].iter().position(|&b| b == b'\n').unwrap_or(doc.text.len() - at);
        }
        at..at
    };
    let end = range.start + insert.len();
    if doc.apply_edit(range, &insert) {
        if let Some(t) = state.tabs.tabs.iter_mut().find(|t| t.path == d.note) {
            t.pending_cursor = Some(end..end);
        }
    } else {
        state.status.error(format!("Saved {name}, but the link could not be inserted"));
    }
}

fn apply(state: &mut AppState, ti: usize, op: EditOp) {
    let Some(path) = state.tabs.tabs.get(ti).map(|t| t.path.clone()) else { return };
    let ok = state.docs.get_mut(&path).is_some_and(|d| !d.read_only && d.apply_edit(op.range.clone(), &op.insert));
    if ok {
        state.tabs.tabs[ti].pending_cursor = Some(op.select);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Wiki { note: String, heading: Option<String> },
    Url(String),
}

/// The wiki link or URL covering byte `at` of `text`, if any.
pub fn link_at(text: &str, at: usize) -> Option<Link> {
    let at = at.min(text.len());
    if !text.is_char_boundary(at) {
        return None;
    }
    let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let le = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let line = &text[ls..le];
    let col = at - ls;
    // `[[...]]`
    if let Some(open) = line[..col.min(line.len())].rfind("[[").or_else(|| if line[col..].starts_with("[[") { Some(col) } else { None }) {
        if let Some(close) = line[open + 2..].find("]]") {
            let end = open + 2 + close;
            if col <= end + 2 {
                let t = iron_sheets_core::links::parse_wikilink(&line[open + 2..end]);
                return Some(Link::Wiki { note: t.note.to_string(), heading: t.heading.map(str::to_string) });
            }
        }
    }
    // URL: widen over non-delimiters.
    let stop = |c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '[' | ']' | '"' | '\'' | '`');
    let s = line[..col].rfind(stop).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let e = line[col..].find(stop).map_or(line.len(), |i| col + i);
    if s >= e {
        return None;
    }
    let tok = line[s..e].trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let l = tok.to_ascii_lowercase();
    if l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:") {
        return Some(Link::Url(tok.to_string()));
    }
    None
}

impl EditorFeatures {
    /// Called before the TextEdit so it can consume keys.
    pub fn before(&mut self, ui: &mut Ui, state: &mut AppState, ti: usize) {
        while let Ok(d) = self.pasting.rx.try_recv() {
            finish_paste(state, d);
        }
        let Some(tab) = state.tabs.tabs.get(ti) else { return };
        let tab_id = tab.id;
        let id = editor_id(tab);
        let Some(doc) = state.docs.get(&tab.path) else { return };
        // Middle-click on a link opens it (see `after`); the PRIMARY-selection paste egui-winit sends with it must not
        // land in the note.
        if let Some((_, _, g, gpos)) = self.last_layout.as_ref().filter(|(p, v, ..)| *p == tab.path && *v == doc.version) {
            let on_link = |pos: Pos2| {
                let c = g.cursor_from_pos(pos - *gpos).index.0;
                let byte = doc.text.char_indices().nth(c).map_or(doc.text.len(), |(i, _)| i);
                link_at(&doc.text, byte).is_some()
            };
            ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::MiddleClickPaste { pos, .. } if on_link(*pos))));
        }
        if ui.memory(|m| m.has_focus(id)) {
            // Every file kind, and read-only notes too (Alt+C copies there; the TextEdit ignores the cut).
            alt_clipboard(ui, &state.keymap);
        }
        if doc.read_only {
            return;
        }
        let kind = doc.kind;
        if state.find_request && state.tabs.active == Some(ti) {
            state.find_request = false;
            let sel = tab.cursor.as_ref().and_then(|r| doc.text.get(r.clone()));
            self.find.open_for(tab_id, sel);
        }
        if ui.input_mut(|i| crate::keys_egui::editor_pressed(&state.keymap, i, "editor.replace")) {
            let sel = tab.cursor.as_ref().and_then(|r| doc.text.get(r.clone()));
            self.find.open_replace(tab_id, sel);
        }
        if self.find.shown_for(tab_id) {
            self.find.bar(ui, state, ti, id);
        }
        if !ui.memory(|m| m.has_focus(id)) {
            return;
        }
        let Some(tab) = state.tabs.tabs.get(ti) else { return };
        let Some(doc) = state.docs.get(&tab.path) else { return };
        let len = doc.text.len();
        let sel: Range<usize> =
            tab.cursor.clone().map_or(len..len, |r| crate::editor::floor_boundary(&doc.text, r.start)..crate::editor::floor_boundary(&doc.text, r.end));
        let path = tab.path.clone();
        if state.config.auto_close_brackets {
            if self.closers_tab != tab_id {
                self.closers_tab = tab_id;
                self.closers.forget();
            }
            if let Some(op) = auto_close_op(ui, &mut self.closers, &doc.text, sel.clone(), kind == FileKind::Markdown) {
                apply(state, ti, op);
                return;
            }
        }
        // Line moves and copies are plain text operations: `.typ` notes get them too.
        if let Some(op) = duplicate_line_op(ui, &state.keymap, &doc.text, sel.clone()).or_else(|| move_line_op(ui, &state.keymap, &doc.text, sel.clone())) {
            apply(state, ti, op);
            return;
        }
        if kind != FileKind::Markdown {
            return;
        }
        let pasted = ui.input_mut(|i| {
            let n = i.events.iter().position(|e| matches!(e, egui::Event::PasteImage(_)))?;
            match i.events.remove(n) {
                egui::Event::PasteImage(img) => Some(img),
                _ => None,
            }
        });
        if let Some(img) = pasted {
            if state.docs.is_untitled(&path) {
                state.status.info("Save the note first to paste images");
                return;
            }
            let (dir, warn) = paste_image::attachment_dir(&state.config, &path, &state.workspace.roots);
            if let Some(w) = warn {
                state.status.info(w);
            }
            if !state.workspace.roots.iter().any(|r| dir.starts_with(r)) {
                state.status.info("The attachment folder is outside the workspace: the image link will not resolve in the preview");
            }
            self.pasting.start(ui.ctx(), img, dir, path, sel, doc.version);
            return;
        }

        // Autocomplete first: its keys win over the list/indent handlers.
        if sel.is_empty() {
            self.wiki.update(state, &doc.text, sel.start, &path);
        } else {
            self.wiki.update(state, "", 0, &path);
        }
        let mut escaped = false;
        let accepted = self.wiki.keys(ui, &mut escaped);
        if escaped {
            ui.memory_mut(|m| m.request_focus(id));
        }
        if let Some(op) = accepted {
            apply(state, ti, op);
            return;
        }
        if escaped {
            return;
        }

        let text = &doc.text;
        let (enter, tab_key, mods) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Tab), i.modifiers));
        let mut op: Option<EditOp> = None;
        let consume = |ui: &Ui, m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
        if enter && !mods.command && !mods.alt && !mods.shift && sel.is_empty() {
            if let Some(o) = edit::on_enter(text, sel.start) {
                consume(ui, Modifiers::NONE, Key::Enter);
                op = Some(o);
            }
        } else if tab_key && !mods.command && !mods.alt && !mods.ctrl {
            let o = edit::on_tab(text, sel.clone(), mods.shift);
            if mods.shift {
                consume(ui, Modifiers::SHIFT, Key::Tab);
                op = o;
            } else if o.is_some() {
                consume(ui, Modifiers::NONE, Key::Tab);
                op = o;
            }
        }
        if op.is_none() {
            let km = &state.keymap;
            let (b, i, k, c) = ui.input_mut(|inp| {
                (
                    crate::keys_egui::editor_pressed(km, inp, "editor.bold"),
                    crate::keys_egui::editor_pressed(km, inp, "editor.italic"),
                    crate::keys_egui::editor_pressed(km, inp, "editor.link"),
                    crate::keys_egui::editor_pressed(km, inp, "editor.code"),
                )
            });
            op = if b {
                Some(edit::toggle_wrap(text, sel.clone(), "**"))
            } else if i {
                Some(edit::toggle_wrap(text, sel.clone(), "*"))
            } else if k {
                Some(edit::insert_link(text, sel.clone()))
            } else if c {
                Some(edit::toggle_wrap(text, sel.clone(), "`"))
            } else {
                None
            };
        }
        if let Some(op) = op {
            apply(state, ti, op);
        }
    }

    /// Called after the TextEdit with its output.
    pub fn after(&mut self, ui: &mut Ui, state: &mut AppState, ti: usize, out: &EditorFrame) {
        let Some(tab) = state.tabs.tabs.get(ti) else { return };
        let (tab_id, path) = (tab.id, tab.path.clone());
        if self.find.shown_for(tab_id) || self.find.refocus.is_some() {
            self.find.paint(ui, &out.galley, out.galley_pos);
        }
        if let (true, Some(caret)) = (self.wiki.active(), out.cursor_rect) {
            if let Some(op) = self.wiki.popup(ui, caret) {
                apply(state, ti, op);
            }
        }
        ctxmenu::show(ui, state, ti, out, &mut self.menu_at);
        // Ctrl+click on `[[wikilink]]` or URL.
        let Some(doc) = state.docs.get(&path) else { return };
        self.last_layout = Some((path.clone(), doc.version, out.galley.clone(), out.galley_pos));
        if doc.kind == FileKind::Other {
            return;
        }
        let (cmd, shift, clicked, middle, pos) =
            ui.input(|i| (i.modifiers.command, i.modifiers.shift, i.pointer.primary_clicked(), i.pointer.button_clicked(egui::PointerButton::Middle), i.pointer.interact_pos()));
        let always = state.config.page_preview == "always";
        if !cmd && !always && !middle {
            return;
        }
        // Not while the pointer is on a popup above the editor.
        let Some(pos) = pos.filter(|p| out.response.rect.contains(*p) && ui.clip_rect().contains(*p) && ui.rect_contains_pointer(out.response.rect)) else {
            return;
        };
        let c = out.galley.cursor_from_pos((pos - out.galley_pos.to_vec2()).to_vec2()).index.0;
        let hit = matches!(&self.probe, Some((p, v, k, _)) if *p == path && *v == doc.version && *k == c);
        if !hit {
            let text = &doc.text;
            let byte = text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
            self.probe = Some((path.clone(), doc.version, c, link_at(text, byte)));
        }
        let Some(link) = self.probe.as_ref().and_then(|p| p.3.clone()) else { return };
        if let Link::Wiki { note, heading } = &link {
            if !note.is_empty() && !clicked && !middle {
                let rect = egui::Rect::from_center_size(pos, egui::vec2(4.0, 18.0));
                state.ui.page_popup.offer(crate::preview::popup::LinkHover { note: note.clone(), heading: heading.clone(), from: path.clone(), rect });
            }
        }
        if !cmd && !middle {
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if !clicked && !middle {
            return;
        }
        let cmd = match link {
            Link::Url(u) => Command::OpenUrl(u),
            Link::Wiki { note, heading } => {
                let note = if note.is_empty() { path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { note };
                Command::OpenLink { note, heading, from: Some(path), new_tab: shift || middle }
            }
        };
        state.commands.push(cmd);
    }
}

/// Alt+Shift+Up/Down copies the current line(s) up/down. Checked before the move keys: egui ignores an extra Shift, so
/// Alt+Up would also match Alt+Shift+Up.
fn duplicate_line_op(ui: &Ui, km: &crate::keymap::Keymap, text: &str, sel: Range<usize>) -> Option<EditOp> {
    let (up, down) = ui.input_mut(|i| (crate::keys_egui::editor_pressed(km, i, "editor.dup_up"), crate::keys_egui::editor_pressed(km, i, "editor.dup_down")));
    (up || down).then(|| edit::duplicate_lines(text, sel, up))
}

/// Alt+X / Alt+C: the same path as Ctrl+X / Ctrl+C (the TextEdit handles `Event::Cut` / `Event::Copy`, and does
/// nothing for an empty selection). egui-winit only suppresses the typed character for Ctrl/Cmd (`lib.rs:1177`), so
/// with Alt held it also sends `Event::Text("x")` right after the key: drop that one event, not other typing.
fn alt_clipboard(ui: &Ui, km: &crate::keymap::Keymap) {
    ui.input_mut(|i| {
        for (id, ev) in [("editor.cut", egui::Event::Cut), ("editor.copy", egui::Event::Copy)] {
            let at = i.events.iter().position(|e| matches!(e, egui::Event::Key { pressed: true, modifiers, .. } if modifiers.alt));
            if crate::keys_egui::editor_pressed(km, i, id) {
                // The key event is consumed (removed), so the character it typed now sits at its index.
                if let Some(n) = at.filter(|&n| matches!(i.events.get(n), Some(egui::Event::Text(_)))) {
                    i.events.remove(n);
                }
                i.events.push(ev);
            }
        }
    });
}

/// Ctrl+Shift+Up/Down swaps the current line with its neighbour.
fn move_line_op(ui: &Ui, km: &crate::keymap::Keymap, text: &str, sel: Range<usize>) -> Option<EditOp> {
    let (up, down) = ui.input_mut(|i| (crate::keys_egui::editor_pressed(km, i, "editor.line_up"), crate::keys_egui::editor_pressed(km, i, "editor.line_down")));
    if !up && !down {
        return None;
    }
    let cur_start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
    let cur_end = text[sel.end..].find('\n').map_or(text.len(), |i| sel.end + i);
    if up {
        if cur_start == 0 {
            return None;
        }
        let prev_start = text[..cur_start - 1].rfind('\n').map_or(0, |i| i + 1);
        let prev = &text[prev_start..cur_start - 1];
        let cur = &text[cur_start..cur_end];
        let ins = format!("{cur}\n{prev}");
        let (a, b) = (sel.start - cur_start + prev_start, sel.end - cur_start + prev_start);
        Some(EditOp { range: prev_start..cur_end, insert: ins, select: a..b })
    } else {
        if cur_end >= text.len() {
            return None;
        }
        let next_end = text[cur_end + 1..].find('\n').map_or(text.len(), |i| cur_end + 1 + i);
        let next = &text[cur_end + 1..next_end];
        let cur = &text[cur_start..cur_end];
        let ins = format!("{next}\n{cur}");
        let shift = next.len() + 1;
        Some(EditOp { range: cur_start..next_end, insert: ins, select: sel.start + shift..sel.end + shift })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links() {
        let t = "see [[Plan#Goals|x]] and https://a.b/c. ok";
        assert_eq!(link_at(t, 8), Some(Link::Wiki { note: "Plan".into(), heading: Some("Goals".into()) }));
        assert_eq!(link_at(t, 30), Some(Link::Url("https://a.b/c".into())));
        assert_eq!(link_at(t, 0), None);
    }
}
