// SPDX-License-Identifier: GPL-3.0-or-later
//! Benchmarks for the hot output path: diffing and encoding a large screen.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use termos_render::caps::Caps;
use termos_render::cell::Style;
use termos_render::color::{Color, ColorDepth};
use termos_render::diff::{diff, full};
use termos_render::encode::Encoder;
use termos_render::graphics::Picture;
use termos_render::graphics::blocks::{self, BlockMode};
use termos_render::grid::Grid;

const COLS: u16 = 250;
const ROWS: u16 = 70;

fn busy_grid(seed: u8) -> Grid {
    let mut g = Grid::new(COLS, ROWS);
    for y in 0..ROWS {
        let fg = Color::rgb(seed.wrapping_add(y as u8), 100, 200);
        let line = format!("{y:>3} the quick brown fox jumps over the lazy dog {seed} ").repeat(5);
        g.put_str(0, y, &line, Style::new().fg(fg), COLS);
    }
    g
}

fn benches(c: &mut Criterion) {
    let a = busy_grid(1);
    let b = busy_grid(2);
    let mut one_cell = a.clone();
    one_cell.put_str(10, 10, "#", Style::new(), COLS);
    let caps = Caps {
        color: ColorDepth::Truecolor,
        ..Caps::default()
    };

    c.bench_function("diff/full-change", |bn| {
        bn.iter(|| diff(black_box(&a), black_box(&b)))
    });
    c.bench_function("diff/one-cell", |bn| {
        bn.iter(|| diff(black_box(&a), black_box(&one_cell)))
    });
    c.bench_function("encode/full-frame", |bn| {
        let spans = full(&a);
        let mut enc = Encoder::new(&caps);
        bn.iter(|| {
            enc.invalidate();
            black_box(enc.frame(COLS, black_box(&spans), None, true).len())
        });
    });
}

/// A 1280×720 photo-like picture (smooth gradients plus detail).
fn photo() -> Picture {
    let (w, h) = (1280u32, 720u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = ((x ^ y) & 0xff) as u8;
            rgba.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, v, 255]);
        }
    }
    Picture::from_rgba(w, h, rgba).expect("valid picture")
}

fn graphics(c: &mut Criterion) {
    let pic = photo();
    let bg = termos_render::color::Rgb::BLACK;
    // An image filling most of a large terminal.
    c.bench_function("blocks/quadrant-1280x720-to-200x60", |bn| {
        bn.iter(|| {
            black_box(blocks::render(
                black_box(&pic),
                200,
                60,
                BlockMode::Quadrant,
                bg,
            ))
        })
    });
    c.bench_function("blocks/half-1280x720-to-200x60", |bn| {
        bn.iter(|| {
            black_box(blocks::render(
                black_box(&pic),
                200,
                60,
                BlockMode::Half,
                bg,
            ))
        })
    });
    c.bench_function("picture/resize-1280x720-to-400x240", |bn| {
        bn.iter(|| black_box(pic.resize(400, 240)))
    });
}

criterion_group!(render, benches, graphics);
criterion_main!(render);
