// SPDX-License-Identifier: GPL-3.0-or-later
//! Images (ARCHITECTURE.md §5.8): decoding with limits, scaling, and the
//! cell-based fallbacks every terminal can show.
//!
//! - [`Picture`]: an RGBA image, decoded from png/jpeg/gif/webp/bmp with
//!   bounded size and memory.
//! - [`blocks`]: half blocks (1×2 pixels per cell) or quadrants (2×2, two
//!   colours per cell), for photos.
//! - [`braille`]: 2×4 dots per cell in one colour, for line art and
//!   charts.
//!
//! The pixel protocols encode a [`Picture`] for a terminal: [`kitty`],
//! [`sixel`] (which also decodes, for Sixel printed inside panes) and
//! [`iterm2`].

pub mod blocks;
pub mod braille;
pub mod chart;
pub mod iterm2;
pub mod kitty;
pub mod sixel;

use std::fmt;
use std::io::Cursor;

use crate::caps::PxSize;
use crate::color::Rgb;

/// Largest image we decode, per side.
pub const MAX_SIDE: u32 = 8192;
/// Most memory one decode may allocate.
pub const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// Why an image cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphicsError {
    #[error("cannot decode the image: {0}")]
    Decode(String),
    #[error("the image is too large ({width}×{height}, at most {MAX_SIDE} per side)")]
    TooLarge { width: u32, height: u32 },
    #[error("pixel data does not match the size {width}×{height}")]
    BadSize { width: u32, height: u32 },
}

/// An RGBA image (8 bits per channel, not premultiplied).
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Picture({}×{})", self.width, self.height)
    }
}

impl Picture {
    /// Wraps RGBA bytes (`width * height * 4` of them).
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, GraphicsError> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(GraphicsError::TooLarge { width, height });
        }
        let expected = u64::from(width) * u64::from(height) * 4;
        if rgba.len() as u64 != expected {
            return Err(GraphicsError::BadSize { width, height });
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Decodes png, jpeg, gif (its first frame), webp or bmp, refusing
    /// images over [`MAX_SIDE`] or needing more than [`MAX_DECODE_BYTES`].
    pub fn decode(bytes: &[u8]) -> Result<Self, GraphicsError> {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| GraphicsError::Decode(e.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(MAX_DECODE_BYTES);
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| GraphicsError::Decode(e.to_string()))?;
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        Self::from_rgba(width, height, rgba.into_raw())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The RGBA bytes, row by row.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// One pixel as `[r, g, b, a]`; transparent outside the picture.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let i = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// The picture scaled to `width`×`height` by area averaging: each
    /// output pixel is the mean of the source area it covers (weighted by
    /// coverage, in premultiplied alpha). Edges stay sharp at whole-number
    /// ratios, and heavy reductions stay smooth.
    pub fn resize(&self, width: u32, height: u32) -> Picture {
        let (width, height) = (width.clamp(1, MAX_SIDE), height.clamp(1, MAX_SIDE));
        if (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let (sw, sh) = (self.width as usize, self.height as usize);
        let (dw, dh) = (width as usize, height as usize);
        // Premultiplied, so transparent pixels do not bleed their colour.
        let src: Vec<[f32; 4]> = self
            .rgba
            .chunks_exact(4)
            .map(|p| {
                let a = f32::from(p[3]) / 255.0;
                [
                    f32::from(p[0]) * a,
                    f32::from(p[1]) * a,
                    f32::from(p[2]) * a,
                    f32::from(p[3]),
                ]
            })
            .collect();
        let wx = area_weights(sw, dw);
        let wy = area_weights(sh, dh);
        // Horizontal pass: sh rows of dw pixels.
        let mut tmp = vec![[0f32; 4]; dw * sh];
        for y in 0..sh {
            for (x, ws) in wx.iter().enumerate() {
                let mut acc = [0f32; 4];
                for &(sx, w) in ws {
                    let p = src[y * sw + sx];
                    for c in 0..4 {
                        acc[c] += p[c] * w;
                    }
                }
                tmp[y * dw + x] = acc;
            }
        }
        // Vertical pass, then back to straight alpha.
        let mut rgba = Vec::with_capacity(dw * dh * 4);
        for ws in &wy {
            for x in 0..dw {
                let mut acc = [0f32; 4];
                for &(sy, w) in ws {
                    let p = tmp[sy * dw + x];
                    for c in 0..4 {
                        acc[c] += p[c] * w;
                    }
                }
                let a = acc[3];
                let un = |v: f32| -> u8 {
                    if a <= 0.0 {
                        0
                    } else {
                        // In range: a weighted mean of u8 values.
                        (v * 255.0 / a).round().clamp(0.0, 255.0) as u8
                    }
                };
                rgba.extend_from_slice(&[
                    un(acc[0]),
                    un(acc[1]),
                    un(acc[2]),
                    a.round().clamp(0.0, 255.0) as u8,
                ]);
            }
        }
        Picture {
            width,
            height,
            rgba,
        }
    }

    /// The picture as PNG (for the Kitty and iTerm2 protocols).
    pub fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        // Encoding valid RGBA into memory cannot fail; an empty result
        // would only show nothing.
        let _ = image::ImageEncoder::write_image(
            encoder,
            &self.rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        );
        out
    }

    /// A part of the picture (clamped to it).
    pub fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> Picture {
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        let width = width.clamp(1, self.width - x);
        let height = height.clamp(1, self.height - y);
        let mut rgba = Vec::with_capacity((u64::from(width) * u64::from(height) * 4) as usize);
        for row in y..y + height {
            let start = ((u64::from(row) * u64::from(self.width) + u64::from(x)) * 4) as usize;
            rgba.extend_from_slice(&self.rgba[start..start + width as usize * 4]);
        }
        Picture {
            width,
            height,
            rgba,
        }
    }
}

/// For each of `dst` output pixels, the source pixels it covers and
/// their weights (summing to 1).
fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let ratio = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let start = i as f64 * ratio;
            let end = (i + 1) as f64 * ratio;
            let first = start.floor() as usize;
            let last = (end.ceil() as usize).min(src);
            (first..last.max(first + 1).min(src))
                .map(|s| {
                    let covered = end.min(s as f64 + 1.0) - start.max(s as f64);
                    (s, (covered / (end - start)) as f32)
                })
                .collect()
        })
        .collect()
}

/// A cell's size in pixels when the terminal did not say: a typical
/// 10×20 font (only the 1:2 ratio matters for the cell-based fallbacks).
pub const DEFAULT_CELL: PxSize = PxSize {
    width: 10,
    height: 20,
};

/// How many cells an image of `width`×`height` pixels takes when fitted
/// into `max_cols`×`max_rows`, keeping its aspect ratio for cells of
/// `cell` pixels. Never upscales past the image's own pixel size unless
/// `upscale`; always at least 1×1.
pub fn fit_cells(
    width: u32,
    height: u32,
    cell: PxSize,
    max_cols: u16,
    max_rows: u16,
    upscale: bool,
) -> (u16, u16) {
    if width == 0 || height == 0 || max_cols == 0 || max_rows == 0 {
        return (1.min(max_cols), 1.min(max_rows));
    }
    let (cw, ch) = (f64::from(cell.width.max(1)), f64::from(cell.height.max(1)));
    let (w, h) = (f64::from(width), f64::from(height));
    let mut scale = (f64::from(max_cols) * cw / w).min(f64::from(max_rows) * ch / h);
    if !upscale {
        scale = scale.min(1.0);
    }
    // In range: at most max_cols / max_rows, which are u16.
    let cols = ((w * scale / cw).round() as u16).clamp(1, max_cols);
    let rows = ((h * scale / ch).round() as u16).clamp(1, max_rows);
    (cols, rows)
}

/// Mixes a pixel over `bg` by its alpha.
pub(crate) fn over(p: [u8; 4], bg: Rgb) -> Rgb {
    let a = u16::from(p[3]);
    let mix = |c: u8, b: u8| -> u8 {
        // At most 255: a convex combination of two u8 values.
        ((u16::from(c) * a + u16::from(b) * (255 - a) + 127) / 255) as u8
    };
    Rgb::new(mix(p[0], bg.r), mix(p[1], bg.g), mix(p[2], bg.b))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A test picture from a function of the pixel position.
    pub(crate) fn picture(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Picture {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
            }
        }
        Picture::from_rgba(w, h, rgba).expect("valid size")
    }

    #[test]
    fn decodes_png_and_refuses_junk_and_giants() {
        let mut png = Vec::new();
        let src = image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([
                u8::try_from(x * 80).unwrap_or(0),
                u8::try_from(y * 100).unwrap_or(0),
                7,
                255,
            ])
        });
        src.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode");
        let p = Picture::decode(&png).expect("decodes");
        assert_eq!((p.width(), p.height()), (3, 2));
        assert_eq!(p.pixel(2, 1), [160, 100, 7, 255]);
        assert_eq!(p.pixel(9, 9), [0, 0, 0, 0]);
        assert!(matches!(
            Picture::decode(b"not an image"),
            Err(GraphicsError::Decode(_))
        ));
        assert_eq!(
            Picture::from_rgba(MAX_SIDE + 1, 1, Vec::new()),
            Err(GraphicsError::TooLarge {
                width: MAX_SIDE + 1,
                height: 1
            })
        );
        assert!(matches!(
            Picture::from_rgba(2, 2, vec![0; 3]),
            Err(GraphicsError::BadSize { .. })
        ));
    }

    #[test]
    fn png_round_trip() {
        let p = picture(5, 3, |x, y| {
            [
                u8::try_from(x * 50).unwrap_or(0),
                9,
                u8::try_from(y * 80).unwrap_or(0),
                200,
            ]
        });
        assert_eq!(Picture::decode(&p.to_png()), Ok(p));
    }

    #[test]
    fn resize_and_crop() {
        let p = picture(4, 4, |x, _| {
            if x < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        });
        let small = p.resize(2, 2);
        assert_eq!((small.width(), small.height()), (2, 2));
        assert_eq!(small.pixel(0, 0)[0], 255, "left stays red");
        assert_eq!(small.pixel(1, 1)[2], 255, "right stays blue");
        let c = p.crop(2, 1, 9, 9);
        assert_eq!((c.width(), c.height()), (2, 3));
        assert_eq!(c.pixel(0, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn fitting_keeps_the_aspect_ratio() {
        let cell = PxSize {
            width: 10,
            height: 20,
        };
        // 800×400 into 80×24 cells of 10×20: width-limited, 80×20.
        assert_eq!(fit_cells(800, 400, cell, 80, 24, false), (80, 20));
        // A small image is not blown up unless asked.
        assert_eq!(fit_cells(100, 100, cell, 80, 24, false), (10, 5));
        assert_eq!(fit_cells(100, 100, cell, 80, 24, true), (48, 24));
        assert_eq!(fit_cells(1, 10_000, cell, 80, 24, false), (1, 24));
        assert_eq!(fit_cells(0, 10, cell, 80, 24, false), (1, 1));
    }

    #[test]
    fn alpha_blends_over_the_background() {
        let bg = Rgb::new(0, 0, 0);
        assert_eq!(over([255, 255, 255, 255], bg), Rgb::new(255, 255, 255));
        assert_eq!(over([255, 255, 255, 0], bg), bg);
        assert_eq!(over([200, 100, 0, 128], bg), Rgb::new(100, 50, 0));
    }
}
