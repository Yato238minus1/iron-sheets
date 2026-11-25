//! Faint placeholders in the final geometry of what they stand in for (shared by the editor, the Markdown preview and
//! the Typst preview while their first content is still on its way).
use egui::{Color32, Rect, pos2, vec2};

/// Placeholder fill; `pulse` breathes it (and asks for repaints) when animations are on.
pub(crate) fn fill(ui: &egui::Ui, pulse: bool) -> Color32 {
    let blend = if pulse {
        let now = ui.input(|i| i.time);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        0.14 + 0.22 * (0.5 + 0.5 * (now * std::f64::consts::TAU / 1.2).sin() as f32)
    } else {
        0.24
    };
    ui.visuals().widgets.inactive.weak_bg_fill.lerp_to_gamma(ui.visuals().weak_text_color(), blend)
}

/// Faint text rows (a title bar, then lines) starting at (`x`, top of `area`) and stopping at its bottom edge.
pub(crate) fn text_rows(ui: &egui::Ui, area: Rect, x: f32, max_w: f32, title_h: f32, fill: Color32) {
    let mut y = area.top() + 18.0;
    for (i, frac) in [0.42, 0.88, 0.76, 0.92, 0.64, 0.81, 0.54].iter().enumerate() {
        let h = if i == 0 { title_h } else { 11.0 };
        if y + h > area.bottom() - 8.0 {
            break;
        }
        ui.painter().rect_filled(Rect::from_min_size(pos2(x, y), vec2((max_w * frac).max(24.0), h)), 4.0, fill);
        y += h + if i == 0 { 16.0 } else { 10.0 };
    }
}

/// An A4-shaped page filling the width of `area` (less `margin`), with faint text rows on it.
#[cfg(feature = "typst")]
pub(crate) fn page(ui: &mut egui::Ui, area: Rect, margin: f32, fill: Color32) {
    let w = (area.width() - 2.0 * margin).max(48.0);
    let r = Rect::from_min_size(pos2(area.left() + (area.width() - w) / 2.0, area.top() + margin), vec2(w, w * 842.0 / 595.0));
    let paper = ui.visuals().extreme_bg_color;
    ui.painter().rect_filled(r.intersect(area), 2.0, paper);
    text_rows(ui, r.intersect(area), r.left() + w * 0.1, w * 0.8, 16.0, fill);
    ui.allocate_rect(area, egui::Sense::hover());
}

/// Framed 4:3 placeholder box for an image whose background decode is still running.
pub(crate) fn image_box(ui: &mut egui::Ui, area: Rect, fill: Color32, stroke: egui::Stroke, radius: f32) {
    let max_w = (area.width() - 32.0).clamp(64.0, 480.0);
    let max_h = (area.height() - 32.0).clamp(64.0, 320.0);
    let w = max_w.min(max_h * 4.0 / 3.0);
    let h = w * 0.75;
    let box_rect = Rect::from_center_size(area.center(), vec2(w, h));
    ui.painter().rect(box_rect, radius, ui.visuals().extreme_bg_color, stroke, egui::StrokeKind::Inside);
    ui.painter().rect_filled(box_rect.shrink(10.0), radius, fill);
    let resp = ui.allocate_rect(area, egui::Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, "Loading image"));
}
