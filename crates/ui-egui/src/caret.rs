//! Zed-style caret for the editor: glides to its new place and fades in and out instead of toggling.
//!
//! egui's `TextEdit` paints its own caret; when the "caret" animation is on the editor hides that one (transparent
//! stroke, no blink) and paints this one. Positions are kept relative to the galley, so scrolling never makes the caret
//! glide. The blink follows egui's rhythm (`on_duration` solid, `off_duration` hidden, restarting solid at every move or
//! edit) with a short [`FADE`] at each edge. Repaints are asked for only while gliding and inside the fade windows.
use crate::anim::ease_out;
use egui::{Pos2, Vec2};

/// Length of a fade in seconds, inside egui's on/off rhythm.
pub const FADE: f32 = 0.1;

/// A move farther than this many viewport heights (galley-relative, so a jump from find/goto or a far click) snaps instead of gliding.
pub const SNAP_VIEWPORTS: f32 = 1.0;

/// Frames after a find/goto jump during which the next caret move snaps (egui applies the jump one frame late).
const JUMP_FRAMES: u8 = 3;

/// Gliding across the whole screen looks wrong: snap when the move is longer than [`SNAP_VIEWPORTS`] viewports.
pub fn snap(delta: Vec2, view_h: f32) -> bool {
    delta.length() > view_h * SNAP_VIEWPORTS
}

/// Position `t0 + dur` seconds into a glide from `from` to `to` (eased out) and whether it is still moving.
pub fn glide(from: Pos2, to: Pos2, t0: f64, now: f64, dur: f32) -> (Pos2, bool) {
    if dur <= 0.0 || from == to || now >= t0 + dur as f64 {
        return (to, false);
    }
    (from.lerp(to, ease_out(((now - t0) / dur as f64) as f32)), true)
}

/// Glide length in seconds: the `ms` setting, scaled like every other animation by the "Animation speed" setting
/// (`anim_time` is [`crate::anim::DURATION`] divided by that speed, 0 with animations off).
pub fn duration(ms: u32, anim_time: f32) -> f32 {
    ms.clamp(10, 200) as f32 / 1000.0 * anim_time / crate::anim::DURATION
}

/// Where the trail's tail is: the same move, squared-eased so it lags the head. `dur` is the tail's own, longer duration
/// (glide plus lag), so it keeps moving after the head has arrived and collapses into it at `t0 + dur`.
pub fn tail(from: Pos2, to: Pos2, t0: f64, now: f64, dur: f32) -> Pos2 {
    if dur <= 0.0 || now >= t0 + dur as f64 {
        return to;
    }
    let e = ease_out(((now - t0) / dur as f64) as f32);
    from.lerp(to, e * e)
}

/// Longest tail lag in ms (setting range) and the most rows a smear may span; farther moves have no trail.
pub const TRAIL_MAX_MS: u32 = 300;
pub const TRAIL_ROWS: f32 = 3.0;

/// Tail lag in seconds: the `ms` setting (0 = no trail), scaled by "Animation speed" like [`duration`].
pub fn trail_lag(ms: u32, anim_time: f32) -> f32 {
    ms.min(TRAIL_MAX_MS) as f32 / 1000.0 * anim_time / crate::anim::DURATION
}

/// A vertical move of `dy` points between rows `row_h` high is smeared only up to [`TRAIL_ROWS`] rows.
pub fn trail_fits(dy: f32, row_h: f32) -> bool {
    dy.abs() <= row_h * TRAIL_ROWS
}

/// Caret opacity `since` seconds after the last move, and the seconds until it next changes (0 while fading).
/// Solid for `on - fade`, fades out over `fade`, hidden for `off - fade`, fades in over `fade`, repeat.
pub fn blink(since: f64, on: f32, off: f32, fade: f32) -> (f32, f32) {
    let total = on + off;
    let fade = fade.min(on).min(off).max(f32::EPSILON);
    let t = (since.max(0.0) % total as f64) as f32;
    let smooth = |x: f32| {
        let x = (x / fade).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    if t < on - fade {
        (1.0, on - fade - t)
    } else if t < on {
        (1.0 - smooth(t - (on - fade)), 0.0)
    } else if t < total - fade {
        (0.0, total - fade - t)
    } else {
        (smooth(t - (total - fade)), 0.0)
    }
}

/// What [`Caret::paint`] needs from the editor for one frame.
pub struct Draw<'a> {
    pub range: Option<egui::text::CCursorRange>,
    pub key: (u64, u64),
    pub galley: &'a egui::Galley,
    pub galley_pos: Pos2,
    pub clip: egui::Rect,
    pub changed: bool,
    pub view_h: f32,
    /// Glide length setting in ms.
    pub glide_ms: u32,
    /// Trail lag in ms (0 = no trail), opacity at the head and tail thickness as a fraction of the caret height.
    pub trail_ms: u32,
    pub trail_opacity: f32,
    pub trail_width: f32,
}

/// What the painted caret remembers between frames.
#[derive(Default)]
pub struct Caret {
    key: Option<(u64, u64)>,
    idx: usize,
    from: Pos2,
    to: Pos2,
    t0: f64,
    reset: f64,
    jump: u8,
    /// An IME composition is in progress (egui paints the preedit caret itself).
    pub composing: bool,
    /// Where the caret was painted last (galley-relative), for tests.
    pub painted: Pos2,
    pub animating: bool,
    pub alpha: f32,
    /// Distance between the head and the tail of the trail last painted (0 when none), for tests.
    pub trail_len: f32,
}

impl Caret {
    /// The caret is not shown (no focus): the next appearance starts solid and snaps.
    pub fn hide(&mut self) {
        self.key = None;
        self.animating = false;
        self.trail_len = 0.0;
        self.alpha = 0.0;
    }

    /// Follow IME events: a non-empty preedit starts a composition, an empty one or a commit ends it.
    pub fn track_ime(&mut self, events: &[egui::Event]) {
        for e in events {
            match e {
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => self.composing = !text.is_empty(),
                egui::Event::Ime(egui::ImeEvent::Commit(_)) => self.composing = false,
                _ => {}
            }
        }
    }

    /// A find/goto jump is coming: snap instead of gliding.
    pub fn jump(&mut self) {
        self.jump = JUMP_FRAMES;
    }

    /// One frame. `(key, idx)`: `key` identifies the editor (tab and navigation), `idx` is the primary caret's char index; `changed`
    /// says the text was edited, `target` the caret's top in galley coordinates. Returns the position to paint and
    /// whether a glide is in progress. A moved caret restarts the blink solid.
    pub fn update(&mut self, now: f64, dur: f32, (key, idx): ((u64, u64), usize), changed: bool, target: Pos2, view_h: f32) -> (Pos2, bool) {
        let same = self.key == Some(key);
        let jumping = self.jump > 0;
        self.jump = self.jump.saturating_sub(1);
        if !same || idx != self.idx || changed {
            self.reset = now;
        }
        if !same {
            (self.from, self.to) = (target, target);
        } else if target != self.to {
            let (cur, _) = glide(self.from, self.to, self.t0, now, dur);
            let moved = idx != self.idx || changed;
            // shortcut: a reflow that moves the caret without moving it in the text (resize, zoom) snaps; fine, no glide wanted.
            self.from = if moved && !jumping && dur > 0.0 && !snap(target - cur, view_h) { cur } else { target };
            (self.to, self.t0) = (target, now);
        }
        (self.key, self.idx) = (Some(key), idx);
        let (p, animating) = glide(self.from, self.to, self.t0, now, dur);
        (self.painted, self.animating) = (p, animating);
        (p, animating)
    }

    /// Paint the caret (colour, width and rhythm of egui's) and ask for the repaints it needs: every frame while
    /// gliding, only inside the fade windows while blinking, none without focus (`d.range` is `None`).
    pub fn paint(&mut self, ui: &egui::Ui, d: Draw) {
        let Some(range) = d.range else {
            self.hide();
            return;
        };
        let style = ui.visuals().text_cursor.clone(); // the editor's own override is scoped to its child ui
        let now = ui.input(|i| i.time);
        let dur = duration(d.glide_ms, ui.style().animation_time);
        // Like egui: the row's box, a little taller than the text.
        let r = d.galley.pos_from_cursor(range.primary).expand(1.5);
        let (p, gliding) = self.update(now, dur, (d.key, range.primary.index.into()), d.changed, r.center_top(), d.view_h);
        let (alpha, wake) = if style.blink { blink(self.since(now), style.on_duration, style.off_duration, FADE) } else { (1.0, f32::INFINITY) };
        self.alpha = alpha;
        let top = p + d.galley_pos.to_vec2();
        if alpha > 0.0 {
            let stroke = egui::Stroke::new(style.stroke.width, style.stroke.color.gamma_multiply(alpha));
            ui.painter_at(d.clip).line_segment([top, top + egui::vec2(0.0, r.height())], stroke);
        }
        self.trail_len = 0.0;
        // A smear from the full-height head to a thinner tail that keeps easing after the head has arrived.
        let lag = trail_lag(d.trail_ms, ui.style().animation_time);
        // shortcut: a new move restarts the tail at the head's current place, so fast typing smears one step; upgrade by carrying the tail over.
        let moving = lag > 0.0 && self.from != self.to && now < self.t0 + (dur + lag) as f64;
        let t = tail(self.from, self.to, self.t0, now, dur + lag) + d.galley_pos.to_vec2();
        let h = r.height();
        let trailing = moving && trail_fits(self.to.y - self.from.y, h);
        if trailing {
            self.trail_len = (top - t).length();
            let half = d.trail_width.clamp(0.0, 1.0) * h / 2.0;
            let cy = t.y + h / 2.0;
            let col = |a: f32| style.stroke.color.gamma_multiply(a);
            // A same-row smear is a bit lighter so it does not read as a selection.
            let head = col(d.trail_opacity.clamp(0.1, 1.0) * ((top.y - t.y) / h).abs().clamp(0.6, 1.0));
            let mut m = egui::Mesh::default();
            m.colored_vertex(top, head);
            m.colored_vertex(top + egui::vec2(0.0, h), head);
            m.colored_vertex(egui::pos2(t.x, cy + half), col(0.0));
            m.colored_vertex(egui::pos2(t.x, cy - half), col(0.0));
            m.add_triangle(0, 1, 2);
            m.add_triangle(0, 2, 3);
            ui.painter_at(d.clip).add(m);
        }
        if gliding || trailing || wake == 0.0 {
            ui.request_repaint();
        } else if wake.is_finite() {
            ui.request_repaint_after_secs(wake);
        }
    }

    /// Seconds since the last move or edit, for [`blink`].
    pub fn since(&self, now: f64) -> f64 {
        now - self.reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn glide_eases_from_old_to_new_and_ends() {
        let (a, b) = (pos2(0.0, 0.0), pos2(10.0, 0.0));
        assert_eq!(glide(a, b, 1.0, 1.0, 0.08), (a, true));
        let (mid, on) = glide(a, b, 1.0, 1.04, 0.08);
        assert!(on && mid.x > 5.0 && mid.x < 10.0, "ease-out is past half way at half time: {mid:?}");
        assert_eq!(glide(a, b, 1.0, 1.08, 0.08), (b, false));
        assert_eq!(glide(a, b, 1.0, 1.0, 0.0), (b, false), "zero duration snaps");
    }

    #[test]
    fn duration_follows_setting_and_speed() {
        assert!((duration(45, crate::anim::DURATION) - 0.045).abs() < 1e-6);
        assert!((duration(45, crate::anim::DURATION / 2.0) - 0.0225).abs() < 1e-6, "speed 2 halves it");
        assert_eq!(duration(45, 0.0), 0.0, "animations off");
    }

    #[test]
    fn tail_lags_the_head_and_both_arrive() {
        let (a, b) = (pos2(0.0, 0.0), pos2(100.0, 0.0));
        for now in [1.01, 1.02, 1.03, 1.04] {
            let (head, _) = glide(a, b, 1.0, now, 0.05);
            assert!(tail(a, b, 1.0, now, 0.05).x < head.x, "tail behind at {now}");
        }
        assert_eq!(tail(a, b, 1.0, 1.0, 0.05), a);
        assert_eq!(tail(a, b, 1.0, 1.05, 0.05), b);
        assert_eq!(tail(a, a, 1.0, 1.02, 0.05), a, "no move, no trail (snaps leave from == to)");
    }

    #[test]
    fn tail_with_lag_trails_after_the_head_arrives_and_arrives_at_lag() {
        let (a, b) = (pos2(0.0, 0.0), pos2(100.0, 0.0));
        let (glide_d, lag) = (0.045_f32, 0.11_f32);
        let now = 1.0 + glide_d as f64 + 0.005;
        assert_eq!(glide(a, b, 1.0, now, glide_d), (b, false), "head has arrived");
        let t = tail(a, b, 1.0, now, glide_d + lag);
        assert!(t.x > 0.0 && t.x < 90.0, "tail still on its way: {t:?}");
        assert_eq!(tail(a, b, 1.0, 1.0 + (glide_d + lag) as f64, glide_d + lag), b, "arrives after glide + lag");
    }

    #[test]
    fn trail_options_are_clamped_and_gated() {
        let d = crate::anim::DURATION;
        assert_eq!(trail_lag(0, d), 0.0, "0 = no trail");
        assert!((trail_lag(110, d) - 0.11).abs() < 1e-6);
        assert!((trail_lag(9999, d) - 0.3).abs() < 1e-6, "clamped to 300 ms");
        assert_eq!(trail_lag(110, 0.0), 0.0, "animations off");
        assert!((trail_lag(110, d / 2.0) - 0.055).abs() < 1e-6, "speed 2 halves it");
        assert!(trail_fits(0.0, 20.0) && trail_fits(-60.0, 20.0));
        assert!(!trail_fits(61.0, 20.0), "more than 3 rows: no trail");
    }

    #[test]
    fn far_moves_snap() {
        assert!(!snap(Vec2::new(8.0, 0.0), 600.0));
        assert!(!snap(Vec2::new(0.0, 500.0), 600.0));
        assert!(snap(Vec2::new(0.0, 700.0), 600.0));
    }

    #[test]
    fn blink_is_solid_after_reset_fades_and_cycles() {
        let b = |t| blink(t, 0.5, 0.5, 0.1);
        assert_eq!(b(0.0).0, 1.0);
        let (a, wake) = b(0.3);
        assert_eq!(a, 1.0);
        assert!((wake - 0.1).abs() < 1e-5, "next change is the fade-out start: {wake}");
        let (a, wake) = b(0.45);
        assert!(a > 0.0 && a < 1.0 && wake == 0.0, "fading out: {a}");
        assert_eq!(b(0.7).0, 0.0);
        let (a, wake) = b(0.95);
        assert!(a > 0.0 && a < 1.0 && wake == 0.0, "fading in: {a}");
        assert_eq!(b(1.0).0, 1.0, "a new cycle starts solid");
        assert_eq!(b(5.3), (1.0, b(0.3).1));
    }

    #[test]
    fn update_glides_on_a_step_snaps_on_tab_change_jump_and_far_move() {
        let mut c = Caret::default();
        let k = (1, 0);
        // First sight: no glide.
        assert_eq!(c.update(0.0, 0.08, (k, 0), false, pos2(0.0, 0.0), 600.0), (pos2(0.0, 0.0), false));
        // One typed char: glides.
        let (p, on) = c.update(1.0, 0.08, (k, 1), true, pos2(8.0, 0.0), 600.0);
        assert!(on && p.x == 0.0);
        let (p, on) = c.update(1.04, 0.08, (k, 1), false, pos2(8.0, 0.0), 600.0);
        assert!(on && p.x > 0.0 && p.x < 8.0);
        assert_eq!(c.update(1.1, 0.08, (k, 1), false, pos2(8.0, 0.0), 600.0), (pos2(8.0, 0.0), false));
        // Far move snaps; other tab snaps; same index but moved caret (reflow) snaps.
        assert!(!c.update(2.0, 0.08, (k, 9), false, pos2(8.0, 900.0), 600.0).1);
        assert!(!c.update(3.0, 0.08, ((2, 0), 9), false, pos2(8.0, 20.0), 600.0).1);
        assert!(!c.update(4.0, 0.08, ((2, 0), 9), false, pos2(40.0, 20.0), 600.0).1);
        // A find/goto jump (flagged a frame before the caret moves) snaps.
        c.jump();
        assert!(!c.update(5.0, 0.08, ((2, 0), 9), false, pos2(40.0, 20.0), 600.0).1);
        assert!(!c.update(5.1, 0.08, ((2, 0), 30), false, pos2(80.0, 40.0), 600.0).1);
        // A jump that lands on the same index must not make a later arrow move snap.
        c.jump();
        for t in 0..4 {
            c.update(5.2 + t as f64 * 0.1, 0.08, ((2, 0), 30), false, pos2(80.0, 40.0), 600.0);
        }
        assert!(c.update(6.0, 0.08, ((2, 0), 31), false, pos2(88.0, 40.0), 600.0).1);
        // Hiding forgets everything; animations off (dur 0) never glide.
        c.hide();
        assert!(!c.update(6.0, 0.0, ((2, 0), 31), true, pos2(90.0, 40.0), 600.0).1);
        assert!(!c.update(7.0, 0.0, ((2, 0), 32), true, pos2(98.0, 40.0), 600.0).1);
    }
}
