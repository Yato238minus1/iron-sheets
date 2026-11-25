//! F12 overlay of the perf counters (the counters themselves are in `iron_sheets_app_core::perf`).
pub use iron_sheets_app_core::perf::*;

pub fn show_overlay(perf: &Perf, ctx: &egui::Context) {
    if !perf.overlay {
        return;
    }
    // While shown, one frame a second keeps "fps" honest (it falls to 0 when idle instead of freezing at the last value).
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    egui::Area::new(egui::Id::NULL.with("perf_overlay"))
        .anchor(egui::Align2::RIGHT_BOTTOM, [-8.0, -28.0])
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.monospace(format!("fps: {} (last second)", perf.fps()));
                ui.monospace(format!("frame cpu: last {:.2} ms, avg {:.2} ms", perf.last_frame_ms, perf.avg_frame_ms));
                let busy: Vec<String> = perf.inflight.iter().filter(|(_, n)| *n > 0).map(|(w, n)| format!("{w} {n}")).collect();
                ui.monospace(format!("in flight: {}", if busy.is_empty() { "none".to_string() } else { busy.join(", ") }));
                for (name, t) in &perf.timers {
                    ui.monospace(format!("{name}: last {:.2} avg {:.2} max {:.2} ms (n={})", t.last_ms, t.avg_ms, t.max_ms, t.count));
                }
            });
        });
}

/// Refreshes `perf.inflight` from what the state can tell cheaply; called while the overlay is on only.
pub fn collect_inflight(st: &mut crate::state::AppState) {
    let saves = st.tabs.tabs.iter().filter(|t| st.docs.is_saving(&t.path)).count();
    let search = usize::from(!st.ui.search.query.is_empty() && !st.ui.search.status_finished());
    let loads = usize::from(st.is_active_tab_loading()) + usize::from(!st.docs.prefetch.is_idle());
    #[cfg(feature = "typst")]
    let typst = usize::from(st.typst.warming().is_some());
    #[cfg(feature = "typst")]
    let compiles = st.typst.compiles_pending();
    #[cfg(not(feature = "typst"))]
    let (typst, compiles) = (0, 0);
    let syntax = usize::from(crate::preview::markdown::codehl::SyntectHighlighter::shared(&st.waker).is_loading());
    #[cfg(feature = "excalidraw")]
    let drawings = st.ui.excalidraw_preview.in_flight();
    #[cfg(not(feature = "excalidraw"))]
    let drawings = 0;
    let decodes = st.ui.image_preview.in_flight() + drawings + st.ui.md_preview.in_flight_decodes();
    let md_parses = st.ui.md_preview.in_flight_parses();
    let git_scans = st.modules.in_flight();
    st.perf.inflight = vec![
        ("open/prefetch loads", loads),
        ("saves", saves),
        ("index scans", usize::from(st.indexer.status().scanning)),
        ("search", search),
        ("link opens", usize::from(st.pending_link.is_some())),
        ("typst warm-up", typst),
        ("typst compiles", compiles),
        ("syntax load", syntax),
        ("image/mermaid decodes", decodes),
        ("md parses", md_parses),
        ("git scans", git_scans),
    ];
}
