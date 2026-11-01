// SPDX-License-Identifier: GPL-3.0-or-later
//! Images programs show in a pane: the Kitty graphics commands and Sixel
//! images the interceptor takes out of their output, kept per pane within
//! limits.
//!
//! Only direct transmission is accepted: a program cannot make TermOS read
//! a file or shared memory (`t=f`, `t=t`, `t=s` are refused). Unicode
//! placeholders and animation frames are not supported (refused with an
//! error the program can see).

use std::collections::{HashMap, VecDeque};
use std::io::Read as _;
use std::sync::Arc;

use base64::Engine as _;
use termos_render::graphics::{MAX_SIDE, Picture};

/// Most decoded image bytes kept per pane (transmitted images and
/// placements together).
pub const MAX_PANE_BYTES: usize = 16 * 1024 * 1024;
/// Most images (and most placements) kept per pane.
pub const MAX_PANE_IMAGES: usize = 128;

/// A Kitty graphics command's control keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KittyCmd {
    pub action: u8,
    pub format: u32,
    pub medium: u8,
    pub width: u32,
    pub height: u32,
    pub id: Option<u32>,
    pub number: Option<u32>,
    pub more: bool,
    pub compressed: bool,
    pub cols: u32,
    pub rows: u32,
    pub stay: bool,
    pub quiet: u32,
    pub src: [u32; 4],
    pub delete: u8,
    pub unicode: bool,
}

/// Splits `ESC _ G` content into control keys and payload.
pub(crate) fn parse_kitty(body: &[u8]) -> (KittyCmd, &[u8]) {
    let (control, payload) = match body.iter().position(|b| *b == b';') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &body[body.len()..]),
    };
    let mut c = KittyCmd {
        action: b't',
        format: 32,
        medium: b'd',
        ..KittyCmd::default()
    };
    for kv in control.split(|b| *b == b',') {
        let (Some(&k), Some(b'='), Some(v)) = (kv.first(), kv.get(1), kv.get(2..)) else {
            continue;
        };
        let num = || {
            std::str::from_utf8(v)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
        };
        let ch = v.first().copied().unwrap_or(0);
        match k {
            b'a' => c.action = ch,
            b'f' => c.format = num().unwrap_or(32),
            b't' => c.medium = ch,
            b's' => c.width = num().unwrap_or(0),
            b'v' => c.height = num().unwrap_or(0),
            b'i' => c.id = num(),
            b'I' => c.number = num(),
            b'm' => c.more = num() == Some(1),
            b'o' => c.compressed = ch == b'z',
            b'c' => c.cols = num().unwrap_or(0),
            b'r' => c.rows = num().unwrap_or(0),
            b'C' => c.stay = num() == Some(1),
            b'q' => c.quiet = num().unwrap_or(0),
            b'x' => c.src[0] = num().unwrap_or(0),
            b'y' => c.src[1] = num().unwrap_or(0),
            b'w' => c.src[2] = num().unwrap_or(0),
            b'h' => c.src[3] = num().unwrap_or(0),
            b'd' => c.delete = ch,
            b'U' => c.unicode = num() == Some(1),
            _ => {}
        }
    }
    (c, payload)
}

/// Turns a finished transmission into a picture.
pub(crate) fn decode_kitty(cmd: &KittyCmd, payload_b64: &[u8]) -> Result<Picture, String> {
    if cmd.medium != b'd' {
        return Err("EBADF:only direct transmission is accepted".into());
    }
    let mut data = base64::engine::general_purpose::STANDARD
        .decode(payload_b64)
        .map_err(|_| "EINVAL:bad base64".to_owned())?;
    if cmd.compressed {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .take(crate::intercept::MAX_SEQUENCE as u64 * 4)
            .read_to_end(&mut out)
            .map_err(|_| "EINVAL:bad zlib data".to_owned())?;
        data = out;
    }
    match cmd.format {
        100 => Picture::decode(&data).map_err(|e| format!("EINVAL:{e}")),
        32 | 24 => {
            let (w, h) = (cmd.width, cmd.height);
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                return Err("EINVAL:bad image size".into());
            }
            let px = (w * h) as usize;
            let rgba = if cmd.format == 32 {
                if data.len() != px * 4 {
                    return Err("EINVAL:data does not match the size".into());
                }
                data
            } else {
                if data.len() != px * 3 {
                    return Err("EINVAL:data does not match the size".into());
                }
                data.chunks_exact(3)
                    .flat_map(|p| [p[0], p[1], p[2], 255])
                    .collect()
            };
            Picture::from_rgba(w, h, rgba).map_err(|e| format!("EINVAL:{e}"))
        }
        other => Err(format!("EINVAL:unsupported format {other}")),
    }
}

/// An image placed in a pane: what the anchor in the text points at.
#[derive(Debug, Clone)]
pub struct Placed {
    pub key: u64,
    pub picture: Arc<Picture>,
    pub cols: u16,
    pub rows: u16,
}

/// A placed image found on screen.
#[derive(Debug, Clone)]
pub struct PaneImage {
    pub key: u64,
    pub picture: Arc<Picture>,
    /// Top-left cell, relative to the pane's view (`y` may be negative:
    /// the top of the image scrolled out of view).
    pub x: u16,
    pub y: i32,
    pub cols: u16,
    pub rows: u16,
}

fn bytes_of(p: &Picture) -> usize {
    p.rgba().len()
}

/// A pane's images, within [`MAX_PANE_BYTES`] and [`MAX_PANE_IMAGES`]:
/// when full, the oldest go first.
#[derive(Debug, Default)]
pub(crate) struct PaneImages {
    /// Kitty images by id, with the key they are shown under.
    transmitted: HashMap<u32, (Arc<Picture>, u64)>,
    transmit_order: VecDeque<u32>,
    /// Placements by anchor.
    placed: HashMap<u64, Placed>,
    place_order: VecDeque<u64>,
    /// A chunked Kitty transmission in progress.
    pub(crate) pending: Option<(KittyCmd, Vec<u8>)>,
    next_anchor: u64,
    next_auto_id: u32,
}

impl PaneImages {
    /// Decoded bytes held, each picture counted once (a placement of a
    /// whole transmitted image shares its pixels).
    fn total_bytes(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        self.transmitted
            .values()
            .map(|(p, _)| p)
            .chain(self.placed.values().map(|p| &p.picture))
            .filter(|p| seen.insert(Arc::as_ptr(p)))
            .map(|p| bytes_of(p))
            .sum()
    }

    fn trim(&mut self) {
        while self.transmitted.len() > MAX_PANE_IMAGES {
            self.drop_oldest_transmitted();
        }
        while self.placed.len() > MAX_PANE_IMAGES {
            self.drop_oldest_placement();
        }
        // Over the byte budget: stored image data goes first (what is on
        // screen keeps its own pixels), then the oldest placements.
        while self.total_bytes() > MAX_PANE_BYTES {
            if !self.drop_oldest_transmitted() && !self.drop_oldest_placement() {
                break;
            }
        }
    }

    fn drop_oldest_transmitted(&mut self) -> bool {
        match self.transmit_order.pop_front() {
            Some(id) => {
                self.transmitted.remove(&id);
                true
            }
            None => false,
        }
    }

    fn drop_oldest_placement(&mut self) -> bool {
        match self.place_order.pop_front() {
            Some(a) => {
                self.placed.remove(&a);
                true
            }
            None => false,
        }
    }

    /// Stores a transmitted Kitty image; returns its id.
    pub(crate) fn transmit(&mut self, id: Option<u32>, picture: Picture) -> u32 {
        let id = id.unwrap_or_else(|| {
            self.next_auto_id = self.next_auto_id.wrapping_add(1).max(1);
            // Ids we pick live above the range programs use.
            0x8000_0000 | self.next_auto_id
        });
        self.transmit_order.retain(|i| *i != id);
        self.transmit_order.push_back(id);
        self.transmitted
            .insert(id, (Arc::new(picture), termos_core::app::new_image_key()));
        self.trim();
        id
    }

    /// A transmitted image and its key.
    pub(crate) fn transmitted(&self, id: u32) -> Option<(Arc<Picture>, u64)> {
        self.transmitted.get(&id).cloned()
    }

    /// Records a placement; returns its anchor.
    pub(crate) fn place(&mut self, placed: Placed) -> u64 {
        self.next_anchor += 1;
        let a = self.next_anchor;
        self.placed.insert(a, placed);
        self.place_order.push_back(a);
        self.trim();
        a
    }

    pub(crate) fn placed(&self, anchor: u64) -> Option<&Placed> {
        self.placed.get(&anchor)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }

    /// The tallest placement, in rows (how far above the view to look for
    /// anchors of images still partly visible).
    pub(crate) fn max_rows(&self) -> u16 {
        self.placed.values().map(|p| p.rows).max().unwrap_or(0)
    }

    /// Kitty `a=d`: removes placements (all, or of one image) and with an
    /// uppercase `d` also the image data.
    pub(crate) fn delete(&mut self, what: u8, id: Option<u32>) {
        match what {
            b'i' | b'I' => {
                if let Some(id) = id
                    && let Some((pic, _)) = self.transmitted.get(&id).cloned()
                {
                    self.placed.retain(|_, p| !Arc::ptr_eq(&p.picture, &pic));
                    if what == b'I' {
                        self.transmitted.remove(&id);
                    }
                }
            }
            // `a`, `A` and anything we do not track finer: everything.
            _ => {
                self.placed.clear();
                if what.is_ascii_uppercase() {
                    self.transmitted.clear();
                }
            }
        }
        self.place_order.retain(|a| self.placed.contains_key(a));
        self.transmit_order
            .retain(|i| self.transmitted.contains_key(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_keys_parse() {
        let (c, payload) = parse_kitty(b"a=T,f=100,i=7,c=10,r=5,C=1,q=2,x=1,y=2,w=3,h=4;QUJD");
        assert_eq!(c.action, b'T');
        assert_eq!(c.format, 100);
        assert_eq!(c.id, Some(7));
        assert_eq!((c.cols, c.rows), (10, 5));
        assert!(c.stay);
        assert_eq!(c.quiet, 2);
        assert_eq!(c.src, [1, 2, 3, 4]);
        assert_eq!(payload, b"QUJD");
        let (c, payload) = parse_kitty(b"m=1");
        assert!(c.more && payload.is_empty());
        assert_eq!(c.action, b't', "transmit is the default");
    }

    #[test]
    fn transmissions_decode_and_files_are_refused() {
        let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
        let rgb = b64(&[255, 0, 0, 0, 255, 0]);
        let (c, _) = parse_kitty(b"f=24,s=2,v=1");
        let p = decode_kitty(&c, rgb.as_bytes()).expect("rgb");
        assert_eq!(p.pixel(1, 0), [0, 255, 0, 255]);
        let png = termos_render::graphics::Picture::from_rgba(1, 1, vec![1, 2, 3, 255])
            .expect("valid")
            .to_png();
        let (c, _) = parse_kitty(b"f=100");
        assert!(decode_kitty(&c, b64(&png).as_bytes()).is_ok());
        // zlib-compressed RGBA.
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut z, &[9, 9, 9, 255]).expect("compress");
        let (c, _) = parse_kitty(b"f=32,s=1,v=1,o=z");
        assert_eq!(
            decode_kitty(&c, b64(&z.finish().expect("zlib")).as_bytes())
                .expect("zlib")
                .pixel(0, 0),
            [9, 9, 9, 255]
        );
        // Files, temp files and shared memory are refused.
        for medium in [&b"t=f"[..], b"t=t", b"t=s"] {
            let (c, _) = parse_kitty(medium);
            assert!(decode_kitty(&c, b64(b"/etc/passwd").as_bytes()).is_err());
        }
        let (c, _) = parse_kitty(b"f=32,s=2,v=2");
        assert!(decode_kitty(&c, rgb.as_bytes()).is_err(), "wrong size");
    }

    #[test]
    fn stores_are_bounded() {
        let mut s = PaneImages::default();
        // 1 MiB images: only 16 MiB fit.
        let big = || Picture::from_rgba(512, 512, vec![0; 512 * 512 * 4]).expect("valid");
        for i in 0..40 {
            s.transmit(Some(i), big());
        }
        assert!(s.total_bytes() <= MAX_PANE_BYTES);
        assert!(s.transmitted(39).is_some(), "the newest stay");
        assert!(s.transmitted(0).is_none(), "the oldest go");
        let small = Arc::new(Picture::from_rgba(1, 1, vec![0; 4]).expect("valid"));
        for _ in 0..(MAX_PANE_IMAGES + 5) {
            s.place(Placed {
                key: 1,
                picture: Arc::clone(&small),
                cols: 1,
                rows: 1,
            });
        }
        assert_eq!(s.placed.len(), MAX_PANE_IMAGES);
        s.delete(b'a', None);
        assert!(s.is_empty());
        assert!(s.transmitted(39).is_some(), "lowercase keeps the data");
        s.delete(b'A', None);
        assert!(s.transmitted(39).is_none());
    }
}
