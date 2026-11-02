// SPDX-License-Identifier: GPL-3.0-or-later
//! How images reach each client (ARCHITECTURE.md §5.8): those apps show
//! and those programs print inside panes.
//!
//! For every client and image the desktop picks one way to show it:
//!
//! - **blocks** (any terminal): coloured block characters drawn into the
//!   cell grid. Windows and overlays drawn later simply cover them.
//! - **Kitty**: one placement per visible piece (the image minus whatever
//!   lies above it), each a crop of the uploaded image.
//! - **Sixel / iTerm2**: only when the whole image is visible, off the
//!   last row (a terminal would scroll), and nothing is being dragged or
//!   animated; otherwise blocks, for the visible pieces.
//!
//! Pixel images leave their cells blank; the server sends the images to
//! the client, which draws them after the cells.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use termos_core::ids::PaneId;
use termos_render::caps::{ImageProtocol, PxSize};
use termos_render::cell::Cell;
use termos_render::color::Rgb;
use termos_render::graphics::blocks::{self, BlockMode};
use termos_render::graphics::{Picture, fit_cells};
use termos_render::grid::{Grid, Rect};

/// Where an image is on screen: its top-left cell (which may be above or
/// left of the screen, for an image partly scrolled out of a pane) and
/// its size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub x: i32,
    pub y: i32,
    pub cols: u16,
    pub rows: u16,
}

impl Place {
    pub fn of(rect: Rect) -> Self {
        Self {
            x: i32::from(rect.x),
            y: i32::from(rect.y),
            cols: rect.w,
            rows: rect.h,
        }
    }

    /// The part within `clip` (empty when none).
    pub fn visible(&self, clip: Rect) -> Rect {
        let x0 = self.x.max(i32::from(clip.x));
        let y0 = self.y.max(i32::from(clip.y));
        let x1 = (self.x + i32::from(self.cols)).min(i32::from(clip.right()));
        let y1 = (self.y + i32::from(self.rows)).min(i32::from(clip.bottom()));
        if x1 <= x0 || y1 <= y0 {
            return Rect::new(0, 0, 0, 0);
        }
        // In range: within `clip`, which is in u16 coordinates.
        Rect::new(x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16)
    }

    /// The whole image as a screen rectangle, when it starts on screen.
    pub fn rect(&self) -> Option<Rect> {
        Some(Rect::new(
            u16::try_from(self.x).ok()?,
            u16::try_from(self.y).ok()?,
            self.cols,
            self.rows,
        ))
    }
}

/// A pixel image a client shows: what the server sends it.
#[derive(Debug, Clone)]
pub struct ShownImage {
    pub key: u64,
    pub picture: Arc<Picture>,
    pub protocol: ImageProtocol,
    /// The whole image.
    pub place: Place,
    /// Its visible parts (the whole image for Sixel and iTerm2).
    pub pieces: Vec<Rect>,
}

/// An image that may be shown in pixels, waiting for occlusion to be
/// known.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub pane: PaneId,
    pub key: u64,
    pub picture: Arc<Picture>,
    pub place: Place,
    /// The pane's content area: the image is clipped to it.
    pub clip: Rect,
}

/// `picture` fitted inside a `w`×`h` box at (`x`, `y`), which may start
/// above or left of the screen, keeping its aspect ratio, and centred.
pub(crate) fn fit_place(
    x: i32,
    y: i32,
    w: u16,
    h: u16,
    picture: &Picture,
    cell: PxSize,
    upscale: bool,
) -> Place {
    let (cols, rows) = fit_cells(picture.width(), picture.height(), cell, w, h, upscale);
    Place {
        x: x + i32::from((w - cols) / 2),
        y: y + i32::from((h - rows) / 2),
        cols,
        rows,
    }
}

/// The parts of an image (clipped to `clip`) that nothing in `above`
/// covers.
pub(crate) fn visible_pieces(place: Place, clip: Rect, above: &[Rect]) -> Vec<Rect> {
    let mut pieces = vec![place.visible(clip)];
    pieces.retain(|r| !r.is_empty());
    for cover in above {
        pieces = pieces.iter().flat_map(|p| p.subtract(cover)).collect();
    }
    pieces
}

/// Block renderings of images, by image, size and background, for the
/// few most recent (rendering one is costly, drawing it is not).
#[derive(Debug, Default)]
pub(crate) struct BlockCache {
    map: HashMap<(u64, u16, u16, Rgb), Arc<Vec<Cell>>>,
    order: VecDeque<(u64, u16, u16, Rgb)>,
}

const BLOCK_CACHE: usize = 16;

impl BlockCache {
    pub(crate) fn get(
        &mut self,
        key: u64,
        picture: &Picture,
        place: Place,
        bg: Rgb,
    ) -> Arc<Vec<Cell>> {
        let k = (key, place.cols, place.rows, bg);
        if let Some(cells) = self.map.get(&k) {
            return Arc::clone(cells);
        }
        let cells = Arc::new(blocks::render(
            picture,
            place.cols,
            place.rows,
            BlockMode::Quadrant,
            bg,
        ));
        self.map.insert(k, Arc::clone(&cells));
        self.order.push_back(k);
        while self.order.len() > BLOCK_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        cells
    }
}

/// Copies block `cells` for an image at `place` into `grid`, only within
/// `within`.
pub(crate) fn draw_blocks(grid: &mut Grid, cells: &[Cell], place: Place, within: &[Rect]) {
    for piece in within {
        let p = place.visible(*piece);
        for y in p.y..p.bottom() {
            for x in p.x..p.right() {
                // Non-negative: (x, y) lies within the image.
                let dx = (i32::from(x) - place.x) as usize;
                let dy = (i32::from(y) - place.y) as usize;
                let i = dy * usize::from(place.cols) + dx;
                if let (Some(src), Some(dst)) = (cells.get(i), grid.get_mut(x, y)) {
                    *dst = src.clone();
                }
            }
        }
    }
}

/// Whether a Sixel or iTerm2 image may be shown in pixels right now.
pub(crate) fn whole_image_allowed(
    place: Place,
    pieces: &[Rect],
    screen_rows: u16,
    moving: bool,
) -> bool {
    place.rect().is_some_and(|rect| {
        pieces.len() == 1 && pieces[0] == rect && rect.bottom() < screen_rows && !moving
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: u32, h: u32) -> Picture {
        Picture::from_rgba(w, h, vec![255; (w * h * 4) as usize]).expect("valid")
    }

    #[test]
    fn images_are_fitted_and_centred() {
        let cell = PxSize {
            width: 10,
            height: 20,
        };
        // 200×100 px is 20×5 cells at 10×20, centred in 40×20.
        let r = fit_place(2, 1, 40, 20, &pic(200, 100), cell, false);
        assert_eq!(r, Place::of(Rect::new(12, 8, 20, 5)));
        // Upscaled to fill the width.
        let r = fit_place(0, 0, 40, 20, &pic(200, 100), cell, true);
        assert_eq!(r, Place::of(Rect::new(0, 5, 40, 10)));
        // A box starting above the screen keeps its size.
        let r = fit_place(0, -6, 40, 20, &pic(200, 100), cell, true);
        assert_eq!((r.y, r.cols, r.rows), (-1, 40, 10));
    }

    #[test]
    fn pieces_exclude_what_lies_above() {
        let rect = Rect::new(0, 0, 10, 4);
        let clip = Rect::new(0, 0, 8, 4);
        let pieces = visible_pieces(Place::of(rect), clip, &[Rect::new(2, 1, 3, 2)]);
        let area: u32 = pieces.iter().map(Rect::area).sum();
        assert_eq!(area, 8 * 4 - 3 * 2);
        assert!(pieces.iter().all(|p| !p.intersects(&Rect::new(2, 1, 3, 2))));
        assert!(visible_pieces(Place::of(rect), clip, &[Rect::new(0, 0, 20, 20)]).is_empty());
        // Partly scrolled above the pane: only its lower rows.
        let up = Place {
            x: 2,
            y: -3,
            cols: 4,
            rows: 5,
        };
        assert_eq!(visible_pieces(up, clip, &[]), vec![Rect::new(2, 0, 4, 2)]);
        assert_eq!(up.rect(), None);
    }

    #[test]
    fn whole_images_need_to_be_whole_and_still() {
        let r = Rect::new(0, 0, 10, 4);
        let p = Place::of(r);
        assert!(whole_image_allowed(p, &[r], 10, false));
        assert!(
            !whole_image_allowed(p, &[r], 4, false),
            "touches the last row"
        );
        assert!(
            !whole_image_allowed(p, &[r], 10, true),
            "a drag is going on"
        );
        assert!(!whole_image_allowed(p, &[Rect::new(0, 0, 5, 4)], 10, false));
        let above = Place { y: -1, ..p };
        assert!(!whole_image_allowed(
            above,
            &[Rect::new(0, 0, 10, 3)],
            10,
            false
        ));
    }

    #[test]
    fn blocks_are_drawn_only_where_visible() {
        let mut g = Grid::new(6, 2);
        let cells = vec![Cell::new('#', termos_render::cell::Style::new()); 4 * 2];
        draw_blocks(
            &mut g,
            &cells,
            Place::of(Rect::new(1, 0, 4, 2)),
            &[Rect::new(0, 0, 3, 2)],
        );
        assert_eq!(g.to_text(), " ##\n ##\n");
        // An image starting above the grid: its lower half shows.
        let mut g = Grid::new(4, 2);
        let cells: Vec<Cell> = "abcdefgh"
            .chars()
            .map(|c| Cell::new(c, termos_render::cell::Style::new()))
            .collect();
        let place = Place {
            x: 0,
            y: -1,
            cols: 4,
            rows: 2,
        };
        let area = g.area();
        draw_blocks(&mut g, &cells, place, &[area]);
        assert_eq!(g.to_text(), "efgh\n\n");
    }
}
