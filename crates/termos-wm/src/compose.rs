// SPDX-License-Identifier: GPL-3.0-or-later
//! Drawing a workspace: tiled panes with tmux-style separators, then
//! floating windows with rounded borders, title bars and drop shadows.

use termos_core::ids::PaneId;
use termos_render::cell::{Cell, Style};
use termos_render::grid::{BorderKind, Grid, Rect};
use termos_render::theme::Theme;

use crate::hit::{CLOSE_OFFSET, MINIMIZE_OFFSET};
use crate::workspace::Placement;

/// Appearance options from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeOptions {
    pub float_border: BorderKind,
    pub shadow: bool,
}

impl Default for ComposeOptions {
    fn default() -> Self {
        Self {
            float_border: BorderKind::Rounded,
            shadow: true,
        }
    }
}

/// Picks the box-drawing glyph that joins a separator cell to its
/// separator neighbours (up, down, left, right).
fn junction(u: bool, d: bool, l: bool, r: bool) -> char {
    match (u, d, l, r) {
        (true, true, true, true) => '┼',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (_, _, false, false) => '│',
        _ => '─',
    }
}

/// Darkens the strips right of and below `outer` (within `bounds`), as
/// if it floated above what is behind it.
pub fn drop_shadow(grid: &mut Grid, outer: Rect, bounds: Rect, theme: &Theme) {
    let right = Rect::new(outer.right(), outer.y.saturating_add(1), 2, outer.h);
    let bottom = Rect::new(outer.x.saturating_add(2), outer.bottom(), outer.w, 1);
    for strip in [right, bottom] {
        let strip = strip.intersect(&bounds);
        grid.shade(strip, &theme.palette, theme.shadow, theme.shadow_amount);
    }
}

/// Draws the workspace. `content` draws a pane's content into its rectangle;
/// `title` names a floating window; `ring` gives the style of a ring
/// around a pane that wants the user (D-115), drawn over the focus colour.
#[allow(clippy::too_many_arguments)]
pub fn compose(
    grid: &mut Grid,
    placement: &Placement,
    focus: Option<PaneId>,
    theme: &Theme,
    opts: &ComposeOptions,
    title: &dyn Fn(PaneId) -> String,
    ring: &dyn Fn(PaneId) -> Option<Style>,
    content: &mut dyn FnMut(PaneId, Rect, &mut Grid),
) {
    for (pane, rect) in &placement.tiled {
        content(*pane, *rect, grid);
    }
    draw_separators(grid, placement, focus, theme, ring);
    for f in &placement.floats {
        let focused = focus == Some(f.pane);
        if opts.shadow && placement.zoomed != Some(f.pane) {
            drop_shadow(grid, f.outer, placement.area, theme);
        }
        grid.fill(f.outer, &Cell::blank(Style::new()));
        content(f.pane, f.inner, grid);
        let border = match ring(f.pane) {
            Some(style) => style,
            None if focused => theme.float_border,
            None => theme.border,
        };
        let title_style = if focused {
            theme.float_title
        } else {
            theme.border
        };
        let name = title(f.pane);
        grid.draw_box(
            f.outer,
            opts.float_border,
            border,
            Some((&name, title_style)),
        );
        if focused && f.outer.w > 4 {
            // A filled title bar (D-088): the accent, with a sheen where
            // the terminal shows truecolor.
            let bar = Rect::new(f.outer.x + 1, f.outer.y, f.outer.w - 2, 1);
            let text = theme.title_text();
            grid.fill(bar, &Cell::blank(text));
            grid.put_str(
                bar.x + 1,
                bar.y,
                &name,
                text,
                f.outer.right().saturating_sub(2),
            );
            if f.outer.w > 10 {
                let y = f.outer.y;
                grid.put_str(
                    f.outer.right() - MINIMIZE_OFFSET,
                    y,
                    "–",
                    text,
                    f.outer.right(),
                );
                grid.put_str(
                    f.outer.right() - CLOSE_OFFSET,
                    y,
                    "×",
                    text,
                    f.outer.right(),
                );
            }
            let (from, to) = theme.title_bar();
            theme.paint(grid, bar, from, to);
        } else if f.outer.w > 10 {
            let y = f.outer.y;
            grid.put_str(
                f.outer.right() - MINIMIZE_OFFSET,
                y,
                "–",
                border,
                f.outer.right(),
            );
            grid.put_str(
                f.outer.right() - CLOSE_OFFSET,
                y,
                "×",
                border,
                f.outer.right(),
            );
        }
    }
}

fn draw_separators(
    grid: &mut Grid,
    placement: &Placement,
    focus: Option<PaneId>,
    theme: &Theme,
    ring_of: &dyn Fn(PaneId) -> Option<Style>,
) {
    if placement.separators.is_empty() {
        return;
    }
    let area = placement.area;
    let (w, h) = (usize::from(area.w), usize::from(area.h));
    let mut mask = vec![false; w * h];
    let idx = |x: u16, y: u16| usize::from(y - area.y) * w + usize::from(x - area.x);
    for s in &placement.separators {
        let r = s.rect.intersect(&area);
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                mask[idx(x, y)] = true;
            }
        }
    }
    let is_sep = |x: i32, y: i32| -> bool {
        let (ax, ay) = (i32::from(area.x), i32::from(area.y));
        if x < ax || y < ay || x >= ax + i32::from(area.w) || y >= ay + i32::from(area.h) {
            return false;
        }
        let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) else {
            return false;
        };
        mask[idx(x, y)]
    };
    let around = |r: &Rect| {
        Rect::new(
            r.x.saturating_sub(1),
            r.y.saturating_sub(1),
            r.w.saturating_add(2),
            r.h.saturating_add(2),
        )
    };
    // Cells around the focused tiled pane draw in the active colour.
    let ring = focus
        .and_then(|f| placement.tiled.iter().find(|(p, _)| *p == f))
        .map(|(_, r)| around(r));
    // Panes that want the user ring in their own style, over the focus.
    let marked: Vec<(Rect, Style)> = placement
        .tiled
        .iter()
        .filter_map(|(p, r)| ring_of(*p).map(|s| (around(r), s)))
        .collect();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if !mask[idx(x, y)] {
                continue;
            }
            let (xi, yi) = (i32::from(x), i32::from(y));
            let ch = junction(
                is_sep(xi, yi - 1),
                is_sep(xi, yi + 1),
                is_sep(xi - 1, yi),
                is_sep(xi + 1, yi),
            );
            let active = ring.is_some_and(|r| r.contains(x, y));
            let style = match marked.iter().find(|(r, _)| r.contains(x, y)) {
                Some((_, s)) => *s,
                None if active => theme.border_active,
                None => theme.border,
            };
            grid.set(x, y, Cell::new(ch, style));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Preset;
    use crate::workspace::{Snap, SplitHow, Workspace};

    const AREA: Rect = Rect::new(0, 0, 40, 12);

    /// Fills each pane with a letter derived from its id.
    fn letters(pane: PaneId, rect: Rect, grid: &mut Grid) {
        let ch = char::from(b'a' + u8::try_from(pane.0 % 26).unwrap_or(0));
        grid.fill(rect, &Cell::new(ch, Style::new()));
    }

    fn draw(ws: &Workspace) -> String {
        let mut g = Grid::new(AREA.w, AREA.h);
        let pl = ws.placement(AREA);
        compose(
            &mut g,
            &pl,
            ws.focus(),
            &Theme::default(),
            &ComposeOptions::default(),
            &|p| format!("pane {}", p.0),
            &|_| None,
            &mut letters,
        );
        g.to_text()
    }

    fn three_panes() -> Workspace {
        let mut ws = Workspace::new("w", PaneId(0));
        ws.add_split(PaneId(1), SplitHow::Right, AREA);
        ws.add_split(PaneId(2), SplitHow::Down, AREA);
        ws
    }

    #[test]
    fn junctions_are_joined() {
        assert_eq!(junction(true, true, false, true), '├');
        assert_eq!(junction(false, false, true, true), '─');
        assert_eq!(junction(true, true, false, false), '│');
    }

    #[test]
    fn snapshot_splits() {
        insta::assert_snapshot!("splits", draw(&three_panes()));
    }

    #[test]
    fn snapshot_presets() {
        let mut ws = three_panes();
        ws.add_split(PaneId(3), SplitHow::Down, AREA);
        for preset in Preset::ALL {
            assert!(ws.select_preset(preset, AREA));
            insta::assert_snapshot!(format!("preset-{}", preset.name()), draw(&ws));
        }
    }

    #[test]
    fn snapshot_float_with_shadow() {
        let mut ws = three_panes();
        ws.add_float(PaneId(7), AREA);
        ws.set_float_rect(PaneId(7), Rect::new(6, 2, 24, 7), AREA);
        insta::assert_snapshot!("float", draw(&ws));
        ws.snap(PaneId(7), Snap::LeftHalf, AREA);
        insta::assert_snapshot!("float-snapped-left", draw(&ws));
    }

    #[test]
    fn snapshot_each_theme() {
        use termos_render::theme::{ThemeBase, ThemeConfig};
        let mut ws = three_panes();
        ws.add_float(PaneId(7), AREA);
        ws.set_float_rect(PaneId(7), Rect::new(6, 2, 24, 7), AREA);
        let pl = ws.placement(AREA);
        // Flat title bars (256 colours), and gradients (truecolor, D-088).
        let looks = [
            (ThemeBase::Dark, false),
            (ThemeBase::Light, false),
            (ThemeBase::HighContrast, false),
            (ThemeBase::Dark, true),
            (ThemeBase::Light, true),
            (ThemeBase::HighContrast, true),
        ];
        for (base, gradients) in looks {
            let mut theme = ThemeConfig {
                base,
                ..ThemeConfig::default()
            }
            .theme(None, None);
            theme.gradients = gradients;
            let mut g = Grid::new(AREA.w, AREA.h);
            compose(
                &mut g,
                &pl,
                ws.focus(),
                &theme,
                &ComposeOptions::default(),
                &|p| format!("pane {}", p.0),
                &|_| None,
                &mut letters,
            );
            let name = if gradients {
                format!("theme-{}-gradient", base.name())
            } else {
                format!("theme-{}", base.name())
            };
            insta::assert_snapshot!(name, g.to_styled_text());
        }
    }

    #[test]
    fn snapshot_zoom() {
        let mut ws = three_panes();
        ws.toggle_zoom();
        insta::assert_snapshot!("zoomed", draw(&ws));
    }

    #[test]
    fn active_separators_use_the_accent() {
        let ws = three_panes();
        let mut g = Grid::new(AREA.w, AREA.h);
        let pl = ws.placement(AREA);
        let theme = Theme::default();
        compose(
            &mut g,
            &pl,
            Some(PaneId(0)),
            &theme,
            &ComposeOptions::default(),
            &|_| String::new(),
            &|_| None,
            &mut letters,
        );
        // The vertical separator next to pane 0 is active.
        assert_eq!(g.get(20, 3).map(|c| c.style), Some(theme.border_active));
        // The horizontal one between panes 1 and 2 is not adjacent to pane 0.
        assert_eq!(g.get(30, 6).map(|c| c.style), Some(theme.border));
        // A pane that wants the user rings in its style, over the focus.
        let want = Style::new().fg(theme.colors.warn);
        compose(
            &mut g,
            &pl,
            Some(PaneId(0)),
            &theme,
            &ComposeOptions::default(),
            &|_| String::new(),
            &|p| (p == PaneId(2)).then_some(want),
            &mut letters,
        );
        assert_eq!(g.get(30, 6).map(|c| c.style), Some(want));
        assert_eq!(g.get(20, 9).map(|c| c.style), Some(want));
        assert_eq!(g.get(20, 3).map(|c| c.style), Some(theme.border_active));
    }

    #[test]
    fn shadow_darkens_cells_beside_the_float() {
        let mut ws = Workspace::new("w", PaneId(0));
        ws.add_float(PaneId(1), AREA);
        ws.set_float_rect(PaneId(1), Rect::new(5, 2, 20, 5), AREA);
        let mut g = Grid::new(AREA.w, AREA.h);
        let pl = ws.placement(AREA);
        let theme = Theme::default();
        compose(
            &mut g,
            &pl,
            ws.focus(),
            &theme,
            &ComposeOptions::default(),
            &|_| String::new(),
            &|_| None,
            &mut letters,
        );
        let shaded = g.get(26, 4).map(|c| c.style.bg);
        assert!(matches!(shaded, Some(termos_render::color::Color::Rgb(_))));
        let untouched = g.get(30, 4).map(|c| c.style.bg);
        assert_eq!(untouched, Some(termos_render::color::Color::Default));
    }
}
