//! Virtualized file tree.
use crate::command::Command;
use crate::state::AppState;
use crate::theme::{RowKind, focus_ring, row_text_color, selected_bg};
use egui::{Id, Sense, WidgetInfo, pos2, vec2};
use iron_sheets_core::FileKind;
use iron_sheets_core::tree::{SortKey, TreeSort};
use std::path::{Path, PathBuf};

/// Tree indent per level and the left padding; the indent guides use the same values as the rows.
const INDENT: f32 = 14.0;
const PAD: f32 = 4.0;
/// Pointer travel from the press that turns it into a drag.
const DRAG_START: f32 = 8.0;

#[derive(Default)]
pub struct FilesPanel {
    /// `AppState::runtime_gen` this panel state belongs to; the tree was rebuilt when it differs.
    gen_seen: u64,
    pub last_active: Option<PathBuf>,
    /// Folder row the user clicked last; drawn as the active folder until the active file changes.
    pub clicked_dir: Option<PathBuf>,
    /// Row to focus once it is rendered (keyboard navigation moved there), with frames left to try.
    pending_focus: Option<(PathBuf, u8)>,
    /// Target path awaiting asynchronous ancestor expansion before scrolling to it.
    pending_reveal: Option<PathBuf>,
    visible: std::ops::Range<usize>,
    /// Row the pointer rests on: path, since when, whether the background read was started.
    hover: Option<(PathBuf, std::time::Instant)>,
    /// The tree changed its root: start at the top.
    scroll_top: bool,
    /// The one folder that is opening or closing right now.
    anim: Option<TreeAnim>,
    /// The file or folder being dragged to another folder.
    drag: Option<Drag>,
    /// Where the primary button went down last (egui forgets it on release).
    press: Option<egui::Pos2>,
    /// A file was released outside any tree folder: where and when. The tree is drawn before the editor and the tab strip,
    /// so they claim it in the same frame with `take_drop` if the pointer is over them; nobody else does, so it just lapses.
    /// One owner of the release (the tree) means a drop is never both a move and a link / new tab.
    released: Option<(PathBuf, egui::Pos2, u64)>,
    /// Frame in which the editor or the tab strip said it would take the dragged file (the ghost shows it a frame later).
    accepted: u64,
    /// A file dropped on the tab strip and the id of the tab it goes before (`None`: at the end); moved there once the open command ran.
    /// A tab id, not an index: opening may reuse a tab or find the file already open, which would shift an index.
    pub place_tab: Option<(PathBuf, Option<u64>)>,
    /// Cached `(normal, UPPERCASE)` labels for workspace roots, rebuilt only when `roots` changes.
    root_labels_roots: Vec<PathBuf>,
    root_labels: Vec<(String, String)>,
}

impl FilesPanel {
    /// Number of tree rows laid out and painted last frame (the viewport range, not the tree size).
    pub fn visible_rows(&self) -> usize {
        self.visible.len()
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// A file (no folder) is being dragged and was not cancelled: the editor and the tab strip may show a drop marker.
    pub fn dragged_file(&self, ctx: &egui::Context) -> bool {
        // The tree runs before the editor in a frame: an older stamp means it was not drawn (hidden mid-drag).
        self.drag.as_ref().is_some_and(|d| !d.is_dir && !d.cancelled && d.frame == ctx.cumulative_frame_nr())
    }

    /// The caller shows a drop marker for the dragged file: the ghost then says it is accepted.
    pub fn accept_drop(&mut self, ctx: &egui::Context) {
        self.accepted = ctx.cumulative_frame_nr();
    }

    /// The file released this frame over `area`, if any.
    pub fn take_drop(&mut self, ctx: &egui::Context, area: egui::Rect) -> Option<PathBuf> {
        let (_, p, f) = self.released.as_ref()?;
        if *f != ctx.cumulative_frame_nr() || !area.contains(*p) {
            return None;
        }
        self.released.take().map(|(path, ..)| path)
    }
}

struct Drag {
    path: PathBuf,
    name: String,
    is_dir: bool,
    /// Esc was pressed: nothing is dropped when the button is released.
    cancelled: bool,
    /// Frame of the last draw: a gap means the tree was not drawn (hidden sidebar, other view).
    frame: u64,
}

/// A folder block (the rows of its descendants) that grows (`opening`) or shrinks over the animation time. A closing folder stays expanded in the tree until the animation ends, so its rows are still there to draw.
struct TreeAnim {
    path: PathBuf,
    t0: f64,
    opening: bool,
}

/// Where the animated block is this frame: its first row, its row count, the fraction of it shown and the rows hidden.
struct Block {
    s: usize,
    k: usize,
    shown: f32,
    hidden: f32,
}

/// Index of the folder row and the number of its descendant rows (the rows after it that are deeper).
fn block_of(rows: &[iron_sheets_core::tree::TreeRow], path: &std::path::Path) -> Option<(usize, usize)> {
    let i = rows.iter().position(|r| !r.skeleton && r.entry.path == path)?;
    let d = rows[i].depth;
    Some((i, rows[i + 1..].iter().take_while(|r| r.depth > d).count()))
}

/// Apply a pending close: a closing folder stays expanded in the tree until its animation ends, so whatever ends it
/// early (another toggle, a tree reset, the Files view no longer drawn) must collapse it here.
pub fn finish_close(state: &mut AppState) {
    if let Some(a) = state.ui.files.anim.take() {
        if !a.opening {
            state.tree.set_expanded(&a.path, false);
        }
    }
}

/// Open or close a folder; with animations on (`dur` > 0), as a block animation. A toggle of the folder in flight
/// reverses it from where it is; any other one is finished first.
fn start_toggle(state: &mut AppState, path: &std::path::Path, now: f64, dur: f64) {
    if let Some(a) = state.ui.files.anim.as_mut().filter(|a| a.path == path && dur > 0.0) {
        // Same shown fraction after the flip: ease(q) = 1 - ease(p), with ease = cubic out, so q = 1 - cbrt(ease(p)).
        let p = crate::anim::ease_out(((now - a.t0) / dur) as f32);
        let q = 1.0 - p.cbrt();
        a.t0 = now - q as f64 * dur;
        a.opening = !a.opening;
        return;
    }
    finish_close(state);
    let opening = !state.tree.is_expanded(path);
    if dur <= 0.0 {
        state.tree.toggle(path);
        if opening {
            let _ = state.tree.rows();
        }
        return;
    }
    if opening {
        state.tree.set_expanded(path, true);
    }
    let k = block_of(state.tree.rows(), path).map_or(0, |(_, k)| k);
    if k == 0 {
        return state.tree.set_expanded(path, opening);
    }
    state.ui.files.anim = Some(TreeAnim { path: path.to_path_buf(), t0: now, opening });
}

/// How long the pointer rests on a note before it is read in the background.
const HOVER_PREFETCH: std::time::Duration = std::time::Duration::from_millis(150);

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let take_focus = std::mem::take(&mut state.focus_sidebar);
    if state.workspace.roots.is_empty() {
        ui.add_space(8.0);
        ui.label("No folders in this workspace yet.");
        let add_btn = ui.button("Add folder");
        if take_focus {
            add_btn.request_focus();
        }
        if add_btn.clicked() {
            state.commands.push(Command::AddRootDialog);
        }
        if ui.button("Add folder by path...").clicked() {
            state.commands.push(Command::AddRootByPath);
        }
        return;
    }
    if state.ui.files.gen_seen != state.runtime_gen {
        finish_close(state);
        state.ui.files = FilesPanel { gen_seen: state.runtime_gen, ..Default::default() };
    }
    let mut go: Option<Option<PathBuf>> = header(ui, state);
    let active = state.active_path();
    if active != state.ui.files.last_active {
        state.ui.files.last_active = active.clone();
        state.ui.files.clicked_dir = None;
        if state.reveal_request.is_none() {
            state.reveal_request = active.clone();
        }
    }
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let mut scroll_to: Option<f32> = None;
    if let Some(p) = state.reveal_request.take() {
        state.ui.files.pending_reveal = Some(p);
    }
    if let Some(p) = state.ui.files.pending_reveal.clone() {
        if let Some(r) = state.tree.reveal(&p) {
            state.ui.files.pending_reveal = None;
            if r < state.ui.files.visible.start || r + 1 >= state.ui.files.visible.end {
                scroll_to = Some((r as f32 - 3.0).max(0.0) * row_h);
            }
        } else if !state.tree.has_pending_loads() {
            state.ui.files.pending_reveal = None;
        }
    }
    let ctx = ui.ctx().clone();
    // A drag that ended while the tree was not drawn has nobody to drop it: forget it.
    if let Some(o) = ctx.input(|i| i.pointer.press_origin()) {
        state.ui.files.press = Some(o);
    }
    let frame = ctx.cumulative_frame_nr();
    if state.ui.files.drag.as_ref().is_some_and(|d| d.frame + 1 < frame && !ctx.input(|i| i.pointer.primary_down())) {
        state.ui.files.drag = None;
    }
    if let Some(d) = state.ui.files.drag.as_mut() {
        d.frame = frame;
        d.cancelled |= ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    }
    // Folder that takes the dragged item: the row under the pointer if it is a folder, else its parent; with the row range of
    // its block (the folder row and the rows below it), found while the rows are laid out.
    let mut drop_target: Option<(PathBuf, std::ops::Range<usize>)> = None;
    let drag_ptr = state.ui.files.drag.as_ref().filter(|d| !d.cancelled).and_then(|_| ctx.pointer_latest_pos());
    crate::theme::clear_rows(&ctx, "files");
    let active_dir = state.ui.files.clicked_dir.clone().or_else(|| active.as_ref().and_then(|a| a.parent().map(|p| p.to_path_buf())));
    if take_focus && state.ui.files.pending_focus.is_none() {
        let rows = state.tree.rows();
        let target = active
            .as_ref()
            .filter(|a| rows.iter().any(|r| !r.skeleton && &r.entry.path == *a))
            .cloned()
            .or_else(|| {
                state
                    .ui
                    .files
                    .clicked_dir
                    .as_ref()
                    .filter(|d| rows.iter().any(|r| !r.skeleton && &r.entry.path == *d))
                    .cloned()
            })
            .or_else(|| rows.iter().find(|r| !r.skeleton).map(|r| r.entry.path.clone()));
        if let Some(p) = target {
            state.ui.files.pending_focus = Some((p, 4));
        }
    }
    let mut pending = state.ui.files.pending_focus.take();
    if let Some((_, left)) = pending.as_mut() {
        *left = left.saturating_sub(1);
    }
    // Finish an animation that ran out (a closed folder leaves the tree only now), then place the animated block.
    let now = ui.input(|i| i.time);
    let dur = if crate::anim::enabled(crate::anim::Group::Tree) { ui.style().animation_time as f64 } else { 0.0 };
    if state.ui.files.anim.as_ref().is_some_and(|a| now - a.t0 >= dur) {
        finish_close(state);
    }
    let mut block = None;
    let mut anim_arrow: Option<(PathBuf, f32)> = None;
    if let Some(a) = &state.ui.files.anim {
        let p = crate::anim::ease_out(((now - a.t0) / dur) as f32);
        let open = if a.opening { p } else { 1.0 - p };
        match block_of(state.tree.rows(), &a.path) {
            Some((_, 0)) if a.opening => {
                state.ui.files.anim = None;
            }
            Some((i, k)) => {
                block = Some(Block { s: i + 1, k, shown: open * k as f32, hidden: (1.0 - open) * k as f32 });
                anim_arrow = Some((a.path.clone(), open));
                ctx.request_repaint();
            }
            None => finish_close(state),
        }
    }
    let n = state.tree.rows().len();
    // (global row index, path, depth, is_dir, expanded, key) of the row that has keyboard focus.
    let mut focused: Option<(usize, PathBuf, u16, bool, bool, Nav)> = None;
    let mut out: Vec<Command> = Vec::new();
    let mut toggles: Vec<PathBuf> = Vec::new();
    let mut prefetch: Vec<PathBuf> = Vec::new();
    let mut area = egui::ScrollArea::vertical().id_salt("file_tree").auto_shrink(false);
    if std::mem::take(&mut state.ui.files.scroll_top) {
        scroll_to = Some(0.0);
    }
    if let Some(y) = scroll_to {
        area = area.vertical_scroll_offset(y);
    }
    let roots = state.workspace.roots.clone();
    if state.ui.files.root_labels_roots != roots {
        state.ui.files.root_labels_roots = roots.clone();
        state.ui.files.root_labels = (0..roots.len())
            .map(|i| {
                let normal = state.workspace.root_label(i);
                let upper = normal.to_uppercase();
                (normal, upper)
            })
            .collect();
    }
    let root_labels = state.ui.files.root_labels.clone();
    let font_gen = state.ui.theme.generation;
    let has_focus_dir = state.tree.focus().is_some();
    let mut vis = 0..0;
    let tree_rect = ui.available_rect_before_wrap();
    let sort = state.workspace.tree_sort;
    let new_dir = state.tree.focus().map(PathBuf::from).or_else(|| roots.first().cloned());
    let mut reveal_active = false;
    let mut escape_to_editor = false;
    #[cfg(feature = "git")]
    let git_snap = iron_sheets_app_core::git::GitModule::of(state).map(|g| std::sync::Arc::clone(g.snapshot()));
    #[cfg(not(feature = "git"))]
    let git_snap = ();
    // Rows are laid out by hand (not `show_rows`) so the animated block can grow and shrink: only the rows in view are
    // placed, and the rows after the block move up by the hidden part of it.
    area.show_viewport(ui, |ui, viewport| {
        // Added before the rows, so a row on top of it gets the click: only the empty space is left to it.
        let bg = ui.interact(tree_rect, Id::NULL.with("tree_empty"), Sense::CLICK);
        let pitch = row_h + ui.spacing().item_spacing.y;
        let hidden = block.as_ref().map_or(0.0, |b| b.hidden);
        ui.set_height(((n as f32 - hidden) * pitch - ui.spacing().item_spacing.y).max(0.0));
        let row_at = |y: f32| {
            let r = y / pitch;
            if block.as_ref().is_some_and(|b| r >= b.s as f32 + b.shown) { r + hidden } else { r }
        };
        let first = (row_at(viewport.min.y).floor().max(0.0) as usize).min(n);
        let range = first..((row_at(viewport.max.y).ceil().max(0.0) as usize + 1).min(n)).max(first);
        vis = range.clone();
        if let (Some(p), Some(d)) = (drag_ptr.filter(|p| tree_rect.contains(*p)), state.ui.files.drag.as_ref()) {
            let rows = state.tree.rows();
            let under = row_at(p.y - ui.max_rect().top()).floor().max(0.0) as usize;
            // Below the last row there is no target.
            let dir = rows
                .get(under)
                .filter(|r| !r.skeleton)
                .and_then(|r| if r.entry.is_dir { Some(r.entry.path.clone()) } else { r.entry.path.parent().map(PathBuf::from) });
            if let Some(dir) = dir.filter(|dir| iron_sheets_app_core::sidebar::moves::can_drop(&d.path, dir)) {
                let block = block_of(rows, &dir).map_or(0..0, |(i, k)| i..i + k + 1);
                drop_target = Some((dir, block));
            }
        }
        let (left, top, width) = (ui.max_rect().left(), ui.max_rect().top(), ui.available_width());
        let base_clip = ui.clip_rect();
        // Fields are borrowed one by one (tree read-only, the rest mutable), so the visible rows need no copy.
        const SKEL_WIDTHS: [f32; 3] = [0.58, 0.74, 0.46];
        let all_rows = state.tree.rows();
        let rows = &all_rows[range.clone()];
        let guide = ui.visuals().widgets.noninteractive.bg_stroke;
        let gap_y = ui.spacing().item_spacing.y / 2.0;
        let mod_cmd = ui.input(|i| i.modifiers.command);
        let mut skel_fill: Option<egui::Color32> = None;
        for (local, row) in rows.iter().enumerate() {
            let e = &row.entry;
            let i = range.start + local;
            // Rows of the animated block that are not shown yet (or any more) are skipped; the last one is cut by the clip.
            let in_block = block.as_ref().filter(|b| i >= b.s && i < b.s + b.k);
            if in_block.is_some_and(|b| (i - b.s) as f32 >= b.shown) {
                continue;
            }
            let y = (i as f32 - block.as_ref().filter(|b| i >= b.s + b.k).map_or(0.0, |b| b.hidden)) * pitch;
            let rect = egui::Rect::from_min_size(pos2(left, top + y), vec2(width, row_h));
            ui.set_clip_rect(
                in_block.map_or(base_clip, |b| {
                    base_clip.intersect(egui::Rect::everything_above(top + (b.s as f32 + b.shown) * pitch - ui.spacing().item_spacing.y))
                }),
            );
            if let Some(note) = &row.note {
                for level in 0..row.depth {
                    let gx = rect.left() + PAD + level as f32 * INDENT + PAD;
                    ui.painter().line_segment([pos2(gx, rect.top() - gap_y), pos2(gx, rect.bottom() + gap_y)], guide);
                }
                let text = match note {
                    iron_sheets_core::tree::RowNote::Empty => "(empty)".to_string(),
                    iron_sheets_core::tree::RowNote::Unreadable(e) => format!("Can't read folder: {}", e.split(" (os error").next().unwrap_or(e)),
                };
                let font = egui::TextStyle::Body.resolve(ui.style());
                ui.painter().text(pos2(rect.left() + PAD + row.depth as f32 * INDENT + 14.0, rect.center().y), egui::Align2::LEFT_CENTER, text, font, ui.visuals().weak_text_color());
                continue;
            }
            if row.skeleton {
                let fill = *skel_fill.get_or_insert_with(|| {
                    let base = ui.visuals().widgets.inactive.weak_bg_fill;
                    let tint = ui.visuals().weak_text_color();
                    let blend = if dur > 0.0 { 0.14 + 0.24 * (0.5 + 0.5 * (now * std::f64::consts::TAU / 1.2).sin() as f32) } else { 0.24 };
                    base.lerp_to_gamma(tint, blend)
                });
                for level in 0..row.depth {
                    let gx = rect.left() + PAD + level as f32 * INDENT + PAD;
                    ui.painter().line_segment([pos2(gx, rect.top() - gap_y), pos2(gx, rect.bottom() + gap_y)], guide);
                }
                let skel_slot = all_rows[..i].iter().rev().take_while(|r| r.skeleton && r.depth == row.depth).count() % SKEL_WIDTHS.len();
                let bar_left = rect.left() + PAD + row.depth as f32 * INDENT + 14.0;
                let bar_right_limit = (rect.right() - 8.0).max(bar_left + 16.0);
                let avail_w = (bar_right_limit - bar_left).max(16.0);
                let bar_w = (avail_w * SKEL_WIDTHS[skel_slot]).max(16.0).min(avail_w);
                let bar_h = 10.5_f32.min((row_h - 4.0).max(6.0));
                let bar_rect = egui::Rect::from_center_size(pos2(bar_left + bar_w * 0.5, rect.center().y), vec2(bar_w, bar_h));
                ui.painter().rect_filled(bar_rect, 3.0, fill);
                continue;
            }
            let resp = ui.interact(rect, Id::NULL.with(("tree_row", &e.path)), Sense::click_and_drag());
            if pending.as_ref().is_some_and(|(p, _)| *p == e.path) {
                resp.request_focus();
                pending = None;
            }
            let root_idx = roots.iter().position(|r| *r == e.path);
            let name: &str = match root_idx {
                Some(i) => &root_labels[i].0,
                None => &e.name,
            };
            // A press starts a drag only once the pointer has left the row or moved `DRAG_START` px; less is a click.
            if resp.dragged_by(egui::PointerButton::Primary) && root_idx.is_none() && state.ui.files.drag.is_none() {
                let moved =
                    ctx.input(|i| i.pointer.press_origin().zip(i.pointer.latest_pos()).is_some_and(|(o, p)| !rect.contains(p) || o.distance(p) >= DRAG_START));
                if moved {
                    state.ui.files.drag = Some(Drag { path: e.path.clone(), name: e.name.clone(), is_dir: e.is_dir, cancelled: false, frame: ctx.cumulative_frame_nr() });
                }
            }
            // A folder with a folder note shows that note as active, since the note has no row of its own.
            let selected = active.as_ref() == Some(&e.path) || (row.folder_note.is_some() && row.folder_note == active);
            let is_active_dir = e.is_dir && active_dir.as_ref() == Some(&e.path);
            let (git_badge, git_dot, git_desc, is_ignored) = git_mark(ui, &git_snap, &e.path, e.is_dir);
            resp.widget_info(|| WidgetInfo::selected(egui::Role::TreeItem, true, selected || is_active_dir, name));
            if e.is_dir || git_desc.is_some() {
                ctx.accesskit_node_builder(resp.id, |b| {
                    if e.is_dir {
                        b.set_expanded(row.expanded);
                    }
                    match (e.is_dir && row.folder_note.is_some(), git_desc) {
                        (true, Some(s)) => b.set_description(format!("folder note, {s}")),
                        (true, None) => b.set_description("folder note"),
                        (false, Some(s)) => b.set_description(s),
                        (false, None) => {}
                    }
                });
            }
            if e.is_dir {
                // A collapsed folder cannot tell whether it has a folder note before its listing is read: read the one the
                // pointer or keyboard is on, in the background (collapsed folders are otherwise never read).
                if !row.expanded && root_idx.is_none() && row.folder_note.is_none() && (resp.hovered() || resp.has_focus()) {
                    prefetch.push(e.path.clone());
                }
            }
            if selected {
                ui.painter().rect_filled(rect, 2.0, selected_bg(ui.visuals()));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            if drop_target.as_ref().is_some_and(|(_, b)| b.contains(&i)) {
                ui.painter().rect_filled(rect, 2.0, selected_bg(ui.visuals()));
            }
            // Indent guides: one vertical segment per ancestor level, running through the row gap so they join up.
            for level in 0..row.depth {
                let gx = rect.left() + PAD + level as f32 * INDENT + PAD;
                ui.painter().line_segment([pos2(gx, rect.top() - gap_y), pos2(gx, rect.bottom() + gap_y)], guide);
            }
            focus_ring(ui, rect, &resp);
            if resp.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(resp.id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() });
                });
                let key = ui.input_mut(|i| {
                    let mut k = |key| i.consume_key(egui::Modifiers::NONE, key);
                    if k(egui::Key::ArrowDown) {
                        Nav::Down
                    } else if k(egui::Key::ArrowUp) {
                        Nav::Up
                    } else if k(egui::Key::ArrowRight) {
                        Nav::Right
                    } else if k(egui::Key::ArrowLeft) {
                        Nav::Left
                    } else if k(egui::Key::Home) {
                        Nav::Home
                    } else if k(egui::Key::End) {
                        Nav::End
                    } else if k(egui::Key::Escape) {
                        if has_focus_dir {
                            go = Some(None);
                        } else {
                            escape_to_editor = true;
                        }
                        Nav::None
                    } else {
                        Nav::None
                    }
                });
                focused = Some((i, e.path.clone(), row.depth, e.is_dir, row.expanded, key));
            }
            if resp.hovered() {
                resp.clone().on_hover_text(e.path.display().to_string());
                if !e.is_dir {
                    let h = &mut state.ui.files.hover;
                    match h {
                        // `request` ignores a repeat of the last request, and `take` forgets it: re-hovering a closed note works.
                        Some((p, since)) if *p == e.path => {
                            if since.elapsed() >= HOVER_PREFETCH {
                                if state.docs.get(&e.path).is_none() {
                                    state.docs.prefetch.request(&e.path);
                                }
                            } else {
                                ctx.request_repaint_after(HOVER_PREFETCH.saturating_sub(since.elapsed()));
                            }
                        }
                        _ => {
                            *h = Some((e.path.clone(), std::time::Instant::now()));
                            ctx.request_repaint_after(HOVER_PREFETCH);
                        }
                    }
                }
            }
            let x = rect.left() + PAD + row.depth as f32 * INDENT;
            let cy = rect.center().y;
            let kind = if selected || is_active_dir {
                RowKind::Active
            } else if is_ignored {
                RowKind::Dim
            } else if e.is_dir || !matches!(e.kind, FileKind::Other) {
                RowKind::Normal
            } else {
                RowKind::Dim
            };
            let color = row_text_color(ui.visuals(), kind, resp.hovered());
            crate::theme::record_row_with(&ctx, "files", || e.path.display().to_string(), color);
            if e.is_dir {
                let c = pos2(x + 4.0, cy);
                let t = anim_arrow.as_ref().filter(|(p, _)| *p == e.path).map_or(row.expanded as u8 as f32, |(_, t)| *t);
                crate::anim::arrow(ui.painter(), c, t, if is_active_dir { color } else { ui.visuals().weak_text_color() });
            }
            let font = egui::TextStyle::Body.resolve(ui.style());
            // A name that does not fit keeps its start and its end ("lo…ong.md"); results are cached per (name, 4 px width, font generation).
            const BADGE_W: f32 = 16.0;
            let has_badge = git_badge.is_some() || git_dot.is_some();
            let max_w = (rect.right() - (x + 14.0) - 4.0 - if has_badge { BADGE_W } else { 0.0 }).max(16.0);
            let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x;
            let label = match root_idx {
                Some(idx) => crate::elide::elide_cached(&root_labels[idx].1, max_w, font_gen, measure),
                None => crate::elide::elide_cached(name, max_w, font_gen, measure),
            };
            let painter = ui.painter().with_clip_rect(rect);
            painter.text(pos2(x + 14.0, cy), egui::Align2::LEFT_CENTER, &*label, font.clone(), color);
            if let Some((sym, col)) = git_badge {
                painter.text(pos2(rect.right() - 6.0, cy), egui::Align2::RIGHT_CENTER, sym, font, col);
            } else if let Some(col) = git_dot {
                painter.circle_filled(pos2(rect.right() - 9.0, cy), 3.0, col);
            }

            let new_tab = mod_cmd || resp.middle_clicked();
            // egui calls a press that moved a few px a drag, not a click: it is a click here when the release is still on
            // the pressed row, near the press, and our drag did not start.
            let released_here = || {
                let (press, p) = (state.ui.files.press, ctx.input(|i| i.pointer.latest_pos()));
                state.ui.files.drag.is_none() && press.zip(p).is_some_and(|(o, p)| rect.contains(p) && o.distance(p) < DRAG_START)
            };
            let clicked = resp.clicked() || (resp.drag_stopped_by(egui::PointerButton::Primary) && released_here());
            if clicked || resp.middle_clicked() {
                let enter = resp.has_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                // Folder note: the name and Enter open it (middle-click on the name in a new tab), the chevron still toggles / enters the folder.
                let on_name = resp.interact_pointer_pos().is_some_and(|p| p.x >= x + 14.0);
                if let (Some(note), true) = (&row.folder_note, enter || on_name) {
                    out.push(Command::OpenPath { path: note.clone(), new_tab, line: None });
                } else if e.is_dir {
                    if resp.middle_clicked() && root_idx.is_none() {
                        go = Some(Some(e.path.clone()));
                    }
                    if clicked {
                        toggles.push(e.path.clone());
                        state.ui.files.clicked_dir = Some(e.path.clone());
                    }
                } else {
                    out.push(Command::OpenPath { path: e.path.clone(), new_tab, line: None });
                }
            }
            let dir_for_new = if e.is_dir { e.path.clone() } else { e.path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
            crate::anim::context_menu(&resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                let mut item = |ui: &mut egui::Ui, text: &str, c: Command| {
                    if ui.button(text).clicked() {
                        out.push(c);
                        ui.close();
                    }
                };
                item(ui, "New note", Command::NewNote { dir: Some(dir_for_new.clone()) });
                item(ui, "New folder", Command::NewFolder { parent: Some(dir_for_new.clone()) });
                if e.is_dir && root_idx.is_none() && ui.button("Show only this folder").clicked() {
                    go = Some(Some(e.path.clone()));
                    ui.close();
                }
                if root_idx.is_none() {
                    item(ui, "Rename...", Command::Rename { path: e.path.clone() });
                    item(ui, "Move to trash", Command::Trash { path: e.path.clone() });
                }
                item(ui, "Reveal in file manager", Command::RevealInFileManager(e.path.clone()));
                item(ui, "Copy path", Command::CopyPath(e.path.clone()));
                if let Some(i) = root_idx {
                    item(ui, "Remove folder from workspace", Command::RemoveRoot(i));
                }
            });
        }
        if let Some((_, b)) = drop_target.as_ref().filter(|(_, b)| !b.is_empty()) {
            ui.set_clip_rect(base_clip);
            let top_y = |i: usize| top + (i as f32 - block.as_ref().filter(|a| i >= a.s + a.k).map_or(0.0, |a| a.hidden)) * pitch;
            // Rows of the animating folder that are not shown yet do not count.
            let last = match block.as_ref() {
                Some(a) if (a.s + a.shown.ceil() as usize..a.s + a.k).contains(&(b.end - 1)) => (a.s + a.shown.ceil() as usize).max(b.start + 1) - 1,
                _ => b.end - 1,
            };
            let r = egui::Rect::from_min_max(pos2(left, top_y(b.start)), pos2(left + width, top_y(last) + row_h));
            ui.painter().rect_stroke(r, 2.0, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
        }
        // Dragging near the top or bottom edge scrolls the tree.
        if let Some(p) = drag_ptr.filter(|p| tree_rect.x_range().contains(p.x)) {
            if p.y > tree_rect.bottom() - 28.0 {
                ui.scroll_with_delta(vec2(0.0, -12.0));
            } else if p.y < tree_rect.top() + 28.0 {
                ui.scroll_with_delta(vec2(0.0, 12.0));
            }
        }
        crate::anim::context_menu(&bg, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            if ui
                .add(egui::Button::new("New note").shortcut_text(state.keymap.index_of("file.new_note").map(|i| state.keymap.hint(i)).unwrap_or_default()))
                .clicked()
            {
                out.push(Command::NewNote { dir: new_dir.clone() });
                ui.close();
            }
            if ui.button("New folder").clicked() {
                out.push(Command::NewFolder { parent: new_dir.clone() });
                ui.close();
            }
            if ui.button("Add folder to workspace...").clicked() {
                out.push(Command::AddRootDialog);
                ui.close();
            }
            ui.menu_button("Sort", |ui| sort_items(ui, sort, &mut out));
            if ui.button("Collapse all").clicked() {
                out.push(Command::CollapseTree);
                ui.close();
            }
            if active.is_some() && ui.button("Reveal active note").clicked() {
                reveal_active = true;
                ui.close();
            }
        });
    });
    state.ui.files.visible = vis;
    if state.ui.files.drag.is_some() {
        if ctx.input(|i| i.pointer.primary_down()) {
            ctx.request_repaint();
            if let Some(d) = state.ui.files.drag.as_ref().filter(|d| !d.cancelled) {
                drag_ghost(&ctx, &d.name, drop_target.is_some() || state.ui.files.accepted + 1 == frame);
            }
        } else if let Some(d) = state.ui.files.drag.take() {
            match (d.cancelled, drop_target) {
                (false, Some((dir, _))) => out.push(Command::Move { to: dir.join(&d.name), from: d.path, links: None, dnd: true }),
                (false, None) if !d.is_dir => state.ui.files.released = ctx.pointer_latest_pos().map(|p| (d.path, p, frame)),
                _ => {}
            }
        }
    }
    if reveal_active {
        state.reveal_request = active.clone();
    }
    state.ui.files.pending_focus = pending.filter(|(_, left)| *left > 0);
    if let Some((idx, path, depth, is_dir, mut expanded, key)) = focused {
        let rows = state.tree.rows();
        // A closing folder counts as collapsed: its rows are about to go, so focus never moves into them.
        let closing = state.ui.files.anim.as_ref().filter(|a| !a.opening).and_then(|a| block_of(rows, &a.path));
        if closing.is_some_and(|(i, _)| i == idx) {
            expanded = false;
        }
        let skip = |t: usize| match closing {
            Some((i, k)) if t > i && t <= i + k => {
                if t > idx {
                    (i + k + 1 < rows.len()).then_some(i + k + 1)
                } else {
                    Some(i)
                }
            }
            _ => Some(t),
        };
        let target = match key {
            Nav::None => None,
            Nav::Down => (idx + 1..rows.len()).find(|&k| !rows[k].skeleton),
            Nav::Up => (0..idx).rev().find(|&k| !rows[k].skeleton),
            Nav::Home => (0..rows.len()).find(|&k| !rows[k].skeleton),
            Nav::End => (0..rows.len()).rev().find(|&k| !rows[k].skeleton),
            Nav::Right if is_dir && !expanded => {
                toggles.push(path.clone());
                None
            }
            Nav::Right => (is_dir && idx + 1 < rows.len() && rows[idx + 1].depth > depth && !rows[idx + 1].skeleton).then_some(idx + 1),
            Nav::Left if is_dir && expanded => {
                toggles.push(path.clone());
                None
            }
            Nav::Left => rows[..idx].iter().rposition(|r| !r.skeleton && r.depth < depth),
        };
        if let Some(t) = target.and_then(skip).and_then(|t| rows.get(t)).map(|r| r.entry.path.clone()) {
            state.ui.files.pending_focus = Some((t.clone(), 4));
            state.reveal_request = Some(t);
        }
    }
    for p in prefetch {
        state.tree.prefetch_dir(&p);
    }
    for p in toggles {
        start_toggle(state, &p, now, dur);
        ctx.request_repaint();
    }
    if let Some(dir) = go {
        state.tree.set_focus(dir);
        state.ui.files.scroll_top = true;
    }
    if escape_to_editor {
        state.focus_editor = true;
    }
    state.commands.extend(out);
}

/// The dragged item's name next to the pointer; the cursor shows whether the folder under it accepts it.
fn drag_ghost(ctx: &egui::Context, name: &str, can_drop: bool) {
    let Some(p) = ctx.pointer_latest_pos() else { return };
    ctx.set_cursor_icon(if can_drop { egui::CursorIcon::Grabbing } else { egui::CursorIcon::NoDrop });
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::NULL.with("tree_drag_ghost")));
    let v = ctx.global_style().visuals.clone();
    let galley = painter.layout_no_wrap(name.to_string(), egui::TextStyle::Body.resolve(&ctx.global_style()), v.text_color());
    let r = egui::Rect::from_min_size(p + vec2(14.0, 10.0), galley.size() + vec2(16.0, 6.0));
    painter.rect_filled(r.translate(vec2(1.5, 2.5)), 4.0, egui::Color32::from_black_alpha(60));
    painter.rect_filled(r, 4.0, v.widgets.hovered.weak_bg_fill);
    painter.rect_stroke(r, 4.0, egui::Stroke::new(1.0, v.selection.stroke.color), egui::StrokeKind::Inside);
    painter.galley(r.min + vec2(8.0, 3.0), galley, v.text_color());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    None,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

/// "Sort by" and "Order" as two radio groups. The menu stays open (see `anim::context_menu`) so both can be set.
fn sort_items(ui: &mut egui::Ui, sort: TreeSort, out: &mut Vec<Command>) {
    ui.weak("Sort by");
    for (key, label) in [(SortKey::Name, "Name"), (SortKey::Modified, "Date modified"), (SortKey::Created, "Date created")] {
        if ui.radio(sort.key == key, label).clicked() {
            out.push(Command::SetTreeSort(TreeSort { key, ..sort }));
        }
    }
    ui.separator();
    ui.weak("Order");
    for (desc, label) in [(false, "Ascending"), (true, "Descending")] {
        if ui.radio(sort.desc == desc, label).clicked() {
            out.push(Command::SetTreeSort(TreeSort { desc, ..sort }));
        }
    }
}

/// Sort button, and the breadcrumb while a folder is entered. Returns the folder to enter (`Some(None)` leaves it).
fn header(ui: &mut egui::Ui, state: &mut AppState) -> Option<Option<PathBuf>> {
    let mut go = None;
    let sort = state.workspace.tree_sort;
    ui.horizontal(|ui| {
        let (r, _) =
            crate::anim::menu_button_with(ui, Id::NULL.with("tree_sort_menu"), sort.label(), true, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                sort_items(ui, sort, &mut state.commands)
            });
        r.on_hover_text("Sort order");
        // In the sort row, not a line of its own: the tree never shifts when a scan starts or ends. The indexer wakes
        // the UI while it scans (throttled) and when it is done, so this needs no repaint of its own.
        let ix = state.indexer.status();
        if ix.scanning {
            ui.weak(format!("Indexing\u{2026} {} notes", ix.notes));
        }
    });
    if let Some(focus) = state.tree.focus().map(PathBuf::from) {
        let Some(root) = state.workspace.root_of(&focus) else { return Some(None) };
        let base = state.workspace.roots[root].clone();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if crate::icons::button(ui, crate::icons::Icon::Close, "Show the whole tree (Esc)").clicked() {
                go = Some(None);
            }
            // One crumb per path component; the root crumb leaves the folder view.
            let mut at = base.clone();
            let rel = focus.strip_prefix(&base).unwrap_or(&focus).components().count();
            for i in 0..=rel {
                if i > 0 {
                    ui.weak(">");
                    if let Some(c) = focus.strip_prefix(&base).ok().and_then(|r| r.components().nth(i - 1)) {
                        at.push(c);
                    }
                }
                let name = if i == 0 { state.workspace.root_label(root) } else { at.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() };
                if i == rel {
                    ui.strong(name);
                } else if ui.link(name).clicked() {
                    go = Some((i > 0).then(|| at.clone()));
                }
            }
        });
    }
    ui.separator();
    go
}
/// A tree row's Git badge (letter and colour), folder dot, accessible description, and whether the row is ignored.
type GitMark = (Option<(&'static str, egui::Color32)>, Option<egui::Color32>, Option<&'static str>, bool);

#[cfg(feature = "git")]
fn git_mark(ui: &egui::Ui, snap: &Option<std::sync::Arc<iron_sheets_app_core::git::GitSnapshot>>, path: &Path, is_dir: bool) -> GitMark {
    use iron_sheets_app_core::git::GitFileStatus as S;
    let git_green = if ui.visuals().dark_mode { egui::Color32::from_rgb(90, 185, 115) } else { egui::Color32::from_rgb(35, 138, 70) };
    let status = snap.as_ref().and_then(|s| s.status_of(path));
    let (badge, dot, desc) = match (is_dir, status) {
        (false, Some(S::Modified)) => (Some(("M", ui.visuals().warn_fg_color)), None, Some("modified")),
        (false, Some(S::Added)) => (Some(("A", git_green)), None, Some("added")),
        (false, Some(S::Untracked)) => (Some(("?", git_green)), None, Some("untracked")),
        (false, Some(S::Deleted)) => (Some(("D", ui.visuals().error_fg_color)), None, Some("deleted")),
        (false, Some(S::Conflict)) => (Some(("U", ui.visuals().error_fg_color)), None, Some("conflict")),
        (true, Some(S::Conflict)) => (Some(("U", ui.visuals().error_fg_color)), None, Some("conflict")),
        (true, Some(S::Modified | S::Added | S::Deleted | S::Untracked)) => (None, Some(ui.visuals().warn_fg_color), Some("dirty")),
        (_, Some(S::Ignored)) => (None, None, Some("ignored")),
        _ => (None, None, None),
    };
    (badge, dot, desc, status == Some(S::Ignored))
}

#[cfg(not(feature = "git"))]
fn git_mark(_: &egui::Ui, _: &(), _: &Path, _: bool) -> GitMark {
    (None, None, None, false)
}
