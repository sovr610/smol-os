// SPDX-License-Identifier: GPL-3.0-or-later
//! Box drawing (U+2500–U+257F), block elements (U+2580–U+259F) and
//! braille (U+2800–U+28FF), drawn from geometry rather than taken from the
//! font, so lines meet exactly at cell edges whatever the font's metrics
//! (window borders are made of them).

/// An 8-bit coverage mask, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl Mask {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            alpha: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// Coverage at `(x, y)`; 0 outside.
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.alpha[(y as usize) * (self.width as usize) + x as usize]
    }

    fn put_max(&mut self, x: i64, y: i64, a: u8) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        let i = (y as usize) * (self.width as usize) + x as usize;
        self.alpha[i] = self.alpha[i].max(a);
    }

    /// Fills `[x0, x1) × [y0, y1)`, clipped.
    fn rect(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, a: u8) {
        for y in y0.max(0)..y1.min(i64::from(self.height)) {
            for x in x0.max(0)..x1.min(i64::from(self.width)) {
                self.put_max(x, y, a);
            }
        }
    }

    /// Pixels with any coverage.
    pub fn ink(&self) -> usize {
        self.alpha.iter().filter(|a| **a > 0).count()
    }
}

/// Line weights of a box-drawing arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum W {
    None,
    Light,
    Heavy,
    Double,
}

/// The arms of a box-drawing character: left, right, up, down.
fn arms(c: char) -> Option<[W; 4]> {
    use W::{Double as D, Heavy as H, Light as L, None as N};
    let a = match c {
        '─' => [L, L, N, N],
        '━' => [H, H, N, N],
        '│' => [N, N, L, L],
        '┃' => [N, N, H, H],
        '┌' => [N, L, N, L],
        '┍' => [N, H, N, L],
        '┎' => [N, L, N, H],
        '┏' => [N, H, N, H],
        '┐' => [L, N, N, L],
        '┑' => [H, N, N, L],
        '┒' => [L, N, N, H],
        '┓' => [H, N, N, H],
        '└' => [N, L, L, N],
        '┕' => [N, H, L, N],
        '┖' => [N, L, H, N],
        '┗' => [N, H, H, N],
        '┘' => [L, N, L, N],
        '┙' => [H, N, L, N],
        '┚' => [L, N, H, N],
        '┛' => [H, N, H, N],
        '├' => [N, L, L, L],
        '┝' => [N, H, L, L],
        '┞' => [N, L, H, L],
        '┟' => [N, L, L, H],
        '┠' => [N, L, H, H],
        '┡' => [N, H, H, L],
        '┢' => [N, H, L, H],
        '┣' => [N, H, H, H],
        '┤' => [L, N, L, L],
        '┥' => [H, N, L, L],
        '┦' => [L, N, H, L],
        '┧' => [L, N, L, H],
        '┨' => [L, N, H, H],
        '┩' => [H, N, H, L],
        '┪' => [H, N, L, H],
        '┫' => [H, N, H, H],
        '┬' => [L, L, N, L],
        '┭' => [H, L, N, L],
        '┮' => [L, H, N, L],
        '┯' => [H, H, N, L],
        '┰' => [L, L, N, H],
        '┱' => [H, L, N, H],
        '┲' => [L, H, N, H],
        '┳' => [H, H, N, H],
        '┴' => [L, L, L, N],
        '┵' => [H, L, L, N],
        '┶' => [L, H, L, N],
        '┷' => [H, H, L, N],
        '┸' => [L, L, H, N],
        '┹' => [H, L, H, N],
        '┺' => [L, H, H, N],
        '┻' => [H, H, H, N],
        '┼' => [L, L, L, L],
        '┽' => [H, L, L, L],
        '┾' => [L, H, L, L],
        '┿' => [H, H, L, L],
        '╀' => [L, L, H, L],
        '╁' => [L, L, L, H],
        '╂' => [L, L, H, H],
        '╃' => [H, L, H, L],
        '╄' => [L, H, H, L],
        '╅' => [H, L, L, H],
        '╆' => [L, H, L, H],
        '╇' => [H, H, H, L],
        '╈' => [H, H, L, H],
        '╉' => [H, L, H, H],
        '╊' => [L, H, H, H],
        '╋' => [H, H, H, H],
        '═' => [D, D, N, N],
        '║' => [N, N, D, D],
        '╒' => [N, D, N, L],
        '╓' => [N, L, N, D],
        '╔' => [N, D, N, D],
        '╕' => [D, N, N, L],
        '╖' => [L, N, N, D],
        '╗' => [D, N, N, D],
        '╘' => [N, D, L, N],
        '╙' => [N, L, D, N],
        '╚' => [N, D, D, N],
        '╛' => [D, N, L, N],
        '╜' => [L, N, D, N],
        '╝' => [D, N, D, N],
        '╞' => [N, D, L, L],
        '╟' => [N, L, D, D],
        '╠' => [N, D, D, D],
        '╡' => [D, N, L, L],
        '╢' => [L, N, D, D],
        '╣' => [D, N, D, D],
        '╤' => [D, D, N, L],
        '╥' => [L, L, N, D],
        '╦' => [D, D, N, D],
        '╧' => [D, D, L, N],
        '╨' => [L, L, D, N],
        '╩' => [D, D, D, N],
        '╪' => [D, D, L, L],
        '╫' => [L, L, D, D],
        '╬' => [D, D, D, D],
        '╴' => [L, N, N, N],
        '╵' => [N, N, L, N],
        '╶' => [N, L, N, N],
        '╷' => [N, N, N, L],
        '╸' => [H, N, N, N],
        '╹' => [N, N, H, N],
        '╺' => [N, H, N, N],
        '╻' => [N, N, N, H],
        '╼' => [L, H, N, N],
        '╽' => [N, N, L, H],
        '╾' => [H, L, N, N],
        '╿' => [N, N, H, L],
        _ => return None,
    };
    Some(a)
}

/// Dashed lines: (horizontal, heavy, dashes).
fn dashes(c: char) -> Option<(bool, bool, u32)> {
    Some(match c {
        '┄' => (true, false, 3),
        '┅' => (true, true, 3),
        '┆' => (false, false, 3),
        '┇' => (false, true, 3),
        '┈' => (true, false, 4),
        '┉' => (true, true, 4),
        '┊' => (false, false, 4),
        '┋' => (false, true, 4),
        '╌' => (true, false, 2),
        '╍' => (true, true, 2),
        '╎' => (false, false, 2),
        '╏' => (false, true, 2),
        _ => return None,
    })
}

/// Draws `c` into a fresh `width`×`height` mask if it is one of the
/// characters drawn from geometry. `stroke` is the light line width.
pub fn draw(c: char, width: u32, height: u32, stroke: u32) -> Option<Mask> {
    let mut m = Mask::new(width, height);
    let light = i64::from(stroke.max(1));
    let heavy = (light * 2).max(light + 1);
    let (w, h) = (i64::from(width), i64::from(height));
    let (cx, cy) = (w / 2, h / 2);
    if let Some(a) = arms(c) {
        lines(&mut m, a, light, heavy);
        return Some(m);
    }
    if let Some((horizontal, is_heavy, n)) = dashes(c) {
        let t = if is_heavy { heavy } else { light };
        let len = if horizontal { w } else { h };
        let n = i64::from(n);
        // Equal dashes and gaps, a gap after each dash.
        for i in 0..n {
            let start = len * i / n;
            let end = start + (len / n) / 2 + (len / n) % 2;
            if horizontal {
                m.rect(start, cy - t / 2, end.max(start + 1), cy - t / 2 + t, 255);
            } else {
                m.rect(cx - t / 2, start, cx - t / 2 + t, end.max(start + 1), 255);
            }
        }
        return Some(m);
    }
    match c {
        '╭' | '╮' | '╯' | '╰' => {
            arc(&mut m, c, light);
            Some(m)
        }
        '╱' | '╲' | '╳' => {
            if c != '╲' {
                diagonal(&mut m, (0.0, h as f32), (w as f32, 0.0), light as f32);
            }
            if c != '╱' {
                diagonal(&mut m, (0.0, 0.0), (w as f32, h as f32), light as f32);
            }
            Some(m)
        }
        '\u{2580}'..='\u{259f}' => {
            block(&mut m, c);
            Some(m)
        }
        '\u{2800}'..='\u{28ff}' => {
            braille(&mut m, c);
            Some(m)
        }
        _ => None,
    }
}

fn lines(m: &mut Mask, [left, right, up, down]: [W; 4], light: i64, heavy: i64) {
    let (w, h) = (i64::from(m.width), i64::from(m.height));
    let (cx, cy) = (w / 2, h / 2);
    let thick = |a: W| match a {
        W::Heavy => heavy,
        _ => light,
    };
    // The widest crossing line, so arms reach into the junction.
    let hmax = [left, right]
        .iter()
        .filter(|a| **a != W::None)
        .map(|a| {
            if *a == W::Double {
                light * 3
            } else {
                thick(*a)
            }
        })
        .max()
        .unwrap_or(0);
    let vmax = [up, down]
        .iter()
        .filter(|a| **a != W::None)
        .map(|a| {
            if *a == W::Double {
                light * 3
            } else {
                thick(*a)
            }
        })
        .max()
        .unwrap_or(0);
    // Double lines: two light lines `gap` either side of the centre.
    let gap = light;
    let (has_l, has_r, has_u, has_d) = (
        left != W::None,
        right != W::None,
        up != W::None,
        down != W::None,
    );
    // Horizontal arms.
    for (arm, to_right) in [(left, false), (right, true)] {
        match arm {
            W::None => {}
            W::Double => {
                // Top line, then bottom line: each stops at the vertical
                // line on its side when there is one, else runs past the
                // centre to meet the other arm.
                for (y, perp) in [(cy - gap - light / 2, has_u), (cy + gap - light / 2, has_d)] {
                    let (x0, x1) = if to_right {
                        (if perp { cx + gap } else { cx - gap - light / 2 }, w)
                    } else {
                        (
                            0,
                            if perp {
                                cx - gap + light
                            } else {
                                cx + gap + light / 2 + 1
                            },
                        )
                    };
                    m.rect(x0, y, x1, y + light, 255);
                }
            }
            a => {
                let t = thick(a);
                let reach = vmax / 2 + vmax % 2;
                let (x0, x1) = if to_right {
                    (cx - vmax / 2, w)
                } else {
                    (0, cx + reach.max(1))
                };
                m.rect(x0, cy - t / 2, x1, cy - t / 2 + t, 255);
            }
        }
    }
    // Vertical arms.
    for (arm, downward) in [(up, false), (down, true)] {
        match arm {
            W::None => {}
            W::Double => {
                for (x, perp) in [(cx - gap - light / 2, has_l), (cx + gap - light / 2, has_r)] {
                    let (y0, y1) = if downward {
                        (if perp { cy + gap } else { cy - gap - light / 2 }, h)
                    } else {
                        (
                            0,
                            if perp {
                                cy - gap + light
                            } else {
                                cy + gap + light / 2 + 1
                            },
                        )
                    };
                    m.rect(x, y0, x + light, y1, 255);
                }
            }
            a => {
                let t = thick(a);
                let reach = hmax / 2 + hmax % 2;
                let (y0, y1) = if downward {
                    (cy - hmax / 2, h)
                } else {
                    (0, cy + reach.max(1))
                };
                m.rect(cx - t / 2, y0, cx - t / 2 + t, y1, 255);
            }
        }
    }
}

/// Anti-aliased coverage of a point at distance `d` from a line of width `t`.
fn cover(d: f32, t: f32) -> u8 {
    let c = (t / 2.0 + 0.5 - d).clamp(0.0, 1.0);
    (c * 255.0).round() as u8
}

fn arc(m: &mut Mask, c: char, light: i64) {
    let (w, h) = (m.width as f32, m.height as f32);
    // Pixel centres: the line's centre is where straight lines are.
    let t = light as f32;
    let cx = (m.width / 2) as f32 - (light / 2) as f32 + t / 2.0;
    let cy = (m.height / 2) as f32 - (light / 2) as f32 + t / 2.0;
    // The curve stops a line-width short of the edges; straight lines
    // cover the rest, so the edge pixels match `─` and `│` exactly.
    let r = ((w - cx).min(cx).min(h - cy).min(cy) - t).max(1.0);
    // Which way the arc opens: toward the arms it joins.
    let (sx, sy) = match c {
        '╭' => (1.0, 1.0),
        '╮' => (-1.0, 1.0),
        '╯' => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let (ox, oy) = (cx + sx * r, cy + sy * r);
    for y in 0..m.height {
        for x in 0..m.width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (px - ox, py - oy);
            // Only the quarter facing the cell's centre.
            if dx * sx > 0.0 || dy * sy > 0.0 {
                continue;
            }
            let d = ((dx * dx + dy * dy).sqrt() - r).abs();
            m.put_max(i64::from(x), i64::from(y), cover(d, t));
        }
    }
    // Straight on from the arc's ends to the cell edges.
    let li = light;
    let (icx, icy) = (i64::from(m.width / 2), i64::from(m.height / 2));
    let (ox, oy) = (ox.round() as i64, oy.round() as i64);
    if sy > 0.0 {
        m.rect(
            icx - li / 2,
            oy,
            icx - li / 2 + li,
            i64::from(m.height),
            255,
        );
    } else {
        m.rect(icx - li / 2, 0, icx - li / 2 + li, oy, 255);
    }
    if sx > 0.0 {
        m.rect(ox, icy - li / 2, i64::from(m.width), icy - li / 2 + li, 255);
    } else {
        m.rect(0, icy - li / 2, ox, icy - li / 2 + li, 255);
    }
}

fn diagonal(m: &mut Mask, (x0, y0): (f32, f32), (x1, y1): (f32, f32), t: f32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
    for y in 0..m.height {
        for x in 0..m.width {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - x0) * dy - (py - y0) * dx).abs() / len;
            m.put_max(i64::from(x), i64::from(y), cover(d, t));
        }
    }
}

fn block(m: &mut Mask, c: char) {
    let (w, h) = (i64::from(m.width), i64::from(m.height));
    let eighth_h = |n: i64| h * n / 8;
    let eighth_w = |n: i64| w * n / 8;
    let (hw, hh) = (w / 2, h / 2);
    match c {
        '▀' => m.rect(0, 0, w, hh, 255),
        '\u{2581}'..='\u{2588}' => {
            let n = i64::from(u32::from(c) - 0x2580);
            m.rect(0, h - eighth_h(n), w, h, 255);
        }
        '\u{2589}'..='\u{258f}' => {
            let n = 8 - i64::from(u32::from(c) - 0x2588);
            m.rect(0, 0, eighth_w(n), h, 255);
        }
        '▐' => m.rect(hw, 0, w, h, 255),
        '░' | '▒' | '▓' => {
            let a = match c {
                '░' => 64,
                '▒' => 128,
                _ => 192,
            };
            m.rect(0, 0, w, h, a);
        }
        '▔' => m.rect(0, 0, w, eighth_h(1).max(1), 255),
        '▕' => m.rect(w - eighth_w(1).max(1), 0, w, h, 255),
        _ => {
            // Quadrants: upper left, upper right, lower left, lower right.
            let q = match c {
                '▖' => [false, false, true, false],
                '▗' => [false, false, false, true],
                '▘' => [true, false, false, false],
                '▙' => [true, false, true, true],
                '▚' => [true, false, false, true],
                '▛' => [true, true, true, false],
                '▜' => [true, true, false, true],
                '▝' => [false, true, false, false],
                '▞' => [false, true, true, false],
                _ => [false, true, true, true],
            };
            let quads = [
                (0, 0, hw, hh),
                (hw, 0, w, hh),
                (0, hh, hw, h),
                (hw, hh, w, h),
            ];
            for (on, (x0, y0, x1, y1)) in q.into_iter().zip(quads) {
                if on {
                    m.rect(x0, y0, x1, y1, 255);
                }
            }
        }
    }
}

fn braille(m: &mut Mask, c: char) {
    let bits = u32::from(c) - 0x2800;
    // Dot n (bit n) sits at (column, row).
    const DOTS: [(u32, u32); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    let (w, h) = (m.width as f32, m.height as f32);
    let r = (w / 4.0).min(h / 8.0) * 0.75;
    for (bit, (col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let ox = w * (2.0 * *col as f32 + 1.0) / 4.0;
        let oy = h * (2.0 * *row as f32 + 1.0) / 8.0;
        for y in 0..m.height {
            for x in 0..m.width {
                let (dx, dy) = (x as f32 + 0.5 - ox, y as f32 + 0.5 - oy);
                let d = (dx * dx + dy * dy).sqrt();
                let a = ((r + 0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
                m.put_max(i64::from(x), i64::from(y), a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `#` for full, `+` for partial, `.` for none.
    pub(crate) fn art(m: &Mask) -> String {
        let mut s = String::new();
        for y in 0..m.height {
            for x in 0..m.width {
                s.push(match m.at(x, y) {
                    0 => '.',
                    255 => '#',
                    _ => '+',
                });
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn lines_reach_every_edge_they_should() {
        for c in ['─', '│', '┼', '━', '┃', '╋', '═', '║', '╬'] {
            let m = draw(c, 10, 20, 1).expect("box");
            let a = arms(c).expect("arms");
            let left = (0..20).any(|y| m.at(0, y) > 0);
            let right = (0..20).any(|y| m.at(9, y) > 0);
            let up = (0..10).any(|x| m.at(x, 0) > 0);
            let down = (0..10).any(|x| m.at(x, 19) > 0);
            assert_eq!(
                [left, right, up, down],
                a.map(|w| w != W::None),
                "{c}\n{}",
                art(&m)
            );
        }
    }

    #[test]
    fn adjacent_cells_join() {
        // The horizontal line sits on the same rows in every cell, so
        // `──` and `┌─┐` are continuous; a vertical one on the same columns.
        let h = draw('─', 10, 20, 2).expect("box");
        for c in ['┌', '┐', '┬', '┼', '├', '┤', '╭', '╮', '└', '┘'] {
            let m = draw(c, 10, 20, 2).expect("box");
            let edge = if arms(c).is_some_and(|a| a[1] != W::None) || matches!(c, '╭' | '╰') {
                9
            } else {
                0
            };
            for y in 0..20 {
                assert_eq!(
                    m.at(edge, y) > 0,
                    h.at(edge, y) > 0,
                    "{c} row {y}\n{}",
                    art(&m)
                );
            }
        }
        let v = draw('│', 10, 20, 2).expect("box");
        for c in ['┌', '┬', '╭', '├'] {
            let m = draw(c, 10, 20, 2).expect("box");
            for x in 0..10 {
                assert_eq!(
                    m.at(x, 19) > 0,
                    v.at(x, 19) > 0,
                    "{c} column {x}\n{}",
                    art(&m)
                );
            }
        }
    }

    #[test]
    fn heavy_is_thicker_and_double_is_two_lines() {
        let rows = |m: &Mask| (0..m.height).filter(|y| m.at(5, *y) > 0).count();
        let l = draw('─', 10, 20, 1).expect("box");
        let hv = draw('━', 10, 20, 1).expect("box");
        let d = draw('═', 10, 20, 1).expect("box");
        assert_eq!(rows(&l), 1);
        assert!(rows(&hv) > rows(&l));
        assert_eq!(rows(&d), 2);
        insta::assert_snapshot!(art(&draw('╔', 10, 12, 1).expect("box")));
        insta::assert_snapshot!(art(&draw('╬', 10, 12, 1).expect("box")));
    }

    #[test]
    fn arcs_blocks_and_braille() {
        insta::assert_snapshot!(art(&draw('╭', 10, 12, 1).expect("arc")));
        let full = draw('█', 8, 16, 1).expect("block");
        assert_eq!(full.ink(), 8 * 16);
        let upper = draw('▀', 8, 16, 1).expect("block");
        assert!(upper.at(3, 2) == 255 && upper.at(3, 12) == 0);
        let lower_quarter = draw('▂', 8, 16, 1).expect("block");
        assert_eq!(lower_quarter.ink(), 8 * 4);
        let shade = draw('▒', 8, 16, 1).expect("block");
        assert_eq!(shade.at(0, 0), 128);
        let quad = draw('▚', 8, 16, 1).expect("block");
        assert!(quad.at(1, 1) == 255 && quad.at(6, 1) == 0 && quad.at(6, 14) == 255);
        // ⣿ has all eight dots; ⠁ only the top left.
        let all = draw('⣿', 8, 16, 1).expect("braille");
        let one = draw('⠁', 8, 16, 1).expect("braille");
        assert!(all.ink() > one.ink() * 6);
        assert!(one.at(2, 2) > 0 && one.at(6, 14) == 0);
        assert!(draw('a', 8, 16, 1).is_none());
    }
}
