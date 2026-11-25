//! Git panel: changed files per repo (staged / not staged), stage, unstage and discard, commit message, commit, push, pull,
//! fetch, the branch and remote dialogs; "Initialize repository" for workspace folders that are in no repository.
//! Everything runs on the Git module's worker: the panel only reads the snapshot and pushes `git.*` commands.
use super::outline::{Jump, panel_keys, reveal};
use crate::command::Command;
use crate::state::AppState;
use crate::theme::{RowKind, focus_ring, row_text_color, selected_bg};
use egui::{Align2, Key, Modifiers, Sense, WidgetInfo, pos2, vec2};
use iron_sheets_app_core::git::{GitAvailability, GitModule, GitSnapshot, OpOutcome, posix_rel_to_path};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::Arc;

const ROW_H: f32 = 22.0;
const BTN_W: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Row {
    Repo(usize),
    Section { repo: usize, staged: bool, n: usize },
    File { repo: usize, entry: usize, staged: bool },
}

/// `(staged, not staged)` file counts of a repo, in one pass.
pub(crate) fn counts(repo: &iron_sheets_app_core::git::RepoSnapshot) -> (usize, usize) {
    repo.changes.iter().fold((0, 0), |(s, u), c| (s + usize::from(c.staged()), u + usize::from(c.unstaged())))
}

/// `notes/projects/foo`: the repo's folder below its workspace folder, led by that folder's name (the repo's own name when
/// it is outside every workspace folder).
pub(crate) fn repo_label(repo_root: &std::path::Path, roots: &[PathBuf]) -> String {
    let root = roots.iter().filter(|r| repo_root.starts_with(r)).max_by_key(|r| r.as_os_str().len());
    let name = |p: &std::path::Path| p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let Some(root) = root else { return name(repo_root) };
    let rel: Vec<String> = repo_root.strip_prefix(root).iter().flat_map(|r| r.components()).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    std::iter::once(name(root)).chain(rel).collect::<Vec<_>>().join("/")
}

/// Rows for all repos: a repo header (with several repos, or for one found below a workspace folder), "Staged Changes" and
/// "Changes" sections with their files.
pub(crate) fn build_rows(snap: &GitSnapshot) -> Vec<Row> {
    let mut rows = Vec::new();
    for (r, repo) in snap.repos.iter().enumerate() {
        if snap.repos.len() > 1 || !snap.root_to_repo.values().any(|&i| i == r) {
            rows.push(Row::Repo(r));
        }
        for staged in [true, false] {
            let n = if staged { counts(repo).0 } else { counts(repo).1 };
            if n == 0 {
                continue;
            }
            rows.push(Row::Section { repo: r, staged, n });
            rows.extend(repo.changes.iter().enumerate().filter(|(_, c)| if staged { c.staged() } else { c.unstaged() }).map(|(entry, _)| Row::File { repo: r, entry, staged }));
        }
    }
    rows
}

/// The file row `step` away from `from` (`None` = before the first / after the last), skipping headers.
pub(crate) fn step_file(rows: &[Row], from: Option<usize>, step: isize) -> Option<usize> {
    let mut i = from.map(|f| f as isize).unwrap_or(if step > 0 { -1 } else { rows.len() as isize });
    loop {
        i += step.signum();
        if i < 0 || i >= rows.len() as isize {
            return None;
        }
        if matches!(rows[i as usize], Row::File { .. }) {
            return Some(i as usize);
        }
    }
}

/// The state without a repository: one row per workspace folder with an "Initialize repository" button (git init on the
/// worker; the next snapshot switches the panel to the changes view). A failure shows git's line, the button is the retry.
fn no_repo(ui: &mut egui::Ui, state: &mut AppState, scanning: bool, take_focus: bool, last: Option<&OpOutcome>) {
    ui.add_space(8.0);
    let roots = state.workspace.roots.clone();
    if roots.is_empty() {
        ui.weak("Add a folder to the workspace to use Git.");
        return;
    }
    if scanning {
        ui.weak("Scanning for repositories...");
        return;
    }
    ui.label("The folders of this workspace are not Git repositories.");
    ui.add_space(4.0);
    let busy: Vec<bool> = roots.iter().map(|r| GitModule::of(state).is_some_and(|g| g.busy(r).is_some())).collect();
    let mut out = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let name = root.file_name().map_or_else(|| root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        ui.horizontal(|ui| {
            ui.strong(&name).on_hover_text(root.to_string_lossy());
            if busy[i] {
                ui.add(egui::Spinner::new().size(12.0));
                ui.weak("Initializing...");
                return;
            }
            let b = ui.button("Initialize repository").on_hover_text("git init in this folder");
            b.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Initialize repository in {name}")));
            if take_focus && i == 0 {
                b.request_focus();
            }
            if b.clicked() {
                out.push(Command::Module { id: "git.init", arg: root.to_string_lossy().into_owned() });
            }
        });
    }
    if let Some(o) = last.filter(|o| o.op == "Init") {
        if let Err(e) = &o.result {
            ui.add(egui::Label::new(egui::RichText::new(e).color(ui.visuals().error_fg_color)).wrap());
        }
    }
    state.commands.extend(out);
}

/// Opens the confirm dialog of a discard; the backup folder is named so the user knows where the copy goes.
fn ask_discard(state: &mut AppState, path: PathBuf, untracked: bool) {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut msg = if untracked {
        format!("Delete the untracked file {name}?")
    } else {
        format!("Discard the changes in {name}? The file goes back to its last staged or committed version.")
    };
    msg += &format!("\nA copy is kept first in {} (Undo is offered afterwards).", iron_sheets_app_core::git::discard_backup_folder().display());
    if state.docs.get(&path).is_some_and(|d| d.is_dirty()) {
        msg += "\nThe note has unsaved edits in the editor; it will report that the file changed on disk.";
    }
    state.dialogs.confirm("Discard changes", msg, "Discard", vec![Command::Module { id: "git.discard", arg: path.to_string_lossy().into_owned() }]);
}

#[derive(Default)]
pub struct GitPanel {
    snap: Option<Arc<GitSnapshot>>,
    rows: Vec<Row>,
    /// `counts` of each repo, refreshed with the rows.
    counts: Vec<(usize, usize)>,
    /// Commit message per repo root.
    msgs: FxHashMap<PathBuf, String>,
    /// Row index of the keyboard/click selection (a file row).
    sel: Option<usize>,
    /// Repo the commit box belongs to: that of the selected file, else the active note's.
    cur: usize,
    seen_serial: u64,
    scroll_y: f32,
    view_h: f32,
    scroll_to: Option<f32>,
}

impl GitPanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) {
        let take_focus = std::mem::take(&mut state.focus_sidebar);
        let Some(git) = GitModule::of(state) else {
            ui.add_space(8.0);
            ui.weak("Git is off (Settings, Modules).");
            return;
        };
        let snap = Arc::clone(git.snapshot());
        let last = git.last_outcome().cloned();
        match snap.availability {
            GitAvailability::Unavailable => {
                ui.add_space(8.0);
                ui.weak("The git program was not found. Install git to use this panel.");
                return;
            }
            GitAvailability::NotARepo => {
                let scanning = git.scanning();
                no_repo(ui, state, scanning, take_focus, last.as_ref());
                return;
            }
            GitAvailability::Ready => {}
        }
        if self.snap.as_ref().is_none_or(|s| !Arc::ptr_eq(s, &snap)) {
            // New snapshot: rebuild rows and counts; the selection follows its (repo, path), also to the other
            // section when a stage or unstage moved the file there.
            let key = self.sel.and_then(|i| self.rows.get(i)).and_then(|r| match (r, &self.snap) {
                (Row::File { repo, entry, staged }, Some(old)) => Some((*repo, old.repos.get(*repo)?.changes.get(*entry)?.rel.clone(), *staged)),
                _ => None,
            });
            self.rows = build_rows(&snap);
            self.counts = snap.repos.iter().map(counts).collect();
            self.sel = key.and_then(|(r, rel, staged)| {
                let at = |side: bool| self.rows.iter().position(|row| matches!(row, Row::File { repo, entry, staged: st } if *repo == r && *st == side && snap.repos[r].changes[*entry].rel == rel));
                at(staged).or_else(|| at(!staged))
            });
            self.snap = Some(Arc::clone(&snap));
            for r in &snap.repos {
                state.commands.push(Command::Module { id: "git.remote", arg: r.repo_root.to_string_lossy().into_owned() });
            }
        }
        if let Some(o) = &last {
            if o.serial != self.seen_serial {
                self.seen_serial = o.serial;
                if o.op == "Commit" && o.result.is_ok() {
                    self.msgs.remove(&o.repo_root);
                }
            }
        }
        if let Some(Row::File { repo, .. }) = self.sel.and_then(|i| self.rows.get(i)) {
            self.cur = *repo;
        } else if self.sel.is_none() && self.cur == 0 {
            let active = state.active_path();
            if let Some(r) = snap.active_repo(active.as_deref(), &state.workspace.roots) {
                self.cur = snap.repos.iter().position(|x| x.repo_root == r.repo_root).unwrap_or(0);
            }
        }
        self.cur = self.cur.min(snap.repos.len().saturating_sub(1));
        let Some(repo) = snap.repos.get(self.cur) else { return };
        let (busy, remote) = GitModule::of(state).map_or((None, None), |g| (g.busy(&repo.repo_root), g.remote_of(&repo.repo_root).map(str::to_string)));
        let remote_tip = |verb: &str, cmd: &str| remote.as_ref().map_or_else(|| cmd.to_string(), |u| format!("{verb} {u}\n({cmd})"));
        let mut out = Vec::new();
        let root = repo.repo_root.to_string_lossy().into_owned();
        let (staged_n, unstaged_n) = self.counts.get(self.cur).copied().unwrap_or_default();

        ui.horizontal(|ui| {
            ui.strong(repo_label(&repo.repo_root, &state.workspace.roots));
            ui.weak(repo.branch_display());
            let sync = repo.sync_text();
            if !sync.is_empty() {
                ui.weak(sync).on_hover_text(format!("Ahead of / behind {}", repo.upstream.as_deref().unwrap_or("its upstream")));
            }
        });
        let msg_id = egui::Id::NULL.with("git_commit_msg");
        let msg = self.msgs.entry(repo.repo_root.clone()).or_default();
        let msg_focused = ui.memory(|m| m.has_focus(msg_id));
        let mut commit = msg_focused && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
        if msg_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            state.focus_editor = true;
        }
        let r = ui.add_enabled(busy != Some("Commit"), egui::TextEdit::multiline(msg).id(msg_id).desired_rows(3).desired_width(f32::INFINITY).hint_text("Commit message (Ctrl+Enter to commit)"));
        crate::theme::name_field(&r, "Commit message");
        ui.horizontal_wrapped(|ui| {
            let can_commit = busy.is_none() && staged_n > 0;
            let b = ui.add_enabled(can_commit, egui::Button::new(format!("Commit ({staged_n})")));
            commit |= b.on_hover_text("Commit the staged files (Ctrl+Enter in the message box)").on_disabled_hover_text(if busy.is_some() { "Another Git operation is running" } else { "Nothing is staged" }).clicked();
            if ui.add_enabled(unstaged_n > 0 && busy.is_none(), egui::Button::new("Stage all")).clicked() {
                out.push(Command::Module { id: "git.stage_all", arg: root.clone() });
            }
            if ui.add_enabled(busy.is_none(), egui::Button::new("Push")).on_hover_text(remote_tip("Push to", "git push; needs an upstream branch")).clicked() {
                out.push(Command::Module { id: "git.push", arg: root.clone() });
            }
            if ui.add_enabled(busy.is_none(), egui::Button::new("Pull")).on_hover_text(remote_tip("Pull from", "git pull --ff-only")).clicked() {
                out.push(Command::Module { id: "git.pull", arg: root.clone() });
            }
            if ui.add_enabled(busy.is_none(), egui::Button::new("Fetch")).on_hover_text("git fetch --all: update the remote branches, change no file").clicked() {
                out.push(Command::Module { id: "git.fetch", arg: root.clone() });
            }
            if ui.button("Branches...").on_hover_text("Switch, create or delete a branch").clicked() {
                out.push(Command::Module { id: "git.branches", arg: root.clone() });
            }
            if ui.button("Remotes...").on_hover_text("List, add, rename or remove remotes").clicked() {
                out.push(Command::Module { id: "git.remotes", arg: root.clone() });
            }
        });
        if commit && busy.is_none() {
            out.push(Command::Module { id: "git.commit", arg: format!("{root}\0{msg}") });
        }
        // One status line: waiting while a long operation runs, else the last failure (success clears the box and shows in the status bar).
        if let Some(op) = busy {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                ui.weak(format!("{op}..."));
            });
        } else if let Some(Err(e)) = last.as_ref().map(|o| &o.result) {
            let o = last.as_ref();
            ui.add(egui::Label::new(egui::RichText::new(e).color(ui.visuals().error_fg_color)).wrap());
            if let Some(op @ ("Push" | "Pull")) = o.map(|o| o.op) {
                ui.weak(format!("Run `git {}` in a terminal.", op.to_lowercase()));
            }
        }
        ui.separator();

        if self.rows.is_empty() {
            ui.weak("Working tree clean. Nothing to commit.");
            state.commands.extend(out);
            return;
        }

        let pid = egui::Id::NULL.with("git_keys");
        if take_focus {
            ui.memory_mut(|m| m.request_focus(pid));
        }
        let nav = panel_keys(ui, pid);
        let pid_focused = ui.memory(|m| m.has_focus(pid));
        let space = pid_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space));
        let del = pid_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete));
        let n = self.rows.len();
        let page = ((self.view_h / ROW_H).floor() as usize).max(1);
        let next = match (nav.jump, nav.step) {
            (Some(Jump::Home), _) => step_file(&self.rows, None, 1),
            (Some(Jump::End), _) => step_file(&self.rows, None, -1),
            (Some(j), _) => {
                let d = if j == Jump::PageDown { 1 } else { -1 };
                let mut at = self.sel;
                for _ in 0..page {
                    match step_file(&self.rows, at, d) {
                        Some(i) => at = Some(i),
                        None => break,
                    }
                }
                at
            }
            (None, s) if s != 0 => step_file(&self.rows, self.sel, s).or(self.sel).or_else(|| step_file(&self.rows, None, s)),
            _ => None,
        };
        if let Some(i) = next {
            self.sel = Some(i);
            self.scroll_to = reveal(i, ROW_H, self.scroll_y, self.view_h);
        }
        let file_path = |repo: usize, entry: usize| -> (PathBuf, bool) {
            let r = &snap.repos[repo];
            let rel = &r.changes[entry].rel;
            (posix_rel_to_path(&r.repo_root, rel.trim_end_matches('/')), rel.ends_with('/'))
        };
        let toggle = |repo: usize, entry: usize, staged: bool| {
            let id = if staged { "git.unstage" } else { "git.stage" };
            Command::Module { id, arg: file_path(repo, entry).0.to_string_lossy().into_owned() }
        };
        // Discard works on the working tree: unstaged rows, not folders (collapsed untracked), not conflicts.
        let can_discard = |repo: usize, entry: usize, staged: bool| !staged && !snap.repos[repo].changes[entry].rel.ends_with('/') && snap.repos[repo].changes[entry].y != b'U';
        let mut ask: Option<(PathBuf, bool)> = None;
        if let Some(Row::File { repo, entry, staged }) = self.sel.and_then(|i| self.rows.get(i)).copied() {
            if space {
                out.push(toggle(repo, entry, staged));
            }
            if del && can_discard(repo, entry, staged) {
                ask = Some((file_path(repo, entry).0, snap.repos[repo].changes[entry].y == b'?'));
            }
            if nav.enter {
                let (path, is_dir) = file_path(repo, entry);
                if !is_dir {
                    out.push(Command::OpenPath { path, new_tab: false, line: None });
                }
            }
        }
        if nav.esc {
            state.focus_editor = true;
        }

        let mut area = egui::ScrollArea::vertical().id_salt("git_rows").auto_shrink(false);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let mut clicked = None;
        let mut pick_repo = None;
        let cur = self.cur;
        let sel_stroke = ui.visuals().selection.stroke;
        let ring = egui::Stroke::new((sel_stroke.width + 1.0).max(2.0), sel_stroke.color);
        let green = if ui.visuals().dark_mode { egui::Color32::from_rgb(90, 185, 115) } else { egui::Color32::from_rgb(35, 138, 70) };
        let sel = self.sel;
        let rows = &self.rows;
        let res = area.show_rows(ui, ROW_H, n, |ui, range| {
            let font = egui::TextStyle::Body.resolve(ui.style());
            for i in range {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let p = ui.painter().with_clip_rect(rect);
                match rows[i] {
                    Row::Repo(r) => {
                        // The commit box below the toolbar belongs to the repo whose header was clicked.
                        let rp = &snap.repos[r];
                        let (label, sync) = (repo_label(&rp.repo_root, &state.workspace.roots), rp.sync_text());
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Repository {label}")));
                        if r == cur {
                            p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        focus_ring(ui, rect, &resp);
                        let head = format!("{label}  {} {sync} ({})", rp.branch_display(), rp.dirty_count);
                        p.text(pos2(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, head, font.clone(), ui.visuals().strong_text_color());
                        if resp.clicked() {
                            pick_repo = Some(r);
                        }
                    }
                    Row::Section { staged, n, .. } => {
                        let t = format!("{} ({n})", if staged { "Staged Changes" } else { "Changes" });
                        p.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, t, font.clone(), ui.visuals().weak_text_color());
                    }
                    Row::File { repo, entry, staged } => {
                        let c = &snap.repos[repo].changes[entry];
                        let (path, is_dir) = file_path(repo, entry);
                        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()) + if is_dir { "/" } else { "" };
                        let dir = path.parent().and_then(|d| d.strip_prefix(&snap.repos[repo].repo_root).ok()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                        let letter = c.letter(staged);
                        let current = sel == Some(i);
                        let btn = egui::Rect::from_min_size(pos2(rect.right() - BTN_W, rect.top()), vec2(BTN_W, ROW_H));
                        let bresp = ui.interact(btn, resp.id.with("toggle"), Sense::click());
                        let verb = if staged { "Unstage" } else { "Stage" };
                        let discardable = can_discard(repo, entry, staged);
                        let dbtn = egui::Rect::from_min_size(pos2(rect.right() - 2.0 * BTN_W, rect.top()), vec2(BTN_W, ROW_H));
                        let dresp = discardable.then(|| ui.interact(dbtn, resp.id.with("discard"), Sense::click()));
                        if let Some(d) = &dresp {
                            d.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Discard {name}")));
                        }
                        resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, current, format!("{name} {letter} {}", if staged { "staged" } else { "not staged" })));
                        bresp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("{verb} {name}")));
                        if current {
                            p.rect_filled(rect, 2.0, selected_bg(ui.visuals()));
                        } else if resp.hovered() {
                            p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        if pid_focused && current {
                            p.rect_stroke(rect.shrink(1.0), crate::theme::tokens::RADIUS, ring, egui::StrokeKind::Inside);
                        } else {
                            focus_ring(ui, rect, &resp);
                        }
                        let col = match letter {
                            'A' | '?' => green,
                            'D' | 'U' => ui.visuals().error_fg_color,
                            _ => ui.visuals().warn_fg_color,
                        };
                        p.text(pos2(rect.left() + 14.0, rect.center().y), Align2::CENTER_CENTER, letter, font.clone(), col);
                        let kind = if current { RowKind::Active } else { RowKind::Normal };
                        let name_w = p.text(pos2(rect.left() + 28.0, rect.center().y), Align2::LEFT_CENTER, &name, font.clone(), row_text_color(ui.visuals(), kind, resp.hovered())).width();
                        if !dir.is_empty() {
                            p.text(pos2(rect.left() + 34.0 + name_w, rect.center().y), Align2::LEFT_CENTER, dir, font.clone(), ui.visuals().weak_text_color());
                        }
                        if resp.hovered() || bresp.hovered() || current {
                            p.rect_filled(btn.shrink2(vec2(2.0, 2.0)), 3.0, if bresp.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().widgets.inactive.weak_bg_fill });
                            p.text(btn.center(), Align2::CENTER_CENTER, if staged { "-" } else { "+" }, font.clone(), ui.visuals().strong_text_color());
                        }
                        if let (Some(d), true) = (&dresp, resp.hovered() || bresp.hovered() || current || dresp.as_ref().is_some_and(|d| d.hovered())) {
                            p.rect_filled(dbtn.shrink2(vec2(2.0, 2.0)), 3.0, if d.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().widgets.inactive.weak_bg_fill });
                            p.text(dbtn.center(), Align2::CENTER_CENTER, "x", font.clone(), ui.visuals().error_fg_color);
                        }
                        if dresp.as_ref().is_some_and(|d| d.clicked()) {
                            ask = Some((path, letter == '?'));
                            clicked = Some(i);
                        } else if bresp.clicked() {
                            out.push(toggle(repo, entry, staged));
                            clicked = Some(i);
                        } else if resp.clicked() {
                            if !is_dir {
                                out.push(Command::OpenPath { path, new_tab: false, line: None });
                            }
                            clicked = Some(i);
                        }
                        if bresp.hovered() {
                            bresp.on_hover_text(verb);
                        }
                        if let Some(d) = dresp.filter(|d| d.hovered()) {
                            d.on_hover_text(if letter == '?' { "Delete this untracked file (a copy is kept)" } else { "Discard the changes (a copy is kept)" });
                        }
                    }
                }
            }
        });
        if pid_focused && self.sel.is_none() {
            ui.painter().rect_stroke(res.inner_rect.shrink(1.0), crate::theme::tokens::RADIUS, ring, egui::StrokeKind::Inside);
        }
        self.scroll_y = res.state.clamped_offset().y;
        self.view_h = res.inner_rect.height();
        if let Some(i) = clicked {
            self.sel = Some(i);
            ui.memory_mut(|m| m.request_focus(pid));
        }
        if let Some(r) = pick_repo {
            (self.cur, self.sel) = (r, None);
            ui.memory_mut(|m| m.request_focus(pid));
        }
        if let Some((path, untracked)) = ask {
            ask_discard(state, path, untracked);
        }
        state.commands.extend(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_sheets_app_core::git::{ChangeEntry, RepoSnapshot};

    fn repo(name: &str, ch: &[(&str, u8, u8)]) -> RepoSnapshot {
        let root = PathBuf::from("/r").join(name);
        RepoSnapshot {
            repo_root: root.clone(),
            git_dir: root.join(".git"),
            common_dir: root.join(".git"),
            branch: "main".into(),
            detached: false,
            unborn: false,
            head_oid: None,
            dirty_count: ch.len(),
            files: Default::default(),
            dirs: Default::default(),
            changes: ch.iter().map(|(rel, x, y)| ChangeEntry { rel: rel.to_string(), x: *x, y: *y }).collect(),
            upstream: None,
            ahead: 0,
            behind: 0,
            aliases: Vec::new(),
        }
    }

    #[test]
    fn rows_group_by_repo_and_split_staged_from_unstaged() {
        let at_root = |names: &[usize]| names.iter().map(|&i| (PathBuf::from("/r").join(["a", "b"][i]), i)).collect();
        let one = GitSnapshot {
            availability: GitAvailability::Ready,
            repos: vec![repo("a", &[("x.md", b'M', b'M'), ("y.md", b'.', b'M'), ("z.md", b'?', b'?')])],
            root_to_repo: at_root(&[0]),
        };
        let rows = build_rows(&one);
        assert_eq!(
            rows,
            [
                Row::Section { repo: 0, staged: true, n: 1 },
                Row::File { repo: 0, entry: 0, staged: true },
                Row::Section { repo: 0, staged: false, n: 3 },
                Row::File { repo: 0, entry: 0, staged: false },
                Row::File { repo: 0, entry: 1, staged: false },
                Row::File { repo: 0, entry: 2, staged: false },
            ],
            "no repo header with one repo at a workspace folder; MM shows on both sides"
        );
        let nested = GitSnapshot { root_to_repo: Default::default(), ..one };
        assert_eq!(build_rows(&nested)[0], Row::Repo(0), "a lone repo below a workspace folder gets its path header");
        let two = GitSnapshot { availability: GitAvailability::Ready, repos: vec![repo("a", &[]), repo("b", &[("q.md", b'A', b'.')])], ..Default::default() };
        let rows = build_rows(&two);
        assert_eq!(rows[0], Row::Repo(0));
        assert_eq!(rows[1], Row::Repo(1));
        assert_eq!(rows.len(), 4);
        assert_eq!(step_file(&rows, None, 1), Some(3));
        assert_eq!(step_file(&rows, Some(3), 1), None);
        assert_eq!(step_file(&rows, None, -1), Some(3));
        assert_eq!(step_file(&rows, Some(3), -1), None, "headers are skipped");
    }

    #[test]
    fn repo_header_shows_the_path_below_the_workspace_folder() {
        let roots = [PathBuf::from("/w/notes"), PathBuf::from("/w/notes/deep")];
        assert_eq!(repo_label(std::path::Path::new("/w/notes"), &roots), "notes");
        assert_eq!(repo_label(std::path::Path::new("/w/notes/projects/foo"), &roots), "notes/projects/foo");
        assert_eq!(repo_label(std::path::Path::new("/w/notes/deep/x"), &roots), "deep/x", "the innermost workspace folder leads");
        assert_eq!(repo_label(std::path::Path::new("/elsewhere/repo"), &roots), "repo");
    }
}
