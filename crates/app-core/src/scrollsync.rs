//! Scroll coupling between the editor pane and the preview pane of a tab.
use crate::tabs::Tab;
pub use iron_sheets_core::scrollsync::ScrollMap;

/// Scroll state of one pane. A pane fills only its own `PaneScroll`; [`sync`] couples the two.
#[derive(Debug, Clone, Default)]
pub struct PaneScroll {
    pub offset: f32,
    pub viewport_h: f32,
    pub content_h: f32,
    /// Offset to apply next frame (set by `sync`, consumed and cleared by the pane).
    pub force: Option<f32>,
    /// The user (not `force`) changed `offset` this frame.
    pub user_scrolled: bool,
    /// Source line -> y in this pane.
    pub map: ScrollMap,
    /// Offset seen last frame (pane bookkeeping).
    pub last_offset: f32,
    /// Where `sync` last put this pane, in the other pane's terms. The pane re-derives its offset from it whenever
    /// its own map changes (block heights settle while scrolling), so it stays in sync instead of drifting.
    /// Cleared when the user scrolls this pane.
    pub follow: Option<Follow>,
}

/// A position in a pane that is independent of that pane's block heights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Follow {
    Top,
    Bottom,
    /// A (fractional) source line.
    Line(f32),
    /// Fraction of the scroll range (no maps available).
    Fraction(f32),
}

impl PaneScroll {
    pub fn max_offset(&self) -> f32 {
        (self.content_h - self.viewport_h).max(0.0)
    }

    /// Offset for `follow` given this pane's current map and scroll range.
    pub fn follow_offset(&self, follow: Follow, max_offset: f32) -> f32 {
        let y = match follow {
            Follow::Top => 0.0,
            Follow::Bottom => max_offset,
            Follow::Line(l) => self.map.y_for_line(l),
            Follow::Fraction(f) => f * max_offset,
        };
        y.clamp(0.0, max_offset)
    }
}

const EDGE: f32 = 0.5;

fn drive(a: &PaneScroll, b: &mut PaneScroll) {
    let (max_a, max_b) = (a.max_offset(), b.max_offset());
    let follow = if a.offset <= EDGE {
        Follow::Top
    } else if max_a > 0.0 && a.offset >= max_a - EDGE {
        Follow::Bottom
    } else if a.map.is_empty() || b.map.is_empty() {
        Follow::Fraction(if max_a > 0.0 { a.offset / max_a } else { 0.0 })
    } else {
        Follow::Line(a.map.line_for_y(a.offset))
    };
    b.follow = Some(follow);
    b.force = Some(b.follow_offset(follow, max_b));
}

/// Whichever pane the user scrolled this frame drives the other.
pub fn sync(tab: &mut Tab, enabled: bool) {
    // Just switched on: the editor leads once, unless the user scrolled a pane this very frame (then that wins).
    // `follow` keeps the preview on that line while its block heights settle; a later user scroll clears it.
    let resync = std::mem::take(&mut tab.resync) && enabled;
    let (e, p) = (&mut tab.editor_scroll, &mut tab.preview_scroll);
    if resync && !e.user_scrolled && !p.user_scrolled {
        drive(e, p);
    }
    if e.user_scrolled {
        e.follow = None;
    }
    if p.user_scrolled {
        p.follow = None;
    }
    if enabled {
        if p.user_scrolled && !e.user_scrolled {
            drive(p, e);
        } else if e.user_scrolled {
            drive(e, p);
        }
    }
    e.user_scrolled = false;
    p.user_scrolled = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(content: f32, viewport: f32, pts: Vec<(f32, f32)>) -> PaneScroll {
        PaneScroll { content_h: content, viewport_h: viewport, map: ScrollMap::from_points(pts), ..Default::default() }
    }

    fn tab() -> Tab {
        let mut t = Tab::new(1, "a.md".into());
        // Editor: 20 px per line, 100 lines. Preview: 30 px per line.
        t.editor_scroll = pane(2000.0, 400.0, (0..100).map(|l| (l as f32, l as f32 * 20.0)).collect());
        t.preview_scroll = pane(3300.0, 600.0, (0..100).map(|l| (l as f32, l as f32 * 30.0)).collect());
        t
    }

    #[test]
    fn enabling_sync_moves_the_preview_to_the_editor_once() {
        let mut t = tab();
        t.editor_scroll.offset = 400.0;
        t.resync = true;
        sync(&mut t, true);
        assert!((t.preview_scroll.force.unwrap() - 600.0).abs() < 0.01, "preview follows the editor's line 20");
        assert_eq!(t.preview_scroll.follow, Some(Follow::Line(20.0)), "and keeps following while its layout settles");
        assert!(!t.resync, "only once");
        t.preview_scroll.force = None;
        sync(&mut t, true);
        assert!(t.preview_scroll.force.is_none());
    }

    #[test]
    fn scrolling_the_preview_while_sync_is_enabled_wins() {
        let mut t = tab();
        t.editor_scroll.offset = 400.0;
        t.preview_scroll.offset = 90.0;
        t.preview_scroll.user_scrolled = true;
        t.resync = true;
        sync(&mut t, true);
        // The user's preview scroll drives the editor instead; the one-time jump is dropped.
        assert!(t.preview_scroll.force.is_none() && t.editor_scroll.force.is_some());
        assert!(!t.resync);
    }

    #[test]
    fn editor_drives_preview() {
        let mut t = tab();
        t.editor_scroll.offset = 400.0;
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert!((t.preview_scroll.force.unwrap() - 600.0).abs() < 0.01);
        assert!(t.editor_scroll.force.is_none());
        assert!(!t.editor_scroll.user_scrolled);
    }

    #[test]
    fn preview_drives_editor() {
        let mut t = tab();
        t.preview_scroll.offset = 900.0;
        t.preview_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert!((t.editor_scroll.force.unwrap() - 600.0).abs() < 0.01);
        assert!(t.preview_scroll.force.is_none());
    }

    #[test]
    fn snaps_top_and_bottom() {
        let mut t = tab();
        t.editor_scroll.offset = 0.0;
        t.editor_scroll.user_scrolled = true;
        t.preview_scroll.offset = 700.0;
        sync(&mut t, true);
        assert_eq!(t.preview_scroll.force, Some(0.0));
        let mut t = tab();
        t.editor_scroll.offset = 1600.0; // max
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert_eq!(t.preview_scroll.force, Some(2700.0));
    }

    #[test]
    fn no_feedback_and_disabled() {
        let mut t = tab();
        // A pane applying `force` does not report user_scrolled, so nothing is driven.
        t.editor_scroll.offset = 400.0;
        t.preview_scroll.offset = 600.0;
        sync(&mut t, true);
        assert!(t.editor_scroll.force.is_none() && t.preview_scroll.force.is_none());
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, false);
        assert!(t.preview_scroll.force.is_none());
        assert!(!t.editor_scroll.user_scrolled);
    }

    #[test]
    fn proportional_fallback_without_map() {
        let mut t = tab();
        t.preview_scroll.map = ScrollMap::default();
        t.editor_scroll.offset = 800.0; // half of 1600
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert!((t.preview_scroll.force.unwrap() - 1350.0).abs() < 0.01);
    }

    #[test]
    fn follower_remembers_its_target_and_survives_a_map_change() {
        let mut t = tab();
        t.editor_scroll.offset = 400.0; // line 20
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, true);
        let f = t.preview_scroll.follow.expect("follow set");
        assert!(matches!(f, Follow::Line(l) if (l - 20.0).abs() < 0.01));
        // The preview's blocks turn out taller: same line, new offset.
        t.preview_scroll.map = ScrollMap::from_points((0..100).map(|l| (l as f32, l as f32 * 40.0)).collect());
        t.preview_scroll.content_h = 4400.0;
        let max = t.preview_scroll.max_offset();
        assert!((t.preview_scroll.follow_offset(f, max) - 800.0).abs() < 0.01);
        // A user scroll of the follower drops the target.
        t.preview_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert!(t.preview_scroll.follow.is_none());
    }

    #[test]
    fn edges_follow_the_scroll_range() {
        let mut t = tab();
        t.editor_scroll.offset = 1600.0;
        t.editor_scroll.user_scrolled = true;
        sync(&mut t, true);
        assert_eq!(t.preview_scroll.follow, Some(Follow::Bottom));
        assert_eq!(t.preview_scroll.follow_offset(Follow::Bottom, 3000.0), 3000.0);
    }

    #[test]
    fn follower_is_monotonic_and_exact_over_a_scale_change() {
        // Editor: 20 px per line. Preview: 10 px per line, except a one-line block 200 px high at line 50.
        let mut t = Tab::new(1, "a.md".into());
        t.editor_scroll = pane(2000.0, 400.0, (0..100).map(|l| (l as f32, l as f32 * 20.0)).collect());
        let mut pts = Vec::new();
        for l in 0..100 {
            let y = if l <= 50 { l as f32 * 10.0 } else { 500.0 + 190.0 + (l - 51) as f32 * 10.0 };
            pts.push((l as f32, y));
        }
        t.preview_scroll = pane(1700.0, 400.0, pts);
        let mut prev = 0.0f32;
        for k in 0..160 {
            t.editor_scroll.offset = 700.0 + k as f32 * 2.0; // 2 px per frame around line 35..48
            t.editor_scroll.user_scrolled = true;
            sync(&mut t, true);
            let y = t.preview_scroll.force.expect("force");
            // Exactly the map's value (no one-sided rounding, no estimate drift) and never backwards.
            let want = t.preview_scroll.map.y_for_line(t.editor_scroll.map.line_for_y(t.editor_scroll.offset));
            assert!((y - want).abs() < 1e-3);
            assert!(y >= prev, "monotonic");
            prev = y;
        }
    }
}
