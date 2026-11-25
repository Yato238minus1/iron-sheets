//! IS-30: a persistent status bar notice when the OS file-watch limit is hit, with a Rescan action.
use crate::common::*;
use egui_kittest::kittest::Queryable;

#[test]
fn watch_limit_notice_is_persistent_and_offers_rescan() {
    let fx = Fx::new();
    let mut h = harness(fx.opts(vec![fx.notes()]));
    settle(&mut h);
    assert!(h.query_by_label("File watching limit reached").is_none(), "no notice while watching works");

    h.state_mut().app().state.watch_limit_forced = true;
    settle(&mut h);
    // Persistent: still there after many frames and simulated seconds.
    for _ in 0..40 {
        h.step();
    }
    assert!(h.query_by_label("File watching limit reached").is_some());
    shot(&mut h, "watch-limit-notice");

    h.get_by_label("Rescan").click();
    h.step();
    h.step();
    let st = &h.state_mut().app().state;
    assert!(st.status.current().is_some_and(|m| m.text.contains("Rescanning")), "Rescan command ran");
}
