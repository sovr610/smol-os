// SPDX-License-Identifier: GPL-3.0-or-later
//! The tiling tree: tmux-style panes separated by one-cell borders.
//!
//! A [`Layout`] is an n-ary tree of splits. Each child of a split has a
//! weight; geometry distributes the split's cells proportionally (minus the
//! separators between children). Resizing works in cells and stores the
//! resulting cell sizes back as weights, so later screen resizes scale
//! proportionally.

use serde::{Deserialize, Serialize};
use termos_core::ids::PaneId;
use termos_render::grid::Rect;

/// How a split arranges its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dir {
    /// Side by side, left to right (separated by vertical lines).
    Horizontal,
    /// Stacked, top to bottom (separated by horizontal lines).
    Vertical,
}

/// A direction on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Dir {
        match self {
            Direction::Left | Direction::Right => Dir::Horizontal,
            Direction::Up | Direction::Down => Dir::Vertical,
        }
    }
}

/// Built-in arrangements (tmux `select-layout`). Master-stack is
/// `MainVertical`; grid is `Tiled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Preset {
    EvenHorizontal,
    EvenVertical,
    MainVertical,
    MainHorizontal,
    Tiled,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::EvenHorizontal,
        Preset::EvenVertical,
        Preset::MainVertical,
        Preset::MainHorizontal,
        Preset::Tiled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::EvenHorizontal => "even-horizontal",
            Preset::EvenVertical => "even-vertical",
            Preset::MainVertical => "main-vertical",
            Preset::MainHorizontal => "main-horizontal",
            Preset::Tiled => "tiled",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// A node of the tiling tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Leaf(PaneId),
    Split {
        dir: Dir,
        children: Vec<(Node, f64)>,
    },
}

/// A separator line between two children of a split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Separator {
    /// The cells of the line (1 wide or 1 tall).
    pub rect: Rect,
    /// The split's direction (a `Horizontal` split has vertical lines).
    pub dir: Dir,
    /// Child indices from the root to the split.
    pub path: Vec<usize>,
    /// The line sits between children `index` and `index + 1`.
    pub index: usize,
}

/// Pane rectangles and separators for one area.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Geometry {
    pub panes: Vec<(PaneId, Rect)>,
    pub separators: Vec<Separator>,
    /// The area of every split, by path.
    pub splits: Vec<(Vec<usize>, Rect)>,
}

impl Geometry {
    pub fn rect_of(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

/// The tiling tree of a workspace (empty when every pane floats).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Layout {
    root: Option<Node>,
}

/// Distributes `total` cells over `weights` (largest remainder), giving each
/// child at least one cell when possible.
fn distribute(total: u16, weights: &[f64]) -> Vec<u16> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: f64 = weights.iter().map(|w| w.max(0.0)).sum();
    let sum = if sum <= 0.0 { n as f64 } else { sum };
    let total_f = f64::from(total);
    let mut sizes: Vec<u16> = Vec::with_capacity(n);
    let mut remainders: Vec<(usize, f64)> = Vec::with_capacity(n);
    for (i, w) in weights.iter().enumerate() {
        let w = if sum == n as f64 && w.max(0.0) == 0.0 {
            1.0
        } else {
            w.max(0.0)
        };
        let exact = total_f * w / sum;
        let floor = exact.floor();
        // `floor` is within 0..=total, so the conversion cannot fail.
        sizes.push(u16::try_from(floor as i64).unwrap_or(0));
        remainders.push((i, exact - floor));
    }
    let assigned: u32 = sizes.iter().map(|&s| u32::from(s)).sum();
    let mut left = u32::from(total).saturating_sub(assigned);
    remainders.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    for (i, _) in remainders.iter().cycle().take(n * 2) {
        if left == 0 {
            break;
        }
        sizes[*i] += 1;
        left -= 1;
    }
    // Steal from the largest to give empty children one cell.
    if u32::from(total) >= n as u32 {
        for i in 0..n {
            if sizes[i] == 0
                && let Some(big) = (0..n).max_by_key(|&j| sizes[j])
                && sizes[big] > 1
            {
                sizes[big] -= 1;
                sizes[i] += 1;
            }
        }
    }
    sizes
}

/// Like [`distribute`], but every child gets at least `mins[i]` cells when
/// `total` allows it: children whose proportional share is below their
/// minimum are pinned there and the rest is shared by weight.
fn distribute_min(total: u16, weights: &[f64], mins: &[u16]) -> Vec<u16> {
    let n = weights.len();
    let need: u32 = mins.iter().map(|&m| u32::from(m)).sum();
    if n == 0 || u32::from(total) < need {
        return distribute(total, weights);
    }
    let mut pinned: Vec<Option<u16>> = vec![None; n];
    loop {
        let pinned_total: u32 = pinned.iter().flatten().map(|&v| u32::from(v)).sum();
        let free = u32::from(total) - pinned_total;
        let free_weight: f64 = (0..n)
            .filter(|&i| pinned[i].is_none())
            .map(|i| weights[i].max(0.0))
            .sum();
        let mut changed = false;
        for i in 0..n {
            if pinned[i].is_some() {
                continue;
            }
            let share = if free_weight > 0.0 {
                f64::from(free) * weights[i].max(0.0) / free_weight
            } else {
                0.0
            };
            if share < f64::from(mins[i]) {
                pinned[i] = Some(mins[i]);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let pinned_total: u32 = pinned.iter().flatten().map(|&v| u32::from(v)).sum();
    let free = u16::try_from(u32::from(total) - pinned_total).unwrap_or(0);
    let free_idx: Vec<usize> = (0..n).filter(|&i| pinned[i].is_none()).collect();
    let free_weights: Vec<f64> = free_idx.iter().map(|&i| weights[i]).collect();
    let mut free_sizes = distribute(free, &free_weights).into_iter();
    (0..n)
        .map(|i| match pinned[i] {
            Some(v) => v,
            None => free_sizes.next().unwrap_or(0).max(mins[i]),
        })
        .collect()
}

impl Node {
    /// The fewest cells this subtree needs along `axis` (every pane one
    /// cell, plus separators).
    fn min_extent(&self, axis: Dir) -> u16 {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { dir, children } if *dir == axis => {
                let seps = u16::try_from(children.len().saturating_sub(1)).unwrap_or(u16::MAX);
                children
                    .iter()
                    .map(|(c, _)| c.min_extent(axis))
                    .fold(seps, u16::saturating_add)
            }
            Node::Split { children, .. } => children
                .iter()
                .map(|(c, _)| c.min_extent(axis))
                .max()
                .unwrap_or(1),
        }
    }
}

impl Node {
    fn panes_into(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(p) => out.push(*p),
            Node::Split { children, .. } => {
                for (c, _) in children {
                    c.panes_into(out);
                }
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == pane,
            Node::Split { children, .. } => children.iter().any(|(c, _)| c.contains(pane)),
        }
    }

    fn geometry(&self, area: Rect, path: &mut Vec<usize>, out: &mut Geometry) {
        match self {
            Node::Leaf(p) => out.panes.push((*p, area)),
            Node::Split { dir, children } => {
                out.splits.push((path.clone(), area));
                let n = children.len();
                let extent = match dir {
                    Dir::Horizontal => area.w,
                    Dir::Vertical => area.h,
                };
                let seps = u16::try_from(n.saturating_sub(1)).unwrap_or(u16::MAX);
                let avail = extent.saturating_sub(seps);
                let weights: Vec<f64> = children.iter().map(|(_, w)| *w).collect();
                let mins: Vec<u16> = children.iter().map(|(c, _)| c.min_extent(*dir)).collect();
                let sizes = distribute_min(avail, &weights, &mins);
                let mut pos = match dir {
                    Dir::Horizontal => area.x,
                    Dir::Vertical => area.y,
                };
                for (i, ((child, _), size)) in children.iter().zip(sizes).enumerate() {
                    let child_area = match dir {
                        Dir::Horizontal => Rect::new(pos, area.y, size, area.h),
                        Dir::Vertical => Rect::new(area.x, pos, area.w, size),
                    };
                    path.push(i);
                    child.geometry(child_area, path, out);
                    path.pop();
                    pos = pos.saturating_add(size);
                    if i + 1 < n {
                        let rect = match dir {
                            Dir::Horizontal => Rect::new(pos, area.y, 1, area.h),
                            Dir::Vertical => Rect::new(area.x, pos, area.w, 1),
                        };
                        out.separators.push(Separator {
                            rect,
                            dir: *dir,
                            path: path.clone(),
                            index: i,
                        });
                        pos = pos.saturating_add(1);
                    }
                }
            }
        }
    }

    fn at_path_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((&i, rest)) => match self {
                Node::Split { children, .. } => children.get_mut(i)?.0.at_path_mut(rest),
                Node::Leaf(_) => None,
            },
        }
    }

    /// Path (child indices) to the leaf for `pane`.
    fn path_to(&self, pane: PaneId) -> Option<Vec<usize>> {
        match self {
            Node::Leaf(p) => (*p == pane).then(Vec::new),
            Node::Split { children, .. } => children.iter().enumerate().find_map(|(i, (c, _))| {
                c.path_to(pane).map(|mut rest| {
                    rest.insert(0, i);
                    rest
                })
            }),
        }
    }

    /// Replaces single-child splits with their child and merges nested
    /// splits of the same direction.
    fn normalize(self) -> Node {
        match self {
            Node::Leaf(_) => self,
            Node::Split { dir, children } => {
                let mut flat: Vec<(Node, f64)> = Vec::new();
                for (child, w) in children {
                    match child.normalize() {
                        Node::Split {
                            dir: d,
                            children: inner,
                        } if d == dir => {
                            let inner_sum: f64 = inner.iter().map(|(_, iw)| *iw).sum();
                            let inner_sum = if inner_sum <= 0.0 { 1.0 } else { inner_sum };
                            for (n, iw) in inner {
                                flat.push((n, w * iw / inner_sum));
                            }
                        }
                        other => flat.push((other, w)),
                    }
                }
                if flat.len() == 1 {
                    flat.pop().map_or(
                        Node::Split {
                            dir,
                            children: Vec::new(),
                        },
                        |(n, _)| n,
                    )
                } else {
                    Node::Split {
                        dir,
                        children: flat,
                    }
                }
            }
        }
    }
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(pane: PaneId) -> Self {
        Self {
            root: Some(Node::Leaf(pane)),
        }
    }

    pub fn root(&self) -> Option<&Node> {
        self.root.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Panes in index order (left-to-right, top-to-bottom traversal).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            r.panes_into(&mut out);
        }
        out
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(pane))
    }

    pub fn geometry(&self, area: Rect) -> Geometry {
        let mut g = Geometry::default();
        if let Some(r) = &self.root {
            r.geometry(area, &mut Vec::new(), &mut g);
        }
        g
    }

    /// Adds `new` next to `target` (after it), splitting in `dir`. With no
    /// `target` (or an empty layout), `new` becomes the root or is appended
    /// to the root split.
    pub fn split(&mut self, target: Option<PaneId>, new: PaneId, dir: Dir) {
        let Some(root) = self.root.take() else {
            self.root = Some(Node::Leaf(new));
            return;
        };
        let target = target.filter(|t| root.contains(*t));
        let Some(target) = target else {
            self.root = Some(
                Node::Split {
                    dir,
                    children: vec![(root, 1.0), (Node::Leaf(new), 1.0)],
                }
                .normalize(),
            );
            return;
        };
        let mut root = root;
        let path = root.path_to(target).unwrap_or_default();
        if let Some((last, parent_path)) = path.split_last()
            && let Some(Node::Split {
                dir: pdir,
                children,
            }) = root.at_path_mut(parent_path)
            && *pdir == dir
        {
            // Same direction: insert a sibling and halve the target's share.
            let w = children[*last].1 / 2.0;
            children[*last].1 = w;
            children.insert(*last + 1, (Node::Leaf(new), w));
        } else if let Some(slot) = root.at_path_mut(&path) {
            let old = std::mem::replace(slot, Node::Leaf(new));
            *slot = Node::Split {
                dir,
                children: vec![(old, 1.0), (Node::Leaf(new), 1.0)],
            };
        }
        self.root = Some(root.normalize());
    }

    /// Whether splitting `target` in `dir` leaves both halves at least one
    /// cell (plus the separator). Without a target, checks the whole area.
    pub fn split_fits(&self, target: Option<PaneId>, dir: Dir, area: Rect) -> bool {
        let rect = target
            .and_then(|t| self.geometry(area).rect_of(t))
            .unwrap_or(area);
        if self.is_empty() {
            return true;
        }
        match dir {
            Dir::Horizontal => rect.w >= 3,
            Dir::Vertical => rect.h >= 3,
        }
    }

    /// The direction a BSP split of `target` would use: its longer side
    /// (cells are about twice as tall as wide, so width counts half), or
    /// the other side if only that one fits.
    pub fn auto_dir(&self, target: Option<PaneId>, area: Rect) -> Option<Dir> {
        let rect = target
            .and_then(|t| self.geometry(area).rect_of(t))
            .unwrap_or(area);
        let preferred = if rect.w >= rect.h.saturating_mul(2) {
            Dir::Horizontal
        } else {
            Dir::Vertical
        };
        let other = match preferred {
            Dir::Horizontal => Dir::Vertical,
            Dir::Vertical => Dir::Horizontal,
        };
        [preferred, other]
            .into_iter()
            .find(|d| self.split_fits(target, *d, area))
    }

    /// Splits `target` along its longer side (BSP). Terminal cells are about
    /// twice as tall as wide, so width counts half.
    pub fn split_auto(&mut self, target: Option<PaneId>, new: PaneId, area: Rect) {
        let rect = target
            .and_then(|t| self.geometry(area).rect_of(t))
            .unwrap_or(area);
        let dir = if rect.w >= rect.h.saturating_mul(2) {
            Dir::Horizontal
        } else {
            Dir::Vertical
        };
        self.split(target, new, dir);
    }

    /// Removes a pane; its space goes to its neighbours.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        let Some(mut root) = self.root.take() else {
            return false;
        };
        if matches!(root, Node::Leaf(p) if p == pane) {
            return true;
        }
        let Some(path) = root.path_to(pane) else {
            self.root = Some(root);
            return false;
        };
        if let Some((last, parent_path)) = path.split_last()
            && let Some(Node::Split { children, .. }) = root.at_path_mut(parent_path)
        {
            let (_, w) = children.remove(*last);
            // Give the space to the previous sibling, or the next one.
            let heir = last.checked_sub(1).unwrap_or(0);
            if let Some(h) = children.get_mut(heir) {
                h.1 += w;
            }
        }
        self.root = Some(root.normalize());
        true
    }

    /// Swaps two panes' positions.
    pub fn swap(&mut self, a: PaneId, b: PaneId) {
        fn rename(node: &mut Node, a: PaneId, b: PaneId) {
            match node {
                Node::Leaf(p) if *p == a => *p = b,
                Node::Leaf(p) if *p == b => *p = a,
                Node::Leaf(_) => {}
                Node::Split { children, .. } => {
                    for (c, _) in children {
                        rename(c, a, b);
                    }
                }
            }
        }
        if let Some(r) = &mut self.root {
            rename(r, a, b);
        }
    }

    /// Moves the border on the `direction` side of `pane` by `cells`
    /// (tmux `resize-pane -L/-R/-U/-D`): the pane grows toward `direction`
    /// if it has a neighbour there, otherwise its opposite border moves
    /// (the pane shrinks).
    pub fn resize(&mut self, pane: PaneId, direction: Direction, cells: u16, area: Rect) {
        let Some(root) = &self.root else { return };
        let Some(path) = root.path_to(pane) else {
            return;
        };
        let axis = direction.axis();
        let geo = self.geometry(area);
        // Innermost ancestor split along the axis that has a border on the
        // requested side; failing that, one with a border on the other side.
        let mut chosen: Option<(Vec<usize>, usize, i32)> = None;
        for depth in (0..path.len()).rev() {
            let split_path = &path[..depth];
            let child = path[depth];
            let Some(Node::Split { dir, children }) = root_at(root, split_path) else {
                continue;
            };
            if *dir != axis {
                continue;
            }
            let forward = matches!(direction, Direction::Right | Direction::Down);
            let n = children.len();
            let delta = i32::from(cells);
            if forward && child + 1 < n {
                chosen = Some((split_path.to_vec(), child, delta));
                break;
            }
            if !forward && child > 0 {
                chosen = Some((split_path.to_vec(), child - 1, -delta));
                break;
            }
            if chosen.is_none() {
                // Border on the other side: moving it shrinks the pane.
                if forward && child > 0 {
                    chosen = Some((split_path.to_vec(), child - 1, delta));
                } else if !forward && child + 1 < n {
                    chosen = Some((split_path.to_vec(), child, -delta));
                }
            }
        }
        if let Some((split_path, index, delta)) = chosen
            && let Some(sep) = geo
                .separators
                .iter()
                .find(|s| s.path == split_path && s.index == index)
        {
            let pos = match axis {
                Dir::Horizontal => i32::from(sep.rect.x) + delta,
                Dir::Vertical => i32::from(sep.rect.y) + delta,
            };
            let pos = u16::try_from(pos.max(0)).unwrap_or(0);
            self.set_boundary(&split_path, index, pos, area);
        }
    }

    /// Moves separator `index` of the split at `path` to screen coordinate
    /// `pos` (mouse drag), keeping every child at least one cell.
    pub fn set_boundary(&mut self, path: &[usize], index: usize, pos: u16, area: Rect) {
        let geo = self.geometry(area);
        let Some(split_area) = geo
            .splits
            .iter()
            .find(|(p, _)| p.as_slice() == path)
            .map(|(_, r)| *r)
        else {
            return;
        };
        let Some(root) = &mut self.root else { return };
        let Some(Node::Split { dir, children }) = root.at_path_mut(path) else {
            return;
        };
        let n = children.len();
        if index + 1 >= n {
            return;
        }
        let (start, extent) = match dir {
            Dir::Horizontal => (split_area.x, split_area.w),
            Dir::Vertical => (split_area.y, split_area.h),
        };
        let seps = u16::try_from(n - 1).unwrap_or(u16::MAX);
        let weights: Vec<f64> = children.iter().map(|(_, w)| *w).collect();
        let axis = *dir;
        let mins: Vec<u16> = children.iter().map(|(c, _)| c.min_extent(axis)).collect();
        let mut sizes = distribute_min(extent.saturating_sub(seps), &weights, &mins);
        let before: u16 = sizes[..=index].iter().sum();
        let index_u16 = u16::try_from(index).unwrap_or(u16::MAX);
        let cur = start + before + index_u16;
        let lo = cur.saturating_sub(sizes[index].saturating_sub(mins[index]));
        let hi = cur.saturating_add(sizes[index + 1].saturating_sub(mins[index + 1]));
        let target = pos.clamp(lo, hi);
        if target >= cur {
            let d = target - cur;
            sizes[index] += d;
            sizes[index + 1] -= d;
        } else {
            let d = cur - target;
            sizes[index] -= d;
            sizes[index + 1] += d;
        }
        for (((_, w), s), m) in children.iter_mut().zip(sizes).zip(mins) {
            *w = f64::from(s.max(m).max(1));
        }
    }

    /// The pane in `direction` from `from` (tmux `select-pane -L/-R/-U/-D`).
    pub fn neighbor(&self, from: PaneId, direction: Direction, area: Rect) -> Option<PaneId> {
        let geo = self.geometry(area);
        let src = geo.rect_of(from)?;
        let mut best: Option<(i32, i32, PaneId)> = None;
        for (p, r) in &geo.panes {
            if *p == from {
                continue;
            }
            let (dist, overlap) = match direction {
                Direction::Left if r.right() <= src.x => (
                    i32::from(src.x) - i32::from(r.right()),
                    overlap_len(src.y, src.bottom(), r.y, r.bottom()),
                ),
                Direction::Right if r.x >= src.right() => (
                    i32::from(r.x) - i32::from(src.right()),
                    overlap_len(src.y, src.bottom(), r.y, r.bottom()),
                ),
                Direction::Up if r.bottom() <= src.y => (
                    i32::from(src.y) - i32::from(r.bottom()),
                    overlap_len(src.x, src.right(), r.x, r.right()),
                ),
                Direction::Down if r.y >= src.bottom() => (
                    i32::from(r.y) - i32::from(src.bottom()),
                    overlap_len(src.x, src.right(), r.x, r.right()),
                ),
                _ => continue,
            };
            if overlap <= 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((bd, bo, _)) => dist < bd || (dist == bd && overlap > bo),
            };
            if better {
                best = Some((dist, overlap, *p));
            }
        }
        best.map(|(_, _, p)| p)
    }

    /// Rebuilds the tree in a preset arrangement, keeping pane order.
    pub fn apply_preset(&mut self, preset: Preset) {
        let panes = self.panes();
        self.root = build_preset(preset, &panes);
    }

    /// Applies `preset` only if every pane keeps at least one cell in
    /// `area`; returns whether it was applied.
    pub fn try_preset(&mut self, preset: Preset, area: Rect) -> bool {
        let old = self.root.clone();
        self.apply_preset(preset);
        let fits = self
            .geometry(area)
            .panes
            .iter()
            .all(|(_, r)| r.w >= 1 && r.h >= 1);
        if !fits {
            self.root = old;
        }
        fits
    }
}

fn root_at<'a>(node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    match path.split_first() {
        None => Some(node),
        Some((&i, rest)) => match node {
            Node::Split { children, .. } => root_at(&children.get(i)?.0, rest),
            Node::Leaf(_) => None,
        },
    }
}

fn overlap_len(a0: u16, a1: u16, b0: u16, b1: u16) -> i32 {
    i32::from(a1.min(b1)) - i32::from(a0.max(b0))
}

fn leaves(panes: &[PaneId]) -> Vec<(Node, f64)> {
    panes.iter().map(|p| (Node::Leaf(*p), 1.0)).collect()
}

fn build_preset(preset: Preset, panes: &[PaneId]) -> Option<Node> {
    match panes {
        [] => None,
        [one] => Some(Node::Leaf(*one)),
        [main, rest @ ..] => Some(
            match preset {
                Preset::EvenHorizontal => Node::Split {
                    dir: Dir::Horizontal,
                    children: leaves(panes),
                },
                Preset::EvenVertical => Node::Split {
                    dir: Dir::Vertical,
                    children: leaves(panes),
                },
                Preset::MainVertical | Preset::MainHorizontal => {
                    let (outer, inner) = if preset == Preset::MainVertical {
                        (Dir::Horizontal, Dir::Vertical)
                    } else {
                        (Dir::Vertical, Dir::Horizontal)
                    };
                    Node::Split {
                        dir: outer,
                        children: vec![
                            (Node::Leaf(*main), 3.0),
                            (
                                Node::Split {
                                    dir: inner,
                                    children: leaves(rest),
                                },
                                2.0,
                            ),
                        ],
                    }
                }
                Preset::Tiled => {
                    let n = panes.len();
                    let mut cols = 1usize;
                    while cols * cols < n {
                        cols += 1;
                    }
                    let rows: Vec<(Node, f64)> = panes
                        .chunks(cols)
                        .map(|row| {
                            (
                                Node::Split {
                                    dir: Dir::Horizontal,
                                    children: leaves(row),
                                },
                                1.0,
                            )
                        })
                        .collect();
                    Node::Split {
                        dir: Dir::Vertical,
                        children: rows,
                    }
                }
            }
            .normalize(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn p(n: u64) -> PaneId {
        PaneId(n)
    }

    #[test]
    fn distribute_is_exact_and_fair() {
        assert_eq!(distribute(10, &[1.0, 1.0, 1.0]), vec![4, 3, 3]);
        assert_eq!(distribute(10, &[3.0, 2.0]), vec![6, 4]);
        assert_eq!(distribute(3, &[100.0, 1.0, 1.0]), vec![1, 1, 1]);
        assert_eq!(distribute(0, &[1.0, 1.0]), vec![0, 0]);
    }

    #[test]
    fn distribute_min_respects_minimums() {
        assert_eq!(distribute_min(10, &[1.0, 9.0], &[3, 1]), vec![3, 7]);
        assert_eq!(distribute_min(10, &[1.0, 1.0], &[1, 1]), vec![5, 5]);
        // Not enough room: falls back to plain proportional sharing.
        assert_eq!(
            distribute_min(3, &[1.0, 1.0], &[3, 3]),
            distribute(3, &[1.0, 1.0])
        );
    }

    #[test]
    fn split_right_then_down() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.split(Some(p(2)), p(3), Dir::Vertical);
        let g = l.geometry(AREA);
        assert_eq!(g.rect_of(p(1)), Some(Rect::new(0, 0, 40, 24)));
        assert_eq!(g.rect_of(p(2)), Some(Rect::new(41, 0, 39, 12)));
        assert_eq!(g.rect_of(p(3)), Some(Rect::new(41, 13, 39, 11)));
        assert_eq!(g.separators.len(), 2);
        assert_eq!(l.panes(), vec![p(1), p(2), p(3)]);
    }

    #[test]
    fn same_direction_splits_become_siblings() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.split(Some(p(1)), p(3), Dir::Horizontal);
        match l.root() {
            Some(Node::Split { children, .. }) => assert_eq!(children.len(), 3),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(l.panes(), vec![p(1), p(3), p(2)]);
    }

    #[test]
    fn remove_collapses() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.split(Some(p(2)), p(3), Dir::Vertical);
        assert!(l.remove(p(3)));
        // The nested vertical split collapsed back into a plain leaf.
        match l.root() {
            Some(Node::Split { dir, children }) => {
                assert_eq!(*dir, Dir::Horizontal);
                assert!(children.iter().all(|(c, _)| matches!(c, Node::Leaf(_))));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(l.panes(), vec![p(1), p(2)]);
        assert!(l.remove(p(1)));
        assert_eq!(l.root(), Some(&Node::Leaf(p(2))));
        assert!(l.remove(p(2)));
        assert!(l.is_empty());
        assert!(!l.remove(p(9)));
    }

    #[test]
    fn neighbors_by_geometry() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.split(Some(p(2)), p(3), Dir::Vertical);
        assert_eq!(l.neighbor(p(1), Direction::Right, AREA), Some(p(2)));
        assert_eq!(l.neighbor(p(3), Direction::Left, AREA), Some(p(1)));
        assert_eq!(l.neighbor(p(2), Direction::Down, AREA), Some(p(3)));
        assert_eq!(l.neighbor(p(1), Direction::Left, AREA), None);
    }

    #[test]
    fn resize_moves_the_right_border() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.resize(p(1), Direction::Right, 5, AREA);
        assert_eq!(l.geometry(AREA).rect_of(p(1)).map(|r| r.w), Some(45));
        // Right pane has no right neighbour: its left border moves right.
        l.resize(p(2), Direction::Right, 10, AREA);
        assert_eq!(l.geometry(AREA).rect_of(p(1)).map(|r| r.w), Some(55));
        // Never below one cell.
        l.resize(p(1), Direction::Left, 200, AREA);
        assert_eq!(l.geometry(AREA).rect_of(p(1)).map(|r| r.w), Some(1));
    }

    #[test]
    fn resize_in_nested_split() {
        let mut l = Layout::single(p(1));
        l.split(Some(p(1)), p(2), Dir::Horizontal);
        l.split(Some(p(2)), p(3), Dir::Vertical);
        l.resize(p(3), Direction::Up, 3, AREA);
        assert_eq!(l.geometry(AREA).rect_of(p(3)).map(|r| r.h), Some(14));
        l.resize(p(3), Direction::Left, 4, AREA);
        assert_eq!(l.geometry(AREA).rect_of(p(1)).map(|r| r.w), Some(36));
    }

    #[test]
    fn presets() {
        let mut l = Layout::single(p(1));
        for n in 2..=5 {
            l.split(Some(p(1)), p(n), Dir::Horizontal);
        }
        l.apply_preset(Preset::Tiled);
        let g = l.geometry(AREA);
        assert_eq!(g.panes.len(), 5);
        let rows: std::collections::BTreeSet<u16> = g.panes.iter().map(|(_, r)| r.y).collect();
        assert_eq!(rows.len(), 2);
        l.apply_preset(Preset::MainVertical);
        let g = l.geometry(AREA);
        let main = g.rect_of(p(1)).expect("main pane");
        assert_eq!((main.x, main.h), (0, 24));
        assert!(main.w > 40);
        assert_eq!(Preset::from_name("tiled"), Some(Preset::Tiled));
    }

    #[test]
    fn split_auto_picks_longer_side() {
        let mut l = Layout::single(p(1));
        l.split_auto(Some(p(1)), p(2), AREA); // 80x24: wide -> side by side
        assert_eq!(l.geometry(AREA).rect_of(p(2)).map(|r| r.y), Some(0));
        l.split_auto(Some(p(2)), p(3), AREA); // 39x24: tall -> stacked
        assert_eq!(
            l.geometry(AREA).rect_of(p(3)).map(|r| r.x),
            l.geometry(AREA).rect_of(p(2)).map(|r| r.x)
        );
    }

    #[derive(Debug, Clone)]
    enum Op {
        Split(u8, bool),
        Remove(u8),
        Resize(u8, u8, u8),
        Preset(u8),
        Swap(u8, u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (any::<u8>(), any::<bool>()).prop_map(|(t, h)| Op::Split(t, h)),
            1 => any::<u8>().prop_map(Op::Remove),
            2 => (any::<u8>(), 0u8..4, 0u8..30).prop_map(|(t, d, c)| Op::Resize(t, d, c)),
            1 => (0u8..5).prop_map(Op::Preset),
            1 => (any::<u8>(), any::<u8>()).prop_map(|(a, b)| Op::Swap(a, b)),
        ]
    }

    proptest! {
        /// Whatever happens (splits only where they fit, as the workspace
        /// enforces), panes and separators tile the area exactly: no
        /// overlaps, no gaps, every pane at least 1x1.
        #[test]
        fn geometry_tiles_the_area(
            ops in prop::collection::vec(op(), 0..40),
            w in 20u16..200,
            h in 8u16..60,
        ) {
            let area = Rect::new(0, 0, w, h);
            let mut l = Layout::single(p(0));
            let mut next = 1u64;
            for o in ops {
                let panes = l.panes();
                let pick = |i: u8| -> Option<PaneId> {
                    (!panes.is_empty()).then(|| panes[usize::from(i) % panes.len()])
                };
                match o {
                    Op::Split(t, horiz) => {
                        // Keep panes able to have >= 1 cell.
                        let dir = if horiz { Dir::Horizontal } else { Dir::Vertical };
                        if l.split_fits(pick(t), dir, area) {
                            l.split(pick(t), p(next), dir);
                            next += 1;
                        }
                    }
                    Op::Remove(t) => {
                        if let Some(t) = pick(t) {
                            l.remove(t);
                        }
                    }
                    Op::Resize(t, d, c) => {
                        if let Some(t) = pick(t) {
                            let dir = [Direction::Left, Direction::Right, Direction::Up, Direction::Down][usize::from(d)];
                            l.resize(t, dir, u16::from(c), area);
                        }
                    }
                    Op::Preset(i) => {
                        l.try_preset(Preset::ALL[usize::from(i)], area);
                    }
                    Op::Swap(a, b) => {
                        if let (Some(a), Some(b)) = (pick(a), pick(b)) {
                            l.swap(a, b);
                        }
                    }
                }
            }
            let g = l.geometry(area);
            let mut covered = vec![0u8; usize::from(w) * usize::from(h)];
            let mut mark = |r: &Rect| {
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        covered[usize::from(y) * usize::from(w) + usize::from(x)] += 1;
                    }
                }
            };
            for (_, r) in &g.panes {
                prop_assert!(r.w >= 1 && r.h >= 1, "degenerate pane {r:?}");
                prop_assert!(r.right() <= w && r.bottom() <= h, "out of bounds {r:?}");
                mark(r);
            }
            for s in &g.separators {
                mark(&s.rect);
            }
            if !l.is_empty() {
                // Separators may cross at junction cells; panes never overlap
                // anything and every cell is covered.
                prop_assert!(covered.iter().all(|&c| c >= 1), "gap in coverage");
                for (_, r) in &g.panes {
                    for y in r.y..r.bottom() {
                        for x in r.x..r.right() {
                            prop_assert_eq!(covered[usize::from(y) * usize::from(w) + usize::from(x)], 1);
                        }
                    }
                }
            }
            let mut ids = l.panes();
            let len = ids.len();
            ids.sort();
            ids.dedup();
            prop_assert_eq!(ids.len(), len, "duplicate pane ids");
        }
    }
}
