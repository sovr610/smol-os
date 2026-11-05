// SPDX-License-Identifier: GPL-3.0-or-later
//! Feeds arbitrary program output into the VT emulator, interleaved with
//! resizes, scrolling and rendering. Any panic is a bug.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_render::grid::Grid;
use termos_term::vt::{AlacrittyVt, VtBackend};

fuzz_target!(|data: &[u8]| {
    let mut vt = AlacrittyVt::new(40, 10, 50);
    let mut grid = Grid::new(40, 10);
    // Split the input at 0xFE bytes; each chunk may carry a control op.
    for chunk in data.split(|&b| b == 0xFE) {
        match chunk.first() {
            Some(&op) if op >= 0xF0 => {
                let arg = chunk.get(1).copied().unwrap_or(1);
                match op & 0x3 {
                    0 => vt.resize(u16::from(arg % 120) + 1, u16::from(arg % 50) + 1),
                    1 => vt.scroll(i32::from(arg as i8)),
                    2 => vt.flush_sync(),
                    _ if arg & 1 == 1 => {
                        // Command blocks (OSC 133) found in the text.
                        for b in vt.blocks() {
                            assert!(b.output_rows.is_none_or(|(a, z)| a <= z));
                        }
                    }
                    _ => vt.scroll_to_bottom(),
                }
            }
            _ => vt.advance(chunk),
        }
        let (cols, rows) = vt.size();
        if grid.cols() != cols || grid.rows() != rows {
            grid.resize(cols, rows);
        }
        let area = grid.area();
        vt.render_into(&mut grid, area);
        let _ = vt.cursor();
        let _ = vt.modes();
        let _ = vt.take_events();
        let history = i32::try_from(vt.history_size()).unwrap_or(0);
        let _ = vt.line_text(-history);
    }
});
