// SPDX-License-Identifier: GPL-3.0-or-later
//! Sixel graphics: an encoder for terminals that show Sixel (foot, xterm,
//! WezTerm, Windows Terminal, …) and a decoder for Sixel images programs
//! print inside panes.
//!
//! The encoder uses at most 256 colours: an image with that few is encoded
//! exactly (up to Sixel's percent precision), others are reduced by median
//! cut. Transparent pixels are not drawn (`P2 = 1`), so what is behind them
//! shows through. The decoder bounds its memory by the pixels actually
//! drawn, not by the size an image claims.

use std::collections::HashMap;

use crate::color::{Rgb, xterm_256_to_rgb};
use crate::graphics::{GraphicsError, Picture};

/// Encodes `pic` as a complete Sixel DCS sequence (ESC P … ESC \).
pub fn encode(pic: &Picture) -> Vec<u8> {
    let (w, h) = (pic.width(), pic.height());
    let (palette, index) = quantize(pic);
    let mut out = format!("\x1bP0;1;0q\"1;1;{w};{h}").into_bytes();
    let pct = |v: u8| (u32::from(v) * 100 + 127) / 255;
    for (i, c) in palette.iter().enumerate() {
        out.extend_from_slice(format!("#{i};2;{};{};{}", pct(c.r), pct(c.g), pct(c.b)).as_bytes());
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut row = vec![0u8; wu];
    for band in (0..hu).step_by(6) {
        // The colours used in this band, in palette order.
        let mut used = vec![false; palette.len()];
        for y in band..(band + 6).min(hu) {
            for x in 0..wu {
                if let Some(c) = index[y * wu + x] {
                    used[usize::from(c)] = true;
                }
            }
        }
        let mut first = true;
        for (color, _) in used.iter().enumerate().filter(|(_, u)| **u) {
            if !first {
                out.push(b'$');
            }
            first = false;
            out.extend_from_slice(format!("#{color}").as_bytes());
            for (x, bits) in row.iter_mut().enumerate() {
                *bits = 0;
                for dy in 0..6 {
                    let y = band + dy;
                    if y < hu && index[y * wu + x] == Some(u8::try_from(color).unwrap_or(0)) {
                        *bits |= 1 << dy;
                    }
                }
            }
            // Trailing empty sixels need not be sent.
            let len = row.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
            run_length(&row[..len], &mut out);
        }
        if band + 6 < hu {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Appends sixel characters for `bits`, with `!n` repeats for runs of 4+.
fn run_length(bits: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < bits.len() {
        let b = bits[i];
        let run = bits[i..].iter().take_while(|x| **x == b).count();
        let ch = 63 + b;
        if run >= 4 {
            out.extend_from_slice(format!("!{run}").as_bytes());
            out.push(ch);
        } else {
            out.extend(std::iter::repeat_n(ch, run));
        }
        i += run;
    }
}

/// A palette of at most 256 colours and each pixel's entry (`None` for
/// transparent pixels).
fn quantize(pic: &Picture) -> (Vec<Rgb>, Vec<Option<u8>>) {
    let opaque = |p: [u8; 4]| p[3] >= 128;
    let mut exact: HashMap<Rgb, u8> = HashMap::new();
    let mut palette = Vec::new();
    let mut fits = true;
    for p in pic.rgba().chunks_exact(4) {
        let p = [p[0], p[1], p[2], p[3]];
        if !opaque(p) {
            continue;
        }
        let c = Rgb::new(p[0], p[1], p[2]);
        if let std::collections::hash_map::Entry::Vacant(e) = exact.entry(c) {
            if palette.len() == 256 {
                fits = false;
                break;
            }
            e.insert(u8::try_from(palette.len()).unwrap_or(255));
            palette.push(c);
        }
    }
    if fits {
        let index = pic
            .rgba()
            .chunks_exact(4)
            .map(|p| {
                opaque([p[0], p[1], p[2], p[3]])
                    .then(|| exact.get(&Rgb::new(p[0], p[1], p[2])).copied())
                    .flatten()
            })
            .collect();
        return (palette, index);
    }
    median_cut(pic)
}

/// Median-cut quantization to 256 colours, on 5-bit-per-channel buckets.
fn median_cut(pic: &Picture) -> (Vec<Rgb>, Vec<Option<u8>>) {
    let key = |r: u8, g: u8, b: u8| {
        (u16::from(r >> 3) << 10) | (u16::from(g >> 3) << 5) | u16::from(b >> 3)
    };
    let mut hist: HashMap<u16, (u64, [u64; 3])> = HashMap::new();
    for p in pic.rgba().chunks_exact(4) {
        if p[3] < 128 {
            continue;
        }
        let e = hist.entry(key(p[0], p[1], p[2])).or_default();
        e.0 += 1;
        e.1[0] += u64::from(p[0]);
        e.1[1] += u64::from(p[1]);
        e.1[2] += u64::from(p[2]);
    }
    // Each bucket: its mean colour and pixel count.
    let colors: Vec<([u8; 3], u64)> = hist
        .values()
        .map(|(n, s)| {
            // Means of u8 values are u8 values.
            let m = |v: u64| (v / n) as u8;
            ([m(s[0]), m(s[1]), m(s[2])], *n)
        })
        .collect();
    let mut boxes: Vec<Vec<([u8; 3], u64)>> = vec![colors];
    while boxes.len() < 256 {
        // Split the box with the widest channel range.
        let Some((bi, ch, _)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .flat_map(|(i, b)| {
                (0..3).map(move |c| {
                    let lo = b.iter().map(|e| e.0[c]).min().unwrap_or(0);
                    let hi = b.iter().map(|e| e.0[c]).max().unwrap_or(0);
                    (i, c, hi - lo)
                })
            })
            .max_by_key(|t| t.2)
        else {
            break;
        };
        let mut b = boxes.swap_remove(bi);
        b.sort_by_key(|e| e.0[ch]);
        let total: u64 = b.iter().map(|e| e.1).sum();
        let mut acc = 0;
        let mut cut = 1;
        for (i, e) in b.iter().enumerate() {
            acc += e.1;
            if acc * 2 >= total {
                cut = (i + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let rest = b.split_off(cut);
        boxes.push(b);
        boxes.push(rest);
    }
    let palette: Vec<Rgb> = boxes
        .iter()
        .map(|b| {
            let n: u64 = b.iter().map(|e| e.1).sum::<u64>().max(1);
            let m = |c: usize| (b.iter().map(|e| u64::from(e.0[c]) * e.1).sum::<u64>() / n) as u8;
            Rgb::new(m(0), m(1), m(2))
        })
        .collect();
    let nearest = |c: Rgb| -> u8 {
        let d = |p: &Rgb| {
            let x = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
            x(p.r, c.r) + x(p.g, c.g) + x(p.b, c.b)
        };
        let i = palette
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| d(p))
            .map_or(0, |(i, _)| i);
        u8::try_from(i).unwrap_or(255)
    };
    let mut cache: HashMap<u16, u8> = HashMap::new();
    let index = pic
        .rgba()
        .chunks_exact(4)
        .map(|p| {
            (p[3] >= 128).then(|| {
                *cache
                    .entry(key(p[0], p[1], p[2]))
                    .or_insert_with(|| nearest(Rgb::new(p[0], p[1], p[2])))
            })
        })
        .collect();
    (palette, index)
}

/// Decodes Sixel data (what follows `q` in the DCS sequence, without the
/// terminating ESC \). Pixels never drawn are transparent. Fails when the
/// image would exceed `max_side` per side or `max_pixels` in all.
pub fn decode(data: &[u8], max_side: u32, max_pixels: u64) -> Result<Picture, GraphicsError> {
    let mut palette: Vec<Rgb> = (0..=255u8).map(xterm_256_to_rgb).collect();
    let mut color = 0u8;
    let (mut x, mut y) = (0u32, 0u32);
    let (mut declared_w, mut declared_h) = (0u32, 0u32);
    // Pixels as palette entries, one row each, grown as drawn.
    let mut rows: Vec<Vec<Option<u8>>> = Vec::new();
    let mut drawn: u64 = 0;
    let too_large = |w: u32, h: u32| GraphicsError::TooLarge {
        width: w,
        height: h,
    };
    let mut i = 0;
    let number = |i: &mut usize| -> u32 {
        let mut v: u32 = 0;
        while let Some(d) = data.get(*i).filter(|c| c.is_ascii_digit()) {
            v = v.saturating_mul(10).saturating_add(u32::from(d - b'0'));
            *i += 1;
        }
        v
    };
    let params = |i: &mut usize| -> Vec<u32> {
        let mut out = vec![number(i)];
        while data.get(*i) == Some(&b';') {
            *i += 1;
            out.push(number(i));
        }
        out
    };
    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            b'"' => {
                let p = params(&mut i);
                if p.len() >= 4 {
                    declared_w = p[2].min(max_side);
                    declared_h = p[3].min(max_side);
                }
            }
            b'#' => {
                let p = params(&mut i);
                let reg = usize::from(u8::try_from(p[0]).unwrap_or(255));
                if p.len() >= 5 {
                    let rgb = match p[1] {
                        1 => hls(p[2], p[3], p[4]),
                        _ => {
                            let v = |pc: u32| u8::try_from(pc.min(100) * 255 / 100).unwrap_or(255);
                            Rgb::new(v(p[2]), v(p[3]), v(p[4]))
                        }
                    };
                    palette[reg] = rgb;
                }
                color = u8::try_from(reg).unwrap_or(255);
            }
            b'$' => x = 0,
            b'-' => {
                x = 0;
                y = y.saturating_add(6);
            }
            b'!' | 0x3f..=0x7e => {
                let (count, ch) = if c == b'!' {
                    let n = number(&mut i).max(1);
                    match data.get(i) {
                        Some(&ch @ 0x3f..=0x7e) => {
                            i += 1;
                            (n, ch)
                        }
                        _ => continue,
                    }
                } else {
                    (1, c)
                };
                let bits = ch - 63;
                let end = x.saturating_add(count);
                if bits != 0 {
                    if end > max_side || y.saturating_add(6) > max_side.saturating_add(5) {
                        return Err(too_large(end, y.saturating_add(6)));
                    }
                    for dy in 0..6u32 {
                        if bits & (1 << dy) == 0 {
                            continue;
                        }
                        let yy = y + dy;
                        if yy >= max_side {
                            continue;
                        }
                        let ry = yy as usize;
                        if rows.len() <= ry {
                            rows.resize_with(ry + 1, Vec::new);
                        }
                        let row = &mut rows[ry];
                        if row.len() < end as usize {
                            drawn += u64::from(end) - row.len() as u64;
                            if drawn > max_pixels {
                                return Err(too_large(end, yy + 1));
                            }
                            row.resize(end as usize, None);
                        }
                        for px in &mut row[x as usize..end as usize] {
                            *px = Some(color);
                        }
                    }
                }
                x = end.min(max_side);
            }
            _ => {}
        }
    }
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(declared_w as usize)
        .min(max_side as usize);
    let height = rows.len().max(declared_h as usize).min(max_side as usize);
    if width == 0 || height == 0 {
        return Err(GraphicsError::Decode("the Sixel image is empty".into()));
    }
    if (width as u64) * (height as u64) > max_pixels {
        return Err(too_large(width as u32, height as u32));
    }
    let mut rgba = vec![0u8; width * height * 4];
    for (yy, row) in rows.iter().enumerate().take(height) {
        for (xx, px) in row.iter().enumerate().take(width) {
            if let Some(c) = px {
                let rgb = palette[usize::from(*c)];
                let o = (yy * width + xx) * 4;
                rgba[o..o + 4].copy_from_slice(&[rgb.r, rgb.g, rgb.b, 255]);
            }
        }
    }
    Picture::from_rgba(width as u32, height as u32, rgba)
}

/// DEC's HLS (hue 0 is blue, 120 red, 240 green; lightness and saturation
/// in percent) to RGB.
fn hls(h: u32, l: u32, s: u32) -> Rgb {
    // A program can send any hue; reduce it before turning it.
    let h = f64::from((h % 360 + 240) % 360) / 360.0;
    let (l, s) = (f64::from(l.min(100)) / 100.0, f64::from(s.min(100)) / 100.0);
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        // In range: v is within 0..=1.
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Rgb::new(channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::tests::picture;

    /// The data part of an encoded sequence.
    fn data(seq: &[u8]) -> &[u8] {
        let start = seq.iter().position(|b| *b == b'q').expect("q") + 1;
        &seq[start..seq.len() - 2]
    }

    #[test]
    fn golden_bytes() {
        // 2×2: red, blue / transparent, red.
        let p = picture(2, 2, |x, y| match (x, y) {
            (0, 0) | (1, 1) => [255, 0, 0, 255],
            (1, 0) => [0, 0, 255, 255],
            _ => [0, 0, 0, 0],
        });
        assert_eq!(
            String::from_utf8(encode(&p)).expect("ascii"),
            "\x1bP0;1;0q\"1;1;2;2#0;2;100;0;0#1;2;0;0;100#0@A$#1?@\x1b\\"
        );
    }

    #[test]
    fn round_trip() {
        // Colours Sixel's percents represent exactly, some transparency,
        // more than one band and runs long enough to repeat.
        let levels = [0u8, 51, 102, 153, 204, 255];
        let p = picture(23, 13, |x, y| {
            if (x + y) % 7 == 0 {
                [0, 0, 0, 0]
            } else {
                [
                    levels[(x / 4) as usize % 6],
                    levels[(y / 3) as usize % 6],
                    levels[((x + y) / 5) as usize % 6],
                    255,
                ]
            }
        });
        let back = decode(data(&encode(&p)), 1024, 1 << 20).expect("decodes");
        assert_eq!(back, p);
    }

    #[test]
    fn many_colours_are_reduced_to_256() {
        let p = picture(64, 64, |x, y| {
            [(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8, 255]
        });
        let seq = encode(&p);
        // Colour definitions are `#n;2;r;g;b`: four semicolons.
        let regs = String::from_utf8_lossy(&seq)
            .split('#')
            .skip(1)
            .filter(|cmd| cmd.matches(';').count() >= 4)
            .count();
        assert!(regs <= 256 && regs > 128, "{regs} colour registers");
        let back = decode(data(&seq), 1024, 1 << 20).expect("decodes");
        assert_eq!((back.width(), back.height()), (64, 64));
        // Close to the original.
        let err: u64 = p
            .rgba()
            .iter()
            .zip(back.rgba())
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        assert!(
            err / (64 * 64 * 4) < 12,
            "mean error {}",
            err / (64 * 64 * 4)
        );
    }

    #[test]
    fn decoding_is_bounded() {
        // A tiny input that claims a huge size or repeats forever.
        assert!(decode(b"\"1;1;99999;99999#0!999999~", 4096, 1 << 20).is_err());
        assert!(decode(b"!5000~", 4096, 1000).is_err());
        assert!(decode(b"", 4096, 1000).is_err(), "nothing drawn");
        assert!(decode(b"#0;1;0;50;100~", 16, 16).is_ok(), "HLS colours");
        let p = decode(b"#1;2;100;100;100!3~-~", 16, 1000).expect("ok");
        assert_eq!((p.width(), p.height()), (3, 12));
        assert_eq!(p.pixel(2, 5), [255, 255, 255, 255]);
        assert_eq!(p.pixel(2, 6), [0, 0, 0, 0], "not drawn: transparent");
    }

    #[test]
    fn hls_primaries() {
        assert_eq!(hls(0, 50, 100), Rgb::new(0, 0, 255), "DEC hue 0 is blue");
        assert_eq!(hls(120, 50, 100), Rgb::new(255, 0, 0));
        assert_eq!(hls(240, 50, 100), Rgb::new(0, 255, 0));
        assert_eq!(hls(0, 100, 0), Rgb::new(255, 255, 255));
        // Found by the fuzzer: a hue near u32::MAX overflowed.
        assert_eq!(hls(u32::MAX, 50, 100), hls(u32::MAX % 360, 50, 100));
        assert_eq!(hls(360 + 120, 50, 100), Rgb::new(255, 0, 0));
    }
}
