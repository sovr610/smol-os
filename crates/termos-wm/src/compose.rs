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
/// `title` names a floating window.
pub fn compose(
    grid: &mut Grid,
    placement: &Placement,
    focus: Option<PaneId>,
    theme: &Theme,
    opts: &ComposeOptions,
    title: &dyn Fn(PaneId) -> String,
    content: &mut dyn FnMut(PaneId, Rect, &mut Grid),
) {
    for (pane, rect) in &placement.tiled {
        content(*pane, *rect, grid);
    }
    draw_separators(grid, placement, focus, theme);
    for f in &placement.floats {
        let focused = focus == Some(f.pane);
        if opts.shadow && placement.zoomed != Some(f.pane) {
            drop_shadow(grid, f.outer, placement.area, theme);
        }
        grid.fill(f.outer, &Cell::blank(Style::new()));
        content(f.pane, f.inner, grid);
        let border = if focused {
            theme.float_border
        } else {
            theme.border
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
        if f.outer.w > 10 {
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

fn draw_separators(grid: &mut Grid, placement: &Placement, focus: Option<PaneId>, theme: &Theme) {
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
    // Cells around the focused tiled pane draw in the active colour.
    let ring = focus
        .and_then(|f| placement.tiled.iter().find(|(p, _)| *p == f))
        .map(|(_, r)| {
            Rect::new(
                r.x.saturating_sub(1),
                r.y.saturating_sub(1),
                r.w.saturating_add(2),
                r.h.saturating_add(2),
            )
        });
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
            let style = if active {
                theme.border_active
            } else {
                theme.border
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
        for base in [ThemeBase::Dark, ThemeBase::Light, ThemeBase::HighContrast] {
            let theme = ThemeConfig {
                base,
                ..ThemeConfig::default()
            }
            .theme(None, None);
            let mut g = Grid::new(AREA.w, AREA.h);
            compose(
                &mut g,
                &pl,
                ws.focus(),
                &theme,
                &ComposeOptions::default(),
                &|p| format!("pane {}", p.0),
                &mut letters,
            );
            insta::assert_snapshot!(format!("theme-{}", base.name()), g.to_styled_text());
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
            &mut letters,
        );
        // The vertical separator next to pane 0 is active.
        assert_eq!(g.get(20, 3).map(|c| c.style), Some(theme.border_active));
        // The horizontal one between panes 1 and 2 is not adjacent to pane 0.
        assert_eq!(g.get(30, 6).map(|c| c.style), Some(theme.border));
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
            &mut letters,
        );
        let shaded = g.get(26, 4).map(|c| c.style.bg);
        assert!(matches!(shaded, Some(termos_render::color::Color::Rgb(_))));
        let untouched = g.get(30, 4).map(|c| c.style.bg);
        assert_eq!(untouched, Some(termos_render::color::Color::Default));
    }
}
