// SPDX-License-Identifier: GPL-3.0-or-later
//! What the console shows: the client's copy of the server's grid, drawn
//! into a CPU framebuffer as frames arrive. Each frame repaints only the
//! cells it changed (and the cursor's old and new cells); the rows of
//! pixels it touched are the damage the display copies out.

use super::damage::Damage;
use termos_core::proto::FrameUpdate;
use termos_render::diff;
use termos_render::encode::CursorState;
use termos_render::grid::Grid;
use termos_render::raster::{Canvas, Layout, Rasterizer, pack};

pub(super) struct Screen {
    pub(super) raster: Rasterizer,
    pub(super) grid: Grid,
    pub(super) layout: Layout,
    pub(super) width: u32,
    pub(super) height: u32,
    /// `0x00RRGGBB`, `width` pixels per row.
    pub(super) pixels: Vec<u32>,
    cursor: Option<CursorState>,
    /// Pixel rows changed since the display last took them.
    damage: Damage,
}

impl Screen {
    pub(super) fn new(raster: Rasterizer, width: u32, height: u32) -> Self {
        let layout = raster.layout(width, height);
        let mut s = Self {
            grid: Grid::new(layout.cols, layout.rows),
            raster,
            layout,
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
            cursor: None,
            damage: Damage::default(),
        };
        s.repaint();
        s
    }

    /// The grid size this screen holds, for `Hello` and `Resize`.
    pub(super) fn cells(&self) -> (u16, u16) {
        (self.layout.cols, self.layout.rows)
    }

    fn damage_cell_row(&mut self, row: u16) {
        let h = self.raster.metrics().height;
        let y0 = self.layout.y + u32::from(row) * h;
        self.damage.add(y0, (y0 + h).min(self.height));
    }

    /// Repaints everything (first frame, a new font size, the seat back).
    pub(super) fn repaint(&mut self) {
        let mut canvas = Canvas {
            pixels: &mut self.pixels,
            stride: self.width as usize,
            width: self.width,
            height: self.height,
        };
        self.raster.paint_all(&mut canvas, self.layout, &self.grid);
        if let Some(c) = self.cursor {
            self.raster
                .paint_cursor(&mut canvas, self.layout, &self.grid, c);
        }
        self.damage = Damage::all(self.height);
    }

    /// Applies a frame from the server.
    pub(super) fn apply(&mut self, f: &FrameUpdate) {
        let resized = f.cols != self.grid.cols() || f.rows != self.grid.rows();
        if resized {
            self.grid = Grid::new(f.cols, f.rows);
        }
        diff::apply(&mut self.grid, &f.spans);
        let old = self.cursor;
        self.cursor = f.cursor;
        if resized || f.clear {
            self.repaint();
            return;
        }
        let (layout, grid) = (self.layout, &self.grid);
        let mut rows = Vec::new();
        {
            let mut canvas = Canvas {
                pixels: &mut self.pixels,
                stride: self.width as usize,
                width: self.width,
                height: self.height,
            };
            for span in &f.spans {
                let n = u16::try_from(span.cells.len()).unwrap_or(u16::MAX);
                self.raster
                    .paint_cells(&mut canvas, layout, grid, span.col, span.row, n);
                rows.push(span.row);
            }
            // The cursor's old cell loses it, the new one gets it.
            if let Some(c) = old
                && old != self.cursor
            {
                self.raster
                    .paint_cells(&mut canvas, layout, grid, c.x, c.y, 1);
                rows.push(c.y);
            }
            if let Some(c) = self.cursor {
                self.raster.paint_cursor(&mut canvas, layout, grid, c);
                rows.push(c.y);
            }
        }
        for r in rows {
            self.damage_cell_row(r);
        }
    }

    /// A new font size; returns the new grid size when it changed.
    pub(super) fn set_font_size(&mut self, px: f32) -> Option<(u16, u16)> {
        let before = self.cells();
        if let Err(e) = self.raster.set_size(px) {
            tracing::warn!("font size {px}: {e}");
            return None;
        }
        self.layout = self.raster.layout(self.width, self.height);
        // Until the server's next frame, show what fits.
        self.repaint();
        (self.cells() != before).then(|| self.cells())
    }

    /// The pixel rows changed since the last call.
    pub(super) fn take_damage(&mut self) -> Damage {
        self.damage.take()
    }

    /// Fills everything with the background (before the first frame).
    pub(super) fn clear(&mut self) {
        self.pixels.fill(pack(self.raster.palette.bg));
        self.damage = Damage::all(self.height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_render::cell::{Cell, Style};
    use termos_render::color::{Color, Palette, Rgb, XTERM_ANSI};
    use termos_render::diff::Span;
    use termos_render::encode::CursorShape;
    use termos_render::raster::FontSet;

    fn screen() -> Screen {
        let palette = Palette {
            fg: Rgb::new(200, 200, 200),
            bg: Rgb::new(0, 0, 0),
            ansi: XTERM_ANSI,
        };
        let r = Rasterizer::new(FontSet::bundled(), 16.0, palette).expect("font");
        // 10×19 cells: 8×4 of them in 80×76 pixels.
        Screen::new(r, 80, 76)
    }

    fn frame(spans: Vec<Span>, cursor: Option<CursorState>) -> FrameUpdate {
        FrameUpdate {
            cols: 8,
            rows: 4,
            clear: false,
            spans,
            cursor,
            images: None,
        }
    }

    #[test]
    fn frames_paint_only_what_changed() {
        let mut s = screen();
        assert_eq!(s.cells(), (8, 4));
        assert_eq!(
            s.take_damage().bands(),
            [(0, 76)],
            "the first paint is everything"
        );
        let red = Style::new().bg(Color::Indexed(1));
        let f = frame(
            vec![Span {
                row: 2,
                col: 1,
                cells: vec![Cell::new('a', red), Cell::new('b', red)],
            }],
            None,
        );
        s.apply(&f);
        assert_eq!(s.take_damage().bands(), [(38, 57)], "row 2 only");
        let at = |s: &Screen, x: u32, y: u32| s.pixels[(y * s.width + x) as usize];
        assert_eq!(at(&s, 10, 38), pack(XTERM_ANSI[1]));
        assert_eq!(at(&s, 0, 38), 0);
        // The cursor: its cell, then its old cell when it moves.
        let cur = CursorState {
            x: 5,
            y: 0,
            shape: CursorShape::Block,
            blinking: false,
        };
        s.apply(&frame(Vec::new(), Some(cur)));
        assert_eq!(at(&s, 52, 2), pack(Rgb::new(200, 200, 200)));
        assert_eq!(s.take_damage().bands(), [(0, 19)]);
        s.apply(&frame(Vec::new(), Some(CursorState { y: 3, ..cur })));
        assert_eq!(at(&s, 52, 2), 0, "the old cursor is gone");
        assert_eq!(
            s.take_damage().bands(),
            [(0, 19), (57, 76)],
            "rows 0 and 3, not the rows between"
        );
        assert!(s.take_damage().is_empty());
    }

    #[test]
    fn a_new_size_or_font_repaints() {
        let mut s = screen();
        s.take_damage();
        let mut f = frame(Vec::new(), None);
        f.cols = 6;
        s.apply(&f);
        assert_eq!(s.grid.cols(), 6);
        assert_eq!(s.take_damage().bands(), [(0, 76)]);
        assert_eq!(s.set_font_size(32.0), Some((4, 2)));
        assert_eq!(s.set_font_size(32.0), None, "the same size again");
    }
}
