// SPDX-License-Identifier: GPL-3.0-or-later
//! Mouse hit testing against a [`Placement`].

use termos_core::ids::PaneId;

use crate::layout::Separator;
use crate::workspace::{FloatPlacement, Placement};

/// A floating window's resize handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Bottom,
    BottomLeft,
    BottomRight,
}

/// Title-bar buttons of a floating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Close,
}

/// What is under the mouse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Nothing,
    /// Inside a pane's content; `x`/`y` are relative to the content area.
    Pane {
        pane: PaneId,
        x: u16,
        y: u16,
    },
    /// A floating window's title bar (drag to move).
    FloatTitle(PaneId),
    FloatButton(PaneId, Button),
    /// A floating window's border (drag to resize).
    FloatEdge(PaneId, Edge),
    /// A tiled separator (drag to resize).
    Separator(Separator),
}

/// Columns of the title-bar buttons, relative to the window's right edge:
/// `… ─ – ─ × ─╮` (minimize at right-5, close at right-3).
pub const MINIMIZE_OFFSET: u16 = 5;
pub const CLOSE_OFFSET: u16 = 3;

fn hit_float(f: &FloatPlacement, x: u16, y: u16) -> Option<Hit> {
    let o = f.outer;
    if !o.contains(x, y) {
        return None;
    }
    let right = o.right() - 1;
    let bottom = o.bottom() - 1;
    if f.inner.contains(x, y) {
        return Some(Hit::Pane {
            pane: f.pane,
            x: x - f.inner.x,
            y: y - f.inner.y,
        });
    }
    if y == o.y {
        if o.w > 10 && x == o.right() - CLOSE_OFFSET {
            return Some(Hit::FloatButton(f.pane, Button::Close));
        }
        if o.w > 10 && x == o.right() - MINIMIZE_OFFSET {
            return Some(Hit::FloatButton(f.pane, Button::Minimize));
        }
        return Some(Hit::FloatTitle(f.pane));
    }
    let edge = match (x == o.x, x == right, y == bottom) {
        (true, _, true) => Edge::BottomLeft,
        (_, true, true) => Edge::BottomRight,
        (_, _, true) => Edge::Bottom,
        (true, _, _) => Edge::Left,
        _ => Edge::Right,
    };
    Some(Hit::FloatEdge(f.pane, edge))
}

impl Placement {
    /// What is at screen cell (`x`, `y`): floats top-down, then separators,
    /// then tiled panes.
    pub fn hit(&self, x: u16, y: u16) -> Hit {
        for f in self.floats.iter().rev() {
            if let Some(h) = hit_float(f, x, y) {
                return h;
            }
        }
        for s in &self.separators {
            if s.rect.contains(x, y) {
                return Hit::Separator(s.clone());
            }
        }
        for (pane, r) in &self.tiled {
            if r.contains(x, y) {
                return Hit::Pane {
                    pane: *pane,
                    x: x - r.x,
                    y: y - r.y,
                };
            }
        }
        Hit::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{SplitHow, Workspace};
    use termos_render::grid::Rect;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    #[test]
    fn hits_tiled_panes_and_separators() {
        let mut ws = Workspace::new("w", PaneId(1));
        ws.add_split(PaneId(2), SplitHow::Right, AREA);
        let pl = ws.placement(AREA);
        assert_eq!(
            pl.hit(3, 4),
            Hit::Pane {
                pane: PaneId(1),
                x: 3,
                y: 4
            }
        );
        assert!(matches!(pl.hit(40, 10), Hit::Separator(_)));
        assert_eq!(
            pl.hit(45, 0),
            Hit::Pane {
                pane: PaneId(2),
                x: 4,
                y: 0
            }
        );
    }

    #[test]
    fn floats_win_over_tiles() {
        let mut ws = Workspace::new("w", PaneId(1));
        ws.add_float(PaneId(2), AREA);
        ws.set_float_rect(PaneId(2), Rect::new(10, 5, 30, 10), AREA);
        let pl = ws.placement(AREA);
        assert_eq!(pl.hit(15, 5), Hit::FloatTitle(PaneId(2)));
        assert_eq!(pl.hit(37, 5), Hit::FloatButton(PaneId(2), Button::Close));
        assert_eq!(pl.hit(35, 5), Hit::FloatButton(PaneId(2), Button::Minimize));
        assert_eq!(pl.hit(10, 9), Hit::FloatEdge(PaneId(2), Edge::Left));
        assert_eq!(pl.hit(39, 14), Hit::FloatEdge(PaneId(2), Edge::BottomRight));
        assert_eq!(
            pl.hit(11, 6),
            Hit::Pane {
                pane: PaneId(2),
                x: 0,
                y: 0
            }
        );
        assert_eq!(
            pl.hit(5, 5),
            Hit::Pane {
                pane: PaneId(1),
                x: 5,
                y: 5
            }
        );
    }
}
