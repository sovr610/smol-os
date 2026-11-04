// SPDX-License-Identifier: GPL-3.0-or-later
//! What the console shows: the client's copy of the server's grid, drawn
//! into a CPU framebuffer as frames arrive. Each frame repaints only the
//! cells it changed (and the cursor's old and new cells); the rows of
//! pixels it touched are the damage the display copies out.
//!
//! Pictures are drawn as pixels over the cells (D-105). The server places
//! them for this console as it would for a Kitty terminal: one placement
//! per visible piece, with the part of the picture it shows, so windows
//! over a picture are already cut out of it.

use std::collections::HashMap;

use super::damage::Damage;
use termos_core::proto::{FrameUpdate, ImagePlacement};
use termos_render::color::Rgb;
use termos_render::diff;
use termos_render::encode::CursorState;
use termos_render::graphics::Picture;
use termos_render::grid::Grid;
use termos_render::raster::{Canvas, Layout, Rasterizer, pack};

/// Image data for one picture is cut off past this (the server's own
/// limits keep real pictures far below it).
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Pictures the server sent, and where it placed them.
#[derive(Default)]
struct Images {
    /// Data still arriving.
    partial: HashMap<u32, Vec<u8>>,
    pictures: HashMap<u32, Picture>,
    placements: Vec<ImagePlacement>,
    /// Each placement's pixels, cut and scaled to its cells, made once.
    scaled: HashMap<ImagePlacement, Picture>,
}

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
    images: Images,
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
            images: Images::default(),
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
        for p in self.images.placements.clone() {
            self.draw_placement(&p);
        }
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
        // A new placement list: pictures that moved or went are painted
        // over with their cells, then all are drawn again.
        let placed = f.images.is_some();
        if let Some(new) = &f.images {
            let gone: Vec<ImagePlacement> = self
                .images
                .placements
                .iter()
                .filter(|p| !new.contains(p))
                .copied()
                .collect();
            self.images.placements.clone_from(new);
            self.images.scaled.retain(|p, _| new.contains(p));
            if !(resized || f.clear) {
                for p in &gone {
                    self.paint_cells_under(p);
                }
            }
        }
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
        // Cells painted over a picture hid it: draw it again.
        for p in self.images.placements.clone() {
            let span = p.y..p.y.saturating_add(p.rows);
            if placed || rows.iter().any(|r| span.contains(r)) {
                self.draw_placement(&p);
            }
        }
        for r in rows {
            self.damage_cell_row(r);
        }
    }

    /// A piece of picture `id` (PNG); `last` completes it.
    pub(super) fn image_data(&mut self, id: u32, chunk: &[u8], last: bool) {
        let buf = self.images.partial.entry(id).or_default();
        if buf.len() + chunk.len() > MAX_IMAGE_BYTES {
            tracing::warn!("picture {id} is too large; not shown");
            self.images.partial.remove(&id);
            return;
        }
        buf.extend_from_slice(chunk);
        if !last {
            return;
        }
        let data = self.images.partial.remove(&id).unwrap_or_default();
        match Picture::decode(&data) {
            Ok(pic) => {
                // New data for an id replaces the picture.
                self.images.pictures.insert(id, pic);
                self.images.scaled.retain(|p, _| p.image != id);
                for p in self.images.placements.clone() {
                    if p.image == id {
                        self.draw_placement(&p);
                    }
                }
            }
            Err(e) => tracing::warn!("picture {id}: {e}"),
        }
    }

    /// Picture `id` is no longer needed.
    pub(super) fn image_free(&mut self, id: u32) {
        self.images.partial.remove(&id);
        self.images.pictures.remove(&id);
        self.images.scaled.retain(|p, _| p.image != id);
        for p in self.images.placements.clone() {
            if p.image == id {
                self.paint_cells_under(&p);
            }
        }
    }

    /// The cells a placement covers, painted again (the picture is gone
    /// from there).
    fn paint_cells_under(&mut self, p: &ImagePlacement) {
        let (layout, grid) = (self.layout, &self.grid);
        let mut canvas = Canvas {
            pixels: &mut self.pixels,
            stride: self.width as usize,
            width: self.width,
            height: self.height,
        };
        let last_row = p.y.saturating_add(p.rows).min(grid.rows());
        for row in p.y..last_row {
            let cols = p.cols.min(grid.cols().saturating_sub(p.x));
            if cols > 0 {
                self.raster
                    .paint_cells(&mut canvas, layout, grid, p.x, row, cols);
            }
        }
        for row in p.y..last_row {
            self.damage_cell_row(row);
        }
    }

    /// Draws a placement's part of its picture over its cells, blended by
    /// the picture's alpha.
    fn draw_placement(&mut self, p: &ImagePlacement) {
        let m = self.raster.metrics();
        let Some(pic) = self.images.pictures.get(&p.image) else {
            return;
        };
        let (w, h) = (u32::from(p.cols) * m.width, u32::from(p.rows) * m.height);
        if w == 0 || h == 0 {
            return;
        }
        let scaled = self.images.scaled.entry(*p).or_insert_with(|| {
            let [x, y, sw, sh] = p.src;
            pic.crop(x, y, sw, sh).resize(w, h)
        });
        let x0 = self.layout.x + u32::from(p.x) * m.width;
        let y0 = self.layout.y + u32::from(p.y) * m.height;
        let (sw, sh) = (scaled.width(), scaled.height());
        let rgba = scaled.rgba();
        for dy in 0..sh {
            let y = y0 + dy;
            if y >= self.height {
                break;
            }
            for dx in 0..sw {
                let x = x0 + dx;
                if x >= self.width {
                    break;
                }
                let i = ((dy * sw + dx) * 4) as usize;
                let a = u32::from(rgba[i + 3]);
                if a == 0 {
                    continue;
                }
                let at = (y * self.width + x) as usize;
                let under = self.pixels[at];
                let mix = |shift: u32, c: u8| -> u32 {
                    let b = (under >> shift) & 0xff;
                    ((u32::from(c) * a + b * (255 - a)) / 255) << shift
                };
                self.pixels[at] = mix(16, rgba[i]) | mix(8, rgba[i + 1]) | mix(0, rgba[i + 2]);
            }
        }
        self.damage
            .add(y0.min(self.height), (y0 + sh).min(self.height));
    }

    /// The visual bell: a frame around the screen's edge. [`Self::repaint`]
    /// takes it away.
    pub(super) fn flash(&mut self, color: Rgb) {
        let t = (self.raster.metrics().height / 6).clamp(2, 8);
        let c = pack(color);
        let (w, h) = (self.width, self.height);
        for y in 0..h {
            let row = &mut self.pixels[(y * w) as usize..((y + 1) * w) as usize];
            if y < t || y >= h.saturating_sub(t) {
                row.fill(c);
            } else {
                let side = (t as usize).min(row.len() / 2);
                let len = row.len();
                row[..side].fill(c);
                row[len - side..].fill(c);
            }
        }
        self.damage = Damage::all(self.height);
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

    /// Pictures are drawn as pixels where the server places them, cut
    /// and scaled; cells painted over them bring them back; a new
    /// placement list or a freed picture leaves the cells underneath.
    #[test]
    fn pictures_are_drawn_as_pixels() {
        use termos_render::caps::ImageProtocol;

        let mut s = screen();
        s.take_damage();
        // 4×2 pixels: the left half red, the right half blue.
        let mut rgba = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                rgba.extend_from_slice(if x < 2 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let png = Picture::from_rgba(4, 2, rgba).expect("valid").to_png();
        let (a, b) = png.split_at(png.len() / 2);
        s.image_data(7, a, false);
        s.image_data(7, b, true);
        // The right half of the picture, over cells 1–2 of row 1.
        let place = ImagePlacement {
            image: 7,
            protocol: ImageProtocol::Kitty,
            x: 1,
            y: 1,
            cols: 2,
            rows: 1,
            src: [2, 0, 2, 2],
        };
        let mut f = frame(Vec::new(), None);
        f.images = Some(vec![place]);
        s.apply(&f);
        let at = |s: &Screen, x: u32, y: u32| s.pixels[(y * s.width + x) as usize];
        let blue = pack(Rgb::new(0, 0, 255));
        assert_eq!(at(&s, 10, 19), blue, "the picture's corner");
        assert_eq!(at(&s, 29, 37), blue, "the far corner, scaled up");
        assert_eq!(at(&s, 30, 19), 0, "nothing past it");
        assert_eq!(s.take_damage().bands(), [(19, 38)]);

        // A cell painted over it: the picture comes back on top.
        let f = frame(
            vec![Span {
                row: 1,
                col: 1,
                cells: vec![Cell::new(' ', Style::new())],
            }],
            None,
        );
        s.apply(&f);
        assert_eq!(at(&s, 10, 19), blue);

        // Placed nowhere now: the cells show again.
        let mut f = frame(Vec::new(), None);
        f.images = Some(Vec::new());
        s.apply(&f);
        assert_eq!(at(&s, 10, 19), 0);

        // Freed while shown: the same.
        let mut f = frame(Vec::new(), None);
        f.images = Some(vec![place]);
        s.apply(&f);
        assert_eq!(at(&s, 10, 19), blue);
        s.image_free(7);
        assert_eq!(at(&s, 10, 19), 0);
    }

    #[test]
    fn the_bell_flashes_a_frame_until_repainted() {
        let mut s = screen();
        s.take_damage();
        let yellow = Rgb::new(255, 255, 0);
        s.flash(yellow);
        let at = |s: &Screen, x: u32, y: u32| s.pixels[(y * s.width + x) as usize];
        assert_eq!(at(&s, 40, 0), pack(yellow), "top edge");
        assert_eq!(at(&s, 0, 40), pack(yellow), "left edge");
        assert_eq!(at(&s, 79, 75), pack(yellow), "bottom right");
        assert_eq!(at(&s, 40, 40), 0, "the middle is untouched");
        assert_eq!(s.take_damage().bands(), [(0, 76)]);
        s.repaint();
        assert_eq!(at(&s, 40, 0), 0, "gone");
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
