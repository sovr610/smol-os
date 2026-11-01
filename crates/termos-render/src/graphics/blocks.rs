// SPDX-License-Identifier: GPL-3.0-or-later
//! Images as coloured block characters: they work in every terminal with
//! colour, and since they are ordinary cells, windows cover them
//! correctly without any special handling.

use crate::cell::{Cell, Style};
use crate::color::{Color, Rgb};
use crate::graphics::{Picture, over};

/// How many pixels a cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMode {
    /// 1×2 pixels per cell (`▀` with two colours): no colour error.
    Half,
    /// 2×2 pixels per cell, the best two-colour split of the four:
    /// sharper edges, some colour error.
    Quadrant,
}

/// Quadrant characters by mask: bit 0 top-left, 1 top-right, 2
/// bottom-left, 3 bottom-right set means "foreground".
const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// Renders `pic` into `cols`×`rows` cells (row by row), stretched to fill
/// them, blending transparency over `bg`.
pub fn render(pic: &Picture, cols: u16, rows: u16, mode: BlockMode, bg: Rgb) -> Vec<Cell> {
    let (px_w, px_h) = match mode {
        BlockMode::Half => (u32::from(cols), u32::from(rows) * 2),
        BlockMode::Quadrant => (u32::from(cols) * 2, u32::from(rows) * 2),
    };
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    if cols == 0 || rows == 0 {
        return cells;
    }
    let scaled = pic.resize(px_w, px_h);
    let at = |x: u32, y: u32| over(scaled.pixel(x, y), bg);
    for row in 0..u32::from(rows) {
        for col in 0..u32::from(cols) {
            let cell = match mode {
                BlockMode::Half => half(at(col, row * 2), at(col, row * 2 + 1)),
                BlockMode::Quadrant => quadrant([
                    at(col * 2, row * 2),
                    at(col * 2 + 1, row * 2),
                    at(col * 2, row * 2 + 1),
                    at(col * 2 + 1, row * 2 + 1),
                ]),
            };
            cells.push(cell);
        }
    }
    cells
}

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c)
}

fn half(top: Rgb, bottom: Rgb) -> Cell {
    if top == bottom {
        Cell::blank(Style::new().bg(rgb(top)))
    } else {
        Cell::new('▀', Style::new().fg(rgb(top)).bg(rgb(bottom)))
    }
}

fn dist2(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| {
        let v = i32::from(x) - i32::from(y);
        (v * v).unsigned_abs()
    };
    d(a.r, b.r) + d(a.g, b.g) + d(a.b, b.b)
}

/// The two-colour split of four pixels with the least squared error.
fn quadrant(px: [Rgb; 4]) -> Cell {
    let mut best = (u32::MAX, 0usize, px[0], px[0]);
    // Masks 0..8 cover every split (a mask and its complement are the
    // same split with the colours swapped); 0 is "one colour".
    for mask in 0usize..8 {
        let mut sums = [[0u32; 3]; 2];
        let mut counts = [0u32; 2];
        for (i, p) in px.iter().enumerate() {
            let side = usize::from(mask & (1 << i) != 0);
            sums[side][0] += u32::from(p.r);
            sums[side][1] += u32::from(p.g);
            sums[side][2] += u32::from(p.b);
            counts[side] += 1;
        }
        let mean = |side: usize| {
            let n = counts[side].max(1);
            // Means of u8 values are u8 values.
            let avg = |s: u32| ((s + n / 2) / n) as u8;
            Rgb::new(avg(sums[side][0]), avg(sums[side][1]), avg(sums[side][2]))
        };
        let (bg, fg) = (mean(0), mean(1));
        let err: u32 = px
            .iter()
            .enumerate()
            .map(|(i, p)| dist2(*p, if mask & (1 << i) != 0 { fg } else { bg }))
            .sum();
        if err < best.0 {
            best = (err, mask, fg, bg);
        }
    }
    let (_, mask, fg, bg) = best;
    if mask == 0 || fg == bg {
        return Cell::blank(Style::new().bg(rgb(bg)));
    }
    Cell::new(QUADRANTS[mask], Style::new().fg(rgb(fg)).bg(rgb(bg)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::tests::picture;
    use crate::grid::Grid;
    use proptest::prelude::*;

    const BLACK: Rgb = Rgb::BLACK;

    fn draw(cells: &[Cell], cols: u16, rows: u16) -> Grid {
        let mut g = Grid::new(cols, rows);
        for (i, c) in cells.iter().enumerate() {
            let i = u16::try_from(i).expect("small");
            if let Some(cell) = g.get_mut(i % cols, i / cols) {
                *cell = c.clone();
            }
        }
        g
    }

    /// Red left half, blue right half, white bottom-right quarter.
    fn sample() -> Picture {
        picture(8, 8, |x, y| match (x < 4, y < 4) {
            (true, _) => [255, 0, 0, 255],
            (false, true) => [0, 0, 255, 255],
            (false, false) => [255, 255, 255, 255],
        })
    }

    #[test]
    fn snapshot_half_and_quadrant() {
        for (name, mode) in [("half", BlockMode::Half), ("quadrant", BlockMode::Quadrant)] {
            let cells = render(&sample(), 4, 2, mode, BLACK);
            insta::assert_snapshot!(
                format!("blocks-{name}"),
                draw(&cells, 4, 2).to_styled_text()
            );
        }
    }

    #[test]
    fn snapshot_a_diagonal_edge() {
        // White above the diagonal, dark below, 8×8 into 4×2 cells.
        let p = picture(8, 8, |x, y| {
            if x > y {
                [255, 255, 255, 255]
            } else {
                [30, 30, 60, 255]
            }
        });
        for (name, mode) in [("half", BlockMode::Half), ("quadrant", BlockMode::Quadrant)] {
            let cells = render(&p, 4, 2, mode, BLACK);
            insta::assert_snapshot!(
                format!("blocks-diagonal-{name}"),
                draw(&cells, 4, 2).to_styled_text()
            );
        }
    }

    #[test]
    fn transparency_shows_the_background() {
        let p = picture(2, 2, |_, _| [255, 255, 255, 0]);
        let bg = Rgb::new(10, 20, 30);
        let cells = render(&p, 1, 1, BlockMode::Quadrant, bg);
        assert_eq!(cells[0].style.bg, Color::Rgb(bg));
        assert_eq!(cells[0].symbol, " ");
    }

    #[test]
    fn quadrant_split_picks_the_edge() {
        // Left column white, right column black: the left half block.
        let w = Rgb::new(255, 255, 255);
        let c = quadrant([w, BLACK, w, BLACK]);
        let (ch, fg, bg) = (c.symbol.as_str(), c.style.fg, c.style.bg);
        assert!(
            (ch == "▌" && fg == Color::Rgb(w) && bg == Color::Rgb(BLACK))
                || (ch == "▐" && fg == Color::Rgb(BLACK) && bg == Color::Rgb(w)),
            "{ch} {fg:?} {bg:?}"
        );
    }

    proptest! {
        #[test]
        fn rasters_fill_exactly_and_use_block_glyphs(
            w in 1u32..40, h in 1u32..40, cols in 1u16..20, rows in 1u16..12,
            seed in any::<u32>(), quad in any::<bool>(),
        ) {
            let p = picture(w, h, |x, y| {
                let v = (x * 31 + y * 17 + seed) % 256;
                let v = u8::try_from(v).unwrap_or(0);
                [v, v.wrapping_mul(3), v.wrapping_add(90), 255]
            });
            let mode = if quad { BlockMode::Quadrant } else { BlockMode::Half };
            let cells = render(&p, cols, rows, mode, BLACK);
            prop_assert_eq!(cells.len(), usize::from(cols) * usize::from(rows));
            for c in &cells {
                prop_assert!(c.symbol == " " || c.symbol == "▀" || QUADRANTS.iter().any(|q| c.symbol == q.to_string()));
                prop_assert_eq!(c.width, 1);
            }
        }

        #[test]
        fn a_uniform_image_is_uniform_cells(r in any::<u8>(), g in any::<u8>(), b in any::<u8>()) {
            let p = picture(6, 6, |_, _| [r, g, b, 255]);
            for mode in [BlockMode::Half, BlockMode::Quadrant] {
                for c in render(&p, 3, 2, mode, BLACK) {
                    prop_assert_eq!(c.symbol.as_str(), " ");
                    prop_assert_eq!(c.style.bg, Color::Rgb(Rgb::new(r, g, b)));
                }
            }
        }
    }
}
