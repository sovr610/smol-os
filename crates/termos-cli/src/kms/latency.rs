// SPDX-License-Identifier: GPL-3.0-or-later
//! The console's input→frame time. D-019's budget (16 ms on x86_64, 33 ms
//! on a Pi 4, p99) runs from reading a key to flushing the frame that
//! answers it, which here is asking the display to flip in a buffer that
//! holds it. The flip itself happens at the next vertical blank, up to a
//! refresh later, so the time until it is shown is reported beside it.
//! Every [`EVERY`] keys the log gets the median, the 99th percentile and
//! the worst of the last [`KEEP`], so a real board's numbers can be read
//! off it (M9).

use std::time::{Duration, Instant};

/// How often (in measured keys) a report is made.
pub(super) const EVERY: usize = 100;
/// How many samples a report covers.
pub(super) const KEEP: usize = 1000;

#[derive(Debug, Default)]
pub(super) struct Latency {
    /// The first key not yet answered by a frame sent to the display.
    pending: Option<Instant>,
    /// A frame came from the server after that key.
    framed: bool,
    /// A key whose frame was flushed: when it was read, and how long the
    /// flush took. The flip that shows it finishes the sample.
    flushed: Option<(Instant, Duration)>,
    /// Key to flush (D-019's measure).
    flush: Vec<Duration>,
    /// Key to the frame on the screen.
    shown: Vec<Duration>,
    measured: usize,
}

/// Percentiles of some samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stats {
    pub(super) p50: Duration,
    pub(super) p99: Duration,
    pub(super) max: Duration,
}

impl Stats {
    fn of(samples: &[Duration]) -> Self {
        let mut s = samples.to_vec();
        s.sort();
        let at = |q: f64| s[((s.len() as f64 - 1.0) * q).round() as usize];
        Self {
            p50: at(0.5),
            p99: at(0.99),
            max: s[s.len() - 1],
        }
    }
}

/// A summary of recent samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) flush: Stats,
    pub(super) shown: Stats,
    pub(super) keys: usize,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "input to frame: p50 {:.1} ms, p99 {:.1} ms, worst {:.1} ms; \
             on the screen: p50 {:.1} ms, p99 {:.1} ms (last {} keys)",
            ms(self.flush.p50),
            ms(self.flush.p99),
            ms(self.flush.max),
            ms(self.shown.p50),
            ms(self.shown.p99),
            self.keys
        )
    }
}

fn keep(samples: &mut Vec<Duration>, d: Duration) {
    if samples.len() == KEEP {
        samples.remove(0);
    }
    samples.push(d);
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

    /// A flip was asked for, of a buffer holding every frame so far.
    pub(super) fn flushed(&mut self, now: Instant) {
        if let Some(start) = self.pending.filter(|_| self.framed) {
            self.pending = None;
            self.flushed = Some((start, now.saturating_duration_since(start)));
        }
    }

    /// A page flip finished; a report every [`EVERY`] keys.
    pub(super) fn flipped(&mut self, now: Instant) -> Option<Report> {
        let (start, flush) = self.flushed.take()?;
        keep(&mut self.flush, flush);
        keep(&mut self.shown, now.saturating_duration_since(start));
        self.measured += 1;
        self.measured.is_multiple_of(EVERY).then(|| Report {
            flush: Stats::of(&self.flush),
            shown: Stats::of(&self.shown),
            keys: self.flush.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn keys_are_timed_to_the_flush_of_their_frame() {
        let t0 = Instant::now();
        let mut l = Latency::default();
        // A flush with no frame since the key does not count.
        l.key(t0);
        l.flushed(t0 + ms(3));
        assert_eq!(l.flipped(t0 + ms(4)), None);
        assert!(l.flush.is_empty());
        // A second key before the frame keeps the first one's time.
        l.key(t0 + ms(5));
        l.frame();
        l.flushed(t0 + ms(12));
        assert_eq!(l.flipped(t0 + ms(20)), None);
        assert_eq!(l.flush, [ms(12)]);
        assert_eq!(l.shown, [ms(20)]);
        // With nothing pending, frames, flushes and flips are not timed.
        l.frame();
        l.flushed(t0 + ms(30));
        assert_eq!(l.flipped(t0 + ms(40)), None);
        assert_eq!(l.flush.len(), 1);
    }

    #[test]
    fn reports_every_hundred_keys() {
        let t0 = Instant::now();
        let mut l = Latency::default();
        let mut reports = Vec::new();
        for i in 0..200u64 {
            let at = t0 + ms(i * 100);
            l.key(at);
            l.frame();
            // Flushed after 1..=100 ms, shown 16 ms later.
            l.flushed(at + ms(i % 100 + 1));
            if let Some(r) = l.flipped(at + ms(i % 100 + 17)) {
                reports.push(r);
            }
        }
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].flush.p50, ms(51));
        assert_eq!(reports[0].flush.p99, ms(99));
        assert_eq!(reports[0].flush.max, ms(100));
        assert_eq!(reports[0].shown.p50, ms(67));
        assert_eq!(reports[1].keys, 200);
        assert_eq!(
            reports[0].to_string(),
            "input to frame: p50 51.0 ms, p99 99.0 ms, worst 100.0 ms; \
             on the screen: p50 67.0 ms, p99 115.0 ms (last 100 keys)"
        );
    }
}
