//! Line-number and git-diff gutter: rendered for the rows in view only; wrapped continuation rows share the line's diff bar.
use egui::{Align2, Color32, FontId, Galley, Pos2, Ui};
#[cfg(feature = "git")]
use egui::{Rect, vec2};
#[cfg(feature = "git")]
pub use iron_sheets_app_core::git::{DiffKind, DiffMark};
/// Without the `git` feature there are no diff marks.
#[cfg(not(feature = "git"))]
#[derive(Clone, Copy)]
pub enum DiffMark {}

/// Width reserved for git diff markers when a file has changes against `HEAD`.
pub const DIFF_W: f32 = 8.0;

/// Width for numbers up to `lines`: the digits plus a small margin on both sides.
pub fn width(ui: &Ui, font: &FontId, lines: usize) -> f32 {
    let digits = lines.max(1).ilog10() as usize + 1;
    ui.fonts_mut(|f| f.glyph_width(font, '0')) * digits.max(2) as f32 + 12.0
}

/// Per galley: how many rows above each row end in a newline, so a scroll frame does not count the rows above the view.
#[derive(Clone, Default)]
pub struct Numbers {
    /// (galley address, row count, doc version) the table was built for.
    key: (usize, usize, u64),
    /// `before[i]` = newline-ended rows among `rows[..i]`.
    before: Vec<u32>,
    /// Tables built (tests).
    builds: usize,
}

impl Numbers {
    /// Newline-ended rows among `galley.rows[..first]`; the table is rebuilt (one pass) only for a new galley or version.
    fn lines_before(&mut self, galley: &Galley, version: u64, first: usize) -> usize {
        let rows = &galley.rows;
        let key = (galley as *const Galley as usize, rows.len(), version);
        if self.key != key {
            self.key = key;
            self.builds += 1;
            let mut n = 0u32;
            self.before.clear();
            self.before.extend(rows.iter().map(|r| {
                let b = n;
                n += u32::from(r.ends_with_newline);
                b
            }));
        }
        self.before.get(first).map_or(0, |&b| b as usize)
    }
}

/// Paints line numbers (right aligned at `right`) and/or git diff marks (at `diff_x`) for visible rows. Returns the mark
/// the pointer clicked (the bar of a change, or the tick of a deletion): the frontend opens its popup.
#[cfg_attr(not(feature = "git"), allow(unused_variables))]
pub fn paint(
    ui: &Ui,
    galley: &Galley,
    galley_pos: Pos2,
    numbers: Option<(f32, &FontId, Color32)>,
    diff: Option<(f32, &[DiffMark])>,
    version: u64,
    cache: &mut Numbers,
) -> Option<DiffMark> {
    #[cfg_attr(not(feature = "git"), allow(unused_mut))]
    let mut clicked = None;
    let clip = ui.clip_rect();
    let (top, bottom) = (clip.top() - galley_pos.y, clip.bottom() - galley_pos.y);
    let rows = &galley.rows;
    let first = rows.partition_point(|r| r.max_y() < top);
    let mut at_start = first == 0 || rows[first - 1].ends_with_newline;
    let mut line = cache.lines_before(galley, version, first) + usize::from(!at_start);
    let painter = ui.painter_at(clip);
    for (offset, r) in rows[first..].iter().enumerate() {
        if r.min_y() > bottom {
            break;
        }
        if at_start {
            line += 1;
            if let Some((right, font, color)) = numbers {
                painter.text(Pos2::new(right, galley_pos.y + r.pos.y), Align2::RIGHT_TOP, line, font.clone(), color);
            }
        }
        #[cfg(feature = "git")]
        if let Some((diff_x, marks)) = diff {
            let added_col = if ui.visuals().dark_mode { Color32::from_rgb(90, 185, 115) } else { Color32::from_rgb(35, 138, 70) };
            let (modified_col, deleted_col) = (ui.visuals().warn_fg_color, ui.visuals().error_fg_color);
            let last_row_idx = rows.len().saturating_sub(1);
            let line0 = (line.saturating_sub(1)) as u32;
            let idx = marks.partition_point(|m| m.end <= line0);
            if let Some(m) = marks.get(idx).filter(|m| m.start <= line0 && line0 < m.end) {
                let col = match m.kind {
                    DiffKind::Added => added_col,
                    DiffKind::Modified => modified_col,
                    DiffKind::Deleted => deleted_col,
                };
                let bar = Rect::from_min_max(
                    Pos2::new(diff_x, galley_pos.y + r.min_y()),
                    Pos2::new(diff_x + 3.0, galley_pos.y + r.max_y()),
                );
                painter.rect_filled(bar, 0.5, col);
                clicked = clicked.or(hit(ui, bar, *m));
            }
            if line0 == 0 && at_start && marks.first().is_some_and(|m| m.start == 0 && m.kind == DiffKind::Deleted) {
                let tick = Rect::from_min_size(Pos2::new(diff_x - 0.5, galley_pos.y + r.min_y()), vec2(4.5, 2.0));
                painter.rect_filled(tick, 0.5, deleted_col);
                clicked = clicked.or(hit(ui, tick, marks[0]));
            }
            let is_last_visual = r.ends_with_newline || first + offset == last_row_idx;
            if is_last_visual {
                let target = line0 + 1;
                let j = marks.partition_point(|m| m.start < target);
                if marks.get(j).is_some_and(|m| m.start == target && m.kind == DiffKind::Deleted) {
                    let inset = if first + offset == last_row_idx && !r.ends_with_newline { 2.0 } else { 1.0 };
                    let y = (galley_pos.y + r.max_y() - inset).max(galley_pos.y + r.min_y());
                    let tick = Rect::from_min_size(Pos2::new(diff_x - 0.5, y), vec2(4.5, 2.0));
                    painter.rect_filled(tick, 0.5, deleted_col);
                    clicked = clicked.or(hit(ui, tick, marks[j]));
                }
            }
        }
        at_start = r.ends_with_newline;
    }
    clicked
}

/// The pointer clicked `bar` (a few pixels around it count): the hand cursor shows over it, the mark is what a click returns.
#[cfg(feature = "git")]
fn hit(ui: &Ui, bar: Rect, mark: DiffMark) -> Option<DiffMark> {
    if !ui.rect_contains_pointer(bar.expand2(vec2(3.0, 2.0))) {
        return None;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    ui.input(|i| i.pointer.primary_clicked()).then_some(mark)
}

#[cfg(all(test, feature = "git"))]
mod tests {
    use super::*;

    fn collect_rects(shapes: &[egui::epaint::ClippedShape], out: &mut Vec<(Rect, Color32)>) {
        fn walk(s: &egui::Shape, out: &mut Vec<(Rect, Color32)>) {
            match s {
                egui::Shape::Rect(r) => out.push((r.rect, r.fill)),
                egui::Shape::Vec(v) => {
                    for child in v {
                        walk(child, out);
                    }
                }
                _ => {}
            }
        }
        for cs in shapes {
            walk(&cs.shape, out);
        }
    }

    fn run_paint(text: &str, max_width: f32, galley_pos: Pos2, clip: Rect, marks: &[DiffMark]) -> (std::sync::Arc<Galley>, Vec<(Rect, Color32)>, Color32, Color32) {
        let ctx = egui::Context::default();
        let mut galley_out = None;
        let mut deleted_col = Color32::TRANSPARENT;
        let mut mod_col = Color32::TRANSPARENT;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let job = egui::text::LayoutJob::simple(text.to_string(), FontId::monospace(13.0), Color32::WHITE, max_width);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            ui.set_clip_rect(clip);
            deleted_col = ui.visuals().error_fg_color;
            mod_col = ui.visuals().warn_fg_color;
            let mut n = Numbers::default();
            paint(ui, &galley, galley_pos, None, Some((10.0, marks)), 1, &mut n);
            galley_out = Some(galley);
        });
        out.textures_delta.clear();
        let mut rects = Vec::new();
        collect_rects(&out.shapes, &mut rects);
        (galley_out.unwrap(), rects, deleted_col, mod_col)
    }

    #[test]
    fn scrolling_does_not_rebuild_the_row_table() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let text: String = (0..5000).map(|i| format!("line {i}\n")).collect();
            let job = egui::text::LayoutJob::simple(text, FontId::monospace(13.0), Color32::WHITE, f32::INFINITY);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let mut n = Numbers::default();
            assert_eq!(n.lines_before(&galley, 1, 3000), 3000);
            assert_eq!(n.lines_before(&galley, 1, 10), 10);
            assert_eq!(n.builds, 1, "same galley and version: no new pass over the rows");
            n.lines_before(&galley, 2, 10);
            assert_eq!(n.builds, 2, "an edit rebuilds");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn soft_wrap_and_scrolled_mid_wrap_bars_and_single_deleted_tick() {
        let text = "word1 word2 word3 word4 word5 word6\nsecond line";
        let marks = [
            DiffMark { start: 0, end: 1, old_start: 0, old_end: 0, kind: DiffKind::Modified },
            DiffMark { start: 1, end: 1, old_start: 0, old_end: 0, kind: DiffKind::Deleted },
        ];
        // First measure full galley with narrow width so line 0 wraps into >= 3 visual rows.
        let full_clip = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let (galley, rects, deleted_col, mod_col) = run_paint(text, 75.0, Pos2::ZERO, full_clip, &marks);
        let line0_rows: Vec<_> = galley.rows.iter().take_while(|r| !r.ends_with_newline).collect();
        let line0_count = line0_rows.len() + 1;
        assert!(line0_count >= 3, "expected >= 3 visual rows for wrapped line 0, got {line0_count}");

        let bars: Vec<_> = rects.iter().filter(|(r, c)| *c == mod_col && (r.width() - 3.0).abs() < 0.01).collect();
        let ticks: Vec<_> = rects.iter().filter(|(r, c)| *c == deleted_col && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(bars.len(), line0_count, "each visual row of line 0 gets a bar");
        for (i, (r, _)) in bars.iter().enumerate() {
            assert!((r.min.y - galley.rows[i].min_y()).abs() < 0.01);
            assert!((r.max.y - galley.rows[i].max_y()).abs() < 0.01);
        }
        assert_eq!(ticks.len(), 1, "exactly one Deleted tick on final visual row of wrapped line 0");
        let last_row0 = &galley.rows[line0_count - 1];
        assert!((ticks[0].0.min.y - (last_row0.max_y() - 1.0)).abs() < 0.01);

        // Scrolled past row 0 (mid-wrap): continuation rows still get bars and row (line0_count - 1) still gets the Deleted tick.
        let scrolled_clip = Rect::from_min_max(Pos2::new(0.0, galley.rows[0].max_y() + 0.5), Pos2::new(400.0, 400.0));
        let (_, scrolled_rects, _, _) = run_paint(text, 75.0, Pos2::ZERO, scrolled_clip, &marks);
        let scrolled_bars: Vec<_> = scrolled_rects.iter().filter(|(r, c)| *c == mod_col && (r.width() - 3.0).abs() < 0.01).collect();
        let scrolled_ticks: Vec<_> = scrolled_rects.iter().filter(|(r, c)| *c == deleted_col && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(scrolled_bars.len(), line0_count - 1);
        assert_eq!(scrolled_ticks.len(), 1);
        assert!((scrolled_ticks[0].0.min.y - (last_row0.max_y() - 1.0)).abs() < 0.01);
    }

    #[test]
    fn line_zero_deletion_unscrolled_mid_wrap_and_empty_buffer() {
        let marks = [DiffMark { start: 0, end: 0, old_start: 0, old_end: 0, kind: DiffKind::Deleted }];
        let text = "word1 word2 word3 word4 word5 word6\nsecond";
        let clip = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));

        // Unscrolled: tick at top edge inside clip, height 2.0.
        let (galley, rects, deleted_col, _) = run_paint(text, 75.0, Pos2::ZERO, clip, &marks);
        let ticks: Vec<_> = rects.iter().filter(|(r, c)| *c == deleted_col && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(ticks.len(), 1);
        assert!(ticks[0].0.min.y >= clip.top());
        assert!((ticks[0].0.height() - 2.0).abs() < 0.01);

        // Wrapped line 0 scrolled past row 0: no line-0 tick emitted on continuation rows.
        let scrolled_clip = Rect::from_min_max(Pos2::new(0.0, galley.rows[0].max_y() + 0.5), Pos2::new(400.0, 400.0));
        let (_, scrolled_rects, _, _) = run_paint(text, 75.0, Pos2::ZERO, scrolled_clip, &marks);
        let scrolled_ticks: Vec<_> = scrolled_rects.iter().filter(|(r, c)| *c == deleted_col && (r.height() - 2.0).abs() < 0.01).collect();
        assert!(scrolled_ticks.is_empty(), "continuation rows must not repeat line-0 Deleted tick");

        // Empty buffer `""` with Deleted { start: 0, end: 0 }: 1 tick at rows[0].min_y().
        let (empty_galley, empty_rects, deleted_col_empty, _) = run_paint("", f32::INFINITY, Pos2::ZERO, clip, &marks);
        let empty_ticks: Vec<_> = empty_rects.iter().filter(|(r, c)| *c == deleted_col_empty && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(empty_ticks.len(), 1);
        assert!((empty_ticks[0].0.min.y - empty_galley.rows[0].min_y()).abs() < 0.01);
    }

    #[test]
    fn eof_deletion_without_and_with_trailing_newline() {
        // Without trailing `\n` (`"a\nb\nc"` -> `"a\nb"`): Deleted { start: 2, end: 2 } scrolled to bottom.
        let no_nl_text = "a\nb";
        let marks = [DiffMark { start: 2, end: 2, old_start: 0, old_end: 0, kind: DiffKind::Deleted }];
        let probe_clip = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0));
        let (galley_no_nl, _, _, _) = run_paint(no_nl_text, f32::INFINITY, Pos2::ZERO, probe_clip, &marks);
        let bottom_clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(400.0, galley_no_nl.rect.height()));
        let (_, rects_no_nl, deleted_col, _) = run_paint(no_nl_text, f32::INFINITY, Pos2::ZERO, bottom_clip, &marks);
        let ticks_no_nl: Vec<_> = rects_no_nl.iter().filter(|(r, c)| *c == deleted_col && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(ticks_no_nl.len(), 1);
        assert!(ticks_no_nl[0].0.max.y <= bottom_clip.bottom() + 0.01, "EOF tick must not clip below clip.bottom()");
        assert!((ticks_no_nl[0].0.height() - 2.0).abs() < 0.01);

        // With trailing `\n` (`"a\nb\nc\n"` -> `"a\nb\n"`): Deleted { start: 2, end: 2 } sits on seam above trailing empty row.
        let with_nl_text = "a\nb\n";
        let (galley_nl, rects_nl, deleted_col_nl, _) = run_paint(with_nl_text, f32::INFINITY, Pos2::ZERO, probe_clip, &marks);
        assert_eq!(galley_nl.rows.len(), 3);
        let ticks_nl: Vec<_> = rects_nl.iter().filter(|(r, c)| *c == deleted_col_nl && (r.height() - 2.0).abs() < 0.01).collect();
        assert_eq!(ticks_nl.len(), 1);
        let expected_y = galley_nl.rows[galley_nl.rows.len() - 2].max_y() - 1.0;
        assert!((ticks_nl[0].0.min.y - expected_y).abs() < 0.01);
    }
}
