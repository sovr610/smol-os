// SPDX-License-Identifier: GPL-3.0-or-later
//! The wallpaper (D-087, D-089): what a workspace shows where it has no
//! tiled panes. A quiet dot pattern with the TermOS logo, a hint, and
//! shortcuts to click; or a picture, with the shortcuts beneath it. Also
//! the logo for the splash and the About window.

use termos_render::cell::{Attrs, Cell, Style};
use termos_render::color::Color;
use termos_render::grid::{BorderKind, Grid, Rect};
use termos_render::icons::Icon;
use termos_render::theme::Theme;

use crate::commands::Command;
use crate::ui::width;

/// The logo, in half blocks: three rows, 25 columns.
pub const LOGO: [&str; 3] = [
    "▀█▀ █▀▀ █▀█ █▀▄▀█ █▀█ █▀▀",
    " █  █▀▀ █▀▄ █ ▀ █ █ █ ▀▀█",
    " ▀  ▀▀▀ ▀ ▀ ▀   ▀ ▀▀▀ ▀▀▀",
];

/// The logo's width in columns.
pub const LOGO_W: u16 = 25;

/// A shortcut on the wallpaper: an icon, a label and what a click runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub icon: Icon,
    pub label: &'static str,
    pub command: Command,
}

/// The shortcuts every wallpaper offers.
pub fn shortcuts() -> Vec<Shortcut> {
    let s = |icon, label, command| Shortcut {
        icon,
        label,
        command,
    };
    vec![
        // A window on the desktop, so the desktop stays.
        s(Icon::Terminal, "Terminal", Command::NewFloat),
        s(Icon::Files, "Files", Command::FilesOpen(String::new())),
        s(Icon::Monitor, "Monitor", Command::MonitorOpen),
        s(Icon::Agent, "AI", Command::AgentOpen(String::new())),
        s(Icon::Settings, "Settings", Command::SettingsOpen),
        s(Icon::Help, "Help", Command::Tutorial),
    ]
}

/// Draws the logo with its top-left corner at (`x`, `y`): the accent
/// shading into the error colour in truecolor, the accent alone
/// otherwise. Only the first `reveal` (0 to 1) of its columns show.
pub fn logo(grid: &mut Grid, x: u16, y: u16, theme: &Theme, reveal: f32) {
    let shown = (f32::from(LOGO_W) * reveal.clamp(0.0, 1.0)).round();
    // In range: a fraction of LOGO_W.
    let shown = shown as u16;
    let (from, to) = (
        theme.rgb(theme.colors.accent),
        theme.rgb(theme.colors.error),
    );
    for (dy, row) in (0u16..).zip(LOGO) {
        for (dx, ch) in (0u16..).zip(row.chars()) {
            if dx >= shown || ch == ' ' {
                continue;
            }
            let fg = if theme.gradients {
                Color::Rgb(from.blend(to, f32::from(dx) / f32::from(LOGO_W - 1)))
            } else {
                theme.colors.accent
            };
            let Some(cell) = grid.get_mut(x + dx, y + dy) else {
                continue;
            };
            let bg = cell.style.bg;
            *cell = Cell::new(ch, Style::new().fg(fg).bg(bg).attrs(Attrs::BOLD));
        }
    }
}

/// The background: in truecolor a soft diagonal glow of the accent over the
/// theme's background, and in every case a sparse lattice of dots.
fn pattern(grid: &mut Grid, area: Rect, theme: &Theme) {
    let base = theme.palette.bg;
    let glow = base.blend(theme.rgb(theme.colors.accent), 0.10);
    let span = f32::from(area.w.max(1)) + 2.0 * f32::from(area.h.max(1));
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let bg = if theme.gradients {
                let t = (f32::from(x - area.x) + 2.0 * f32::from(y - area.y)) / span;
                Color::Rgb(base.blend(glow, t))
            } else {
                Color::Default
            };
            let dot = (y - area.y) % 2 == 1 && (x - area.x + 2 * ((y - area.y) / 2 % 2)) % 4 == 2;
            let ch = if dot { '·' } else { ' ' };
            if let Some(cell) = grid.get_mut(x, y) {
                *cell = Cell::new(ch, Style::new().fg(theme.colors.border).bg(bg));
            }
        }
    }
}

/// Takes the dots out of `rect` (and a margin around it), so what is drawn
/// there stands clear of the pattern.
fn clearing(grid: &mut Grid, area: Rect, rect: Rect) {
    let x0 = rect.x.saturating_sub(2).max(area.x);
    let x1 = (rect.right() + 2).min(area.right());
    let y0 = rect.y.saturating_sub(1).max(area.y);
    let y1 = (rect.bottom() + 1).min(area.bottom());
    for y in y0..y1 {
        for x in x0..x1 {
            if let Some(cell) = grid.get_mut(x, y) {
                let style = cell.style;
                *cell = Cell::blank(style);
            }
        }
    }
}

fn label(theme: &Theme, s: &Shortcut) -> String {
    let g = s.icon.glyph(theme.icons);
    if g.is_empty() {
        format!(" {} ", s.label)
    } else {
        format!(" {g} {} ", s.label)
    }
}

/// The width of the shortcut column.
fn column_width(theme: &Theme, items: &[Shortcut]) -> u16 {
    items
        .iter()
        .map(|s| width(&label(theme, s)))
        .max()
        .unwrap_or(0)
}

/// The shortcuts in a column at the top left, like icons on a desktop
/// (a floating window opens in the middle, so they stay in sight). The
/// one under `pointer` is highlighted. Returns where each one is.
fn shortcut_column(
    grid: &mut Grid,
    area: Rect,
    theme: &Theme,
    items: &[Shortcut],
    pointer: Option<(u16, u16)>,
) -> Vec<(Rect, Command)> {
    let w = column_width(theme, items);
    let x = area.x + 2;
    if x + w > area.right() {
        return Vec::new();
    }
    let mut regions = Vec::new();
    for (i, s) in (0u16..).zip(items) {
        // One row each, with a row between; as many as fit.
        let y = area.y + 1 + 2 * i;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(x, y, w, 1);
        let hover = pointer.is_some_and(|(px, py)| rect.contains(px, py));
        let style = if hover {
            theme.status_tab_active
        } else {
            Style::new().fg(theme.colors.fg).bg(theme.colors.overlay_bg)
        };
        grid.fill(rect, &Cell::blank(style));
        grid.put_str(x, y, &label(theme, s), style, rect.right());
        if hover {
            let (from, to) = theme.sheen(theme.colors.accent, theme.colors.status_bg);
            theme.paint(grid, rect, from, to);
        }
        regions.push((rect, s.command.clone()));
    }
    regions
}

/// What [`draw`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look<'a> {
    /// The pattern, with the logo and `hint` at the bottom right.
    Pattern { hint: &'a str },
    /// A picture (drawn by the image layer) right of the shortcuts.
    Picture,
}

/// The picture's part of `area`, for [`Look::Picture`]: right of the
/// shortcut column.
pub fn picture_area(area: Rect, theme: &Theme) -> Rect {
    let left = (column_width(theme, &shortcuts()) + 4).min(area.w);
    Rect::new(area.x + left, area.y, area.w - left, area.h)
}

/// Draws the wallpaper into `area`; returns where the shortcuts are.
pub fn draw(
    grid: &mut Grid,
    area: Rect,
    theme: &Theme,
    look: Look<'_>,
    pointer: Option<(u16, u16)>,
) -> Vec<(Rect, Command)> {
    pattern(grid, area, theme);
    let items = shortcuts();
    let col_w = column_width(theme, &items);
    let rows = u16::try_from(items.len() * 2).unwrap_or(u16::MAX);
    clearing(
        grid,
        area,
        Rect::new(area.x + 2, area.y + 1, col_w, rows.saturating_sub(1)),
    );
    if let Look::Pattern { hint } = look {
        // The logo and the hint at the bottom right, clear of the column.
        if area.w >= col_w + LOGO_W + 8 && area.h >= 6 {
            let x = area.right() - LOGO_W - 3;
            let y = area.bottom() - 5;
            clearing(grid, area, Rect::new(x, y, LOGO_W, 3));
            logo(grid, x, y, theme, 1.0);
        }
        let hw = width(hint);
        if !hint.is_empty() && area.h >= 2 && area.w >= col_w + hw + 8 {
            let x = area.right() - hw - 3;
            let y = area.bottom() - 1;
            clearing(grid, area, Rect::new(x, y, hw, 1));
            for (cx, ch) in (x..).zip(hint.chars()) {
                if let Some(cell) = grid.get_mut(cx, y) {
                    let bg = cell.style.bg;
                    *cell = Cell::new(ch, Style::new().fg(theme.colors.muted).bg(bg));
                }
            }
        }
    }
    shortcut_column(grid, area, theme, &items, pointer)
}

/// The splash (D-089): the logo on a card in the middle of `screen`,
/// drawn in from the left over the first half of `progress` (0 to 1).
/// Returns the card's rectangle (images stay out of it).
pub fn splash(
    grid: &mut Grid,
    screen: Rect,
    theme: &Theme,
    border: BorderKind,
    progress: f32,
) -> Option<Rect> {
    let (w, h) = (LOGO_W + 8, 7);
    if screen.w < w || screen.h < h {
        return None;
    }
    let card = Rect::new(
        screen.x + (screen.w - w) / 2,
        screen.y + (screen.h - h) / 2,
        w,
        h,
    );
    grid.fill(card, &Cell::blank(Style::new().bg(theme.colors.overlay_bg)));
    grid.draw_box(
        card,
        border,
        theme.float_border.bg(theme.colors.overlay_bg),
        None,
    );
    logo(grid, card.x + 4, card.y + 2, theme, progress * 2.0);
    let tagline = "a terminal-first desktop";
    let tw = width(tagline);
    grid.put_str(
        card.x + (w - tw) / 2,
        card.y + 5,
        tagline,
        Style::new()
            .fg(theme.colors.muted)
            .bg(theme.colors.overlay_bg),
        card.right() - 1,
    );
    Some(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_render::icons::IconSet;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn the_splash_draws_the_logo_in() {
        let screen = Rect::new(0, 0, 60, 16);
        let mut early = Grid::new(60, 16);
        let card = splash(
            &mut early,
            screen,
            &Theme::default(),
            BorderKind::Rounded,
            0.1,
        )
        .expect("room");
        let mut done = Grid::new(60, 16);
        splash(
            &mut done,
            screen,
            &Theme::default(),
            BorderKind::Rounded,
            0.8,
        );
        let blocks = |g: &Grid| g.to_text().matches('█').count();
        assert!(blocks(&early) < blocks(&done), "drawn in from the left");
        insta::assert_snapshot!(done.to_text());
        assert_eq!((card.w, card.h), (LOGO_W + 8, 7));
        // Too small: no splash.
        let mut tiny = Grid::new(20, 5);
        let none = splash(
            &mut tiny,
            Rect::new(0, 0, 20, 5),
            &Theme::default(),
            BorderKind::Rounded,
            1.0,
        );
        assert!(none.is_none());
    }

    #[test]
    fn the_logo_is_as_wide_as_it_says() {
        for row in LOGO {
            assert_eq!(UnicodeWidthStr::width(row), usize::from(LOGO_W));
        }
    }

    #[test]
    fn pattern_logo_and_shortcuts() {
        let area = Rect::new(0, 0, 70, 16);
        let mut g = Grid::new(70, 16);
        let theme = Theme::default();
        let regions = draw(
            &mut g,
            area,
            &theme,
            Look::Pattern {
                hint: "Right-click anywhere for the menu",
            },
            None,
        );
        insta::assert_snapshot!(g.to_text());
        assert_eq!(regions.len(), shortcuts().len());
        // Each shortcut is where it says, and runs its command.
        let (rect, command) = &regions[1];
        assert_eq!(*command, Command::FilesOpen(String::new()));
        assert!(g.row_text(rect.y).contains("Files"));
    }

    #[test]
    fn gradients_and_hover_in_truecolor() {
        let area = Rect::new(0, 0, 60, 14);
        let theme = Theme {
            gradients: true,
            icons: IconSet::Ascii,
            ..Theme::default()
        };
        let mut g = Grid::new(60, 14);
        let regions = draw(&mut g, area, &theme, Look::Pattern { hint: "" }, None);
        let (r, _) = regions[0];
        let mut h = Grid::new(60, 14);
        draw(
            &mut h,
            area,
            &theme,
            Look::Pattern { hint: "" },
            Some((r.x + 1, r.y)),
        );
        let style = |g: &Grid| g.get(r.x + 1, r.y).map(|c| c.style);
        assert_ne!(
            style(&g),
            style(&h),
            "the shortcut under the pointer lights up"
        );
        // The background glows from the top-left to the bottom-right.
        let bg = |x, y| g.get(x, y).map(|c| c.style.bg);
        assert_ne!(bg(0, 0), bg(59, 13));
        // The logo shades from one colour to another.
        let fgs: std::collections::BTreeSet<String> = (0..60)
            .filter_map(|x| g.get(x, 9).filter(|c| c.symbol == "▀"))
            .map(|c| c.style.fg.to_string())
            .collect();
        assert!(fgs.len() > 3, "{fgs:?}");
    }

    #[test]
    fn too_small_for_the_logo_still_has_shortcuts() {
        let theme = Theme::default();
        let mut g = Grid::new(40, 5);
        let regions = draw(
            &mut g,
            Rect::new(0, 0, 40, 5),
            &theme,
            Look::Pattern { hint: "hint" },
            None,
        );
        assert!(!g.to_text().contains('█'));
        assert_eq!(regions.len(), 2, "as many as fit");
        let area = Rect::new(0, 0, 80, 20);
        let mut g = Grid::new(80, 20);
        let regions = draw(&mut g, area, &theme, Look::Picture, None);
        let room = picture_area(area, &theme);
        assert!(
            regions.iter().all(|(r, _)| r.right() < room.x),
            "left of the picture"
        );
        assert_eq!((room.right(), room.h), (80, 20));
    }
}
