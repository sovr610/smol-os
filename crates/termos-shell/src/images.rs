// SPDX-License-Identifier: GPL-3.0-or-later
//! How app images reach each client (ARCHITECTURE.md §5.8).
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

/// A pixel image a client shows: what the server sends it.
#[derive(Debug, Clone)]
pub struct ShownImage {
    pub key: u64,
    pub picture: Arc<Picture>,
    pub protocol: ImageProtocol,
    /// The whole image, in screen cells.
    pub rect: Rect,
    /// Its visible parts (all of `rect` for Sixel and iTerm2).
    pub pieces: Vec<Rect>,
}

/// An image that may be shown in pixels, waiting for occlusion to be
/// known.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub pane: PaneId,
    pub key: u64,
    pub picture: Arc<Picture>,
    pub rect: Rect,
    /// The pane's content area: the image is clipped to it.
    pub clip: Rect,
}

/// `picture` fitted inside `area` (keeping its aspect ratio for cells of
/// `cell` pixels) and centred.
pub(crate) fn fit(area: Rect, picture: &Picture, cell: PxSize, upscale: bool) -> Rect {
    let (cols, rows) = fit_cells(
        picture.width(),
        picture.height(),
        cell,
        area.w,
        area.h,
        upscale,
    );
    Rect::new(
        area.x + (area.w - cols) / 2,
        area.y + (area.h - rows) / 2,
        cols,
        rows,
    )
}

/// The parts of `rect` (clipped to `clip`) that nothing in `above` covers.
pub(crate) fn visible_pieces(rect: Rect, clip: Rect, above: &[Rect]) -> Vec<Rect> {
    let mut pieces = vec![rect.intersect(&clip)];
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
        rect: Rect,
        bg: Rgb,
    ) -> Arc<Vec<Cell>> {
        let k = (key, rect.w, rect.h, bg);
        if let Some(cells) = self.map.get(&k) {
            return Arc::clone(cells);
        }
        let cells = Arc::new(blocks::render(
            picture,
            rect.w,
            rect.h,
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

/// Copies block `cells` for an image at `rect` into `grid`, only within
/// `within`.
pub(crate) fn draw_blocks(grid: &mut Grid, cells: &[Cell], rect: Rect, within: &[Rect]) {
    for piece in within {
        let p = piece.intersect(&rect);
        for y in p.y..p.bottom() {
            for x in p.x..p.right() {
                let i = usize::from(y - rect.y) * usize::from(rect.w) + usize::from(x - rect.x);
                if let (Some(src), Some(dst)) = (cells.get(i), grid.get_mut(x, y)) {
                    *dst = src.clone();
                }
            }
        }
    }
}

/// Whether a Sixel or iTerm2 image may be shown in pixels right now.
pub(crate) fn whole_image_allowed(
    rect: Rect,
    pieces: &[Rect],
    screen_rows: u16,
    moving: bool,
) -> bool {
    pieces.len() == 1 && pieces[0] == rect && rect.bottom() < screen_rows && !moving
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
        let r = fit(Rect::new(2, 1, 40, 20), &pic(200, 100), cell, false);
        assert_eq!(r, Rect::new(12, 8, 20, 5));
        // Upscaled to fill the width.
        let r = fit(Rect::new(0, 0, 40, 20), &pic(200, 100), cell, true);
        assert_eq!(r, Rect::new(0, 5, 40, 10));
    }

    #[test]
    fn pieces_exclude_what_lies_above() {
        let rect = Rect::new(0, 0, 10, 4);
        let clip = Rect::new(0, 0, 8, 4);
        let pieces = visible_pieces(rect, clip, &[Rect::new(2, 1, 3, 2)]);
        let area: u32 = pieces.iter().map(Rect::area).sum();
        assert_eq!(area, 8 * 4 - 3 * 2);
        assert!(pieces.iter().all(|p| !p.intersects(&Rect::new(2, 1, 3, 2))));
        assert!(visible_pieces(rect, clip, &[Rect::new(0, 0, 20, 20)]).is_empty());
    }

    #[test]
    fn whole_images_need_to_be_whole_and_still() {
        let r = Rect::new(0, 0, 10, 4);
        assert!(whole_image_allowed(r, &[r], 10, false));
        assert!(
            !whole_image_allowed(r, &[r], 4, false),
            "touches the last row"
        );
        assert!(
            !whole_image_allowed(r, &[r], 10, true),
            "a drag is going on"
        );
        assert!(!whole_image_allowed(r, &[Rect::new(0, 0, 5, 4)], 10, false));
    }

    #[test]
    fn blocks_are_drawn_only_where_visible() {
        let mut g = Grid::new(6, 2);
        let cells = vec![Cell::new('#', termos_render::cell::Style::new()); 4 * 2];
        draw_blocks(
            &mut g,
            &cells,
            Rect::new(1, 0, 4, 2),
            &[Rect::new(0, 0, 3, 2)],
        );
        assert_eq!(g.to_text(), " ##\n ##\n");
    }
}
