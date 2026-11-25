//! egui wrapper that lets a `ScrollArea` take part in scroll sync.
use crate::scrollsync::PaneScroll;

/// Show `add_contents` in a vertical `ScrollArea` that honours `scroll.force` and fills `scroll`.
/// Preview panes should use this so they take part in scroll sync.
pub fn scroll_pane<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    scroll: &mut PaneScroll,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut area = egui::ScrollArea::vertical().id_salt(id_salt).auto_shrink(false);
    let forced = scroll.force.take();
    if let Some(f) = forced {
        area = area.vertical_scroll_offset(f.max(0.0));
    }
    let out = area.show(ui, add_contents);
    let off = out.state.clamped_offset().y;
    scroll.user_scrolled = forced.is_none() && (off - scroll.last_offset).abs() > 0.01;
    scroll.last_offset = off;
    scroll.offset = off;
    scroll.viewport_h = out.inner_rect.height();
    scroll.content_h = out.content_size.y;
    out.inner
}
