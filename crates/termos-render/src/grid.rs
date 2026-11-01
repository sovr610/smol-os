// SPDX-License-Identifier: GPL-3.0-or-later
//! The compositor surface: a grid of cells with clipped drawing helpers.

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::cell::{Cell, Style, sanitize};
use crate::color::{Color, Palette, Rgb};

/// A rectangle in cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// One past the last column.
    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }

    /// One past the last row.
    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub const fn area(&self) -> u32 {
        self.w as u32 * self.h as u32
    }

    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        if r <= x || b <= y {
            Rect::new(x, y, 0, 0)
        } else {
            Rect::new(x, y, r - x, b - y)
        }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        !self.intersect(other).is_empty()
    }

    /// Shrinks by `n` cells on every side.
    pub fn inset(&self, n: u16) -> Rect {
        let w = self.w.saturating_sub(n.saturating_mul(2));
        let h = self.h.saturating_sub(n.saturating_mul(2));
        Rect::new(self.x.saturating_add(n), self.y.saturating_add(n), w, h)
    }

    /// Moves the rectangle so it lies inside `bounds` as far as possible.
    pub fn clamp_within(&self, bounds: &Rect) -> Rect {
        let w = self.w.min(bounds.w);
        let h = self.h.min(bounds.h);
        let x = self.x.clamp(bounds.x, bounds.right() - w);
        let y = self.y.clamp(bounds.y, bounds.bottom() - h);
        Rect::new(x, y, w, h)
    }
}

/// Border glyph sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderKind {
    Square,
    Rounded,
    Heavy,
    Double,
}

impl BorderKind {
    /// `[horizontal, vertical, top-left, top-right, bottom-left, bottom-right]`.
    pub const fn glyphs(self) -> [char; 6] {
        match self {
            BorderKind::Square => ['─', '│', '┌', '┐', '└', '┘'],
            BorderKind::Rounded => ['─', '│', '╭', '╮', '╰', '╯'],
            BorderKind::Heavy => ['━', '┃', '┏', '┓', '┗', '┛'],
            BorderKind::Double => ['═', '║', '╔', '╗', '╚', '╝'],
        }
    }
}

/// A screen-sized grid of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); usize::from(cols) * usize::from(rows)],
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.cols, self.rows)
    }

    /// Resizes, clearing all content.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells.clear();
        self.cells
            .resize(usize::from(cols) * usize::from(rows), Cell::default());
    }

    fn idx(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.cols && y < self.rows)
            .then(|| usize::from(y) * usize::from(self.cols) + usize::from(x))
    }

    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        self.idx(x, y).map(|i| &self.cells[i])
    }

    /// Mutable access to a cell. Callers that change a cell's width must keep
    /// wide-character pairs consistent; prefer [`Grid::set`].
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.idx(x, y).map(|i| &mut self.cells[i])
    }

    /// One row of cells.
    pub fn row(&self, y: u16) -> &[Cell] {
        let start = usize::from(y) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }

    /// Sets a cell, repairing any wide character it overlaps. A width-2
    /// cell also writes its continuation (or a space if it would not fit).
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        let Some(i) = self.idx(x, y) else { return };
        self.break_wide_at(x, y);
        if cell.width == 2 {
            if x + 1 >= self.cols {
                self.cells[i] = Cell::blank(cell.style);
                return;
            }
            self.break_wide_at(x + 1, y);
            let style = cell.style;
            self.cells[i] = cell;
            self.cells[i + 1] = Cell::continuation(style);
        } else {
            self.cells[i] = cell;
        }
    }

    /// If (x, y) is part of a wide character, blanks both halves.
    fn break_wide_at(&mut self, x: u16, y: u16) {
        let Some(i) = self.idx(x, y) else { return };
        let style = self.cells[i].style;
        if self.cells[i].width == 2 {
            if let Some(next) = self.idx(x + 1, y) {
                self.cells[next] = Cell::blank(style);
            }
        } else if self.cells[i].is_continuation() && x > 0 {
            if let Some(prev) = self.idx(x - 1, y) {
                let prev_style = self.cells[prev].style;
                self.cells[prev] = Cell::blank(prev_style);
            }
            self.cells[i] = Cell::blank(style);
        }
    }

    /// Fills `rect` (clipped) with copies of `cell` (which must be width 1).
    pub fn fill(&mut self, rect: Rect, cell: &Cell) {
        let r = rect.intersect(&self.area());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                self.set(x, y, cell.clone());
            }
        }
    }

    /// Clears the whole grid to blanks in `style`.
    pub fn clear(&mut self, style: Style) {
        let blank = Cell::blank(style);
        for c in &mut self.cells {
            c.clone_from(&blank);
        }
    }

    /// Writes `text` starting at (x, y), never past `max_x` (exclusive) or
    /// the grid edge. Control characters become spaces, graphemes are kept
    /// together, and a wide character that does not fit is replaced by a
    /// space. Returns the column after the last written cell.
    pub fn put_str(&mut self, x: u16, y: u16, text: &str, style: Style, max_x: u16) -> u16 {
        let limit = max_x.min(self.cols);
        if y >= self.rows {
            return x;
        }
        let mut col = x;
        for g in text.graphemes(true) {
            if col >= limit {
                break;
            }
            let width = UnicodeWidthStr::width(g);
            if width == 0 {
                // Zero-width cluster on its own (e.g. a lone combining mark).
                continue;
            }
            if width >= 2 {
                if col + 1 >= limit {
                    self.set(col, y, Cell::blank(style));
                    col += 1;
                    continue;
                }
                let mut cell = Cell::blank(style);
                cell.symbol.clear();
                cell.symbol.extend(g.chars().map(sanitize));
                cell.width = 2;
                self.set(col, y, cell);
                col += 2;
            } else {
                let mut cell = Cell::blank(style);
                cell.symbol.clear();
                cell.symbol.extend(g.chars().map(sanitize));
                self.set(col, y, cell);
                col += 1;
            }
        }
        col
    }

    /// Changes the style of every cell in `rect`, keeping its content.
    pub fn set_style(&mut self, rect: Rect, style: Style) {
        let r = rect.intersect(&self.area());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                if let Some(c) = self.get_mut(x, y) {
                    c.style = style;
                }
            }
        }
    }

    /// Applies `f` to the style of every cell in `rect`.
    pub fn map_style(&mut self, rect: Rect, mut f: impl FnMut(Style) -> Style) {
        let r = rect.intersect(&self.area());
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                if let Some(c) = self.get_mut(x, y) {
                    c.style = f(c.style);
                }
            }
        }
    }

    /// Draws a border around `rect` with an optional title in the top edge.
    pub fn draw_box(
        &mut self,
        rect: Rect,
        kind: BorderKind,
        style: Style,
        title: Option<(&str, Style)>,
    ) {
        if rect.w < 2 || rect.h < 2 {
            return;
        }
        let [h, v, tl, tr, bl, br] = kind.glyphs();
        let right = rect.right() - 1;
        let bottom = rect.bottom() - 1;
        for x in rect.x + 1..right {
            self.set(x, rect.y, Cell::new(h, style));
            self.set(x, bottom, Cell::new(h, style));
        }
        for y in rect.y + 1..bottom {
            self.set(rect.x, y, Cell::new(v, style));
            self.set(right, y, Cell::new(v, style));
        }
        self.set(rect.x, rect.y, Cell::new(tl, style));
        self.set(right, rect.y, Cell::new(tr, style));
        self.set(rect.x, bottom, Cell::new(bl, style));
        self.set(right, bottom, Cell::new(br, style));
        if let Some((text, title_style)) = title
            && rect.w > 4
        {
            let x = rect.x + 2;
            let end = self.put_str(x, rect.y, " ", style, right);
            let end = self.put_str(end, rect.y, text, title_style, right.saturating_sub(1));
            self.put_str(end, rect.y, " ", style, right);
        }
    }

    /// Draws a horizontal line of `ch` from `x` (inclusive) to `x_end` (exclusive).
    pub fn hline(&mut self, x: u16, x_end: u16, y: u16, ch: char, style: Style) {
        for col in x..x_end.min(self.cols) {
            self.set(col, y, Cell::new(ch, style));
        }
    }

    /// Draws a vertical line of `ch` from `y` (inclusive) to `y_end` (exclusive).
    pub fn vline(&mut self, x: u16, y: u16, y_end: u16, ch: char, style: Style) {
        for row in y..y_end.min(self.rows) {
            self.set(x, row, Cell::new(ch, style));
        }
    }

    /// Darkens the cells in `rect` toward `shadow` by `amount` (a drop
    /// shadow), resolving default/ANSI colours through `palette`.
    pub fn shade(&mut self, rect: Rect, palette: &Palette, shadow: Rgb, amount: f32) {
        self.map_style(rect, |mut s| {
            s.fg = Color::Rgb(s.fg.resolve(palette, true).blend(shadow, amount));
            s.bg = Color::Rgb(s.bg.resolve(palette, false).blend(shadow, amount));
            s
        });
    }

    /// Copies `src` into this grid with its top-left corner at (x, y), clipped.
    pub fn blit(&mut self, src: &Grid, x: u16, y: u16) {
        for sy in 0..src.rows {
            let dy = y.saturating_add(sy);
            if dy >= self.rows {
                break;
            }
            for sx in 0..src.cols {
                let dx = x.saturating_add(sx);
                if dx >= self.cols {
                    break;
                }
                let cell = &src.cells[usize::from(sy) * usize::from(src.cols) + usize::from(sx)];
                if cell.is_continuation() {
                    continue;
                }
                self.set(dx, dy, cell.clone());
            }
        }
    }

    /// The text of row `y` (continuations skipped, trailing spaces kept).
    pub fn row_text(&self, y: u16) -> String {
        self.row(y)
            .iter()
            .filter(|c| !c.is_continuation())
            .map(Cell::display_symbol)
            .collect()
    }

    /// The whole grid as text, one line per row, trailing spaces trimmed.
    /// Used by snapshot tests.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for y in 0..self.rows {
            out.push_str(self.row_text(y).trim_end());
            out.push('\n');
        }
        out
    }

    /// The text plus a style layer, for snapshot tests of anything that
    /// is about colour (themes, block images): every cell's style as one
    /// character, then a legend. `.` is the default style.
    pub fn to_styled_text(&self) -> String {
        const KEYS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut styles: Vec<Style> = Vec::new();
        let mut layer = String::new();
        for y in 0..self.rows {
            let mut row = String::new();
            for cell in self.row(y).iter().filter(|c| !c.is_continuation()) {
                if cell.style == Style::new() {
                    row.push('.');
                    continue;
                }
                let i = styles
                    .iter()
                    .position(|s| *s == cell.style)
                    .unwrap_or_else(|| {
                        styles.push(cell.style);
                        styles.len() - 1
                    });
                row.push(KEYS.chars().nth(i).unwrap_or('?'));
            }
            layer.push_str(row.trim_end_matches('.'));
            layer.push('\n');
        }
        let mut out = self.to_text();
        out.push_str("--- styles\n");
        out.push_str(&layer);
        out.push_str("--- legend\n");
        for (i, s) in styles.iter().enumerate() {
            let key = KEYS.chars().nth(i).unwrap_or('?');
            out.push_str(&format!("{key}: fg={} bg={}", s.fg, s.bg));
            if s.ul != Color::Default {
                out.push_str(&format!(" ul={}", s.ul));
            }
            if !s.attrs.is_empty() {
                out.push_str(&format!(" {:?}", s.attrs));
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_math() {
        let a = Rect::new(0, 0, 10, 5);
        let b = Rect::new(8, 3, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(8, 3, 2, 2));
        assert!(!a.intersect(&Rect::new(20, 20, 1, 1)).intersects(&a));
        assert!(a.contains(9, 4));
        assert!(!a.contains(10, 4));
        assert_eq!(a.inset(1), Rect::new(1, 1, 8, 3));
        assert_eq!(
            Rect::new(15, 15, 4, 4).clamp_within(&a),
            Rect::new(6, 1, 4, 4)
        );
    }

    #[test]
    fn put_str_handles_wide_and_clipping() {
        let mut g = Grid::new(6, 1);
        let end = g.put_str(0, 0, "a界b", Style::new(), 6);
        assert_eq!(end, 4);
        assert_eq!(g.row_text(0), "a界b  ");
        assert_eq!(g.get(1, 0).map(|c| c.width), Some(2));
        assert!(g.get(2, 0).is_some_and(Cell::is_continuation));
        // A wide char that does not fit before max_x becomes a space.
        let mut g = Grid::new(4, 1);
        g.put_str(0, 0, "abc界", Style::new(), 4);
        assert_eq!(g.row_text(0), "abc ");
        // Control characters never land in the grid.
        let mut g = Grid::new(4, 1);
        g.put_str(0, 0, "a\x1bb", Style::new(), 4);
        assert_eq!(g.row_text(0), "a b ");
    }

    #[test]
    fn overwriting_half_a_wide_char_repairs_the_pair() {
        let mut g = Grid::new(4, 1);
        g.put_str(0, 0, "界界", Style::new(), 4);
        g.set(1, 0, Cell::new('x', Style::new()));
        assert_eq!(g.row_text(0), " x界");
        g.set(2, 0, Cell::new('y', Style::new()));
        assert_eq!(g.row_text(0), " xy ");
    }

    #[test]
    fn draw_box_with_title() {
        let mut g = Grid::new(12, 3);
        let s = Style::new();
        g.draw_box(g.area(), BorderKind::Rounded, s, Some(("hi", s)));
        assert_eq!(g.to_text(), "╭─ hi ─────╮\n│          │\n╰──────────╯\n");
    }

    #[test]
    fn shade_darkens_toward_shadow() {
        let palette = Palette {
            fg: Rgb::new(200, 200, 200),
            bg: Rgb::new(100, 100, 100),
            ansi: crate::color::XTERM_ANSI,
        };
        let mut g = Grid::new(2, 1);
        g.shade(Rect::new(0, 0, 1, 1), &palette, Rgb::BLACK, 0.5);
        let s = g.get(0, 0).map(|c| c.style).unwrap_or_default();
        assert_eq!(s.fg, Color::rgb(100, 100, 100));
        assert_eq!(s.bg, Color::rgb(50, 50, 50));
        assert_eq!(g.get(1, 0).map(|c| c.style.bg), Some(Color::Default));
    }

    #[test]
    fn blit_clips() {
        let mut src = Grid::new(3, 2);
        src.put_str(0, 0, "abc", Style::new(), 3);
        src.put_str(0, 1, "def", Style::new(), 3);
        let mut dst = Grid::new(4, 2);
        dst.blit(&src, 2, 1);
        // Source row 0 lands on row 1; source row 1 falls off the bottom.
        assert_eq!(dst.to_text(), "\n  ab\n");
    }
}
