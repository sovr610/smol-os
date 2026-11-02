// SPDX-License-Identifier: GPL-3.0-or-later
//! Benchmarks for the KMS client's drawing: a whole 1080p screen of text,
//! and one changed row (typing), with the glyph cache warm.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use termos_render::cell::Style;
use termos_render::color::{Color, Palette, Rgb, XTERM_ANSI};
use termos_render::grid::Grid;
use termos_render::raster::{Canvas, FontSet, Rasterizer};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

fn benches(c: &mut Criterion) {
    let palette = Palette {
        fg: Rgb::new(220, 220, 220),
        bg: Rgb::new(20, 20, 20),
        ansi: XTERM_ANSI,
    };
    let mut r = Rasterizer::new(FontSet::bundled(), 16.0, palette).expect("bundled font");
    let layout = r.layout(WIDTH, HEIGHT);
    let mut grid = Grid::new(layout.cols, layout.rows);
    for y in 0..layout.rows {
        let fg = Color::rgb(100 + (y as u8 % 100), 180, 220);
        let line = format!("{y:>3} the quick brown fox jumps over the lazy dog │ ").repeat(4);
        grid.put_str(0, y, &line, Style::new().fg(fg), layout.cols);
    }
    let mut pixels = vec![0u32; (WIDTH * HEIGHT) as usize];
    // Warm the glyph cache.
    {
        let mut canvas = Canvas {
            pixels: &mut pixels,
            stride: WIDTH as usize,
            width: WIDTH,
            height: HEIGHT,
        };
        r.paint_all(&mut canvas, layout, &grid);
    }
    c.bench_function("raster/full-1080p", |b| {
        b.iter(|| {
            let mut canvas = Canvas {
                pixels: &mut pixels,
                stride: WIDTH as usize,
                width: WIDTH,
                height: HEIGHT,
            };
            r.paint_all(&mut canvas, layout, black_box(&grid));
        });
    });
    c.bench_function("raster/one-row", |b| {
        b.iter(|| {
            let mut canvas = Canvas {
                pixels: &mut pixels,
                stride: WIDTH as usize,
                width: WIDTH,
                height: HEIGHT,
            };
            r.paint_cells(&mut canvas, layout, black_box(&grid), 0, 10, layout.cols);
        });
    });
}

criterion_group!(raster, benches);
criterion_main!(raster);
