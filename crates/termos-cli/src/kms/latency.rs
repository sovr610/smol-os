// SPDX-License-Identifier: GPL-3.0-or-later
//! The console's input→frame time (D-019's budget: 16 ms on x86_64, 33 ms
//! on a Pi 4, p99): from reading a key to the page flip that shows the
//! desktop's next frame. Every [`EVERY`] keys it reports the median, the
//! 99th percentile and the worst of the last [`KEEP`], for the log, so a
//! real board's numbers can be read off it (M9).

use std::time::{Duration, Instant};

/// How often (in measured keys) a report is made.
pub(super) const EVERY: usize = 100;
/// How many samples a report covers.
pub(super) const KEEP: usize = 1000;

#[derive(Debug, Default)]
pub(super) struct Latency {
    /// The first key not yet answered by a frame on the screen.
    pending: Option<Instant>,
    /// A frame came from the server after that key.
    framed: bool,
    samples: Vec<Duration>,
    measured: usize,
}

/// A summary of recent samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) p50: Duration,
    pub(super) p99: Duration,
    pub(super) max: Duration,
    pub(super) keys: usize,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "input to frame: p50 {:.1} ms, p99 {:.1} ms, worst {:.1} ms (last {} keys)",
            ms(self.p50),
            ms(self.p99),
            ms(self.max),
            self.keys
        )
    }
}

impl Latency {
    /// A key was read.
    pub(super) fn key(&mut self, at: Instant) {
        if self.pending.is_none() {
            self.pending = Some(at);
            self.framed = false;
        }
    }

    /// A frame arrived from the desktop.
    pub(super) fn frame(&mut self) {
        if self.pending.is_some() {
            self.framed = true;
        }
    }

    /// A page flip finished; a report every [`EVERY`] keys.
    pub(super) fn flipped(&mut self, now: Instant) -> Option<Report> {
        let start = self.pending.filter(|_| self.framed)?;
        self.pending = None;
        if self.samples.len() == KEEP {
            self.samples.remove(0);
        }
        self.samples.push(now.saturating_duration_since(start));
        self.measured += 1;
        self.measured.is_multiple_of(EVERY).then(|| self.report())
    }

    fn report(&self) -> Report {
        let mut s = self.samples.clone();
        s.sort();
        let at = |q: f64| s[((s.len() as f64 - 1.0) * q).round() as usize];
        Report {
            p50: at(0.5),
            p99: at(0.99),
            max: s[s.len() - 1],
            keys: s.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_timed_to_the_flip_after_their_frame() {
        let t0 = Instant::now();
        let mut l = Latency::default();
        // A flip with no frame since the key does not count.
        l.key(t0);
        assert_eq!(l.flipped(t0 + Duration::from_millis(3)), None);
        // A second key before the frame keeps the first one's time.
        l.key(t0 + Duration::from_millis(5));
        l.frame();
        assert_eq!(l.flipped(t0 + Duration::from_millis(12)), None);
        assert_eq!(l.samples, [Duration::from_millis(12)]);
        // With nothing pending, frames and flips are not timed.
        l.frame();
        assert_eq!(l.flipped(t0 + Duration::from_millis(40)), None);
        assert_eq!(l.samples.len(), 1);
    }

    #[test]
    fn reports_every_hundred_keys() {
        let t0 = Instant::now();
        let mut l = Latency::default();
        let mut reports = Vec::new();
        for i in 0..200u64 {
            let at = t0 + Duration::from_millis(i * 100);
            l.key(at);
            l.frame();
            // 1..=100 ms, then again.
            if let Some(r) = l.flipped(at + Duration::from_millis(i % 100 + 1)) {
                reports.push(r);
            }
        }
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].p50, Duration::from_millis(51));
        assert_eq!(reports[0].p99, Duration::from_millis(99));
        assert_eq!(reports[0].max, Duration::from_millis(100));
        assert_eq!(reports[1].keys, 200);
        assert!(
            reports[0]
                .to_string()
                .starts_with("input to frame: p50 51.0 ms, p99 99.0 ms")
        );
    }
}
