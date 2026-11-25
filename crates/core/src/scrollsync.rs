//! Piecewise-linear mapping between source lines and preview y positions.

#[derive(Debug, Clone, Default)]
pub struct ScrollMap {
    /// Sorted by line; y is non-decreasing.
    pts: Vec<(f32, f32)>,
}

impl ScrollMap {
    pub fn from_points(mut points: Vec<(f32, f32)>) -> Self {
        points.retain(|(l, y)| l.is_finite() && y.is_finite());
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut max_y = f32::NEG_INFINITY;
        for p in points.iter_mut() {
            max_y = max_y.max(p.1);
            p.1 = max_y;
        }
        ScrollMap { pts: points }
    }

    pub fn is_empty(&self) -> bool {
        self.pts.is_empty()
    }

    pub fn y_for_line(&self, line: f32) -> f32 {
        let (Some(first), Some(last)) = (self.pts.first(), self.pts.last()) else {
            return 0.0;
        };
        if line.is_nan() || line <= first.0 {
            return first.1;
        }
        if line >= last.0 {
            return last.1;
        }
        let idx = self.pts.partition_point(|p| p.0 <= line);
        let (p0, p1) = (self.pts[idx - 1], self.pts[idx]);
        p0.1 + (line - p0.0) / (p1.0 - p0.0) * (p1.1 - p0.1)
    }

    pub fn line_for_y(&self, y: f32) -> f32 {
        let (Some(first), Some(last)) = (self.pts.first(), self.pts.last()) else {
            return 0.0;
        };
        if y.is_nan() || y <= first.1 {
            return first.0;
        }
        if y >= last.1 {
            return last.0;
        }
        let idx = self.pts.partition_point(|p| p.1 < y);
        let (p0, p1) = (self.pts[idx - 1], self.pts[idx]);
        p0.0 + (y - p0.1) / (p1.1 - p0.1) * (p1.0 - p0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_single() {
        let m = ScrollMap::from_points(vec![]);
        assert!(m.is_empty());
        assert_eq!((m.y_for_line(5.0), m.line_for_y(5.0)), (0.0, 0.0));
        let m = ScrollMap::from_points(vec![(3.0, 40.0)]);
        assert_eq!((m.y_for_line(0.0), m.y_for_line(9.0), m.line_for_y(0.0), m.line_for_y(99.0)), (40.0, 40.0, 3.0, 3.0));
    }

    #[test]
    fn interpolation_and_clamp() {
        let m = ScrollMap::from_points(vec![(10.0, 100.0), (0.0, 0.0), (20.0, 400.0), (f32::NAN, 1.0)]);
        assert_eq!(m.y_for_line(5.0), 50.0);
        assert_eq!(m.y_for_line(15.0), 250.0);
        assert_eq!(m.y_for_line(-3.0), 0.0);
        assert_eq!(m.y_for_line(99.0), 400.0);
        assert_eq!(m.line_for_y(250.0), 15.0);
        assert_eq!(m.line_for_y(1e9), 20.0);
    }

    #[test]
    fn non_decreasing_forced_and_inverse() {
        let m = ScrollMap::from_points(vec![(0.0, 0.0), (1.0, 50.0), (2.0, 30.0), (3.0, 30.0), (4.0, 80.0), (4.0, 90.0)]);
        let mut prev = -1.0;
        for i in 0..=400 {
            let l = i as f32 * 0.01;
            let y = m.y_for_line(l);
            assert!(y >= prev);
            prev = y;
        }
        // inverse where strictly increasing
        for l in [0.2f32, 0.9, 3.5, 3.9] {
            let y = m.y_for_line(l);
            assert!((m.line_for_y(y) - l).abs() < 1e-3, "{l}");
        }
        for y in [10.0f32, 49.0, 60.0, 79.0] {
            assert!((m.y_for_line(m.line_for_y(y)) - y).abs() < 1e-3, "{y}");
        }
    }
}
