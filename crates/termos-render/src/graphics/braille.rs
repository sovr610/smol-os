// SPDX-License-Identifier: GPL-3.0-or-later
//! Line art and charts as braille: 2×4 dots per cell, one colour per cell
//! on the terminal's own background.

use crate::cell::{Cell, Style};
use crate::color::{Color, Rgb};
use crate::graphics::{Picture, over};

/// Dot bits by position within the cell: `DOTS[y][x]`.
const DOTS: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Renders `pic` into `cols`×`rows` braille cells. A dot is on where the
/// pixel stands out from `bg` (by more than `threshold` in luma, 0-255);
/// a cell's colour is the mean of its "on" pixels.
pub fn render(pic: &Picture, cols: u16, rows: u16, bg: Rgb, threshold: u8) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    if cols == 0 || rows == 0 {
        return cells;
    }
    let scaled = pic.resize(u32::from(cols) * 2, u32::from(rows) * 4);
    let luma = |c: Rgb| -> i32 {
        (2126 * i32::from(c.r) + 7152 * i32::from(c.g) + 722 * i32::from(c.b)) / 10_000
    };
    let bg_luma = luma(bg);
    for row in 0..u32::from(rows) {
        for col in 0..u32::from(cols) {
            let mut bits = 0u32;
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for (dy, line) in DOTS.iter().enumerate() {
                for (dx, bit) in line.iter().enumerate() {
                    let x = col * 2 + dx as u32;
                    let y = row * 4 + dy as u32;
                    let c = over(scaled.pixel(x, y), bg);
                    if (luma(c) - bg_luma).abs() > i32::from(threshold) {
                        bits |= bit;
                        r += u32::from(c.r);
                        g += u32::from(c.g);
                        b += u32::from(c.b);
                        n += 1;
                    }
                }
            }
            if bits == 0 {
                cells.push(Cell::blank(Style::new()));
                continue;
            }
            // Means of u8 values are u8 values.
            let avg = |s: u32| (s / n) as u8;
            let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
            cells.push(Cell::new(
                ch,
                Style::new().fg(Color::Rgb(Rgb::new(avg(r), avg(g), avg(b)))),
            ));
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::tests::picture;

    #[test]
    fn a_diagonal_line() {
        // A white diagonal on black, 4×8 pixels into 2×2 cells.
        let p = picture(4, 8, |x, y| {
            if x == y / 2 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let cells = render(&p, 2, 2, Rgb::BLACK, 64);
        let text: String = cells.iter().map(|c| c.symbol.as_str()).collect();
        // Each of the two cells on the diagonal has dots in its left column
        // at rows 0-1 and its right column at rows 2-3: U+28A3.
        assert_eq!(text, "⢣  ⢣");
        assert_eq!(cells[0].style.fg, Color::Rgb(Rgb::new(255, 255, 255)));
    }

    #[test]
    fn empty_and_faint_pixels_leave_blanks() {
        let p = picture(2, 4, |_, _| [20, 20, 20, 255]);
        let cells = render(&p, 1, 1, Rgb::BLACK, 64);
        assert_eq!(cells[0].symbol, " ");
        let cells = render(&p, 1, 1, Rgb::BLACK, 5);
        assert_eq!(cells[0].symbol, "⣿", "every dot past a low threshold");
    }
}
