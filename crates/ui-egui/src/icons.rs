//! Vector icons for compact buttons, drawn with the painter: egui's bundled emoji fonts are not part of the build
//! (feature `monochrome_emoji_fonts` is off, so only five logo glyphs exist), and a few strokes cost nothing at startup.
//! Every icon button needs a tooltip and an accessible label with the old text.
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo, vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Copy,
    Check,
    Window,
    Fullscreen,
    ZoomIn,
    ZoomOut,
    Reset,
    Up,
    Down,
    Close,
    Minimize,
    Maximize,
    Folder,
    Search,
    List,
    Link,
    Sigma,
    Calendar,
    #[cfg(feature = "git")]
    GitBranch,
}

/// Draw `icon` centred in `r` (about 55 % of the smaller side) with strokes of `color`.
pub fn paint(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.5, color);
    let c = r.center();
    let h = r.width().min(r.height()) * 0.28;
    let line = |a: Vec2, b: Vec2| p.line_segment([c + a * h, c + b * h], s);
    let v = |x: f32, y: f32| vec2(x, y);
    match icon {
        Icon::Close => {
            line(v(-1.0, -1.0), v(1.0, 1.0));
            line(v(-1.0, 1.0), v(1.0, -1.0));
        }
        Icon::Minimize => {
            line(v(-1.2, 0.8), v(1.2, 0.8));
        }
        Icon::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, v(2.4, 2.4) * h), 0.0, s, egui::StrokeKind::Middle);
        }
        Icon::Up | Icon::Down => {
            let d = if icon == Icon::Up { 1.0 } else { -1.0 };
            line(v(-1.2, 0.6 * d), v(0.0, -0.6 * d));
            line(v(0.0, -0.6 * d), v(1.2, 0.6 * d));
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            line(v(-1.2, 0.0), v(1.2, 0.0));
            if icon == Icon::ZoomIn {
                line(v(0.0, -1.2), v(0.0, 1.2));
            }
        }
        Icon::Folder => {
            p.rect_stroke(Rect::from_min_max(c + v(-1.4, -0.5) * h, c + v(1.4, 1.0) * h), 1.0, s, egui::StrokeKind::Middle);
            for (a, b) in [(v(-1.4, -0.5), v(-1.4, -1.0)), (v(-1.4, -1.0), v(-0.4, -1.0)), (v(-0.4, -1.0), v(-0.1, -0.5))] {
                line(a, b);
            }
        }
        Icon::Search => {
            p.circle_stroke(c + v(-0.3, -0.3) * h, 0.9 * h, s);
            line(v(0.35, 0.35), v(1.35, 1.35));
        }
        Icon::List => {
            for y in [-0.9, 0.0, 0.9] {
                p.circle_filled(c + v(-1.1, y) * h, 0.2 * h, color);
                line(v(-0.4, y), v(1.3, y));
            }
        }
        Icon::Link => {
            for x in [-0.6, 0.6] {
                p.rect_stroke(Rect::from_center_size(c + v(x, 0.0) * h, v(1.9, 1.1) * h), 0.55 * h, s, egui::StrokeKind::Middle);
            }
        }
        Icon::Sigma => {
            line(v(1.1, -1.2), v(-1.1, -1.2));
            line(v(-1.1, -1.2), v(0.1, 0.0));
            line(v(0.1, 0.0), v(-1.1, 1.2));
            line(v(-1.1, 1.2), v(1.1, 1.2));
        }
        Icon::Calendar => {
            p.rect_stroke(Rect::from_min_max(c + v(-1.3, -1.0) * h, c + v(1.3, 1.3) * h), 1.0, s, egui::StrokeKind::Middle);
            line(v(-1.3, -0.2), v(1.3, -0.2));
            line(v(-0.6, -1.5), v(-0.6, -0.6));
            line(v(0.6, -1.5), v(0.6, -0.6));
        }
        Icon::Check => {
            line(v(-1.2, 0.1), v(-0.4, 0.9));
            line(v(-0.4, 0.9), v(1.3, -0.9));
        }
        Icon::Copy => {
            // Two overlapping pages: the back one shows only its top and left edges.
            let (a, b) = (Rect::from_min_size(c + v(-0.4, -0.4) * h, v(1.5, 1.7) * h), c + v(-1.3, -1.3) * h);
            p.rect_stroke(a, 1.0, s, egui::StrokeKind::Middle);
            p.line_segment([b + v(0.0, 1.7) * h, b], s);
            p.line_segment([b, b + v(1.7, 0.0) * h], s);
        }
        Icon::Window => {
            let w = Rect::from_center_size(c, v(2.6, 2.2) * h);
            p.rect_stroke(w, 1.0, s, egui::StrokeKind::Middle);
            p.line_segment([w.left_top() + v(0.0, 0.7) * h, w.right_top() + v(0.0, 0.7) * h], s);
        }
        Icon::Fullscreen => {
            // Four corner brackets.
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let k = c + v(sx, sy) * 1.3 * h;
                p.line_segment([k, k + v(-sx, 0.0) * 0.8 * h], s);
                p.line_segment([k, k + v(0.0, -sy) * 0.8 * h], s);
            }
        }
        Icon::Reset => {
            // An open circle (arc of 300 degrees) with an arrow head at its end.
            let rad = 1.3 * h;
            let at = |t: f32| c + rad * v(t.cos(), t.sin());
            let pts: Vec<Pos2> = (0..=20).map(|i| at(-1.2 + 5.2 * i as f32 / 20.0)).collect();
            p.add(egui::Shape::line(pts, s));
            let tip = at(-1.2);
            p.line_segment([tip, tip + v(0.9, 0.2) * h], s);
            p.line_segment([tip, tip + v(0.1, 0.95) * h], s);
        }
        #[cfg(feature = "git")]
        Icon::GitBranch => {
            let trunk_x = -0.65;
            let branch_x = 0.65;
            line(v(trunk_x, -1.0), v(trunk_x, 1.0));
            line(v(trunk_x, 0.45), v(branch_x, -0.15));
            line(v(branch_x, -0.15), v(branch_x, -0.9));
            let r_dot = 0.36 * h;
            p.circle_filled(c + v(trunk_x, -1.05) * h, r_dot, color);
            p.circle_filled(c + v(trunk_x, 1.05) * h, r_dot, color);
            p.circle_filled(c + v(branch_x, -0.95) * h, r_dot, color);
        }
    }
}

/// Set the accessible label of a custom-drawn or icon-only button to `label` (the picture alone says nothing to a screen reader).
pub fn name(r: &Response, label: &str) {
    r.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label));
}

/// A small square icon button with `tip` as hover text and accessible label (add the shortcut to `tip` if there is one).
pub fn button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let side = ui.spacing().interact_size.y;
    let (rect, r) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let r = r.on_hover_text(tip);
    name(&r, tip);
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact(&r);
        ui.painter().rect(rect, v.corner_radius, v.weak_bg_fill, v.bg_stroke, egui::StrokeKind::Inside);
        paint(ui.painter(), rect, icon, v.fg_stroke.color);
        crate::theme::focus_ring(ui, rect, &r);
    }
    r
}

/// A square tab with an icon for a view picker: `label` is the accessible name, `selected` the current one. The caller adds the tooltip.
pub fn tab(ui: &mut Ui, icon: Icon, label: &str, selected: bool) -> Response {
    let side = ui.spacing().interact_size.y;
    let (rect, r) = ui.allocate_exact_size(vec2(side + 4.0, side), Sense::click());
    r.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, selected, label));
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact_selectable(&r, selected);
        ui.painter().rect_filled(rect, v.corner_radius, v.weak_bg_fill);
        paint(ui.painter(), rect, icon, v.fg_stroke.color);
        crate::theme::focus_ring(ui, rect, &r);
    }
    r
}

/// Width of a [`tab`] in the current style.
pub fn tab_width(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y + 4.0
}
