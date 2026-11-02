// SPDX-License-Identifier: GPL-3.0-or-later
//! The display: a DRM device driven through its "dumb" buffers, so any KMS
//! driver works (virtio-gpu, simpledrm, i915, amdgpu, vc4) and no GPU code
//! is needed (ARCHITECTURE.md §5.9).
//!
//! Two buffers take turns: the screen's changed rows are copied into the
//! one not shown, which is then flipped in at the next vertical blank.
//! Each buffer remembers the rows it missed while the other was shown.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::{
    Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc,
    framebuffer,
};

/// An open DRM device.
pub(super) struct Card(pub(super) OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

struct Fb {
    db: DumbBuffer,
    fb: framebuffer::Handle,
}

pub(super) struct Display {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    buffers: [Fb; 2],
    /// The buffer on screen (or about to be, while `flipping`).
    front: usize,
    flipping: bool,
    /// Rows each buffer lacks: `[start, end)`.
    stale: [Option<(u32, u32)>; 2],
    pub(super) width: u32,
    pub(super) height: u32,
    /// The screen's size in millimetres, when the monitor says.
    pub(super) size_mm: Option<(u32, u32)>,
    cursor: Option<DumbBuffer>,
}

fn union(a: Option<(u32, u32)>, b: Option<(u32, u32)>) -> Option<(u32, u32)> {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        (x, None) | (None, x) => x,
    }
}

impl Display {
    /// Sets up the first connected output of `card`.
    pub(super) fn open(card: Card) -> io::Result<Self> {
        let res = card.resource_handles()?;
        let (connector, info) = res
            .connectors()
            .iter()
            .filter_map(|c| card.get_connector(*c, true).ok().map(|i| (*c, i)))
            .find(|(_, i)| i.state() == connector::State::Connected && !i.modes().is_empty())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "no connected output with a mode")
            })?;
        let mode = info
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| info.modes().first())
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the output lists no modes"))?;
        let encoder = info
            .current_encoder()
            .or_else(|| info.encoders().first().copied())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the output has no encoder"))?;
        let enc = card.get_encoder(encoder)?;
        let crtc = enc
            .crtc()
            .or_else(|| res.filter_crtcs(enc.possible_crtcs()).first().copied())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no display controller for the output",
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
        let size_mm = info.size().filter(|(w, h)| *w > 0 && *h > 0);
        tracing::info!(
            "display: {:?} {}x{}@{} on {:?}, {:?} mm",
            info.interface(),
            width,
            height,
            mode.vrefresh(),
            crtc,
            size_mm
        );
        let mut d = Self {
            card,
            crtc,
            connector,
            mode,
            buffers,
            front: 0,
            flipping: false,
            stale: [Some((0, height)), Some((0, height))],
            width,
            height,
            size_mm,
            cursor: None,
        };
        d.modeset()?;
        d.cursor = d.make_cursor();
        Ok(d)
    }

    pub(super) fn fd(&self) -> RawFd {
        self.card.as_fd().as_raw_fd()
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
        if let Some(c) = &self.cursor {
            let _ = self.card.set_cursor2(self.crtc, Some(c), (0, 0));
        }
        Ok(())
    }

    /// Whether a buffer still lacks changes (show them after the flip).
    pub(super) fn behind(&self) -> bool {
        self.stale[1 - self.front].is_some()
    }

    /// Records changed `rows` of `pixels` and, unless a flip is under way,
    /// copies what the hidden buffer lacks into it and flips it in.
    pub(super) fn present(&mut self, pixels: &[u32], rows: Option<(u32, u32)>) -> io::Result<()> {
        self.stale[0] = union(self.stale[0], rows);
        self.stale[1] = union(self.stale[1], rows);
        if self.flipping {
            return Ok(());
        }
        let back = 1 - self.front;
        let Some((y0, y1)) = self.stale[back].take() else {
            return Ok(());
        };
        let width = self.width as usize;
        let fb = self.buffers[back].fb;
        let db = &mut self.buffers[back].db;
        let pitch = db.pitch() as usize;
        let mut map = self.card.map_dumb_buffer(db)?;
        for y in y0..y1.min(self.height) {
            let src = &pixels[y as usize * width..(y as usize + 1) * width];
            let dst = &mut map[y as usize * pitch..y as usize * pitch + width * 4];
            for (d, s) in dst.chunks_exact_mut(4).zip(src) {
                d.copy_from_slice(&s.to_le_bytes());
            }
        }
        drop(map);
        self.card
            .page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)?;
        self.front = back;
        self.flipping = true;
        Ok(())
    }

    /// Reads the device's events; whether a flip finished.
    pub(super) fn events(&mut self) -> io::Result<bool> {
        let mut flipped = false;
        for ev in self.card.receive_events()? {
            if let Event::PageFlip(_) = ev {
                self.flipping = false;
                flipped = true;
            }
        }
        Ok(flipped)
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
    pub(super) fn move_cursor(&self, x: i32, y: i32) {
        if self.cursor.is_some() {
            let _ = self.card.move_cursor(self.crtc, (x, y));
        }
    }
}

impl Drop for Display {
    #[allow(deprecated)]
    fn drop(&mut self) {
        let _ = self.card.set_cursor2::<DumbBuffer>(self.crtc, None, (0, 0));
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
    use super::union;

    #[test]
    fn damage_unions() {
        assert_eq!(union(None, None), None);
        assert_eq!(union(Some((5, 9)), None), Some((5, 9)));
        assert_eq!(union(Some((5, 9)), Some((2, 6))), Some((2, 9)));
    }
}
