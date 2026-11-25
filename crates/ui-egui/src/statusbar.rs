//! Bottom status bar.
//!
//! Segments are laid out left to right in reading order (so the accessibility tree follows it too), none of them is
//! selectable text, and when the window is too narrow the least important ones are dropped and the rest is truncated
//! with an ellipsis instead of overlapping.
use crate::command::Command;
use crate::state::AppState;
use crate::statusbar_model::{DocBadge, WATCH_LIMIT_LABEL, WATCH_LIMIT_TIP, activity_text, message_text};
use egui::{Align, Color32, Layout, Rect, Ui, UiBuilder, pos2, vec2};
use rustc_hash::FxHashMap;
use std::cell::RefCell;

/// Gap between two segments.
const GAP: f32 = crate::theme::tokens::STATUS_GAP;
/// Width reserved before `Saved` / `Unsaved` for the 6 px status dot.
const DOT_W: f32 = 11.0;
/// A write shorter than this shows no "Saving…" badge.
const SAVING_CUE_AFTER: std::time::Duration = std::time::Duration::from_millis(150);

#[derive(Default)]
struct TextWCache {
    key: (u64, u32),
    widths: FxHashMap<String, f32>,
}

thread_local! {
    static TEXT_W_CACHE: RefCell<TextWCache> = RefCell::new(TextWCache::default());
}

fn text_w_cached(ui: &Ui, font_gen: u64, text: &str) -> f32 {
    let key = (font_gen, ui.ctx().pixels_per_point().to_bits());
    TEXT_W_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.key != key || c.widths.len() > 512 {
            c.key = key;
            c.widths.clear();
        }
        if let Some(&w) = c.widths.get(text) {
            return w;
        }
        let w = text_w_uncached(ui, text);
        c.widths.insert(text.to_string(), w);
        w
    })
}

fn text_w_uncached(ui: &Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter().layout_no_wrap(text.to_string(), font, Color32::WHITE).size().x
}

/// Which optional segments fit next to the mandatory ones. Pure, so it is unit-tested.
///
/// `fixed` is the width that always stays (the workspace button, shrunk to `min_fixed` at most); `optional` are
/// widths listed from the most to the least important segment. Returns how many of them are kept.
pub fn fit_optional(avail: f32, fixed: f32, optional: &[f32]) -> usize {
    let mut used = fixed;
    let mut kept = 0;
    for &w in optional {
        if used + GAP + w > avail {
            break;
        }
        used += GAP + w;
        kept += 1;
    }
    kept
}

/// `text` shortened with an ellipsis so it is at most `max_w` wide (as measured by `measure`).
pub fn ellipsize(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    // Longest prefix that still fits together with the ellipsis.
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        if measure(&cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>() + "…"
}

/// Text of the save badge; a failed save says what a click does.
fn badge_label(b: DocBadge) -> &'static str {
    if b.is_error() { "Save failed – retry" } else { b.text() }
}

fn dot_color(ui: &Ui, b: DocBadge) -> Option<Color32> {
    match b {
        DocBadge::Saved => Some(if ui.visuals().dark_mode { Color32::from_rgb(90, 185, 115) } else { Color32::from_rgb(35, 138, 70) }),
        DocBadge::Unsaved => Some(ui.visuals().warn_fg_color),
        _ => None,
    }
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let mut out: Vec<Command> = Vec::new();
    let ctx = ui.ctx().clone();
    // Status text is not meant to be selected or copied.
    ui.style_mut().interaction.selectable_labels = false;
    let pad = ui.spacing().button_padding.x * 2.0;
    let total = ui.available_width();
    let font_gen = state.ui.theme.generation;
    let text_w = |ui: &Ui, text: &str| text_w_cached(ui, font_gen, text);

    // Right group, in reading order: external change controls, save badge, counts, position.
    let mut right_ext: Option<(std::path::PathBuf, f32)> = None;
    let mut ext_compact = false;
    let mut ext_deleted = false;
    let mut badge: Option<(DocBadge, f32)> = None;
    let mut counts: Option<(String, f32)> = None;
    let mut pos: Option<(String, f32)> = None;
    let ws_floor = 90.0_f32.min(text_w(ui, &state.workspace.name) + pad + 14.0);
    if let Some(path) = state.active_path() {
        if let Some(doc) = state.docs.get(&path) {
            state.stats.update(&path, doc, state.tabs.active_tab().and_then(|t| t.cursor.as_ref()).map(|c| c.start));
            // "Saving…" only for a write that takes noticeably long: a fast autosave must not flash it for a frame.
            let slow = match state.docs.saving_for(&path) {
                Some(d) if d >= SAVING_CUE_AFTER => true,
                Some(d) => {
                    ui.ctx().request_repaint_after(SAVING_CUE_AFTER - d);
                    false
                }
                None => false,
            };
            let b = DocBadge::of_note(doc, state.tabs.is_untitled(&path)).saving(slow);
            // A hidden segment is neither measured nor drawn. The conflict controls below stay whatever the settings say.
            if state.config.status_badge || b.is_error() {
                let extra = if matches!(b, DocBadge::Saved | DocBadge::Unsaved) { DOT_W } else { 0.0 };
                badge = Some((b, text_w(ui, badge_label(b)) + extra));
            }
            if state.config.status_counts {
                let t = state.stats.counts_text();
                counts = Some((t.to_string(), text_w(ui, t)));
            }
            if state.config.status_position {
                let t = state.stats.position_text();
                pos = Some((t.to_string(), text_w(ui, t)));
            }
            if doc.external_change || doc.deleted {
                ext_deleted = doc.deleted;
                let (msg, a, b, b_short) = if ext_deleted { ("Deleted on disk", "Save again", "Close", "Close") } else { ("Changed on disk", "Reload", "Keep mine", "Keep") };
                let a_short = if ext_deleted { "Save" } else { "Reload" };
                let sp = ui.spacing().item_spacing.x;
                let full = text_w(ui, msg) + text_w(ui, a) + text_w(ui, b) + 2.0 * pad + 2.0 * sp * 2.0;
                let compact = text_w(ui, "!") + text_w(ui, a_short) + text_w(ui, b_short) + 2.0 * pad + 2.0 * sp * 2.0;
                // The conflict controls are never dropped: below the width where everything fits they shrink instead.
                ext_compact = ws_floor + GAP + full > total;
                right_ext = Some((path.clone(), if ext_compact { compact } else { full }));
            }
        }
    }
    let ws_name = state.workspace.name.clone();
    let show_ws = state.config.status_workspace;
    let ws_full = if show_ws { text_w(ui, &ws_name) + pad + 14.0 } else { 0.0 };
    let git_seg = git_seg::GitSeg::build(ui, state, &text_w);
    let idx_status = state.indexer.status();
    // The note count can be hidden; indexing progress is activity, so it always shows.
    let show_idx = state.config.status_notes || idx_status.scanning;
    let idx_text = if show_idx { state.stats.index_text(idx_status).to_string() } else { String::new() };
    let idx_w = if show_idx { text_w(ui, &idx_text) } else { 0.0 };
    // A transient message wins; otherwise what the app is loading in the background (link, Typst, syntax colouring).
    let msg = state.status.current().map(|m| (message_text(m).into_owned(), m.error)).or_else(|| {
        #[cfg(feature = "typst")]
        let typst = state.typst.warming();
        #[cfg(not(feature = "typst"))]
        let typst = None;
        let syntax = crate::preview::markdown::codehl::SyntectHighlighter::shared(&state.waker).is_loading();
        activity_text(state.pending_link.is_some(), typst, syntax).map(|t| (t.to_string(), false))
    });
    let msg_w = msg.as_ref().map_or(0.0, |(t, err)| text_w(ui, t) + if *err { 22.0 } else { 0.0 });
    // Persistent while the OS has no watch slots left: a label plus the Rescan action.
    let watch_limit = state.watch_limit_hit();
    let watch_w = if watch_limit { text_w(ui, WATCH_LIMIT_LABEL) + text_w(ui, "Rescan") + pad + ui.spacing().item_spacing.x } else { 0.0 };

    // Priorities, most important first. The workspace button always stays (it shrinks); everything else may be
    // dropped when the bar is too narrow.
    #[derive(Clone, Copy, PartialEq)]
    enum Seg {
        Ext,
        Badge,
        Watch,
        Msg,
        Git,
        Pos,
        Index,
        Counts,
    }
    let mut order: Vec<(Seg, f32)> = Vec::new();
    if let Some((_, w)) = &badge {
        order.push((Seg::Badge, *w));
    }
    if watch_limit {
        order.push((Seg::Watch, watch_w));
    }
    if msg.is_some() {
        // A message is shown truncated at worst; ask for a useful minimum.
        order.push((Seg::Msg, msg_w.min(120.0)));
    }
    if let Some((_, w)) = &git_seg {
        order.push((Seg::Git, *w));
    }
    if let Some((_, w)) = &pos {
        order.push((Seg::Pos, *w));
    }
    if show_idx {
        order.push((Seg::Index, idx_w));
    }
    if let Some((_, w)) = &counts {
        order.push((Seg::Counts, *w));
    }
    let ws_min = if show_ws { ws_full.min(90.0) } else { 0.0 };
    // An unsaved-conflict action has top priority: it is always shown and reserved before anything optional.
    let ext_w = right_ext.as_ref().map_or(0.0, |r| r.1 + GAP);
    let kept = fit_optional((total - ext_w).max(0.0), ws_min, &order.iter().map(|o| o.1).collect::<Vec<_>>());
    let shown = |s: Seg| if s == Seg::Ext { right_ext.is_some() } else { order[..kept].iter().any(|o| o.0 == s) };
    let right_w: f32 = [
        (Seg::Ext, right_ext.as_ref().map(|r| r.1)),
        (Seg::Badge, badge.as_ref().map(|r| r.1)),
        (Seg::Pos, pos.as_ref().map(|r| r.1)),
        (Seg::Counts, counts.as_ref().map(|r| r.1)),
    ]
    .iter()
    .filter(|(s, w)| shown(*s) && w.is_some())
    .map(|(_, w)| w.unwrap_or(0.0) + GAP)
    .sum();

    let bar = ui.available_rect_before_wrap();
    let left_rect = Rect::from_min_max(bar.min, pos2((bar.right() - right_w).max(bar.left()), bar.bottom()));
    let right_rect = Rect::from_min_max(pos2(left_rect.right(), bar.top()), bar.max);

    let mut dismiss_error = false;
    // Left group: workspace, git status, index status, transient message (takes what is left).
    ui.scope_builder(UiBuilder::new().max_rect(left_rect).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        let idx_shown = shown(Seg::Index);
        let git_shown = shown(Seg::Git);
        let git_reserve = if git_shown { git_seg.as_ref().map_or(0.0, |(_, w)| *w + GAP) } else { 0.0 };
        let reserve = if idx_shown { idx_w + GAP } else { 0.0 };
        let watch_reserve = if shown(Seg::Watch) { watch_w + GAP } else { 0.0 };
        let msg_reserve = if shown(Seg::Msg) { msg_w.min(120.0) + GAP } else { 0.0 };
        let ws_avail = (left_rect.width() - reserve - git_reserve - watch_reserve - msg_reserve).max(ws_min);
        let mut need_gap = false;
        if show_ws {
            need_gap = true;
            let ws_label = ellipsize(&ws_name, (ws_avail - pad - 14.0).max(20.0), |t| text_w_cached(ui, font_gen, t));
            let ws = ui.menu_button(ws_label, |ui| {
                for n in state.workspace_list() {
                    if ui.selectable_label(n == &ws_name, n).clicked() {
                        out.push(Command::SwitchWorkspace(n.clone()));
                        ui.close();
                    }
                }
                ui.separator();
                for (t, c) in [
                    ("New workspace...", Command::NewWorkspace),
                    ("Rename workspace...", Command::RenameWorkspace),
                    ("Delete workspace...", Command::DeleteWorkspace),
                    ("Add folder...", Command::AddRootDialog),
                ] {
                    if ui.button(t).clicked() {
                        out.push(c);
                        ui.close();
                    }
                }
            });
            ws.response.on_hover_text(format!("Workspace \"{ws_name}\": switch or manage workspaces"));
        }
        if let (true, Some((g, _))) = (git_shown, git_seg.as_ref()) {
            if need_gap {
                ui.add_space(GAP - ui.spacing().item_spacing.x);
            }
            need_gap = true;
            out.extend(g.show(ui));
        }
        if idx_shown {
            if need_gap {
                ui.add_space(GAP - ui.spacing().item_spacing.x);
            }
            need_gap = true;
            ui.add(egui::Label::new(idx_text.as_str()).selectable(false));
        }
        if shown(Seg::Watch) {
            if need_gap {
                ui.add_space(GAP - ui.spacing().item_spacing.x);
            }
            need_gap = true;
            ui.colored_label(ui.visuals().warn_fg_color, WATCH_LIMIT_LABEL).on_hover_text(WATCH_LIMIT_TIP);
            if ui.button("Rescan").on_hover_text("Scan the workspace and register the folders again").clicked() {
                out.push(Command::Rescan);
            }
        }
        if state.config.professional {
            if need_gap {
                ui.add_space(GAP - ui.spacing().item_spacing.x);
            }
            need_gap = true;
            if ui.small_button("Professional mode").on_hover_text("Customisation is hidden. Click to turn off").clicked() {
                out.push(Command::ToggleProfessional);
            }
        }
        if let (true, Some((text, error))) = (shown(Seg::Msg), msg.as_ref()) {
            if need_gap {
                ui.add_space(GAP - ui.spacing().item_spacing.x);
            }
            let color = if *error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
            let btn_reserve = if *error { ui.spacing().interact_size.y + ui.spacing().item_spacing.x } else { 0.0 };
            let max_lbl_w = (ui.available_width() - btn_reserve).max(0.0);
            let shown_text = ellipsize(text, max_lbl_w, |t| text_w_cached(ui, font_gen, t));
            let resp = ui.add(egui::Label::new(egui::RichText::new(shown_text).color(color)).selectable(false).sense(egui::Sense::hover()));
            if *error {
                resp.on_hover_text(text.as_str());
                if crate::icons::button(ui, crate::icons::Icon::Close, "Dismiss error").clicked() {
                    dismiss_error = true;
                }
            }
            if let Some(d) = state.status.next_expiry() {
                ctx.request_repaint_after(d);
            }
        }
    });
    if dismiss_error {
        state.status.msgs.retain(|m| !m.error);
    }

    let on_stroke = state.ui.theme.controls.on_stroke;
    // Right group, left to right, aligned to the right edge of the bar.
    ui.scope_builder(UiBuilder::new().max_rect(right_rect).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.add_space(GAP - ui.spacing().item_spacing.x);
        if let (true, Some((path, _))) = (shown(Seg::Ext), right_ext.as_ref()) {
            let msg = if ext_deleted { "Deleted on disk" } else { "Changed on disk" };
            let warn = ui.colored_label(ui.visuals().warn_fg_color, if ext_compact { "!" } else { msg });
            if ext_compact {
                warn.on_hover_text(if ext_deleted { "This file was deleted on disk" } else { "This file changed on disk" });
            }
            let (a_label, a_tip, a_cmd) = if ext_deleted {
                ("Save again", "Write the file again from my version", Command::SaveAgain(path.clone()))
            } else {
                ("Reload", "Reload the file from disk and discard my changes", Command::ReloadDoc(path.clone()))
            };
            let first = ui.add(egui::Button::new(if ext_compact && ext_deleted { "Save" } else { a_label }).stroke(on_stroke));
            if ext_compact {
                first.clone().on_hover_text(a_tip);
            }
            if first.clicked() {
                out.push(a_cmd);
            }
            let (b_label, b_tip, b_cmd) = if ext_deleted {
                ("Close", "Close the tab", Command::CloseActiveTab)
            } else {
                (if ext_compact { "Keep" } else { "Keep mine" }, "Keep my version and ignore the change on disk", Command::KeepMine(path.clone()))
            };
            let second = ui.add(egui::Button::new(b_label).stroke(on_stroke));
            if ext_compact {
                second.clone().on_hover_text(b_tip);
            }
            if second.clicked() {
                out.push(b_cmd);
            }
            ui.add_space(GAP - ui.spacing().item_spacing.x);
        }
        if let (true, Some((b, _))) = (shown(Seg::Badge), badge.as_ref()) {
            if b.is_error() {
                // Actionable: a click saves the document again (the failure reason is in the message and tooltip).
                let retry = ui.add(egui::Button::new(egui::RichText::new(badge_label(*b)).color(ui.visuals().error_fg_color)).frame(false));
                if retry.on_hover_text("Try saving again").clicked() {
                    out.push(Command::Save);
                }
            } else {
                if let Some(col) = dot_color(ui, *b) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let (dot_rect, _) = ui.allocate_exact_size(vec2(6.0, ui.available_height().min(14.0)), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 3.0, col);
                        ui.label(b.text());
                    });
                } else {
                    ui.label(b.text());
                }
            }
            ui.add_space(GAP - ui.spacing().item_spacing.x);
        }
        if let (true, Some((t, _))) = (shown(Seg::Counts), counts.as_ref()) {
            ui.label(t.as_str());
            ui.add_space(GAP - ui.spacing().item_spacing.x);
        }
        if let (true, Some((t, _))) = (shown(Seg::Pos), pos.as_ref()) {
            ui.label(t.as_str());
        }
    });
    ui.advance_cursor_after_rect(bar);
    toast(ui, state, &mut out);
    state.commands.extend(out);
}

/// The notice (see `AppState::notify`) above the bar's left corner. One repaint is scheduled for its end.
fn toast(ui: &Ui, state: &mut AppState, out: &mut Vec<Command>) {
    let Some(n) = &state.notice else { return };
    let now = std::time::Instant::now();
    if n.until <= now {
        state.notice = None;
        return;
    }
    let ctx = ui.ctx();
    ctx.request_repaint_after(n.until - now);
    let mut clicked = false;
    egui::Area::new(egui::Id::NULL.with("notice"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(pos2(ui.max_rect().left() + 8.0, ui.max_rect().top() - 8.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&n.text);
                    if let Some((label, cmd)) = &n.action {
                        if ui.button(*label).clicked() {
                            out.push(cmd.clone());
                            clicked = true;
                        }
                    }
                });
            });
        });
    if clicked {
        state.notice = None;
    }
}

/// The Git segment: branch, dirty count and repo tip, or "Git unavailable".
#[cfg(feature = "git")]
mod git_seg {
    use crate::state::AppState;
    use egui::{Ui, vec2};
    use iron_sheets_app_core::git::{GitAvailability, GitModule};

    pub enum GitSeg {
        Branch { branch: String, sync: String, repo_root: std::path::PathBuf, dirty: usize, dirty_text: String, tip: String },
        Unavailable,
    }

    impl GitSeg {
        /// The segment and its width, or `None` when Git is off or the active note is in no repo.
        pub fn build(ui: &Ui, state: &AppState, text_w: &dyn Fn(&Ui, &str) -> f32) -> Option<(GitSeg, f32)> {
            let snap = GitModule::of(state)?.snapshot();
            match snap.availability {
                GitAvailability::NotARepo => None,
                GitAvailability::Unavailable => Some((GitSeg::Unavailable, text_w(ui, "Git unavailable"))),
                GitAvailability::Ready => {
                    let repo = snap.active_repo(state.active_path().as_deref(), &state.workspace.roots)?;
                    let branch = repo.branch_display();
                    let dirty = repo.dirty_count;
                    let dirty_text = if dirty > 0 { format!("*{dirty}") } else { String::new() };
                    let state_desc = if dirty == 0 {
                        "working tree clean".to_string()
                    } else if dirty == 1 {
                        "1 uncommitted change".to_string()
                    } else {
                        format!("{dirty} uncommitted changes")
                    };
                    let head_note = if repo.unborn {
                        " (no commits yet)"
                    } else if repo.detached {
                        " (detached HEAD)"
                    } else {
                        ""
                    };
                    let sync = repo.sync_text();
                    let up = repo.upstream.as_deref().map_or_else(String::new, |u| format!("\nUpstream: {u} ({sync})"));
                    let tip = format!("Git branch: {branch}{head_note}\nRepo: {}\nStatus: {state_desc}{up}\nClick to switch branch", repo.repo_root.display());
                    let w = 14.0 + 4.0 + text_w(ui, &branch) + if sync.is_empty() { 0.0 } else { 4.0 + text_w(ui, &sync) } + if dirty > 0 { 4.0 + text_w(ui, &dirty_text) } else { 0.0 };
                    Some((GitSeg::Branch { branch, sync, repo_root: repo.repo_root.clone(), dirty, dirty_text, tip }, w))
                }
            }
        }

        /// Draws the segment; the command to run when the branch was clicked (opens the branch picker).
        pub fn show(&self, ui: &mut Ui) -> Option<iron_sheets_app_core::command::Command> {
            match self {
                GitSeg::Unavailable => {
                    ui.add(
                        egui::Label::new(egui::RichText::new("Git unavailable").color(ui.visuals().weak_text_color()))
                            .selectable(false)
                            .sense(egui::Sense::hover()),
                    )
                    .on_hover_text("System git executable was not found; install git to see branch and file status");
                    None
                }
                GitSeg::Branch { branch, sync, repo_root, dirty, dirty_text, tip } => {
                    let r = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let side = ui.available_height().min(14.0);
                        let (icon_rect, _) = ui.allocate_exact_size(vec2(14.0, side), egui::Sense::hover());
                        crate::icons::paint(ui.painter(), icon_rect, crate::icons::Icon::GitBranch, ui.visuals().weak_text_color());
                        ui.add(egui::Label::new(branch.as_str()).selectable(false));
                        if !sync.is_empty() {
                            ui.add(egui::Label::new(egui::RichText::new(sync.as_str()).color(ui.visuals().weak_text_color())).selectable(false));
                        }
                        if *dirty > 0 {
                            ui.add(egui::Label::new(egui::RichText::new(dirty_text.as_str()).color(ui.visuals().warn_fg_color)).selectable(false));
                        }
                    });
                    let click = ui.interact(r.response.rect, ui.scope_id().with("git_status_tip"), egui::Sense::click());
                    click.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Switch Git branch".to_string()));
                    let clicked = click.on_hover_text(tip.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                    clicked.then(|| iron_sheets_app_core::command::Command::Module { id: "git.branches", arg: repo_root.to_string_lossy().into_owned() })
                }
            }
        }
    }
}

/// Without the `git` feature there is never a Git segment.
#[cfg(not(feature = "git"))]
mod git_seg {
    use crate::state::AppState;
    use egui::Ui;

    pub enum GitSeg {}

    impl GitSeg {
        pub fn build(_: &Ui, _: &AppState, _: &dyn Fn(&Ui, &str) -> f32) -> Option<(GitSeg, f32)> {
            None
        }

        pub fn show(&self, _: &mut Ui) -> Option<iron_sheets_app_core::command::Command> {
            match *self {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_segments_drop_from_the_end_of_the_priority_list() {
        // 100 fixed + (12 + 50) each.
        assert_eq!(fit_optional(1000.0, 100.0, &[50.0, 50.0, 50.0]), 3);
        assert_eq!(fit_optional(100.0 + 62.0 * 2.0, 100.0, &[50.0, 50.0, 50.0]), 2);
        assert_eq!(fit_optional(100.0 + 62.0 * 2.0 - 1.0, 100.0, &[50.0, 50.0, 50.0]), 1);
        assert_eq!(fit_optional(50.0, 100.0, &[50.0]), 0);
        // A wide high-priority segment blocks the cheaper ones behind it: order is strict, never reshuffled.
        assert_eq!(fit_optional(200.0, 100.0, &[150.0, 10.0]), 0);
    }

    #[test]
    fn text_w_cache_matches_uncached() {
        let ctx = egui::Context::default();
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let a = text_w_uncached(ui, "Ln 12, Col 4");
            let b = text_w_cached(ui, 1, "Ln 12, Col 4");
            let c = text_w_cached(ui, 1, "Ln 12, Col 4");
            assert!((a - b).abs() < 0.01);
            assert_eq!(b, c);
        });
        frame.textures_delta.clear();
    }

    #[test]
    fn ellipsize_fits_the_width() {
        let m = |t: &str| t.chars().count() as f32 * 10.0;
        assert_eq!(ellipsize("short", 100.0, m), "short");
        let e = ellipsize("a very long workspace name", 100.0, m);
        assert!(e.ends_with('…') && m(&e) <= 100.0, "{e}");
        assert_eq!(e, "a very lo…");
        assert_eq!(ellipsize("abc", 5.0, m), "…");
    }
}
