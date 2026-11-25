//! Git dialogs: the branch picker (palette style) and the remotes dialog. The lists come from the Git module (listed on its
//! worker when a dialog opens), every change goes out as a `git.*` command; nothing here waits for git.
use crate::command::Command;
use crate::state::AppState;
use egui::{Align2, Key, Modifiers, Sense, WidgetInfo, pos2, vec2};
use iron_sheets_app_core::git::{Branch, GitDialog, GitModule, RemoteInfo, remote_name_error, remote_url_error};
use iron_sheets_core::fuzzy::fuzzy_rank;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ROW_H: f32 = 24.0;
const LIST_H: f32 = 288.0;

/// A row of the branch picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PRow {
    /// Create a branch of this name from the current one.
    Create(String),
    /// Index into the branch list.
    Branch(usize),
}

/// Rows for `query`: the matches best first (the current branch first when nothing is typed), plus a create row for the
/// typed name unless a branch has exactly that name. The create row leads when nothing matches, else it closes the list,
/// so Enter on a partial name switches to the match instead of making a new branch.
pub(crate) fn picker_rows(list: &[Branch], query: &str) -> Vec<PRow> {
    let q = query.trim();
    let mut rows: Vec<PRow> = if q.is_empty() {
        let mut v: Vec<usize> = (0..list.len()).collect();
        v.sort_by_key(|&i| !list[i].current);
        v.into_iter().map(PRow::Branch).collect()
    } else {
        fuzzy_rank(q, list.iter().enumerate().map(|(i, b)| (i, b.name.as_str())), 200).into_iter().map(|(i, _)| PRow::Branch(i)).collect()
    };
    if !q.is_empty() && !list.iter().any(|b| b.name == q) {
        rows.push(PRow::Create(q.to_string()));
    }
    rows
}

struct Picker {
    root: PathBuf,
    query: String,
    sel: usize,
    focus: bool,
    scroll_y: f32,
    scroll_to: Option<f32>,
    error: Option<String>,
}

struct Remotes {
    root: PathBuf,
    name: String,
    url: String,
    /// `(from, new name)` while a remote is being renamed.
    rename: Option<(String, String)>,
    /// Outcomes up to this serial were there before the dialog opened.
    base_serial: u64,
    seen_serial: u64,
    focus: bool,
}

#[derive(Default)]
pub struct GitDialogs {
    picker: Option<Picker>,
    remotes: Option<Remotes>,
}

fn arg(root: &Path, payload: &str) -> String {
    format!("{}\0{payload}", root.display())
}

fn repo_name(root: &Path) -> String {
    root.file_name().map_or_else(|| root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

/// A button whose accessible name says which row it belongs to.
fn named_button(ui: &mut egui::Ui, text: &str, name: String, enabled: bool) -> egui::Response {
    let r = ui.add_enabled(enabled, egui::Button::new(text));
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, name.clone()));
    r
}

impl GitDialogs {
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) {
        if let Some(d) = GitModule::of_mut(state).and_then(GitModule::take_dialog) {
            match d {
                GitDialog::Branches(root) => self.picker = Some(Picker { root, query: String::new(), sel: 0, focus: true, scroll_y: 0.0, scroll_to: None, error: None }),
                GitDialog::Remotes(root) => {
                    let serial = GitModule::of(state).and_then(|g| g.last_outcome()).map_or(0, |o| o.serial);
                    self.remotes = Some(Remotes { root, name: String::new(), url: String::new(), rename: None, base_serial: serial, seen_serial: serial, focus: true });
                }
            }
        }
        if let Some(mut p) = self.picker.take() {
            if show_picker(&mut p, ctx, state) {
                self.picker = Some(p);
            }
        }
        if let Some(mut r) = self.remotes.take() {
            if show_remotes(&mut r, ctx, state) {
                self.remotes = Some(r);
            }
        }
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_some() || self.remotes.is_some()
    }
}

/// Returns whether the picker stays open.
fn show_picker(p: &mut Picker, ctx: &egui::Context, state: &mut AppState) -> bool {
    let list = GitModule::of(state).and_then(|g| g.branches(&p.root));
    let items: &[Branch] = match &list {
        Some(Ok(l)) => l,
        _ => &[],
    };
    let rows = picker_rows(items, &p.query);
    let current = items.iter().find(|b| b.current).map(|b| b.name.clone());
    p.sel = p.sel.min(rows.len().saturating_sub(1));
    let screen_h = ctx.content_rect().height();
    let top_gap = 70.0_f32.min(screen_h * 0.1);
    let list_h = LIST_H.min((screen_h - top_gap - 100.0).max(ROW_H * 2.0));

    // Keys are consumed before the text field sees them; a confirm dialog on top gets them instead.
    let (mut enter, mut delete) = (false, false);
    if state.dialogs.current.is_none() {
        let before = p.sel;
        ctx.input_mut(|i| {
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
            delete = i.consume_key(Modifiers::ALT, Key::Delete);
            let last = rows.len().saturating_sub(1);
            for (key, to) in [(Key::ArrowDown, (p.sel + 1).min(last)), (Key::ArrowUp, p.sel.saturating_sub(1)), (Key::PageDown, (p.sel + 8).min(last)), (Key::PageUp, p.sel.saturating_sub(8)), (Key::Home, 0), (Key::End, last)] {
                if i.consume_key(Modifiers::NONE, key) {
                    p.sel = to;
                }
            }
        });
        if p.sel != before {
            let top = p.sel as f32 * ROW_H;
            if top < p.scroll_y {
                p.scroll_to = Some(top);
            } else if top + ROW_H > p.scroll_y + list_h {
                p.scroll_to = Some(top + ROW_H - list_h);
            }
        }
    }

    let id = egui::Id::NULL.with("iron_sheets_git_branches");
    let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, top_gap));
    let mut clicked: Option<(usize, bool)> = None;
    let resp = egui::Modal::new(id).area(area).show(ctx, |ui| {
        ui.set_width(520.0_f32.min((ctx.content_rect().width() - 40.0).max(120.0)));
        let hint = format!("Switch branch of {}, or type a new branch name", repo_name(&p.root));
        let r = ui.add(egui::TextEdit::singleline(&mut p.query).hint_text(hint).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(6, 6)));
        crate::theme::name_field(&r, "Branch");
        if std::mem::take(&mut p.focus) {
            r.request_focus();
        }
        if r.changed() {
            p.sel = 0;
            p.scroll_to = Some(0.0);
            p.error = None;
        }
        ui.add_space(4.0);
        match &list {
            None => {
                ui.weak("Loading branches...");
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                return;
            }
            Some(Ok(l)) if l.is_empty() && p.query.trim().is_empty() => {
                ui.weak("No branches yet: commit first, then branches appear here.");
                return;
            }
            Some(Ok(_)) => {}
        }
        if let Some(e) = &p.error {
            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
        }
        let mut area = egui::ScrollArea::vertical().id_salt("git_branch_list").max_height(list_h).auto_shrink([false, true]);
        if let Some(y) = p.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        let sel = p.sel;
        let out = area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for i in range {
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let label = match &rows[i] {
                    PRow::Create(n) => format!("Create branch {n}"),
                    PRow::Branch(b) => items[*b].name.clone(),
                };
                resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, i == sel, &label));
                crate::theme::focus_ring(ui, rect, &resp);
                if i == sel {
                    ui.painter().rect_filled(rect, 3.0, crate::theme::selected_bg(ui.visuals()));
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let font = egui::TextStyle::Body.resolve(ui.style());
                let kind = if i == sel { crate::theme::RowKind::Active } else { crate::theme::RowKind::Normal };
                let col = crate::theme::row_text_color(ui.visuals(), kind, resp.hovered());
                let weak = ui.visuals().weak_text_color();
                let painter = ui.painter().with_clip_rect(rect);
                match &rows[i] {
                    PRow::Create(n) => {
                        let from = current.as_deref().map_or(String::new(), |c| format!(" from {c}"));
                        painter.text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, format!("Create branch {n}{from}"), font, col);
                    }
                    PRow::Branch(b) => {
                        let br = &items[*b];
                        let name_w = painter.text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, &br.name, font.clone(), col).width();
                        let mut x = rect.left() + 16.0 + name_w;
                        if br.current {
                            x += painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, "current", font.clone(), ui.visuals().selection.stroke.color).width() + 8.0;
                        }
                        painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &br.subject, egui::TextStyle::Small.resolve(ui.style()), weak);
                        if !br.current && (resp.hovered() || i == sel) {
                            let btn = egui::Rect::from_min_size(pos2(rect.right() - ROW_H, rect.top()), vec2(ROW_H, ROW_H));
                            let b_resp = ui.interact(btn, resp.id.with("del"), Sense::click());
                            b_resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Delete branch {}", br.name)));
                            painter.rect_filled(btn.shrink(2.0), 3.0, if b_resp.hovered() { ui.visuals().widgets.hovered.bg_fill } else { ui.visuals().widgets.inactive.weak_bg_fill });
                            painter.text(btn.center(), Align2::CENTER_CENTER, "x", egui::TextStyle::Body.resolve(ui.style()), ui.visuals().error_fg_color);
                            if b_resp.clicked() {
                                clicked = Some((i, true));
                                continue;
                            }
                        }
                    }
                }
                if resp.clicked() {
                    clicked = Some((i, false));
                }
            }
        });
        p.scroll_y = out.state.clamped_offset().y;
        ui.add_space(2.0);
        ui.weak("Enter: switch or create   Alt+Delete: delete a merged branch   Esc: close");
    });

    let act = clicked.or_else(|| (enter && !rows.is_empty()).then_some((p.sel, false))).or_else(|| (delete && !rows.is_empty()).then_some((p.sel, true)));
    let Some((i, del)) = act else { return !resp.should_close() };
    match (&rows[i], del) {
        (PRow::Create(n), false) => state.commands.push(Command::Module { id: "git.branch_new", arg: arg(&p.root, n) }),
        (PRow::Branch(b), false) if !items[*b].current => state.commands.push(Command::Module { id: "git.switch", arg: arg(&p.root, &items[*b].name) }),
        (PRow::Branch(b), true) if items[*b].current => {
            p.error = Some("The current branch cannot be deleted: switch to another one first".into());
            p.sel = i;
            return true;
        }
        (PRow::Branch(b), true) => {
            let name = items[*b].name.clone();
            let msg = format!("Delete the branch {name}? Only a fully merged branch can be deleted (git branch -d); git says no otherwise.");
            state.dialogs.confirm("Delete branch", msg, "Delete", vec![Command::Module { id: "git.branch_delete", arg: arg(&p.root, &name) }]);
            p.sel = i;
            return true;
        }
        _ => {}
    }
    false
}

/// Returns whether the dialog stays open.
fn show_remotes(r: &mut Remotes, ctx: &egui::Context, state: &mut AppState) -> bool {
    let list: Option<Result<Arc<[RemoteInfo]>, String>> = GitModule::of(state).and_then(|g| g.remote_list(&r.root));
    let (busy, last) = GitModule::of(state).map_or((None, None), |g| (g.busy(&r.root), g.last_outcome().cloned()));
    // Clear the add fields once an add worked; a rename ends with its result.
    if let Some(o) = last.as_ref().filter(|o| o.serial > r.seen_serial) {
        r.seen_serial = o.serial;
        if o.result.is_ok() {
            match o.op {
                "Add remote" => (r.name, r.url) = (String::new(), String::new()),
                "Rename remote" => r.rename = None,
                _ => {}
            }
        }
    }
    let note = last.filter(|o| o.serial > r.base_serial && matches!(o.op, "Add remote" | "Remove remote" | "Rename remote" | "Fetch"));
    let id = egui::Id::NULL.with("iron_sheets_git_remotes");
    let mut out: Vec<Command> = Vec::new();
    let mut close = false;
    let resp = egui::Modal::new(id).show(ctx, |ui| {
        ui.set_width(560.0_f32.min((ctx.content_rect().width() - 40.0).max(160.0)));
        ui.heading(format!("Remotes of {}", repo_name(&r.root)));
        ui.add_space(4.0);
        let fetching = busy == Some("Fetch");
        match &list {
            None => {
                ui.weak("Loading remotes...");
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            }
            Some(Ok(l)) if l.is_empty() => {
                ui.weak("No remotes yet. Add one below.");
            }
            Some(Ok(l)) => {
                let row_h = ui.spacing().interact_size.y;
                egui::ScrollArea::vertical().id_salt("git_remote_list").max_height(row_h * 6.0 + 20.0).auto_shrink([false, true]).show_rows(ui, row_h, l.len(), |ui, range| {
                    for i in range {
                        let rm = &l[i];
                        ui.horizontal(|ui| {
                            ui.add_sized([90.0, row_h], egui::Label::new(egui::RichText::new(&rm.name).strong()).truncate());
                            ui.add_sized([220.0, row_h], egui::Label::new(egui::RichText::new(&rm.url).weak()).truncate()).on_hover_text(&rm.url);
                            if named_button(ui, "Fetch", format!("Fetch {}", rm.name), !fetching && busy.is_none()).clicked() {
                                out.push(Command::Module { id: "git.fetch", arg: arg(&r.root, &rm.name) });
                            }
                            if named_button(ui, "Rename", format!("Rename remote {}", rm.name), busy.is_none()).clicked() {
                                r.rename = Some((rm.name.clone(), rm.name.clone()));
                            }
                            if named_button(ui, "Remove", format!("Remove remote {}", rm.name), busy.is_none()).clicked() {
                                let msg = format!("Remove the remote {}? Its remote-tracking branches are deleted from this repository; the server is not touched.", rm.name);
                                state.dialogs.confirm("Remove remote", msg, "Remove", vec![Command::Module { id: "git.remote_remove", arg: arg(&r.root, &rm.name) }]);
                            }
                        });
                    }
                });
            }
        }
        let mut cancel_rename = false;
        if let Some((from, to)) = &mut r.rename {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("Rename {from} to"));
                let te = ui.add(egui::TextEdit::singleline(to).desired_width(140.0));
                crate::theme::name_field(&te, "New remote name");
                let err = remote_name_error(to);
                let go = ui.add_enabled(err.is_none() && to != from, egui::Button::new("Rename to this name")).clicked() || (te.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && err.is_none());
                if go {
                    out.push(Command::Module { id: "git.remote_rename", arg: arg(&r.root, &format!("{from}\0{to}")) });
                }
                if ui.button("Cancel rename").clicked() {
                    cancel_rename = true;
                }
                if let (Some(e), false) = (err, to.is_empty()) {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });
        }
        if cancel_rename {
            r.rename = None;
        }
        ui.separator();
        ui.strong("New remote");
        let (name_err, url_err) = (remote_name_error(&r.name), remote_url_error(&r.url));
        let mut submit = false;
        ui.horizontal(|ui| {
            ui.label("Name");
            let n = ui.add(egui::TextEdit::singleline(&mut r.name).hint_text("origin").desired_width(110.0));
            crate::theme::name_field(&n, "Remote name");
            if std::mem::take(&mut r.focus) {
                n.request_focus();
            }
            ui.label("URL");
            let u = ui.add(egui::TextEdit::singleline(&mut r.url).hint_text("https://host/user/repo.git").desired_width(260.0));
            crate::theme::name_field(&u, "Remote URL");
            submit = u.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        });
        // Inline validation only for what was typed.
        for (typed, err) in [(!r.name.is_empty(), name_err), (!r.url.trim().is_empty(), url_err)] {
            if let (true, Some(e)) = (typed, err) {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        }
        ui.horizontal(|ui| {
            let ok = name_err.is_none() && url_err.is_none() && busy.is_none();
            if ui.add_enabled(ok, egui::Button::new("Add remote")).clicked() || (submit && ok) {
                out.push(Command::Module { id: "git.remote_add", arg: arg(&r.root, &format!("{}\0{}", r.name, r.url.trim())) });
            }
            if ui.add_enabled(busy.is_none() && list.as_ref().is_some_and(|l| l.as_ref().is_ok_and(|l| !l.is_empty())), egui::Button::new("Fetch all")).on_hover_text("git fetch --all").clicked() {
                out.push(Command::Module { id: "git.fetch", arg: r.root.to_string_lossy().into_owned() });
            }
            if ui.button("Close").clicked() {
                close = true;
            }
            if fetching {
                ui.add(egui::Spinner::new().size(12.0));
                ui.weak("Fetching...");
            }
        });
        // One result line: git's first error line, or what worked. A password prompt cannot appear here: run it in a terminal.
        match &note {
            Some(o) => match &o.result {
                Ok(t) => {
                    ui.weak(t);
                }
                Err(e) => {
                    ui.add(egui::Label::new(egui::RichText::new(e).color(ui.visuals().error_fg_color)).wrap());
                    if o.op == "Fetch" {
                        ui.weak("If it asks for a password, run `git fetch` in a terminal.");
                    }
                }
            },
            None => {
                ui.weak("");
            }
        }
    });
    state.commands.extend(out);
    !(close || resp.should_close())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(name: &str, current: bool) -> Branch {
        Branch { name: name.into(), current, subject: String::new() }
    }

    #[test]
    fn picker_rows_put_current_first_and_offer_create_only_for_new_names() {
        let list = [b("alpha", false), b("main", true), b("feature-x", false)];
        assert_eq!(picker_rows(&list, ""), [PRow::Branch(1), PRow::Branch(0), PRow::Branch(2)]);
        assert_eq!(picker_rows(&list, "  "), picker_rows(&list, ""), "blank is empty");
        // A partial name switches to its match first; creating it is the last row.
        assert_eq!(picker_rows(&list, "feat"), [PRow::Branch(2), PRow::Create("feat".into())]);
        // An exact name is no new branch; a name matching nothing leads with create.
        assert_eq!(picker_rows(&list, "main"), [PRow::Branch(1)]);
        assert_eq!(picker_rows(&list, "zzz"), [PRow::Create("zzz".into())]);
        assert_eq!(picker_rows(&[], "x"), [PRow::Create("x".into())]);
    }
}
