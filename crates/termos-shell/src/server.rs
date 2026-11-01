// SPDX-License-Identifier: GPL-3.0-or-later
//! The TermOS server: sessions, attached clients, the session socket and
//! the JSON-RPC control API (DECISIONS.md D-029 to D-032).
//!
//! The server is event-driven. Connection tasks (socket or in-process, via
//! [`Connector`]) feed an event channel; PTY output arrives on another; the
//! loop sleeps until one of them, a config change, a signal, or the
//! earliest deadline (desktop timers, frame pacing). Each client gets the
//! cells that changed since its last frame, and only once its previous
//! frame has been written (backpressure for slow links such as SSH).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};
use termos_core::config::{Config, RestorePolicy};
use termos_core::ids::{ClientId, PaneId, PaneIds};
use termos_core::input::KeyEvent;
use termos_core::local::{self, LocalListener, LocalRead, LocalStream, LocalWrite, PeerId};
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, FrameUpdate, ImagePlacement, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::{ClientEnd, read_line};
use termos_core::wire::{
    self, ConnKind, FrameDecoder, Hello, HelloReply, Message, Request, Response, RpcError, codes,
};
use termos_plugin::proto::{self, methods};
use termos_render::caps::{Caps, ImageProtocol};
use termos_render::cell::Cell;
use termos_render::diff::{Span, diff, full};
use termos_render::encode::CursorState;
use termos_render::graphics::{DEFAULT_CELL, Picture};
use termos_render::grid::{Grid, Rect};
use termos_term::pty::PtyEvent;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::commands::{Command, CommandError};
use crate::desktop::{Desktop, DesktopOptions, SessionRequest};
use crate::images::ShownImage;
use crate::notify::Level;
use crate::plugins::{
    Cells, HostEvent, PluginHost, PluginView, PluginWindow, SharedView, to_cells, truncate_cells,
    valid_command,
};
use crate::snapshot::{self, SessionSnapshot};

/// Minimum time between frames during output floods (~120 fps cap).
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// Below this much pane output since the last frame (typing echo, prompts,
/// small updates) a frame is drawn immediately, even within the interval.
pub const SMALL_OUTPUT: usize = 4096;

/// How a server runs.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// The server's name (`termos -L NAME`); names its snapshot directory.
    pub name: String,
    pub socket_path: PathBuf,
    pub paths: Paths,
    pub config_path: PathBuf,
    /// `$SHELL` from the environment.
    pub env_shell: Option<String>,
    /// The only uid allowed to connect (normally our own).
    pub allowed_peer: PeerId,
    /// Stop when the last session ends, as tmux does.
    pub exit_when_empty: bool,
    /// Handle SIGTERM, SIGINT and SIGHUP (the daemon does; tests do not).
    pub handle_signals: bool,
    /// Extra environment for every pane (tests set a predictable prompt).
    pub pane_env: Vec<(String, String)>,
}

impl ServerOptions {
    /// Defaults for server `name`: its socket under the runtime directory,
    /// our own uid, exit when empty, no signal handling.
    pub fn new(name: &str, paths: Paths, config_path: PathBuf) -> Self {
        Self {
            name: name.to_owned(),
            socket_path: paths.socket_path(name),
            paths,
            config_path,
            // On Windows $SHELL comes from Git Bash or MSYS and names a
            // path that cannot be started; the default shell applies.
            env_shell: std::env::var("SHELL").ok().filter(|_| cfg!(unix)),
            // Nobody can connect if our own identity is unknown.
            allowed_peer: PeerId::current().unwrap_or_else(|_| PeerId::nobody()),
            exit_when_empty: true,
            handle_signals: false,
            pane_env: Vec::new(),
        }
    }
}

/// Events from connection tasks into the server loop.
#[derive(Debug)]
enum Event {
    FrontendHello {
        hello: Hello,
        reply: oneshot::Sender<Result<Attached, RpcError>>,
    },
    FrontendMsg {
        client: ClientId,
        msg: ClientMsg,
    },
    FrameWritten {
        client: ClientId,
    },
    Disconnected {
        client: ClientId,
    },
    Control {
        req: Request,
        reply: oneshot::Sender<Option<Response>>,
    },
    PluginHello {
        token: String,
        reply: oneshot::Sender<Result<PluginConn, RpcError>>,
    },
    PluginRequest {
        plugin: String,
        req: Request,
        reply: oneshot::Sender<Option<Response>>,
    },
    PluginGone {
        plugin: String,
        conn: u64,
    },
}

/// An admitted plugin connection: its name, connection id, and the lines
/// the server sends it.
type PluginConn = (String, u64, mpsc::UnboundedReceiver<String>);

/// A frontend admitted by the server.
#[derive(Debug)]
struct Attached {
    client: ClientId,
    session: String,
    rx: mpsc::UnboundedReceiver<ServerMsg>,
}

type SessionKey = u64;

/// Server-side state of one attached frontend.
struct Conn {
    session: SessionKey,
    tx: mpsc::UnboundedSender<ServerMsg>,
    caps: Caps,
    cols: u16,
    rows: u16,
    /// The client sent its binary `Hello` (size and capabilities).
    ready: bool,
    /// A frame was sent and not yet written to the client.
    busy: bool,
    frame_due: Option<Instant>,
    last_frame: Option<Instant>,
    cur: Grid,
    prev: Option<Grid>,
    sent_size: (u16, u16),
    last_cursor: Option<CursorState>,
    images: ClientImages,
}

/// The pixel images a client has and shows.
#[derive(Debug, Default)]
struct ClientImages {
    /// Images uploaded to the client: (image key, width, height in pixels)
    /// → the id it knows them by.
    sent: HashMap<(u64, u32, u32), u32>,
    next_id: u32,
    /// What the client shows after its last frame.
    placed: Vec<ImagePlacement>,
}

/// Images uploaded but not shown that a client may keep (so switching
/// back to them costs nothing); beyond this they are freed.
const SPARE_IMAGES: usize = 8;

/// Largest piece of image data in one message.
const IMAGE_CHUNK: usize = 1024 * 1024;

/// Encoded image variants (PNG at one size), shared by all clients.
#[derive(Debug, Default)]
struct VariantCache {
    map: HashMap<(u64, u32, u32), Arc<Vec<u8>>>,
    order: VecDeque<(u64, u32, u32)>,
}

const VARIANT_CACHE: usize = 32;

impl VariantCache {
    fn png(&mut self, key: u64, picture: &Picture, w: u32, h: u32) -> Arc<Vec<u8>> {
        let k = (key, w, h);
        if let Some(png) = self.map.get(&k) {
            return Arc::clone(png);
        }
        let png = Arc::new(picture.resize(w, h).to_png());
        self.map.insert(k, Arc::clone(&png));
        self.order.push_back(k);
        while self.order.len() > VARIANT_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        png
    }
}

/// Works out the image part of `conn`'s next frame: uploads what it does
/// not have yet, frees what it no longer needs, and returns the new
/// placement list when it changed (or `force`). Sixel and iTerm2 images
/// that moved or went away left pixels behind: their cells are poisoned
/// in `conn.prev`, so the next diff paints over them.
fn update_images(
    conn: &mut Conn,
    shown: Vec<ShownImage>,
    cache: &mut VariantCache,
    visible: (u16, u16),
    force: bool,
) -> Option<Vec<ImagePlacement>> {
    let cell = conn.caps.cell_px.unwrap_or(DEFAULT_CELL);
    let screen = Rect::new(0, 0, visible.0, visible.1);
    let mut placements = Vec::new();
    let mut live = HashSet::new();
    for img in shown {
        let (w, h) = (
            u32::from(img.place.cols) * cell.width,
            u32::from(img.place.rows) * cell.height,
        );
        let id = match conn.images.sent.get(&(img.key, w, h)) {
            Some(id) => *id,
            None => {
                let png = cache.png(img.key, &img.picture, w, h);
                conn.images.next_id = conn.images.next_id.wrapping_add(1).max(1);
                let id = conn.images.next_id;
                let chunks: Vec<&[u8]> = png.chunks(IMAGE_CHUNK).collect();
                let n = chunks.len();
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let _ = conn.tx.send(ServerMsg::ImageData {
                        id,
                        chunk: chunk.to_vec(),
                        last: i + 1 == n,
                    });
                }
                conn.images.sent.insert((img.key, w, h), id);
                id
            }
        };
        live.insert(id);
        match img.protocol {
            ImageProtocol::Kitty => {
                for piece in &img.pieces {
                    let p = piece.intersect(&screen);
                    if p.is_empty() {
                        continue;
                    }
                    placements.push(ImagePlacement {
                        image: id,
                        protocol: img.protocol,
                        x: p.x,
                        y: p.y,
                        cols: p.w,
                        rows: p.h,
                        src: [
                            // Non-negative: pieces lie within the image.
                            (i32::from(p.x) - img.place.x).unsigned_abs() * cell.width,
                            (i32::from(p.y) - img.place.y).unsigned_abs() * cell.height,
                            u32::from(p.w) * cell.width,
                            u32::from(p.h) * cell.height,
                        ],
                    });
                }
            }
            // Whole images only; one cut off by a smaller terminal is left
            // out.
            _ => {
                if let Some(rect) = img.place.rect().filter(|r| r.intersect(&screen) == *r) {
                    placements.push(ImagePlacement {
                        image: id,
                        protocol: img.protocol,
                        x: rect.x,
                        y: rect.y,
                        cols: rect.w,
                        rows: rect.h,
                        src: [0, 0, w, h],
                    });
                }
            }
        }
    }
    if conn.images.sent.len() > live.len() + SPARE_IMAGES {
        let tx = &conn.tx;
        conn.images.sent.retain(|_, id| {
            let keep = live.contains(id);
            if !keep {
                let _ = tx.send(ServerMsg::ImageFree { id: *id });
            }
            keep
        });
    }
    if placements == conn.images.placed && !force {
        return None;
    }
    if let Some(prev) = conn.prev.as_mut() {
        for old in &conn.images.placed {
            if old.protocol != ImageProtocol::Kitty && !placements.contains(old) {
                prev.poison(Rect::new(old.x, old.y, old.cols, old.rows));
            }
        }
    }
    conn.images.placed.clone_from(&placements);
    Some(placements)
}

impl Conn {
    fn schedule(&mut self, due: Instant) {
        self.frame_due = Some(self.frame_due.map_or(due, |d| d.min(due)));
    }

    /// The next frame is a full redraw, as soon as possible.
    fn redraw(&mut self) {
        self.prev = None;
        self.schedule(Instant::now());
    }
}

struct SessionEntry {
    desktop: Desktop,
    created: SystemTime,
    last_used: Instant,
    /// When the debounced snapshot is due (D-033).
    snapshot_due: Option<Instant>,
    /// The session name of the snapshot file last written.
    saved_as: Option<String>,
}

/// The startup restore question (`restore = "ask"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Nothing to ask (or already answered).
    No,
    /// Ask the first client that attaches.
    Pending,
    /// Asked in this session; its own snapshot waits for the answer.
    Asked(SessionKey),
}

fn stopping() -> RpcError {
    RpcError::new(codes::INTERNAL_ERROR, "the server is stopping")
}

fn hello_reply(kind: ConnKind, session: Option<String>) -> HelloReply {
    HelloReply {
        version: kind.version(),
        server: format!("termos {}", env!("CARGO_PKG_VERSION")),
        pid: std::process::id(),
        session,
    }
}

/// Lets connection tasks, tests and embedders talk to a running server.
#[derive(Debug, Clone)]
pub struct Connector {
    events: mpsc::UnboundedSender<Event>,
}

impl Connector {
    async fn attach_raw(&self, hello: Hello) -> Result<Attached, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.events
            .send(Event::FrontendHello { hello, reply: tx })
            .map_err(|_| stopping())?;
        rx.await.map_err(|_| stopping())?
    }

    /// Attaches a frontend without a socket (tests, embedding). The
    /// returned [`ClientEnd`] behaves like a socket connection, including
    /// frame backpressure.
    pub async fn attach(&self, hello: Hello) -> Result<(HelloReply, ClientEnd), RpcError> {
        let att = self.attach_raw(hello).await?;
        let client = att.client;
        let reply = hello_reply(ConnKind::Frontend, Some(att.session));
        let (user_tx, mut user_rx) = mpsc::unbounded_channel::<ClientMsg>();
        let (to_user_tx, to_user_rx) = mpsc::unbounded_channel::<ServerMsg>();
        let events = self.events.clone();
        tokio::spawn(async move {
            while let Some(msg) = user_rx.recv().await {
                if events.send(Event::FrontendMsg { client, msg }).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::Disconnected { client });
        });
        let events = self.events.clone();
        let mut server_rx = att.rx;
        tokio::spawn(async move {
            while let Some(msg) = server_rx.recv().await {
                let is_frame = matches!(msg, ServerMsg::Frame(_));
                if to_user_tx.send(msg).is_err() {
                    break;
                }
                if is_frame {
                    let _ = events.send(Event::FrameWritten { client });
                }
            }
        });
        Ok((
            reply,
            ClientEnd {
                tx: user_tx,
                rx: to_user_rx,
            },
        ))
    }

    /// Handles one JSON-RPC request; `None` for notifications.
    pub async fn control(&self, req: Request) -> Option<Response> {
        let id = req.id.clone();
        let (tx, rx) = oneshot::channel();
        if self.events.send(Event::Control { req, reply: tx }).is_err() {
            return id.map(|id| Response::err(id, stopping()));
        }
        rx.await.ok().flatten()
    }

    /// Calls a control method and returns its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match self.control(Request::new(1, method, Some(params))).await {
            Some(resp) => resp.into_result(),
            None => Err(stopping()),
        }
    }

    async fn plugin_hello(&self, token: String) -> Result<PluginConn, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.events
            .send(Event::PluginHello { token, reply: tx })
            .map_err(|_| stopping())?;
        rx.await.map_err(|_| stopping())?
    }

    async fn plugin_request(&self, plugin: &str, req: Request) -> Option<Response> {
        let id = req.id.clone();
        let (tx, rx) = oneshot::channel();
        let ev = Event::PluginRequest {
            plugin: plugin.to_owned(),
            req,
            reply: tx,
        };
        if self.events.send(ev).is_err() {
            return id.map(|id| Response::err(id, stopping()));
        }
        rx.await.ok().flatten()
    }
}

/// The session server.
pub struct Server {
    opts: ServerOptions,
    config: Config,
    sessions: BTreeMap<SessionKey, SessionEntry>,
    next_session: SessionKey,
    clients: BTreeMap<ClientId, Conn>,
    next_client: u64,
    pane_ids: PaneIds,
    pty_tx: mpsc::Sender<PtyEvent>,
    pty_rx: mpsc::Receiver<PtyEvent>,
    events_tx: mpsc::UnboundedSender<Event>,
    events_rx: mpsc::UnboundedReceiver<Event>,
    reload_rx: Option<mpsc::UnboundedReceiver<()>>,
    plugins: PluginHost,
    host_rx: mpsc::UnboundedReceiver<HostEvent>,
    /// Notices for the next session created (startup problems).
    notices: Vec<(Level, String)>,
    /// Saved sessions not restored (yet).
    saved: Vec<SessionSnapshot>,
    ask: Ask,
    shutdown: Option<String>,
    started: SystemTime,
    known_names: Vec<String>,
    variants: VariantCache,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.opts.name)
            .field("sessions", &self.sessions.len())
            .field("clients", &self.clients.len())
            .finish_non_exhaustive()
    }
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Session names: 1 to 64 characters, no `/`, `:` or control characters,
/// not starting with `$` (reserved for ids such as `$1`).
pub fn valid_session_name(name: &str) -> Result<(), RpcError> {
    let bad = name.trim().is_empty()
        || name.chars().count() > 64
        || name.starts_with('$')
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | ':'));
    if bad {
        Err(RpcError::invalid_params(format!(
            "invalid session name `{name}` (1 to 64 characters, no `/`, `:` or control characters, not starting with `$`)"
        )))
    } else {
        Ok(())
    }
}

/// Keeps a frame within a client's visible size.
fn crop(spans: Vec<Span>, cols: u16, rows: u16) -> Vec<Span> {
    spans
        .into_iter()
        .filter(|s| s.row < rows && s.col < cols)
        .map(|mut s| {
            let max = usize::from(cols - s.col);
            if s.cells.len() > max {
                s.cells.truncate(max);
                // Half of a wide character cannot be shown.
                if let Some(last) = s.cells.last_mut()
                    && last.width == 2
                {
                    *last = Cell::blank(last.style);
                }
            }
            s
        })
        .collect()
}

#[derive(Deserialize)]
struct TargetParams {
    #[serde(default)]
    session: Option<String>,
}

impl Server {
    pub fn new(opts: ServerOptions, config: Config) -> Self {
        let (pty_tx, pty_rx) = mpsc::channel(1024);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (host_tx, host_rx) = mpsc::unbounded_channel();
        let plugins = PluginHost::new(opts.paths.plugins_dir(), host_tx);
        Self {
            opts,
            config,
            sessions: BTreeMap::new(),
            next_session: 1,
            clients: BTreeMap::new(),
            next_client: 1,
            pane_ids: PaneIds::new(),
            pty_tx,
            pty_rx,
            events_tx,
            events_rx,
            reload_rx: None,
            plugins,
            host_rx,
            notices: Vec::new(),
            saved: Vec::new(),
            ask: Ask::No,
            shutdown: None,
            started: SystemTime::now(),
            known_names: Vec::new(),
            variants: VariantCache::default(),
        }
    }

    /// Delivers config-file change notifications (hot reload).
    pub fn set_reload(&mut self, rx: mpsc::UnboundedReceiver<()>) {
        self.reload_rx = Some(rx);
    }

    /// Shows `text` in the next session created (startup problems such as
    /// a broken config file).
    pub fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.notices.push((level, text.into()));
    }

    pub fn options(&self) -> &ServerOptions {
        &self.opts
    }

    pub fn connector(&self) -> Connector {
        Connector {
            events: self.events_tx.clone(),
        }
    }

    /// Binds the session socket (mode 0600 in a 0700 directory), replacing a
    /// stale socket file but refusing to start beside a live server.
    pub fn bind(&self) -> io::Result<LocalListener> {
        LocalListener::bind(&self.opts.socket_path)
    }

    // ----- sessions ---------------------------------------------------

    fn find_session(&self, name: &str) -> Option<SessionKey> {
        if let Some(id) = name.strip_prefix('$').and_then(|n| n.parse::<u64>().ok()) {
            return self.sessions.contains_key(&id).then_some(id);
        }
        self.sessions
            .iter()
            .find(|(_, e)| e.desktop.name() == name)
            .map(|(k, _)| *k)
    }

    fn most_recent(&self) -> Option<SessionKey> {
        self.sessions
            .iter()
            .max_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| *k)
    }

    fn target(&self, name: Option<&str>) -> Result<SessionKey, RpcError> {
        match name {
            Some(n) => self
                .find_session(n)
                .ok_or_else(|| RpcError::not_found(format!("no session named `{n}`"))),
            None => self
                .most_recent()
                .ok_or_else(|| RpcError::not_found("there are no sessions")),
        }
    }

    fn free_name(&self) -> String {
        if self.find_session("main").is_none() {
            return "main".into();
        }
        (1..=u32::MAX)
            .map(|n| n.to_string())
            .find(|n| self.find_session(n).is_none())
            .unwrap_or_else(|| "session".into())
    }

    fn session_name(&self, key: SessionKey) -> String {
        self.sessions
            .get(&key)
            .map(|e| e.desktop.name().to_owned())
            .unwrap_or_default()
    }

    /// Creates a session with one shell. Without a name it is `main`, or
    /// else the first free number.
    pub fn create_session(
        &mut self,
        name: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<SessionKey, RpcError> {
        let name = match name {
            Some(n) => {
                valid_session_name(n)?;
                if self.find_session(n).is_some() {
                    return Err(RpcError::invalid_params(format!(
                        "a session named `{n}` already exists"
                    )));
                }
                n.to_owned()
            }
            None => self.free_name(),
        };
        let key = self.next_session;
        let mut opts = self.desktop_options(key, name);
        opts.cwd = cwd.map(PathBuf::from).filter(|c| c.is_dir());
        let desktop = Desktop::new(opts, self.config.clone(), self.pty_tx.clone(), 80, 24)
            .map_err(|e| {
                RpcError::new(codes::INTERNAL_ERROR, format!("cannot start a shell: {e}"))
            })?;
        self.next_session += 1;
        self.insert_session(key, desktop);
        Ok(key)
    }

    fn desktop_options(&self, key: SessionKey, name: String) -> DesktopOptions {
        DesktopOptions {
            cwd: None,
            session_name: name,
            config_path: self.opts.config_path.clone(),
            paths: self.opts.paths.clone(),
            env_shell: self.opts.env_shell.clone(),
            pane_ids: self.pane_ids.clone(),
            // Lets `termos msg`, `ls` and `detach` inside a pane find us
            // (D-029; supersedes D-027's pid-only value).
            pane_env: std::iter::once((
                "TERMOS".into(),
                format!(
                    "{},{},${key}",
                    self.opts.socket_path.display(),
                    std::process::id()
                ),
            ))
            .chain(self.opts.pane_env.iter().cloned())
            .collect(),
        }
    }

    fn insert_session(&mut self, key: SessionKey, mut desktop: Desktop) {
        for (level, text) in self.notices.drain(..) {
            desktop.post(level, text);
        }
        desktop.set_plugin_ui(self.plugins.status_widgets(), self.plugins.commands());
        self.sessions.insert(
            key,
            SessionEntry {
                desktop,
                created: SystemTime::now(),
                last_used: Instant::now(),
                snapshot_due: None,
                saved_as: None,
            },
        );
        self.sync_plugins(false);
    }

    // ----- snapshots and restore (D-033) -------------------------------

    fn snapshot_dir(&self) -> PathBuf {
        self.opts.paths.sessions_dir(&self.opts.name)
    }

    /// Reads the snapshots an earlier server left and applies the restore
    /// policy.
    fn load_saved(&mut self) {
        let (snaps, errors) = snapshot::load_all(&self.snapshot_dir());
        for e in errors {
            tracing::warn!("{e}");
            self.notices.push((Level::Warn, e));
        }
        self.saved = snaps;
        if self.saved.is_empty() {
            return;
        }
        match self.config.session.restore {
            RestorePolicy::Always => {
                let (_, failed) = self.restore_saved(None, None);
                self.report_failed(None, &failed);
            }
            RestorePolicy::Ask => self.ask = Ask::Pending,
            RestorePolicy::Never => {}
        }
    }

    /// "Restore session main saved at 14:02".
    fn restore_question(&self) -> String {
        let newest = self.saved.iter().map(|s| s.saved_at).max().unwrap_or(0);
        let when = snapshot::describe_time(newest);
        match self.saved.as_slice() {
            [one] => format!("Restore session {} saved at {when}", one.name),
            many => {
                let names: Vec<&str> = many.iter().map(|s| s.name.as_str()).collect();
                format!(
                    "Restore {} saved sessions ({}) from {when}",
                    many.len(),
                    names.join(", ")
                )
            }
        }
    }

    /// A name for a restored session: its own if free, else `name-2`, ….
    fn restored_name(&self, name: &str) -> String {
        if valid_session_name(name).is_err() {
            return self.free_name();
        }
        if self.find_session(name).is_none() {
            return name.to_owned();
        }
        (2..=u32::MAX)
            .map(|n| format!("{name}-{n}"))
            .find(|n| self.find_session(n).is_none())
            .unwrap_or_else(|| self.free_name())
    }

    /// Restores the saved sessions (all, or those in `names`). A snapshot
    /// named like the `in_place` session replaces it (the session that
    /// asked the startup question); other name clashes get a new name.
    /// Returns the restored names and the failures.
    fn restore_saved(
        &mut self,
        in_place: Option<SessionKey>,
        names: Option<&[String]>,
    ) -> (Vec<String>, Vec<String>) {
        let (todo, keep): (Vec<SessionSnapshot>, Vec<SessionSnapshot>) =
            std::mem::take(&mut self.saved)
                .into_iter()
                .partition(|s| names.is_none_or(|n| n.contains(&s.name)));
        self.saved = keep;
        let mut restored = Vec::new();
        let mut failed = Vec::new();
        for snap in todo {
            let replace = in_place.filter(|k| {
                self.sessions
                    .get(k)
                    .is_some_and(|e| e.desktop.name() == snap.name)
            });
            let (key, name) = match replace {
                Some(k) => (k, snap.name.clone()),
                None => (self.next_session, self.restored_name(&snap.name)),
            };
            let opts = self.desktop_options(key, name.clone());
            match Desktop::from_snapshot(opts, self.config.clone(), self.pty_tx.clone(), &snap) {
                Ok((mut desktop, skipped)) => {
                    desktop.post(
                        Level::Info,
                        format!(
                            "restored session {name} (saved at {})",
                            snapshot::describe_time(snap.saved_at)
                        ),
                    );
                    if !skipped.is_empty() {
                        desktop.post(
                            Level::Warn,
                            format!("not restarted: {}", skipped.join(", ")),
                        );
                    }
                    match replace {
                        Some(k) => self.replace_session(k, desktop),
                        None => {
                            self.next_session += 1;
                            self.insert_session(key, desktop);
                        }
                    }
                    tracing::info!("restored session {name}");
                    restored.push(name);
                }
                Err(e) => {
                    tracing::warn!("cannot restore session {}: {e}", snap.name);
                    failed.push(format!("{}: {e}", snap.name));
                }
            }
        }
        (restored, failed)
    }

    fn report_failed(&mut self, key: Option<SessionKey>, failed: &[String]) {
        if failed.is_empty() {
            return;
        }
        let text = format!("could not restore {}", failed.join("; "));
        match key.and_then(|k| self.sessions.get_mut(&k)) {
            Some(e) => e.desktop.post(Level::Error, text),
            None => self.notices.push((Level::Error, text)),
        }
    }

    /// Swaps a session's desktop for `desktop`; attached clients stay and
    /// see the new one.
    fn replace_session(&mut self, key: SessionKey, desktop: Desktop) {
        let mut hellos = Vec::new();
        for (id, conn) in self.clients.iter_mut().filter(|(_, c)| c.session == key) {
            conn.redraw();
            if conn.ready {
                hellos.push((
                    *id,
                    ClientMsg::Hello {
                        version: PROTOCOL_VERSION,
                        caps: conn.caps.clone(),
                        cols: conn.cols,
                        rows: conn.rows,
                    },
                ));
            }
        }
        let (status, commands) = (self.plugins.status_widgets(), self.plugins.commands());
        if let Some(e) = self.sessions.get_mut(&key) {
            let mut old = std::mem::replace(&mut e.desktop, desktop);
            old.end("replaced by its saved state");
            // What the server told the old desktop, the new one needs too.
            e.desktop.set_plugin_ui(status, commands);
            e.desktop.set_known_sessions(self.known_names.clone());
            e.last_used = Instant::now();
            for (id, hello) in hellos {
                e.desktop.handle_client(id, hello);
            }
        }
    }

    /// Writes a session's snapshot now.
    fn save_snapshot(&mut self, key: SessionKey) {
        let dir = self.snapshot_dir();
        let Some(e) = self.sessions.get_mut(&key) else {
            return;
        };
        e.snapshot_due = None;
        let snap = e.desktop.snapshot();
        if let Some(old) = e.saved_as.as_ref().filter(|o| **o != snap.name)
            && let Err(err) = snapshot::remove(&dir, old)
        {
            tracing::warn!("cannot remove the old snapshot of {old}: {err}");
        }
        match snapshot::save(&dir, &snap) {
            Ok(_) => e.saved_as = Some(snap.name),
            Err(err) => tracing::warn!("cannot save session {}: {err}", snap.name),
        }
    }

    /// Debounces snapshots after structural changes and writes those due.
    fn snapshots(&mut self) {
        let now = Instant::now();
        let delay = Duration::from_millis(self.config.session.snapshot_delay_ms);
        for e in self.sessions.values_mut() {
            if e.desktop.take_layout_changed() {
                e.snapshot_due.get_or_insert(now + delay);
            }
        }
        let due: Vec<SessionKey> = self
            .sessions
            .iter()
            .filter(|(k, e)| {
                self.ask != Ask::Asked(**k) && e.snapshot_due.is_some_and(|d| d <= now)
            })
            .map(|(k, _)| *k)
            .collect();
        for k in due {
            self.save_snapshot(k);
        }
    }

    fn resolve_frontend(&mut self, hello: &Hello) -> Result<SessionKey, RpcError> {
        match &hello.session {
            Some(name) => match self.find_session(name) {
                Some(k) => Ok(k),
                None if hello.create => self.create_session(Some(name), hello.cwd.as_deref()),
                None => Err(RpcError::not_found(format!("no session named `{name}`"))),
            },
            None => match self.most_recent() {
                Some(k) => Ok(k),
                None if hello.create => self.create_session(None, hello.cwd.as_deref()),
                None => Err(RpcError::not_found(
                    "there are no sessions; start one with `termos`",
                )),
            },
        }
    }

    /// Moves a client to another session.
    fn move_client(&mut self, client: ClientId, to: SessionKey) {
        let Some(conn) = self.clients.get_mut(&client) else {
            return;
        };
        let from = conn.session;
        conn.session = to;
        conn.redraw();
        let hello = conn.ready.then(|| ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            caps: conn.caps.clone(),
            cols: conn.cols,
            rows: conn.rows,
        });
        if from != to
            && let Some(e) = self.sessions.get_mut(&from)
        {
            e.desktop.detach_client(client);
        }
        if let Some(e) = self.sessions.get_mut(&to) {
            e.last_used = Instant::now();
            if let Some(h) = hello {
                e.desktop.handle_client(client, h);
            }
        }
    }

    fn remove_client(&mut self, client: ClientId) {
        let Some(conn) = self.clients.remove(&client) else {
            return;
        };
        if let Some(e) = self.sessions.get_mut(&conn.session) {
            e.desktop.detach_client(client);
        }
        // The restore question left with its last viewer: ask the next
        // client instead, and let the session be saved meanwhile.
        if self.ask == Ask::Asked(conn.session) && self.clients_of(conn.session).is_empty() {
            self.ask = Ask::Pending;
        }
    }

    /// Sends a message; `Detached` and `Exit` also drop the client.
    fn send(&mut self, client: ClientId, msg: ServerMsg) {
        let ends = matches!(msg, ServerMsg::Detached { .. } | ServerMsg::Exit { .. });
        if let Some(conn) = self.clients.get(&client) {
            let _ = conn.tx.send(msg);
        }
        if ends {
            self.remove_client(client);
        }
    }

    fn clients_of(&self, key: SessionKey) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter(|(_, c)| c.session == key)
            .map(|(id, _)| *id)
            .collect()
    }

    // ----- the loop ---------------------------------------------------

    /// Runs until stopped (`server.kill`, a signal, or the last session
    /// ending when `exit_when_empty`). Serves `listener` if given and
    /// removes the socket file on the way out. Returns why it stopped.
    pub async fn run(mut self, listener: Option<LocalListener>) -> String {
        let owns_socket = listener.is_some();
        let accept = listener.map(|mut l| {
            let connector = self.connector();
            let allowed = self.opts.allowed_peer.clone();
            tokio::spawn(async move {
                loop {
                    match l.accept().await {
                        Ok(stream) => {
                            tokio::spawn(handle_conn(stream, connector.clone(), allowed.clone()));
                        }
                        Err(e) => {
                            // Out of descriptors and the like: back off
                            // instead of spinning.
                            tracing::warn!("accept failed: {e}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            })
        });
        if owns_socket {
            self.plugins.set_socket(self.opts.socket_path.clone());
        }
        self.load_saved();
        let mut signals = if self.opts.handle_signals {
            Signals::new()
        } else {
            None
        };
        let reason = loop {
            self.pump();
            if let Some(r) = self.shutdown.take() {
                break r;
            }
            let timer = self.next_deadline();
            tokio::select! {
                Some(ev) = self.events_rx.recv() => self.on_event(ev),
                Some(ev) = self.pty_rx.recv() => {
                    self.on_pty(ev);
                    // Drain what is already queued so a burst becomes one
                    // frame.
                    while let Ok(ev) = self.pty_rx.try_recv() {
                        self.on_pty(ev);
                    }
                }
                Some(ev) = self.host_rx.recv() => self.on_host(ev),
                r = recv_reload(&mut self.reload_rx) => match r {
                    Some(()) => self.reload_config(),
                    None => self.reload_rx = None,
                },
                sig = recv_signal(&mut signals) => {
                    self.shutdown = Some(format!("stopped by {sig}"));
                }
                () = sleep_until(timer) => {
                    let now = Instant::now();
                    for e in self.sessions.values_mut() {
                        e.desktop.on_timer(now);
                    }
                }
            }
        };
        tracing::info!("server stopping: {reason}");
        // Sessions still running survive in their snapshots, except one
        // whose restore question is unanswered: its old snapshot stays.
        let keys: Vec<SessionKey> = self.sessions.keys().copied().collect();
        for k in keys {
            if self.ask != Ask::Asked(k) {
                self.save_snapshot(k);
            }
        }
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for c in ids {
            self.send(
                c,
                ServerMsg::Exit {
                    reason: format!("the server stopped ({reason})"),
                },
            );
        }
        for e in self.sessions.values_mut() {
            e.desktop.end(&reason);
        }
        if let Some(a) = accept {
            a.abort();
        }
        if owns_socket {
            local::remove_endpoint(&self.opts.socket_path);
        }
        // Let connection tasks flush the last messages.
        tokio::time::sleep(Duration::from_millis(50)).await;
        reason
    }

    fn reload_config(&mut self) {
        if let Ok(c) = Config::load(&self.opts.config_path) {
            self.config = c;
        }
        for e in self.sessions.values_mut() {
            e.desktop.reload_config();
        }
        self.sync_plugins(true);
    }

    // ----- plugins (D-034) ------------------------------------------------

    /// Shows `text` in every session (or the next one created).
    fn post_everywhere(&mut self, level: Level, text: &str) {
        if self.sessions.is_empty() {
            self.notices.push((level, text.to_owned()));
        }
        for e in self.sessions.values_mut() {
            e.desktop.post(level, text);
        }
    }

    /// Starts and stops plugins to match `[plugins] enabled`. Plugins start
    /// with the first session, so they have somewhere to open windows.
    fn sync_plugins(&mut self, retry: bool) {
        if self.sessions.is_empty() {
            return;
        }
        let (problems, closed) = self.plugins.sync(&self.config.plugins.enabled, retry);
        self.close_windows(&closed);
        for p in problems {
            tracing::warn!("{p}");
            self.post_everywhere(Level::Warn, &p);
        }
        self.push_plugin_ui();
    }

    /// Gives every desktop the current plugin widgets and commands.
    fn push_plugin_ui(&mut self) {
        let status = self.plugins.status_widgets();
        let commands = self.plugins.commands();
        for e in self.sessions.values_mut() {
            e.desktop.set_plugin_ui(status.clone(), commands.clone());
        }
    }

    fn close_windows(&mut self, ids: &[PaneId]) {
        for id in ids {
            if let Some(e) = self.sessions.values_mut().find(|e| e.desktop.has_pane(*id)) {
                e.desktop.kill_pane(*id);
            }
        }
    }

    fn on_host(&mut self, ev: HostEvent) {
        let (plugin, method, params) = match ev {
            HostEvent::Key {
                plugin,
                window,
                key,
            } => (
                plugin,
                methods::WINDOW_KEY,
                json!({"window": window.0, "key": key.to_string()}),
            ),
            HostEvent::Mouse {
                plugin,
                window,
                kind,
                x,
                y,
            } => (
                plugin,
                methods::WINDOW_MOUSE,
                json!({"window": window.0, "kind": kind, "x": x, "y": y}),
            ),
            HostEvent::Resize {
                plugin,
                window,
                cols,
                rows,
            } => (
                plugin,
                methods::WINDOW_RESIZE,
                json!({"window": window.0, "cols": cols, "rows": rows}),
            ),
            HostEvent::Closed { plugin, window } => {
                // Only windows the plugin still knows about (not ones it
                // closed itself, nor those of a plugin that went away).
                let known = self
                    .plugins
                    .plugins
                    .get_mut(&plugin)
                    .is_some_and(|st| st.windows.remove(&window).is_some());
                if !known {
                    return;
                }
                (plugin, methods::WINDOW_CLOSED, json!({"window": window.0}))
            }
            HostEvent::Exited {
                plugin,
                generation,
                status,
            } => {
                if let Some(closed) = self.plugins.exited(&plugin, generation) {
                    tracing::warn!("plugin {plugin} exited ({status})");
                    self.close_windows(&closed);
                    self.push_plugin_ui();
                    self.post_everywhere(
                        Level::Warn,
                        &format!("plugin {plugin} exited ({status})"),
                    );
                }
                return;
            }
        };
        if let Some(st) = self.plugins.plugins.get(&plugin) {
            // Input for a window the plugin no longer has is dropped.
            let known = params["window"]
                .as_u64()
                .is_some_and(|w| st.windows.contains_key(&PaneId(w)))
                || method == methods::WINDOW_CLOSED;
            if known {
                st.notify(method, params);
            }
        }
    }

    /// Runs a plugin command (`<plugin>.<name>`) on behalf of `session`.
    fn invoke_plugin_command(&mut self, full: &str, session: Option<String>) -> bool {
        let Some((plugin, name)) = full.split_once('.') else {
            return false;
        };
        match self.plugins.plugins.get(plugin) {
            Some(st) if st.commands.contains_key(name) => {
                st.notify(
                    methods::COMMAND_INVOKED,
                    json!({"name": name, "session": session}),
                );
                true
            }
            _ => false,
        }
    }

    /// One request from a connected plugin, checked against its manifest.
    fn plugin_request(&mut self, plugin: &str, req: &Request) -> Result<Value, RpcError> {
        /// Bounds what one plugin can make the server hold.
        const MAX_WINDOWS: usize = 16;
        const MAX_LINES: usize = 1000;
        const MAX_LINE_WIDTH: usize = 1000;
        let perms = self
            .plugins
            .plugins
            .get(plugin)
            .map(|st| st.manifest.permissions)
            .ok_or_else(|| RpcError::not_found(format!("plugin {plugin} is not running")))?;
        let need = |allowed: bool, what: &str, key: &str| {
            if allowed {
                Ok(())
            } else {
                Err(RpcError::forbidden(format!(
                    "plugin {plugin} may not {what}: add `{key} = true` under [permissions] in its plugin.toml"
                )))
            }
        };
        let lines = |ls: &[termos_plugin::Line]| -> Vec<Cells> {
            ls.iter()
                .take(MAX_LINES)
                .map(|l| truncate_cells(to_cells(l), MAX_LINE_WIDTH))
                .collect()
        };
        let clean_title =
            |t: &str| -> String { t.chars().filter(|c| !c.is_control()).take(64).collect() };
        match req.method.as_str() {
            methods::WINDOW_OPEN => {
                need(perms.windows, "open windows", "windows")?;
                let p: proto::WindowOpen = req.params_as()?;
                let open = self
                    .plugins
                    .plugins
                    .get(plugin)
                    .map_or(0, |st| st.windows.len());
                if open >= MAX_WINDOWS {
                    return Err(RpcError::invalid_params(format!(
                        "a plugin may have at most {MAX_WINDOWS} windows"
                    )));
                }
                let key = self.target(p.session.as_deref())?;
                let view: SharedView = Arc::new(Mutex::new(PluginView {
                    title: clean_title(&p.title),
                    lines: lines(&p.lines),
                }));
                let events = self.plugins.events();
                let shared = view.clone();
                let e = self
                    .sessions
                    .get_mut(&key)
                    .ok_or_else(|| RpcError::not_found("no such session"))?;
                let id = e
                    .desktop
                    .open_window_with(|id| Box::new(PluginWindow::new(plugin, id, shared, events)));
                if let Some(st) = self.plugins.plugins.get_mut(plugin) {
                    st.windows.insert(id, view);
                }
                Ok(json!({"window": id.0}))
            }
            methods::WINDOW_DRAW => {
                need(perms.windows, "draw windows", "windows")?;
                let p: proto::WindowDraw = req.params_as()?;
                let id = PaneId(p.window);
                let view = self
                    .plugins
                    .plugins
                    .get(plugin)
                    .and_then(|st| st.windows.get(&id))
                    .cloned()
                    .ok_or_else(|| {
                        RpcError::not_found(format!("plugin {plugin} has no window {}", p.window))
                    })?;
                {
                    let mut v = view.lock().unwrap_or_else(PoisonError::into_inner);
                    v.lines = lines(&p.lines);
                    if let Some(t) = &p.title {
                        v.title = clean_title(t);
                    }
                }
                if let Some(e) = self.sessions.values_mut().find(|e| e.desktop.has_pane(id)) {
                    e.desktop.touch();
                }
                Ok(json!({}))
            }
            methods::WINDOW_CLOSE => {
                need(perms.windows, "close windows", "windows")?;
                let p: proto::WindowRef = req.params_as()?;
                let id = PaneId(p.window);
                let owned = self
                    .plugins
                    .plugins
                    .get_mut(plugin)
                    .is_some_and(|st| st.windows.remove(&id).is_some());
                if !owned {
                    return Err(RpcError::not_found(format!(
                        "plugin {plugin} has no window {}",
                        p.window
                    )));
                }
                self.close_windows(&[id]);
                Ok(json!({}))
            }
            methods::STATUS_SET => {
                need(perms.status, "show a status widget", "status")?;
                let p: proto::StatusSet = req.params_as()?;
                let cells = p.text.map(|l| truncate_cells(to_cells(&l), 40));
                let changed = match self.plugins.plugins.get_mut(plugin) {
                    Some(st) if st.status != cells => {
                        st.status = cells;
                        true
                    }
                    _ => false,
                };
                if changed {
                    self.push_plugin_ui();
                }
                Ok(json!({}))
            }
            methods::COMMAND_REGISTER => {
                need(perms.commands, "add commands", "commands")?;
                let p: proto::CommandRegister = req.params_as()?;
                let full = valid_command(plugin, &p.name).map_err(RpcError::invalid_params)?;
                let description: String = p
                    .description
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect();
                if let Some(st) = self.plugins.plugins.get_mut(plugin) {
                    if st.commands.len() >= 64 && !st.commands.contains_key(&p.name) {
                        return Err(RpcError::invalid_params(
                            "a plugin may register at most 64 commands",
                        ));
                    }
                    st.commands.insert(p.name, description);
                }
                self.push_plugin_ui();
                Ok(json!({"command": full}))
            }
            methods::NOTIFY_SEND => {
                need(perms.notify, "post notifications", "notify")?;
                let p: proto::NotifySend = req.params_as()?;
                let level = match p.level.as_deref() {
                    None | Some("info") => Level::Info,
                    Some("warn" | "warning") => Level::Warn,
                    Some("error") => Level::Error,
                    Some(other) => {
                        return Err(RpcError::invalid_params(format!(
                            "unknown level `{other}` (info, warn, error)"
                        )));
                    }
                };
                let text: String = p
                    .text
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect();
                self.post_everywhere(level, &format!("{plugin}: {text}"));
                Ok(json!({}))
            }
            methods::HELLO => Err(RpcError::new(
                codes::INVALID_REQUEST,
                "this connection already said hello",
            )),
            other => Err(RpcError::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown plugin method `{other}`"),
            )),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let desktops = self
            .sessions
            .values()
            .filter_map(|e| e.desktop.next_deadline());
        let frames = self
            .clients
            .values()
            .filter(|c| c.ready && !c.busy)
            .filter_map(|c| c.frame_due);
        let snapshots = self
            .sessions
            .iter()
            .filter(|(k, _)| self.ask != Ask::Asked(**k))
            .filter_map(|(_, e)| e.snapshot_due);
        desktops.chain(frames).chain(snapshots).min()
    }

    fn on_pty(&mut self, ev: PtyEvent) {
        let id = match &ev {
            PtyEvent::Output(id, _) | PtyEvent::Exited(id, _) | PtyEvent::Wake(id) => *id,
        };
        if let Some(e) = self.sessions.values_mut().find(|e| e.desktop.has_pane(id)) {
            e.desktop.on_pty(ev);
        }
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::FrontendHello { hello, reply } => {
                let result = self.resolve_frontend(&hello).map(|key| self.admit(key));
                if let Err(Ok(att)) = reply.send(result) {
                    // The connection went away before the reply.
                    self.remove_client(att.client);
                }
            }
            Event::FrontendMsg { client, msg } => {
                let Some(conn) = self.clients.get_mut(&client) else {
                    return;
                };
                match &msg {
                    ClientMsg::Hello {
                        caps, cols, rows, ..
                    } => {
                        conn.caps = caps.clone();
                        conn.cols = (*cols).max(1);
                        conn.rows = (*rows).max(1);
                        conn.ready = true;
                        conn.redraw();
                    }
                    ClientMsg::Resize { cols, rows } => {
                        conn.cols = (*cols).max(1);
                        conn.rows = (*rows).max(1);
                        conn.redraw();
                    }
                    _ => {}
                }
                let key = conn.session;
                let hello = matches!(msg, ClientMsg::Hello { .. });
                let question = (hello && self.ask == Ask::Pending && !self.saved.is_empty())
                    .then(|| self.restore_question());
                if let Some(e) = self.sessions.get_mut(&key) {
                    e.last_used = Instant::now();
                    e.desktop.handle_client(client, msg);
                    if let Some(q) = question {
                        e.desktop.ask_restore(client, &q);
                        self.ask = Ask::Asked(key);
                    }
                }
            }
            Event::FrameWritten { client } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.busy = false;
                }
            }
            Event::Disconnected { client } => self.remove_client(client),
            Event::PluginHello { token, reply } => {
                let result = self.plugins.connect(&token).ok_or_else(|| {
                    RpcError::forbidden("unknown plugin token, or the plugin is already connected")
                });
                if let Ok((name, _, _)) = &result {
                    tracing::info!("plugin {name} connected");
                }
                if let Err(Ok((name, conn, _))) = reply.send(result) {
                    self.plugins.disconnected(&name, conn);
                }
            }
            Event::PluginRequest { plugin, req, reply } => {
                let id = req.id.clone();
                let result = self.plugin_request(&plugin, &req);
                let resp = id.map(|id| match result {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, e),
                });
                let _ = reply.send(resp);
            }
            Event::PluginGone { plugin, conn } => {
                let closed = self.plugins.disconnected(&plugin, conn);
                self.close_windows(&closed);
                self.push_plugin_ui();
            }
            Event::Control { req, reply } => {
                let id = req.id.clone();
                let result = self.control(&req);
                let resp = id.map(|id| match result {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, e),
                });
                let _ = reply.send(resp);
            }
        }
    }

    /// Registers a frontend on session `key`.
    fn admit(&mut self, key: SessionKey) -> Attached {
        let client = ClientId(self.next_client);
        self.next_client += 1;
        let (tx, rx) = mpsc::unbounded_channel();
        self.clients.insert(
            client,
            Conn {
                session: key,
                tx,
                caps: Caps::default(),
                cols: 80,
                rows: 24,
                ready: false,
                busy: false,
                frame_due: None,
                last_frame: None,
                cur: Grid::new(1, 1),
                prev: None,
                sent_size: (0, 0),
                last_cursor: None,
                images: ClientImages::default(),
            },
        );
        let session = match self.sessions.get_mut(&key) {
            Some(e) => {
                e.last_used = Instant::now();
                e.desktop.name().to_owned()
            }
            None => String::new(),
        };
        tracing::info!("{client} attached to session {session}");
        Attached {
            client,
            session,
            rx,
        }
    }

    /// Delivers what the desktops produced, handles session requests and
    /// ended sessions, and draws the frames that are due.
    fn pump(&mut self) {
        // Requests can produce more output (a moved client is greeted, a
        // failed switch posts a notification): settle, with a bound.
        for _ in 0..4 {
            if !self.deliver() {
                break;
            }
        }
        let names: Vec<String> = self
            .sessions
            .values()
            .map(|e| e.desktop.name().to_owned())
            .collect();
        if names != self.known_names {
            for e in self.sessions.values_mut() {
                e.desktop.set_known_sessions(names.clone());
            }
            self.known_names = names;
        }
        self.snapshots();
        self.schedule_frames();
        self.draw_due_frames();
    }

    /// One round of outbox delivery and request handling. Returns whether
    /// anything happened.
    fn deliver(&mut self) -> bool {
        let mut requests = Vec::new();
        let mut ended = Vec::new();
        let mut outgoing = Vec::new();
        for (key, entry) in &mut self.sessions {
            for (to, msg) in entry.desktop.take_outbox() {
                outgoing.push((*key, to, msg));
            }
            requests.extend(entry.desktop.take_requests().into_iter().map(|r| (*key, r)));
            if let Some(r) = entry.desktop.quit_reason() {
                ended.push((*key, r.to_owned()));
            }
        }
        let busy = !(requests.is_empty() && ended.is_empty() && outgoing.is_empty());
        for (key, to, msg) in outgoing {
            match to {
                Some(c) => self.send(c, msg),
                None => {
                    for c in self.clients_of(key) {
                        self.send(c, msg.clone());
                    }
                }
            }
        }
        for (key, req) in requests {
            self.on_request(key, req);
        }
        for (key, reason) in ended {
            self.end_session(key, &reason);
        }
        busy
    }

    /// Schedules frames for the clients of sessions that changed.
    fn schedule_frames(&mut self) {
        let now = Instant::now();
        let mut changed = Vec::new();
        for (key, e) in &mut self.sessions {
            if e.desktop.take_dirty() {
                changed.push((*key, e.desktop.pending_output()));
            }
        }
        for (key, pending) in changed {
            for conn in self.clients.values_mut().filter(|c| c.session == key) {
                // Draw now, unless this is a flood and the last frame was
                // less than an interval ago.
                let due = match conn.last_frame {
                    Some(t) if pending >= SMALL_OUTPUT => (t + FRAME_INTERVAL).max(now),
                    _ => now,
                };
                conn.schedule(due);
            }
        }
    }

    fn on_request(&mut self, origin: SessionKey, req: SessionRequest) {
        match req {
            SessionRequest::Switch { client, to } => match self.find_session(&to) {
                Some(k) => self.move_client(client, k),
                None => self.post_to_client(client, &format!("no session named `{to}`")),
            },
            SessionRequest::Cycle { client, forward } => {
                let keys: Vec<SessionKey> = self.sessions.keys().copied().collect();
                if let Some(cur) = self.clients.get(&client).map(|c| c.session)
                    && let Some(pos) = keys.iter().position(|k| *k == cur)
                {
                    let n = keys.len();
                    let next = if forward {
                        keys[(pos + 1) % n]
                    } else {
                        keys[(pos + n - 1) % n]
                    };
                    if next != cur {
                        self.move_client(client, next);
                    }
                }
            }
            SessionRequest::New { client } => match self.create_session(None, None) {
                Ok(k) => self.move_client(client, k),
                Err(e) => self.post_to_client(client, &e.message),
            },
            SessionRequest::Rename { from, to } => {
                let Some(k) = self.find_session(&from) else {
                    return;
                };
                let problem = match valid_session_name(&to) {
                    Err(e) => Some(e.message),
                    Ok(()) if self.find_session(&to).is_some_and(|o| o != k) => {
                        Some(format!("a session named `{to}` already exists"))
                    }
                    Ok(()) => None,
                };
                if let Some(e) = self.sessions.get_mut(&k) {
                    match problem {
                        Some(p) => e.desktop.post(Level::Warn, p),
                        None => e.desktop.rename(to),
                    }
                }
            }
            SessionRequest::RestoreSaved { from_question } => {
                let in_place = (from_question && self.ask == Ask::Asked(origin)).then_some(origin);
                if self.ask == Ask::Asked(origin) {
                    self.ask = Ask::No;
                }
                let (restored, failed) = self.restore_saved(in_place, None);
                self.report_failed(Some(origin), &failed);
                if restored.is_empty()
                    && failed.is_empty()
                    && let Some(e) = self.sessions.get_mut(&origin)
                {
                    e.desktop
                        .post(Level::Info, "there are no saved sessions to restore");
                }
            }
            SessionRequest::PluginCommand { name } => {
                let session = self
                    .sessions
                    .get(&origin)
                    .map(|e| e.desktop.name().to_owned());
                if !self.invoke_plugin_command(&name, session)
                    && let Some(e) = self.sessions.get_mut(&origin)
                {
                    e.desktop
                        .post(Level::Warn, format!("plugin command {name} is gone"));
                }
            }
            SessionRequest::RestoreDeclined => {
                if self.ask == Ask::Asked(origin) {
                    self.ask = Ask::No;
                    if let Some(e) = self.sessions.get_mut(&origin) {
                        e.desktop.post(
                            Level::Info,
                            "saved sessions kept: restore them with the session.restore command or `termos restore`",
                        );
                    }
                }
            }
        }
    }

    fn post_to_client(&mut self, client: ClientId, text: &str) {
        if let Some(key) = self.clients.get(&client).map(|c| c.session)
            && let Some(e) = self.sessions.get_mut(&key)
        {
            e.desktop.post(Level::Warn, text);
        }
    }

    /// A session ended: its clients move to the most recently used other
    /// session, or exit if there is none.
    fn end_session(&mut self, key: SessionKey, reason: &str) {
        let Some(entry) = self.sessions.remove(&key) else {
            return;
        };
        let name = entry.desktop.name().to_owned();
        let saved_as = entry.saved_as.clone();
        drop(entry);
        tracing::info!("session {name} ended: {reason}");
        // Ended on purpose: its snapshot goes too (D-033).
        let dir = self.snapshot_dir();
        for n in saved_as.iter().chain(std::iter::once(&name)) {
            if let Err(e) = snapshot::remove(&dir, n) {
                tracing::warn!("cannot remove the snapshot of {n}: {e}");
            }
        }
        if self.ask == Ask::Asked(key) {
            self.ask = Ask::No;
        }
        if self.sessions.is_empty() && self.opts.exit_when_empty && self.shutdown.is_none() {
            self.shutdown = Some("no sessions are left".into());
        }
        let fallback = self.most_recent();
        for c in self.clients_of(key) {
            match fallback {
                Some(k) => {
                    self.move_client(c, k);
                    if let Some(e) = self.sessions.get_mut(&k) {
                        e.desktop
                            .post(Level::Info, format!("session {name} ended ({reason})"));
                    }
                }
                None => self.send(
                    c,
                    ServerMsg::Exit {
                        reason: reason.to_owned(),
                    },
                ),
            }
        }
    }

    fn draw_due_frames(&mut self) {
        let now = Instant::now();
        let due: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.ready && !c.busy && c.frame_due.is_some_and(|d| d <= now))
            .map(|(id, _)| *id)
            .collect();
        for client in due {
            self.draw(client);
        }
    }

    fn draw(&mut self, client: ClientId) {
        let Some(conn) = self.clients.get_mut(&client) else {
            return;
        };
        let Some(entry) = self.sessions.get_mut(&conn.session) else {
            return;
        };
        let cursor = entry.desktop.render(client, &mut conn.cur);
        let (grid_cols, grid_rows) = (conn.cur.cols(), conn.cur.rows());
        let visible = (grid_cols.min(conn.cols), grid_rows.min(conn.rows));
        let incremental = conn.sent_size == visible
            && conn
                .prev
                .as_ref()
                .is_some_and(|p| p.cols() == grid_cols && p.rows() == grid_rows);
        // Images first: moving a Sixel image changes which cells the diff
        // must repaint. A full redraw erases the screen, images included.
        let shown = entry.desktop.pixel_images(client);
        let images = update_images(conn, shown, &mut self.variants, visible, !incremental);
        let (spans, clear) = match &conn.prev {
            Some(prev) if incremental => (diff(prev, &conn.cur), false),
            _ => (full(&conn.cur), true),
        };
        let spans = crop(spans, visible.0, visible.1);
        let cursor = cursor.filter(|c| c.x < visible.0 && c.y < visible.1);
        if clear || !spans.is_empty() || cursor != conn.last_cursor || images.is_some() {
            let frame = FrameUpdate {
                cols: visible.0,
                rows: visible.1,
                clear,
                spans,
                cursor,
                images,
            };
            if conn.tx.send(ServerMsg::Frame(frame)).is_ok() {
                conn.busy = true;
            }
        }
        conn.last_cursor = cursor;
        conn.sent_size = visible;
        match &mut conn.prev {
            Some(prev) if incremental => std::mem::swap(prev, &mut conn.cur),
            _ => conn.prev = Some(conn.cur.clone()),
        }
        conn.frame_due = None;
        conn.last_frame = Some(Instant::now());
    }

    // ----- control API --------------------------------------------------

    fn control(&mut self, req: &Request) -> Result<Value, RpcError> {
        let target = || req.params_as::<TargetParams>().ok().and_then(|t| t.session);
        match req.method.as_str() {
            "server.info" => Ok(json!({
                "pid": std::process::id(),
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION,
                "rpc": termos_core::wire::RPC_VERSION,
                "name": self.opts.name,
                "socket": self.opts.socket_path.display().to_string(),
                "sessions": self.sessions.len(),
                "clients": self.clients.len(),
                "started": unix_secs(self.started),
                "restorable": self.saved.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            })),
            "plugin.list" => Ok(Value::Array(
                self.plugins
                    .plugins
                    .iter()
                    .map(|(name, st)| {
                        json!({
                            "name": name,
                            "version": st.manifest.version,
                            "description": st.manifest.description,
                            "running": st.running(),
                            "connected": st.conn.is_some(),
                            "windows": st.windows.keys().map(|w| w.0).collect::<Vec<_>>(),
                            "commands": st.commands.keys().map(|c| format!("{name}.{c}")).collect::<Vec<_>>(),
                            "status": st.status.as_ref().map(|c| c.iter().map(|(t, _)| t.as_str()).collect::<String>()),
                        })
                    })
                    .collect(),
            )),
            "session.saved" => Ok(Value::Array(
                self.saved
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "saved_at": s.saved_at,
                            "workspaces": s.layout.workspaces.len(),
                            "panes": s.panes.len(),
                        })
                    })
                    .collect(),
            )),
            "session.restore" => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    names: Option<Vec<String>>,
                }
                let p: P = req.params_as()?;
                if self.saved.is_empty() {
                    return Err(RpcError::not_found(
                        "there are no saved sessions to restore",
                    ));
                }
                if self.ask != Ask::No {
                    self.ask = Ask::No;
                }
                let (restored, failed) = self.restore_saved(None, p.names.as_deref());
                if restored.is_empty() && failed.is_empty() {
                    return Err(RpcError::not_found("no saved session has that name"));
                }
                Ok(json!({ "restored": restored, "failed": failed }))
            }
            "server.kill" => {
                self.shutdown = Some("killed by request".into());
                Ok(json!({ "stopping": true }))
            }
            "config.reload" => {
                self.reload_config();
                Ok(json!({ "reloaded": true }))
            }
            "session.list" => Ok(Value::Array(
                self.sessions
                    .iter()
                    .map(|(k, e)| {
                        let s = e.desktop.session();
                        json!({
                            "id": format!("${k}"),
                            "name": e.desktop.name(),
                            "workspaces": s.workspaces.len(),
                            "active_workspace": s.active_index(),
                            "panes": s.all_panes().len(),
                            "clients": self.clients_of(*k).len(),
                            "created": unix_secs(e.created),
                        })
                    })
                    .collect(),
            )),
            "session.new" => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    name: Option<String>,
                    #[serde(default)]
                    cwd: Option<String>,
                }
                let p: P = req.params_as()?;
                let key = self.create_session(p.name.as_deref(), p.cwd.as_deref())?;
                Ok(json!({ "id": format!("${key}"), "name": self.session_name(key) }))
            }
            "session.kill" => {
                let key = self.target(target().as_deref())?;
                let name = self.session_name(key);
                if let Some(e) = self.sessions.get_mut(&key) {
                    e.desktop.end("killed");
                }
                Ok(json!({ "killed": name }))
            }
            "session.rename" => {
                #[derive(Deserialize)]
                struct P {
                    #[serde(default)]
                    session: Option<String>,
                    name: String,
                }
                let p: P = req.params_as()?;
                valid_session_name(&p.name)?;
                let key = self.target(p.session.as_deref())?;
                if self.find_session(&p.name).is_some_and(|o| o != key) {
                    return Err(RpcError::invalid_params(format!(
                        "a session named `{}` already exists",
                        p.name
                    )));
                }
                if let Some(e) = self.sessions.get_mut(&key) {
                    e.desktop.rename(p.name.clone());
                }
                Ok(json!({ "name": p.name }))
            }
            "client.list" => Ok(Value::Array(
                self.clients
                    .iter()
                    .map(|(id, c)| {
                        json!({
                            "id": id.0,
                            "session": self.session_name(c.session),
                            "cols": c.cols,
                            "rows": c.rows,
                        })
                    })
                    .collect(),
            )),
            "client.detach" => {
                // Without a session: every client (`termos detach` outside
                // TermOS); inside a pane the CLI passes its own session.
                let targets: Vec<ClientId> = match target() {
                    Some(name) => {
                        let key = self.target(Some(&name))?;
                        self.clients_of(key)
                    }
                    None => self.clients.keys().copied().collect(),
                };
                for c in &targets {
                    let session = self
                        .clients
                        .get(c)
                        .map(|conn| self.session_name(conn.session))
                        .unwrap_or_default();
                    self.send(*c, ServerMsg::Detached { session });
                }
                Ok(json!({ "detached": targets.len() }))
            }
            "pane.list" => {
                let keys: Vec<SessionKey> = match target() {
                    Some(n) => vec![self.target(Some(&n))?],
                    None => self.sessions.keys().copied().collect(),
                };
                let mut out = Vec::new();
                for k in keys {
                    let Some(e) = self.sessions.get(&k) else {
                        continue;
                    };
                    for p in e.desktop.pane_infos() {
                        let mut v = serde_json::to_value(&p)
                            .map_err(|err| RpcError::new(codes::INTERNAL_ERROR, err.to_string()))?;
                        if let Value::Object(m) = &mut v {
                            m.insert("session".into(), Value::from(e.desktop.name()));
                        }
                        out.push(v);
                    }
                }
                Ok(Value::Array(out))
            }
            "pane.send-text" | "pane.send-keys" | "pane.kill" => self.pane_method(req),
            "notify.send" => {
                #[derive(Deserialize)]
                struct P {
                    text: String,
                    #[serde(default)]
                    level: Option<String>,
                    #[serde(default)]
                    session: Option<String>,
                }
                let p: P = req.params_as()?;
                let level = match p.level.as_deref() {
                    None | Some("info") => Level::Info,
                    Some("warn" | "warning") => Level::Warn,
                    Some("error") => Level::Error,
                    Some(other) => {
                        return Err(RpcError::invalid_params(format!(
                            "unknown level `{other}` (info, warn, error)"
                        )));
                    }
                };
                let keys: Vec<SessionKey> = match p.session {
                    Some(s) => vec![self.target(Some(&s))?],
                    None => self.sessions.keys().copied().collect(),
                };
                for k in &keys {
                    if let Some(e) = self.sessions.get_mut(k) {
                        e.desktop.post(level, p.text.clone());
                    }
                }
                Ok(json!({ "sessions": keys.len() }))
            }
            "command.run" => {
                #[derive(Deserialize)]
                struct P {
                    command: String,
                    #[serde(default)]
                    session: Option<String>,
                }
                let p: P = req.params_as()?;
                let cmd: Command = p
                    .command
                    .parse()
                    .map_err(|e: CommandError| RpcError::invalid_params(e.0))?;
                self.run_command(p.session.as_deref(), cmd)
            }
            other => {
                // Every named desktop command is a method too:
                // `termos msg pane.split-right`.
                #[derive(Deserialize, Default)]
                struct P {
                    #[serde(default)]
                    arg: Option<String>,
                }
                let text = match req.params_as::<P>().unwrap_or_default().arg {
                    Some(a) => format!("{other} {a}"),
                    None => other.to_owned(),
                };
                if self.plugins.commands().iter().any(|c| c.name == other) {
                    let key = self.target(target().as_deref())?;
                    let session = Some(self.session_name(key));
                    self.invoke_plugin_command(other, session);
                    return Ok(json!({ "invoked": other }));
                }
                match text.parse::<Command>() {
                    Ok(cmd) => self.run_command(target().as_deref(), cmd),
                    Err(_) => Err(RpcError::new(
                        codes::METHOD_NOT_FOUND,
                        format!("unknown method `{other}`"),
                    )),
                }
            }
        }
    }

    fn pane_method(&mut self, req: &Request) -> Result<Value, RpcError> {
        #[derive(Deserialize)]
        struct P {
            #[serde(default)]
            session: Option<String>,
            #[serde(default)]
            pane: Option<u64>,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            keys: Vec<String>,
        }
        let p: P = req.params_as()?;
        let key = match (p.session.as_deref(), p.pane) {
            (Some(s), _) => self.target(Some(s))?,
            (None, Some(id)) => self
                .sessions
                .iter()
                .find(|(_, e)| e.desktop.has_pane(PaneId(id)))
                .map(|(k, _)| *k)
                .ok_or_else(|| RpcError::not_found(format!("no pane {}", PaneId(id))))?,
            (None, None) => self.target(None)?,
        };
        let e = self
            .sessions
            .get_mut(&key)
            .ok_or_else(|| RpcError::not_found("no such session"))?;
        let pane = p.pane.map(PaneId);
        match req.method.as_str() {
            "pane.kill" => {
                let id = pane.ok_or_else(|| RpcError::invalid_params("`pane` is required"))?;
                if !e.desktop.kill_pane(id) {
                    return Err(RpcError::not_found(format!("no pane {id}")));
                }
            }
            "pane.send-keys" => {
                let keys = p
                    .keys
                    .iter()
                    .map(|k| k.parse::<KeyEvent>())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| RpcError::invalid_params(err.to_string()))?;
                e.desktop
                    .send_keys(pane, &keys)
                    .map_err(RpcError::not_found)?;
            }
            _ => {
                let text = p
                    .text
                    .ok_or_else(|| RpcError::invalid_params("`text` is required"))?;
                e.desktop
                    .send_text(pane, &text)
                    .map_err(RpcError::not_found)?;
            }
        }
        Ok(json!({ "ok": true }))
    }

    fn run_command(&mut self, session: Option<&str>, cmd: Command) -> Result<Value, RpcError> {
        let key = self.target(session)?;
        // Act as the session's newest client when there is one, so session
        // commands (switch, new, detach) have someone to act on.
        let client = self.clients_of(key).into_iter().max();
        let text = cmd.to_string();
        if let Some(e) = self.sessions.get_mut(&key) {
            e.desktop.run_as(client, cmd);
        }
        Ok(json!({ "ran": text }))
    }
}

/// The signals that stop the server (it saves snapshots first).
#[cfg(unix)]
struct Signals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn new() -> Option<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Some(Self {
            term: signal(SignalKind::terminate()).ok()?,
            int: signal(SignalKind::interrupt()).ok()?,
            hup: signal(SignalKind::hangup()).ok()?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.term.recv() => "SIGTERM",
            _ = self.int.recv() => "SIGINT",
            _ = self.hup.recv() => "SIGHUP",
        }
    }
}

/// The console events that stop the server. Logoff and shutdown are best
/// effort: Windows may end the process before the snapshots are written,
/// and the debounced snapshots are the safety net.
#[cfg(windows)]
struct Signals {
    c: tokio::signal::windows::CtrlC,
    brk: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Signals {
    fn new() -> Option<Self> {
        use tokio::signal::windows;
        Some(Self {
            c: windows::ctrl_c().ok()?,
            brk: windows::ctrl_break().ok()?,
            close: windows::ctrl_close().ok()?,
            logoff: windows::ctrl_logoff().ok()?,
            shutdown: windows::ctrl_shutdown().ok()?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        // TODO(M9): a hidden window for WM_ENDSESSION, so logoff and shutdown always save.
        tokio::select! {
            _ = self.c.recv() => "Ctrl+C",
            _ = self.brk.recv() => "Ctrl+Break",
            _ = self.close.recv() => "the console closing",
            _ = self.logoff.recv() => "logoff",
            _ = self.shutdown.recv() => "shutdown",
        }
    }
}

async fn recv_signal(signals: &mut Option<Signals>) -> &'static str {
    match signals {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}

async fn recv_reload(rx: &mut Option<mpsc::UnboundedReceiver<()>>) -> Option<()> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

async fn write_response(write: &mut LocalWrite, resp: &Response) -> io::Result<()> {
    write.write_all(wire::to_line(resp).as_bytes()).await
}

/// One socket connection: peer check, handshake, then the frontend frame
/// pump or the control request loop.
async fn handle_conn(stream: LocalStream, connector: Connector, allowed: PeerId) {
    let peer_ok = stream.peer().is_ok_and(|p| p == allowed);
    let (read, mut write) = stream.into_split();
    if !peer_ok {
        let e = RpcError::forbidden("connections from other users are not allowed");
        let _ = write_response(&mut write, &Response::err(Value::Null, e)).await;
        return;
    }
    let mut reader = BufReader::new(read);
    let Ok(Some(line)) = read_line(&mut reader).await else {
        return;
    };
    let req = match wire::parse_message(&line) {
        Ok(Message::Request(r)) => r,
        Ok(Message::Response(_)) => {
            let e = RpcError::new(codes::HELLO_REQUIRED, "the first message must be `hello`");
            let _ = write_response(&mut write, &Response::err(Value::Null, e)).await;
            return;
        }
        Err(e) => {
            let _ = write_response(&mut write, &Response::err(Value::Null, e)).await;
            return;
        }
    };
    let id = req.id.clone().unwrap_or(Value::Null);
    let hello = match wire::check_hello(&req) {
        Ok(h) => h,
        Err(e) => {
            let _ = write_response(&mut write, &Response::err(id, e)).await;
            return;
        }
    };
    match hello.kind {
        ConnKind::Frontend => match connector.attach_raw(hello).await {
            Ok(att) => {
                let reply = serde_json::to_value(hello_reply(
                    ConnKind::Frontend,
                    Some(att.session.clone()),
                ))
                .unwrap_or(Value::Null);
                if write_response(&mut write, &Response::ok(id, reply))
                    .await
                    .is_err()
                {
                    let _ = connector
                        .events
                        .send(Event::Disconnected { client: att.client });
                    return;
                }
                frontend_pump(reader, write, att, connector).await;
            }
            Err(e) => {
                let _ = write_response(&mut write, &Response::err(id, e)).await;
            }
        },
        ConnKind::Control => {
            let reply =
                serde_json::to_value(hello_reply(ConnKind::Control, None)).unwrap_or(Value::Null);
            if write_response(&mut write, &Response::ok(id, reply))
                .await
                .is_ok()
            {
                control_loop(reader, write, connector).await;
            }
        }
        ConnKind::Plugin => {
            let token = hello.token.clone().unwrap_or_default();
            match connector.plugin_hello(token).await {
                Ok((name, conn, rx)) => {
                    let reply = serde_json::to_value(hello_reply(ConnKind::Plugin, None))
                        .unwrap_or(Value::Null);
                    if write_response(&mut write, &Response::ok(id, reply))
                        .await
                        .is_ok()
                    {
                        plugin_loop(reader, write, &name, rx, &connector).await;
                    }
                    let _ = connector
                        .events
                        .send(Event::PluginGone { plugin: name, conn });
                }
                Err(e) => {
                    let _ = write_response(&mut write, &Response::err(id, e)).await;
                }
            }
        }
    }
}

/// Frames to the frontend, input from it.
async fn frontend_pump(
    mut reader: BufReader<LocalRead>,
    mut write: LocalWrite,
    att: Attached,
    connector: Connector,
) {
    let client = att.client;
    let events = connector.events.clone();
    let reader_task = tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut buf = vec![0u8; 64 * 1024];
        'conn: loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => decoder.push(&buf[..n]),
            }
            loop {
                match decoder.next_frame::<ClientMsg>() {
                    Ok(Some(msg)) => {
                        if events.send(Event::FrontendMsg { client, msg }).is_err() {
                            break 'conn;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("bad frame from {client}: {e}");
                        break 'conn;
                    }
                }
            }
        }
        let _ = events.send(Event::Disconnected { client });
    });
    let mut rx = att.rx;
    while let Some(msg) = rx.recv().await {
        let is_frame = matches!(msg, ServerMsg::Frame(_));
        let last = matches!(msg, ServerMsg::Detached { .. } | ServerMsg::Exit { .. });
        match wire::encode_frame(&msg) {
            Ok(bytes) => {
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            Err(e) => tracing::warn!("cannot encode a message for {client}: {e}"),
        }
        if is_frame {
            let _ = connector.events.send(Event::FrameWritten { client });
        }
        if last {
            break;
        }
    }
    let _ = write.shutdown().await;
    reader_task.abort();
    let _ = connector.events.send(Event::Disconnected { client });
}

/// A plugin's requests in, its responses and the server's notifications
/// out, until either side hangs up.
async fn plugin_loop(
    mut reader: BufReader<LocalRead>,
    mut write: LocalWrite,
    name: &str,
    mut notes: mpsc::UnboundedReceiver<String>,
    connector: &Connector,
) {
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        loop {
            let line = tokio::select! {
                Some(l) = notes.recv() => l,
                Some(l) = resp_rx.recv() => l,
                else => break,
            };
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = write.shutdown().await;
    });
    while let Ok(Some(line)) = read_line(&mut reader).await {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match wire::parse_message(&line) {
            Ok(Message::Request(req)) => connector.plugin_request(name, req).await,
            Ok(Message::Response(_)) => None,
            Err(e) => Some(Response::err(Value::Null, e)),
        };
        if let Some(r) = resp
            && resp_tx.send(wire::to_line(&r)).is_err()
        {
            break;
        }
    }
    writer.abort();
}

/// Newline-delimited JSON-RPC requests until the peer hangs up.
async fn control_loop(
    mut reader: BufReader<LocalRead>,
    mut write: LocalWrite,
    connector: Connector,
) {
    while let Ok(Some(line)) = read_line(&mut reader).await {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match wire::parse_message(&line) {
            Ok(Message::Request(req)) => connector.control(req).await,
            Ok(Message::Response(_)) => None,
            Err(e) => Some(Response::err(Value::Null, e)),
        };
        if let Some(r) = resp
            && write_response(&mut write, &r).await.is_err()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_render::cell::Style;

    #[test]
    fn session_names() {
        assert!(valid_session_name("main").is_ok());
        assert!(valid_session_name("web dev").is_ok());
        for bad in ["", " ", "$1", "a/b", "a:b", "x\u{7}"] {
            assert!(valid_session_name(bad).is_err(), "{bad:?}");
        }
        assert!(valid_session_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn cropping_keeps_frames_inside_small_clients() {
        let mut g = Grid::new(10, 3);
        g.put_str(0, 0, "abcdefgh界", Style::new(), 10);
        g.put_str(0, 2, "zz", Style::new(), 10);
        let spans = crop(full(&g), 9, 2);
        assert!(spans.iter().all(|s| s.row < 2));
        let first = spans.iter().find(|s| s.row == 0).expect("row 0 is sent");
        assert_eq!(first.cells.len(), 9);
        // The wide character at columns 8-9 does not fit: it became a blank.
        assert_eq!(first.cells[8].width, 1);
    }
}
