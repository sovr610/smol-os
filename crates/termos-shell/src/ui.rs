// SPDX-License-Identifier: GPL-3.0-or-later
//! Small drawing helpers shared by the overlays.

use termos_render::cell::Cell;
use termos_render::grid::{BorderKind, Grid, Rect};
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

/// A `w`×`h` rectangle centred in `area` (shrunk to fit).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.w);
    let h = h.min(area.h);
    Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w, h)
}

/// Draws an overlay panel (filled, rounded border, title) and returns its
/// inner rectangle.
pub fn panel(grid: &mut Grid, rect: Rect, theme: &Theme, title: &str) -> Rect {
    grid.fill(rect, &Cell::blank(theme.overlay));
    grid.draw_box(
        rect,
        BorderKind::Rounded,
        theme.overlay_border,
        (!title.is_empty()).then_some((title, theme.overlay_title)),
    );
    rect.inset(1)
}

/// Display width of a string in cells.
pub fn width(s: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(s)).unwrap_or(u16::MAX)
}

/// The first `max` cells of `s` (no ellipsis; a wide character that does
/// not fit is left out).
pub fn cut(s: &str, max: u16) -> String {
    let mut out = String::new();
    let mut w = 0u16;
    for ch in s.chars() {
        let cw = width(ch.encode_utf8(&mut [0u8; 4]));
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// Truncates `s` to at most `max` cells, ending in `…` if cut.
pub fn truncate(s: &str, max: u16) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0u16;
    for ch in s.chars() {
        let cw = width(ch.encode_utf8(&mut [0u8; 4]));
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centering_and_truncation() {
        assert_eq!(
            centered(Rect::new(0, 0, 10, 10), 4, 2),
            Rect::new(3, 4, 4, 2)
        );
        assert_eq!(
            centered(Rect::new(0, 0, 3, 3), 10, 10),
            Rect::new(0, 0, 3, 3)
        );
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("界界界", 4), "界…");
    }
}
