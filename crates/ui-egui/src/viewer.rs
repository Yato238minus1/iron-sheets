//! Viewers for one piece of content (a Mermaid diagram, an image) fit to the available area: a fullscreen dark
//! overlay over the app window (`show_with`) or an auxiliary window (`window`). Wheel zooms around the pointer, drag
//! pans, double-click resets, Esc closes. `View` is only the zoom/pan transform; what is painted is a callback, so
//! callers decide (texture, shapes, a text overlay on top).
use crate::aux_window::{self, AuxSpec};
use crate::icons::Icon;
use egui::{Area, Color32, Context, Id, LayerId, Order, Pos2, Rect, Sense, Vec2, vec2};
use iron_sheets_core::config::AuxWindowGeom;

const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 64.0;
/// Margin between the window edge and the fitted content.
const MARGIN: f32 = 24.0;

/// Zoom (a multiple of the fit-to-window scale) and pan (offset of the content centre from the window centre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub pan: Vec2,
}

impl Default for View {
    fn default() -> Self {
        View { zoom: 1.0, pan: Vec2::ZERO }
    }
}

impl View {
    /// Where the content of `size` points lies in `area`. `max_fit` caps the fit scale (1.0: small content is not enlarged).
    pub fn rect(&self, area: Rect, size: Vec2, max_fit: f32) -> Rect {
        Rect::from_center_size(area.center() + self.pan, size * self.scale(area, size, max_fit))
    }

    fn scale(&self, area: Rect, size: Vec2, max_fit: f32) -> f32 {
        let avail = (area.size() - Vec2::splat(2.0 * MARGIN)).max(Vec2::splat(1.0));
        (avail.x / size.x.max(1.0)).min(avail.y / size.y.max(1.0)).min(max_fit) * self.zoom
    }

    /// Multiply the zoom by `factor`; the content point under `at` stays under it.
    pub fn zoom_at(&mut self, at: Pos2, factor: f32, area: Rect) {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = zoom / self.zoom;
        let centre = area.center() + self.pan;
        self.pan = at - (at - centre) * k - area.center();
        self.zoom = zoom;
    }
}

/// Drag pans, the wheel zooms around the pointer, a double-click resets: on the response `bg` that covers `area`.
/// `pan(p)` says whether a press at `p` belongs to the background (false: the caller's content handles it).
pub fn pan_zoom(ui: &egui::Ui, bg: &egui::Response, view: &mut View, area: Rect, pan: &dyn Fn(Pos2) -> bool) {
    let origin = ui.input(|i| i.pointer.press_origin());
    if bg.dragged() && origin.is_none_or(pan) {
        view.pan += bg.drag_delta();
    }
    if let Some(p) = bg.hover_pos() {
        let f = ui.input(|i| i.zoom_delta() * (i.smooth_scroll_delta.y * 0.002).exp());
        if f != 1.0 {
            view.zoom_at(p, f, area);
        }
    }
    if bg.double_clicked() && bg.interact_pointer_pos().is_none_or(pan) {
        *view = View::default();
    }
}

/// Zoom out, zoom in and reset icon buttons on their own dark bar at the top-right of `area`, `gap` points in from
/// the edge (room for a close button). Never at the top-left, where the app's menu bar text is.
pub fn toolbar(ui: &egui::Ui, id: Id, area: Rect, gap: f32, view: &mut View) {
    const W: f32 = 28.0;
    let bar = Rect::from_min_size(Pos2::new(area.max.x - gap - (3.0 * W + 4.0 * 4.0), area.min.y + 8.0), vec2(3.0 * W + 16.0, 32.0));
    ui.painter().rect_filled(bar, 6.0, Color32::from_black_alpha(200));
    for (k, icon) in [Icon::ZoomOut, Icon::ZoomIn, Icon::Reset].into_iter().enumerate() {
        let r = Rect::from_min_size(bar.min + vec2(4.0 + k as f32 * (W + 4.0), 4.0), vec2(W, 24.0));
        let name = ["Zoom out", "Zoom in", "Reset"][k];
        let rs = ui.interact(r, id.with(("zoom-btn", k)), Sense::click()).on_hover_text(name);
        crate::icons::name(&rs, name);
        ui.painter().rect_filled(r, 4.0, Color32::from_white_alpha(if rs.hovered() { 60 } else { 28 }));
        crate::icons::paint(ui.painter(), r, icon, Color32::WHITE);
        if rs.clicked() {
            match k {
                0 => view.zoom_at(area.center(), 0.8, area),
                1 => view.zoom_at(area.center(), 1.25, area),
                _ => *view = View::default(),
            }
        }
    }
}

/// Draw the fullscreen viewer for one frame; `paint` gets the `Ui`, the rect the content occupies and the background
/// response (selection, links); `pan` see `pan_zoom`. Returns true when the user closed it (the caller drops its `View`).
pub fn show_with(
    ctx: &Context,
    id: Id,
    view: &mut View,
    size: Vec2,
    max_fit: f32,
    pan: &dyn Fn(Pos2) -> bool,
    paint: impl FnOnce(&mut egui::Ui, Rect, &egui::Response, &mut View),
) -> bool {
    let screen = ctx.content_rect();
    let mut close = false;
    let layer = LayerId::new(Order::Foreground, id);
    Area::new(id).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        let bg = ui.allocate_rect(screen, Sense::click_and_drag());
        // Typing must not reach an editor underneath.
        bg.request_focus();
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(225));
        pan_zoom(ui, &bg, view, screen, pan);
        let r = view.rect(screen, size, max_fit);
        paint(ui, r, &bg, view);
        toolbar(ui, id, screen, 56.0, view);
        let x = Rect::from_min_size(screen.right_top() + vec2(-48.0, 8.0), Vec2::splat(40.0));
        let xr = ui.interact(x, id.with("close"), Sense::click()).on_hover_text("Close (Esc)");
        crate::icons::name(&xr, "Close");
        let col = if xr.hovered() { Color32::WHITE } else { Color32::from_gray(190) };
        crate::icons::paint(ui.painter(), x, Icon::Close, col);
        let outside = bg.clicked() && bg.interact_pointer_pos().is_some_and(|p| !r.contains(p));
        close = xr.clicked()
            || outside
            || (ui.ctx().memory(|m| m.areas().top_layer_id(Order::Foreground)) == Some(layer)
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)));
    });
    close
}

/// Content of `size` points with zoom, pan and the zoom toolbar in an auxiliary window (the Mermaid diagram and image
/// popups); `paint` gets the `Ui`, the rect the content occupies and the background response. Returns true when closed.
#[allow(clippy::too_many_arguments)]
pub fn window(
    ctx: &Context,
    spec: &AuxSpec,
    geom: &mut AuxWindowGeom,
    focus: bool,
    view: &mut View,
    size: Vec2,
    max_fit: f32,
    pan: &dyn Fn(Pos2) -> bool,
    mut paint: impl FnMut(&mut egui::Ui, Rect, &egui::Response),
) -> bool {
    aux_window::show(ctx, spec, geom, focus, |ui| {
        let area = ui.available_rect_before_wrap();
        let bg = ui.allocate_rect(area, Sense::click_and_drag());
        pan_zoom(ui, &bg, view, area, pan);
        let r = view.rect(area, size, max_fit);
        ui.set_clip_rect(area);
        paint(ui, r, &bg);
        toolbar(ui, bg.id, area, 8.0, view);
    })
    .close
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0))
    }

    #[test]
    fn fits_the_window_and_caps_small_content() {
        let v = View::default();
        let r = v.rect(area(), vec2(2000.0, 1000.0), 1.0);
        assert!((r.width() - 952.0).abs() < 0.01 && r.center() == area().center());
        // Smaller than the window: shown at 1:1 with max_fit 1, enlarged without the cap.
        assert_eq!(v.rect(area(), vec2(100.0, 50.0), 1.0).size(), vec2(100.0, 50.0));
        assert!(v.rect(area(), vec2(100.0, 50.0), f32::MAX).width() > 100.0);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let size = vec2(400.0, 300.0);
        let mut v = View { zoom: 1.0, pan: vec2(30.0, -10.0) };
        let at = pos2(600.0, 250.0);
        let r0 = v.rect(area(), size, 1.0);
        let rel = ((at - r0.min) / r0.size()).to_pos2();
        v.zoom_at(at, 2.5, area());
        let r1 = v.rect(area(), size, 1.0);
        let after = r1.min + rel.to_vec2() * r1.size();
        assert!((after - at).length() < 0.01, "{after:?}");
        assert!((r1.width() / r0.width() - 2.5).abs() < 1e-3);
        // Clamped.
        v.zoom_at(at, 1e9, area());
        assert_eq!(v.zoom, MAX_ZOOM);
    }

    fn pos2(x: f32, y: f32) -> Pos2 {
        Pos2::new(x, y)
    }
}
