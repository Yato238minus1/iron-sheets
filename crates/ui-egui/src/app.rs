//! The eframe application: frame loop and glue.
use crate::command::{self, Command};
use crate::dialogs::{self, Pick};
use crate::state::{AppOptions, AppState};
use crate::{menu, sidebar, statusbar, tabstrip, theme};
use iron_sheets_core::config::WindowGeom;
use iron_sheets_core::watch::FsEvent;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct App {
    pub state: AppState,
    font_missing: crate::sysfont::Missing,
    pub frames: u32,
    /// Exit after frame 3 (`--bench-startup`).
    pub bench: bool,
    /// With `bench`: keep running until index, tree and watcher are done (`--bench-startup=full`).
    pub bench_full: bool,
    bench_marks: [bool; 5],
    /// Save a PNG after a few frames and exit (`--screenshot`).
    pub screenshot: Option<PathBuf>,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
    last_title: String,
    geom: Option<WindowGeom>,
    settings_milestone: bool,
    bench_closed: bool,
    ime_filter: crate::ime_filter::ImeFilter,
    /// Ctrl+wheel (zoom) input not yet turned into whole font steps, in log units.
    zoom_acc: f32,
    /// `MarkdownPreview::frames_shown` seen last frame.
    md_shown: u64,
    zoom_last: f64,
    /// Scroll position (source lines at the top of each pane) to restore after a font size change, and for how many more
    /// frames: the preview measures its blocks lazily, so its map settles over a few frames.
    pending_anchor: Option<(ScrollAnchor, u8)>,
}

/// Source line at the top of each pane of a tab, captured before a font size change.
#[derive(Debug, Clone, Copy)]
struct ScrollAnchor {
    tab: u64,
    editor: Option<f32>,
    preview: Option<f32>,
}

/// One font step (1 pt on both fonts) per this much Ctrl+wheel input (natural log of the zoom factor egui reports;
/// one notch of a mouse wheel is 0.2, so a notch is one step, with a little left over and a smooth touchpad needs a longer swipe).
const WHEEL_STEP: f32 = 0.18;

/// Set by a preview that zooms by itself on Ctrl+wheel (the Typst page view): `(its rect, frame number)`. While the
/// pointer is over it the wheel belongs to it and does not change the font sizes.
pub fn wheel_owner_id() -> egui::Id {
    egui::Id::NULL.with("iron_sheets_wheel_zoom_owner")
}

/// Whole font steps from this frame's Ctrl+wheel (or pinch) input; the remainder is kept in `acc` for the next frames.
fn wheel_font_steps(ctx: &egui::Context, acc: &mut f32, last: &mut f64) -> i32 {
    let (zd, now, ptr) = ctx.input(|i| (i.zoom_delta(), i.time, i.pointer.latest_pos()));
    if (zd - 1.0).abs() < 1e-5 {
        return 0;
    }
    // A new swipe starts from zero (egui sends no idle frame in between, so reset here, not when the wheel stops).
    if now - *last > 0.4 {
        *acc = 0.0;
    }
    if let (Some(p), Some((r, frame))) = (ptr, ctx.data(|d| d.get_temp::<(egui::Rect, u64)>(wheel_owner_id()))) {
        if frame + 1 >= ctx.cumulative_frame_nr() && r.contains(p) {
            return 0;
        }
    }
    *last = now;
    *acc += zd.ln();
    let steps = (*acc / WHEEL_STEP).trunc();
    *acc -= steps * WHEEL_STEP;
    steps as i32
}

fn capture_anchor(st: &AppState) -> Option<ScrollAnchor> {
    let t = st.tabs.active_tab().filter(|t| !t.is_empty())?;
    let line = |p: &iron_sheets_app_core::scrollsync::PaneScroll| (!p.map.is_empty() && p.offset > 0.5).then(|| p.map.line_for_y(p.offset));
    Some(ScrollAnchor { tab: t.id, editor: line(&t.editor_scroll), preview: line(&t.preview_scroll) })
}

/// Width the central area keeps free next to the sidebar.
pub const MIN_CENTRAL_WIDTH: f32 = 300.0;

/// (min, max) sidebar width for a window of `window_w` points.
pub fn sidebar_width_range(window_w: f32) -> (f32, f32) {
    let max = (window_w - MIN_CENTRAL_WIDTH).max(80.0);
    (140.0_f32.min(max), max)
}

/// Widest glyph atlas we ask for. egui makes it as wide as the GPU allows (16384 px: a 2 MB texture that is allocated,
/// copied and uploaded in frame 1, about 2.5 ms before the window can show). It grows downwards when it needs room.
/// shortcut: the same cap is what `max_texture_side` reports to the UI, so Typst pages render at most 4096 px per side (A4: ~4.9 px/pt,
/// sharp to ~240% zoom on a 2x screen); upgrade by tiling pages (egui debug-asserts textures against this value).
const ATLAS_SIDE: usize = 4096;

/// Reports at most [`ATLAS_SIDE`] as the maximum texture side, in every viewport (egui reads it to size the atlas).
struct AtlasSide;

impl egui::Plugin for AtlasSide {
    fn debug_name(&self) -> &'static str {
        "atlas_side"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        // Only the first input of a viewport carries it; later passes keep the value egui stored.
        if let Some(side) = input.max_texture_side.as_mut() {
            *side = (*side).min(ATLAS_SIDE);
        }
    }
}

impl App {
    pub fn new(ctx: &egui::Context, opts: AppOptions) -> App {
        ctx.add_plugin(AtlasSide);
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        let mut state = crate::state::new_state(Arc::new(crate::platform_egui::EguiWaker(ctx.clone())), opts);
        // Fonts are built once before the first frame; a font file is read only when one is configured.
        let choice = theme::FontChoice::of(&iron_sheets_app_core::effective::effective(&state.config, state.settings.theme.as_ref()));
        let font_errors = theme::install_fonts(ctx, &choice);
        state.ui.theme.fonts_installed(choice, font_errors);
        let font_missing = crate::sysfont::install(ctx);
        state.perf.milestone("app created");
        // Settings was open when the app quit: reopen it (after the first frames, see `frame`).
        if state.config.settings_window.is_some_and(|g| g.open) {
            state.settings.open = true;
        }
        App {
            state,
            font_missing,
            frames: 0,
            bench: false,
            bench_full: false,
            bench_marks: [false; 5],
            screenshot: None,
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
            last_title: String::new(),
            geom: None,
            settings_milestone: false,
            bench_closed: false,
            ime_filter: Default::default(),
            zoom_acc: 0.0,
            md_shown: 0,
            zoom_last: 0.0,
            pending_anchor: None,
        }
    }

    fn handle_fs(state: &mut AppState, evs: Vec<FsEvent>) {
        state.modules.on_fs_event(&evs);
        for ev in evs {
            match &ev {
                FsEvent::Rescan => {
                    state.indexer.rescan();
                    state.tree.invalidate_all();
                    state.docs.prefetch.invalidate_all();
                }
                FsEvent::Changed(p) | FsEvent::Removed(p) => {
                    let removed = matches!(ev, FsEvent::Removed(_));
                    let p = p.clone();
                    state.docs.prefetch.invalidate(&p);
                    if let Some(parent) = p.parent() {
                        state.tree.invalidate(parent);
                    }
                    state.tree.invalidate(&p);
                    let mut dirty = false;
                    // A stat here (never in a frame): the event can be one half of a rename-over-save.
                    if let Some(d) = state.docs.get_mut(&p) {
                        d.deleted = !p.exists();
                    }
                    if !removed && state.docs.changed_on_disk(&p) {
                        if state.docs.get(&p).is_some_and(|d| d.is_dirty()) {
                            if let Some(d) = state.docs.get_mut(&p) {
                                d.external_change = true;
                            }
                            dirty = true;
                        } else if let Err(e) = state.docs.reload(&p) {
                            state.status.error(format!("Cannot reload {}: {e}", p.display()));
                        } else {
                            command::fix_cursors(state, &p);
                        }
                    } else if state.docs.get(&p).is_some_and(|d| d.is_dirty()) {
                        dirty = true;
                    }
                    if !dirty {
                        state.indexer.file_changed(p);
                    }
                }
            }
        }
    }

    fn tick_timers(&mut self, ctx: &egui::Context) {
        let st = &mut self.state;
        let now = Instant::now();
        let delay = Duration::from_millis(st.config.autosave_delay_ms as u64);
        let idx_delay = Duration::from_millis(300);
        let mut to_save = Vec::new();
        let mut to_index = Vec::new();
        let mut wait: Option<Duration> = None;
        let mut soonest = |d: Duration| wait = Some(wait.map_or(d, |w| w.min(d)));
        for d in st.docs.iter() {
            if !d.is_dirty() {
                continue;
            }
            let el = now.saturating_duration_since(d.last_edit);
            if st.config.autosave && !d.read_only && !d.autosave_blocked() {
                let backoff = Duration::from_secs(5);
                let since_fail = d.last_save_fail.map(|t| now.saturating_duration_since(t));
                if let Some(sf) = since_fail.filter(|sf| *sf < backoff) {
                    soonest(backoff - sf);
                } else if el >= delay {
                    to_save.push(d.path.clone())
                } else {
                    soonest(delay - el)
                }
            }
            if d.indexed_version != d.version
                && matches!(
                    d.kind,
                    iron_sheets_core::FileKind::Markdown | iron_sheets_core::FileKind::Typst | iron_sheets_core::FileKind::Excalidraw
                )
            {
                if el >= idx_delay { to_index.push(d.path.clone()) } else { soonest(idx_delay - el) }
            }
        }
        for p in to_index {
            if let Some(d) = st.docs.get_mut(&p) {
                d.indexed_version = d.version;
                st.indexer.update_from_text(&p, &d.text);
            }
        }
        for p in to_save {
            command::autosave_async(st, &p);
        }
        // Debounced session / config saves.
        let sig = st.compute_session_sig();
        if sig != st.session_sig {
            st.session_sig = sig;
            st.session_dirty_at = Some(now);
        }
        if let Some(t) = st.session_dirty_at {
            if now.saturating_duration_since(t) >= Duration::from_secs(1) {
                st.flush_session_async();
                st.flush_tree_cache_async();
            } else {
                soonest(Duration::from_secs(1));
            }
        }
        if let Some(t) = st.config_dirty_at {
            if now.saturating_duration_since(t) >= Duration::from_millis(500) { st.save_config_async() } else { soonest(Duration::from_millis(500)) }
        }
        if let Some(w) = wait {
            ctx.request_repaint_after(w + Duration::from_millis(5));
        }
    }

    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.frames += 1;
        {
            let st = &mut self.state;
            // The preview was drawn last frame and painted math, diagrams or images (textures keyed by scale).
            let preview_shown = st.ui.md_preview.frames_shown != self.md_shown;
            self.md_shown = st.ui.md_preview.frames_shown;
            st.ui.theme.zoom_instant = (preview_shown && st.ui.md_preview.textures_painted > 0)
                || st.tabs.active_tab().and_then(|t| st.docs.get(&t.path)).is_some_and(|d| d.text.len() > crate::theme::ZOOM_ANIM_MAX_BYTES);
            let eff = st.ui.eff.get(&st.config, st.settings.theme.as_ref(), (st.config_gen, st.settings_gen));
            st.ui.theme.refresh(&ctx, eff, st.settings_gen);
            // egui's wheel line speed is 40 points per notch; one global place for every scroll area.
            let speed = 40.0 * st.config.scroll_speed;
            if ctx.options(|o| o.input_options.line_scroll_speed) != speed {
                ctx.options_mut(|o| o.input_options.line_scroll_speed = speed);
            }
            for e in std::mem::take(&mut st.ui.theme.font_errors) {
                st.status.error(e);
            }
            if st.rename_restart {
                st.rename_restart = false;
                st.start_runtime();
            }
            st.poll_persist();
            iron_sheets_app_core::sidebar::moves::poll(st);
            command::poll_trash(st);
            command::poll_saves(st);
            let had_pending_tree = st.tree.has_pending_loads();
            if st.tree.poll_loads() && had_pending_tree && !st.tree.has_pending_loads() {
                st.flush_tree_cache_async();
            }
            while let Ok(evs) = st.fs_rx.try_recv() {
                Self::handle_fs(st, evs);
            }
            while let Ok(p) = st.dialogs.rx.try_recv() {
                st.dialogs.picking = false;
                match p {
                    Pick::Folder(f) => st.commands.push(Command::AddRoot(f)),
                    Pick::Files(v) => st.commands.extend(v.into_iter().map(|path| Command::OpenPath { path, new_tab: true, line: None })),
                    Pick::Unavailable => {
                        st.status.info("No native file dialog available; using the built-in folder browser");
                        st.commands.push(Command::AddRootByPath);
                    }
                    Pick::FilesUnavailable => {
                        st.dialogs.native_ok = false;
                        if !st.workspace.roots.is_empty() {
                            st.status.info("No native file dialog available; using Quick Open");
                            st.commands.push(Command::QuickOpen);
                        } else {
                            st.status.info("No native file dialog available; using the built-in folder browser");
                            st.commands.push(Command::AddRootByPath);
                        }
                    }
                    Pick::Cancelled => {}
                }
            }
            // Window events.
            let (close_req, focused, dropped, geom) = ctx.input(|i| {
                let v = i.viewport();
                // `inner_rect` is `None` on Wayland (no window positions): fall back to the viewport rect (the window size).
                let g = Some(v.inner_rect.unwrap_or_else(|| i.viewport_rect())).filter(|r| r.width() > 0.0).map(|r| WindowGeom {
                    width: r.width(),
                    height: r.height(),
                    maximized: v.maximized.unwrap_or(false),
                });
                (v.close_requested(), v.focused.unwrap_or(true), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>(), g)
            });
            if geom.is_some() && !geom.is_some_and(|g| g.maximized) {
                self.geom = geom;
            }
            if close_req && !st.quit_ok {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                st.commands.push(Command::Quit);
            }
            if !focused && st.had_focus && st.config.autosave {
                command::save_all_async(st);
            }
            if focused && !st.had_focus && st.modules.any() {
                st.modules.on_focus_gained();
            }
            st.had_focus = focused;
            for p in dropped {
                if p.is_dir() {
                    st.commands.push(Command::AddRoot(p));
                } else {
                    st.commands.push(Command::OpenPath { path: p, new_tab: true, line: None });
                }
            }
            command::collect_shortcuts(&ctx, st);
            // Mouse side buttons: back / forward through the tab's history (a no-op when there is none).
            let (back, fwd) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
            let modal = st.settings.open || st.palette.open || st.dialogs.current.is_some();
            if let Some(t) = st.tabs.active_tab().filter(|_| !modal) {
                if back && t.can_back() {
                    st.commands.push(Command::HistoryBack);
                } else if fwd && t.can_forward() {
                    st.commands.push(Command::HistoryForward);
                }
            }
            crate::preview::typst::poll(st, &ctx);
            if st.modules.any() {
                let doc = st.tabs.active_path().and_then(|p| st.docs.get(p)).map(|d| (d.path.as_path(), d.version));
                if st.modules.frame(doc) {
                    ctx.request_repaint();
                }
                #[cfg(feature = "git")]
                iron_sheets_app_core::git::report(st);
            }
            let steps = wheel_font_steps(&ctx, &mut self.zoom_acc, &mut self.zoom_last);
            if steps != 0 {
                st.commands.push(Command::FontZoom(steps));
            }
            // Anything that may change a font size (Ctrl+wheel, reset, a command from the palette) keeps the top line of each pane in place.
            let sizes0 = (st.config.editor_font_size, st.config.preview_font_size);
            let anchor = if st.commands.is_empty() { None } else { capture_anchor(st) };
            for _ in 0..8 {
                if st.commands.is_empty() {
                    break;
                }
                let mut platform = crate::platform_egui::EguiPlatform::new(&ctx, st.waker.clone());
                for c in std::mem::take(&mut st.commands) {
                    if matches!(c, Command::OpenSettings) && st.settings.open {
                        // Already open: raise and focus it instead of opening a second one.
                        st.settings.focus_request = true;
                    }
                    command::execute(st, &mut platform, c);
                }
            }
            if let Some(a) = anchor.filter(|_| sizes0 != (st.config.editor_font_size, st.config.preview_font_size)) {
                self.pending_anchor = Some((a, 4));
            }
            if st.ui.editor.cached_tabs() > st.tabs.tabs.len() {
                let ids: Vec<u64> = st.tabs.tabs.iter().map(|t| t.id).collect();
                st.ui.editor.prune(&ids);
            }
            // Non-blocking lazy load of the active tab's document.
            if iron_sheets_app_core::command::poll_open(st) {
                ctx.request_repaint();
            }
            // A link click waits for the index lock: retry at a modest rate, only while it is pending.
            if st.pending_link.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }
            // Once the first frames are out, the restored tabs that are not shown yet load in the background.
            if self.frames > 3 && iron_sheets_app_core::command::prefetch_restored_tabs(st) {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }
            let title = match st.tabs.active_path() {
                Some(p) if st.tabs.is_untitled(p) => {
                    format!("{} - {} - iron sheets", iron_sheets_app_core::command::untitled_title(p), st.workspace.name)
                }
                Some(p) => match p.file_name() {
                    Some(n) => format!("{} - {} - iron sheets", n.to_string_lossy(), st.workspace.name),
                    None => format!("{} - iron sheets", st.workspace.name),
                },
                None => format!("{} - iron sheets", st.workspace.name),
            };
            if title != self.last_title {
                crate::platform::Platform::set_title(&mut crate::platform_egui::EguiPlatform::new(&ctx, st.waker.clone()), &title);
                self.last_title = title;
            }
        }

        let st = &mut self.state;
        crate::preview::popup::PagePopup::take_escape(&ctx, st);
        egui::Panel::top("menu_bar").show(ui, |ui| menu::show(ui, st));
        if st.config.custom_title_bar {
            crate::titlebar::resize_zones(&ctx);
        }
        egui::Panel::bottom("status_bar").show(ui, |ui| statusbar::show(ui, st));
        // 0..=1: how far the sidebar has slid in. The stored width is never touched by it; the panel is pinned to a
        // fraction of that width and the views keep their full-width layout, shifted left by what is still outside.
        let slide = crate::anim::bool_t(ui, egui::Id::NULL.with("sidebar_slide_t"), st.config.sidebar_visible, crate::anim::Group::Slide);
        if slide > 0.0 {
            let w0 = st.config.sidebar_width;
            // The central area keeps a usable minimum however wide the sidebar was dragged or the window shrinks.
            let (min_w, max_w) = sidebar_width_range(ctx.content_rect().width());
            // egui remembers the panel's own (clamped) size, so outside a drag of its edge the width is pinned to the
            // stored one; the range is open only while the edge is dragged. The id is egui's resize-handle id.
            let handle = ui.scope_id().with("sidebar").with("__resize");
            let dragging = ui.read_response(handle).is_some_and(|r| r.dragged() || r.drag_stopped());
            let full = w0.clamp(min_w, max_w);
            let shift = full * (1.0 - slide);
            let panel = egui::Panel::left("sidebar").resizable(shift == 0.0);
            let panel = if dragging { panel.default_size(full).size_range(min_w..=max_w) } else { panel.exact_size(full - shift) };
            let r = panel.show(ui, |ui| sidebar::show(ui, st, shift));
            let w = r.response.rect.width();
            if dragging && (w - w0).abs() > 0.5 {
                st.config.sidebar_width = w;
                st.touch_config();
            }
        } else {
            // A folder closing while the sidebar went away must still end up closed.
            sidebar::files::finish_close(st);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            tabstrip::show_strip(ui, st);
            ui.separator();
            tabstrip::show_content(ui, st);
        });
        // Before the generic dialogs, so a confirm opened from a Git dialog (delete branch, remove remote) is on top of it.
        #[cfg(feature = "git")]
        {
            let mut gd = std::mem::take(&mut st.ui.git_dialogs);
            gd.show(&ctx, st);
            st.ui.git_dialogs = gd;
            crate::git_hunk::show(&ctx, st);
        }
        dialogs::show(&ctx, &mut st.dialogs, &mut st.commands);
        // A native Settings window is not created before the first frames are on screen.
        if self.frames > 3 || !crate::aux_window::is_native(&ctx) {
            let was_shown = st.settings.open;
            crate::settings::show(&ctx, st);
            if was_shown && !self.settings_milestone {
                self.settings_milestone = true;
                st.perf.milestone("settings window built");
            }
        }
        let mut pal = std::mem::take(&mut st.ui.palette);
        pal.show(&ctx, st);
        st.ui.palette = pal;
        crate::preview::popup::PagePopup::show(&ctx, st);
        // Professional mode hides the overlay (F12 state is kept).
        if st.perf.overlay && !st.config.professional {
            crate::perf::collect_inflight(st);
            crate::perf::show_overlay(&st.perf, &ctx);
        }

        self.tick_timers(&ctx);
        let st = &mut self.state;
        if let Some(t) = st.tabs.active_tab_mut().filter(|t| !t.is_empty()) {
            if let Some(d) = st.docs.get(&t.path) {
                let narrow = ctx.data(|d| d.get_temp::<bool>(crate::tabstrip::split_narrow_id())).unwrap_or(false);
                if t.view == iron_sheets_core::config::ViewMode::Split
                    && !narrow
                    && d.kind != iron_sheets_core::FileKind::Excalidraw
                {
                    crate::scrollsync::sync(t, st.config.sync_scroll);
                    if t.editor_scroll.force.is_some() || t.preview_scroll.force.is_some() {
                        ctx.request_repaint();
                    }
                }
            }
        }
        // After a font size change: put the line that was at the top of each pane back at the top (new heights are known now).
        if let Some((a, left)) = self.pending_anchor.take() {
            if let Some(t) = st.tabs.active_tab_mut().filter(|t| t.id == a.tab && !t.editor_scroll.user_scrolled && !t.preview_scroll.user_scrolled) {
                for (line, p) in [(a.editor, &mut t.editor_scroll), (a.preview, &mut t.preview_scroll)] {
                    if let Some(l) = line.filter(|_| !p.map.is_empty()) {
                        p.force = Some(p.map.y_for_line(l).min(p.max_offset()));
                    }
                }
                ctx.request_repaint();
                if left > 1 {
                    self.pending_anchor = Some((a, left - 1));
                }
            }
        }
        // Layout met a script no system font covers (see `sysfont`): say so once, then keep the tofu.
        let missing = std::mem::take(&mut *self.font_missing.lock());
        for script in &missing {
            st.status.info(format!("No system font for {script} installed"));
        }
        // Commands queued during the UI pass (clicks) run in the next frame.
        if !st.commands.is_empty() || !missing.is_empty() {
            ctx.request_repaint();
        }
        if st.tree.has_pending_loads() && crate::anim::enabled(crate::anim::Group::Tree) && ctx.global_style().animation_time > 0.0 {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.finish_frame(&ctx, t0);
    }

    fn finish_frame(&mut self, ctx: &egui::Context, t0: Instant) {
        let n = self.frames;
        if n == 1 {
            let st = &mut self.state;
            st.modules.go_live(&mut iron_sheets_app_core::modules::ModuleCx { waker: &st.waker, commands: &mut st.commands });
        }
        if n <= 3 {
            self.state.perf.milestone(match n {
                1 => "frame 1",
                2 => "frame 2",
                _ => "frame 3",
            });
            if n < 3 || (n == 3 && self.state.settings.open) {
                ctx.request_repaint();
            }
        }
        if self.bench_full && n >= 3 && !self.bench_closed {
            self.bench_usable(ctx);
        } else if self.bench && n >= 3 && !self.bench_closed && (!self.state.settings.open || self.settings_milestone) {
            self.bench_closed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.state.quit_ok = true;
        }
        if let Some(path) = self.screenshot.clone() {
            if n >= 5 && !self.shot_requested {
                self.shot_requested = true;
                let done = self.shot_done.clone();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    match image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                        Some(i) => match i.save(&path) {
                            Ok(()) => eprintln!("screenshot saved: {}", path.display()),
                            Err(e) => eprintln!("screenshot failed: {e}"),
                        },
                        None => eprintln!("screenshot: bad buffer"),
                    }
                    done.store(true, Ordering::SeqCst);
                });
            }
            if n < 5 || (self.shot_requested && !self.shot_done.load(Ordering::SeqCst)) {
                ctx.request_repaint();
            }
            if self.shot_done.load(Ordering::SeqCst) {
                self.state.quit_ok = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.state.perf.frame_done(t0.elapsed());
    }

    /// `--bench-startup=full`: print when index, tree and watcher become usable, then quit (polled once per frame, so the
    /// resolution is one frame). Waits for the index cache to be written too, so a warm run can follow.
    fn bench_usable(&mut self, ctx: &egui::Context) {
        let st = &mut self.state;
        let scanning = st.indexer.status().scanning;
        let queryable = st.indexer.index().try_read().is_some_and(|g| !g.is_empty());
        let conds = [
            queryable || !scanning,
            !scanning,
            !st.tree.has_pending_loads(),
            st.watcher.as_ref().is_none_or(|w| w.registered()),
            !scanning && st.indexer.wait_idle(Duration::from_millis(1)),
        ];
        for (i, name) in ["index queryable", "index done", "tree loaded", "watch registered", "index cache written"].into_iter().enumerate() {
            if conds[i] && !self.bench_marks[i] {
                self.bench_marks[i] = true;
                st.perf.milestone(name);
            }
        }
        if self.bench_marks.iter().all(|m| *m) || st.perf.start.elapsed() > Duration::from_secs(120) {
            self.bench_closed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            st.quit_ok = true;
        } else {
            ctx.request_repaint();
        }
    }

    /// Save everything that must survive a quit.
    pub fn on_quit(&mut self) {
        let st = &mut self.state;
        if st.config.autosave {
            command::save_all(st);
        }
        st.flush_session();
        st.flush_tree_cache();
        if let Some(g) = self.geom {
            st.config.window = Some(g);
        }
        st.save_config_now();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.ime_filter.filter(&mut raw_input.events);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_quit();
    }
}

/// Command-line derived options of a run.
pub struct RunArgs {
    pub opts: AppOptions,
    /// Print startup milestones and exit after frame 3.
    pub bench: bool,
    /// With `bench`: wait until index, tree and watcher are done before exiting.
    pub bench_full: bool,
    /// Save a PNG after a few frames and exit.
    pub screenshot: Option<PathBuf>,
    /// Open the Settings window at start (`--settings`).
    pub open_settings: bool,
}

pub type RunResult = eframe::Result;

/// On a single-GPU NVIDIA Linux box, tell GLVND to load only the NVIDIA EGL vendor: its default probe also loads Mesa's
/// libgallium + LLVM (~10 ms) for nothing. Skipped when the user set the variables, with several GPUs (hybrid laptops), or
/// without NVIDIA's own vendor file; `IRON_NO_NVIDIA_EGL=1` turns it off. Must run before any thread exists (`set_var`).
#[cfg(target_os = "linux")]
fn prefer_nvidia_egl() {
    use std::{env, fs, path::Path};
    if env::var_os("IRON_NO_NVIDIA_EGL").is_some() || env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_some() || env::var_os("__EGL_VENDOR_LIBRARY_DIRS").is_some() || !Path::new("/proc/driver/nvidia/version").exists() {
        return;
    }
    let render_nodes = fs::read_dir("/dev/dri").map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("renderD")).count());
    if render_nodes != 1 {
        return;
    }
    for dir in ["/etc/glvnd/egl_vendor.d", "/usr/share/glvnd/egl_vendor.d"] {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            if fs::read_to_string(e.path()).is_ok_and(|s| s.contains("libEGL_nvidia")) {
                // SAFETY: called first thing in `run`, before this process starts any thread.
                unsafe { env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", e.path()) };
                return;
            }
        }
    }
}

/// Opens the window and runs the application until it quits.
pub fn run(args: RunArgs) -> RunResult {
    #[cfg(target_os = "linux")]
    prefer_nvidia_egl();
    let RunArgs { opts, bench, bench_full, screenshot, open_settings } = args;
    let geom = opts.config.window.unwrap_or_default();
    let viewport =
        egui::ViewportBuilder::default()
            .with_title("iron sheets")
            .with_app_id("iron-sheets")
            .with_inner_size([geom.width, geom.height])
            .with_maximized(geom.maximized)
            .with_decorations(!opts.config.custom_title_bar);
    let options = eframe::NativeOptions { viewport, system_font_fallback: false, ..Default::default() };
    if bench {
        println!("MILESTONE before run_native {:.1}", opts.start.elapsed().as_secs_f32() * 1000.0);
    }
    eframe::run_native(
        "iron-sheets",
        options,
        Box::new(move |cc| {
            let mut app = App::new(&cc.egui_ctx, opts);
            app.bench = bench;
            app.bench_full = bench_full;
            app.screenshot = screenshot;
            app.state.settings.open |= open_settings;
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod atlas_tests {
    #[test]
    fn glyph_atlas_is_capped() {
        let ctx = egui::Context::default();
        ctx.add_plugin(super::AtlasSide);
        let input = egui::RawInput { max_texture_side: Some(16384), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            ui.label("x");
        });
        out.textures_delta.clear();
        assert_eq!(ctx.fonts(|f| f.font_image_size())[0], super::ATLAS_SIDE);
        assert_eq!(ctx.input(|i| i.max_texture_side), super::ATLAS_SIDE);
    }
}
