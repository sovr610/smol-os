// SPDX-License-Identifier: GPL-3.0-or-later
//! The mouse: a position in pixels, turned into the cell-based mouse
//! events the desktop understands.

use termos_core::input::{Mods, MouseButton, MouseEvent, MouseKind};
use termos_render::raster::{CellMetrics, Layout};

// evdev button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// One wheel notch in libinput's v120 units.
const NOTCH: f64 = 120.0;

#[derive(Debug, Default)]
pub(super) struct Pointer {
    pub(super) x: f64,
    pub(super) y: f64,
    held: Option<MouseButton>,
    cell: Option<(u16, u16)>,
    wheel_v: f64,
    wheel_h: f64,
}

impl Pointer {
    /// The pointer moved to another display: the next move reports its
    /// cell there, whatever it was here.
    pub(super) fn entered_another(&mut self) {
        self.cell = None;
    }

    fn at(&self, layout: &Layout, m: &CellMetrics) -> (u16, u16) {
        layout.cell_at(m, self.x.max(0.0) as u32, self.y.max(0.0) as u32)
    }

    fn event(&self, kind: MouseKind, (x, y): (u16, u16), mods: Mods) -> MouseEvent {
        MouseEvent { kind, x, y, mods }
    }

    /// Moves to `(x, y)` pixels (clamped to `w`×`h`); an event when the
    /// cell under it changes.
    pub(super) fn move_to(
        &mut self,
        x: f64,
        y: f64,
        (w, h): (u32, u32),
        layout: &Layout,
        m: &CellMetrics,
        mods: Mods,
    ) -> Option<MouseEvent> {
        self.x = x.clamp(0.0, f64::from(w.saturating_sub(1)));
        self.y = y.clamp(0.0, f64::from(h.saturating_sub(1)));
        let cell = self.at(layout, m);
        if self.cell == Some(cell) {
            return None;
        }
        self.cell = Some(cell);
        let kind = match self.held {
            Some(b) => MouseKind::Drag(b),
            None => MouseKind::Moved,
        };
        Some(self.event(kind, cell, mods))
    }

    /// A button went down or up.
    pub(super) fn button(
        &mut self,
        code: u32,
        pressed: bool,
        layout: &Layout,
        m: &CellMetrics,
        mods: Mods,
    ) -> Option<MouseEvent> {
        let b = match code {
            BTN_LEFT => MouseButton::Left,
            BTN_RIGHT => MouseButton::Right,
            BTN_MIDDLE => MouseButton::Middle,
            _ => return None,
        };
        let cell = self.at(layout, m);
        if pressed {
            self.held = Some(b);
            Some(self.event(MouseKind::Down(b), cell, mods))
        } else {
            if self.held == Some(b) {
                self.held = None;
            }
            Some(self.event(MouseKind::Up(b), cell, mods))
        }
    }

    /// Wheel movement in v120 units (positive: down / right): one event
    /// per notch.
    pub(super) fn wheel(
        &mut self,
        vertical: f64,
        horizontal: f64,
        layout: &Layout,
        m: &CellMetrics,
        mods: Mods,
    ) -> Vec<MouseEvent> {
        let cell = self.at(layout, m);
        let mut out = Vec::new();
        self.wheel_v += vertical;
        self.wheel_h += horizontal;
        while self.wheel_v.abs() >= NOTCH {
            let down = self.wheel_v > 0.0;
            self.wheel_v -= NOTCH.copysign(self.wheel_v);
            out.push(self.event(
                if down {
                    MouseKind::ScrollDown
                } else {
                    MouseKind::ScrollUp
                },
                cell,
                mods,
            ));
        }
        while self.wheel_h.abs() >= NOTCH {
            let right = self.wheel_h > 0.0;
            self.wheel_h -= NOTCH.copysign(self.wheel_h);
            out.push(self.event(
                if right {
                    MouseKind::ScrollRight
                } else {
                    MouseKind::ScrollLeft
                },
                cell,
                mods,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> CellMetrics {
        CellMetrics {
            width: 10,
            height: 20,
            baseline: 15,
            underline: 17,
            strike: 10,
            stroke: 1,
        }
    }

    const L: Layout = Layout {
        cols: 80,
        rows: 24,
        x: 0,
        y: 0,
    };

    #[test]
    fn moves_clicks_drags_and_wheels() {
        let mut p = Pointer::default();
        let none = Mods::empty();
        let e = p
            .move_to(25.0, 45.0, (800, 480), &L, &m(), none)
            .expect("moved");
        assert_eq!((e.kind, e.x, e.y), (MouseKind::Moved, 2, 2));
        assert_eq!(
            p.move_to(28.0, 50.0, (800, 480), &L, &m(), none),
            None,
            "same cell"
        );
        let d = p.button(BTN_LEFT, true, &L, &m(), none).expect("down");
        assert_eq!(d.kind, MouseKind::Down(MouseButton::Left));
        let drag = p
            .move_to(45.0, 50.0, (800, 480), &L, &m(), Mods::SHIFT)
            .expect("drag");
        assert_eq!(
            (drag.kind, drag.x, drag.mods),
            (MouseKind::Drag(MouseButton::Left), 4, Mods::SHIFT)
        );
        let u = p.button(BTN_LEFT, false, &L, &m(), none).expect("up");
        assert_eq!(u.kind, MouseKind::Up(MouseButton::Left));
        assert_eq!(
            p.move_to(5000.0, -5.0, (800, 480), &L, &m(), none)
                .map(|e| (e.x, e.y)),
            Some((79, 0)),
            "clamped"
        );
        assert!(
            p.wheel(60.0, 0.0, &L, &m(), none).is_empty(),
            "half a notch"
        );
        let w = p.wheel(60.0, 0.0, &L, &m(), none);
        assert_eq!(
            w.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [MouseKind::ScrollDown]
        );
        let w = p.wheel(-240.0, 120.0, &L, &m(), none);
        assert_eq!(
            w.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [
                MouseKind::ScrollUp,
                MouseKind::ScrollUp,
                MouseKind::ScrollRight
            ]
        );
        assert_eq!(p.button(0x113, true, &L, &m(), none), None, "side buttons");
    }
}
