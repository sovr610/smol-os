// SPDX-License-Identifier: GPL-3.0-or-later
//! The displays: a DRM device driven through its "dumb" buffers, so any
//! KMS driver works (virtio-gpu, simpledrm, i915, amdgpu, vc4) and no GPU
//! code is needed (ARCHITECTURE.md §5.9). Each connected output is a
//! [`Display`] with a display controller (CRTC) of its own; they share the
//! card (M13e, D-128).
//!
//! Two buffers take turns: the screen's changed rows are copied into the
//! one not shown, which is then flipped in at the next vertical blank.
//! Each buffer remembers the rows it missed while the other was shown.
//! Rows are copied as bands ([`Damage`]), so a change at the top and one at
//! the bottom do not copy the screen between them: that copy, into memory
//! the CPU writes slowly on a Pi, is most of a frame's cost there (M9).

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::rc::Rc;

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::{
    Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc,
    framebuffer,
};

use super::damage::Damage;
use super::outputs;

/// An open DRM device.
pub(super) struct Card(pub(super) OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub(super) fn fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }

    /// The display controllers whose page flips finished.
    pub(super) fn flips(&self) -> io::Result<Vec<crtc::Handle>> {
        Ok(self
            .receive_events()?
            .filter_map(|ev| match ev {
                Event::PageFlip(f) => Some(f.crtc),
                _ => None,
            })
            .collect())
    }
}

/// A connector of the card, with what it takes to drive it.
pub(super) struct CardConnector {
    pub(super) handle: connector::Handle,
    pub(super) info: outputs::Connector,
    modes: Vec<Mode>,
    size_mm: Option<(u32, u32)>,
    encoders: Vec<drm::control::encoder::Handle>,
    current_encoder: Option<drm::control::encoder::Handle>,
}

/// Every connector of `card`, connected or not.
pub(super) fn connectors(card: &Card) -> io::Result<Vec<CardConnector>> {
    let res = card.resource_handles()?;
    let mut out = Vec::new();
    for c in res.connectors() {
        let Ok(info) = card.get_connector(*c, true) else {
            continue;
        };
        let modes: Vec<Mode> = info.modes().to_vec();
        out.push(CardConnector {
            handle: *c,
            info: outputs::Connector {
                name: format!("{}-{}", info.interface().as_str(), info.interface_id()),
                connected: info.state() == connector::State::Connected,
                modes: modes
                    .iter()
                    .map(|m| {
                        let (w, h) = m.size();
                        outputs::ModeInfo {
                            width: w,
                            height: h,
                            refresh: m.vrefresh(),
                            preferred: m.mode_type().contains(ModeTypeFlags::PREFERRED),
                        }
                    })
                    .collect(),
            },
            modes,
            size_mm: info.size().filter(|(w, h)| *w > 0 && *h > 0),
            encoders: info.encoders().to_vec(),
            current_encoder: info.current_encoder(),
        });
    }
    Ok(out)
}

struct Fb {
    db: DumbBuffer,
    fb: framebuffer::Handle,
}

pub(super) struct Display {
    card: Rc<Card>,
    /// Its connector's name (`HDMI-A-1`).
    pub(super) name: String,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffers: [Fb; 2],
    /// The buffer on screen (or about to be, while `flipping`).
    front: usize,
    flipping: bool,
    /// Rows each buffer lacks.
    stale: [Damage; 2],
    pub(super) width: u32,
    pub(super) height: u32,
    /// The screen's size in millimetres, when the monitor says.
    pub(super) size_mm: Option<(u32, u32)>,
    cursor: Option<DumbBuffer>,
    /// The pointer is on this display (the cursor shows).
    cursor_shown: bool,
}

/// One row of `0x00RRGGBB` pixels into XRGB8888 memory, which holds each
/// pixel's bytes little-endian: a plain copy on a little-endian machine.
fn copy_row(dst: &mut [u8], src: &[u32]) {
    #[cfg(target_endian = "little")]
    dst.copy_from_slice(bytemuck::cast_slice(src));
    #[cfg(not(target_endian = "little"))]
    for (d, s) in dst.chunks_exact_mut(4).zip(src) {
        d.copy_from_slice(&s.to_le_bytes());
    }
}

impl Display {
    /// Drives connector `c` of `card` in its mode `mode` (an index into
    /// its modes), with a display controller none of `taken` uses.
    pub(super) fn open(
        card: Rc<Card>,
        c: &CardConnector,
        mode: usize,
        taken: &[crtc::Handle],
    ) -> io::Result<Self> {
        let res = card.resource_handles()?;
        let mode = *c
            .modes
            .get(mode)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the output lists no modes"))?;
        let mut crtc = None;
        for e in c.current_encoder.iter().chain(c.encoders.iter()) {
            let Ok(enc) = card.get_encoder(*e) else {
                continue;
            };
            crtc = enc.crtc().filter(|h| !taken.contains(h)).or_else(|| {
                res.filter_crtcs(enc.possible_crtcs())
                    .into_iter()
                    .find(|h| !taken.contains(h))
            });
            if crtc.is_some() {
                break;
            }
        }
        let crtc = crtc.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no free display controller for {}", c.info.name),
            )
        })?;
        let (w, h) = mode.size();
        let (width, height) = (u32::from(w), u32::from(h));
        let make = |card: &Card| -> io::Result<Fb> {
            let db = card.create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)?;
            let fb = card.add_framebuffer(&db, 24, 32)?;
            Ok(Fb { db, fb })
        };
        let buffers = [make(&card)?, make(&card)?];
        tracing::info!(
            "display {}: {}x{}@{} on {:?}, {:?} mm",
            c.info.name,
            width,
            height,
            mode.vrefresh(),
            crtc,
            c.size_mm
        );
        let mut d = Self {
            card,
            name: c.info.name.clone(),
            crtc,
            connector: c.handle,
            mode,
            buffers,
            front: 0,
            flipping: false,
            stale: [Damage::all(height), Damage::all(height)],
            width,
            height,
            size_mm: c.size_mm,
            cursor: None,
            cursor_shown: true,
        };
        d.modeset()?;
        d.cursor = d.make_cursor();
        Ok(d)
    }

    /// Its display controller.
    pub(super) fn crtc(&self) -> crtc::Handle {
        self.crtc
    }

    /// Its mode's size and refresh.
    pub(super) fn mode_size(&self) -> (u16, u16, u32) {
        let (w, h) = self.mode.size();
        (w, h, self.mode.vrefresh())
    }

    /// Its page flip finished.
    pub(super) fn flipped(&mut self) {
        self.flipping = false;
    }

    /// Shows the front buffer (at start, and when the seat comes back).
    // The legacy cursor calls (here and below) work on every driver with a
    // cursor; cursor planes would need atomic modesetting, which this
    // display does not use.
    #[allow(deprecated)]
    pub(super) fn modeset(&mut self) -> io::Result<()> {
        self.flipping = false;
        let fb = self.buffers[self.front].fb;
        self.card.set_crtc(
            self.crtc,
            Some(fb),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        )?;
        if let Some(c) = &self.cursor
            && self.cursor_shown
        {
            let _ = self.card.set_cursor2(self.crtc, Some(c), (0, 0));
        }
        Ok(())
    }

    /// Whether a buffer still lacks changes (show them after the flip).
    pub(super) fn behind(&self) -> bool {
        !self.stale[1 - self.front].is_empty()
    }

    /// Records changed `rows` of `pixels` and, unless a flip is under way,
    /// copies what the hidden buffer lacks into it and flips it in; whether
    /// it asked for a flip.
    pub(super) fn present(&mut self, pixels: &[u32], rows: &Damage) -> io::Result<bool> {
        self.stale[0].merge(rows);
        self.stale[1].merge(rows);
        if self.flipping {
            return Ok(false);
        }
        let back = 1 - self.front;
        let stale = self.stale[back].take();
        if stale.is_empty() {
            return Ok(false);
        }
        let width = self.width as usize;
        let fb = self.buffers[back].fb;
        let db = &mut self.buffers[back].db;
        let pitch = db.pitch() as usize;
        let mut map = self.card.map_dumb_buffer(db)?;
        for (y0, y1) in stale.clipped(self.height) {
            for y in y0 as usize..y1 as usize {
                copy_row(
                    &mut map[y * pitch..y * pitch + width * 4],
                    &pixels[y * width..(y + 1) * width],
                );
            }
        }
        drop(map);
        self.card
            .page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)?;
        self.front = back;
        self.flipping = true;
        Ok(true)
    }

    /// The mouse pointer: a white arrow outlined in black, in a 64×64
    /// cursor plane (`None` where the driver has no cursor plane).
    #[allow(deprecated)]
    fn make_cursor(&mut self) -> Option<DumbBuffer> {
        const ARROW: [&str; 19] = [
            "X",
            "XX",
            "X.X",
            "X..X",
            "X...X",
            "X....X",
            "X.....X",
            "X......X",
            "X.......X",
            "X........X",
            "X.........X",
            "X......XXXXX",
            "X...X..X",
            "X..XX..X",
            "X.X  X..X",
            "XX   X..X",
            "X     X..X",
            "      X..X",
            "       XX",
        ];
        let mut db = self
            .card
            .create_dumb_buffer((64, 64), DrmFourcc::Argb8888, 32)
            .ok()?;
        let pitch = db.pitch() as usize;
        {
            let mut map = self.card.map_dumb_buffer(&mut db).ok()?;
            map.fill(0);
            for (y, row) in ARROW.iter().enumerate() {
                for (x, ch) in row.chars().enumerate() {
                    let px: u32 = match ch {
                        'X' => 0xff00_0000,
                        '.' => 0xffff_ffff,
                        _ => continue,
                    };
                    let at = y * pitch + x * 4;
                    map[at..at + 4].copy_from_slice(&px.to_le_bytes());
                }
            }
        }
        match self.card.set_cursor2(self.crtc, Some(&db), (0, 0)) {
            Ok(()) => Some(db),
            Err(e) => {
                tracing::info!("no hardware cursor: {e}");
                let _ = self.card.destroy_dumb_buffer(db);
                None
            }
        }
    }

    #[allow(deprecated)]
    pub(super) fn move_cursor(&mut self, x: i32, y: i32) {
        if let Some(c) = &self.cursor {
            if !self.cursor_shown {
                let _ = self.card.set_cursor2(self.crtc, Some(c), (0, 0));
                self.cursor_shown = true;
            }
            let _ = self.card.move_cursor(self.crtc, (x, y));
        }
    }

    /// The pointer left for another display.
    #[allow(deprecated)]
    pub(super) fn hide_cursor(&mut self) {
        if self.cursor.is_some() && self.cursor_shown {
            let _ = self.card.set_cursor2::<DumbBuffer>(self.crtc, None, (0, 0));
            self.cursor_shown = false;
        }
    }
}

impl Drop for Display {
    #[allow(deprecated)]
    fn drop(&mut self) {
        let _ = self.card.set_cursor2::<DumbBuffer>(self.crtc, None, (0, 0));
        // The controller goes back for another display (an unplugged or
        // turned-off one shows nothing).
        let _ = self.card.set_crtc(self.crtc, None, (0, 0), &[], None);
        if let Some(c) = self.cursor.take() {
            let _ = self.card.destroy_dumb_buffer(c);
        }
        for b in &self.buffers {
            let _ = self.card.destroy_framebuffer(b.fb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::copy_row;

    #[test]
    fn rows_are_copied_as_xrgb8888() {
        let mut dst = [0u8; 8];
        copy_row(&mut dst, &[0x0011_2233, 0x00aa_bbcc]);
        // Blue, green, red, then the unused byte.
        assert_eq!(dst, [0x33, 0x22, 0x11, 0, 0xcc, 0xbb, 0xaa, 0]);
    }
}
