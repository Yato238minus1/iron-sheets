//! Hunk popup: a click on a change bar in the editor gutter (or `git.hunk`, `git.hunk_next`, `git.hunk_prev`) shows the
//! change under the bar: the removed lines of the last commit, the added lines of the buffer, and Revert hunk (a plain
//! undoable edit of the buffer, no git), Stage hunk, Unstage hunk, Previous, Next, Close. Keyboard first: R, S, U, P, N and Esc
//! work while the popup has the focus, which it takes when it opens. Nothing waits for git: the marks and the `HEAD` text come
//! from the Git module's cache, staging goes out as a command to its worker.
use crate::command::Command;
use crate::editor::features::EditorFrame;
use crate::state::AppState;
use egui::text::CCursor;
use egui::{Key, Modifiers, Rect, RichText, Sense, WidgetInfo, pos2, vec2};
use iron_sheets_app_core::git::{DiffKind, DiffMark, GitModule, HunkNav};
use iron_sheets_app_core::git_hunks::{STALE, revert_edit};
use std::path::PathBuf;

/// Lines of each side the popup lists before "N more lines".
const MAX_LINES: usize = 12;
const WIDTH: f32 = 520.0;
const UPDATING: &str = "Changes are being updated, try again in a moment";

struct Open {
    path: PathBuf,
    mark: DiffMark,
    /// The document version the popup shows; a later one (an edit, a revert, an undo) closes it.
    version: u64,
    /// Char index of the start of the change's first line, to find its row in the editor.
    start_char: usize,
    /// Row of that line in screen space, and the visible editor area, as the editor laid them out this frame.
    anchor: Option<(Rect, Rect)>,
    old: Vec<String>,
    new: Vec<String>,
    old_n: usize,
    new_n: usize,
    /// The file is new: `HEAD` has no text to revert to.
    new_file: bool,
    /// Frames still asking for the keyboard focus (the editor takes it back once when the caret moves to the change).
    focus_frames: u8,
}

#[derive(Default)]
pub struct HunkPopup {
    open: Option<Open>,
    /// A gutter click the editor saw this frame.
    click: Option<(PathBuf, DiffMark)>,
}

impl HunkPopup {
    /// The change the popup shows, if it is open.
    pub fn shown(&self) -> Option<(&std::path::Path, DiffMark)> {
        self.open.as_ref().map(|o| (o.path.as_path(), o.mark))
    }
}

/// Right after the editor drew: remembers a gutter click and where the popup's change is on screen.
pub fn after_editor(state: &mut AppState, ti: usize, f: &EditorFrame) {
    let Some(path) = state.tabs.tabs.get(ti).map(|t| t.path.clone()) else { return };
    let p = &mut state.ui.git_hunk;
    if let Some(m) = f.hunk_click {
        p.click = Some((path.clone(), m));
    }
    if let Some(o) = p.open.as_mut().filter(|o| o.path == path) {
        let row = f.galley.pos_from_cursor(CCursor::new(o.start_char)).translate(f.galley_pos.to_vec2());
        o.anchor = Some((row, f.viewport));
    }
}

fn line_start(text: &str, line: usize) -> usize {
    if line == 0 { 0 } else { text.match_indices('\n').nth(line - 1).map_or(text.len(), |(i, _)| i + 1) }
}

/// The change at line `line` (a deletion counts for the line it sits before).
fn contains(m: &DiffMark, line: u32) -> bool {
    m.start <= line && line < m.end.max(m.start + 1)
}

/// The mark `nav` asks for: relative to `from` (the shown change's start) or, without one, to the caret line. Wraps around.
fn pick(marks: &[DiffMark], nav: HunkNav, from: Option<u32>, caret: u32) -> Option<DiffMark> {
    let after = |p: u32| marks.iter().find(|m| m.start > p).or(marks.first());
    let before = |p: u32| marks.iter().rev().find(|m| m.start < p).or(marks.last());
    match (nav, from) {
        (HunkNav::Next, Some(p)) => after(p),
        (HunkNav::Prev, Some(p)) => before(p),
        (HunkNav::Next, None) => marks.iter().find(|m| m.start > caret && !contains(m, caret)).or(marks.first()),
        (HunkNav::Prev, None) => marks.iter().rev().find(|m| m.start < caret && !contains(m, caret)).or(marks.last()),
        (HunkNav::At, _) => marks.iter().find(|m| contains(m, caret)).or_else(|| after(caret.saturating_sub(1))),
    }
    .copied()
}

impl HunkPopup {
    /// Shows or navigates to the change `nav` asks for in the note at `path`.
    fn navigate(&mut self, state: &mut AppState, path: PathBuf, nav: HunkNav) {
        let Some((marks, _, version)) = GitModule::of(state).and_then(|g| g.hunk_data(&path)) else {
            self.open = None;
            return state.status.info("No changes against the last commit in this note");
        };
        let Some((text, doc_version)) = state.docs.get(&path).map(|d| (&d.text, d.version)) else { return };
        if version != doc_version {
            return state.status.info(UPDATING);
        }
        let caret = state.tabs.tabs.iter().find(|t| t.path == path).and_then(|t| t.cursor.as_ref()).map_or(0, |c| c.start.min(text.len()));
        let caret_line = text.as_bytes()[..caret].iter().filter(|&&b| b == b'\n').count() as u32;
        let from = self.open.as_ref().filter(|o| o.path == path).map(|o| o.mark.start);
        if let Some(m) = pick(&marks, nav, from, caret_line) {
            self.open_mark(state, path, m, true);
        }
    }

    /// Opens the popup on `mark`; `go` also moves the caret to the change (the editor scrolls there).
    fn open_mark(&mut self, state: &mut AppState, path: PathBuf, mark: DiffMark, go: bool) {
        let (Some(doc), Some((_, old, version))) = (state.docs.get(&path), GitModule::of(state).and_then(|g| g.hunk_data(&path))) else { return };
        // The marks are debounced behind the text: act on them only when they are for this very version.
        if version != doc.version {
            return state.status.info(UPDATING);
        }
        let take = |text: &str, from: u32, to: u32| -> (Vec<String>, usize) {
            let n = to.saturating_sub(from) as usize;
            (text.split('\n').skip(from as usize).take(n.min(MAX_LINES)).map(str::to_string).collect(), n)
        };
        let (new, new_n) = take(&doc.text, mark.start, mark.end);
        let (old_lines, old_n) = take(&old, mark.old_start, mark.old_end);
        let at = line_start(&doc.text, mark.start as usize);
        let start_char = doc.text[..at].chars().count();
        let version = doc.version;
        if go {
            if let Some(t) = state.tabs.tabs.iter_mut().find(|t| t.path == path) {
                t.pending_cursor = Some(at..at);
            }
        }
        self.open = Some(Open { path, mark, version, start_char, anchor: None, old: old_lines, new, old_n, new_n, new_file: old.is_empty(), focus_frames: 3 });
    }
}

/// Once per frame, after the central panel.
pub fn show(ctx: &egui::Context, state: &mut AppState) {
    let mut p = std::mem::take(&mut state.ui.git_hunk);
    p.run(ctx, state);
    state.ui.git_hunk = p;
}

enum Act {
    Revert,
    Stage(bool),
    Nav(HunkNav),
    Close,
}

impl HunkPopup {
    fn run(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if let Some((path, nav)) = GitModule::of_mut(state).and_then(GitModule::take_hunk_request) {
            self.navigate(state, path, nav);
        }
        if let Some((path, mark)) = self.click.take() {
            self.open_mark(state, path, mark, false);
        }
        let Some(mut o) = self.open.take() else { return };
        // The popup belongs to one note version and one of its changes as the Git module still knows them.
        let alive = state.active_path().as_deref() == Some(o.path.as_path())
            && state.docs.get(&o.path).is_some_and(|d| d.version == o.version)
            && GitModule::of(state).and_then(|g| g.hunk_data(&o.path)).is_some_and(|(marks, _, v)| v == o.version && marks.contains(&o.mark));
        if !alive {
            return;
        }
        // A new or ignored file is not in the index: there is nothing to stage a hunk into.
        let not_indexed = GitModule::of(state).and_then(|g| g.snapshot().status_of(&o.path)).is_some_and(|s| matches!(s, iron_sheets_app_core::git::GitFileStatus::Untracked | iron_sheets_app_core::git::GitFileStatus::Ignored));
        let read_only = state.docs.get(&o.path).is_none_or(|d| d.read_only);
        let sink = egui::Id::NULL.with("git_hunk_popup");
        let dialog = state.dialogs.current.is_some();
        let mut act = None;
        if !dialog {
            // Keys go to the popup while it has the focus; typing into the editor never reaches here.
            let has_focus = ctx.memory(|m| m.has_focus(sink)) || o.focus_frames > 0;
            if has_focus {
                ctx.input_mut(|i| {
                    for (key, a) in [(Key::R, Act::Revert), (Key::S, Act::Stage(true)), (Key::U, Act::Stage(false)), (Key::P, Act::Nav(HunkNav::Prev)), (Key::N, Act::Nav(HunkNav::Next)), (Key::Escape, Act::Close)] {
                        if act.is_none() && i.consume_key(Modifiers::NONE, key) {
                            act = Some(a);
                        }
                    }
                });
            }
        }
        let (anchor, view) = o.anchor.unwrap_or((Rect::from_min_size(pos2(40.0, 120.0), vec2(0.0, 20.0)), Rect::from_min_size(pos2(0.0, 80.0), vec2(WIDTH + 40.0, 400.0))));
        let pos = pos2(view.left() + 6.0, (anchor.bottom() + 2.0).clamp(view.top(), (view.bottom() - 120.0).max(view.top())));
        let weak = ctx.global_style().visuals.weak_text_color();
        let mut pressed_outside = false;
        let resp = egui::Area::new(sink.with("area")).order(egui::Order::Foreground).fixed_pos(pos).constrain(true).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(WIDTH.min(view.width() - 12.0).max(260.0));
                // The key sink: focusable, so the editor gives the focus up and R, S, U, P, N and Esc reach us.
                let r = ui.interact(ui.max_rect(), sink, Sense::focusable_noninteractive());
                if o.focus_frames > 0 {
                    o.focus_frames -= 1;
                    r.request_focus();
                    ui.ctx().request_repaint(); // the focus settles over a few frames
                }
                // Without this egui drops the focus on Escape before we see the key.
                ui.memory_mut(|m| m.set_focus_lock_filter(sink, egui::EventFilter { escape: true, ..Default::default() }));
                r.widget_info(|| WidgetInfo::labeled(egui::Role::Group, true, "Change in the note"));
                let m = o.mark;
                let head = match m.kind {
                    DiffKind::Added => format!("Added line{} {}", if o.new_n == 1 { "" } else { "s" }, range_text(m.start, m.end)),
                    DiffKind::Modified => format!("Changed line{} {}", if o.new_n == 1 { "" } else { "s" }, range_text(m.start, m.end)),
                    DiffKind::Deleted => format!("Deleted {} line{} after line {}", o.old_n, if o.old_n == 1 { "" } else { "s" }, m.start),
                };
                ui.strong(head);
                let (red, green) = (ui.visuals().error_fg_color, if ui.visuals().dark_mode { egui::Color32::from_rgb(90, 185, 115) } else { egui::Color32::from_rgb(35, 138, 70) });
                for (lines, total, sign, col) in [(&o.old, o.old_n, "- ", red), (&o.new, o.new_n, "+ ", green)] {
                    for l in lines {
                        ui.add(egui::Label::new(RichText::new(format!("{sign}{l}")).monospace().color(col)).truncate());
                    }
                    if total > lines.len() {
                        ui.label(RichText::new(format!("{sign}... {} more lines", total - lines.len())).color(weak));
                    }
                }
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (text, a, enabled, tip) in [
                        ("Revert hunk (R)", Act::Revert, !read_only && !o.new_file, if o.new_file { "The file is new: there is nothing to revert to" } else { "Put the last commit's lines back in the note (Undo undoes it)" }),
                        ("Stage hunk (S)", Act::Stage(true), !not_indexed, if not_indexed { "The file is not in the index yet: stage the file first" } else { "Add this change to the index, whatever else is staged" }),
                        ("Unstage hunk (U)", Act::Stage(false), !not_indexed, "Take this change out of the index again"),
                        ("Previous (P)", Act::Nav(HunkNav::Prev), true, "Previous change in this note"),
                        ("Next (N)", Act::Nav(HunkNav::Next), true, "Next change in this note"),
                        ("Close (Esc)", Act::Close, true, ""),
                    ] {
                        let b = ui.add_enabled(enabled, egui::Button::new(text));
                        let b = if tip.is_empty() { b } else { b.on_hover_text(tip) };
                        if b.clicked() {
                            act = Some(a);
                        }
                    }
                });
            });
        });
        // A press anywhere else leaves the popup (the click on another change bar opens that one instead).
        if ctx.input(|i| i.pointer.any_pressed()) {
            pressed_outside = ctx.pointer_interact_pos().is_some_and(|p| !resp.response.rect.contains(p));
        }
        if pressed_outside && act.is_none() {
            return;
        }
        match act {
            None => self.open = Some(o),
            Some(Act::Nav(nav)) => {
                self.open = Some(o);
                let path = self.open.as_ref().map(|o| o.path.clone()).unwrap_or_default();
                self.navigate(state, path, nav);
            }
            Some(Act::Close) => state.focus_editor = true,
            Some(Act::Stage(_)) if not_indexed => {
                self.open = Some(o);
                state.status.error("The file is not in the index yet: stage the file first");
            }
            Some(Act::Stage(stage)) => {
                state.commands.push(Command::GitHunk { path: o.path, mark: o.mark, stage });
                state.focus_editor = true;
            }
            Some(Act::Revert) => {
                state.focus_editor = true;
                let edit = state.docs.get(&o.path).zip(GitModule::of(state).and_then(|g| g.hunk_data(&o.path))).and_then(|(d, (_, old, _))| revert_edit(&d.text, &old, &o.mark));
                match edit {
                    Some((range, insert)) => {
                        let at = range.start;
                        state.commands.push(Command::ApplyEdit { path: o.path, range, insert, select: Some(at..at) });
                    }
                    None => state.status.error(STALE),
                }
            }
        }
    }
}

/// `3` or `3-5` (1-based lines of the 0-based range `[start, end)`).
fn range_text(start: u32, end: u32) -> String {
    if end <= start + 1 { format!("{}", start + 1) } else { format!("{}-{}", start + 1, end) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(start: u32, end: u32) -> DiffMark {
        let kind = if start == end { DiffKind::Deleted } else { DiffKind::Modified };
        DiffMark { start, end, old_start: start, old_end: end.max(start + 1), kind }
    }

    #[test]
    fn navigation_picks_the_change_at_the_caret_then_next_and_previous_and_wraps() {
        let marks = [m(2, 4), m(10, 10), m(20, 21)];
        assert_eq!(pick(&marks, HunkNav::At, None, 3), Some(marks[0]), "inside");
        assert_eq!(pick(&marks, HunkNav::At, None, 10), Some(marks[1]), "a deletion is at the line it sits before");
        assert_eq!(pick(&marks, HunkNav::At, None, 12), Some(marks[2]), "between: the next one");
        assert_eq!(pick(&marks, HunkNav::At, None, 30), Some(marks[0]), "past the last: wraps");
        assert_eq!(pick(&marks, HunkNav::Next, None, 3), Some(marks[1]), "from the caret, not the one it is in");
        assert_eq!(pick(&marks, HunkNav::Prev, None, 3), Some(marks[2]), "wraps to the last");
        assert_eq!(pick(&marks, HunkNav::Next, Some(10), 0), Some(marks[2]));
        assert_eq!(pick(&marks, HunkNav::Next, Some(20), 0), Some(marks[0]));
        assert_eq!(pick(&marks, HunkNav::Prev, Some(10), 0), Some(marks[0]));
        assert_eq!(pick(&[], HunkNav::At, None, 0), None);
    }

    #[test]
    fn line_starts_and_ranges() {
        assert_eq!([line_start("a\nbb\nc", 0), line_start("a\nbb\nc", 1), line_start("a\nbb\nc", 2), line_start("a\nbb\nc", 9)], [0, 2, 5, 6]);
        assert_eq!((range_text(0, 1), range_text(2, 5)), ("1".to_string(), "3-5".to_string()));
    }
}
