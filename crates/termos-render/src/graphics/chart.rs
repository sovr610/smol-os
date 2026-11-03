// SPDX-License-Identifier: GPL-3.0-or-later
//! Charts as pictures (D-090), for terminals that show images: the same
//! data as the braille [`super::braille`] charts, drawn in pixels with a
//! smooth line, a soft fill beneath it and faint guide lines. The
//! background is transparent, so the cells' colour shows through.

use crate::color::Rgb;

use super::{GraphicsError, Picture};

/// One line of an [`area_chart`].
#[derive(Debug, Clone, Copy)]
pub struct ChartSeries<'a> {
    pub data: &'a [f64],
    pub color: Rgb,
    /// Fill beneath the line, fading downwards.
    pub fill: bool,
}

/// Width of the lines, in pixels.
const LINE: f32 = 2.0;

/// An RGBA canvas that blends what is drawn over what is there.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            px: vec![0; (w as usize) * (h as usize) * 4],
        }
    }

    /// `color` at opacity `alpha` (0 to 1) over the pixel at (`x`, `y`).
    fn blend(&mut self, x: u32, y: u32, color: Rgb, alpha: f32) {
        if x >= self.w || y >= self.h || alpha <= 0.0 {
            return;
        }
        let a = alpha.min(1.0);
        let i = ((y * self.w + x) * 4) as usize;
        let dst_a = f32::from(self.px[i + 3]) / 255.0;
        let out_a = a + dst_a * (1.0 - a);
        let mix = |src: u8, dst: u8| -> u8 {
            let v = (f32::from(src) * a + f32::from(dst) * dst_a * (1.0 - a)) / out_a;
            // In range: a weighted mean of two u8 values.
            v.round().clamp(0.0, 255.0) as u8
        };
        self.px[i] = mix(color.r, self.px[i]);
        self.px[i + 1] = mix(color.g, self.px[i + 1]);
        self.px[i + 2] = mix(color.b, self.px[i + 2]);
        // In range: out_a is within 0..=1.
        self.px[i + 3] = (out_a * 255.0).round() as u8;
    }
}

/// The value at pixel column `x` of `w`: the data stretched over the
/// width (newest at the right), linearly between samples.
fn sample(data: &[f64], x: u32, w: u32) -> f64 {
    match data.len() {
        0 => 0.0,
        1 => data[0],
        n => {
            let pos = f64::from(x) / f64::from((w - 1).max(1)) * (n - 1) as f64;
            // In range: pos is within 0..=n-1.
            let i = (pos.floor() as usize).min(n - 2);
            let t = pos - i as f64;
            data[i] * (1.0 - t) + data[i + 1] * t
        }
    }
}

/// Lines (and fills) of `series` over a `w`×`h` transparent picture,
/// scaled to `max`, with guide lines in `guide` at each quarter.
pub fn area_chart(
    series: &[ChartSeries<'_>],
    max: f64,
    guide: Rgb,
    w: u32,
    h: u32,
) -> Result<Picture, GraphicsError> {
    if w == 0 || h == 0 {
        return Err(GraphicsError::TooLarge {
            width: w,
            height: h,
        });
    }
    let mut c = Canvas::new(w, h);
    for q in 1..4 {
        let y = h * q / 4;
        for x in (0..w).step_by(2) {
            c.blend(x, y, guide, 0.35);
        }
    }
    let top = if max > 0.0 { max } else { 1.0 };
    let bottom = (h - 1) as f32;
    // In range: a fraction of the height.
    let y_of = |v: f64| bottom - ((v.max(0.0) / top).min(1.0) as f32) * bottom;
    for s in series {
        if s.data.is_empty() {
            continue;
        }
        let ys: Vec<f32> = (0..w).map(|x| y_of(sample(s.data, x, w))).collect();
        if s.fill {
            for (x, &y) in (0u32..).zip(&ys) {
                // In range: y is within the height.
                let start = y.ceil() as u32;
                for py in start..h {
                    let depth = (py as f32 - y) / (bottom - y).max(1.0);
                    c.blend(x, py, s.color, 0.30 * (1.0 - depth) + 0.04);
                }
            }
        }
        for (x, &y) in (0u32..).zip(&ys) {
            // Join to the previous column so steep changes stay solid.
            let prev = if x == 0 { y } else { ys[x as usize - 1] };
            let (lo, hi) = (prev.min(y) - LINE / 2.0, prev.max(y) + LINE / 2.0);
            // In range: within the picture (clamped below).
            let (from, to) = (lo.floor().max(0.0) as u32, hi.ceil().min(bottom) as u32);
            for py in from..=to {
                // Soft edges: partial coverage at the ends of the span.
                let p = py as f32;
                let cover = (hi.min(p + 1.0) - lo.max(p)).clamp(0.0, 1.0);
                c.blend(x, py, s.color, cover);
            }
        }
    }
    Picture::from_rgba(w, h, c.px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(p: &Picture, x: u32, y: u32) -> u8 {
        p.rgba()[((y * p.width() + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_rising_line_with_its_fill() {
        let data = [0.0, 50.0, 100.0];
        let blue = Rgb::new(0, 0, 255);
        let p = area_chart(
            &[ChartSeries {
                data: &data,
                color: blue,
                fill: true,
            }],
            100.0,
            Rgb::new(128, 128, 128),
            40,
            20,
        )
        .expect("drawn");
        assert_eq!((p.width(), p.height()), (40, 20));
        // The line: bottom left, top right, opaque.
        assert_eq!(alpha(&p, 0, 19), 255);
        assert_eq!(alpha(&p, 39, 0), 255);
        // Above the line at the left: nothing but a guide line.
        assert_eq!(alpha(&p, 1, 2), 0);
        // Below the line on the right: the fill, fainter than the line.
        let fill = alpha(&p, 38, 10);
        assert!(fill > 0 && fill < 255, "{fill}");
        // Guide lines at the quarters, faint.
        let g = alpha(&p, 2, 5);
        assert!(g > 0 && g < 128, "{g}");
    }

    #[test]
    fn values_are_clamped_and_empty_data_is_blank() {
        let p = area_chart(
            &[ChartSeries {
                data: &[500.0, -3.0],
                color: Rgb::new(255, 0, 0),
                fill: false,
            }],
            100.0,
            Rgb::new(0, 0, 0),
            10,
            10,
        )
        .expect("drawn");
        assert_eq!(alpha(&p, 0, 0), 255, "over the top: at the top");
        assert_eq!(alpha(&p, 9, 9), 255, "below zero: at the bottom");
        let blank = area_chart(
            &[ChartSeries {
                data: &[],
                color: Rgb::new(255, 0, 0),
                fill: true,
            }],
            0.0,
            Rgb::new(0, 0, 0),
            8,
            8,
        )
        .expect("drawn");
        assert!(blank.rgba().chunks(4).all(|px| px[3] < 128), "guides only");
        assert!(area_chart(&[], 1.0, Rgb::new(0, 0, 0), 0, 4).is_err());
    }
}
