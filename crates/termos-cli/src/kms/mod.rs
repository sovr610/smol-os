// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos --kms`: the bare-metal console (ARCHITECTURE.md §5.9, D-073).
//!
//! A client of the server like the terminal one, but it draws the frames
//! itself, with the software rasterizer into a DRM display, and reads the
//! keyboard and mouse through libinput and XKB. libseat gives it the
//! devices without root. Everything waits on file descriptors: nothing
//! runs while the screen and input are idle (a held key repeats on a
//! timer only while it is held, and the visual bell has one only while
//! it shows).
//!
//! With no terminal around it, the console does itself what a terminal
//! would (D-105): it keeps the last copied text (Ctrl+Shift+V or
//! Shift+Insert pastes it), flashes for the bell, and draws pictures as
//! pixels.

mod damage;
mod display;
mod keyboard;
mod latency;
mod pointer;
mod screen;
mod seat;

use std::io::{self, Write as _};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use input::event::keyboard::{KeyState, KeyboardEventTrait as _};
use input::event::pointer::{Axis, ButtonState, PointerScrollEvent as _};
use input::event::{KeyboardEvent, PointerEvent};
use input::{Event as InputEvent, Libinput};
use libseat::SeatEvent;
use termos_core::config::KmsConfig;
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::ClientEnd;
use termos_render::caps::{Caps, Graphics, PxSize};
use termos_render::color::{ColorDepth, Palette, Rgb, XTERM_ANSI};
use termos_render::raster::{FontData, FontSet, Rasterizer};
use tokio::io::unix::AsyncFd;

use crate::client::Ended;
use display::{Card, Display};
use keyboard::{Action, Keyboard};
use pointer::Pointer;
use screen::Screen;
use seat::{InputInterface, SeatHandle};

/// What the console prints on its standard output once its first frame is
/// on the screen. The image's session script sends that to the system
/// console, where the QEMU boot tests wait for it (D-076).
pub(crate) const READY: &str = "TERMOS-READY";

/// The console's default colours: TermOS's dark background and the VGA
/// palette programs expect.
fn palette() -> Palette {
    Palette {
        fg: Rgb::new(0xd0, 0xd0, 0xd0),
        bg: Rgb::new(0x14, 0x16, 0x1b),
        ansi: XTERM_ANSI,
    }
}

fn load_font(path: &Path) -> anyhow::Result<FontData> {
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read the font {}", path.display()))?;
    FontData::new(bytes).with_context(|| path.display().to_string())
}

/// The fonts `[kms]` names, else the built-in ones.
fn fonts(cfg: &KmsConfig) -> anyhow::Result<FontSet> {
    let mut set = FontSet::bundled();
    if let Some(p) = &cfg.font {
        set.regular = load_font(p)?;
        set.bold = None;
    }
    if let Some(p) = &cfg.bold_font {
        set.bold = Some(load_font(p)?);
    }
    for p in &cfg.fallback_fonts {
        match load_font(p) {
            Ok(f) => set.fallbacks.push(f),
            Err(e) => tracing::warn!("{e:#}"),
        }
    }
    Ok(set)
}

/// Scales the configured size (pixels at 96 dpi) for the screen's density,
/// in quarter steps from 1× to 3×.
fn scaled_size(px: f32, width: u32, size_mm: Option<(u32, u32)>) -> f32 {
    let Some((mm, _)) = size_mm else { return px };
    let dpi = width as f32 / (mm as f32 / 25.4);
    let scale = ((dpi / 96.0) * 4.0).round() / 4.0;
    px * scale.clamp(1.0, 3.0)
}

/// Opens the first DRM device with a connected output.
fn open_display(seat: &SeatHandle) -> anyhow::Result<Display> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .context("no /dev/dri: the kernel has no DRM driver for this display")?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("card"))
        })
        .collect();
    cards.sort();
    let mut errors = Vec::new();
    for path in cards {
        let fd = match seat.open_device(&path) {
            Ok(fd) => fd,
            Err(e) => {
                errors.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        match Display::open(Card(fd)) {
            Ok(d) => return Ok(d),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    if errors.is_empty() {
        bail!("no usable display: no DRM devices in /dev/dri");
    }
    bail!("no usable display: {}", errors.join("; "))
}

/// A descriptor to wait on, owned elsewhere.
struct Fd(RawFd);

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

/// Waits on the descriptor of the display, libinput or the seat.
#[allow(unsafe_code)]
fn watch(fd: RawFd) -> io::Result<AsyncFd<Fd>> {
    // SAFETY: each descriptor watched belongs to an object `run` created
    // before the watcher (the display, libinput, the seat), so it is
    // dropped after the watcher and stays open as long as it; none of
    // them replaces its descriptor while open, so `Fd` always names the
    // same file description.
    Ok(unsafe { AsyncFd::register(Fd(fd)) }?)
}

struct Console {
    seat: SeatHandle,
    display: Display,
    screen: Screen,
    keyboard: Keyboard,
    pointer: Pointer,
    input: Libinput,
    end: ClientEnd,
    /// The configured size, scaled for the screen (Ctrl+0 goes back to it).
    base_size: f32,
    /// The seat is ours (not switched away).
    active: bool,
    ready: bool,
    /// A held key: what it does, and when it next repeats.
    repeat: Option<(Action, u32, Instant)>,
    /// Input→frame times, reported to the log (M9's Pi measurements).
    latency: latency::Latency,
    /// The text last copied in TermOS: in memory only, never logged.
    clipboard: Option<String>,
    visual_bell: bool,
    /// While the bell's frame shows: when it goes.
    flash_until: Option<Instant>,
}

/// How long the bell's frame shows.
const FLASH: Duration = Duration::from_millis(150);

impl Console {
    fn send(&self, msg: ClientMsg) -> io::Result<()> {
        self.end
            .tx
            .send(msg)
            .map_err(|_| io::Error::other("the desktop server stopped"))
    }

    /// Puts the screen's changes on the display.
    fn show(&mut self) -> io::Result<()> {
        let damage = self.screen.take_damage();
        if !self.active {
            // Shown in full when the seat comes back.
            return Ok(());
        }
        if self.display.present(&self.screen.pixels, &damage)? {
            self.latency.flushed(Instant::now());
        }
        Ok(())
    }

    fn font_size(&mut self, px: f32) -> io::Result<()> {
        if let Some((cols, rows)) = self.screen.set_font_size(px) {
            self.send(ClientMsg::Resize { cols, rows })?;
        }
        self.show()
    }

    fn act(&mut self, a: Action) -> io::Result<()> {
        match a {
            Action::Key(k) => {
                self.latency.key(Instant::now());
                self.send(ClientMsg::Key(k))
            }
            Action::FontBigger => self.font_size(self.screen.raster.size() * 1.125),
            Action::FontSmaller => self.font_size(self.screen.raster.size() / 1.125),
            Action::FontReset => self.font_size(self.base_size),
            Action::SwitchVt(n) => {
                if let Err(e) = self.seat.switch_session(n) {
                    tracing::info!("cannot switch to VT {n}: {e}");
                }
                Ok(())
            }
            Action::Paste => match &self.clipboard {
                Some(text) => self.send(ClientMsg::Paste(text.clone())),
                None => Ok(()),
            },
        }
    }

    fn input(&mut self) -> io::Result<()> {
        self.input.dispatch()?;
        let size = (self.display.width, self.display.height);
        while let Some(ev) = self.input.next() {
            let metrics = self.screen.raster.metrics();
            let layout = self.screen.layout;
            let mods = self.keyboard.mods();
            match ev {
                InputEvent::Keyboard(KeyboardEvent::Key(k)) => {
                    let pressed = k.key_state() == KeyState::Pressed;
                    let code = k.key();
                    if self
                        .repeat
                        .as_ref()
                        .is_some_and(|(_, c, _)| *c == code || pressed)
                    {
                        self.repeat = None;
                    }
                    if let Some((action, repeats)) = self.keyboard.key(code, pressed) {
                        if repeats {
                            self.repeat =
                                Some((action.clone(), code, Instant::now() + self.keyboard.delay));
                        }
                        self.act(action)?;
                    }
                }
                InputEvent::Pointer(p) => {
                    let events = match p {
                        PointerEvent::Motion(m) => {
                            let (x, y) = (self.pointer.x + m.dx(), self.pointer.y + m.dy());
                            self.pointer
                                .move_to(x, y, size, &layout, &metrics, mods)
                                .into_iter()
                                .collect()
                        }
                        PointerEvent::MotionAbsolute(m) => {
                            let (x, y) = (
                                m.absolute_x_transformed(size.0),
                                m.absolute_y_transformed(size.1),
                            );
                            self.pointer
                                .move_to(x, y, size, &layout, &metrics, mods)
                                .into_iter()
                                .collect()
                        }
                        PointerEvent::Button(b) => {
                            let pressed = b.button_state() == ButtonState::Pressed;
                            self.pointer
                                .button(b.button(), pressed, &layout, &metrics, mods)
                                .into_iter()
                                .collect()
                        }
                        PointerEvent::ScrollWheel(s) => {
                            let v = |a| {
                                if s.has_axis(a) {
                                    s.scroll_value_v120(a)
                                } else {
                                    0.0
                                }
                            };
                            self.pointer.wheel(
                                v(Axis::Vertical),
                                v(Axis::Horizontal),
                                &layout,
                                &metrics,
                                mods,
                            )
                        }
                        _ => Vec::new(),
                    };
                    self.display
                        .move_cursor(self.pointer.x as i32, self.pointer.y as i32);
                    for e in events {
                        self.send(ClientMsg::Mouse(e))?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn seat_events(&mut self) -> io::Result<()> {
        for ev in self.seat.dispatch()? {
            match ev {
                SeatEvent::Disable => {
                    tracing::info!("the seat went to another session");
                    self.active = false;
                    self.repeat = None;
                    self.input.suspend();
                    self.seat.disable()?;
                }
                SeatEvent::Enable => {
                    tracing::info!("the seat is back");
                    self.active = true;
                    if self.input.resume().is_err() {
                        tracing::warn!("libinput could not resume");
                    }
                    self.display.modeset()?;
                    self.screen.repaint();
                    self.show()?;
                }
            }
        }
        Ok(())
    }

    fn server(&mut self, msg: ServerMsg) -> io::Result<Option<Ended>> {
        match msg {
            ServerMsg::Frame(f) => {
                self.screen.apply(&f);
                self.latency.frame();
                self.show()?;
            }
            ServerMsg::Exit { reason } => return Ok(Some(Ended::Exited(reason))),
            ServerMsg::Detached { session } => return Ok(Some(Ended::Detached(session))),
            ServerMsg::Clipboard(text) => self.clipboard = Some(text),
            ServerMsg::Bell => {
                // A bell while the frame shows (a loop of them) adds
                // nothing.
                if self.visual_bell && self.active && self.flash_until.is_none() {
                    self.screen.flash(self.screen.raster.palette.ansi[11]);
                    self.flash_until = Some(Instant::now() + FLASH);
                    self.show()?;
                }
            }
            ServerMsg::ImageData { id, chunk, last } => {
                self.screen.image_data(id, &chunk, last);
                self.show()?;
            }
            ServerMsg::ImageFree { id } => {
                self.screen.image_free(id);
                self.show()?;
            }
            // There is no window title on a console, nor a notification
            // service: the desktop's own toast shows it.
            ServerMsg::Title(_) | ServerMsg::Notify { .. } => {}
            // The console's keyboard reports neither repeats nor releases.
            ServerMsg::KeyboardFlags(_) => {}
            // The desktop copies addresses for a console instead: there
            // is no browser to open them in (D-118).
            ServerMsg::Open { .. } => {}
        }
        Ok(None)
    }

    fn flipped(&mut self) -> io::Result<()> {
        if !self.display.events()? {
            return Ok(());
        }
        if let Some(report) = self.latency.flipped(Instant::now()) {
            tracing::info!("{report}");
        }
        if !self.ready {
            self.ready = true;
            tracing::info!("first frame on the screen");
            let mut out = io::stdout();
            let _ = writeln!(out, "{READY}");
            let _ = out.flush();
            // On an image, the boot script marking the slot good waits
            // for this file (D-078).
            if let Some(file) = std::env::var_os("TERMOS_READY_FILE")
                && let Err(e) = std::fs::write(&file, b"")
            {
                tracing::warn!("cannot write {}: {e}", Path::new(&file).display());
            }
        }
        if self.display.behind() && self.active {
            self.show()?;
        }
        Ok(())
    }
}

/// Runs the console until the session ends or detaches it.
pub(crate) async fn run(end: ClientEnd, cfg: &KmsConfig) -> anyhow::Result<Ended> {
    let seat = SeatHandle::open()?;
    let display = open_display(&seat)?;
    let size = scaled_size(cfg.font_size, display.width, display.size_mm);
    let raster = Rasterizer::new(fonts(cfg)?, size, palette()).context("the console font")?;
    let mut screen = Screen::new(raster, display.width, display.height);
    screen.clear();
    let keyboard = match Keyboard::new(cfg) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("{e}; using the US layout");
            Keyboard::new(&KmsConfig::default()).map_err(anyhow::Error::msg)?
        }
    };
    let mut input = Libinput::new_with_udev(InputInterface(seat.clone()));
    if input.udev_assign_seat(&seat.name()).is_err() {
        bail!("libinput cannot use seat {}", seat.name());
    }
    let metrics = screen.raster.metrics();
    let (cols, rows) = screen.cells();
    let caps = Caps {
        color: ColorDepth::Truecolor,
        sync_output: false,
        kitty_keyboard: true,
        // Placed like Kitty's (a crop per visible piece), drawn here as
        // pixels.
        graphics: Graphics {
            kitty: true,
            ..Graphics::default()
        },
        term: "termos-kms".into(),
        cell_px: Some(PxSize {
            width: metrics.width,
            height: metrics.height,
        }),
        window_px: Some(PxSize {
            width: display.width,
            height: display.height,
        }),
        background: Some(palette().bg),
        terminal: Some(format!("TermOS console {}", env!("CARGO_PKG_VERSION"))),
        ..Caps::default()
    };
    let mut c = Console {
        seat: seat.clone(),
        display,
        screen,
        keyboard,
        pointer: Pointer::default(),
        input,
        end,
        base_size: size,
        active: true,
        ready: false,
        repeat: None,
        latency: latency::Latency::default(),
        clipboard: None,
        visual_bell: cfg.visual_bell,
        flash_until: None,
    };
    c.send(ClientMsg::Hello {
        version: PROTOCOL_VERSION,
        caps,
        cols,
        rows,
    })?;
    c.show()?;
    let drm_fd = watch(c.display.fd())?;
    let input_fd = watch(c.input.as_raw_fd())?;
    let seat_fd = watch(seat.fd()?)?;
    loop {
        let repeat_at = c
            .repeat
            .as_ref()
            .map(|(_, _, at)| tokio::time::Instant::from_std(*at));
        let flash_at = c.flash_until.map(tokio::time::Instant::from_std);
        tokio::select! {
            msg = c.end.rx.recv() => match msg {
                Some(m) => if let Some(ended) = c.server(m)? {
                    return Ok(ended);
                },
                None => return Ok(Ended::Exited("the server closed the connection".into())),
            },
            g = drm_fd.readable() => {
                // Clear first: anything that arrives while reading wakes us again.
                g?.clear_ready();
                c.flipped()?;
            }
            g = input_fd.readable() => {
                g?.clear_ready();
                c.input()?;
            }
            g = seat_fd.readable() => {
                g?.clear_ready();
                c.seat_events()?;
            }
            () = async {
                match repeat_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some((action, code, _)) = c.repeat.take() {
                    c.repeat = Some((action.clone(), code, Instant::now() + c.keyboard.interval));
                    c.act(action)?;
                }
            }
            () = async {
                match flash_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                c.flash_until = None;
                c.screen.repaint();
                c.show()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_screens_get_bigger_text() {
        // 1920 px across 527 mm: 92 dpi, no scaling.
        assert_eq!(scaled_size(16.0, 1920, Some((527, 296))), 16.0);
        // A 13" 2560×1600 laptop: 227 dpi → 2.25×.
        assert_eq!(scaled_size(16.0, 2560, Some((286, 179))), 36.0);
        // A 4K 27": 163 dpi → 1.75×.
        assert_eq!(scaled_size(16.0, 3840, Some((597, 336))), 28.0);
        assert_eq!(scaled_size(16.0, 1024, None), 16.0, "no size known (QEMU)");
    }

    #[test]
    fn fonts_from_the_config() {
        let set = fonts(&KmsConfig::default()).expect("bundled");
        assert!(set.bold.is_some() && set.fallbacks.is_empty());
        let missing = KmsConfig {
            font: Some("/nonexistent/font.ttf".into()),
            ..KmsConfig::default()
        };
        assert!(fonts(&missing).is_err());
        let bad_fallback = KmsConfig {
            fallback_fonts: vec!["/nonexistent.ttf".into()],
            ..KmsConfig::default()
        };
        assert!(
            fonts(&bad_fallback)
                .expect("a missing fallback is skipped")
                .fallbacks
                .is_empty()
        );
    }
}
