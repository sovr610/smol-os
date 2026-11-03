// SPDX-License-Identifier: GPL-3.0-or-later
//! Network (D-081): the machine's links with their addresses and, where iwd
//! runs (TermOS images, D-072), its Wi-Fi networks.
//!
//! Joining a network writes iwd's file for it through `termos system
//! wifi-add`, the passphrase on its input, never in its arguments; iwd then
//! joins it whenever it is in range. Reading the state and scanning use
//! iwd's D-Bus interface, which the desktop's user may use (group
//! `netdev`). Everything slow happens on a thread; the page refreshes
//! itself only while it is on screen (D-028).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use secrecy::{ExposeSecret as _, SecretString};
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_hw::net::{Link, LinkState};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;

/// How often the page reads the links again while it is on screen.
const INTERVAL: Duration = Duration::from_secs(5);

/// A Wi-Fi interface and the networks it sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wifi {
    pub device: String,
    /// iwd's station state: `connected`, `disconnected`, `connecting`…
    pub state: String,
    /// Strongest first.
    pub networks: Vec<WifiNetwork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// `open`, `psk` or `8021x`.
    pub security: String,
    pub signal_dbm: i16,
    pub connected: bool,
    pub known: bool,
}

/// Where the page's information comes from, and what it changes.
pub trait Backend: Send + Sync {
    fn links(&self) -> Vec<Link>;
    /// `None` where iwd does not run.
    fn wifi(&self) -> Option<Result<Wifi, String>>;
    fn scan(&self) -> Result<(), String>;
    /// `passphrase` is `None` for an open network.
    fn join(&self, ssid: &str, passphrase: Option<&SecretString>) -> Result<(), String>;
    fn forget(&self, ssid: &str) -> Result<(), String>;
}

#[derive(Default)]
struct Shared {
    links: Vec<Link>,
    wifi: Option<Result<Wifi, String>>,
    loaded: bool,
    message: Option<String>,
}

pub struct NetworkPage {
    backend: Arc<dyn Backend>,
    shared: Arc<Mutex<Shared>>,
    waker: Arc<Mutex<Option<AppWaker>>>,
    selected: usize,
    /// The network a passphrase is being typed for, and what is typed.
    entering: Option<(String, SecretString)>,
    next: Option<Instant>,
}

impl std::fmt::Debug for NetworkPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkPage")
            .field("selected", &self.selected)
            .field("entering", &self.entering.as_ref().map(|(s, _)| s))
            .finish_non_exhaustive()
    }
}

/// Signal bars for a strength in dBm.
pub fn bars(dbm: i16) -> &'static str {
    match dbm {
        d if d >= -55 => "▂▄▆█",
        d if d >= -67 => "▂▄▆ ",
        d if d >= -75 => "▂▄  ",
        _ => "▂   ",
    }
}

impl NetworkPage {
    /// The page for this machine.
    pub fn open() -> Self {
        Self::with_backend(Arc::new(System))
    }

    pub fn with_backend(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            shared: Arc::new(Mutex::new(Shared::default())),
            waker: Arc::new(Mutex::new(None)),
            selected: 0,
            entering: None,
            next: None,
        }
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
            let (links, wifi) = (backend.links(), backend.wifi());
            if let Ok(mut s) = shared.lock() {
                s.links = links;
                s.wifi = wifi;
                s.loaded = true;
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
    }

    /// Reads everything now (tests).
    pub fn refresh_now(&self) {
        let (links, wifi) = (self.backend.links(), self.backend.wifi());
        if let Ok(mut s) = self.shared.lock() {
            s.links = links;
            s.wifi = wifi;
            s.loaded = true;
        }
    }

    /// Runs `job` on a thread; its result becomes the page's message, and
    /// the page reads everything again a little later.
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
            if let Ok(mut s) = shared.lock() {
                s.message = Some(msg);
            }
            if let Some(w) = waker.lock().ok().and_then(|w| w.clone()) {
                w.wake();
            }
        });
        self.refresh(Duration::from_secs(4));
    }

    fn say(&self, msg: String) {
        if let Ok(mut s) = self.shared.lock() {
            s.message = Some(msg);
        }
    }

    fn networks(&self) -> Vec<WifiNetwork> {
        self.shared
            .lock()
            .ok()
            .and_then(|s| match &s.wifi {
                Some(Ok(w)) => Some(w.networks.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn passphrase_key(&mut self, key: KeyEvent) -> AppEffect {
        let Some((ssid, typed)) = self.entering.take() else {
            return AppEffect::None;
        };
        match key.code {
            KeyCode::Esc => self.say("not joined".into()),
            KeyCode::Enter => {
                let len = typed.expose_secret().chars().count();
                if !(8..=63).contains(&len) {
                    self.say(if len < 8 { "too short" } else { "too long" }.into());
                    self.entering = Some((ssid, typed));
                } else {
                    let name = ssid.clone();
                    self.background(format!("joining {ssid}…"), move |b| {
                        b.join(&name, Some(&typed))
                            .map(|()| format!("saved {name}: Wi-Fi joins it when it is in range"))
                    });
                }
            }
            KeyCode::Backspace => {
                let mut s = typed.expose_secret().to_owned();
                s.pop();
                self.entering = Some((ssid, SecretString::from(s)));
            }
            KeyCode::Char(c) => {
                let s = format!("{}{c}", typed.expose_secret());
                self.entering = Some((ssid, SecretString::from(s)));
            }
            _ => self.entering = Some((ssid, typed)),
        }
        AppEffect::Redraw
    }
}

impl App for NetworkPage {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Network
    }

    fn title(&self) -> String {
        "Network".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if self.entering.is_some() {
            return self.passphrase_key(key);
        }
        let networks = self.networks();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AppEffect::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(networks.len().saturating_sub(1));
            }
            KeyCode::Char('r') => {
                self.background("scanning for Wi-Fi networks…".into(), |b| {
                    b.scan().map(|()| "scanned".to_owned())
                });
            }
            KeyCode::Enter => {
                let Some(n) = networks.get(self.selected) else {
                    return AppEffect::None;
                };
                if n.connected {
                    self.say(format!("connected to {}", n.ssid));
                } else if n.known {
                    self.say(format!(
                        "{} is saved: Wi-Fi joins it when it is in range",
                        n.ssid
                    ));
                } else if n.security == "open" {
                    let name = n.ssid.clone();
                    self.background(format!("joining {name}…"), move |b| {
                        b.join(&name, None).map(|()| format!("saved {name}"))
                    });
                } else if n.security == "psk" {
                    if let Ok(mut s) = self.shared.lock() {
                        s.message = None;
                    }
                    self.entering = Some((n.ssid.clone(), SecretString::from(String::new())));
                } else {
                    self.say(format!(
                        "{}: {} networks are not supported here yet",
                        n.ssid, n.security
                    ));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let Some(n) = networks.get(self.selected).filter(|n| n.known) else {
                    return AppEffect::None;
                };
                let name = n.ssid.clone();
                self.background(format!("forgetting {name}…"), move |b| {
                    b.forget(&name).map(|()| format!("forgot {name}"))
                });
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn handle_paste(&mut self, text: &str) -> AppEffect {
        if let Some((ssid, typed)) = self.entering.take() {
            let s = format!(
                "{}{}",
                typed.expose_secret(),
                text.trim_end_matches(['\r', '\n'])
            );
            self.entering = Some((ssid, SecretString::from(s)));
            return AppEffect::Redraw;
        }
        AppEffect::None
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 4 || area.width < 30 {
            return;
        }
        let c = &theme.colors;
        let Ok(s) = self.shared.lock() else {
            return;
        };
        let width = usize::from(area.width.saturating_sub(2));
        let mut y = area.y;
        let bottom = area.y + area.height - 2;
        let line = |buf: &mut Buffer, y: &mut u16, x: u16, text: &str, style: Style| {
            if *y < bottom {
                buf.set_stringn(
                    area.x + x,
                    *y,
                    text,
                    width.saturating_sub(usize::from(x)),
                    tui_style(style),
                );
            }
        };
        if !s.loaded {
            line(
                buf,
                &mut y,
                1,
                "Reading the network…",
                Style::new().fg(c.muted),
            );
            return;
        }
        line(
            buf,
            &mut y,
            1,
            "Connections",
            Style::new().fg(c.fg).attrs(Attrs::BOLD),
        );
        y += 1;
        if s.links.is_empty() {
            line(
                buf,
                &mut y,
                3,
                "No network interfaces",
                Style::new().fg(c.muted),
            );
            y += 1;
        }
        for l in &s.links {
            let state_style = match l.state {
                LinkState::Up => Style::new().fg(c.accent),
                LinkState::NoCarrier | LinkState::Down => Style::new().fg(c.muted),
                LinkState::Unknown => Style::new().fg(c.fg),
            };
            line(buf, &mut y, 3, &l.name, Style::new().fg(c.fg));
            line(
                buf,
                &mut y,
                13,
                if l.wifi { "Wi-Fi" } else { "wired" },
                Style::new().fg(c.muted),
            );
            line(buf, &mut y, 21, l.state.label(), state_style);
            line(
                buf,
                &mut y,
                37,
                &l.addresses.join("  "),
                Style::new().fg(c.fg),
            );
            y += 1;
        }
        y += 1;
        match &s.wifi {
            None => {
                line(
                    buf,
                    &mut y,
                    1,
                    "Wi-Fi is managed by this system's own network tools",
                    Style::new().fg(c.muted),
                );
            }
            Some(Err(e)) => {
                line(
                    buf,
                    &mut y,
                    1,
                    &format!("Wi-Fi: {e}"),
                    Style::new().fg(c.error),
                );
            }
            Some(Ok(w)) => {
                line(
                    buf,
                    &mut y,
                    1,
                    &format!("Wi-Fi ({}: {})", w.device, w.state),
                    Style::new().fg(c.fg).attrs(Attrs::BOLD),
                );
                y += 1;
                if w.networks.is_empty() {
                    line(
                        buf,
                        &mut y,
                        3,
                        "No networks seen (r scans)",
                        Style::new().fg(c.muted),
                    );
                }
                for (i, n) in w.networks.iter().enumerate() {
                    let selected = i == self.selected;
                    // Selected: everything in the text colour (D-081, the
                    // contrast review).
                    let paint = |colour| {
                        if selected {
                            c.selected(Style::new())
                        } else {
                            Style::new().fg(colour)
                        }
                    };
                    let base = paint(c.fg);
                    if selected && y < bottom {
                        buf.set_style(TuiRect::new(area.x, y, area.width, 1), tui_style(base));
                    }
                    let security = match n.security.as_str() {
                        "psk" => "WPA",
                        "open" => "open",
                        "8021x" => "802.1X",
                        other => other,
                    };
                    let status = if n.connected {
                        "connected"
                    } else if n.known {
                        "saved"
                    } else {
                        ""
                    };
                    line(buf, &mut y, 3, &n.ssid, base);
                    line(buf, &mut y, 29, security, paint(c.muted));
                    line(buf, &mut y, 37, bars(n.signal_dbm), paint(c.accent));
                    line(buf, &mut y, 43, status, paint(c.accent));
                    y += 1;
                }
            }
        }
        // The last two rows: the passphrase or a message, then the keys.
        let (msg, msg_style, keys) = match &self.entering {
            Some((ssid, typed)) => (
                format!(
                    "Passphrase for {ssid}: {}▏  {}",
                    "•".repeat(typed.expose_secret().chars().count()),
                    s.message.as_deref().unwrap_or("")
                ),
                Style::new().fg(c.fg),
                "8 to 63 characters · Enter joins · Esc cancels",
            ),
            None => (
                s.message.clone().unwrap_or_default(),
                Style::new().fg(c.muted),
                "↑↓ choose · Enter join · d forget · r scan · Esc close",
            ),
        };
        buf.set_stringn(area.x + 1, bottom, &msg, width, tui_style(msg_style));
        buf.set_stringn(
            area.x + 1,
            bottom + 1,
            keys,
            width,
            tui_style(Style::new().fg(c.muted)),
        );
    }

    fn set_waker(&mut self, waker: AppWaker) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
        self.refresh(Duration::ZERO);
        self.next = Some(Instant::now() + INTERVAL);
    }

    fn deadline(&self) -> Option<Instant> {
        self.next
    }

    fn on_timer(&mut self, now: Instant) -> AppEffect {
        self.refresh(Duration::ZERO);
        self.next = Some(now + INTERVAL);
        AppEffect::None
    }
}

/// This machine: links from sysfs and sysinfo; Wi-Fi from iwd, and
/// `termos system` to save and forget networks.
struct System;

impl Backend for System {
    fn links(&self) -> Vec<Link> {
        termos_hw::net::links()
    }

    fn wifi(&self) -> Option<Result<Wifi, String>> {
        #[cfg(target_os = "linux")]
        {
            iwd::wifi()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    fn scan(&self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            iwd::scan()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("Wi-Fi is managed by this system's own network tools".into())
        }
    }

    fn join(&self, ssid: &str, passphrase: Option<&SecretString>) -> Result<(), String> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("sudo")
            .args(["-n", "/usr/bin/termos", "system", "wifi-add", ssid])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run sudo: {e}"))?;
        if let Some(mut input) = child.stdin.take() {
            let secret = passphrase.map(|p| p.expose_secret()).unwrap_or("");
            input
                .write_all(format!("{secret}\n").as_bytes())
                .map_err(|e| format!("cannot pass the passphrase: {e}"))?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    fn forget(&self, ssid: &str) -> Result<(), String> {
        let out = std::process::Command::new("sudo")
            .args(["-n", "/usr/bin/termos", "system", "wifi-forget", ssid])
            .output()
            .map_err(|e| format!("cannot run sudo: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
}

/// iwd over D-Bus (`net.connman.iwd`).
#[cfg(target_os = "linux")]
mod iwd {
    use std::collections::HashMap;

    use zbus::blocking::Connection;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

    use super::{Wifi, WifiNetwork};

    const SERVICE: &str = "net.connman.iwd";
    type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

    fn objects(conn: &Connection) -> Result<Objects, String> {
        conn.call_method(
            Some(SERVICE),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .and_then(|m| m.body().deserialize::<Objects>())
        .map_err(|e| e.to_string())
    }

    fn text(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
        props
            .get(key)
            .and_then(|v| v.downcast_ref::<&str>().ok())
            .map(str::to_owned)
    }

    fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
        props
            .get(key)
            .and_then(|v| v.downcast_ref::<bool>().ok())
            .unwrap_or(false)
    }

    fn path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
        props
            .get(key)
            .and_then(|v| v.downcast_ref::<ObjectPath<'_>>().ok())
            .map(|p| p.to_string())
    }

    /// The system bus, if iwd is on it.
    fn connect() -> Option<Connection> {
        let conn = Connection::system().ok()?;
        let has: bool = conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "NameHasOwner",
                &(SERVICE,),
            )
            .and_then(|m| m.body().deserialize::<bool>())
            .unwrap_or(false);
        has.then_some(conn)
    }

    /// The first station (a Wi-Fi interface in station mode): its object
    /// path, interface name and state.
    fn station(objs: &Objects) -> Option<(String, String, String)> {
        objs.iter().find_map(|(p, ifaces)| {
            let st = ifaces.get("net.connman.iwd.Station")?;
            let name = ifaces
                .get("net.connman.iwd.Device")
                .and_then(|d| text(d, "Name"))
                .unwrap_or_default();
            Some((p.to_string(), name, text(st, "State").unwrap_or_default()))
        })
    }

    pub(super) fn wifi() -> Option<Result<Wifi, String>> {
        let conn = connect()?;
        Some((|| {
            let objs = objects(&conn)?;
            let Some((station_path, device, state)) = station(&objs) else {
                return Ok(Wifi::default());
            };
            let ordered: Vec<(OwnedObjectPath, i16)> = conn
                .call_method(
                    Some(SERVICE),
                    station_path.as_str(),
                    Some("net.connman.iwd.Station"),
                    "GetOrderedNetworks",
                    &(),
                )
                .and_then(|m| m.body().deserialize())
                .map_err(|e| e.to_string())?;
            let networks = ordered
                .into_iter()
                .filter_map(|(p, signal)| {
                    let n = objs.get(&p)?.get("net.connman.iwd.Network")?;
                    Some(WifiNetwork {
                        ssid: text(n, "Name")?,
                        security: text(n, "Type").unwrap_or_default(),
                        signal_dbm: signal / 100,
                        connected: flag(n, "Connected"),
                        known: path(n, "KnownNetwork").is_some(),
                    })
                })
                .collect();
            Ok(Wifi {
                device,
                state,
                networks,
            })
        })())
    }

    pub(super) fn scan() -> Result<(), String> {
        let conn = connect().ok_or("Wi-Fi (iwd) is not running")?;
        let objs = objects(&conn)?;
        let (station_path, _, _) = station(&objs).ok_or("no Wi-Fi interface")?;
        conn.call_method(
            Some(SERVICE),
            station_path.as_str(),
            Some("net.connman.iwd.Station"),
            "Scan",
            &(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::input::Mods;

    struct Fixed(Mutex<Vec<String>>);

    impl Backend for Fixed {
        fn links(&self) -> Vec<Link> {
            vec![
                Link {
                    name: "eth0".into(),
                    wifi: false,
                    state: LinkState::Up,
                    mac: "52:54:00:12:34:56".into(),
                    addresses: vec!["10.0.2.15/24".into(), "fec0::5054:ff:fe12:3456/64".into()],
                },
                Link {
                    name: "wlan0".into(),
                    wifi: true,
                    state: LinkState::NoCarrier,
                    mac: String::new(),
                    addresses: vec![],
                },
            ]
        }

        fn wifi(&self) -> Option<Result<Wifi, String>> {
            let n = |ssid: &str, security: &str, signal_dbm, connected, known| WifiNetwork {
                ssid: ssid.into(),
                security: security.into(),
                signal_dbm,
                connected,
                known,
            };
            Some(Ok(Wifi {
                device: "wlan0".into(),
                state: "disconnected".into(),
                networks: vec![
                    n("Home", "psk", -48, false, true),
                    n("Library", "open", -63, false, false),
                    n("Neighbours", "psk", -80, false, false),
                ],
            }))
        }

        fn scan(&self) -> Result<(), String> {
            Ok(())
        }

        fn join(&self, ssid: &str, passphrase: Option<&SecretString>) -> Result<(), String> {
            let p = passphrase.map_or("", |p| p.expose_secret());
            if let Ok(mut log) = self.0.lock() {
                log.push(format!("join {ssid} {}", p.len()));
            }
            Ok(())
        }

        fn forget(&self, ssid: &str) -> Result<(), String> {
            if let Ok(mut log) = self.0.lock() {
                log.push(format!("forget {ssid}"));
            }
            Ok(())
        }
    }

    fn render(page: &mut NetworkPage) -> String {
        let area = TuiRect::new(0, 0, 72, 14);
        let mut buf = Buffer::empty(area);
        page.render(area, &mut buf, &Theme::default());
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::empty())
    }

    #[test]
    fn the_page_shows_links_and_networks() {
        let backend = Arc::new(Fixed(Mutex::new(Vec::new())));
        let mut page = NetworkPage::with_backend(backend);
        assert!(render(&mut page).contains("Reading the network"));
        page.refresh_now();
        insta::assert_snapshot!("network-page", render(&mut page));
    }

    #[test]
    fn passphrases_are_masked_and_checked() {
        let backend = Arc::new(Fixed(Mutex::new(Vec::new())));
        let mut page = NetworkPage::with_backend(Arc::clone(&backend) as Arc<dyn Backend>);
        page.refresh_now();
        page.handle_key(key(KeyCode::Down));
        page.handle_key(key(KeyCode::Down));
        page.handle_key(key(KeyCode::Enter));
        for c in "hunter2".chars() {
            page.handle_key(key(KeyCode::Char(c)));
        }
        let shown = render(&mut page);
        assert!(
            shown.contains("Passphrase for Neighbours: •••••••▏"),
            "{shown}"
        );
        assert!(!shown.contains("hunter2"));
        // Too short: still asking.
        page.handle_key(key(KeyCode::Enter));
        let shown = render(&mut page);
        assert!(shown.contains("•••••••▏  too short"), "{shown}");
        page.handle_paste("!\n");
        page.handle_key(key(KeyCode::Enter));
        // The join runs on a thread.
        for _ in 0..100 {
            if !backend.0.lock().expect("log").is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*backend.0.lock().expect("log"), ["join Neighbours 8"]);
        // Saved networks are forgotten with d.
        page.handle_key(key(KeyCode::Up));
        page.handle_key(key(KeyCode::Up));
        page.handle_key(key(KeyCode::Char('d')));
        for _ in 0..100 {
            if backend.0.lock().expect("log").len() > 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(backend.0.lock().expect("log")[1], "forget Home");
    }

    #[test]
    fn signal_bars() {
        assert_eq!(bars(-40), "▂▄▆█");
        assert_eq!(bars(-70), "▂▄  ");
        assert_eq!(bars(-90), "▂   ");
    }
}
