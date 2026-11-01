// SPDX-License-Identifier: GPL-3.0-or-later
//! The client's side of pixel images: it collects image data from the
//! server and turns each frame's placement list into Kitty, Sixel or
//! iTerm2 output for this terminal (ARCHITECTURE.md §5.8).

use std::collections::{HashMap, HashSet};

use termos_core::proto::ImagePlacement;
use termos_render::caps::ImageProtocol;
use termos_render::graphics::{Picture, iterm2, kitty, sixel};

#[derive(Debug, Default)]
pub(crate) struct ImageStore {
    /// Image data still arriving.
    partial: HashMap<u32, Vec<u8>>,
    /// Complete images (PNG).
    png: HashMap<u32, Vec<u8>>,
    /// Sixel encodings, made once per image.
    sixel: HashMap<u32, Vec<u8>>,
    /// Images the terminal has (Kitty keeps them).
    uploaded: HashSet<u32>,
    /// What is on screen, with each Kitty placement's id.
    placed: HashMap<ImagePlacement, u32>,
    next_placement: u32,
    kitty_used: bool,
}

impl ImageStore {
    /// A piece of image data.
    pub(crate) fn data(&mut self, id: u32, chunk: Vec<u8>, last: bool) {
        let buf = self.partial.entry(id).or_default();
        buf.extend_from_slice(&chunk);
        if last {
            let png = self.partial.remove(&id).unwrap_or_default();
            // New data for an id replaces the old image.
            self.sixel.remove(&id);
            self.uploaded.remove(&id);
            self.png.insert(id, png);
        }
    }

    /// Forgets image `id`; returns what the terminal needs to hear.
    pub(crate) fn free(&mut self, id: u32) -> Vec<u8> {
        self.png.remove(&id);
        self.sixel.remove(&id);
        self.partial.remove(&id);
        self.placed.retain(|p, _| p.image != id);
        if self.uploaded.remove(&id) {
            kitty::free(id)
        } else {
            Vec::new()
        }
    }

    /// Output for a frame: `images` is the new placement list (if it
    /// changed); `clear` means the screen was erased.
    pub(crate) fn frame(&mut self, images: Option<&[ImagePlacement]>, clear: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if clear {
            if self.kitty_used {
                out.extend_from_slice(b"\x1b_Ga=d,d=a,q=2\x1b\\");
            }
            self.placed.clear();
        }
        let Some(new) = images else {
            return out;
        };
        let wanted: HashSet<&ImagePlacement> = new.iter().collect();
        let gone: Vec<(ImagePlacement, u32)> = self
            .placed
            .iter()
            .filter(|(p, _)| !wanted.contains(p))
            .map(|(p, id)| (*p, *id))
            .collect();
        for (p, pid) in gone {
            self.placed.remove(&p);
            // Sixel and iTerm2 pixels are painted over by the server's
            // cells; Kitty placements must be removed.
            if p.protocol == ImageProtocol::Kitty {
                out.extend_from_slice(&kitty::remove_placement(p.image, pid));
            }
        }
        for p in new {
            if self.placed.contains_key(p) {
                continue;
            }
            let Some(png) = self.png.get(&p.image) else {
                continue;
            };
            match p.protocol {
                ImageProtocol::Kitty => {
                    if self.uploaded.insert(p.image) {
                        out.extend_from_slice(&kitty::upload(p.image, png));
                    }
                    self.kitty_used = true;
                    self.next_placement = self.next_placement.wrapping_add(1).max(1);
                    let pid = self.next_placement;
                    let [x, y, w, h] = p.src;
                    out.extend_from_slice(&kitty::place(
                        p.image,
                        pid,
                        p.x,
                        p.y,
                        kitty::SourceRect { x, y, w, h },
                        p.cols,
                        p.rows,
                    ));
                    self.placed.insert(*p, pid);
                }
                ImageProtocol::Sixel => {
                    let six = match self.sixel.entry(p.image) {
                        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::hash_map::Entry::Vacant(e) => {
                            match Picture::decode(png) {
                                Ok(pic) => e.insert(sixel::encode(&pic)),
                                Err(err) => {
                                    tracing::warn!("image {}: {err}", p.image);
                                    continue;
                                }
                            }
                        }
                    };
                    out.extend_from_slice(
                        format!("\x1b[{};{}H", u32::from(p.y) + 1, u32::from(p.x) + 1).as_bytes(),
                    );
                    out.extend_from_slice(six);
                    self.placed.insert(*p, 0);
                }
                ImageProtocol::Iterm2 => {
                    out.extend_from_slice(&iterm2::inline(png, p.x, p.y, p.cols, p.rows));
                    self.placed.insert(*p, 0);
                }
                ImageProtocol::Blocks => {}
            }
        }
        out
    }

    /// What to send the terminal when the client leaves: Kitty keeps
    /// images until told otherwise.
    pub(crate) fn cleanup(&self) -> &'static [u8] {
        if self.kitty_used {
            b"\x1b_Ga=d,d=A,q=2\x1b\\"
        } else {
            b""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        Picture::from_rgba(2, 2, vec![255; 16])
            .expect("valid")
            .to_png()
    }

    fn kitty_at(x: u16) -> ImagePlacement {
        ImagePlacement {
            image: 1,
            protocol: ImageProtocol::Kitty,
            x,
            y: 0,
            cols: 2,
            rows: 1,
            src: [0, 0, 20, 20],
        }
    }

    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    #[test]
    fn kitty_uploads_once_and_moves_placements() {
        let mut s = ImageStore::default();
        let data = png();
        let (a, b) = data.split_at(data.len() / 2);
        s.data(1, a.to_vec(), false);
        s.data(1, b.to_vec(), true);
        let out = text(&s.frame(Some(&[kitty_at(0)]), false));
        assert!(out.contains("a=t,f=100,i=1"), "uploaded: {out}");
        assert!(out.contains("a=p,i=1,p=1"), "placed: {out}");
        // Unchanged: nothing.
        assert!(s.frame(Some(&[kitty_at(0)]), false).is_empty());
        // Moved: the old placement goes, the new one comes, no re-upload.
        let out = text(&s.frame(Some(&[kitty_at(4)]), false));
        assert!(out.contains("a=d,d=i,i=1,p=1"), "{out}");
        assert!(out.contains("a=p,i=1,p=2"), "{out}");
        assert!(!out.contains("a=t"), "{out}");
        // A cleared screen: placements are redone.
        let out = text(&s.frame(Some(&[kitty_at(4)]), true));
        assert!(out.starts_with("\x1b_Ga=d,d=a,q=2"), "{out}");
        assert!(out.contains("a=p,i=1,p=3"), "{out}");
        assert_eq!(text(&s.free(1)), "\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
        assert_eq!(s.cleanup(), b"\x1b_Ga=d,d=A,q=2\x1b\\");
    }

    #[test]
    fn sixel_and_iterm2_are_drawn_where_placed() {
        let mut s = ImageStore::default();
        s.data(1, png(), true);
        let mut p = kitty_at(3);
        p.protocol = ImageProtocol::Sixel;
        let out = text(&s.frame(Some(&[p]), false));
        assert!(out.starts_with("\x1b[1;4H\x1bP0;1;0q"), "{out}");
        p.protocol = ImageProtocol::Iterm2;
        let out = text(&s.frame(Some(&[p]), false));
        assert!(out.contains("]1337;File=inline=1"), "{out}");
        // Data that never completed draws nothing.
        s.data(2, vec![1, 2], false);
        p.image = 2;
        assert!(s.frame(Some(&[p]), false).is_empty());
        assert!(s.cleanup().is_empty(), "no Kitty, nothing to clean");
    }
}
