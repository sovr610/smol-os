// SPDX-License-Identifier: GPL-3.0-or-later
//! Screenshots from QEMU (`screendump`, PPM): is the desktop there? And a
//! small PNG writer, so CI keeps the pictures.

use std::collections::HashMap;

/// An RGB picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// `width * height` pixels, row by row.
    pub rgb: Vec<[u8; 3]>,
}

/// Reads a binary PPM (P6, 8 bits per channel).
pub fn parse_ppm(data: &[u8]) -> Result<Picture, String> {
    let mut fields = Vec::new();
    let mut i = 0;
    // Four header fields (magic, width, height, maxval), with comments.
    while fields.len() < 4 {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if data.get(i) == Some(&b'#') {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        if start == i {
            return Err("a truncated PPM header".into());
        }
        fields.push(String::from_utf8_lossy(&data[start..i]).into_owned());
    }
    // One whitespace byte, then the pixels.
    i += 1;
    if fields[0] != "P6" {
        return Err(format!("not a binary PPM ({})", fields[0]));
    }
    let num = |s: &str| {
        s.parse::<u32>()
            .map_err(|_| format!("a bad number in the PPM header: {s}"))
    };
    let (width, height, max) = (num(&fields[1])?, num(&fields[2])?, num(&fields[3])?);
    if max != 255 {
        return Err(format!("{max} levels per channel; only 255 is supported"));
    }
    let n = (width as usize)
        .checked_mul(height as usize)
        .filter(|n| n.checked_mul(3).and_then(|b| b.checked_add(i)).is_some())
        .ok_or("the PPM's size is impossible")?;
    let body = data
        .get(i..i + n * 3)
        .ok_or("the PPM is shorter than its header says")?;
    let rgb = body.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
    Ok(Picture { width, height, rgb })
}

/// What a screenshot shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Findings {
    /// The most common colour: the desktop's background.
    pub background: [u8; 3],
    /// Distinct colours.
    pub colours: usize,
    /// Pixels that are not the background, per mille.
    pub ink_permille: u32,
    /// The rows of the band at the bottom whose colour differs from the
    /// background (the status bar), if there is one.
    pub status_bar: Option<(u32, u32)>,
}

impl Findings {
    /// Problems, if the screenshot does not look like the desktop.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.colours < 2 || self.ink_permille < 5 {
            out.push(format!(
                "the screen is blank ({} colours, {}‰ not background)",
                self.colours, self.ink_permille
            ));
        }
        if self.status_bar.is_none() {
            out.push("no status bar row at the bottom of the screen".into());
        }
        out
    }
}

fn mode(pixels: impl Iterator<Item = [u8; 3]>) -> Option<[u8; 3]> {
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    for p in pixels {
        *counts.entry(p).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c)
}

/// Looks at a screenshot of the desktop.
pub fn inspect(p: &Picture) -> Findings {
    let background = mode(p.rgb.iter().copied()).unwrap_or([0, 0, 0]);
    let mut seen = std::collections::HashSet::new();
    let mut ink = 0u64;
    for px in &p.rgb {
        seen.insert(*px);
        if *px != background {
            ink += 1;
        }
    }
    let total = (p.rgb.len() as u64).max(1);
    let row = |y: u32| {
        p.rgb[(y * p.width) as usize..((y + 1) * p.width) as usize]
            .iter()
            .copied()
    };
    // From the bottom: skip margin rows of the background, then the status
    // bar is a band of rows whose main colour is something else, in the
    // bottom tenth of the screen, at least 8 pixels high.
    let mut status_bar = None;
    let limit = p.height.saturating_sub(p.height / 10);
    let mut y = p.height;
    while y > limit {
        y -= 1;
        if mode(row(y)) != Some(background) {
            let bottom = y + 1;
            let mut top = y;
            while top > 0 && mode(row(top - 1)) != Some(background) && bottom - top < p.height / 10
            {
                top -= 1;
            }
            if bottom - top >= 8 {
                status_bar = Some((top, bottom));
            }
            break;
        }
    }
    Findings {
        background,
        colours: seen.len(),
        ink_permille: (ink * 1000 / total) as u32,
        status_bar,
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut c = 0xffff_ffffu32;
    for b in data {
        c = table[((c ^ u32::from(*b)) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for x in data {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Encodes a PNG (uncompressed deflate: simple, and the files are only
/// kept as CI artifacts, which are zipped anyway).
pub fn png(p: &Picture) -> Vec<u8> {
    let mut raw = Vec::with_capacity(p.rgb.len() * 3 + p.height as usize);
    for y in 0..p.height {
        raw.push(0); // no filter
        for px in &p.rgb[(y * p.width) as usize..((y + 1) * p.width) as usize] {
            raw.extend_from_slice(px);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    if raw.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&p.width.to_be_bytes());
    ihdr.extend_from_slice(&p.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: [u8; 3] = [20, 22, 27];
    const BAR: [u8; 3] = [30, 33, 40];

    /// A 200×100 desktop: text in the middle, a status bar 10 rows high
    /// two rows above the bottom.
    fn desktop(bar: bool, text: bool) -> Picture {
        let (w, h) = (200u32, 100u32);
        let mut rgb = vec![BG; (w * h) as usize];
        if text {
            for y in 20..30 {
                for x in 10..120 {
                    if (x + y) % 3 == 0 {
                        rgb[(y * w + x) as usize] = [208, 208, 208];
                    }
                }
            }
        }
        if bar {
            for y in 88..98 {
                for x in 0..w {
                    rgb[(y * w + x) as usize] = if x % 7 == 0 { [255, 255, 255] } else { BAR };
                }
            }
        }
        Picture {
            width: w,
            height: h,
            rgb,
        }
    }

    fn ppm(p: &Picture) -> Vec<u8> {
        let mut out = format!("P6\n# a comment\n{} {}\n255\n", p.width, p.height).into_bytes();
        for px in &p.rgb {
            out.extend_from_slice(px);
        }
        out
    }

    #[test]
    fn the_desktop_is_recognised() {
        let p = parse_ppm(&ppm(&desktop(true, true))).expect("ppm");
        let f = inspect(&p);
        assert_eq!(f.background, BG);
        assert_eq!(f.status_bar, Some((88, 98)));
        assert!(f.problems().is_empty(), "{:?}", f.problems());
        let blank = inspect(&desktop(false, false));
        assert_eq!(blank.problems().len(), 2, "{:?}", blank.problems());
        let no_bar = inspect(&desktop(false, true));
        assert_eq!(
            no_bar.problems(),
            ["no status bar row at the bottom of the screen"]
        );
    }

    #[test]
    fn bad_ppms_are_refused() {
        assert!(parse_ppm(b"P3\n1 1\n255\n0 0 0").is_err());
        assert!(parse_ppm(b"P6\n2 2\n255\nabc").is_err());
        assert!(parse_ppm(b"P6\n2").is_err());
        assert!(parse_ppm(b"P6\n4294967295 4294967295\n255\nxyz").is_err());
    }

    #[test]
    fn png_is_well_formed() {
        let p = desktop(true, true);
        let data = png(&p);
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&data[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes([data[16], data[17], data[18], data[19]]),
            200
        );
        assert_eq!(&data[data.len() - 8..data.len() - 4], b"IEND");
        // Known values.
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }
}
