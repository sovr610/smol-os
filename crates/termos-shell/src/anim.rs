// SPDX-License-Identifier: GPL-3.0-or-later
//! Short UI animations (D-042): floats opening and closing, toasts, and
//! the splash a client sees when it attaches (D-089).
//!
//! Animations are visual only. They change how a frame is drawn for the
//! clients that animate (not over SSH, not with reduced motion), never
//! the layout or a pane's size. While one runs and some client animates,
//! the desktop asks for a frame every [`FRAME`]; otherwise there is no
//! timer at all (D-028).

use std::time::{Duration, Instant};

use termos_core::ids::{ClientId, PaneId};
use termos_render::grid::Rect;

/// How long each animation lasts (the splash: [`SPLASH`]).
pub const DURATION: Duration = Duration::from_millis(120);

/// How long the splash shows.
pub const SPLASH: Duration = Duration::from_millis(900);

/// Time between animation frames (about 60 per second).
pub const FRAME: Duration = Duration::from_millis(16);

/// What is animating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A float appearing: it grows from its centre.
    FloatOpen(PaneId),
    /// A float that closed: its frame shrinks away where it was.
    FloatClose(Rect),
    /// A new toast: it slides in from the right of the status line.
    Toast,
    /// The logo, for one client that just attached.
    Splash(ClientId),
}

impl Kind {
    pub fn duration(self) -> Duration {
        match self {
            Kind::Splash(_) => SPLASH,
            _ => DURATION,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Anim {
    kind: Kind,
    start: Instant,
}

/// The animations in progress.
#[derive(Debug, Default)]
pub struct Animations {
    list: Vec<Anim>,
    /// When the last animation frame was due.
    last_frame: Option<Instant>,
}

impl Animations {
    /// Starts an animation (restarting one of the same kind).
    pub fn start(&mut self, kind: Kind, now: Instant) {
        self.list.retain(|a| a.kind != kind);
        self.list.push(Anim { kind, start: now });
        self.last_frame = Some(now);
    }

    /// When the running animation of `kind` started (tests render frames
    /// relative to it).
    pub fn started(&self, kind: Kind) -> Option<Instant> {
        self.list.iter().find(|a| a.kind == kind).map(|a| a.start)
    }

    pub fn is_active(&self) -> bool {
        !self.list.is_empty()
    }

    /// When the next frame is due, while something animates.
    pub fn deadline(&self) -> Option<Instant> {
        let last = self.last_frame?;
        let end = self
            .list
            .iter()
            .map(|a| a.start + a.kind.duration())
            .max()?;
        Some((last + FRAME).min(end))
    }

    /// Advances to `now`: drops finished animations. Returns whether a
    /// frame must be drawn (one more after the last one ends, so the final
    /// state shows).
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.list.is_empty() {
            return false;
        }
        self.list.retain(|a| now < a.start + a.kind.duration());
        self.last_frame = (!self.list.is_empty()).then_some(now);
        true
    }

    /// Ends the animation of `kind` early. Returns whether one ran.
    pub fn stop(&mut self, kind: Kind) -> bool {
        let before = self.list.len();
        self.list.retain(|a| a.kind != kind);
        before != self.list.len()
    }

    /// Linear progress (0 to 1) of an animation of `kind`, if it runs.
    pub fn linear(&self, kind: Kind, now: Instant) -> Option<f32> {
        self.list
            .iter()
            .find(|a| a.kind == kind)
            .map(|a| fraction(a.kind, now.saturating_duration_since(a.start)))
    }

    /// Drops everything (no client animates any more).
    pub fn clear(&mut self) {
        self.list.clear();
        self.last_frame = None;
    }

    /// Eased progress (0 to 1) of an animation of `kind`, if it runs.
    pub fn progress(&self, kind: Kind, now: Instant) -> Option<f32> {
        self.list
            .iter()
            .find(|a| a.kind == kind)
            .map(|a| ease_out(fraction(a.kind, now.saturating_duration_since(a.start))))
    }

    /// The closing floats still shrinking, with their progress.
    pub fn closing(&self, now: Instant) -> impl Iterator<Item = (Rect, f32)> + '_ {
        self.list.iter().filter_map(move |a| match a.kind {
            Kind::FloatClose(r) => Some((
                r,
                ease_out(fraction(a.kind, now.saturating_duration_since(a.start))),
            )),
            _ => None,
        })
    }
}

fn fraction(kind: Kind, elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / kind.duration().as_secs_f32()).clamp(0.0, 1.0)
}

/// Cubic ease-out: fast start, gentle stop.
pub fn ease_out(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// `rect` scaled by `factor` around its centre, at least 2x2 (room for a
/// border).
pub fn scale(rect: Rect, factor: f32) -> Rect {
    let f = factor.clamp(0.0, 1.0);
    let scaled = |v: u16| -> u16 {
        // In range: a fraction of a u16.
        let s = (f32::from(v) * f).round() as u16;
        s.clamp(2.min(v), v)
    };
    let (w, h) = (scaled(rect.w), scaled(rect.h));
    Rect::new(rect.x + (rect.w - w) / 2, rect.y + (rect.h - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_and_scaling() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "ease-out is ahead of linear");
        let r = Rect::new(10, 4, 40, 12);
        assert_eq!(scale(r, 1.0), r);
        assert_eq!(scale(r, 0.5), Rect::new(20, 7, 20, 6));
        assert_eq!(scale(r, 0.0), Rect::new(29, 9, 2, 2));
    }

    #[test]
    fn frames_while_running_then_one_last_then_none() {
        let t0 = Instant::now();
        let mut a = Animations::default();
        assert_eq!(a.deadline(), None, "idle: no timer");
        a.start(Kind::Toast, t0);
        assert_eq!(a.deadline(), Some(t0 + FRAME));
        let mut now = t0;
        let mut frames = 0;
        while let Some(d) = a.deadline() {
            now = d;
            assert!(a.tick(now));
            frames += 1;
            assert!(frames < 20, "runs away");
        }
        assert!((7..=9).contains(&frames), "{frames} frames for 120 ms");
        assert!(now >= t0 + DURATION);
        assert!(!a.is_active());
        assert!(!a.tick(now), "nothing more to draw");
        assert_eq!(a.progress(Kind::Toast, now), None);
    }

    #[test]
    fn progress_and_restart() {
        let t0 = Instant::now();
        let mut a = Animations::default();
        a.start(Kind::FloatOpen(PaneId(3)), t0);
        let half = a
            .progress(Kind::FloatOpen(PaneId(3)), t0 + DURATION / 2)
            .expect("running");
        assert!(half > 0.5 && half < 1.0);
        assert_eq!(a.progress(Kind::FloatOpen(PaneId(4)), t0), None);
        a.start(Kind::FloatOpen(PaneId(3)), t0 + DURATION);
        assert_eq!(a.list.len(), 1, "restarting replaces");
        a.start(Kind::FloatClose(Rect::new(0, 0, 10, 5)), t0);
        assert_eq!(a.closing(t0).count(), 1);
        a.clear();
        assert!(!a.is_active() && a.deadline().is_none());
    }
}
