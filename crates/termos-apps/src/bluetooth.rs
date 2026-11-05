// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth (M13e, D-128): the Network window's Bluetooth page. Power,
//! scan, pair, connect, disconnect and forget, through BlueZ
//! (`bluetoothd`) on the system bus; keyboards and mice then work through
//! libinput, headsets through PipeWire.
//!
//! Pairing asks questions: "does 123456 show on the headset?", "type this
//! PIN on the keyboard". BlueZ asks them of an agent, which the page
//! registers on D-Bus while it is open ([`agent`]); the agent hands each
//! question to the page and waits for the answer typed there. Everything
//! slow runs on a thread; the page reads the devices again only while it
//! is on screen (D-028).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;

/// How often the devices are read again while the page is on screen.
const INTERVAL: Duration = Duration::from_secs(5);
/// While scanning, new devices show sooner.
const SCANNING: Duration = Duration::from_secs(2);

/// The machine's Bluetooth adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adapter {
    /// BlueZ's object (`/org/bluez/hci0`).
    pub path: String,
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
}

/// A device the adapter knows or sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Device {
    /// BlueZ's object (`/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`).
    pub path: String,
    pub address: String,
    pub name: String,
    /// BlueZ's icon name: `audio-headset`, `input-keyboard`, …
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// Signal, while scanning.
    pub rssi: Option<i16>,
}

impl Device {
    /// What it is, in a word.
    pub fn kind(&self) -> &'static str {
        match self.icon.as_str() {
            "audio-headset" | "audio-headphones" => "headset",
            "audio-card" => "speaker",
            "input-keyboard" => "keyboard",
            "input-mouse" => "mouse",
            "input-tablet" => "tablet",
            "input-gaming" => "controller",
            "phone" => "phone",
            "computer" => "computer",
            _ => "",
        }
    }
}

/// What BlueZ has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// `None`: no Bluetooth adapter.
    pub adapter: Option<Adapter>,
    /// Connected first, then paired, then by signal.
    pub devices: Vec<Device>,
}

/// Sorts devices: connected, paired, strongest, then by name.
pub fn sort_devices(d: &mut [Device]) {
    d.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// A question pairing asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Does this number show on the device? (yes or no)
    Confirm { device: String, passkey: u32 },
    /// May this device pair, or use this service? (yes or no)
    Authorize {
        device: String,
        service: Option<String>,
    },
    /// The PIN the device has (typed).
    Pin { device: String },
    /// The six-digit passkey the device shows (typed).
    Passkey { device: String },
    /// Type this on the device (a keyboard); no answer.
    Show { device: String, code: String },
    /// BlueZ gave up on the last question.
    Cancel,
}

/// The answer to an [`Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Text(String),
}

/// A question, and where its answer goes.
pub struct Question {
    pub ask: Ask,
    reply: Option<Box<dyn FnOnce(Answer) + Send>>,
}

impl std::fmt::Debug for Question {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Question")
            .field("ask", &self.ask)
            .finish_non_exhaustive()
    }
}

impl Question {
    pub fn new(ask: Ask, reply: Option<Box<dyn FnOnce(Answer) + Send>>) -> Self {
        Self { ask, reply }
    }

    /// Answers it (dropping a question unanswered says no).
    pub fn answer(mut self, a: Answer) {
        if let Some(r) = self.reply.take() {
            r(a);
        }
    }
}

/// Where the questions go: the page, which is woken for each.
#[derive(Clone)]
pub struct Asker {
    tx: Sender<Question>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Asker {
    pub fn new(tx: Sender<Question>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { tx, wake }
    }

    /// Hands `q` to the page; false when it is gone.
    pub fn ask(&self, q: Question) -> bool {
        let sent = self.tx.send(q).is_ok();
        (self.wake)();
        sent
    }
}

/// Where the page's information comes from, and what it changes.
pub trait Backend: Send + Sync {
    /// `None` where BlueZ does not run.
    fn state(&self) -> Option<Result<State, String>>;
    fn power(&self, adapter: &str, on: bool) -> Result<(), String>;
    fn scan(&self, adapter: &str, on: bool) -> Result<(), String>;
    /// Pairs, trusts and connects; the agent's questions come meanwhile.
    fn pair(&self, device: &str) -> Result<(), String>;
    fn connect(&self, device: &str, on: bool) -> Result<(), String>;
    fn forget(&self, adapter: &str, device: &str) -> Result<(), String>;
    /// Registers the pairing agent, whose questions go to `asker`.
    fn agent(&self, asker: Asker) -> Result<(), String>;
}

#[derive(Default)]
struct Shared {
    state: Option<Result<State, String>>,
    loaded: bool,
    message: Option<String>,
}

/// The Bluetooth page (a tab of the Network window).
pub struct BluetoothTab {
    backend: Arc<dyn Backend>,
    shared: Arc<Mutex<Shared>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    asks: Receiver<Question>,
    ask_tx: Sender<Question>,
    /// The question on screen, and what is typed for it.
    question: Option<(Question, String)>,
    agent: bool,
    selected: usize,
    pub(crate) next: Option<Instant>,
    /// Where `[bluetooth] enabled` is saved.
    config: Option<PathBuf>,
}

impl std::fmt::Debug for BluetoothTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BluetoothTab")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

/// `dev_AA_BB_CC_DD_EE_FF` → `AA:BB:CC:DD:EE:FF`.
fn address_of(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|l| l.strip_prefix("dev_"))
        .map(|a| a.replace('_', ":"))
        .unwrap_or_else(|| path.to_owned())
}

impl BluetoothTab {
    /// The tab for this machine.
    pub fn open(config: Option<PathBuf>) -> Self {
        Self::with_backend(Arc::new(System::default()), config)
    }

    pub fn with_backend(backend: Arc<dyn Backend>, config: Option<PathBuf>) -> Self {
        let (ask_tx, asks) = channel();
        Self {
            backend,
            shared: Arc::new(Mutex::new(Shared::default())),
            waker: Arc::new(Mutex::new(None)),
            asks,
            ask_tx,
            question: None,
            agent: false,
            selected: 0,
            next: None,
            config,
        }
    }

    /// Starts reading (and registers the agent) once the window has a
    /// waker.
    pub fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
        self.start();
    }

    /// The tab came on screen: read now, then every few seconds.
    pub fn start(&mut self) {
        if !self.agent {
            self.agent = true;
            let waker = Arc::clone(&self.waker);
            let asker = Asker::new(
                self.ask_tx.clone(),
                Arc::new(move || {
                    if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                        w.wake();
                    }
                }),
            );
            let backend = Arc::clone(&self.backend);
            let shared = Arc::clone(&self.shared);
            std::thread::spawn(move || {
                if let Err(e) = backend.agent(asker) {
                    tracing::info!("no Bluetooth pairing agent: {e}");
                    if let Ok(mut s) = shared.lock() {
                        s.message
                            .get_or_insert(format!("pairing needs BlueZ's agent: {e}"));
                    }
                }
            });
        }
        self.refresh(Duration::ZERO);
        self.next = Some(Instant::now() + INTERVAL);
    }

    /// Reads everything again on a thread, after `delay`, then redraws.
    fn refresh(&self, delay: Duration) {
        let (backend, shared, waker) = (
            Arc::clone(&self.backend),
            Arc::clone(&self.shared),
            Arc::clone(&self.waker),
        );
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let state = backend.state();
            if let Ok(mut s) = shared.lock() {
                s.state = state;
                s.loaded = true;
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    /// Reads everything now (tests).
    pub fn refresh_now(&self) {
        let state = self.backend.state();
        if let Ok(mut s) = self.shared.lock() {
            s.state = state;
            s.loaded = true;
        }
    }

    /// Runs `job` on a thread; its result becomes the message, then the
    /// devices are read again.
    fn background(
        &self,
        start: String,
        job: impl FnOnce(&dyn Backend) -> Result<String, String> + Send + 'static,
    ) {
        self.say(start);
        let (backend, shared, waker) = (
            Arc::clone(&self.backend),
            Arc::clone(&self.shared),
            Arc::clone(&self.waker),
        );
        std::thread::spawn(move || {
            let msg = match job(backend.as_ref()) {
                Ok(m) | Err(m) => m,
            };
            let state = backend.state();
            if let Ok(mut s) = shared.lock() {
                s.message = Some(msg);
                s.state = state;
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    fn say(&self, msg: String) {
        if let Ok(mut s) = self.shared.lock() {
            s.message = Some(msg);
        }
    }

    fn state(&self) -> State {
        self.shared
            .lock()
            .ok()
            .and_then(|s| match &s.state {
                Some(Ok(st)) => Some(st.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The agent's questions that came: the last one is shown.
    fn take_questions(&mut self) {
        while let Ok(q) = self.asks.try_recv() {
            if q.ask == Ask::Cancel {
                self.question = None;
                self.say("pairing cancelled".into());
            } else {
                // A new question replaces (and refuses) an older one.
                self.question = Some((q, String::new()));
            }
        }
    }

    /// A device's name for a question.
    fn label(&self, path: &str) -> String {
        self.state()
            .devices
            .into_iter()
            .find(|d| d.path == path)
            .map_or_else(|| address_of(path), |d| d.name)
    }

    fn question_key(&mut self, key: KeyEvent) -> AppEffect {
        let Some((q, mut typed)) = self.question.take() else {
            return AppEffect::None;
        };
        let typed_kind = matches!(q.ask, Ask::Pin { .. } | Ask::Passkey { .. });
        match key.code {
            KeyCode::Esc => {
                q.answer(Answer::No);
                self.say("refused".into());
            }
            KeyCode::Char('y') if !typed_kind => q.answer(Answer::Yes),
            KeyCode::Char('n') if !typed_kind => {
                q.answer(Answer::No);
                self.say("refused".into());
            }
            KeyCode::Enter if matches!(q.ask, Ask::Show { .. }) => q.answer(Answer::Yes),
            KeyCode::Enter if typed_kind => {
                let ok = match q.ask {
                    Ask::Passkey { .. } => typed.parse::<u32>().is_ok_and(|n| n <= 999_999),
                    _ => (1..=16).contains(&typed.chars().count()),
                };
                if ok {
                    q.answer(Answer::Text(typed));
                } else {
                    self.question = Some((q, typed));
                }
            }
            KeyCode::Backspace if typed_kind => {
                typed.pop();
                self.question = Some((q, typed));
            }
            KeyCode::Char(c) if typed_kind && !c.is_control() && typed.chars().count() < 16 => {
                let digits_only = matches!(q.ask, Ask::Passkey { .. });
                if !digits_only || c.is_ascii_digit() {
                    typed.push(c);
                }
                self.question = Some((q, typed));
            }
            _ => self.question = Some((q, typed)),
        }
        AppEffect::Redraw
    }

    /// Whether a question waits for an answer (the window gives it keys).
    pub fn asking(&mut self) -> bool {
        self.take_questions();
        self.question.is_some()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if self.asking() {
            return self.question_key(key);
        }
        let state = self.state();
        let Some(adapter) = state.adapter.clone() else {
            return match key.code {
                KeyCode::Char('r') => {
                    self.refresh(Duration::ZERO);
                    AppEffect::Redraw
                }
                _ => AppEffect::None,
            };
        };
        let device = state.devices.get(self.selected).cloned();
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(state.devices.len().saturating_sub(1));
            }
            KeyCode::Char('r') => self.refresh(Duration::ZERO),
            KeyCode::Char('p') => {
                let on = !adapter.powered;
                let config = self.config.clone();
                self.background(
                    format!("turning Bluetooth {}…", if on { "on" } else { "off" }),
                    move |b| {
                        b.power(&adapter.path, on)?;
                        if let Some(path) = config {
                            crate::config_edit::edit(&path, |d| {
                                crate::config_edit::table(d, "bluetooth")
                                    .insert("enabled", toml_edit::value(on));
                            })?;
                        }
                        Ok(format!("Bluetooth {}", if on { "on" } else { "off" }))
                    },
                );
            }
            KeyCode::Char('s') if !adapter.powered => {
                self.say("Bluetooth is off (p turns it on)".into())
            }
            KeyCode::Char('s') => {
                let on = !adapter.discovering;
                self.background(
                    if on { "scanning…" } else { "scan stopped" }.into(),
                    move |b| {
                        b.scan(&adapter.path, on)?;
                        Ok(if on {
                            "scanning: devices in pairing mode appear below"
                        } else {
                            "scan stopped"
                        }
                        .into())
                    },
                );
                self.next = Some(Instant::now() + SCANNING);
            }
            KeyCode::Enter => {
                let Some(d) = device else {
                    return AppEffect::None;
                };
                let (path, name) = (d.path.clone(), d.name.clone());
                if d.connected {
                    self.say(format!("{name} is connected"));
                } else if d.paired {
                    self.background(format!("connecting {name}…"), move |b| {
                        b.connect(&path, true)
                            .map(|()| format!("{name} connected"))
                            .map_err(|e| format!("{name}: {e}"))
                    });
                } else {
                    self.background(format!("pairing with {name}…"), move |b| {
                        b.pair(&path)
                            .map(|()| format!("{name} paired and connected"))
                            .map_err(|e| format!("{name}: {e}"))
                    });
                }
            }
            KeyCode::Char('d') => {
                let Some(d) = device.filter(|d| d.connected) else {
                    return AppEffect::None;
                };
                let (path, name) = (d.path, d.name);
                self.background(format!("disconnecting {name}…"), move |b| {
                    b.connect(&path, false)
                        .map(|()| format!("{name} disconnected"))
                });
            }
            KeyCode::Char('f') | KeyCode::Delete => {
                let Some(d) = device.filter(|d| d.paired) else {
                    return AppEffect::None;
                };
                let (path, name) = (d.path, d.name);
                self.background(format!("forgetting {name}…"), move |b| {
                    b.forget(&adapter.path, &path)
                        .map(|()| format!("forgot {name}"))
                });
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    pub fn on_timer(&mut self, now: Instant) {
        let scanning = self.state().adapter.is_some_and(|a| a.discovering);
        self.refresh(Duration::ZERO);
        self.next = Some(now + if scanning { SCANNING } else { INTERVAL });
    }

    pub fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.take_questions();
        if area.height < 4 || area.width < 30 {
            return;
        }
        let c = &theme.colors;
        let state = self
            .shared
            .lock()
            .ok()
            .map(|s| (s.state.clone(), s.loaded, s.message.clone()));
        let Some((state, loaded, message)) = state else {
            return;
        };
        let width = usize::from(area.width.saturating_sub(2));
        let bottom = area.y + area.height - 2;
        let line = |buf: &mut Buffer, y: u16, x: u16, text: &str, style: Style| {
            if y < bottom {
                buf.set_stringn(
                    area.x + x,
                    y,
                    text,
                    width.saturating_sub(usize::from(x)),
                    tui_style(style),
                );
            }
        };
        let muted = Style::new().fg(c.muted);
        let mut y = area.y;
        let mut keys = "r read again · Esc close";
        match (state, loaded) {
            (_, false) => line(buf, y, 1, "Reading Bluetooth…", muted),
            (None, true) => line(
                buf,
                y,
                1,
                "Bluetooth is managed by this system's own settings",
                muted,
            ),
            (Some(Err(e)), true) => {
                line(
                    buf,
                    y,
                    1,
                    &format!("Bluetooth: {e}"),
                    Style::new().fg(c.error),
                );
            }
            (Some(Ok(State { adapter: None, .. })), true) => {
                line(buf, y, 1, "No Bluetooth adapter", muted);
            }
            (
                Some(Ok(State {
                    adapter: Some(a),
                    devices,
                })),
                true,
            ) => {
                let status = match (a.powered, a.discovering) {
                    (false, _) => "off",
                    (true, true) => "on, scanning",
                    (true, false) => "on",
                };
                line(
                    buf,
                    y,
                    1,
                    &format!("Bluetooth ({}: {status})", a.name),
                    Style::new().fg(c.fg).attrs(Attrs::BOLD),
                );
                y += 2;
                keys =
                    "↑↓ choose · Enter pair/connect · d disconnect · f forget · s scan · p power";
                if devices.is_empty() {
                    line(
                        buf,
                        y,
                        3,
                        if a.powered {
                            "No devices (s scans; put the device in pairing mode)"
                        } else {
                            "Bluetooth is off (p turns it on)"
                        },
                        muted,
                    );
                }
                for (i, d) in devices.iter().enumerate() {
                    let selected = i == self.selected;
                    let paint = |colour| {
                        if selected {
                            c.selected(Style::new())
                        } else {
                            Style::new().fg(colour)
                        }
                    };
                    if selected && y < bottom {
                        buf.set_style(
                            TuiRect::new(area.x, y, area.width, 1),
                            tui_style(paint(c.fg)),
                        );
                    }
                    let status = if d.connected {
                        "connected"
                    } else if d.paired {
                        "paired"
                    } else {
                        ""
                    };
                    let name: String = d.name.chars().take(26).collect();
                    line(buf, y, 3, &name, paint(c.fg));
                    line(buf, y, 31, d.kind(), paint(c.muted));
                    if let Some(rssi) = d.rssi.filter(|_| !d.paired) {
                        line(buf, y, 43, crate::network::bars(rssi), paint(c.accent));
                    }
                    line(buf, y, 43, status, paint(c.accent));
                    y += 1;
                }
            }
        }
        // The last two rows: a question or a message, then the keys.
        let (msg, msg_style, keys) = match &self.question {
            Some((q, typed)) => {
                let (text, keys) = match &q.ask {
                    Ask::Confirm { device, passkey } => (
                        format!("Does {passkey:06} show on {}?", self.label(device)),
                        "y yes · n no",
                    ),
                    Ask::Authorize { device, service } => (
                        match service {
                            Some(s) => format!("Let {} use service {s}?", self.label(device)),
                            None => format!("Pair with {}?", self.label(device)),
                        },
                        "y yes · n no",
                    ),
                    Ask::Pin { device } => (
                        format!("PIN for {}: {typed}▏", self.label(device)),
                        "the PIN in its manual (often 0000) · Enter · Esc refuses",
                    ),
                    Ask::Passkey { device } => (
                        format!("The number {} shows: {typed}▏", self.label(device)),
                        "six digits · Enter · Esc refuses",
                    ),
                    Ask::Show { device, code } => (
                        format!("Type {code} on {}, then its Enter key", self.label(device)),
                        "Enter when done · Esc refuses",
                    ),
                    Ask::Cancel => (String::new(), ""),
                };
                (text, Style::new().fg(c.fg).attrs(Attrs::BOLD), keys)
            }
            None => (message.unwrap_or_default(), muted, keys),
        };
        buf.set_stringn(area.x + 1, bottom, &msg, width, tui_style(msg_style));
        buf.set_stringn(area.x + 1, bottom + 1, keys, width, tui_style(muted));
    }
}

/// Turns the adapter on or off (`[bluetooth] enabled`, at start).
pub fn set_power(on: bool) -> Result<(), String> {
    let s = System::default();
    let state = s.state().ok_or("BlueZ is not running")??;
    let adapter = state.adapter.ok_or("no Bluetooth adapter")?;
    if adapter.powered != on {
        s.power(&adapter.path, on)?;
    }
    Ok(())
}

/// This machine: BlueZ on the system bus.
#[derive(Default)]
struct System {
    /// One connection: the agent lives on it, so it lasts as long as the
    /// page (BlueZ forgets the agent when it closes).
    #[cfg(target_os = "linux")]
    conn: Mutex<Option<zbus::blocking::Connection>>,
}

#[cfg(not(target_os = "linux"))]
impl Backend for System {
    fn state(&self) -> Option<Result<State, String>> {
        None
    }
    fn power(&self, _: &str, _: bool) -> Result<(), String> {
        Err(OTHER.into())
    }
    fn scan(&self, _: &str, _: bool) -> Result<(), String> {
        Err(OTHER.into())
    }
    fn pair(&self, _: &str) -> Result<(), String> {
        Err(OTHER.into())
    }
    fn connect(&self, _: &str, _: bool) -> Result<(), String> {
        Err(OTHER.into())
    }
    fn forget(&self, _: &str, _: &str) -> Result<(), String> {
        Err(OTHER.into())
    }
    fn agent(&self, _: Asker) -> Result<(), String> {
        Err(OTHER.into())
    }
}

#[cfg(not(target_os = "linux"))]
const OTHER: &str = "Bluetooth is managed by this system's own settings";

#[cfg(target_os = "linux")]
impl System {
    /// The system bus, if BlueZ is on it.
    fn conn(&self) -> Option<zbus::blocking::Connection> {
        let mut guard = self.conn.lock().ok()?;
        if guard.is_none() {
            let conn = zbus::blocking::Connection::system().ok()?;
            let has: bool = conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &(bluez::SERVICE,),
                )
                .and_then(|m| m.body().deserialize::<bool>())
                .unwrap_or(false);
            if !has {
                return None;
            }
            *guard = Some(conn);
        }
        guard.clone()
    }

    fn need(&self) -> Result<zbus::blocking::Connection, String> {
        self.conn()
            .ok_or_else(|| "Bluetooth (BlueZ) is not running".to_owned())
    }
}

#[cfg(target_os = "linux")]
impl Backend for System {
    fn state(&self) -> Option<Result<State, String>> {
        let conn = self.conn()?;
        Some(bluez::state(&conn))
    }

    fn power(&self, adapter: &str, on: bool) -> Result<(), String> {
        bluez::set(&self.need()?, adapter, "org.bluez.Adapter1", "Powered", on)
    }

    fn scan(&self, adapter: &str, on: bool) -> Result<(), String> {
        let method = if on {
            "StartDiscovery"
        } else {
            "StopDiscovery"
        };
        bluez::call(&self.need()?, adapter, "org.bluez.Adapter1", method, &())
    }

    fn pair(&self, device: &str) -> Result<(), String> {
        let conn = self.need()?;
        bluez::call(&conn, device, "org.bluez.Device1", "Pair", &())?;
        // Trusted: it may connect again by itself (a headset at power-on).
        bluez::set(&conn, device, "org.bluez.Device1", "Trusted", true)?;
        bluez::call(&conn, device, "org.bluez.Device1", "Connect", &())
    }

    fn connect(&self, device: &str, on: bool) -> Result<(), String> {
        let method = if on { "Connect" } else { "Disconnect" };
        bluez::call(&self.need()?, device, "org.bluez.Device1", method, &())
    }

    fn forget(&self, adapter: &str, device: &str) -> Result<(), String> {
        let path = zbus::zvariant::ObjectPath::try_from(device).map_err(|e| e.to_string())?;
        bluez::call(
            &self.need()?,
            adapter,
            "org.bluez.Adapter1",
            "RemoveDevice",
            &(path,),
        )
    }

    fn agent(&self, asker: Asker) -> Result<(), String> {
        let conn = self.need()?;
        conn.object_server()
            .at(agent::PATH, agent::Agent::new(asker))
            .map_err(|e| e.to_string())?;
        let path = zbus::zvariant::ObjectPath::try_from(agent::PATH).map_err(|e| e.to_string())?;
        let manager = "org.bluez.AgentManager1";
        bluez::call(
            &conn,
            "/org/bluez",
            manager,
            "RegisterAgent",
            &(&path, agent::CAPABILITY),
        )?;
        bluez::call(
            &conn,
            "/org/bluez",
            manager,
            "RequestDefaultAgent",
            &(&path,),
        )
    }
}

/// BlueZ's objects and calls.
#[cfg(target_os = "linux")]
mod bluez {
    use std::collections::HashMap;

    use zbus::blocking::Connection;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    use super::{Adapter, Device, State, sort_devices};

    pub(super) const SERVICE: &str = "org.bluez";
    type Props = HashMap<String, OwnedValue>;
    type Objects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

    fn text(p: &Props, key: &str) -> Option<String> {
        p.get(key)
            .and_then(|v| v.downcast_ref::<&str>().ok())
            .map(str::to_owned)
    }

    fn flag(p: &Props, key: &str) -> bool {
        p.get(key)
            .and_then(|v| v.downcast_ref::<bool>().ok())
            .unwrap_or(false)
    }

    fn path(p: &Props, key: &str) -> Option<String> {
        p.get(key)
            .and_then(|v| v.downcast_ref::<ObjectPath<'_>>().ok())
            .map(|p| p.to_string())
    }

    pub(super) fn state(conn: &Connection) -> Result<State, String> {
        let objs: Objects = conn
            .call_method(
                Some(SERVICE),
                "/",
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .and_then(|m| m.body().deserialize())
            .map_err(|e| e.to_string())?;
        let mut adapters: Vec<Adapter> = objs
            .iter()
            .filter_map(|(p, ifaces)| {
                let a = ifaces.get("org.bluez.Adapter1")?;
                Some(Adapter {
                    path: p.to_string(),
                    name: text(a, "Alias")
                        .or_else(|| text(a, "Name"))
                        .unwrap_or_default(),
                    powered: flag(a, "Powered"),
                    discovering: flag(a, "Discovering"),
                })
            })
            .collect();
        adapters.sort_by(|a, b| a.path.cmp(&b.path));
        let Some(adapter) = adapters.into_iter().next() else {
            return Ok(State::default());
        };
        let mut devices: Vec<Device> = objs
            .iter()
            .filter_map(|(p, ifaces)| {
                let d = ifaces.get("org.bluez.Device1")?;
                if path(d, "Adapter").as_deref() != Some(adapter.path.as_str()) {
                    return None;
                }
                let address = text(d, "Address").unwrap_or_default();
                Some(Device {
                    path: p.to_string(),
                    name: text(d, "Alias")
                        .or_else(|| text(d, "Name"))
                        .unwrap_or_else(|| address.clone()),
                    address,
                    icon: text(d, "Icon").unwrap_or_default(),
                    paired: flag(d, "Paired"),
                    trusted: flag(d, "Trusted"),
                    connected: flag(d, "Connected"),
                    rssi: d.get("RSSI").and_then(|v| v.downcast_ref::<i16>().ok()),
                })
            })
            .collect();
        sort_devices(&mut devices);
        Ok(State {
            adapter: Some(adapter),
            devices,
        })
    }

    pub(super) fn call<B>(
        conn: &Connection,
        object: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> Result<(), String>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        conn.call_method(Some(SERVICE), object, Some(iface), method, body)
            .map(drop)
            .map_err(|e| match e {
                zbus::Error::MethodError(name, Some(why), _) => {
                    format!("{} ({why})", name.trim_start_matches("org.bluez.Error."))
                }
                other => other.to_string(),
            })
    }

    pub(super) fn set(
        conn: &Connection,
        object: &str,
        iface: &str,
        prop: &str,
        on: bool,
    ) -> Result<(), String> {
        conn.call_method(
            Some(SERVICE),
            object,
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(iface, prop, Value::from(on)),
        )
        .map(drop)
        .map_err(|e| e.to_string())
    }
}

/// The pairing agent BlueZ asks (`org.bluez.Agent1`): each question goes
/// to the page, and the D-Bus reply waits for its answer.
#[cfg(target_os = "linux")]
pub mod agent {
    use zbus::zvariant::OwnedObjectPath;

    use super::{Answer, Ask, Asker, Question};

    /// Where the agent is served.
    pub const PATH: &str = "/org/termos/bluetooth/agent";
    /// The page shows codes and takes typed ones.
    pub const CAPABILITY: &str = "KeyboardDisplay";

    pub struct Agent {
        asker: Asker,
    }

    impl Agent {
        pub fn new(asker: Asker) -> Self {
            Self { asker }
        }

        async fn ask(&self, ask: Ask) -> Result<Answer, AgentError> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let q = Question::new(
                ask,
                Some(Box::new(move |a| {
                    let _ = tx.send(a);
                })),
            );
            if !self.asker.ask(q) {
                return Err(AgentError::Canceled("the Bluetooth page is closed".into()));
            }
            // A question dropped unanswered (the page closed, or another
            // question replaced it) is a no.
            rx.await
                .map_err(|_| AgentError::Canceled("not answered".into()))
        }

        async fn yes(&self, ask: Ask) -> Result<(), AgentError> {
            match self.ask(ask).await? {
                Answer::Yes => Ok(()),
                _ => Err(AgentError::Rejected("refused".into())),
            }
        }
    }

    /// BlueZ's agent errors.
    #[derive(Debug, zbus::DBusError)]
    #[zbus(prefix = "org.bluez.Error")]
    pub enum AgentError {
        #[zbus(error)]
        ZBus(zbus::Error),
        Rejected(String),
        Canceled(String),
    }

    #[zbus::interface(name = "org.bluez.Agent1")]
    impl Agent {
        async fn release(&self) {}

        async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
            match self
                .ask(Ask::Pin {
                    device: device.to_string(),
                })
                .await?
            {
                Answer::Text(t) => Ok(t),
                _ => Err(AgentError::Rejected("no PIN".into())),
            }
        }

        async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
            let q = Question::new(
                Ask::Show {
                    device: device.to_string(),
                    code: pincode,
                },
                None,
            );
            self.asker.ask(q);
        }

        async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, AgentError> {
            match self
                .ask(Ask::Passkey {
                    device: device.to_string(),
                })
                .await?
            {
                Answer::Text(t) => t
                    .parse()
                    .map_err(|_| AgentError::Rejected("not a number".into())),
                _ => Err(AgentError::Rejected("no passkey".into())),
            }
        }

        async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, _entered: u16) {
            let q = Question::new(
                Ask::Show {
                    device: device.to_string(),
                    code: format!("{passkey:06}"),
                },
                None,
            );
            self.asker.ask(q);
        }

        async fn request_confirmation(
            &self,
            device: OwnedObjectPath,
            passkey: u32,
        ) -> Result<(), AgentError> {
            self.yes(Ask::Confirm {
                device: device.to_string(),
                passkey,
            })
            .await
        }

        async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
            self.yes(Ask::Authorize {
                device: device.to_string(),
                service: None,
            })
            .await
        }

        async fn authorize_service(
            &self,
            device: OwnedObjectPath,
            uuid: String,
        ) -> Result<(), AgentError> {
            self.yes(Ask::Authorize {
                device: device.to_string(),
                service: Some(uuid),
            })
            .await
        }

        async fn cancel(&self) {
            self.asker.ask(Question::new(Ask::Cancel, None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::input::Mods;

    /// An adapter with a paired keyboard and a headset in pairing mode.
    struct Fake {
        log: Mutex<Vec<String>>,
        asker: Mutex<Option<Asker>>,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                log: Mutex::new(Vec::new()),
                asker: Mutex::new(None),
            })
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().expect("log").clone()
        }

        fn push(&self, s: String) {
            self.log.lock().expect("log").push(s);
        }
    }

    impl Backend for Fake {
        fn state(&self) -> Option<Result<State, String>> {
            let mut devices = vec![
                Device {
                    path: "/org/bluez/hci0/dev_AC_80_0A_11_22_33".into(),
                    address: "AC:80:0A:11:22:33".into(),
                    name: "WH-1000XM4".into(),
                    icon: "audio-headset".into(),
                    rssi: Some(-52),
                    ..Device::default()
                },
                Device {
                    path: "/org/bluez/hci0/dev_F0_11_22_33_44_55".into(),
                    address: "F0:11:22:33:44:55".into(),
                    name: "MX Keys".into(),
                    icon: "input-keyboard".into(),
                    paired: true,
                    trusted: true,
                    connected: true,
                    ..Device::default()
                },
            ];
            sort_devices(&mut devices);
            Some(Ok(State {
                adapter: Some(Adapter {
                    path: "/org/bluez/hci0".into(),
                    name: "termos".into(),
                    powered: true,
                    discovering: true,
                }),
                devices,
            }))
        }

        fn power(&self, a: &str, on: bool) -> Result<(), String> {
            self.push(format!("power {a} {on}"));
            Ok(())
        }

        fn scan(&self, a: &str, on: bool) -> Result<(), String> {
            self.push(format!("scan {a} {on}"));
            Ok(())
        }

        fn pair(&self, d: &str) -> Result<(), String> {
            // BlueZ asks the agent while Pair runs.
            let asker = self
                .asker
                .lock()
                .expect("asker")
                .clone()
                .ok_or("no agent")?;
            let (tx, rx) = channel();
            asker.ask(Question::new(
                Ask::Confirm {
                    device: d.to_owned(),
                    passkey: 42,
                },
                Some(Box::new(move |a| {
                    let _ = tx.send(a);
                })),
            ));
            let answer = rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
            self.push(format!("pair {d} {answer:?}"));
            match answer {
                Answer::Yes => Ok(()),
                _ => Err("Rejected".into()),
            }
        }

        fn connect(&self, d: &str, on: bool) -> Result<(), String> {
            self.push(format!("connect {d} {on}"));
            Ok(())
        }

        fn forget(&self, a: &str, d: &str) -> Result<(), String> {
            self.push(format!("forget {a} {d}"));
            Ok(())
        }

        fn agent(&self, asker: Asker) -> Result<(), String> {
            *self.asker.lock().expect("asker") = Some(asker);
            self.push("agent".into());
            Ok(())
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::empty())
    }

    fn render(tab: &mut BluetoothTab) -> String {
        let area = TuiRect::new(0, 0, 80, 9);
        let mut buf = Buffer::empty(area);
        tab.render(area, &mut buf, &Theme::default());
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait(mut what: impl FnMut() -> bool) {
        for _ in 0..300 {
            if what() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn the_page_shows_the_adapter_and_devices() {
        let mut tab = BluetoothTab::with_backend(Fake::new(), None);
        assert!(render(&mut tab).contains("Reading Bluetooth"));
        tab.refresh_now();
        insta::assert_snapshot!("bluetooth-page", render(&mut tab));
    }

    #[test]
    fn pairing_asks_on_the_page() {
        let fake = Fake::new();
        let mut tab = BluetoothTab::with_backend(Arc::clone(&fake) as Arc<dyn Backend>, None);
        tab.start();
        wait(|| fake.log().contains(&"agent".to_owned()));
        tab.refresh_now();
        // The headset (second: the keyboard is connected) pairs.
        tab.handle_key(key(KeyCode::Down));
        tab.handle_key(key(KeyCode::Enter));
        wait(|| tab.asking());
        let shown = render(&mut tab);
        assert!(shown.contains("Does 000042 show on WH-1000XM4?"), "{shown}");
        // Keys answer the question, not the list.
        tab.handle_key(key(KeyCode::Char('y')));
        wait(|| fake.log().iter().any(|l| l.starts_with("pair")));
        assert_eq!(
            fake.log()[1],
            "pair /org/bluez/hci0/dev_AC_80_0A_11_22_33 Yes"
        );
        wait(|| render(&mut tab).contains("WH-1000XM4 paired and connected"));
        // Typed codes: digits only for a passkey; Esc refuses.
        let (tx, rx) = channel();
        let asker = fake.asker.lock().expect("asker").clone().expect("agent");
        asker.ask(Question::new(
            Ask::Passkey {
                device: "/org/bluez/hci0/dev_00_11".into(),
            },
            Some(Box::new(move |a| {
                let _ = tx.send(a);
            })),
        ));
        assert!(tab.asking());
        for c in "12a3456".chars() {
            tab.handle_key(key(KeyCode::Char(c)));
        }
        assert!(render(&mut tab).contains("The number 00:11 shows: 123456▏"));
        tab.handle_key(key(KeyCode::Enter));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Ok(Answer::Text("123456".into()))
        );
        // Cancel clears a question.
        asker.ask(Question::new(
            Ask::Show {
                device: "x".into(),
                code: "1234".into(),
            },
            None,
        ));
        assert!(tab.asking());
        asker.ask(Question::new(Ask::Cancel, None));
        assert!(!tab.asking());
    }

    #[test]
    fn power_is_saved_and_devices_are_forgotten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        let fake = Fake::new();
        let mut tab =
            BluetoothTab::with_backend(Arc::clone(&fake) as Arc<dyn Backend>, Some(config.clone()));
        tab.refresh_now();
        tab.handle_key(key(KeyCode::Char('p')));
        wait(|| config.exists());
        assert_eq!(fake.log(), ["power /org/bluez/hci0 false"]);
        let c = termos_core::config::Config::load(&config).expect("valid");
        assert!(!c.bluetooth.enabled);
        tab.handle_key(key(KeyCode::Char('f')));
        wait(|| fake.log().len() == 2);
        assert_eq!(
            fake.log()[1],
            "forget /org/bluez/hci0 /org/bluez/hci0/dev_F0_11_22_33_44_55"
        );
        // Only a connected device disconnects; only a paired one is forgotten.
        tab.handle_key(key(KeyCode::Down));
        assert_eq!(tab.handle_key(key(KeyCode::Char('f'))), AppEffect::None);
        assert_eq!(tab.handle_key(key(KeyCode::Char('d'))), AppEffect::None);
    }

    #[test]
    fn devices_sort_and_name_themselves() {
        assert_eq!(address_of("/org/bluez/hci0/dev_AA_BB"), "AA:BB");
        let d = Device {
            icon: "input-mouse".into(),
            ..Device::default()
        };
        assert_eq!(d.kind(), "mouse");
    }

    /// The real agent, over a socket pair: the test plays BlueZ.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(deprecated)] // zbus's std-socket builder (the other needs async-io directly)
    fn the_agent_answers_bluez() {
        use zbus::zvariant::ObjectPath;
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let (tx, rx) = channel::<Question>();
        let asker = Asker::new(tx, Arc::new(|| {}));
        let guid = zbus::Guid::generate();
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::unix_stream(a)
                .server(guid)
                .and_then(|b| b.p2p().serve_at(agent::PATH, agent::Agent::new(asker)))
                .and_then(|b| b.build())
        });
        let bluez = zbus::blocking::connection::Builder::unix_stream(b)
            .p2p()
            .build()
            .expect("client");
        let _agent = server.join().expect("thread").expect("server");
        // The page: yes to the number, a PIN, no to the rest.
        std::thread::spawn(move || {
            for q in rx {
                let a = match &q.ask {
                    Ask::Confirm {
                        passkey: 123_456, ..
                    } => Answer::Yes,
                    Ask::Pin { .. } => Answer::Text("0000".into()),
                    _ => Answer::No,
                };
                q.answer(a);
            }
        });
        let dev = ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB").expect("path");
        let call = |method: &str, body: &(ObjectPath<'_>, u32)| {
            bluez.call_method(
                None::<&str>,
                agent::PATH,
                Some("org.bluez.Agent1"),
                method,
                body,
            )
        };
        assert!(call("RequestConfirmation", &(dev.clone(), 123_456)).is_ok());
        let pin: String = bluez
            .call_method(
                None::<&str>,
                agent::PATH,
                Some("org.bluez.Agent1"),
                "RequestPinCode",
                &(dev.clone(),),
            )
            .and_then(|m| m.body().deserialize())
            .expect("a PIN");
        assert_eq!(pin, "0000");
        match call("RequestConfirmation", &(dev.clone(), 999_999)) {
            Err(zbus::Error::MethodError(name, _, _)) => {
                assert_eq!(name.as_str(), "org.bluez.Error.Rejected");
            }
            other => panic!("{other:?}"),
        }
    }
}
