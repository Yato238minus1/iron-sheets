//! Tab strip, view-mode toolbar and tab content (Source / Preview / Split).
use crate::command::Command;
use crate::state::AppState;
use crate::theme::{ControlStyle, RowKind, focus_ring, row_text_color, selected_bg, tokens};
use egui::{Color32, Id, Rect, Sense, Stroke, UiBuilder, WidgetInfo, pos2, vec2};
use iron_sheets_core::FileKind;
use iron_sheets_core::config::ViewMode;

fn title_of(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Index of the tab a dragged tab would be dropped on: the tab under `x`, or the first / last one when the pointer is
/// beyond either end of the strip.
pub fn drop_index(rects: &[Rect], x: f32) -> Option<usize> {
    let first = rects.first()?;
    let last = rects.last()?;
    if x <= first.left() {
        return Some(0);
    }
    if x >= last.right() {
        return Some(rects.len() - 1);
    }
    // Gaps between tabs belong to the tab on their left.
    rects.iter().rposition(|r| x >= r.left())
}

/// Slot (0..=len) where a file dropped at `x` would open: before the first tab whose middle lies right of `x`.
pub fn insert_slot(rects: &[Rect], x: f32) -> usize {
    rects.iter().position(|r| x < r.center().x).unwrap_or(rects.len())
}

/// Tooltip text "Title (Ctrl+T)" for a command; built only when the tooltip is shown.
pub(crate) fn tip_with_key(keymap: &crate::keymap::Keymap, title: &str, id: &str) -> String {
    match keymap.index_of(id).map(|i| keymap.hint(i)) {
        Some(h) if !h.is_empty() => format!("{title} ({h})"),
        _ => title.to_string(),
    }
}

pub fn show_strip(ui: &mut egui::Ui, state: &mut AppState) {
    let mut out: Vec<Command> = Vec::new();
    // A file dropped from the tree opened a tab last frame: put it where it was dropped.
    if let Some((p, before)) = state.ui.files.place_tab.take() {
        if let Some(from) = state.tabs.tabs.iter().position(|t| t.path == p) {
            let at = before.and_then(|id| state.tabs.tabs.iter().position(|t| t.id == id)).unwrap_or(state.tabs.tabs.len());
            state.tabs.move_tab(from, if from < at { at - 1 } else { at });
        }
    }
    // Widths of the view toolbar (full and compact form) as measured in earlier frames; they grow with zoom and font size.
    let full_id = Id::NULL.with("tab_toolbar_w");
    let compact_id = Id::NULL.with("tab_toolbar_compact_w");
    let full_w: f32 = ui.data(|d| d.get_temp(full_id)).unwrap_or(250.0);
    let compact_w: f32 = ui.data(|d| d.get_temp(compact_id)).unwrap_or(100.0);
    // Narrow pane: the four view buttons collapse into one menu so the tabs keep room and nothing leaves the window.
    let compact = ui.available_width() - full_w < 150.0;
    // 0 = full toolbar, 1 = compact; glides between the two so the tabs grow and shrink smoothly instead of jumping.
    let ct = crate::anim::bool_t(ui, Id::NULL.with("tab_toolbar_compact_t"), compact, crate::anim::Group::Collapse);
    let toolbar_w = full_w + (compact_w - full_w) * ct;
    let animating = ct > 0.0 && ct < 1.0;
    let h = tokens::TAB_H;
    let font = egui::TextStyle::Button.resolve(ui.style());
    let controls = state.ui.theme.controls;
    let font_gen = state.ui.theme.generation;
    let max_title_w = (font.size * tokens::TAB_TITLE_EM).round();
    crate::theme::clear_rows(ui.ctx(), "tabs");
    ui.horizontal(|ui| {
        let (can_back, can_fwd) = state.tabs.active_tab().map_or((false, false), |t| (t.can_back(), t.can_forward()));
        for (left, enabled, cmd, title, id) in
            [(true, can_back, Command::HistoryBack, "Back", "nav.back"), (false, can_fwd, Command::HistoryForward, "Forward", "nav.forward")]
        {
            if nav_button(ui, &controls, left, enabled, title, id, &state.keymap) {
                out.push(cmd);
            }
        }
        let overflow_id = Id::NULL.with("tab_strip_overflow");
        let overflow_prev: bool = ui.data(|d| d.get_temp(overflow_id)).unwrap_or(false);
        let overflow_w = if overflow_prev { h + 4.0 } else { 0.0 };
        let avail = (ui.available_width() - toolbar_w - overflow_w).max(40.0);
        let strip = egui::ScrollArea::horizontal()
            .id_salt("tab_strip")
            .max_width(avail)
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let active = state.tabs.active;
                    let mut rects: Vec<Rect> = Vec::new();
                    let mut drag_from: Option<usize> = None;
                    // Tab being dragged right now: (index, title, text colour, selected).
                    let mut dragging: Option<(usize, String, Color32, bool)> = None;
                    let (inactive_bg, hovered_bg, edit_bg, accent) = {
                        let v = ui.visuals();
                        (v.widgets.inactive.weak_bg_fill, v.widgets.hovered.weak_bg_fill, v.extreme_bg_color, v.hyperlink_color)
                    };
                    crate::anim::fades_begin(ui, state.tabs.tabs.len());
                    for i in 0..state.tabs.tabs.len() {
                        // Nothing in this loop changes `state`: borrow the tab instead of cloning its path.
                        let t = &state.tabs.tabs[i];
                        let (path, id, empty) = (&t.path, t.id, t.is_empty());
                        let dirty = !empty && state.docs.get(path).is_some_and(|d| d.is_dirty());
                        let untitled = state.tabs.is_untitled(path);
                        // An untitled note shows "Untitled-3", not its file in the app data folder.
                        let title: std::borrow::Cow<str> = if empty {
                            "New tab".into()
                        } else if untitled {
                            path.file_stem().map(|n| n.to_string_lossy()).unwrap_or_default()
                        } else {
                            path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                        };
                        let selected = active == Some(i);
                        // Long names are cut in the middle; the full name is in the tooltip. The galley is cached, so a steady
                        // frame lays out nothing; it serves width and drawing (the colour is passed to `galley` below).
                        let galley = crate::elide::title_galley(ui.painter(), &title, max_title_w, font_gen, &font);
                        let w = galley.size().x + tokens::TAB_PAD;
                        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
                        rects.push(rect);
                        // A tab that was restored but not shown yet reads in the background while the pointer is on it
                        // (once per hovered tab: the last one is kept in egui's temp data).
                        if resp.hovered() && !empty && state.docs.get(path).is_none() {
                            let last = Id::NULL.with("tab_hover_prefetch");
                            if ui.data(|d| d.get_temp::<std::path::PathBuf>(last)).as_deref() != Some(path.as_path()) {
                                state.docs.prefetch.request(path);
                                ui.data_mut(|d| d.insert_temp(last, path.clone()));
                            }
                        }
                        resp.widget_info(|| {
                            let name = if dirty { format!("{title} (unsaved)") } else { title.to_string() };
                            WidgetInfo::selected(egui::Role::Tab, true, selected, &name)
                        });
                        let kind = if selected { RowKind::Active } else { RowKind::Normal };
                        let color = row_text_color(ui.visuals(), kind, resp.hovered());
                        crate::theme::record_row_with(ui.ctx(), "tabs", || title.to_string(), color);
                        let is_dragged = resp.dragged_by(egui::PointerButton::Primary);
                        if is_dragged {
                            dragging = Some((i, title.to_string(), color, selected));
                        }
                        let fade = if is_dragged { 0.35 } else { 1.0 };
                        // Hover fades in and out; the animation asks for frames only while it runs.
                        let hover_t = crate::anim::fade(i, resp.hovered() && !selected);
                        let bg = if selected { edit_bg } else { inactive_bg.lerp_to_gamma(hovered_bg, hover_t) };
                        ui.painter().rect_filled(rect, tokens::RADIUS, bg.gamma_multiply(fade));
                        if selected {
                            // The active tab joins the editor (same fill) and carries the accent bar; the blue fill stays for selections.
                            let bar = Rect::from_min_size(rect.min + vec2(tokens::RADIUS, 0.0), vec2(rect.width() - 2.0 * tokens::RADIUS, tokens::TAB_BAR));
                            ui.painter().rect_filled(bar, 1.0, accent.gamma_multiply(fade));
                        }
                        focus_ring(ui, rect, &resp);
                        ui.painter().galley(pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, color.gamma_multiply(fade));
                        // The drawn cross is 16 px; the target is 20 px so it is easy to hit.
                        let close_rect = Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(20.0, 20.0));
                        let close = ui.interact(close_rect, Id::NULL.with(("tab_close", id)), Sense::click());
                        close.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, format!("Close {title}")));
                        let close = close.on_hover_ui(|ui| {
                            ui.label(tip_with_key(&state.keymap, "Close tab", "tab.close"));
                        });
                        if dirty && !close.hovered() && !close.has_focus() {
                            // Unsaved marker: a filled dot (shape cue, also in the accessible name).
                            ui.painter().circle_filled(close_rect.center(), 3.5, color);
                        } else {
                            let c = if close.hovered() || close.has_focus() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                            let m = close_rect.center();
                            let s = Stroke::new(1.3, c);
                            ui.painter().line_segment([m + vec2(-3.5, -3.5), m + vec2(3.5, 3.5)], s);
                            ui.painter().line_segment([m + vec2(-3.5, 3.5), m + vec2(3.5, -3.5)], s);
                        }
                        if close.clicked() {
                            out.push(Command::CloseTab(i));
                        } else if resp.clicked() {
                            out.push(Command::ActivateTab(i));
                        } else if resp.middle_clicked() {
                            out.push(Command::CloseTab(i));
                        }
                        if resp.drag_started() {
                            out.push(Command::ActivateTab(i));
                        }
                        if resp.drag_stopped() {
                            drag_from = Some(i);
                        }
                        if selected {
                            let key = Id::NULL.with("tab_strip_last_active");
                            let last: Option<u64> = ui.data(|d| d.get_temp(key));
                            if last != Some(id) {
                                ui.data_mut(|d| d.insert_temp(key, id));
                                resp.scroll_to_me(None);
                            }
                        }
                        // Tooltips are built on hover only (no string per tab per frame).
                        if untitled {
                            resp.clone().on_hover_text("Not saved to a folder yet (Ctrl+S)");
                        } else if !empty {
                            resp.clone().on_hover_ui(|ui| {
                                ui.label(path.display().to_string());
                            });
                        }
                        crate::anim::context_menu(&resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                            if ui.button("Close").clicked() {
                                out.push(Command::CloseTab(i));
                                ui.close();
                            }
                            if ui.button("Close others").clicked() {
                                out.push(Command::CloseOtherTabs(i));
                                ui.close();
                            }
                            if ui.add_enabled(i + 1 < state.tabs.tabs.len(), egui::Button::new("Close tabs to the right")).clicked() {
                                out.push(Command::CloseTabsToRight(i));
                                ui.close();
                            }
                            if ui.button("Close all").clicked() {
                                out.push(Command::CloseAllTabs);
                                ui.close();
                            }
                            if !empty && !untitled {
                                if ui.button("Copy path").clicked() {
                                    out.push(Command::CopyPath(path.clone()));
                                    ui.close();
                                }
                                if ui.button("Reveal in file manager").clicked() {
                                    out.push(Command::RevealInFileManager(path.clone()));
                                    ui.close();
                                }
                            }
                        });
                    }
                    crate::anim::fades_end(ui);
                    if let Some((from, title, color, selected)) = dragging {
                        drag_feedback(ui, &rects, from, &title, color, selected);
                    }
                    // A file dragged out of the tree: the same marker as a tab drag, between the tabs where it would open.
                    // The scroll area clips only along x: the tab row's own height bounds the target.
                    let band = Rect::from_x_y_ranges(ui.clip_rect().x_range(), ui.min_rect().y_range());
                    let ctx = ui.ctx().clone();
                    if let Some(p) = ctx.pointer_latest_pos().filter(|p| band.contains(*p) && state.ui.files.dragged_file(&ctx)) {
                        state.ui.files.accept_drop(&ctx);
                        if let Some(last) = rects.last() {
                            let slot = insert_slot(&rects, p.x);
                            let (r, x) = rects.get(slot).map_or((last, last.right() + 1.0), |r| (r, r.left() - 1.0));
                            ui.painter().line_segment([pos2(x, r.top() + 1.0), pos2(x, r.bottom() - 1.0)], Stroke::new(2.5, ui.visuals().selection.stroke.color));
                        }
                    }
                    if let Some(path) = state.ui.files.take_drop(&ctx, band) {
                        let slot = ctx.pointer_latest_pos().map_or(rects.len(), |p| insert_slot(&rects, p.x));
                        out.push(Command::OpenPath { path: path.clone(), new_tab: true, line: None });
                        state.ui.files.place_tab = Some((path, state.tabs.tabs.get(slot).map(|t| t.id)));
                    }
                    if let (Some(from), Some(p)) = (drag_from, ui.ctx().pointer_latest_pos()) {
                        if let Some(to) = drop_index(&rects, p.x) {
                            state.tabs.move_tab(from, to);
                        }
                    }
                });
            });
        let off_x = strip.state.clamped_offset().x;
        let overflow = strip.content_size.x > strip.inner_rect.width() + 1.0;
        if overflow != overflow_prev {
            ui.data_mut(|d| d.insert_temp(overflow_id, overflow));
            ui.ctx().request_repaint();
        }
        if overflow {
            let max_off = (strip.content_size.x - strip.inner_rect.width()).max(0.0);
            let bg = ui.visuals().panel_fill;
            let fade_w = 14.0_f32.min(strip.inner_rect.width() * 0.25);
            if off_x > 1.0 {
                let r = Rect::from_min_max(strip.inner_rect.left_top(), pos2(strip.inner_rect.left() + fade_w, strip.inner_rect.bottom()));
                paint_horiz_fade(ui.painter(), r, bg, Color32::TRANSPARENT);
            }
            if off_x < max_off - 1.0 {
                let r = Rect::from_min_max(pos2(strip.inner_rect.right() - fade_w, strip.inner_rect.top()), strip.inner_rect.right_bottom());
                paint_horiz_fade(ui.painter(), r, Color32::TRANSPARENT, bg);
            }
        }
        // Double-click on the free space right of the last tab opens a new tab. Sensed after the tabs, so it never
        // takes a click or drag from one (the rect starts where the last tab ends).
        let free_from = strip.inner_rect.left() + strip.content_size.x - off_x;
        if free_from < strip.inner_rect.right() {
            let free = Rect::from_min_max(pos2(free_from, strip.inner_rect.top()), strip.inner_rect.right_bottom());
            if ui.interact(free, Id::NULL.with("tab_strip_free"), Sense::click()).double_clicked() {
                out.push(Command::NewTab);
            }
        }
        if overflow {
            let color = ui.visuals().text_color();
            let icon = egui::Atom::paint(vec2(12.0, 12.0), move |ui, args| crate::icons::paint(ui.painter(), args.rect, crate::icons::Icon::Down, color));
            let (mr, _) = egui::containers::menu::MenuButton::new(icon).ui(ui, |ui| {
                for i in 0..state.tabs.tabs.len() {
                    let t = &state.tabs.tabs[i];
                    let label: String = if t.is_empty() {
                        "New tab".into()
                    } else if state.tabs.is_untitled(&t.path) {
                        t.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    } else {
                        title_of(&t.path)
                    };
                    if ui.selectable_label(state.tabs.active == Some(i), label).clicked() {
                        out.push(Command::ActivateTab(i));
                        ui.close();
                    }
                }
            });
            mr.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, "Open tabs"));
            mr.on_hover_text("Open tabs");
        }
        // The new-tab button stays visible however many tabs are open.
        let x0 = ui.cursor().left();
        if state.config.toolbar_new_tab {
            let plus = ui.add_sized([h, h], egui::Button::new("+")).on_hover_ui(|ui| {
                ui.label(tip_with_key(&state.keymap, "New tab", "tab.new"));
            });
            plus.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, "New tab"));
            if plus.clicked() {
                out.push(Command::NewTab);
            }
        }
        // View mode toolbar.
        // Not for an empty tab (it has no editor or preview to switch); each part can be hidden in the settings.
        let show_modes = state.config.toolbar_view_buttons;
        if let Some(i) = state.tabs.active.filter(|&i| !state.tabs.tabs[i].is_empty() && (show_modes || state.config.toolbar_sync_button)) {
            let kind = FileKind::from_path(&state.tabs.tabs[i].path);
            let show_sync = state.config.toolbar_sync_button && kind != FileKind::Excalidraw;
            if matches!(kind, FileKind::Markdown | FileKind::Typst | FileKind::Excalidraw) && (show_modes || show_sync) {
                let view = state.tabs.tabs[i].view;
                let modes = [
                    ("Source", ViewMode::Source, "Editor only"),
                    ("Split", ViewMode::Split, "Editor and preview side by side"),
                    ("Preview", ViewMode::Preview, "Preview only"),
                ];
                // While the toolbar glides between its two forms the first half fades the old form out, the second half
                // fades the new one in (a cross-fade of two widget sets, no layout of both at once).
                let (draw_compact, opacity) = if animating { if ct < 0.5 { (false, 1.0 - 2.0 * ct) } else { (true, 2.0 * ct - 1.0) } } else { (compact, 1.0) };
                ui.scope(|ui| {
                    ui.set_opacity(opacity);
                    if draw_compact {
                        let (vm, _) = crate::anim::menu_button(ui, Id::NULL.with("view_menu"), "Mode", true, |ui| {
                            if show_modes {
                                for (label, m, tip) in modes {
                                    if ui.selectable_label(view == m, label).on_hover_text(tip).clicked() {
                                        out.push(Command::SetViewMode(m));
                                        ui.close();
                                    }
                                }
                                let cycle_hint = hint_of(state, "view.cycle");
                                let cycle_btn = egui::Button::new("Cycle view mode");
                                let cycle_btn = if cycle_hint.is_empty() { cycle_btn } else { cycle_btn.shortcut_text(cycle_hint) };
                                if ui.add(cycle_btn).clicked() {
                                    out.push(Command::CycleViewMode);
                                    ui.close();
                                }
                            }
                            if show_sync && ui.selectable_label(state.config.sync_scroll, "Sync scrolling").clicked() {
                                out.push(Command::ToggleSyncScroll);
                                ui.close();
                            }
                        });
                        vm.on_hover_text(tip_with_key(&state.keymap, "View mode", "view.cycle"));
                    } else {
                        if show_modes {
                            if let Some(m) = segmented(ui, &controls, &modes, view) {
                                out.push(Command::SetViewMode(m));
                            }
                        }
                        if show_sync && toggle_chip(ui, &controls, "Sync", "Synchronized scrolling", state.config.sync_scroll) {
                            out.push(Command::ToggleSyncScroll);
                        }
                    }
                });
            }
        }
        // Only a settled toolbar is measured: mid-glide it shows one form with a different width.
        if !animating {
            let used = ui.min_rect().right() - x0 + 12.0;
            let (id, old) = if compact { (compact_id, compact_w) } else { (full_id, full_w) };
            if (used - old).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, used));
                ui.ctx().request_repaint();
            }
        }
    });
    state.commands.extend(out);
}

fn paint_horiz_fade(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// A flat text button for the toolbar: `lead` points of room for a mark before the label, a fill that follows the pointer, and
/// a size that does not depend on the hover state. (egui's `Button` with an explicit stroke or fill loses its hover look, and
/// its frame shrinks by the hover stroke width when the pointer is over it.) Returns the response and the rect of the `lead`.
fn flat_button(ui: &mut egui::Ui, label: &str, text: Option<Color32>, lead: f32, fill: [Color32; 2], stroke: Stroke) -> (egui::Response, Rect) {
    let pad = ui.spacing().button_padding;
    let galley = ui.painter().layout_no_wrap(label.to_owned(), egui::TextStyle::Button.resolve(ui.style()), Color32::PLACEHOLDER);
    let size = vec2(lead + galley.size().x + 2.0 * pad.x, (galley.size().y + 2.0 * pad.y).max(ui.spacing().interact_size.y));
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    let color = text.unwrap_or_else(|| ui.style().interact(&r).text_color());
    ui.painter().rect(rect, tokens::RADIUS, fill[r.hovered() as usize], stroke, egui::StrokeKind::Inside);
    let mark = Rect::from_min_size(pos2(rect.min.x + pad.x, rect.min.y), vec2(lead, rect.height()));
    ui.painter().galley(pos2(mark.max.x, rect.center().y - galley.size().y / 2.0), galley, color);
    (r, mark)
}

/// A history arrow (drawn, the fonts have no arrow glyph); greyed and inert when `enabled` is false. Returns true when clicked.
fn nav_button(ui: &mut egui::Ui, c: &ControlStyle, left: bool, enabled: bool, title: &str, id: &str, keymap: &crate::keymap::Keymap) -> bool {
    let size = vec2(ui.spacing().interact_size.y, ui.spacing().interact_size.y);
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    if enabled && r.hovered() {
        ui.painter().rect_filled(rect, tokens::RADIUS, c.hover_fill);
    }
    let color = if enabled { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
    let (m, d) = (rect.center(), if left { -1.0 } else { 1.0 });
    let w = Stroke::new(1.5, color);
    let (tip_x, tail_x, h) = (m.x + 5.0 * d, m.x - 5.0 * d, 4.5);
    ui.painter().line_segment([pos2(tail_x, m.y), pos2(tip_x, m.y)], w);
    ui.painter().line_segment([pos2(tip_x - h * d, m.y - h), pos2(tip_x, m.y)], w);
    ui.painter().line_segment([pos2(tip_x - h * d, m.y + h), pos2(tip_x, m.y)], w);
    // The key hint is built for the accessible label and while the tooltip is shown, not every frame.
    let r = if r.hovered() { r.on_hover_text(tip_with_key(keymap, title, id)) } else { r };
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, tip_with_key(keymap, title, id)));
    // Always sensed (so assistive tech and tests see the arrow); a click counts only when it is enabled.
    r.clicked() && enabled
}

/// The view modes as one segmented control: a shared frame, the selected segment filled. Returns the clicked mode.
fn segmented(ui: &mut egui::Ui, c: &ControlStyle, items: &[(&str, ViewMode, &str)], current: ViewMode) -> Option<ViewMode> {
    let mut clicked = None;
    let mut span: Option<Rect> = None;
    let item_gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.x, 0.0);
    for &(label, m, tip) in items {
        let selected = m == current;
        let (fill, text) = if selected { ([c.seg_fill, c.seg_hover_fill], Some(c.seg_text)) } else { ([Color32::TRANSPARENT, c.hover_fill], None) };
        let (r, _) = flat_button(ui, label, text, 0.0, fill, Stroke::NONE);
        let r = r.on_hover_text(tip);
        // Selection is not colour alone: screen readers get the selected state, keyboard users a focus ring.
        r.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, selected, label));
        focus_ring(ui, r.rect, &r);
        span = Some(span.map_or(r.rect, |s| s.union(r.rect)));
        if r.clicked() {
            clicked = Some(m);
        }
    }
    ui.spacing_mut().item_spacing.x = item_gap;
    if let Some(span) = span {
        ui.painter().rect_stroke(span, tokens::RADIUS, c.frame, egui::StrokeKind::Inside);
    }
    clicked
}

/// An on/off chip: a check box and an outline in the accent colour when on, a plain outline when off. Returns true when clicked.
fn toggle_chip(ui: &mut egui::Ui, c: &ControlStyle, label: &str, tip: &str, on: bool) -> bool {
    const BOX: f32 = 10.0;
    let (stroke, color) = if on { (c.on_stroke, Some(c.on_text)) } else { (c.frame, None) };
    let (r, mark) = flat_button(ui, label, color, BOX + ui.spacing().icon_spacing, [Color32::TRANSPARENT, c.hover_fill], stroke);
    let box_rect = Rect::from_min_size(pos2(mark.min.x, mark.center().y - BOX / 2.0), vec2(BOX, BOX));
    if on {
        ui.painter().rect_filled(box_rect, 2.0, stroke.color);
        let w = Stroke::new(1.5, ui.visuals().extreme_bg_color);
        let (a, b, d) = (box_rect.left_top() + vec2(2.0, 5.0), box_rect.left_top() + vec2(4.2, 7.2), box_rect.left_top() + vec2(8.0, 2.8));
        ui.painter().line_segment([a, b], w);
        ui.painter().line_segment([b, d], w);
    } else {
        ui.painter().rect_stroke(box_rect, 2.0, c.frame, egui::StrokeKind::Inside);
    }
    let r = r.on_hover_text(tip);
    r.widget_info(|| WidgetInfo::selected(egui::Role::CheckBox, true, on, label));
    focus_ring(ui, r.rect, &r);
    r.clicked()
}

/// While a tab is dragged: an insertion marker where it would land and a ghost of the tab that follows the pointer.
fn drag_feedback(ui: &mut egui::Ui, rects: &[Rect], from: usize, title: &str, color: Color32, selected: bool) {
    let Some(p) = ui.ctx().pointer_latest_pos() else { return };
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    if let Some(to) = drop_index(rects, p.x).filter(|&to| to != from) {
        let r = rects[to];
        let x = if to > from { r.right() + 1.0 } else { r.left() - 1.0 };
        let c = ui.visuals().selection.stroke.color;
        ui.painter().line_segment([pos2(x, r.top() + 1.0), pos2(x, r.bottom() - 1.0)], Stroke::new(2.5, c));
    }
    let src = rects[from];
    let grab = ui.input(|i| i.pointer.press_origin()).map_or(src.width() / 2.0, |o| (o.x - src.left()).clamp(0.0, src.width()));
    let screen = ui.ctx().content_rect();
    let x = (p.x - grab).clamp(screen.left(), (screen.right() - src.width()).max(screen.left()));
    let ghost = Rect::from_min_size(pos2(x, src.top() - 3.0), src.size());
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::NULL.with("tab_drag_ghost")));
    let bg = if selected { selected_bg(ui.visuals()) } else { ui.visuals().widgets.hovered.weak_bg_fill };
    painter.rect_filled(ghost.translate(vec2(1.5, 2.5)), 4.0, Color32::from_black_alpha(60));
    painter.rect_filled(ghost, 4.0, bg);
    painter.rect_stroke(ghost, 4.0, Stroke::new(1.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Inside);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = painter.layout_no_wrap(title.to_string(), font, color);
    painter.galley(pos2(ghost.left() + 8.0, ghost.center().y - galley.size().y / 2.0), galley, color);
    // Keep the strip scrolling while the pointer rests near either edge.
    let clip = ui.clip_rect();
    if p.x > clip.right() - 24.0 {
        ui.scroll_with_delta(vec2(-10.0, 0.0));
    } else if p.x < clip.left() + 24.0 {
        ui.scroll_with_delta(vec2(10.0, 0.0));
    }
    ui.ctx().request_repaint();
}

/// Text of a command's first shortcut, e.g. "Ctrl+O" (empty when it has none).
fn hint_of<'a>(state: &'a AppState, id: &str) -> &'a str {
    state.keymap.index_of(id).map(|i| state.keymap.hint(i)).unwrap_or_default()
}

/// Text typed while the empty page has the keyboard: printable characters, without a shortcut modifier. Whitespace
/// alone does not count (Space activates a focused button). The events are removed so nothing else sees them.
fn typed_text(ctx: &egui::Context) -> Option<String> {
    // Cheap pre-check: nearly every frame has no text event.
    if !ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))) {
        return None;
    }
    let m = ctx.input(|i| i.modifiers);
    if (m.command || m.mac_cmd) && !m.alt {
        return None;
    }
    let mut text = String::new();
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Text(t) => {
                text.extend(t.chars().filter(|c| !c.is_control() || *c == '\n'));
                false
            }
            _ => true,
        })
    });
    (!text.trim().is_empty()).then_some(text)
}

/// The empty tab: no editor and no preview, only what you can do next and the recently closed files. Printable text
/// typed here starts an untitled note in this tab. Widgets are plain buttons and links in reading order, so Tab walks
/// them and a screen reader announces each by its label.
fn empty_page(ui: &mut egui::Ui, state: &mut AppState) {
    let mut out: Vec<Command> = Vec::new();
    let ctx = ui.ctx().clone();
    crate::newtab::show(ui, state, state.tabs.active_tab().map_or(0, |t| t.id));
    let keyboard_free = !ctx.text_edit_focused() && state.dialogs.current.is_none() && !state.palette.open && !(state.settings.open && ctx.embed_viewports());
    if keyboard_free {
        if let Some(text) = typed_text(&ctx) {
            out.push(Command::StartUntitled { text });
        }
    }
    let recent: Vec<std::path::PathBuf> = {
        let mut v: Vec<std::path::PathBuf> = Vec::new();
        for p in state.tabs.closed.iter().rev() {
            if !v.contains(p) && !state.tabs.is_open(p) {
                v.push(p.clone());
            }
            if v.len() == 5 {
                break;
            }
        }
        v
    };
    let no_roots = state.workspace.roots.is_empty();
    let take_focus = std::mem::take(&mut state.focus_empty_page);
    let mut rows: Vec<(&str, &str, Command)> = vec![
        ("New note", hint_of(state, "file.new_note"), Command::NewNote { dir: None }),
        ("New untitled note", "", Command::StartUntitled { text: String::new() }),
        ("Open note", hint_of(state, "file.quick_open"), Command::QuickOpen),
        ("Command palette", hint_of(state, "edit.palette"), Command::CommandPalette),
    ];
    if no_roots {
        rows.push(("Add folder", "", Command::AddRootDialog));
        rows.push(("Add folder by path...", "", Command::AddRootByPath));
    }
    ui.vertical_centered(|ui| {
        ui.set_max_width(420.0);
        ui.add_space((ui.available_height() * 0.18).min(120.0));
        ui.label(egui::RichText::new(if no_roots { "This workspace has no folders yet" } else { "New tab" }).heading());
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Start typing to begin an untitled note.").weak());
        ui.add_space(10.0);
        for (i, (label, hint, cmd)) in rows.into_iter().enumerate() {
            // The shortcut sits at the right edge in a muted colour, as in the menus.
            let b = egui::Button::new(label);
            let r = ui.add_sized([260.0, 28.0], if hint.is_empty() { b } else { b.shortcut_text(hint) });
            if i == 0 && take_focus {
                r.request_focus();
            }
            if r.clicked() {
                out.push(cmd);
            }
        }
        if !recent.is_empty() {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Recently closed").weak());
            for p in recent {
                if ui.link(title_of(&p)).on_hover_text(p.display().to_string()).clicked() {
                    out.push(Command::OpenPath { path: p, new_tab: false, line: None });
                }
            }
        }
    });
    state.commands.extend(out);
}

pub fn show_content(ui: &mut egui::Ui, state: &mut AppState) {
    // There is always a tab; without an active one (cannot happen) the empty page is the safe thing to draw.
    let Some(i) = state.tabs.active.filter(|&i| i < state.tabs.tabs.len() && !state.tabs.tabs[i].is_empty()) else {
        empty_page(ui, state);
        return;
    };
    let path = state.tabs.tabs[i].path.clone();
    if let Some(err) = state.tabs.tabs[i].load_error.clone() {
        failed_panel(ui, state, i, &path, &err);
        return;
    }
    let Some(kind) = state.docs.get(&path).map(|d| d.kind) else {
        loading_skeleton(ui, state, i);
        return;
    };
    let view = match kind {
        FileKind::Image => ViewMode::Preview,
        FileKind::Other => ViewMode::Source,
        _ => state.tabs.tabs[i].view,
    };
    match view {
        ViewMode::Source => crate::editor::show(ui, state, i),
        ViewMode::Preview => preview_pane(ui, state, i),
        ViewMode::Split => split(ui, state, i),
    }
}

/// IS-33: the tab of a file that could not be read says so and offers Retry (focused, Enter), Reveal and Close (Esc).
fn failed_panel(ui: &mut egui::Ui, state: &mut AppState, i: usize, path: &std::path::Path, err: &str) {
    let mut out: Vec<Command> = Vec::new();
    let first = Id::NULL.with(("open_failed_focus", state.tabs.tabs[i].id));
    ui.vertical_centered(|ui| {
        ui.set_max_width(460.0);
        ui.add_space((ui.available_height() * 0.18).min(120.0));
        ui.label(egui::RichText::new(format!("Can't open {}", title_of(path))).heading());
        ui.add_space(4.0);
        ui.label(egui::RichText::new(err).weak());
        ui.add_space(10.0);
        let retry = ui.add_sized([260.0, 28.0], egui::Button::new("Retry"));
        let reveal = ui.add_sized([260.0, 28.0], egui::Button::new("Reveal in file manager"));
        let close = ui.add_sized([260.0, 28.0], egui::Button::new("Close"));
        if !ui.data(|d| d.get_temp::<bool>(first).unwrap_or(false)) {
            retry.request_focus();
            ui.data_mut(|d| d.insert_temp(first, true));
        }
        if retry.clicked() {
            out.push(Command::RetryOpen(i));
        }
        if reveal.clicked() {
            out.push(Command::RevealInFileManager(path.to_path_buf()));
        }
        if close.clicked() || ((retry.lost_focus() || reveal.lost_focus() || close.lost_focus()) && ui.input(|inp| inp.key_pressed(egui::Key::Escape))) {
            // egui drops the focus on Escape before the widgets run, so "had focus" is `lost_focus`.
            out.push(Command::CloseTab(i));
        }
    });
    if out.iter().any(|c| matches!(c, Command::RetryOpen(_) | Command::CloseTab(_))) {
        ui.data_mut(|d| d.remove::<bool>(first));
    }
    state.commands.extend(out);
}

/// Subtle skeleton placeholder shown while a tab's file is loading in the background.
fn loading_skeleton(ui: &mut egui::Ui, state: &AppState, i: usize) {
    let rect = ui.available_rect_before_wrap();
    let view = state.tabs.tabs[i].view;
    let editor_rect = if view == ViewMode::Split && rect.width() >= SPLIT_MIN_WIDTH {
        let div_w = 7.0;
        let ratio = state.tabs.tabs[i].split_ratio.clamp(0.1, 0.9);
        let x = rect.left() + (rect.width() - div_w) * ratio;
        let left = Rect::from_min_max(rect.min, pos2(x, rect.bottom()));
        let div_x = x + div_w * 0.5;
        let line_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        ui.painter().line_segment([pos2(div_x, rect.top()), pos2(div_x, rect.bottom())], Stroke::new(1.0, line_color));
        left
    } else {
        rect
    };
    let pulse = ui.style().animation_time > 0.0 && state.config.animations;
    let fill = crate::skeleton::fill(ui, pulse);
    let max_w = (editor_rect.width() - 32.0).clamp(24.0, 680.0);
    crate::skeleton::text_rows(ui, editor_rect, editor_rect.left() + 16.0, max_w, 16.0, fill);
    ui.allocate_rect(rect, Sense::hover());
}

/// The preview of the tab, or the board view for a Kanban note.
fn preview_pane(ui: &mut egui::Ui, state: &mut AppState, i: usize) {
    #[cfg(feature = "kanban")]
    if crate::kanban::is_board_tab(ui, state, i) {
        crate::kanban::show(ui, state, i);
        return;
    }
    crate::preview::show(ui, state, i);
}

/// Below this width the split view shows only the editor (a layout fallback; the tab's view mode is unchanged).
pub const SPLIT_MIN_WIDTH: f32 = 450.0;

/// Set in the egui temp memory every frame the active tab is in split view: true while the narrow fallback shows
/// only the editor, so the frame loop does not wait for a preview that is not drawn.
pub fn split_narrow_id() -> Id {
    Id::NULL.with("iron_sheets_split_narrow")
}

fn split(ui: &mut egui::Ui, state: &mut AppState, i: usize) {
    let rect = ui.available_rect_before_wrap();
    let narrow = rect.width() < SPLIT_MIN_WIDTH;
    ui.ctx().data_mut(|d| d.insert_temp(split_narrow_id(), narrow));
    if narrow {
        crate::editor::show(ui, state, i);
        return;
    }
    let div_w = 7.0;
    let ratio = state.tabs.tabs[i].split_ratio.clamp(0.1, 0.9);
    let x = rect.left() + (rect.width() - div_w) * ratio;
    let left = Rect::from_min_max(rect.min, pos2(x, rect.bottom()));
    let div = Rect::from_min_max(pos2(x, rect.top()), pos2(x + div_w, rect.bottom()));
    let right = Rect::from_min_max(pos2(x + div_w, rect.top()), rect.max);
    let tab_id = state.tabs.tabs[i].id;
    let resp = ui.interact(div, Id::NULL.with(("split_divider", tab_id)), Sense::drag());
    resp.widget_info(|| WidgetInfo::labeled(egui::Role::Slider, true, "Split divider"));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if resp.dragged() {
        if let Some(p) = ui.ctx().pointer_latest_pos() {
            state.tabs.tabs[i].split_ratio = ((p.x - rect.left() - div_w / 2.0) / (rect.width() - div_w)).clamp(0.1, 0.9);
        }
    }
    let line_color =
        if resp.hovered() || resp.dragged() { ui.visuals().widgets.hovered.fg_stroke.color } else { ui.visuals().widgets.noninteractive.bg_stroke.color };
    ui.painter().line_segment([div.center_top(), div.center_bottom()], Stroke::new(1.0, line_color));
    // The pane the user is scrolling with the wheel is drawn first (not on a press: a pane drawn later wins overlapping clicks), then the panes are coupled, then
    // the other pane is drawn: the follower moves in the same frame instead of one frame later.
    let preview_first = ui.ctx().input(|inp| {
        let over_preview = inp.pointer.latest_pos().is_some_and(|p| right.contains(p));
        over_preview && inp.smooth_scroll_delta != egui::Vec2::ZERO
    });
    let sync_on = state.config.sync_scroll;
    let show_editor = |ui: &mut egui::Ui, state: &mut AppState| {
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            ui.set_clip_rect(left.intersect(ui.clip_rect()));
            crate::editor::show(ui, state, i);
        });
    };
    let show_preview = |ui: &mut egui::Ui, state: &mut AppState| {
        ui.scope_builder(UiBuilder::new().max_rect(right.shrink2(vec2(4.0, 0.0))), |ui| {
            ui.set_clip_rect(right.intersect(ui.clip_rect()));
            preview_pane(ui, state, i);
        });
    };
    if preview_first {
        show_preview(ui, state);
        crate::scrollsync::sync(&mut state.tabs.tabs[i], sync_on);
        show_editor(ui, state);
    } else {
        show_editor(ui, state);
        crate::scrollsync::sync(&mut state.tabs.tabs[i], sync_on);
        show_preview(ui, state);
    }
    ui.allocate_rect(rect, Sense::hover());
}
