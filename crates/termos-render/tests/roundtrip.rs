// SPDX-License-Identifier: GPL-3.0-or-later
//! Round-trip property: encoding a grid and feeding the bytes to an
//! independent emulator (`vt100`) reproduces the grid exactly, both for full
//! frames and for incremental diffs.

use proptest::prelude::*;
use termos_render::caps::Caps;
use termos_render::cell::{Attrs, Style};
use termos_render::color::{Color, ColorDepth};
use termos_render::diff::{diff, full};
use termos_render::encode::Encoder;
use termos_render::grid::Grid;

const COLS: u16 = 12;
const ROWS: u16 = 4;

fn color() -> impl Strategy<Value = Color> {
    prop_oneof![
        Just(Color::Default),
        any::<u8>().prop_map(Color::Indexed),
        (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| Color::rgb(r, g, b)),
    ]
}

/// Attributes vt100 can report back. vt100 models bold and dim as one
/// "intensity" value (the last SGR wins), so the generator never sets both.
fn attrs() -> impl Strategy<Value = Attrs> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(b, d, i, u, r)| {
            let mut a = Attrs::empty();
            a.set(Attrs::BOLD, b);
            a.set(Attrs::DIM, d && !b);
            a.set(Attrs::ITALIC, i);
            a.set(Attrs::UNDERLINE, u);
            a.set(Attrs::REVERSE, r);
            a
        })
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            8 => prop::char::range('a', 'z').prop_map(String::from),
            1 => Just("界".to_owned()),
            1 => Just("é".to_owned()),
            1 => Just("─".to_owned()),
        ],
        0..8,
    )
    .prop_map(|v| v.concat())
}

fn op() -> impl Strategy<Value = (u16, u16, String, Style)> {
    (0..COLS, 0..ROWS, text(), color(), color(), attrs()).prop_map(|(x, y, t, fg, bg, a)| {
        let mut s = Style::new().fg(fg).bg(bg);
        s.attrs = a;
        (x, y, t, s)
    })
}

fn grid_from(ops: &[(u16, u16, String, Style)]) -> Grid {
    let mut g = Grid::new(COLS, ROWS);
    for (x, y, t, s) in ops {
        g.put_str(*x, *y, t, *s, COLS);
    }
    g
}

fn vt_color(c: Color) -> vt100::Color {
    match c {
        Color::Default => vt100::Color::Default,
        Color::Indexed(i) => vt100::Color::Idx(i),
        Color::Rgb(rgb) => vt100::Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn assert_screen_matches(screen: &vt100::Screen, g: &Grid) {
    for y in 0..ROWS {
        for x in 0..COLS {
            let ours = g.get(x, y).expect("in bounds");
            let theirs = screen.cell(y, x).expect("in bounds");
            if ours.is_continuation() {
                assert!(theirs.is_wide_continuation(), "({x},{y}) continuation");
                continue;
            }
            // Every cell is written after the initial clear, so blanks are
            // explicit spaces on both sides.
            assert_eq!(
                theirs.contents(),
                ours.display_symbol(),
                "text at ({x},{y})"
            );
            assert_eq!(theirs.fgcolor(), vt_color(ours.style.fg), "fg at ({x},{y})");
            assert_eq!(theirs.bgcolor(), vt_color(ours.style.bg), "bg at ({x},{y})");
            let a = ours.style.attrs;
            assert_eq!(theirs.bold(), a.contains(Attrs::BOLD), "bold at ({x},{y})");
            assert_eq!(theirs.dim(), a.contains(Attrs::DIM), "dim at ({x},{y})");
            assert_eq!(
                theirs.italic(),
                a.contains(Attrs::ITALIC),
                "italic at ({x},{y})"
            );
            assert_eq!(
                theirs.underline(),
                a.contains(Attrs::UNDERLINE),
                "ul at ({x},{y})"
            );
            assert_eq!(
                theirs.inverse(),
                a.contains(Attrs::REVERSE),
                "rev at ({x},{y})"
            );
        }
    }
}

fn parser() -> vt100::Parser {
    let nz = |v: u16| std::num::NonZeroU16::new(v).expect("non-zero size");
    vt100::Parser::new(nz(ROWS), nz(COLS), 0)
}

fn caps() -> Caps {
    Caps {
        color: ColorDepth::Truecolor,
        sync_output: true,
        ..Caps::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn full_frame_round_trips(ops in prop::collection::vec(op(), 0..12)) {
        let g = grid_from(&ops);
        let mut enc = Encoder::new(&caps());
        let mut vt = parser();
        vt.process(enc.frame(COLS, &full(&g), None, true));
        assert_screen_matches(vt.screen(), &g);
    }

    #[test]
    fn incremental_frames_round_trip(
        first in prop::collection::vec(op(), 0..8),
        second in prop::collection::vec(op(), 0..8),
        third in prop::collection::vec(op(), 0..8),
    ) {
        let a = grid_from(&first);
        let mut b = a.clone();
        for (x, y, t, s) in &second {
            b.put_str(*x, *y, t, *s, COLS);
        }
        let mut c = b.clone();
        for (x, y, t, s) in &third {
            c.put_str(*x, *y, t, *s, COLS);
        }
        let mut enc = Encoder::new(&caps());
        let mut vt = parser();
        vt.process(enc.frame(COLS, &full(&a), None, true));
        vt.process(enc.frame(COLS, &diff(&a, &b), None, false));
        assert_screen_matches(vt.screen(), &b);
        vt.process(enc.frame(COLS, &diff(&b, &c), None, false));
        assert_screen_matches(vt.screen(), &c);
    }
}
