// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspaces (tmux "windows") and the session that holds them.
//!
//! A workspace has a tiling [`Layout`] plus a stack of floating windows
//! (bottom to top). Any pane can move between the two (D-024).

use serde::{Deserialize, Serialize};
use termos_core::ids::PaneId;
use termos_render::grid::Rect;

use crate::layout::{Dir, Direction, Layout, Preset, Separator};

/// A floating window. `rect` is the outer rectangle, border included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Floating {
    pub pane: PaneId,
    pub rect: Rect,
    pub minimized: bool,
}

/// Where a new pane goes relative to the focused one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitHow {
    Right,
    Down,
    /// Along the longer side (BSP).
    Auto,
}

/// Quick positions for a floating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
    Maximize,
    Center,
}

/// A floating window as placed on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatPlacement {
    pub pane: PaneId,
    pub outer: Rect,
    /// Content area (inside the border).
    pub inner: Rect,
}

/// Everything visible in a workspace for one screen area.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Placement {
    pub area: Rect,
    pub tiled: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
    /// Bottom to top.
    pub floats: Vec<FloatPlacement>,
    pub zoomed: Option<PaneId>,
}

impl Placement {
    /// The content rectangle of a visible pane.
    pub fn content_rect(&self, pane: PaneId) -> Option<Rect> {
        self.floats
            .iter()
            .find(|f| f.pane == pane)
            .map(|f| f.inner)
            .or_else(|| self.tiled.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r))
    }

    /// Visible panes, bottom to top.
    pub fn visible(&self) -> Vec<PaneId> {
        self.tiled
            .iter()
            .map(|(p, _)| *p)
            .chain(self.floats.iter().map(|f| f.pane))
            .collect()
    }
}

/// Smallest outer size of a floating window.
pub const MIN_FLOAT_W: u16 = 12;
pub const MIN_FLOAT_H: u16 = 4;

/// A virtual desktop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// The name follows the focused program until the user renames it.
    pub auto_name: bool,
    pub layout: Layout,
    /// Bottom to top.
    pub floats: Vec<Floating>,
    focus: Option<PaneId>,
    last_focus: Option<PaneId>,
    zoomed: Option<PaneId>,
    preset_index: usize,
}

impl Workspace {
    pub fn new(name: impl Into<String>, first: PaneId) -> Self {
        Self {
            name: name.into(),
            auto_name: true,
            layout: Layout::single(first),
            floats: Vec::new(),
            focus: Some(first),
            last_focus: None,
            zoomed: None,
            preset_index: 0,
        }
    }

    /// Tiled panes in index order, then floats bottom to top.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = self.layout.panes();
        v.extend(self.floats.iter().map(|f| f.pane));
        v
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.layout.contains(pane) || self.is_floating(pane)
    }

    pub fn is_empty(&self) -> bool {
        self.layout.is_empty() && self.floats.is_empty()
    }

    pub fn is_floating(&self, pane: PaneId) -> bool {
        self.floats.iter().any(|f| f.pane == pane)
    }

    pub fn focus(&self) -> Option<PaneId> {
        self.focus
    }

    pub fn last_focus(&self) -> Option<PaneId> {
        self.last_focus.filter(|p| self.contains(*p))
    }

    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// Focuses `pane`: raises and restores it if floating, and leaves zoom
    /// if another pane was zoomed (like tmux).
    pub fn set_focus(&mut self, pane: PaneId) {
        if !self.contains(pane) {
            return;
        }
        if self.focus != Some(pane) {
            self.last_focus = self.focus;
            self.focus = Some(pane);
        }
        if self.zoomed.is_some_and(|z| z != pane) {
            self.zoomed = None;
        }
        if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            let mut f = self.floats.remove(i);
            f.minimized = false;
            self.floats.push(f);
        }
    }

    /// Renames panes (all at once) and drops those `f` maps to `None`.
    pub fn remap_panes(&mut self, f: &mut impl FnMut(PaneId) -> Option<PaneId>) {
        let mut map = std::collections::HashMap::new();
        for p in self.panes() {
            match f(p) {
                Some(n) => {
                    map.insert(p, n);
                }
                None => {
                    self.remove(p);
                }
            }
        }
        let g = |p: PaneId| map.get(&p).copied().unwrap_or(p);
        self.layout.map_panes(g);
        for fl in &mut self.floats {
            fl.pane = g(fl.pane);
        }
        self.focus = self.focus.map(g);
        self.last_focus = self.last_focus.map(g);
        self.zoomed = self.zoomed.map(g);
    }

    /// Removes duplicate panes (keeping the first), repairs the tree and
    /// makes focus and zoom point at panes that exist.
    fn sanitize(&mut self, seen: &mut std::collections::HashSet<PaneId>) {
        self.layout.sanitize();
        let mut local = std::collections::HashSet::new();
        for p in self.panes() {
            if seen.contains(&p) || !local.insert(p) {
                self.remove(p);
            }
        }
        seen.extend(local);
        self.zoomed = self.zoomed.filter(|z| self.layout.contains(*z));
        self.last_focus = self.last_focus.filter(|p| self.contains(*p));
        if !self.focus.is_some_and(|p| self.contains(p)) {
            self.focus = None;
            self.refocus();
        }
    }

    /// Focus after the focused pane went away.
    fn refocus(&mut self) {
        let candidate = self
            .last_focus()
            .filter(|p| !self.is_minimized(*p))
            .or_else(|| {
                self.floats
                    .iter()
                    .rev()
                    .find(|f| !f.minimized)
                    .map(|f| f.pane)
            })
            .or_else(|| self.layout.panes().first().copied())
            .or_else(|| self.floats.last().map(|f| f.pane));
        self.focus = candidate;
        self.last_focus = None;
        if let Some(p) = candidate {
            self.set_focus(p);
        }
    }

    fn is_minimized(&self, pane: PaneId) -> bool {
        self.floats.iter().any(|f| f.pane == pane && f.minimized)
    }

    /// The tiled pane to split from: the focused one if tiled, else the
    /// last focused tiled pane, else the first.
    fn tiled_anchor(&self) -> Option<PaneId> {
        [self.focus, self.last_focus]
            .into_iter()
            .flatten()
            .find(|p| self.layout.contains(*p))
            .or_else(|| self.layout.panes().first().copied())
    }

    /// Adds `new` as a tiled pane next to the focused one and focuses it.
    /// Returns `false` (and changes nothing) if there is no space, like
    /// tmux's "no space for new pane".
    pub fn add_split(&mut self, new: PaneId, how: SplitHow, area: Rect) -> bool {
        let anchor = self.tiled_anchor();
        let dir = match how {
            SplitHow::Right => Some(Dir::Horizontal),
            SplitHow::Down => Some(Dir::Vertical),
            SplitHow::Auto => self.layout.auto_dir(anchor, area),
        };
        let Some(dir) = dir.filter(|d| self.layout.split_fits(anchor, *d, area)) else {
            return false;
        };
        self.layout.split(anchor, new, dir);
        self.zoomed = None;
        self.set_focus(new);
        true
    }

    /// Adds `new` as a floating window and focuses it.
    pub fn add_float(&mut self, new: PaneId, area: Rect) {
        let rect = self.default_float_rect(area);
        self.floats.push(Floating {
            pane: new,
            rect,
            minimized: false,
        });
        self.set_focus(new);
    }

    fn default_float_rect(&self, area: Rect) -> Rect {
        let w = (area.w * 7 / 10).max(MIN_FLOAT_W).min(area.w);
        let h = (area.h * 7 / 10).max(MIN_FLOAT_H).min(area.h);
        // Cascade successive floats so they do not hide each other.
        let n = u16::try_from(self.floats.len() % 6).unwrap_or(0);
        let x = area.x + (area.w - w) / 2 + 2 * n;
        let y = area.y + (area.h - h) / 2 + n;
        Rect::new(x, y, w, h).clamp_within(&area)
    }

    /// Removes a pane. Returns whether it was here.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        let found = if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            self.floats.remove(i);
            true
        } else {
            self.layout.remove(pane)
        };
        if found {
            if self.zoomed == Some(pane) {
                self.zoomed = None;
            }
            if self.last_focus == Some(pane) {
                self.last_focus = None;
            }
            if self.focus == Some(pane) {
                self.focus = None;
                self.refocus();
            }
        }
        found
    }

    /// Floats a tiled pane, or tiles a floating one. Returns `false` if a
    /// floating pane cannot be tiled for lack of space.
    pub fn toggle_float(&mut self, pane: PaneId, area: Rect) -> bool {
        if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            let anchor = self.tiled_anchor();
            let Some(dir) = self.layout.auto_dir(anchor, area) else {
                return false;
            };
            self.floats.remove(i);
            self.layout.split(anchor, pane, dir);
        } else if self.layout.remove(pane) {
            let rect = self.default_float_rect(area);
            self.floats.push(Floating {
                pane,
                rect,
                minimized: false,
            });
        }
        self.zoomed = None;
        self.set_focus(pane);
        true
    }

    /// Hides or shows a floating window (it stays listed in the status bar).
    pub fn toggle_minimize(&mut self, pane: PaneId) {
        let Some(f) = self.floats.iter_mut().find(|f| f.pane == pane) else {
            return;
        };
        f.minimized = !f.minimized;
        if f.minimized {
            if self.focus == Some(pane) {
                self.focus = None;
                self.last_focus = self.last_focus.filter(|p| *p != pane);
                self.refocus_excluding(pane);
            }
        } else {
            self.set_focus(pane);
        }
    }

    fn refocus_excluding(&mut self, pane: PaneId) {
        let next = self
            .last_focus()
            .filter(|p| *p != pane && !self.is_minimized(*p))
            .or_else(|| {
                self.floats
                    .iter()
                    .rev()
                    .find(|f| !f.minimized && f.pane != pane)
                    .map(|f| f.pane)
            })
            .or_else(|| self.layout.panes().first().copied());
        self.focus = next;
    }

    /// Minimized floating windows.
    pub fn minimized(&self) -> Vec<PaneId> {
        self.floats
            .iter()
            .filter(|f| f.minimized)
            .map(|f| f.pane)
            .collect()
    }

    /// Sets a floating window's outer rectangle (clamped to `area`, with a
    /// minimum size).
    pub fn set_float_rect(&mut self, pane: PaneId, rect: Rect, area: Rect) {
        if let Some(f) = self.floats.iter_mut().find(|f| f.pane == pane) {
            let r = Rect::new(
                rect.x,
                rect.y,
                rect.w.max(MIN_FLOAT_W).min(area.w),
                rect.h.max(MIN_FLOAT_H).min(area.h),
            );
            f.rect = r.clamp_within(&area);
        }
    }

    pub fn float_rect(&self, pane: PaneId) -> Option<Rect> {
        self.floats.iter().find(|f| f.pane == pane).map(|f| f.rect)
    }

    /// Moves a floating window to a preset position.
    pub fn snap(&mut self, pane: PaneId, snap: Snap, area: Rect) {
        let (hw, hh) = (area.w / 2, area.h / 2);
        let rect = match snap {
            Snap::LeftHalf => Rect::new(area.x, area.y, hw, area.h),
            Snap::RightHalf => Rect::new(area.x + hw, area.y, area.w - hw, area.h),
            Snap::TopHalf => Rect::new(area.x, area.y, area.w, hh),
            Snap::BottomHalf => Rect::new(area.x, area.y + hh, area.w, area.h - hh),
            Snap::Maximize => area,
            Snap::Center => {
                let w = area.w * 7 / 10;
                let h = area.h * 7 / 10;
                Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
            }
        };
        self.set_float_rect(pane, rect, area);
    }

    /// Zooms (maximizes) the focused pane, or unzooms.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => self.focus,
        };
    }

    /// Re-arranges tiled panes with the next preset that fits (tmux
    /// `next-layout`).
    pub fn next_preset(&mut self, area: Rect) -> Option<Preset> {
        for _ in 0..Preset::ALL.len() {
            let preset = Preset::ALL[self.preset_index % Preset::ALL.len()];
            self.preset_index = (self.preset_index + 1) % Preset::ALL.len();
            if self.layout.try_preset(preset, area) {
                self.zoomed = None;
                return Some(preset);
            }
        }
        None
    }

    /// Applies a preset if it fits.
    pub fn select_preset(&mut self, preset: Preset, area: Rect) -> bool {
        if !self.layout.try_preset(preset, area) {
            return false;
        }
        if let Some(i) = Preset::ALL.iter().position(|p| *p == preset) {
            self.preset_index = (i + 1) % Preset::ALL.len();
        }
        self.zoomed = None;
        true
    }

    /// Focus in a direction. From a floating window, left/up/right/down go
    /// to the tiled layer.
    pub fn focus_direction(&mut self, dir: Direction, area: Rect) -> bool {
        let Some(focus) = self.focus else {
            return false;
        };
        let target = if self.layout.contains(focus) {
            self.layout.neighbor(focus, dir, area)
        } else {
            self.tiled_anchor()
        };
        match target {
            Some(t) => {
                self.set_focus(t);
                true
            }
            None => false,
        }
    }

    /// Visible panes in cycling order (tiled, then floats bottom to top).
    fn cycle_order(&self) -> Vec<PaneId> {
        let mut v = self.layout.panes();
        v.extend(self.floats.iter().filter(|f| !f.minimized).map(|f| f.pane));
        v
    }

    /// Focuses the next (`forward`) or previous visible pane.
    pub fn focus_cycle(&mut self, forward: bool) {
        let order = self.cycle_order();
        if order.is_empty() {
            return;
        }
        // Floats get re-ordered by focusing, so cycle over tiled + floats in
        // a stable order: by pane id among floats.
        let mut order = order;
        let tiled = self.layout.panes().len();
        order[tiled..].sort();
        let pos = self
            .focus
            .and_then(|f| order.iter().position(|p| *p == f))
            .unwrap_or(0);
        let n = order.len();
        let next = if forward {
            (pos + 1) % n
        } else {
            (pos + n - 1) % n
        };
        self.set_focus(order[next]);
    }

    /// Swaps the focused tiled pane with the next/previous one.
    pub fn swap_cycle(&mut self, forward: bool) {
        let Some(focus) = self.focus.filter(|f| self.layout.contains(*f)) else {
            return;
        };
        let order = self.layout.panes();
        let n = order.len();
        if n < 2 {
            return;
        }
        let pos = order.iter().position(|p| *p == focus).unwrap_or(0);
        let other = order[if forward {
            (pos + 1) % n
        } else {
            (pos + n - 1) % n
        }];
        self.layout.swap(focus, other);
    }

    /// Resizes the focused pane: moves a tiled border, or grows/shrinks a
    /// floating window on that side.
    pub fn resize_focused(&mut self, dir: Direction, cells: u16, area: Rect) {
        let Some(focus) = self.focus else { return };
        if self.layout.contains(focus) {
            self.layout.resize(focus, dir, cells, area);
        } else if let Some(r) = self.float_rect(focus) {
            let r = match dir {
                Direction::Right => Rect::new(r.x, r.y, r.w.saturating_add(cells), r.h),
                Direction::Left => Rect::new(r.x, r.y, r.w.saturating_sub(cells), r.h),
                Direction::Down => Rect::new(r.x, r.y, r.w, r.h.saturating_add(cells)),
                Direction::Up => Rect::new(r.x, r.y, r.w, r.h.saturating_sub(cells)),
            };
            self.set_float_rect(focus, r, area);
        }
    }

    /// Lays the workspace out in `area`.
    pub fn placement(&self, area: Rect) -> Placement {
        let floats: Vec<FloatPlacement> = self
            .floats
            .iter()
            .filter(|f| !f.minimized)
            .map(|f| {
                let outer = if self.zoomed == Some(f.pane) {
                    area
                } else {
                    f.rect.clamp_within(&area)
                };
                FloatPlacement {
                    pane: f.pane,
                    outer,
                    inner: outer.inset(1),
                }
            })
            .collect();
        match self.zoomed {
            Some(z) if self.layout.contains(z) => Placement {
                area,
                tiled: vec![(z, area)],
                separators: Vec::new(),
                floats,
                zoomed: Some(z),
            },
            Some(z) => Placement {
                area,
                tiled: Vec::new(),
                separators: Vec::new(),
                // Only the zoomed float.
                floats: floats.into_iter().filter(|f| f.pane == z).collect(),
                zoomed: Some(z),
            },
            None => {
                let geo = self.layout.geometry(area);
                Placement {
                    area,
                    tiled: geo.panes,
                    separators: geo.separators,
                    floats,
                    zoomed: None,
                }
            }
        }
    }
}

/// The set of workspaces a client looks at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub workspaces: Vec<Workspace>,
    active: usize,
    last: Option<usize>,
}

impl Session {
    pub fn new(name: impl Into<String>, first: Workspace) -> Self {
        Self {
            name: name.into(),
            workspaces: vec![first],
            active: 0,
            last: None,
        }
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The previously active workspace (tmux `-` flag).
    pub fn last_index(&self) -> Option<usize> {
        self.last.filter(|l| *l < self.workspaces.len())
    }

    pub fn active(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    /// Appends a workspace and switches to it. Returns its index.
    pub fn add(&mut self, ws: Workspace) -> usize {
        self.workspaces.push(ws);
        let idx = self.workspaces.len() - 1;
        self.select(idx);
        idx
    }

    pub fn select(&mut self, idx: usize) -> bool {
        if idx >= self.workspaces.len() {
            return false;
        }
        if idx != self.active {
            self.last = Some(self.active);
            self.active = idx;
        }
        true
    }

    pub fn next(&mut self) {
        let n = self.workspaces.len();
        self.select((self.active + 1) % n);
    }

    pub fn prev(&mut self) {
        let n = self.workspaces.len();
        self.select((self.active + n - 1) % n);
    }

    /// Back to the previously active workspace.
    pub fn select_last(&mut self) -> bool {
        match self.last {
            Some(l) if l < self.workspaces.len() => self.select(l),
            _ => false,
        }
    }

    /// Removes a workspace and returns it (the caller closes its panes).
    /// The last workspace cannot be removed this way.
    pub fn remove(&mut self, idx: usize) -> Option<Workspace> {
        if idx >= self.workspaces.len() || self.workspaces.len() == 1 {
            return None;
        }
        let ws = self.workspaces.remove(idx);
        let fix = |i: usize| if i > idx { i - 1 } else { i };
        if self.active == idx {
            let target = self
                .last
                .filter(|l| *l != idx)
                .map_or(idx.min(self.workspaces.len() - 1), fix);
            self.active = target;
            self.last = None;
        } else {
            self.active = fix(self.active);
            self.last = self.last.filter(|l| *l != idx).map(fix);
        }
        Some(ws)
    }

    /// Index of the workspace containing `pane`.
    pub fn find(&self, pane: PaneId) -> Option<usize> {
        self.workspaces.iter().position(|w| w.contains(pane))
    }

    pub fn all_panes(&self) -> Vec<PaneId> {
        self.workspaces.iter().flat_map(Workspace::panes).collect()
    }

    /// Repairs a session read from a file (snapshots may be old, edited or
    /// damaged): duplicate panes, bad split weights, focus on missing
    /// panes, empty workspaces, indices out of range. Returns `false` if
    /// no pane is left.
    pub fn sanitize(&mut self) -> bool {
        let mut seen = std::collections::HashSet::new();
        for ws in &mut self.workspaces {
            ws.sanitize(&mut seen);
        }
        self.drop_empty()
    }

    /// Renames panes (all at once) and drops those `f` maps to `None`,
    /// then removes workspaces left empty. Returns `false` if no pane is
    /// left.
    pub fn remap_panes(&mut self, mut f: impl FnMut(PaneId) -> Option<PaneId>) -> bool {
        for ws in &mut self.workspaces {
            ws.remap_panes(&mut f);
        }
        self.drop_empty()
    }

    fn drop_empty(&mut self) -> bool {
        let keep: Vec<bool> = self.workspaces.iter().map(|w| !w.is_empty()).collect();
        let new_index = |i: usize| keep[..i].iter().filter(|k| **k).count();
        let active = self.active.min(self.workspaces.len().saturating_sub(1));
        self.active = if keep.get(active) == Some(&true) {
            new_index(active)
        } else {
            0
        };
        self.last = self
            .last
            .filter(|l| keep.get(*l) == Some(&true))
            .map(new_index);
        let mut i = 0;
        self.workspaces.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        if self.last == Some(self.active) {
            self.last = None;
        }
        !self.workspaces.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn p(n: u64) -> PaneId {
        PaneId(n)
    }

    #[test]
    fn split_focuses_new_pane_and_remove_refocuses() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        assert_eq!(ws.focus(), Some(p(2)));
        ws.add_split(p(3), SplitHow::Down, AREA);
        assert_eq!(ws.last_focus(), Some(p(2)));
        ws.remove(p(3));
        assert_eq!(ws.focus(), Some(p(2)));
        ws.remove(p(2));
        assert_eq!(ws.focus(), Some(p(1)));
        ws.remove(p(1));
        assert!(ws.is_empty());
        assert_eq!(ws.focus(), None);
    }

    #[test]
    fn float_toggle_round_trip() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        assert!(ws.toggle_float(p(2), AREA));
        assert!(ws.is_floating(p(2)));
        assert_eq!(ws.layout.panes(), vec![p(1)]);
        let pl = ws.placement(AREA);
        assert_eq!(pl.tiled, vec![(p(1), AREA)]);
        assert_eq!(pl.floats.len(), 1);
        assert_eq!(pl.floats[0].inner, pl.floats[0].outer.inset(1));
        ws.toggle_float(p(2), AREA);
        assert!(!ws.is_floating(p(2)));
        assert_eq!(ws.layout.panes().len(), 2);
    }

    #[test]
    fn splits_refuse_when_there_is_no_space() {
        let tiny = Rect::new(0, 0, 4, 2);
        let mut ws = Workspace::new("w", p(1));
        assert!(ws.add_split(p(2), SplitHow::Right, tiny));
        assert!(!ws.add_split(p(3), SplitHow::Right, tiny));
        assert!(!ws.add_split(p(3), SplitHow::Down, tiny));
        assert_eq!(ws.panes(), vec![p(1), p(2)]);
    }

    #[test]
    fn focusing_a_float_raises_it() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_float(p(2), AREA);
        ws.add_float(p(3), AREA);
        assert_eq!(ws.floats.last().map(|f| f.pane), Some(p(3)));
        ws.set_focus(p(2));
        assert_eq!(ws.floats.last().map(|f| f.pane), Some(p(2)));
    }

    #[test]
    fn minimize_hides_and_restores() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_float(p(2), AREA);
        ws.toggle_minimize(p(2));
        assert_eq!(ws.minimized(), vec![p(2)]);
        assert_eq!(ws.focus(), Some(p(1)));
        assert!(ws.placement(AREA).floats.is_empty());
        ws.toggle_minimize(p(2));
        assert_eq!(ws.focus(), Some(p(2)));
    }

    #[test]
    fn zoom_shows_only_the_focused_pane() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        ws.toggle_zoom();
        let pl = ws.placement(AREA);
        assert_eq!(pl.tiled, vec![(p(2), AREA)]);
        assert!(pl.separators.is_empty());
        // Focusing another pane unzooms (tmux behaviour).
        ws.set_focus(p(1));
        assert_eq!(ws.zoomed(), None);
    }

    #[test]
    fn floats_stay_on_screen_and_respect_minimum_size() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_float(p(2), AREA);
        ws.set_float_rect(p(2), Rect::new(75, 22, 3, 1), AREA);
        let r = ws.float_rect(p(2)).expect("float");
        assert!(r.w >= MIN_FLOAT_W && r.h >= MIN_FLOAT_H);
        assert!(r.right() <= AREA.right() && r.bottom() <= AREA.bottom());
        ws.snap(p(2), Snap::RightHalf, AREA);
        assert_eq!(ws.float_rect(p(2)), Some(Rect::new(40, 0, 40, 24)));
    }

    #[test]
    fn cycling_and_swapping() {
        let mut ws = Workspace::new("w", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        ws.add_split(p(3), SplitHow::Right, AREA);
        ws.focus_cycle(true);
        assert_eq!(ws.focus(), Some(p(1)));
        ws.focus_cycle(false);
        assert_eq!(ws.focus(), Some(p(3)));
        ws.swap_cycle(true);
        assert_eq!(ws.layout.panes(), vec![p(3), p(2), p(1)]);
    }

    #[test]
    fn session_workspace_navigation() {
        let mut s = Session::new("main", Workspace::new("a", p(1)));
        s.add(Workspace::new("b", p(2)));
        s.add(Workspace::new("c", p(3)));
        assert_eq!(s.active_index(), 2);
        s.next();
        assert_eq!(s.active_index(), 0);
        assert!(s.select_last());
        assert_eq!(s.active_index(), 2);
        s.prev();
        assert_eq!(s.active_index(), 1);
        assert_eq!(s.find(p(3)), Some(2));
        let removed = s.remove(1).expect("removable");
        assert_eq!(removed.name, "b");
        assert_eq!(s.active().name, "c");
        assert!(s.remove(5).is_none());
        s.remove(0);
        assert!(s.remove(0).is_none(), "last workspace stays");
    }

    #[test]
    fn remap_renames_panes_and_drops_unmapped_ones() {
        let mut ws = Workspace::new("a", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        ws.add_float(p(3), AREA);
        let mut s = Session::new("s", ws);
        s.add(Workspace::new("b", p(4)));
        s.select(1);
        // 1 -> 2 and 2 -> 1 at once (no collisions), 3 and 4 dropped.
        let ok = s.remap_panes(|old| match old.0 {
            1 => Some(p(2)),
            2 => Some(p(1)),
            _ => None,
        });
        assert!(ok);
        assert_eq!(s.workspaces.len(), 1, "the emptied workspace is gone");
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.all_panes(), vec![p(2), p(1)]);
        assert!(s.active().focus().is_some());
        assert!(!s.remap_panes(|_| None), "nothing left");
    }

    #[test]
    fn sanitize_repairs_damaged_sessions() {
        let mut ws = Workspace::new("a", p(1));
        ws.add_split(p(2), SplitHow::Right, AREA);
        let mut s = Session::new("s", ws);
        s.add(Workspace::new("dup", p(2)));
        let mut json = serde_json::to_value(&s).expect("serialize");
        json["active"] = serde_json::json!(7);
        json["workspaces"][0]["focus"] = serde_json::json!(99);
        let children = &mut json["workspaces"][0]["layout"]["root"]["Split"]["children"];
        children[0][1] = serde_json::json!(-3.0);
        let mut s: Session = serde_json::from_value(json).expect("deserialize");
        assert!(s.sanitize());
        assert_eq!(
            s.workspaces.len(),
            1,
            "the duplicate-only workspace is gone"
        );
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.all_panes(), vec![p(1), p(2)]);
        assert!(s.active().focus().is_some_and(|f| s.active().contains(f)));
        let placement = s.active().placement(AREA);
        assert!(placement.tiled.iter().all(|(_, r)| r.w > 0));
    }
}
