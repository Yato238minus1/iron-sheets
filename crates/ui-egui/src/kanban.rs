//! Obsidian-Kanban board view: a note with front matter `kanban-plugin` shows as lanes of cards in Preview. The data stays
//! the Markdown text (`iron_sheets_md::kanban`); every change is a text edit on the document.
use crate::command::Command;
use crate::state::AppState;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, UiBuilder, WidgetInfo, pos2, vec2};
use iron_sheets_core::FileKind;
use iron_sheets_md::kanban::{Board, is_board, merge, parse, valid};
use std::ops::Range;
use std::sync::{Arc, Mutex};

const LANE_W: f32 = 280.0;
const PAD: f32 = 8.0;
const GAP: f32 = 6.0;
const BOX: f32 = 16.0;

enum Target {
    Card(usize),
    NewCard,
    Title,
}

struct Editing {
    lane: usize,
    target: Target,
    buf: String,
    fresh: bool,
    /// Document version the lane and card indices belong to.
    version: u64,
}

#[derive(Default)]
struct KState {
    path: std::path::PathBuf,
    version: u64,
    board: Option<Arc<Board>>,
    /// Height of each card plus gap, per lane: estimated until the card has been drawn once.
    heights: Vec<Vec<f32>>,
    editing: Option<Editing>,
    drag: Option<(usize, usize)>,
    /// Lane being dragged by its header.
    lane_drag: Option<usize>,
    /// Lane with cards waiting for "Delete" to be confirmed.
    confirm: Option<usize>,
    /// Scroll offsets as of the last frame (horizontal; vertical per lane) and the ones auto-scroll asks for next frame.
    h_off: f32,
    v_off: Vec<f32>,
    h_req: Option<f32>,
    v_req: Option<(usize, f32)>,
}

fn estimate(text: &str) -> f32 {
    2.0 * PAD + 17.0 * text.lines().map(|l| l.chars().count() / 30 + 1).sum::<usize>() as f32 + GAP
}

fn state_of(ctx: &egui::Context, tab_id: u64) -> Arc<Mutex<KState>> {
    ctx.data_mut(|d| d.get_temp_mut_or_insert_with(Id::NULL.with(("kanban", tab_id)), || Arc::new(Mutex::new(KState::default()))).clone())
}

/// True when the tab's note is a board (checked once per document version).
pub fn is_board_tab(ui: &egui::Ui, state: &AppState, tab_index: usize) -> bool {
    let Some(tab) = state.tabs.tabs.get(tab_index) else { return false };
    if !state.modules.enabled(iron_sheets_app_core::modules::KANBAN) {
        // Module off: keep no board state (an open board tab's state is dropped here).
        ui.ctx().data_mut(|d| d.remove_temp::<Arc<Mutex<KState>>>(Id::NULL.with(("kanban", tab.id))));
        return false;
    }
    let Some(doc) = state.docs.get(&tab.path).filter(|d| d.kind == FileKind::Markdown) else { return false };
    let ks = state_of(ui.ctx(), tab.id);
    let mut ks = ks.lock().unwrap_or_else(|e| e.into_inner());
    if ks.path != tab.path || ks.version != doc.version {
        let board = is_board(&doc.text).then(|| parse(&doc.text));
        (ks.drag, ks.lane_drag, ks.confirm) = (None, None, None);
        if ks.editing.as_ref().is_some_and(|e| e.version != doc.version) {
            ks.editing = None;
        }
        ks.heights = board.iter().flat_map(|b| &b.lanes).map(|l| l.cards.iter().map(|c| estimate(&c.text)).collect()).collect();
        ks.board = board.map(Arc::new);
        ks.path = tab.path.clone();
        ks.version = doc.version;
    }
    ks.board.is_some()
}

enum Link {
    Wiki(String),
    Url(String),
}

/// Cheap inline Markdown: `**bold**`, `*italic*`, `` `code` ``, `[[wiki|alias]]`, `[text](url)`. Returns the job and the
/// char ranges of the links.
fn inline_job(text: &str, done: bool, ui: &egui::Ui, width: f32) -> (LayoutJob, Vec<(Range<usize>, Link)>) {
    let v = ui.visuals();
    let base = if done { v.weak_text_color() } else { v.text_color() };
    let font = FontId::proportional(14.0);
    let fmt = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        strikethrough: if done { Stroke::new(1.0, color) } else { Stroke::NONE },
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    let mut links = Vec::new();
    let (mut plain, mut i) = (0, 0);
    while i < text.len() {
        let rest = &text[i..];
        let close = |open: usize, pat: &str| rest[open..].find(pat).filter(|&e| e > 0).map(|e| (open, e));
        // (display, consumed bytes, format, link)
        let tok: Option<(&str, usize, TextFormat, Option<Link>)> = if rest.starts_with("**") {
            close(2, "**").map(|(o, e)| (&rest[o..o + e], o + e + 2, fmt(v.strong_text_color()), None))
        } else if rest.starts_with('`') {
            close(1, "`")
                .map(|(o, e)| (&rest[o..o + e], o + e + 1, TextFormat { font_id: FontId::monospace(13.0), background: v.code_bg_color, ..fmt(base) }, None))
        } else if rest.starts_with("[[") {
            close(2, "]]").map(|(o, e)| {
                let inner = &rest[o..o + e];
                let (target, shown) = inner.split_once('|').unwrap_or((inner, inner));
                let f = TextFormat { underline: Stroke::new(1.0, v.hyperlink_color), ..fmt(v.hyperlink_color) };
                (shown, o + e + 2, f, Some(Link::Wiki(target.to_string())))
            })
        } else if rest.starts_with('[') {
            close(1, "](").and_then(|(o, e)| {
                let url_len = rest[o + e + 2..].find(')')?;
                let f = TextFormat { underline: Stroke::new(1.0, v.hyperlink_color), ..fmt(v.hyperlink_color) };
                Some((&rest[o..o + e], o + e + 2 + url_len + 1, f, Some(Link::Url(rest[o + e + 2..o + e + 2 + url_len].to_string()))))
            })
        } else if rest.starts_with('*') && !rest[1..].starts_with([' ', '*']) {
            close(1, "*").map(|(o, e)| (&rest[o..o + e], o + e + 1, TextFormat { italics: true, ..fmt(base) }, None))
        } else {
            None
        };
        let Some((shown, used, f, link)) = tok else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        job.append(&text[plain..i], 0.0, fmt(base));
        let from = job.text.chars().count();
        job.append(shown, 0.0, f);
        if let Some(l) = link {
            links.push((from..job.text.chars().count(), l));
        }
        i += used;
        plain = i;
    }
    job.append(&text[plain..], 0.0, fmt(base));
    (job, links)
}

fn open_link(cmds: &mut Vec<Command>, from: &std::path::Path, link: &Link, new_tab: bool) {
    match link {
        Link::Url(u) if u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:") => cmds.push(Command::OpenUrl(u.clone())),
        Link::Url(t) | Link::Wiki(t) => {
            let (note, heading) = match t.split_once('#') {
                Some((n, h)) => (n, Some(h.to_string())),
                None => (t.as_str(), None),
            };
            let note = note.strip_suffix(".md").unwrap_or(note).to_string();
            if !note.is_empty() {
                cmds.push(Command::OpenLink { note, heading, from: Some(from.to_path_buf()), new_tab });
            }
        }
    }
}

/// Result of a one-line/multi-line inline editor: Enter saves, Esc cancels, losing focus saves.
fn edit_field(ui: &mut egui::Ui, e: &mut Editing, width: f32, rows: usize) -> Option<bool> {
    let enter = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let esc = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    let out = ui.add(egui::TextEdit::multiline(&mut e.buf).desired_width(width).desired_rows(rows));
    crate::theme::name_field(&out, "Card text");
    if e.fresh {
        out.request_focus();
        e.fresh = false;
        return None;
    }
    if esc {
        Some(false)
    } else if enter || out.lost_focus() {
        Some(true)
    } else {
        None
    }
}

enum Action {
    Toggle(usize, usize),
    EditCard(usize, usize, String),
    Rename(usize, String),
    AddCard(usize, String),
    /// After lane `n`, or first when `None`.
    AddLane(Option<usize>),
    Move((usize, usize), (usize, usize)),
    DeleteLane(usize),
    MoveLane(usize, usize),
}

#[derive(Default)]
struct Out {
    actions: Vec<Action>,
    cmds: Vec<Command>,
    /// Visible cards (lane, index, rect) and lane rects, for the drop marker.
    rects: Vec<(usize, usize, Rect)>,
    lanes: Vec<Rect>,
    /// Card list viewport of each lane, and the board's viewport (for auto-scroll).
    lists: Vec<Rect>,
    view: Option<Rect>,
}

pub fn show(ui: &mut egui::Ui, state: &mut AppState, tab_index: usize) {
    let Some(tab) = state.tabs.tabs.get(tab_index) else { return };
    let (path, tab_id) = (tab.path.clone(), tab.id);
    let ks = state_of(ui.ctx(), tab_id);
    let mut ks = ks.lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = ks.board.clone() else { return };
    let doc = state.docs.get(&path);
    let read_only = doc.is_none_or(|d| d.read_only);
    let text = doc.map_or("", |d| d.text.as_str());
    let mut out = Out { view: Some(ui.available_rect_before_wrap()), ..Default::default() };
    let h = (ui.available_height() - 12.0).max(120.0);
    ks.v_off.resize(board.lanes.len(), 0.0);
    let mut area = egui::ScrollArea::horizontal().id_salt(("kanban_h", tab_id)).auto_shrink(false);
    if let Some(x) = ks.h_req.take() {
        area = area.horizontal_scroll_offset(x);
    }
    let shown = area.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.add_space(PAD);
            for lane in 0..board.lanes.len() {
                show_lane(ui, &mut ks, &board, text, lane, h, &path, read_only, &mut out);
            }
            if board.lanes.is_empty() && !read_only && ui.button("+ Add lane").clicked() {
                out.actions.push(Action::AddLane(None));
            }
        });
    });
    ks.h_off = shown.state.unclamped_offset().x;
    if let Some(from) = ks.drag {
        // A drag ends with the button: dropped where the pointer is, or cancelled (Esc, or the release was missed).
        let (down, released) = ui.input(|i| (i.pointer.any_down(), i.pointer.any_released()));
        let p = ui.ctx().pointer_latest_pos();
        let to = p.and_then(|p| drop_target(&out, p));
        if let Some(p) = p.filter(|_| down) {
            drag_feedback(ui, &board, &out, from, to, p);
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) || (!down && !released) {
            ks.drag = None;
        } else if !down {
            ks.drag = None;
            out.actions.extend(to.map(|to| Action::Move(from, to)));
        }
    }
    if let Some(from) = ks.lane_drag {
        let (down, released) = ui.input(|i| (i.pointer.any_down(), i.pointer.any_released()));
        let p = ui.ctx().pointer_latest_pos();
        let to = p.map(|p| lane_slot(&out, p));
        if let Some((p, to)) = p.zip(to).filter(|_| down) {
            lane_feedback(ui, &board, &out, from, to, p);
        }
        if ui.input(|i| i.key_pressed(Key::Escape)) || (!down && !released) {
            ks.lane_drag = None;
        } else if !down {
            ks.lane_drag = None;
            out.actions.extend(to.map(|to| Action::MoveLane(from, to)));
        }
    }
    if let Some(p) = ui.ctx().pointer_latest_pos().filter(|_| (ks.drag.is_some() || ks.lane_drag.is_some()) && ui.input(|i| i.pointer.any_down())) {
        autoscroll(ui, &mut ks, &out, p);
    }
    apply_actions(state, &path, &board, &mut ks, out.actions, read_only);
    drop(ks);
    state.commands.extend(out.cmds);
}

/// Turns the frame's actions into edits against the current text, drops those that clash with an earlier one, and applies the
/// rest only when all ranges are valid.
fn apply_actions(state: &mut AppState, path: &std::path::Path, board: &Board, ks: &mut KState, actions: Vec<Action>, read_only: bool) {
    let Some(doc) = state.docs.get_mut(path).filter(|d| !read_only && !d.read_only && d.version == ks.version) else { return };
    let mut all = Vec::new();
    let mut new_lane = None;
    for a in actions {
        let edits = match &a {
            Action::Toggle(l, i) => board.toggle(*l, *i),
            Action::EditCard(l, i, t) => board.edit_text(&doc.text, *l, *i, t),
            Action::Rename(l, t) => board.rename_lane(*l, t),
            Action::AddCard(l, t) => board.add_card(*l, t),
            Action::AddLane(after) => board.add_lane(&doc.text, *after, "New lane"),
            Action::Move(from, to) => board.move_card(&doc.text, *from, *to),
            Action::DeleteLane(l) => board.delete_lane(*l),
            Action::MoveLane(from, to) => board.move_lane(&doc.text, *from, *to),
        };
        if merge(&mut all, edits) {
            if let Action::AddLane(after) = a {
                new_lane = Some(after.map_or(0, |a| a + 1));
            }
        }
    }
    if !valid(&doc.text, &all) {
        return;
    }
    for e in &all {
        doc.apply_edit(e.range.clone(), &e.insert);
    }
    // The new lane is renamed right away; it exists from the next frame on.
    if let (Some(lane), false) = (new_lane, all.is_empty()) {
        ks.editing = Some(Editing { lane, target: Target::Title, buf: "New lane".into(), fresh: true, version: doc.version });
    }
}

/// (lane, slot) a dragged card would land in: the lane under `p.x`, before the first visible card whose middle is below `p.y`.
fn drop_target(out: &Out, p: egui::Pos2) -> Option<(usize, usize)> {
    let lane = out.lanes.iter().position(|r| p.x >= r.left() && p.x < r.right() + GAP + PAD)?;
    let mine = || out.rects.iter().filter(|r| r.0 == lane);
    let slot = mine().find(|r| r.2.center().y > p.y).map(|r| r.1).or_else(|| mine().next_back().map(|r| r.1 + 1)).unwrap_or(0);
    Some((lane, slot))
}

/// Slot (0..=len) a dragged lane would land in: before the first lane whose middle is right of `p.x`.
fn lane_slot(out: &Out, p: egui::Pos2) -> usize {
    out.lanes.iter().position(|r| r.center().x > p.x).unwrap_or(out.lanes.len())
}

/// While a lane is dragged: an insertion marker between lanes and a ghost with its title.
fn lane_feedback(ui: &egui::Ui, board: &Board, out: &Out, from: usize, to: usize, p: egui::Pos2) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::NULL.with("kanban_ghost")));
    let accent = ui.visuals().selection.stroke.color;
    let x = match (out.lanes.get(to), to.checked_sub(1).and_then(|i| out.lanes.get(i))) {
        (Some(r), _) => Some(r.left() - GAP / 2.0),
        (None, Some(r)) => Some(r.right() + GAP / 2.0),
        _ => None,
    };
    if let Some(x) = x {
        let v = out.view.unwrap_or(Rect::EVERYTHING);
        let x = x.clamp(v.left() + 1.5, v.right() - 1.5); // a lane scrolled out of view: the marker stays at the edge
        painter.line_segment([pos2(x, v.top() + 4.0), pos2(x, v.bottom() - 4.0)], Stroke::new(2.5, accent));
    }
    if let Some(l) = board.lanes.get(from) {
        let r = Rect::from_min_size(p + vec2(8.0, 4.0), vec2(LANE_W / 2.0, 30.0));
        painter.rect_filled(r.translate(vec2(1.5, 2.5)), 4.0, Color32::from_black_alpha(60));
        painter.rect_filled(r, 4.0, ui.visuals().window_fill());
        painter.rect_stroke(r, 4.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
        let color = ui.visuals().strong_text_color();
        let g = painter.layout_no_wrap(l.title.clone(), FontId::proportional(14.0), color);
        painter.galley(r.min + vec2(6.0, 6.0), g, color);
    }
}

/// Distance from the edge of the board (or a card list) at which a drag starts scrolling it, and the top speed in points per second.
const EDGE: f32 = 56.0;
const SPEED: f32 = 1100.0;

/// Scrolls the board sideways while a drag is near its left or right edge, and the card list under the pointer up or down when a
/// card is dragged near its top or bottom. The new offsets are applied by the next frame.
fn autoscroll(ui: &egui::Ui, ks: &mut KState, out: &Out, p: egui::Pos2) {
    let dt = ui.input(|i| i.stable_dt).min(0.05);
    // 0 at the inner end of the zone, 1 at the edge and beyond.
    let push = |inside: f32| ((EDGE - inside) / EDGE).clamp(0.0, 1.0) * SPEED * dt;
    let Some(v) = out.view else { return };
    let dx = if p.x < v.left() + EDGE {
        -push(p.x - v.left())
    } else if p.x > v.right() - EDGE {
        push(v.right() - p.x)
    } else {
        0.0
    };
    if dx != 0.0 {
        ks.h_req = Some((ks.h_off + dx).max(0.0));
    }
    if ks.drag.is_some() {
        let lane = out.lanes.iter().position(|r| p.x >= r.left() && p.x < r.right() + GAP + PAD);
        if let Some((lane, l)) = lane.and_then(|i| out.lists.get(i).map(|l| (i, l))) {
            let dy = if p.y < l.top() + EDGE {
                -push(p.y - l.top())
            } else if p.y > l.bottom() - EDGE {
                push(l.bottom() - p.y)
            } else {
                0.0
            };
            if dy != 0.0 {
                ks.v_req = Some((lane, (ks.v_off.get(lane).copied().unwrap_or(0.0) + dy).max(0.0)));
            }
        }
    }
    if ks.h_req.is_some() || ks.v_req.is_some() {
        ui.ctx().request_repaint();
    }
}

/// While a card is dragged: an insertion marker where it would land and a ghost that follows the pointer.
fn drag_feedback(ui: &egui::Ui, board: &Board, out: &Out, from: (usize, usize), to: Option<(usize, usize)>, p: egui::Pos2) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::NULL.with("kanban_ghost")));
    let accent = ui.visuals().selection.stroke.color;
    if let Some((l, slot)) = to {
        let at = |i: usize| out.rects.iter().find(|r| r.0 == l && r.1 == i).map(|r| r.2);
        let y = match (at(slot), slot.checked_sub(1).and_then(at)) {
            (Some(r), _) => r.top() - GAP / 2.0,
            (None, Some(r)) => r.bottom() + GAP / 2.0,
            _ => out.lanes[l].top() + 40.0,
        };
        let lr = out.lanes[l];
        painter.line_segment([pos2(lr.left() + PAD, y), pos2(lr.right() - PAD, y)], Stroke::new(2.5, accent));
    }
    if let Some(c) = board.lanes.get(from.0).and_then(|l| l.cards.get(from.1)) {
        let r = Rect::from_min_size(p + vec2(8.0, 4.0), vec2(LANE_W - 2.0 * PAD, 30.0));
        painter.rect_filled(r.translate(vec2(1.5, 2.5)), 4.0, Color32::from_black_alpha(60));
        painter.rect_filled(r, 4.0, ui.visuals().window_fill());
        painter.rect_stroke(r, 4.0, Stroke::new(1.0, accent), egui::StrokeKind::Inside);
        let color = ui.visuals().text_color();
        let g = painter.layout_no_wrap(c.text.lines().next().unwrap_or("").to_string(), FontId::proportional(14.0), color);
        painter.galley(r.min + vec2(6.0, 6.0), g, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn show_lane(ui: &mut egui::Ui, ks: &mut KState, board: &Board, text: &str, lane: usize, h: f32, path: &std::path::Path, read_only: bool, out: &mut Out) {
    let l = &board.lanes[lane];
    let (rect, _) = ui.allocate_exact_size(vec2(LANE_W, h), Sense::hover());
    ui.add_space(GAP);
    out.lanes.push(rect);
    ui.painter().rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink(PAD)).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    ui.set_clip_rect(rect.intersect(ui.clip_rect()));
    let w = rect.width() - 2.0 * PAD;
    // Header: title (or its editor), card count, menu.
    let (mut rename, mut add, mut del) = (false, false, false);
    let renaming = ks.editing.as_ref().is_some_and(|e| e.lane == lane && matches!(e.target, Target::Title));
    if !read_only && !renaming {
        // Registered before the header widgets, so the menu button on top still gets its clicks.
        let grip = Rect::from_min_size(rect.min + vec2(PAD, PAD), vec2(w, 24.0));
        let r = ui.interact(grip, Id::NULL.with(("kanban_lane_grip", path, lane)), Sense::drag()).on_hover_cursor(egui::CursorIcon::Grab);
        r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Move lane {}", l.title)));
        if r.drag_started() {
            (ks.lane_drag, ks.drag) = (Some(lane), None);
        }
    }
    ui.horizontal(|ui| {
        if let Some(e) = ks.editing.as_mut().filter(|e| e.lane == lane && matches!(e.target, Target::Title)) {
            match edit_field(ui, e, w - 8.0, 1) {
                Some(true) => {
                    if e.buf.trim() != l.title {
                        out.actions.push(Action::Rename(lane, e.buf.clone()));
                    }
                    ks.editing = None;
                }
                Some(false) => ks.editing = None,
                None => {}
            }
        } else {
            ui.add(egui::Label::new(egui::RichText::new(&l.title).strong()).truncate());
            ui.weak(l.cards.len().to_string());
            if !read_only {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.menu_button("\u{2026}", |ui| {
                        rename = ui.button("Rename lane").clicked();
                        add = ui.button("Add lane after").clicked();
                        del = ui.button("Delete lane").clicked();
                        if rename || add || del {
                            ui.close();
                        }
                    });
                    r.response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, "Lane menu"));
                });
            }
        }
    });
    if rename {
        ks.editing = Some(Editing { lane, target: Target::Title, buf: l.title.clone(), fresh: true, version: ks.version });
    }
    if add {
        out.actions.push(Action::AddLane(Some(lane)));
    }
    if del && board.lane_is_empty(text, lane) {
        out.actions.push(Action::DeleteLane(lane));
    } else if del {
        ks.confirm = Some(lane);
    }
    if ks.confirm == Some(lane) {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Delete this lane and its {} cards?", l.cards.len()));
            if ui.button("Delete").clicked() {
                out.actions.push(Action::DeleteLane(lane));
                ks.confirm = None;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                ks.confirm = None;
            }
        });
    }
    let adding = ks.editing.as_ref().is_some_and(|e| e.lane == lane && matches!(e.target, Target::NewCard));
    let footer = if adding { 64.0 } else { 34.0 };
    let list_h = (ui.available_height() - footer).max(40.0);
    // Cards are virtualised: only those in the viewport are laid out; the rest are spacers of their (estimated) height.
    let mut area = egui::ScrollArea::vertical().id_salt(("kanban_v", lane)).max_height(list_h).auto_shrink([false, false]);
    if let Some((_, y)) = ks.v_req.filter(|r| r.0 == lane) {
        area = area.vertical_scroll_offset(y);
        ks.v_req = None;
    }
    let shown = area.show_viewport(ui, |ui, vp| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let n = l.cards.len();
        let (mut y, mut first) = (0.0, 0);
        while first < n && y + ks.heights[lane][first] <= vp.min.y {
            y += ks.heights[lane][first];
            first += 1;
        }
        ui.add_space(y);
        let mut idx = first;
        while idx < n && y < vp.max.y {
            y += show_card(ui, ks, board, lane, idx, w, path, read_only, out);
            idx += 1;
        }
        ui.add_space(ks.heights[lane][idx..].iter().sum());
    });
    ks.v_off[lane] = shown.state.unclamped_offset().y;
    out.lists.push(shown.inner_rect);
    if ks.lane_drag == Some(lane) {
        ui.painter().rect_filled(rect, 6.0, ui.visuals().window_fill().gamma_multiply(0.5));
    }
    if let Some(e) = ks.editing.as_mut().filter(|e| e.lane == lane && matches!(e.target, Target::NewCard)) {
        match edit_field(ui, e, w, 2) {
            Some(true) => {
                if !e.buf.trim().is_empty() {
                    out.actions.push(Action::AddCard(lane, e.buf.trim().to_string()));
                }
                ks.editing = None;
            }
            Some(false) => ks.editing = None,
            None => {}
        }
    } else if !read_only && ui.add(egui::Button::new("+ Add card").frame(false)).clicked() {
        ks.editing = Some(Editing { lane, target: Target::NewCard, buf: String::new(), fresh: true, version: ks.version });
    }
}

/// Draws one card; returns its height including the gap below.
#[allow(clippy::too_many_arguments)]
fn show_card(
    ui: &mut egui::Ui,
    ks: &mut KState,
    board: &Board,
    lane: usize,
    idx: usize,
    w: f32,
    path: &std::path::Path,
    read_only: bool,
    out: &mut Out,
) -> f32 {
    let c = &board.lanes[lane].cards[idx];
    if let Some(e) = ks.editing.as_mut().filter(|e| e.lane == lane && matches!(e.target, Target::Card(i) if i == idx)) {
        let r = ui.scope(|ui| edit_field(ui, e, w, 2));
        match r.inner {
            Some(true) => {
                if !e.buf.trim().is_empty() && e.buf.trim() != c.text {
                    out.actions.push(Action::EditCard(lane, idx, e.buf.trim().to_string()));
                }
                ks.editing = None;
            }
            Some(false) => ks.editing = None,
            None => {}
        }
        ui.add_space(GAP);
        return r.response.rect.height() + GAP;
    }
    let tw = w - 2.0 * PAD - BOX - 6.0;
    let (job, links) = inline_job(&c.text, c.done, ui, tw);
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, galley.size().y.max(BOX) + 2.0 * PAD), Sense::click_and_drag());
    ui.add_space(GAP);
    ks.heights[lane][idx] = rect.height() + GAP;
    out.rects.push((lane, idx, rect));
    let v = ui.visuals();
    let faded = ks.drag == Some((lane, idx));
    let bg = if faded { v.window_fill().gamma_multiply(0.4) } else { v.window_fill() };
    let stroke = if resp.hovered() { v.widgets.hovered.bg_stroke } else { v.widgets.noninteractive.bg_stroke };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, bg);
    painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);
    // Checkbox, created after the card so it wins clicks on its own area.
    let cb = Rect::from_min_size(rect.min + vec2(PAD, PAD), vec2(BOX, BOX));
    let cb_resp = ui.interact(cb, resp.id.with("done"), Sense::click());
    cb_resp.widget_info(|| WidgetInfo::selected(egui::Role::CheckBox, !read_only, c.done, &c.text));
    let painter = ui.painter();
    painter.rect_stroke(cb, 3.0, Stroke::new(1.5, v.widgets.inactive.fg_stroke.color), egui::StrokeKind::Inside);
    if c.done {
        painter.rect_filled(cb.shrink(2.0), 2.0, v.selection.bg_fill);
        let m = |x: f32, y: f32| cb.min + vec2(x, y);
        painter.add(egui::Shape::line(vec![m(4.0, 8.5), m(7.0, 11.5), m(12.0, 5.0)], Stroke::new(2.0, v.selection.stroke.color)));
    }
    let text_pos = rect.min + vec2(PAD + BOX + 6.0, PAD);
    painter.galley(text_pos, galley.clone(), v.text_color());
    let link_at = |p: egui::Pos2| {
        let i = galley.cursor_from_pos(p - text_pos).index.0;
        links.iter().find(|(r, _)| r.contains(&i)).map(|(_, l)| l)
    };
    if !read_only && cb_resp.clicked() {
        out.actions.push(Action::Toggle(lane, idx));
    }
    if !read_only && resp.drag_started() {
        ks.drag = Some((lane, idx));
    }
    if !read_only && resp.double_clicked() {
        ks.editing = Some(Editing { lane, target: Target::Card(idx), buf: c.text.clone(), fresh: true, version: ks.version });
    } else if let Some(p) = resp.hover_pos().or(resp.interact_pointer_pos()) {
        if let Some(l) = link_at(p).filter(|_| ks.drag.is_none()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            // Ctrl+click and middle-click open in a new tab.
            let mid = resp.middle_clicked();
            if resp.clicked() || mid {
                open_link(&mut out.cmds, path, l, mid || ui.input(|i| i.modifiers.command));
            }
        }
    }
    rect.height() + GAP
}
