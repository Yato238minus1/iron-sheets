//! Auxiliary windows (Settings now, other dialogs later): a real OS window where the backend has them, an
//! `egui::Window` inside the app window where it has not.
//!
//! Native (eframe glow on Linux X11/Wayland, Windows, macOS): an *immediate* viewport (`Context::show_viewport_immediate`).
//! The content closure runs inside the root's frame with the caller's `&mut` state, so no shared state is needed. The
//! window exists exactly as long as `show` is called every frame; stopping the calls closes it (egui drops the viewport
//! at the end of the root pass and eframe destroys the window), so nothing dangles. Repaints of the window go through the
//! root (eframe re-runs the parent); while neither window has input no frame runs at all.
//!
//! Embedded (`Context::embed_viewports()`: the kittest harness, Web, a backend without viewports): an `egui::Window`
//! constrained to the app window, with its own close button.
//!
//! The persisted geometry is plain numbers (`AuxWindowGeom`, no egui types). Position is only recorded and restored
//! where the platform reports one (X11, Windows, macOS); on Wayland clients can neither read nor set window positions,
//! `outer_rect` is `None` and the compositor places the window.
use egui::{Align2, Context, Id, LayerId, Order, Ui, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};
use iron_sheets_core::config::AuxWindowGeom;

/// What identifies and sizes an auxiliary window.
pub struct AuxSpec<'a> {
    /// Stable key (viewport id, temp data). Not shown.
    pub id: &'a str,
    pub title: &'a str,
    /// Used when no geometry was stored (points).
    pub default_size: Vec2,
    pub min_size: Vec2,
}

/// Result of one `show` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shown {
    /// The user asked to close it (OS close button, Esc, close control). The caller stops calling `show`.
    pub close: bool,
    /// `geom` changed this frame (the caller persists it).
    pub geom_changed: bool,
}

/// Size and position the window was created with: kept so the builder stays constant while the user resizes
/// (eframe sends a resize command whenever the builder changes).
#[derive(Clone, Copy)]
struct Initial {
    size: Vec2,
    pos: Option<egui::Pos2>,
}

fn initial_key(spec: &AuxSpec) -> Id {
    Id::unique(("aux_window_initial", spec.id))
}

/// Viewport id of an auxiliary window.
pub fn viewport_id(spec_id: &str) -> ViewportId {
    ViewportId::from_hash_of(("aux_window", spec_id))
}

/// Forget the creation geometry: the next `show` builds the window from the stored geometry again.
pub fn reset(ctx: &Context, spec: &AuxSpec) {
    let key = initial_key(spec);
    if ctx.data(|d| d.get_temp::<Initial>(key)).is_some() {
        ctx.data_mut(|d| d.remove::<Initial>(key));
    }
}

/// True when `show` opens a real OS window (not an embedded `egui::Window`).
pub fn is_native(ctx: &Context) -> bool {
    !ctx.embed_viewports()
}

/// A stored position is only used when it is plausible; a monitor that went away must not leave the window out of reach.
fn sane_pos(x: f32, y: f32) -> bool {
    x.is_finite() && y.is_finite() && x.abs() < 20_000.0 && (-50.0..20_000.0).contains(&y)
}

/// Shows the window. `focus` raises and focuses it (also the frame it is created in). `add` draws the content.
pub fn show(ctx: &Context, spec: &AuxSpec, geom: &mut AuxWindowGeom, focus: bool, mut add: impl FnMut(&mut Ui)) -> Shown {
    if !is_native(ctx) {
        return show_embedded(ctx, spec, focus, add);
    }
    let vid = viewport_id(spec.id);
    let key = initial_key(spec);
    let first = ctx.data(|d| d.get_temp::<Initial>(key));
    let initial = first.unwrap_or_else(|| {
        let size = Vec2::new(geom.width, geom.height).max(spec.min_size);
        let pos = geom.x.zip(geom.y).filter(|&(x, y)| sane_pos(x, y)).map(|(x, y)| egui::pos2(x, y));
        let i = Initial { size, pos };
        ctx.data_mut(|d| d.insert_temp(key, i));
        i
    });
    let mut builder = ViewportBuilder::default().with_title(spec.title).with_app_id("iron-sheets").with_inner_size(initial.size).with_min_inner_size(spec.min_size);
    if let Some(p) = initial.pos {
        builder = builder.with_position(p);
    }
    let created = first.is_none();
    let mut out = Shown::default();
    // eframe 0.37 returns from its immediate-viewport renderer without calling us when it cannot set up the window in
    // this frame (`glow_integration.rs::render_immediate_viewport`), and egui then panics ("the user callback was never
    // called", `context.rs` `show_viewport_immediate`). Seen once on X11 (XWayland) right after start, not reproducible.
    // Nothing of ours ran in that case, so the panic is caught and the window is tried again shortly.
    let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.show_viewport_immediate(vid, builder, |ui, class| {
            let ctx = ui.ctx().clone();
            if created || focus {
                ctx.send_viewport_cmd_to(vid, ViewportCommand::Focus);
            }
            // Geometry and close request of this window (the input of this callback is the window's own).
            // `inner_rect` is `None` on Wayland (no window positions); the viewport rect is the window size everywhere.
            let (close_requested, inner, outer, minimized) = ctx.input(|i| {
                let v = i.viewport();
                (v.close_requested(), v.inner_rect.unwrap_or_else(|| i.viewport_rect()), v.outer_rect, v.minimized.unwrap_or(false))
            });
            if !minimized {
                let mut g = *geom;
                if let Some(r) = Some(inner).filter(|r| r.width() >= 100.0 && r.height() >= 100.0) {
                    g.width = r.width();
                    g.height = r.height();
                }
                if let Some(o) = outer {
                    g.x = Some(o.min.x);
                    g.y = Some(o.min.y);
                }
                let moved = (g.width - geom.width).abs() > 0.5 || (g.height - geom.height).abs() > 0.5 || g.x != geom.x || g.y != geom.y;
                if moved {
                    *geom = g;
                    out.geom_changed = true;
                }
            }
            if close_requested {
                out.close = true;
            }
            egui::CentralPanel::default().show(ui, |ui| {
                if class == ViewportClass::EmbeddedWindow {
                    // A backend without viewports wrapped us in a titled window that has no close button.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        if ui.button("Close").clicked() {
                            out.close = true;
                        }
                    });
                }
                add(ui);
            });
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                out.close = true;
            }
        })
    }));
    if shown.is_err() {
        reset(ctx, spec);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    out
}

fn show_embedded(ctx: &Context, spec: &AuxSpec, focus: bool, mut add: impl FnMut(&mut Ui)) -> Shown {
    // Centred, and always fully inside the app window, also after the window was made smaller.
    let screen = ctx.content_rect();
    let room = screen.shrink(6.0);
    let size = Vec2::new(spec.default_size.x.min(room.width()), spec.default_size.y.min(room.height()));
    let mut open = true;
    let win_id = Id::unique(("aux_window_embedded", spec.id));
    let layer = LayerId::new(Order::Middle, win_id);
    if focus {
        ctx.move_to_top(layer);
    }
    egui::Window::new(spec.title)
        .id(win_id)
        .open(&mut open)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .default_size(size)
        .min_size(size.min(spec.min_size))
        .max_size(room.size())
        .constrain_to(room)
        .collapsible(false)
        .show(ctx, |ui| add(ui));
    let mut out = Shown { close: !open, geom_changed: false };
    // Esc closes it while it is the top window (a palette or dialog above it takes the key first).
    if ctx.top_layer_id() == Some(layer) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        out.close = true;
    }
    out
}
