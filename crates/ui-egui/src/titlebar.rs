//! Custom title bar (setting `custom_title_bar`): window buttons and drag area in the menu bar, resize edges around the window.
use crate::command::Command;
use crate::icons::{self, Icon};
use egui::viewport::ResizeDirection as Dir;
use egui::{Align, CursorIcon, Layout, Sense, ViewportCommand};

/// Resize grip thickness in points.
const EDGE: f32 = 5.0;
/// The top grip stays inside the menu bar's 2 pt margin, so the menus and window buttons keep their full height.
const TOP: f32 = 2.0;

/// Inside the menu bar's horizontal `ui`, after the menus: the free space drags the window (double-click maximizes),
/// minimize / maximize / close sit at the right. Close goes through `Command::Quit`, the path of the unsaved-changes dialog.
pub fn show(ui: &mut egui::Ui, out: &mut Vec<Command>) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    // Registered before the buttons, so they win where they overlap. Not focusable: no invisible Tab stop.
    let drag = ui.interact(ui.available_rect_before_wrap(), ui.scope_id().with("title_drag"), Sense::CLICK | Sense::DRAG);
    if drag.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if icons::button(ui, Icon::Close, "Close").clicked() {
            out.push(Command::Quit);
        }
        let (icon, tip) = if maximized { (Icon::Copy, "Restore") } else { (Icon::Maximize, "Maximize") };
        if icons::button(ui, icon, tip).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
        if icons::button(ui, Icon::Minimize, "Minimize").clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    });
}

/// Invisible edge and corner grips on the top layer; none while maximized or fullscreen.
pub fn resize_zones(ctx: &egui::Context) {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let r = ctx.content_rect();
    let (w, h) = (r.width(), r.height());
    let zones = [
        (Dir::North, CursorIcon::ResizeNorth, egui::pos2(EDGE, 0.0), egui::vec2(w - 2.0 * EDGE, TOP)),
        (Dir::South, CursorIcon::ResizeSouth, egui::pos2(EDGE, h - EDGE), egui::vec2(w - 2.0 * EDGE, EDGE)),
        (Dir::West, CursorIcon::ResizeWest, egui::pos2(0.0, TOP), egui::vec2(EDGE, h - TOP - EDGE)),
        (Dir::East, CursorIcon::ResizeEast, egui::pos2(w - EDGE, TOP), egui::vec2(EDGE, h - TOP - EDGE)),
        (Dir::NorthWest, CursorIcon::ResizeNorthWest, egui::pos2(0.0, 0.0), egui::vec2(EDGE, TOP)),
        (Dir::NorthEast, CursorIcon::ResizeNorthEast, egui::pos2(w - EDGE, 0.0), egui::vec2(EDGE, TOP)),
        (Dir::SouthWest, CursorIcon::ResizeSouthWest, egui::pos2(0.0, h - EDGE), egui::Vec2::splat(EDGE)),
        (Dir::SouthEast, CursorIcon::ResizeSouthEast, egui::pos2(w - EDGE, h - EDGE), egui::Vec2::splat(EDGE)),
    ];
    for (dir, cursor, pos, size) in zones {
        egui::Area::new(egui::Id::NULL.with(("resize_zone", dir as u8))).order(egui::Order::Foreground).fixed_pos(r.min + pos.to_vec2()).show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(size, Sense::DRAG);
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(cursor);
            }
            if resp.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        });
    }
}
