// SPDX-License-Identifier: GPL-3.0-or-later
//! Benchmarks for the hot output path: diffing and encoding a large screen.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use termos_render::caps::Caps;
use termos_render::cell::Style;
use termos_render::color::{Color, ColorDepth};
use termos_render::diff::{diff, full};
use termos_render::encode::Encoder;
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

criterion_group!(render, benches);
criterion_main!(render);
