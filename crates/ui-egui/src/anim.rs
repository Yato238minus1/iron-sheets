//! Animation helpers: one duration, one easing, and the arrow that marks a button which opens a list or a folder that opens.
//!
//! The duration is `Style::animation_time`, set in `Theme::rebuild_styles` ([`DURATION`] divided by the "Animation
//! speed" setting, or 0 with the master switch "Animations" off), so a single setting also scales egui's own
//! transitions. The [`Group`] switches turn one kind of the app's own animations off; they are a thread-local
//! bitmask, read for free. egui requests repaints only while an animated value is between its two ends, so an idle
//! frame costs exactly what it did before; after the animation settles no wakeup is left over.
use egui::{Color32, Id, Painter, PopupCloseBehavior, Pos2, Shape, Stroke, Ui, emath::easing, vec2};

/// Length of the app's transitions in seconds, before the "Animation speed" multiplier.
pub const DURATION: f32 = 0.08;

/// `Style::animation_time` for the master switch: [`DURATION`] at `speed` 1, or 0 when animations are off.
/// A hand-edited config could hold any speed; the clamp keeps the result finite and sane.
pub fn duration(animations: bool, speed: f32) -> f32 {
    if animations { DURATION / speed.clamp(0.25, 4.0) } else { 0.0 }
}

/// The kinds of animation the user can switch off one by one (besides the master switch).
#[derive(Clone, Copy)]
pub enum Group {
    /// Dropdown arrows, menu fade-in.
    Menus,
    /// The view toolbar and the sidebar tab row gliding into one button.
    Collapse,
    /// The sidebar sliding out and in.
    Slide,
    /// Folders opening and closing in the file tree, with their arrow.
    Tree,
    /// Hover fades of the tabs.
    Hover,
    /// The UI zoom factor easing to a new value.
    Zoom,
    /// The editor caret gliding and fading (see `caret`).
    Caret,
}

thread_local! {
    static GROUPS: std::cell::Cell<u8> = const { std::cell::Cell::new(0b1111111) };
}

/// Take the group switches from the settings (called with the styles, whenever the settings change).
pub fn set_groups(c: &iron_sheets_core::config::AppConfig) {
    let bits =
        [c.anim_menus, c.anim_collapse, c.anim_slide, c.anim_tree, c.anim_hover, c.anim_zoom, c.anim_caret].iter().enumerate().fold(0, |m, (i, on)| m | (*on as u8) << i);
    GROUPS.with(|g| g.set(bits));
}

pub fn enabled(g: Group) -> bool {
    GROUPS.with(|b| b.get() >> g as u8 & 1 == 1)
}

/// 0..=1 towards `open`, eased out. First sight of `id` returns the target at once (no animation on first draw).
/// With the group switched off the value jumps to the target.
pub fn bool_t(ui: &Ui, id: Id, open: bool, group: Group) -> f32 {
    let time = if enabled(group) { ui.style().animation_time } else { 0.0 };
    ui.ctx().animate_bool_with_time_and_easing(id, open, time, easing::cubic_out)
}

/// Per-slot fades for lists (the hover fade of each tab). They are plain numbers in a thread-local, so a steady frame
/// takes no lock and nothing keyed by an id is left behind when a row goes away (the vector follows the row count).
/// Call [`fades_begin`] once per frame, then [`fade`] per row.
pub struct Fades {
    v: Vec<f32>,
    step: f32,
    moving: bool,
}

thread_local! {
    static FADES: std::cell::RefCell<Fades> = const { std::cell::RefCell::new(Fades { v: Vec::new(), step: 1.0, moving: false }) };
}

/// Start a frame of `n` slots with the transition length of the UI style.
pub fn fades_begin(ui: &Ui, n: usize) {
    let (dt, dur) = (ui.input(|i| i.stable_dt), ui.style().animation_time);
    FADES.with(|f| {
        let mut f = f.borrow_mut();
        f.v.resize(n, 0.0);
        f.step = if dur > 0.0 && enabled(Group::Hover) { dt / dur } else { 1.0 };
        f.moving = false;
    });
}

/// Eased 0..=1 of slot `i` moving towards `on`.
pub fn fade(i: usize, on: bool) -> f32 {
    FADES.with(|f| {
        let Fades { v, step, moving } = &mut *f.borrow_mut();
        let target = if on { 1.0 } else { 0.0 };
        let Some(v) = v.get_mut(i) else { return target };
        if *v != target {
            *v = if on { (*v + *step).min(1.0) } else { (*v - *step).max(0.0) };
            *moving |= *v != target;
        }
        ease_out(*v)
    })
}

/// End of the frame: asks for one more frame while any slot is still moving.
pub fn fades_end(ui: &Ui) {
    if FADES.with(|f| f.borrow().moving) {
        ui.ctx().request_repaint();
    }
}

pub fn ease_out(t: f32) -> f32 {
    easing::cubic_out(t.clamp(0.0, 1.0))
}

/// Half-size of the arrow: the file tree's folder arrow is 8 points tall pointing right.
pub const ARROW_R: f32 = 4.0;

/// The three points of the arrow centred on `c` with half-height `r`: `t` = 0 points right, `t` = 1 points down.
/// The triangle is rotated around its centre by `t * 90` degrees; pure so it can be tested.
pub fn chevron_points(c: Pos2, r: f32, t: f32) -> [Pos2; 3] {
    // Pointing right: tip at +x, base to the left.
    let base = [vec2(-0.7 * r, -r), vec2(0.7 * r, 0.0), vec2(-0.7 * r, r)];
    let a = t.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    let (s, co) = a.sin_cos();
    base.map(|p| c + vec2(p.x * co - p.y * s, p.x * s + p.y * co))
}

/// Paint the filled disclosure arrow centred on `c`: right when closed (`t` = 0), down when open (`t` = 1, from
/// [`bool_t`]). The file tree's folders and the menu buttons share it.
pub fn arrow(painter: &Painter, c: Pos2, t: f32, color: Color32) {
    painter.add(Shape::convex_polygon(chevron_points(c, ARROW_R, t).to_vec(), color, Stroke::NONE));
}

/// Size reserved for the arrow inside a button.
pub const CHEVRON_SIZE: f32 = 12.0;

/// A menu button with a label and, when `with_chevron`, an arrow before the label (like a folder in the file tree) that rotates from right (closed) to down (open). The
/// open state is the one of the previous frame (stored under `id`), so the button never needs the popup id before it
/// is shown; a change of state asks for one more frame. The popup content fades in. Reports the expanded state to
/// accessibility tools.
pub fn menu_button<R>(
    ui: &mut Ui,
    id: Id,
    label: &str,
    with_chevron: bool,
    content: impl FnOnce(&mut Ui) -> R,
) -> (egui::Response, Option<egui::InnerResponse<R>>) {
    menu_button_with(ui, id, label, with_chevron, PopupCloseBehavior::CloseOnClick, content)
}

/// [`menu_button`] with a choice of when a click closes the menu (`CloseOnClickOutside`: only a click outside or Esc).
pub fn menu_button_with<R>(
    ui: &mut Ui,
    id: Id,
    label: &str,
    with_chevron: bool,
    close: PopupCloseBehavior,
    content: impl FnOnce(&mut Ui) -> R,
) -> (egui::Response, Option<egui::InnerResponse<R>>) {
    let cfg = egui::containers::menu::MenuConfig::new().close_behavior(close);
    let was_open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
    let fade_id = id.with("fade");
    let body = |ui: &mut Ui| {
        let a = popup_fade(ui, fade_id);
        ui.set_opacity(a);
        content(ui)
    };
    let (resp, inner) = if with_chevron {
        let t = bool_t(ui, id.with("chev"), was_open, Group::Menus);
        let color = ui.visuals().weak_text_color();
        let chev = egui::Atom::paint(vec2(CHEVRON_SIZE, CHEVRON_SIZE), move |ui, args| arrow(ui.painter(), args.rect.center(), t, color));
        egui::containers::menu::MenuButton::new((chev, label)).config(cfg).ui(ui, body)
    } else {
        egui::containers::menu::MenuButton::new(label).config(cfg).ui(ui, body)
    };
    let open = inner.is_some();
    if open != was_open {
        if !open {
            popup_closed(ui, fade_id);
        }
        ui.data_mut(|d| d.insert_temp(id, open));
        ui.ctx().request_repaint();
    }
    ui.ctx().accesskit_node_builder(resp.id, |b| b.set_expanded(open));
    (resp, inner)
}

/// `Response::context_menu` that also works for a right-click while a menu is already open.
/// egui keeps one open popup per viewport. A secondary click on the widget whose menu is open re-opens it at the new
/// position, but in the same `Popup::show` the release counts as a click after a press that began while the menu was
/// open, so the close-on-click check closes it again (egui 0.37 `containers/popup.rs`, `press_started_while_open`).
/// Ignoring clicks for the frame of the secondary click leaves the freshly opened menu alone; Esc still closes it.
pub fn context_menu(resp: &egui::Response, close: PopupCloseBehavior, content: impl FnOnce(&mut Ui)) {
    let close = if resp.secondary_clicked() { PopupCloseBehavior::IgnoreClicks } else { close };
    egui::Popup::context_menu(resp).close_behavior(close).show(content);
}

/// Fade-in of the content of a popup: call at the top of the popup content with a stable `id`; returns the opacity.
/// Remembers when the popup opened (cleared by [`popup_closed`] when the button sees it closed).
pub fn popup_fade(ui: &Ui, id: Id) -> f32 {
    let now = ui.input(|i| i.time);
    let t0: f64 = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let dur = ui.style().animation_time as f64;
    if dur <= 0.0 || !enabled(Group::Menus) {
        return 1.0;
    }
    let t = ((now - t0) / dur).clamp(0.0, 1.0) as f32;
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
    ease_out(t)
}

/// Forget the open time of a popup (the popup is closed), so it fades in again next time.
pub fn popup_closed(ui: &Ui, id: Id) {
    ui.data_mut(|d| d.remove::<f64>(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn easing_is_monotone_with_fixed_ends() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..=20 {
            let v = ease_out(i as f32 / 20.0);
            assert!(v >= prev);
            prev = v;
        }
        assert!(ease_out(0.5) > 0.5, "ease-out starts fast");
    }

    #[test]
    fn chevron_rotates_from_right_to_down() {
        let c = pos2(10.0, 10.0);
        let closed = chevron_points(c, 4.0, 0.0);
        let open = chevron_points(c, 4.0, 1.0);
        // Closed: the tip (middle point) is to the right of the arms; open: below them.
        assert!(closed[1].x > closed[0].x && (closed[1].y - c.y).abs() < 1e-4);
        assert!(open[1].y > open[0].y && (open[1].x - c.x).abs() < 1e-4);
        let half = chevron_points(c, 4.0, 0.5);
        assert!(half[1].x > c.x && half[1].y > c.y, "in between: pointing down-right");
    }

    /// Drive `bool_t` at 60 fps: it rises monotonically over ~DURATION, asks for frames only while it moves and is quiet
    /// (no repaint requested) once it arrived; with `animation_time` 0 it is instant.
    #[test]
    fn bool_animation_runs_then_requests_no_more_frames() {
        for reduce in [false, true] {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|s| s.animation_time = duration(!reduce, 1.0));
            let id = Id::NULL.with("t");
            let mut values = Vec::new();
            let mut repaint_frames = 0;
            for frame in 0..30 {
                let raw = egui::RawInput { time: Some(frame as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
                let mut out = ctx.run_ui(raw, |ui| {
                    // The target flips to `true` on frame 5 (first sight at frame 0 is `false`).
                    values.push(bool_t(ui, id, frame >= 5, Group::Tree));
                });
                out.textures_delta.clear();
                // The first two frames of a fresh context ask for frames by themselves; they are not the animation.
                if frame >= 2 && out.viewport_output.values().any(|v| v.repaint_delay == std::time::Duration::ZERO) {
                    repaint_frames += 1;
                }
            }
            assert!(values.windows(2).all(|w| w[1] >= w[0] - 1e-6), "monotone: {values:?}");
            assert_eq!(values[4], 0.0);
            assert_eq!(*values.last().unwrap(), 1.0);
            if reduce {
                assert_eq!(values[5], 1.0, "instant");
                assert_eq!(repaint_frames, 0, "no frame requested");
            } else {
                assert!(values[5] > 0.0 && values[5] < 1.0, "in between on the first frame: {}", values[5]);
                // 0.08 s at 60 fps is about 5 frames; allow the frame the animation ends on.
                assert!((4..=8).contains(&repaint_frames), "{repaint_frames} frames asked for a repaint");
                // After arriving no frame asks for more.
                assert!(values[15..].iter().all(|v| *v == 1.0));
            }
        }
    }

    /// The slot fade runs towards its target in steps, reports no motion once arrived and follows the slot count.
    #[test]
    fn slot_fade_moves_then_rests() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.animation_time = DURATION);
        let mut seen = Vec::new();
        for frame in 0..20 {
            let raw = egui::RawInput { time: Some(frame as f64 / 60.0), predicted_dt: 1.0 / 60.0, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                fades_begin(ui, if frame < 15 { 3 } else { 1 });
                seen.push((fade(1, frame < 10), FADES.with(|f| f.borrow().v.len())));
                fades_end(ui);
            });
            out.textures_delta.clear();
        }
        assert!(seen[1].0 > 0.0 && seen[1].0 < 1.0, "in between: {}", seen[1].0);
        assert_eq!(seen[9].0, 1.0);
        assert_eq!(seen[19].0, 0.0, "back to rest");
        assert_eq!(seen[19].1, 1, "slots follow the count");
        assert!(!FADES.with(|f| f.borrow().moving));
    }

    #[test]
    fn reduced_motion_is_instant() {
        assert_eq!(duration(false, 1.0), 0.0);
        assert_eq!(duration(true, 1.0), DURATION);
        assert_eq!(duration(true, 2.0), DURATION / 2.0);
        // A hand-edited config cannot make it infinite or negative.
        assert!(duration(true, 0.0).is_finite() && duration(true, 0.0) > DURATION);
    }
}
