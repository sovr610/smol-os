// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos --kms`: the bare-metal console (ARCHITECTURE.md §5.9, D-073).
//!
//! A client of the server like the terminal one, but it draws the frames
//! itself, with the software rasterizer into DRM displays, and reads the
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
//!
//! It is also the image's lock screen (M13d, D-124): it locks after idle,
//! on the lid, before suspending and when the desktop asks, and while
//! locked it sends the server nothing ([`lock`], [`locking`]).
//!
//! **Several displays (M13e, D-127, D-128):** every connected display is
//! driven ([`outputs`] plans them from `[[kms.display]]`). In `extend`
//! mode (the default) each one after the first is a client of the session
//! of its own, with a workspace of its own; in `mirror` mode the first
//! one's picture is copied to the others. The pointer moves across them,
//! left to right, and the keys go to the one it is on. Plugging a display
//! in or out (udev's hotplug events) and reloading the config set them up
//! again.

mod damage;
mod display;
mod keyboard;
mod latency;
mod lock;
mod locking;
mod outputs;
mod pointer;
mod screen;
mod seat;

use std::io::{self, Write as _};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use input::event::keyboard::{KeyState, KeyboardEventTrait as _};
use input::event::pointer::{Axis, ButtonState, PointerScrollEvent as _};
use input::event::switch::{Switch, SwitchEvent, SwitchState};
use input::event::{KeyboardEvent, PointerEvent};
use input::{Event as InputEvent, Libinput};
use libseat::SeatEvent;
use termos_core::config::{Config, DisplaysMode, KmsConfig};
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::{ClientEnd, connect_frontend};
use termos_core::wire::{ConnKind, Hello};
use termos_render::caps::{Caps, Graphics, PxSize};
use termos_render::color::{ColorDepth, Palette, Rgb, XTERM_ANSI};
use termos_render::grid::Grid;
use termos_render::raster::{FontData, FontSet, Rasterizer};
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::client::Ended;
use damage::Damage;
use display::{Card, CardConnector, Display};
use keyboard::{Action, Keyboard};
use pointer::Pointer;
use screen::Screen;
use seat::{InputInterface, SeatHandle};

/// What the console prints on its standard output once its first frame is
/// on the screen. The image's session script sends that to the system
/// console, where the QEMU boot tests wait for it (D-076).
pub(crate) const READY: &str = "TERMOS-READY";

/// What it prints each time its displays are set up: how many, and their
/// names (the QEMU tests with two outputs look for it, D-128).
pub(crate) const DISPLAYS: &str = "TERMOS-DISPLAYS";

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

/// Opens the first DRM device with a connected output.
fn open_card(seat: &SeatHandle) -> anyhow::Result<Card> {
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
        let card = Card(fd);
        match display::connectors(&card) {
            Ok(c)
                if c.iter()
                    .any(|c| c.info.connected && !c.info.modes.is_empty()) =>
            {
                return Ok(card);
            }
            Ok(_) => errors.push(format!("{}: no connected output", path.display())),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    if errors.is_empty() {
        bail!("no usable display: no DRM devices in /dev/dri");
    }
    bail!("no usable display: {}", errors.join("; "))
}

/// What the console tells the desktop about one display.
fn caps(screen: &Screen, own_workspace: bool) -> Caps {
    let metrics = screen.raster.metrics();
    Caps {
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
            width: screen.width,
            height: screen.height,
        }),
        background: Some(palette().bg),
        terminal: Some(format!("TermOS console {}", env!("CARGO_PKG_VERSION"))),
        own_workspace,
        ..Caps::default()
    }
}

/// How each further display reaches the session (its own connection).
pub(crate) struct Connect {
    pub(crate) socket: PathBuf,
    pub(crate) session: String,
    /// The config file, read again when the server reloads it.
    pub(crate) config_path: PathBuf,
}

/// One display and what it shows.
struct Output {
    /// Stable for its life: server messages for it carry it.
    id: u32,
    display: Display,
    screen: Screen,
    /// Its connection to the session; none for a mirrored copy.
    tx: Option<UnboundedSender<ClientMsg>>,
    /// The text size it starts at (Ctrl+0 goes back to it).
    base_size: f32,
    scale: Option<f32>,
    /// The bell's frame shows on it.
    flashing: bool,
    lock_grid: Grid,
    /// The lock screen's own pixels: the desktop's stay out of sight.
    lock_pixels: Vec<u32>,
    lock_dirty: bool,
}

impl Output {
    fn send(&self, msg: ClientMsg) -> io::Result<()> {
        match &self.tx {
            Some(tx) => tx
                .send(msg)
                .map_err(|_| io::Error::other("the desktop server stopped")),
            None => Ok(()),
        }
    }
}

struct Console {
    seat: SeatHandle,
    card: Rc<Card>,
    outputs: Vec<Output>,
    /// The display the pointer is on, which gets the keys.
    focus: usize,
    mirror: bool,
    connect: Connect,
    config: Config,
    next_id: u32,
    /// Messages from every display's connection, by output id (`None`:
    /// that connection closed).
    srv_tx: UnboundedSender<(u32, Option<ServerMsg>)>,
    keyboard: Keyboard,
    pointer: Pointer,
    /// The pointer across all displays, left to right.
    gx: f64,
    gy: f64,
    input: Libinput,
    /// The seat is ours (not switched away).
    active: bool,
    ready: bool,
    /// A held key: what it does, and when it next repeats.
    repeat: Option<(Action, u32, Instant)>,
    /// Input→frame times, reported to the log (M9's Pi measurements).
    latency: latency::Latency,
    /// The text last copied in TermOS: in memory only, never logged.
    clipboard: Option<String>,
    /// While the bell's frame shows: when it goes.
    flash_until: Option<Instant>,
    /// The lock screen, while it shows (D-124).
    lock: Option<lock::Lock>,
    /// When the lock screen's clock changes.
    lock_minute: Option<Instant>,
    settings: locking::Settings,
    /// The last key, click or move (the lock screen's keys count).
    last_input: Instant,
    /// When idle suspend last looked.
    suspend_checked: Instant,
    suspending: bool,
    /// Where root jobs (`sudo termos system …`) answer.
    root_tx: tokio::sync::mpsc::UnboundedSender<locking::Done>,
}

/// What the console must do after a server message.
enum Next {
    Go,
    End(Ended),
    /// Set the displays up again (the config changed).
    Replan,
}

/// The start of the next wall-clock minute.
fn next_minute() -> Instant {
    let z = jiff::Zoned::now();
    let into = u64::try_from(z.second()).unwrap_or(0);
    let nanos = u64::try_from(z.subsec_nanosecond()).unwrap_or(0);
    let wait = Duration::from_secs(60 - into.min(59)).saturating_sub(Duration::from_nanos(nanos));
    Instant::now() + wait.max(Duration::from_millis(10))
}

/// How long the bell's frame shows.
const FLASH: Duration = Duration::from_millis(150);

impl Console {
    /// The display that gets the keys: the one the pointer is on (the
    /// first, when mirrored).
    fn keyed(&self) -> usize {
        if self.mirror {
            0
        } else {
            self.focus.min(self.outputs.len().saturating_sub(1))
        }
    }

    fn send(&self, msg: ClientMsg) -> io::Result<()> {
        match self.outputs.get(self.keyed()) {
            Some(o) => o.send(msg),
            None => Ok(()),
        }
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.outputs.iter().position(|o| o.id == id)
    }

    /// Puts every display's changes on it.
    fn show(&mut self) -> io::Result<()> {
        for i in 0..self.outputs.len() {
            self.show_output(i)?;
        }
        Ok(())
    }

    /// Puts display `i`'s changes on it (the lock screen's, while
    /// locked; the first display's, for a mirrored copy).
    fn show_output(&mut self, i: usize) -> io::Result<()> {
        if !self.active {
            // Shown in full when the seat comes back.
            for o in &mut self.outputs {
                let _ = o.screen.take_damage();
            }
            return Ok(());
        }
        if self.lock.is_some() {
            // The desktop's frames go on in the background, unseen; they
            // are all drawn again on unlocking.
            if let Some(o) = self.outputs.get_mut(i) {
                let _ = o.screen.take_damage();
            }
            return self.show_lock(i);
        }
        if self.mirror && i > 0 {
            // Copies follow the first display: see `mirror_copies`.
            return Ok(());
        }
        let Some(o) = self.outputs.get_mut(i) else {
            return Ok(());
        };
        let damage = o.screen.take_damage();
        if self.mirror && i == 0 && self.outputs.len() > 1 {
            self.mirror_copies(&damage)?;
        }
        let o = &mut self.outputs[i];
        if o.display.present(&o.screen.pixels, &damage)? {
            self.latency.flushed(Instant::now());
        }
        Ok(())
    }

    /// Mirror mode: the first display's changed rows into the others,
    /// cut to their size.
    fn mirror_copies(&mut self, damage: &Damage) -> io::Result<()> {
        let (first, rest) = self.outputs.split_at_mut(1);
        let src = &first[0].screen;
        for o in rest {
            let (w, h) = (
                o.screen.width.min(src.width) as usize,
                o.screen.height.min(src.height),
            );
            let mut rows = Damage::default();
            for (y0, y1) in damage.clipped(h) {
                for y in y0 as usize..y1 as usize {
                    let s = y * src.width as usize;
                    let d = y * o.screen.width as usize;
                    o.screen.pixels[d..d + w].copy_from_slice(&src.pixels[s..s + w]);
                }
                rows.add(y0, y1);
            }
            o.display.present(&o.screen.pixels, &rows)?;
        }
        Ok(())
    }

    fn font_size(&mut self, px: f32) -> io::Result<()> {
        let i = self.keyed();
        let Some(o) = self.outputs.get_mut(i) else {
            return Ok(());
        };
        if let Some((cols, rows)) = o.screen.set_font_size(px) {
            o.send(ClientMsg::Resize { cols, rows })?;
        }
        self.show()
    }

    fn act(&mut self, a: Action) -> io::Result<()> {
        if self.lock.is_some() {
            // Locked: keys are the lock screen's, and nothing leaves it.
            return match a {
                Action::Key(k) => self.lock_key(k),
                Action::Brightness(d) => {
                    self.brightness(d);
                    Ok(())
                }
                _ => Ok(()),
            };
        }
        let size = self
            .outputs
            .get(self.keyed())
            .map_or(16.0, |o| o.screen.raster.size());
        let base = self.outputs.get(self.keyed()).map_or(16.0, |o| o.base_size);
        match a {
            Action::Key(k) => {
                self.latency.key(Instant::now());
                self.send(ClientMsg::Key(k))
            }
            Action::FontBigger => self.font_size(size * 1.125),
            Action::FontSmaller => self.font_size(size / 1.125),
            Action::FontReset => self.font_size(base),
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
            Action::Brightness(d) => {
                self.brightness(d);
                Ok(())
            }
            Action::Lock => self.lock_now(),
            Action::Command(c) => self.send(ClientMsg::Command(c.to_owned())),
        }
    }

    /// The displays' sizes, left to right, as the pointer crosses them
    /// (only the first, when mirrored).
    fn pointer_space(&self) -> Vec<(u32, u32)> {
        let n = if self.mirror { 1 } else { self.outputs.len() };
        self.outputs
            .iter()
            .take(n)
            .map(|o| (o.screen.width, o.screen.height))
            .collect()
    }

    /// Moves the pointer to `(x, y)` across the displays; the mouse event
    /// for the display it is on, if the cell under it changed.
    fn pointer_to(&mut self, x: f64, y: f64) -> Option<(usize, termos_core::input::MouseEvent)> {
        let space = self.pointer_space();
        let (i, lx, ly) = outputs::locate(&space, x, y)?;
        let start: u32 = space.iter().take(i).map(|(w, _)| *w).sum();
        self.gx = f64::from(start) + lx;
        self.gy = ly;
        if i != self.focus {
            if let Some(o) = self.outputs.get_mut(self.focus) {
                o.display.hide_cursor();
            }
            self.focus = i;
            self.pointer.entered_another();
        }
        let mods = self.keyboard.mods();
        let o = &mut self.outputs[i];
        o.display.move_cursor(lx as i32, ly as i32);
        let metrics = o.screen.raster.metrics();
        let layout = o.screen.layout;
        let size = (o.screen.width, o.screen.height);
        self.pointer
            .move_to(lx, ly, size, &layout, &metrics, mods)
            .map(|e| (i, e))
    }

    fn input(&mut self) -> io::Result<()> {
        self.input.dispatch()?;
        while let Some(ev) = self.input.next() {
            if !matches!(ev, InputEvent::Switch(_)) {
                self.last_input = Instant::now();
            }
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
                        if repeats && self.lock.is_none() {
                            self.repeat =
                                Some((action.clone(), code, Instant::now() + self.keyboard.delay));
                        }
                        self.act(action)?;
                    }
                }
                InputEvent::Switch(SwitchEvent::Toggle(t)) => {
                    if t.switch() == Some(Switch::Lid) {
                        self.lid(t.switch_state() == SwitchState::On)?;
                    }
                }
                InputEvent::Pointer(p) => {
                    let events: Vec<(usize, termos_core::input::MouseEvent)> = match p {
                        PointerEvent::Motion(m) => {
                            let (x, y) = (self.gx + m.dx(), self.gy + m.dy());
                            self.pointer_to(x, y).into_iter().collect()
                        }
                        PointerEvent::MotionAbsolute(m) => {
                            // A tablet (QEMU's) spans every display.
                            let space = self.pointer_space();
                            let w: u32 = space.iter().map(|(w, _)| *w).sum();
                            let h = space.iter().map(|(_, h)| *h).max().unwrap_or(1);
                            let (x, y) = (m.absolute_x_transformed(w), m.absolute_y_transformed(h));
                            self.pointer_to(x, y).into_iter().collect()
                        }
                        PointerEvent::Button(b) => {
                            let pressed = b.button_state() == ButtonState::Pressed;
                            let i = self.keyed();
                            let o = &self.outputs[i];
                            let (layout, metrics) = (o.screen.layout, o.screen.raster.metrics());
                            self.pointer
                                .button(b.button(), pressed, &layout, &metrics, mods)
                                .map(|e| (i, e))
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
                            let i = self.keyed();
                            let o = &self.outputs[i];
                            let (layout, metrics) = (o.screen.layout, o.screen.raster.metrics());
                            self.pointer
                                .wheel(
                                    v(Axis::Vertical),
                                    v(Axis::Horizontal),
                                    &layout,
                                    &metrics,
                                    mods,
                                )
                                .into_iter()
                                .map(|e| (i, e))
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                    if self.lock.is_some() {
                        continue;
                    }
                    for (i, e) in events {
                        let target = if self.mirror { 0 } else { i };
                        if let Some(o) = self.outputs.get(target) {
                            o.send(ClientMsg::Mouse(e))?;
                        }
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
                    for o in &mut self.outputs {
                        o.display.modeset()?;
                        o.screen.repaint();
                        o.lock_dirty = true;
                    }
                    self.show()?;
                }
            }
        }
        Ok(())
    }

    fn server(&mut self, id: u32, msg: ServerMsg) -> io::Result<Next> {
        let Some(i) = self.index_of(id) else {
            return Ok(Next::Go);
        };
        match msg {
            ServerMsg::Frame(f) => {
                self.outputs[i].screen.apply(&f);
                self.latency.frame();
                self.show_output(i)?;
            }
            ServerMsg::Exit { reason } => return Ok(Next::End(Ended::Exited(reason))),
            ServerMsg::Detached { session } => return Ok(Next::End(Ended::Detached(session))),
            ServerMsg::Clipboard(text) => self.clipboard = Some(text),
            ServerMsg::Bell => {
                if self.config.kms.audible_bell && self.lock.is_none() {
                    termos_hw::sound::beep();
                }
                // A bell while the frame shows (a loop of them) adds
                // nothing; the lock screen shows none.
                if self.config.kms.visual_bell
                    && self.active
                    && self.lock.is_none()
                    && !self.outputs[i].flashing
                {
                    let o = &mut self.outputs[i];
                    o.screen.flash(o.screen.raster.palette.ansi[11]);
                    o.flashing = true;
                    self.flash_until = Some(Instant::now() + FLASH);
                    self.show_output(i)?;
                }
            }
            ServerMsg::ImageData {
                id: image,
                chunk,
                last,
            } => {
                self.outputs[i].screen.image_data(image, &chunk, last);
                self.show_output(i)?;
            }
            ServerMsg::ImageFree { id: image } => {
                self.outputs[i].screen.image_free(image);
                self.show_output(i)?;
            }
            // There is no window title on a console, nor a notification
            // service: the desktop's own toast shows it.
            ServerMsg::Title(_) | ServerMsg::Notify { .. } => {}
            // The console's keyboard reports neither repeats nor releases.
            ServerMsg::KeyboardFlags(_) => {}
            // The desktop copies addresses for a console instead: there
            // is no browser to open them in (D-118).
            ServerMsg::Open { .. } => {}
            ServerMsg::Lock => self.lock_now()?,
            ServerMsg::Suspend => self.suspend_now()?,
            ServerMsg::Reconfigure => return Ok(Next::Replan),
        }
        Ok(Next::Go)
    }

    fn flipped(&mut self) -> io::Result<()> {
        let crtcs = self.card.flips()?;
        if crtcs.is_empty() {
            return Ok(());
        }
        for o in &mut self.outputs {
            if crtcs.contains(&o.display.crtc()) {
                o.display.flipped();
            }
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
        if self.active {
            for i in 0..self.outputs.len() {
                if self.outputs[i].display.behind() {
                    self.show_output(i)?;
                }
            }
        }
        Ok(())
    }

    /// The flash ended on every display that showed it.
    fn flash_over(&mut self) -> io::Result<()> {
        self.flash_until = None;
        for o in &mut self.outputs {
            if std::mem::take(&mut o.flashing) {
                o.screen.repaint();
            }
        }
        self.show()
    }

    /// A display's screen at the configured text size, scaled for it.
    fn screen_for(&self, display: &Display, scale: Option<f32>) -> anyhow::Result<(Screen, f32)> {
        let size = outputs::text_size(
            self.config.kms.font_size,
            display.width,
            display.size_mm,
            scale,
        );
        let raster = Rasterizer::new(fonts(&self.config.kms)?, size, palette())
            .context("the console font")?;
        let mut screen = Screen::new(raster, display.width, display.height);
        screen.clear();
        Ok((screen, size))
    }

    /// Forwards a connection's messages into the console's loop.
    fn forward(&self, id: u32, end: ClientEnd) -> UnboundedSender<ClientMsg> {
        let ClientEnd { tx, mut rx } = end;
        let srv = self.srv_tx.clone();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if srv.send((id, Some(m))).is_err() {
                    return;
                }
            }
            let _ = srv.send((id, None));
        });
        tx
    }

    /// Sets the displays up from the connectors and `[[kms.display]]`:
    /// the ones that went or changed mode go, new ones come (each with
    /// its own connection, in extend mode). `first` is the connection the
    /// console started with, for the first display. A gone first display
    /// ends the console (init starts it again with the others).
    async fn replan(&mut self, mut first: Option<ClientEnd>) -> anyhow::Result<Option<Ended>> {
        let conns: Vec<CardConnector> = display::connectors(&self.card)?;
        let infos: Vec<outputs::Connector> = conns.iter().map(|c| c.info.clone()).collect();
        let wants = outputs::plan(&infos, &self.config.kms.display);
        let same = |o: &Output| {
            wants.iter().any(|w| {
                w.name == o.display.name
                    && infos
                        .iter()
                        .find(|c| c.name == w.name)
                        .and_then(|c| c.modes.get(w.mode))
                        .is_some_and(|m| (m.width, m.height, m.refresh) == o.display.mode_size())
            })
        };
        if !self.outputs.is_empty() && !same(&self.outputs[0]) {
            return Ok(Some(Ended::Exited(
                "the first display went or changed: the console starts again".into(),
            )));
        }
        let mut i = 1;
        while i < self.outputs.len() {
            if same(&self.outputs[i]) {
                i += 1;
            } else {
                let o = self.outputs.remove(i);
                tracing::info!("display {} goes", o.display.name);
                let _ = o.send(ClientMsg::Bye);
            }
        }
        for w in &wants {
            if let Some(o) = self.outputs.iter_mut().find(|o| o.display.name == w.name) {
                // Still here: a new scale takes effect at once.
                if o.scale != w.scale {
                    o.scale = w.scale;
                    o.base_size = outputs::text_size(
                        self.config.kms.font_size,
                        o.display.width,
                        o.display.size_mm,
                        w.scale,
                    );
                    if let Some((cols, rows)) = o.screen.set_font_size(o.base_size) {
                        o.send(ClientMsg::Resize { cols, rows })?;
                    }
                }
                continue;
            }
            let Some(conn) = conns.iter().find(|c| c.info.name == w.name) else {
                continue;
            };
            let taken: Vec<_> = self.outputs.iter().map(|o| o.display.crtc()).collect();
            let display = match Display::open(Rc::clone(&self.card), conn, w.mode, &taken) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("display {}: {e}", w.name);
                    continue;
                }
            };
            let (screen, size) = self.screen_for(&display, w.scale)?;
            let id = self.next_id;
            self.next_id += 1;
            let is_first = self.outputs.is_empty();
            let tx = if let Some(end) = first.take() {
                Some(self.forward(id, end))
            } else if self.mirror {
                None
            } else {
                let mut hello = Hello::new(ConnKind::Frontend);
                hello.session = Some(self.connect.session.clone());
                match connect_frontend(&self.connect.socket, &hello).await {
                    Ok((_, end)) => Some(self.forward(id, end)),
                    Err(e) => {
                        tracing::warn!("display {}: cannot reach the session: {e}", w.name);
                        continue;
                    }
                }
            };
            let o = Output {
                id,
                display,
                screen,
                tx,
                base_size: size,
                scale: w.scale,
                flashing: false,
                lock_grid: Grid::new(1, 1),
                lock_pixels: Vec::new(),
                lock_dirty: false,
            };
            // The first display follows the session, as a terminal does;
            // each further one shows a workspace of its own (D-128).
            let (cols, rows) = o.screen.cells();
            o.send(ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                caps: caps(&o.screen, !is_first && !self.mirror),
                cols,
                rows,
            })?;
            self.outputs.push(o);
        }
        if self.outputs.is_empty() {
            bail!("no usable display");
        }
        self.focus = self.focus.min(self.outputs.len() - 1);
        for o in &mut self.outputs {
            o.screen.repaint();
            o.lock_dirty = true;
        }
        if self.lock.is_some() {
            self.draw_lock();
        }
        let names: Vec<&str> = self
            .outputs
            .iter()
            .map(|o| o.display.name.as_str())
            .collect();
        tracing::info!("displays: {}", names.join(", "));
        let mut out = io::stdout();
        let _ = writeln!(out, "{DISPLAYS} {} {}", names.len(), names.join(" "));
        let _ = out.flush();
        self.show()?;
        Ok(None)
    }
}

/// Runs the console until the session ends or detaches it. `end` is the
/// first display's connection, made by the caller; `connect` says how the
/// others reach the same session. `state_dir` keeps whether the first
/// boot asked for a password.
pub(crate) async fn run(
    end: ClientEnd,
    connect: Connect,
    config: &Config,
    state_dir: &Path,
) -> anyhow::Result<Ended> {
    let cfg = &config.kms;
    if let Some(p) = config.power.brightness
        && let Err(e) =
            termos_hw::power::set_brightness(Path::new(termos_hw::power::BACKLIGHT_DIR), p)
    {
        tracing::info!("brightness at start: {e}");
    }
    let seat = SeatHandle::open()?;
    let card = Rc::new(open_card(&seat)?);
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
    let (srv_tx, mut srv_rx) = unbounded_channel();
    let (root_tx, mut root_rx) = unbounded_channel();
    let (plug_tx, mut plug_rx) = unbounded_channel::<()>();
    let mut c = Console {
        seat: seat.clone(),
        card,
        outputs: Vec::new(),
        focus: 0,
        mirror: cfg.displays == DisplaysMode::Mirror,
        connect,
        config: config.clone(),
        next_id: 0,
        srv_tx,
        keyboard,
        pointer: Pointer::default(),
        gx: 0.0,
        gy: 0.0,
        input,
        active: true,
        ready: false,
        repeat: None,
        latency: latency::Latency::default(),
        clipboard: None,
        flash_until: None,
        lock: None,
        lock_minute: None,
        settings: locking::Settings::new(config, state_dir),
        last_input: Instant::now(),
        suspend_checked: Instant::now(),
        suspending: false,
        root_tx,
    };
    if let Some(ended) = c.replan(Some(end)).await? {
        return Ok(ended);
    }
    // Displays plugged in or out set them up again.
    if let Err(e) =
        termos_hw::drives::listen_where(termos_hw::drives::Uevent::is_display_hotplug, move |_| {
            plug_tx.send(()).is_ok()
        })
    {
        tracing::info!("no display hotplug events: {e}");
    }
    // Locked before (the console restarted) or locked at start: the first
    // frame is the lock screen. Otherwise, at first boot, ask for a
    // password.
    let was_locked = c.settings.locked_file.as_ref().is_some_and(|f| f.exists());
    if was_locked || config.lock.at_start {
        c.lock_now()?;
    } else if !c.settings.boottest && !c.settings.nolock && !c.settings.asked_file.exists() {
        locking::root(
            &c.root_tx,
            locking::Job::HasPassword { first_boot: true },
            &["has-password"],
            None,
        );
    }
    c.show()?;
    let drm_fd = watch(c.card.fd())?;
    let input_fd = watch(c.input.as_raw_fd())?;
    let seat_fd = watch(seat.fd()?)?;
    loop {
        let repeat_at = c
            .repeat
            .as_ref()
            .map(|(_, _, at)| tokio::time::Instant::from_std(*at));
        let flash_at = c.flash_until.map(tokio::time::Instant::from_std);
        let power_at = c.power_deadline().map(tokio::time::Instant::from_std);
        tokio::select! {
            Some(done) = root_rx.recv() => c.root_done(done)?,
            () = async {
                match power_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => c.on_power_timer(Instant::now())?,
            Some((id, msg)) = srv_rx.recv() => match msg {
                Some(m) => match c.server(id, m)? {
                    Next::Go => {}
                    Next::End(ended) => return Ok(ended),
                    Next::Replan => {
                        match termos_core::config::Config::load(&c.connect.config_path) {
                            Ok(new) => {
                                if new.kms.displays != c.config.kms.displays {
                                    return Ok(Ended::Exited(
                                        "mirror or extend changed: the console starts again".into(),
                                    ));
                                }
                                c.config = new;
                            }
                            Err(e) => tracing::warn!("the console keeps its displays: {e}"),
                        }
                        if let Some(ended) = c.replan(None).await? {
                            return Ok(ended);
                        }
                    }
                },
                None if c.index_of(id) == Some(0) => {
                    return Ok(Ended::Exited("the server closed the connection".into()));
                }
                None => {
                    if let Some(i) = c.index_of(id) {
                        c.outputs.remove(i);
                        c.focus = c.focus.min(c.outputs.len().saturating_sub(1));
                    }
                }
            },
            Some(()) = plug_rx.recv() => {
                // A burst of events is one change.
                while plug_rx.try_recv().is_ok() {}
                if let Some(ended) = c.replan(None).await? {
                    return Ok(ended);
                }
            }
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
            } => c.flash_over()?,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
