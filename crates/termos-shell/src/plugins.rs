// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin host (DECISIONS.md D-034).
//!
//! A plugin lives in `~/.config/termos/plugins/<name>/` with a
//! `plugin.toml` manifest, and runs only while its name is listed in
//! `[plugins] enabled` (listing it is the user's consent). The server
//! launches it with `TERMOS_SOCKET`, `TERMOS_PLUGIN_NAME` and a fresh
//! `TERMOS_PLUGIN_TOKEN`; the plugin connects back, identifies itself with
//! the token, and may then do what its manifest's `[permissions]` allow.
//!
//! Plugin windows are in-process [`App`]s whose content the plugin draws
//! remotely ([`PluginWindow`]); their input goes back to the plugin as
//! notifications. A plugin that exits is not restarted (v1); the user is
//! told.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use serde::Deserialize;
use termos_core::app::{App, AppEffect, tui_style};
use termos_core::ids::PaneId;
use termos_core::input::{KeyEvent, MouseEvent, MouseKind};
use termos_plugin::proto::env;
use termos_plugin::{Line, Span};
use termos_render::cell::{Attrs, Style};
use termos_render::color::Color;
use termos_render::theme::Theme;
use tokio::io::AsyncBufReadExt as _;
use tokio::sync::{mpsc, oneshot};
use unicode_width::UnicodeWidthStr;

use crate::commands::Command;

/// What a plugin may do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Permissions {
    /// Open, draw and close windows.
    pub windows: bool,
    /// Show a status-line widget.
    pub status: bool,
    /// Add palette commands.
    pub commands: bool,
    /// Post notifications.
    pub notify: bool,
}

/// `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// The program and its arguments. A program with a `/` is relative to
    /// the plugin directory; a bare name is looked up in `PATH`.
    pub exec: Vec<String>,
    #[serde(default)]
    pub permissions: Permissions,
}

/// Plugin names: lowercase letters, digits, `-` and `_`, at most 32.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Reads and checks `<dir>/plugin.toml`; its name must be the directory's.
pub fn load_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let m: Manifest =
        toml::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e.message()))?;
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if m.name != dir_name {
        return Err(format!(
            "{}: name = \"{}\" must match the directory name \"{dir_name}\"",
            path.display(),
            m.name
        ));
    }
    if !valid_name(&m.name) {
        return Err(format!(
            "{}: invalid plugin name \"{}\" (lowercase letters, digits, - and _)",
            path.display(),
            m.name
        ));
    }
    if m.exec.first().is_none_or(String::is_empty) {
        return Err(format!("{}: exec must name a program", path.display()));
    }
    Ok(m)
}

/// The per-launch token a plugin proves its identity with. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl Token {
    /// 128 random bits from the operating system, as hex.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// Compares without stopping at the first difference.
    pub fn matches(&self, other: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), other.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

/// Styled text as the desktop draws it.
pub type Cells = Vec<(String, Style)>;

fn parse_color(c: Option<&String>) -> Color {
    c.and_then(|c| c.parse().ok()).unwrap_or(Color::Default)
}

/// Converts protocol spans to styled runs (unknown colours become the
/// default colour; control characters are dropped).
pub fn to_cells(line: &Line) -> Cells {
    line.spans()
        .iter()
        .map(|s: &Span| {
            let mut attrs = Attrs::empty();
            for (on, a) in [
                (s.bold, Attrs::BOLD),
                (s.dim, Attrs::DIM),
                (s.italic, Attrs::ITALIC),
                (s.underline, Attrs::UNDERLINE),
                (s.reverse, Attrs::REVERSE),
            ] {
                if on {
                    attrs |= a;
                }
            }
            let style = Style::new()
                .fg(parse_color(s.fg.as_ref()))
                .bg(parse_color(s.bg.as_ref()))
                .attrs(attrs);
            (s.text.chars().filter(|c| !c.is_control()).collect(), style)
        })
        .collect()
}

/// Cuts styled runs to `max` columns.
pub fn truncate_cells(cells: Cells, max: usize) -> Cells {
    let mut used = 0;
    let mut out = Vec::new();
    for (text, style) in cells {
        let mut kept = String::new();
        for ch in text.chars() {
            let w = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
            if used + w > max {
                break;
            }
            used += w;
            kept.push(ch);
        }
        let full = kept.len() == text.len();
        if !kept.is_empty() {
            out.push((kept, style));
        }
        if !full {
            break;
        }
    }
    out
}

/// What plugin windows report to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostEvent {
    Key {
        plugin: String,
        window: PaneId,
        key: KeyEvent,
    },
    Mouse {
        plugin: String,
        window: PaneId,
        kind: &'static str,
        x: u16,
        y: u16,
    },
    Resize {
        plugin: String,
        window: PaneId,
        cols: u16,
        rows: u16,
    },
    Closed {
        plugin: String,
        window: PaneId,
    },
    /// A plugin process ended.
    Exited {
        plugin: String,
        generation: u64,
        status: String,
    },
}

/// What a plugin window shows (shared between the window and the host).
#[derive(Debug, Default)]
pub(crate) struct PluginView {
    pub(crate) title: String,
    pub(crate) lines: Vec<Cells>,
}

pub(crate) type SharedView = Arc<Mutex<PluginView>>;

/// A window drawn by a plugin.
pub(crate) struct PluginWindow {
    plugin: String,
    id: PaneId,
    view: SharedView,
    events: mpsc::UnboundedSender<HostEvent>,
    size: Option<(u16, u16)>,
}

impl PluginWindow {
    pub(crate) fn new(
        plugin: &str,
        id: PaneId,
        view: SharedView,
        events: mpsc::UnboundedSender<HostEvent>,
    ) -> Self {
        Self {
            plugin: plugin.to_owned(),
            id,
            view,
            events,
            size: None,
        }
    }
}

impl Drop for PluginWindow {
    fn drop(&mut self) {
        let _ = self.events.send(HostEvent::Closed {
            plugin: self.plugin.clone(),
            window: self.id,
        });
    }
}

impl App for PluginWindow {
    fn title(&self) -> String {
        self.view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .title
            .clone()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        let _ = self.events.send(HostEvent::Key {
            plugin: self.plugin.clone(),
            window: self.id,
            key,
        });
        AppEffect::None
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        let kind = match event.kind {
            MouseKind::Down(_) => "down",
            MouseKind::Up(_) => "up",
            MouseKind::Drag(_) => "drag",
            MouseKind::ScrollUp => "scroll-up",
            MouseKind::ScrollDown => "scroll-down",
            _ => return AppEffect::None,
        };
        let _ = self.events.send(HostEvent::Mouse {
            plugin: self.plugin.clone(),
            window: self.id,
            kind,
            x: event.x,
            y: event.y,
        });
        AppEffect::None
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, _theme: &Theme) {
        let size = (area.width, area.height);
        if self.size != Some(size) {
            self.size = Some(size);
            let _ = self.events.send(HostEvent::Resize {
                plugin: self.plugin.clone(),
                window: self.id,
                cols: size.0,
                rows: size.1,
            });
        }
        let view = self.view.lock().unwrap_or_else(PoisonError::into_inner);
        for (row, line) in view.lines.iter().enumerate().take(usize::from(area.height)) {
            let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
            let mut x = area.x;
            for (text, style) in line {
                if x >= area.right() {
                    break;
                }
                let width = usize::from(area.right() - x);
                let (nx, _) = buf.set_stringn(x, y, text, width, tui_style(*style));
                x = nx;
            }
        }
    }
}

/// A running (or failed) plugin.
#[derive(Debug)]
pub(crate) struct PluginState {
    pub(crate) manifest: Manifest,
    pub(crate) token: Token,
    pub(crate) generation: u64,
    /// Stops the process (dropped: the process is killed).
    stop: Option<oneshot::Sender<()>>,
    /// The connection: its id and the lines to send.
    pub(crate) conn: Option<(u64, mpsc::UnboundedSender<String>)>,
    pub(crate) status: Option<Cells>,
    /// Registered commands: short name → description.
    pub(crate) commands: BTreeMap<String, String>,
    pub(crate) windows: BTreeMap<PaneId, SharedView>,
}

impl PluginState {
    pub(crate) fn running(&self) -> bool {
        self.stop.is_some()
    }

    /// Sends a JSON-RPC notification if connected.
    pub(crate) fn notify(&self, method: &str, params: serde_json::Value) {
        if let Some((_, tx)) = &self.conn {
            let msg = termos_core::wire::Request::notification(method, Some(params));
            let _ = tx.send(termos_core::wire::to_line(&msg));
        }
    }

    /// Forgets what the plugin showed; returns its windows (to close).
    pub(crate) fn clear_ui(&mut self) -> Vec<PaneId> {
        self.status = None;
        self.commands.clear();
        std::mem::take(&mut self.windows).into_keys().collect()
    }
}

/// A palette command offered by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    /// `<plugin>.<name>`.
    pub name: String,
    pub description: String,
    pub plugin: String,
}

/// Starts, tracks and stops plugin processes.
#[derive(Debug)]
pub(crate) struct PluginHost {
    dir: PathBuf,
    socket: Option<PathBuf>,
    pub(crate) plugins: BTreeMap<String, PluginState>,
    /// Plugins that could not start; tried again only after a reload.
    failed: BTreeSet<String>,
    next_generation: u64,
    next_conn: u64,
    events: mpsc::UnboundedSender<HostEvent>,
}

/// Resolves the manifest's program: with a `/` relative to the plugin
/// directory, else looked up in `PATH`.
fn program_path(dir: &Path, program: &str) -> PathBuf {
    if Path::new(program).components().count() > 1 {
        dir.join(program)
    } else {
        PathBuf::from(program)
    }
}

impl PluginHost {
    pub(crate) fn new(dir: PathBuf, events: mpsc::UnboundedSender<HostEvent>) -> Self {
        Self {
            dir,
            socket: None,
            plugins: BTreeMap::new(),
            failed: BTreeSet::new(),
            next_generation: 1,
            next_conn: 1,
            events,
        }
    }

    pub(crate) fn events(&self) -> mpsc::UnboundedSender<HostEvent> {
        self.events.clone()
    }

    /// Plugins can run once the server listens on `socket`.
    pub(crate) fn set_socket(&mut self, socket: PathBuf) {
        self.socket = Some(socket);
    }

    /// Starts plugins newly enabled and stops those no longer enabled.
    /// `retry` also retries plugins that failed to start (after a config
    /// reload). Returns problems to show and the windows of stopped
    /// plugins.
    pub(crate) fn sync(&mut self, enabled: &[String], retry: bool) -> (Vec<String>, Vec<PaneId>) {
        if retry {
            self.failed.clear();
        }
        let mut problems = Vec::new();
        let mut closed = Vec::new();
        let gone: Vec<String> = self
            .plugins
            .keys()
            .filter(|n| !enabled.contains(n))
            .cloned()
            .collect();
        for name in gone {
            if let Some(mut st) = self.plugins.remove(&name) {
                closed.extend(st.clear_ui());
                tracing::info!("plugin {name} stopped (disabled)");
            }
        }
        let Some(socket) = self.socket.clone() else {
            if !enabled.is_empty() && retry {
                problems.push(
                    "plugins need the session socket; they start with the server daemon".into(),
                );
            }
            return (problems, closed);
        };
        let wanted: BTreeSet<&String> = enabled.iter().collect();
        for name in wanted {
            if self.plugins.contains_key(name) || self.failed.contains(name) {
                continue;
            }
            match self.start(name, &socket) {
                Ok(st) => {
                    self.plugins.insert(name.clone(), st);
                }
                Err(e) => {
                    self.failed.insert(name.clone());
                    problems.push(format!("plugin {name} not started: {e}"));
                }
            }
        }
        (problems, closed)
    }

    fn start(&mut self, name: &str, socket: &Path) -> Result<PluginState, String> {
        if !valid_name(name) {
            return Err("invalid plugin name (lowercase letters, digits, - and _)".into());
        }
        let dir = self.dir.join(name);
        let manifest = load_manifest(&dir)?;
        let token = Token::generate().map_err(|e| format!("no random token: {e}"))?;
        let generation = self.next_generation;
        self.next_generation += 1;
        // TODO(M6): run launched plugins under the agent sandbox (bwrap + landlock).
        let mut cmd = tokio::process::Command::new(program_path(&dir, &manifest.exec[0]));
        cmd.args(&manifest.exec[1..])
            .current_dir(&dir)
            .env(env::SOCKET, socket)
            .env(env::TOKEN, token.expose())
            .env(env::NAME, name)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", manifest.exec[0]))?;
        if let Some(stderr) = child.stderr.take() {
            let plugin = name.to_owned();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!(plugin = %plugin, "{line}");
                }
            });
        }
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let events = self.events.clone();
        let plugin = name.to_owned();
        tokio::spawn(async move {
            tokio::select! {
                status = child.wait() => {
                    let status = match status {
                        Ok(s) => s.code().map_or_else(|| s.to_string(), |c| format!("status {c}")),
                        Err(e) => e.to_string(),
                    };
                    let _ = events.send(HostEvent::Exited { plugin, generation, status });
                }
                _ = stop_rx => {
                    let _ = child.kill().await;
                }
            }
        });
        tracing::info!("plugin {name} started");
        Ok(PluginState {
            manifest,
            token,
            generation,
            stop: Some(stop_tx),
            conn: None,
            status: None,
            commands: BTreeMap::new(),
            windows: BTreeMap::new(),
        })
    }

    /// The plugin a hello token belongs to, if it is not connected yet.
    pub(crate) fn connect(
        &mut self,
        token: &str,
    ) -> Option<(String, u64, mpsc::UnboundedReceiver<String>)> {
        let conn_id = self.next_conn;
        let (name, st) = self
            .plugins
            .iter_mut()
            .find(|(_, st)| st.running() && st.token.matches(token))?;
        if st.conn.is_some() {
            return None;
        }
        self.next_conn += 1;
        let (tx, rx) = mpsc::unbounded_channel();
        st.conn = Some((conn_id, tx));
        Some((name.clone(), conn_id, rx))
    }

    /// The process ended (`generation` guards against a newer launch).
    /// Returns the windows to close.
    pub(crate) fn exited(&mut self, name: &str, generation: u64) -> Option<Vec<PaneId>> {
        let st = self.plugins.get_mut(name)?;
        if st.generation != generation {
            return None;
        }
        st.stop = None;
        st.conn = None;
        Some(st.clear_ui())
    }

    /// The connection closed. Returns the windows to close.
    pub(crate) fn disconnected(&mut self, name: &str, conn_id: u64) -> Vec<PaneId> {
        match self.plugins.get_mut(name) {
            Some(st) if st.conn.as_ref().is_some_and(|(id, _)| *id == conn_id) => {
                st.conn = None;
                st.clear_ui()
            }
            _ => Vec::new(),
        }
    }

    /// Every plugin's status widget, by plugin name.
    pub(crate) fn status_widgets(&self) -> Vec<Cells> {
        self.plugins
            .values()
            .filter_map(|st| st.status.clone())
            .collect()
    }

    /// Every registered command.
    pub(crate) fn commands(&self) -> Vec<PluginCommand> {
        self.plugins
            .iter()
            .flat_map(|(plugin, st)| {
                st.commands
                    .iter()
                    .map(move |(name, description)| PluginCommand {
                        name: format!("{plugin}.{name}"),
                        description: description.clone(),
                        plugin: plugin.clone(),
                    })
            })
            .collect()
    }
}

/// A command name a plugin may register: like a plugin name, and its full
/// name must not shadow a built-in command.
pub fn valid_command(plugin: &str, name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        });
    if !ok {
        return Err(format!(
            "invalid command name \"{name}\" (lowercase letters, digits, -, _ and .)"
        ));
    }
    let full = format!("{plugin}.{name}");
    if full.parse::<Command>().is_ok() {
        return Err(format!("\"{full}\" is a built-in command"));
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, dir: &str, text: &str) -> PathBuf {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::write(d.join("plugin.toml"), text).expect("write");
        d
    }

    #[test]
    fn manifests_are_checked() {
        let root = tempfile::tempdir().expect("tempdir");
        let ok = write_manifest(
            root.path(),
            "clock",
            "name = \"clock\"\nversion = \"1.0\"\nexec = [\"./clock\", \"--big\"]\n[permissions]\nwindows = true\nstatus = true\n",
        );
        let m = load_manifest(&ok).expect("valid");
        assert_eq!(m.exec, ["./clock", "--big"]);
        assert_eq!(
            m.permissions,
            Permissions {
                windows: true,
                status: true,
                commands: false,
                notify: false
            }
        );
        let cases = [
            ("other", "name = \"clock\"\nexec = [\"x\"]\n", "must match"),
            ("empty", "name = \"empty\"\nexec = []\n", "exec"),
            (
                "typo",
                "name = \"typo\"\nexec = [\"x\"]\n[permissions]\nwindow = true\n",
                "window",
            ),
            (
                "Bad",
                "name = \"Bad\"\nexec = [\"x\"]\n",
                "invalid plugin name",
            ),
        ];
        for (dir, text, needle) in cases {
            let d = write_manifest(root.path(), dir, text);
            let err = load_manifest(&d).expect_err(dir);
            assert!(err.contains(needle), "{dir}: {err}");
        }
        assert!(load_manifest(&root.path().join("missing")).is_err());
    }

    #[test]
    fn the_sdk_speaks_the_server_protocol() {
        assert_eq!(
            termos_plugin::proto::PROTOCOL_VERSION,
            termos_core::wire::RPC_VERSION
        );
    }

    #[test]
    fn tokens_are_random_and_redacted() {
        let a = Token::generate().expect("token");
        let b = Token::generate().expect("token");
        assert_ne!(a, b);
        assert_eq!(a.expose().len(), 32);
        assert!(a.matches(a.expose()));
        assert!(!a.matches(b.expose()));
        assert!(!a.matches(""));
        assert_eq!(format!("{a:?}"), "Token(<redacted>)");
    }

    #[test]
    fn styled_text_and_names() {
        let line = Line::Styled(vec![
            Span::new("a\u{7}b").fg("red").bold(),
            Span::new("c").bg("#102030"),
            Span::new("d").fg("no-such-colour"),
        ]);
        let cells = to_cells(&line);
        assert_eq!(cells[0].0, "ab");
        assert_eq!(cells[0].1.fg, Color::Indexed(1));
        assert!(cells[0].1.attrs.contains(Attrs::BOLD));
        assert_eq!(cells[1].1.bg, Color::rgb(0x10, 0x20, 0x30));
        assert_eq!(cells[2].1.fg, Color::Default);
        let cut = truncate_cells(cells, 2);
        assert_eq!(cut.iter().map(|c| c.0.as_str()).collect::<String>(), "ab");
        assert_eq!(truncate_cells(to_cells(&Line::from("界界")), 3)[0].0, "界");

        assert!(valid_name("my-plugin_2"));
        assert!(!valid_name("My"));
        assert!(!valid_name("../x"));
        assert_eq!(valid_command("clock", "toggle"), Ok("clock.toggle".into()));
        assert!(valid_command("clock", "Toggle").is_err());
        assert!(
            valid_command("pane", "split-right").is_err(),
            "shadows a built-in"
        );
        assert_eq!(
            program_path(Path::new("/p"), "./run"),
            PathBuf::from("/p/./run")
        );
        assert_eq!(
            program_path(Path::new("/p"), "python3"),
            PathBuf::from("python3")
        );
    }
}
