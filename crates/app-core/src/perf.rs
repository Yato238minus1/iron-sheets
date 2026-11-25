//! Startup milestones, frame timing and named timers with an F12 overlay.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct Timer {
    pub last_ms: f32,
    pub avg_ms: f32,
    pub max_ms: f32,
    pub count: u64,
}

pub struct Perf {
    pub start: Instant,
    pub milestones: Vec<(&'static str, f32)>,
    pub overlay: bool,
    pub frames: u64,
    pub last_frame_ms: f32,
    pub avg_frame_ms: f32,
    pub timers: BTreeMap<&'static str, Timer>,
    /// Background work in flight as (what, count), refreshed by the frontend each frame while the overlay is on.
    pub inflight: Vec<(&'static str, usize)>,
    /// Starts of the frames of the last second (see `fps`).
    recent: std::collections::VecDeque<Instant>,
    /// Print milestones to stdout as they are recorded (`--bench-startup`).
    pub print: bool,
}

impl Perf {
    pub fn new(start: Instant) -> Self {
        Perf { start, milestones: Vec::new(), overlay: false, frames: 0, last_frame_ms: 0.0, avg_frame_ms: 0.0, timers: BTreeMap::new(), inflight: Vec::new(), recent: Default::default(), print: false }
    }

    pub fn milestone(&mut self, name: &'static str) {
        let ms = self.start.elapsed().as_secs_f32() * 1000.0;
        self.milestones.push((name, ms));
        if self.print {
            println!("MILESTONE {name} {ms:.1}");
        }
    }

    /// Report a duration under a name (e.g. `"md.parse"`).
    pub fn record(&mut self, name: &'static str, d: Duration) {
        let ms = d.as_secs_f32() * 1000.0;
        let t = self.timers.entry(name).or_default();
        t.last_ms = ms;
        t.count += 1;
        t.avg_ms += (ms - t.avg_ms) / t.count.min(60) as f32;
        t.max_ms = t.max_ms.max(ms);
    }

    /// Time a closure under `name`.
    pub fn time<R>(&mut self, name: &'static str, f: impl FnOnce() -> R) -> R {
        let t = Instant::now();
        let r = f();
        self.record(name, t.elapsed());
        r
    }

    /// Frames painted during the last second (0 when idle: the app only repaints on demand).
    pub fn fps(&self) -> usize {
        let now = Instant::now();
        self.recent.iter().filter(|t| now.duration_since(**t) <= Duration::from_secs(1)).count()
    }

    pub fn frame_done(&mut self, d: Duration) {
        let now = Instant::now();
        while self.recent.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1)) {
            self.recent.pop_front();
        }
        self.recent.push_back(now);
        self.frames += 1;
        self.last_frame_ms = d.as_secs_f32() * 1000.0;
        let n = self.frames.min(60) as f32;
        self.avg_frame_ms += (self.last_frame_ms - self.avg_frame_ms) / n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_counts_the_last_second_only() {
        let mut p = Perf::new(Instant::now());
        assert_eq!(p.fps(), 0);
        for _ in 0..5 {
            p.frame_done(Duration::from_millis(2));
        }
        assert_eq!(p.fps(), 5);
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(p.fps(), 0, "idle: nothing painted for a second");
    }
}
