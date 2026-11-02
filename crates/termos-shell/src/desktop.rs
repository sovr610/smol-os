// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop: all server-side state and behaviour.
//!
//! [`Desktop`] is a synchronous state machine. The async loop in
//! [`crate::server`] feeds it client messages, PTY output, config reloads
//! and timer ticks, and asks it to render when something changed.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use termos_core::app::{App, AppEffect, AppWaker, render_app};
use termos_core::clipboard::{ClipSource, SharedClipboard};
use termos_core::config::{Config, StatusPosition, WindowSize};
use termos_core::ids::{ClientId, PaneId, PaneIds};
use termos_core::input::{KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_render::caps::{Caps, ImageProtocol};
use termos_render::cell::Style;
use termos_render::color::Rgb;

use crate::anim::{self, Animations};
use crate::images::{self, BlockCache, Candidate, Place, ShownImage};
use termos_render::encode::CursorState;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::{Theme, ThemeBase, is_light};
use termos_term::input::{encode_focus, encode_key, encode_mouse, encode_paste};
use termos_term::pty::{PtyEvent, PtyPane, SpawnSpec};
use termos_term::vt::{AlacrittyVt, VtBackend, VtEvent};
use termos_wm::compose::{ComposeOptions, compose};
use termos_wm::hit::{Button, Edge, Hit};
use termos_wm::layout::Dir;
use termos_wm::workspace::{Placement, Session, Snap, SplitHow, Workspace};
use tokio::sync::mpsc;

use crate::apps::{NotificationCenter, Tutorial};
use crate::commands::{COMMANDS, Command};
use crate::copymode::{CopyAction, CopyMode};
use crate::keys::KeyTables;
use crate::notify::{Level, Notifications};
use crate::palette::{Palette, PaletteAction, PaletteItem};
use crate::plugins::{Cells, PluginCommand};
use crate::prompt::{Confirmed, Prompt, PromptAction, PromptKind};
use crate::snapshot::{self, PaneSnapshot, Relaunch, SNAPSHOT_VERSION, SavedKind, SessionSnapshot};
use crate::status::{self, StatusInfo, StatusRegions, TabInfo};
use crate::ui::Chrome;
use crate::whichkey;

/// What a pane shows.
pub enum PaneKind {
    /// A program on a PTY.
    Term {
        vt: Box<dyn VtBackend>,
        pty: PtyPane,
    },
    /// An in-process app.
    App(Box<dyn App>),
}

/// A pane (a TermOS window).
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    /// Title set by the program (OSC 0/2).
    pub title: String,
    /// Working directory reported by the program (OSC 7).
    pub cwd: Option<PathBuf>,
    pub size: (u16, u16),
    /// Rang the bell while not visible.
    pub bell: bool,
}

impl Pane {
    /// A short name: the foreground program (Linux), else the title, else
    /// the app title.
    pub fn display_name(&self) -> String {
        match &self.kind {
            PaneKind::Term { pty, .. } => pty
                .foreground_name()
                .or_else(|| (!self.title.is_empty()).then(|| self.title.clone()))
                .unwrap_or_else(|| "shell".into()),
            PaneKind::App(app) => app.title(),
        }
    }

    fn title_for_border(&self) -> String {
        match &self.kind {
            PaneKind::Term { .. } if !self.title.is_empty() => self.title.clone(),
            _ => self.display_name(),
        }
    }
}

/// Facts about a pane (`pane.list`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PaneInfo {
    pub id: u64,
    pub workspace: usize,
    pub name: String,
    pub title: String,
    pub kind: String,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub floating: bool,
    pub focused: bool,
}

/// Startup settings for a desktop.
#[derive(Debug, Clone)]
pub struct DesktopOptions {
    pub session_name: String,
    pub config_path: PathBuf,
    pub paths: Paths,
    /// `$SHELL` from the environment.
    pub env_shell: Option<String>,
    /// Server-wide pane id allocator.
    pub pane_ids: PaneIds,
    /// Extra environment for panes (e.g. `TERMOS=<socket>,<pid>,<session>`).
    pub pane_env: Vec<(String, String)>,
    /// Where the first shell starts (default: the server's directory).
    pub cwd: Option<PathBuf>,
    /// Server-wide clipboard history (D-050), shared by every session.
    pub clipboard: SharedClipboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyState {
    Normal,
    /// The prefix was pressed; the next key is looked up in the prefix table.
    Prefix,
    /// A repeatable binding ran; more repeatable keys work until then.
    Repeat(Instant),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Drag {
    Move {
        pane: PaneId,
        dx: u16,
        dy: u16,
    },
    Resize {
        pane: PaneId,
        edge: Edge,
        start: Rect,
        x0: u16,
        y0: u16,
    },
    Separator {
        path: Vec<usize>,
        index: usize,
        dir: Dir,
    },
    /// Pressed in a pane; becomes a text selection once the mouse moves.
    Select {
        pane: PaneId,
        col: u16,
        line: i32,
        started: bool,
    },
}

/// One attached client's UI state (D-032): every terminal has its own
/// prefix state, overlays, drags and size.
#[derive(Debug)]
struct ClientUi {
    caps: Caps,
    /// This client's theme: `[theme]` for its terminal's background, or
    /// the one it picked with `theme.select`.
    theme: Theme,
    theme_pick: Option<ThemeBase>,
    /// Pixel images this client shows after its last render (the server
    /// sends them to it).
    pixel_images: Vec<ShownImage>,
    cols: u16,
    rows: u16,
    key_state: KeyState,
    whichkey_deadline: Option<Instant>,
    whichkey_visible: bool,
    whichkey_full: bool,
    palette: Option<Palette>,
    prompt: Option<Prompt>,
    drag: Option<Drag>,
    mouse_owner: Option<PaneId>,
    status_regions: StatusRegions,
    title_sent: String,
}

impl Default for ClientUi {
    fn default() -> Self {
        Self {
            caps: Caps::default(),
            theme: Theme::default(),
            theme_pick: None,
            pixel_images: Vec::new(),
            cols: 80,
            rows: 24,
            key_state: KeyState::Normal,
            whichkey_deadline: None,
            whichkey_visible: false,
            whichkey_full: false,
            palette: None,
            prompt: None,
            drag: None,
            mouse_owner: None,
            status_regions: StatusRegions::default(),
            title_sent: String::new(),
        }
    }
}

/// What the desktop asks of the server it cannot do alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRequest {
    /// Move a client to another session.
    Switch { client: ClientId, to: String },
    /// Move a client to the next (`true`) or previous session.
    Cycle { client: ClientId, forward: bool },
    /// Create a session and move the client to it.
    New { client: ClientId },
    /// Rename a session (the server keeps names unique).
    Rename { from: String, to: String },
    /// Restore saved sessions; `from_question` when answering the startup
    /// question (the asking session is then replaced by its saved state).
    RestoreSaved { from_question: bool },
    /// The startup restore question was answered "no".
    RestoreDeclined,
    /// Run a plugin's command (`<plugin>.<name>`).
    PluginCommand { name: String },
}

/// All state of one session: its workspaces and panes, plus the UI state of
/// every attached client.
pub struct Desktop {
    opts: DesktopOptions,
    config: Config,
    keys: KeyTables,
    /// Default foreground and background of the panes: the colours of the
    /// most recently active client's terminal (OSC 10/11 answers).
    term_colors: (Rgb, Rgb),
    /// That terminal's cell size in pixels, for images in panes.
    cell_px: Option<termos_render::caps::PxSize>,
    session: Session,
    panes: BTreeMap<PaneId, Pane>,
    pty_tx: mpsc::Sender<PtyEvent>,
    /// The session size (from the attached clients, D-032).
    cols: u16,
    rows: u16,
    /// The client being served; its state is swapped into `ui`.
    current: Option<ClientId>,
    ui: ClientUi,
    clients: BTreeMap<ClientId, ClientUi>,
    /// The client that most recently sent input (`window-size = latest`).
    last_active: Option<ClientId>,
    copy: Option<CopyMode>,
    notes: Notifications,
    paste_buffer: Option<String>,
    clock_deadline: Option<Instant>,
    /// Float and toast animations (drawn only for clients that animate).
    anim: Animations,
    /// App images rendered as blocks.
    block_cache: BlockCache,
    /// Messages for one client (`Some`) or all of them (`None`).
    outbox: Vec<(Option<ClientId>, ServerMsg)>,
    requests: Vec<SessionRequest>,
    /// Names of all sessions on the server (for the palette).
    known_sessions: Vec<String>,
    /// Status-line widgets and palette commands of plugins (D-034).
    plugin_status: Vec<Cells>,
    plugin_commands: Vec<PluginCommand>,
    dirty: bool,
    /// The workspace/pane structure changed since the last snapshot.
    layout_changed: bool,
    quit: Option<String>,
    /// Bytes of visible pane output since the last render.
    output_since_render: usize,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("session", &self.session.name)
            .field("panes", &self.panes.len())
            .finish_non_exhaustive()
    }
}

/// A terminal's default foreground and background: its real background
/// when known, with a foreground that reads on it; xterm's otherwise.
fn default_term_colors(bg: Option<Rgb>) -> (Rgb, Rgb) {
    match bg {
        Some(bg) if is_light(bg) => (Rgb::BLACK, bg),
        Some(bg) => (Rgb::new(229, 229, 229), bg),
        None => (Rgb::new(229, 229, 229), Rgb::BLACK),
    }
}

/// Time until the next wall-clock minute.
fn next_minute(now: Instant) -> Instant {
    let z = jiff::Zoned::now();
    let into = u64::try_from(z.second()).unwrap_or(0);
    let nanos = u64::try_from(z.subsec_nanosecond()).unwrap_or(0);
    let wait = Duration::from_secs(60 - into.min(59)).saturating_sub(Duration::from_nanos(nanos));
    now + wait.max(Duration::from_millis(10))
}

/// The editor for `file.edit`: `$VISUAL`, else `$EDITOR` (split into a
/// program and its arguments), else `vi` (Notepad on Windows).
pub(crate) fn editor_command(env: impl Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    for var in ["VISUAL", "EDITOR"] {
        if let Some(v) = env(var) {
            let mut words = v.split_whitespace().map(str::to_owned);
            if let Some(program) = words.next() {
                return (program, words.collect());
            }
        }
    }
    let fallback = if cfg!(windows) { "notepad.exe" } else { "vi" };
    (fallback.to_owned(), Vec::new())
}

/// The user's home directory (`HOME`, or `USERPROFILE` on Windows).
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

impl Desktop {
    /// A desktop without panes yet (the caller adds them).
    fn blank(
        opts: DesktopOptions,
        config: Config,
        pty_tx: mpsc::Sender<PtyEvent>,
        cols: u16,
        rows: u16,
    ) -> Self {
        let (keys, key_errors) = KeyTables::from_config(&config);
        let placeholder = Session::new(
            opts.session_name.clone(),
            Workspace::new("shell", PaneId(0)),
        );
        let mut d = Self {
            opts,
            keys,
            term_colors: default_term_colors(None),
            cell_px: None,
            session: placeholder,
            panes: BTreeMap::new(),
            pty_tx,
            cols: cols.max(2),
            rows: rows.max(2),
            current: None,
            ui: ClientUi::default(),
            clients: BTreeMap::new(),
            last_active: None,
            copy: None,
            notes: Notifications::new(),
            paste_buffer: None,
            clock_deadline: None,
            anim: Animations::default(),
            block_cache: BlockCache::default(),
            outbox: Vec::new(),
            requests: Vec::new(),
            known_sessions: Vec::new(),
            plugin_status: Vec::new(),
            plugin_commands: Vec::new(),
            dirty: true,
            layout_changed: true,
            quit: None,
            output_since_render: 0,
            config,
        };
        if d.config.status.clock {
            d.clock_deadline = Some(next_minute(Instant::now()));
        }
        for e in key_errors {
            d.notify(Level::Warn, e);
        }
        d
    }

    /// Creates the desktop with one workspace running the shell. Shows the
    /// tutorial on first run.
    pub fn new(
        opts: DesktopOptions,
        config: Config,
        pty_tx: mpsc::Sender<PtyEvent>,
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let mut d = Self::blank(opts, config, pty_tx, cols, rows);
        let area = d.layout_area();
        let first = d.spawn_term(d.opts.cwd.clone(), (area.w, area.h))?;
        d.session = Session::new(d.opts.session_name.clone(), Workspace::new("shell", first));
        let marker = d.opts.paths.tutorial_marker();
        if d.config.tutorial.show_on_first_run && !marker.exists() {
            d.open_app(Box::new(Tutorial::new(&d.keys.prefix_key.to_string())));
            let written = std::fs::create_dir_all(&d.opts.paths.state_dir)
                .and_then(|()| std::fs::write(&marker, "shown\n"));
            if let Err(e) = written {
                tracing::warn!("cannot record that the tutorial was shown: {e}");
            }
        }
        Ok(d)
    }

    /// Rebuilds a saved session (D-033): the layout, a shell in each saved
    /// directory, and the allowlisted programs typed into their shells.
    /// Built-in apps are not restored. Returns the desktop and the names of
    /// programs that were running but not restarted.
    pub fn from_snapshot(
        opts: DesktopOptions,
        config: Config,
        pty_tx: mpsc::Sender<PtyEvent>,
        snap: &SessionSnapshot,
    ) -> io::Result<(Self, Vec<String>)> {
        let mut d = Self::blank(opts, config, pty_tx, 80, 24);
        let mut layout = snap.layout.clone();
        if !layout.sanitize() {
            return Err(io::Error::other("the snapshot has no panes"));
        }
        let allow = d.config.session.restore_programs.clone();
        let area = d.layout_area();
        let mut map = std::collections::HashMap::new();
        let mut skipped = Vec::new();
        for old in layout.all_panes() {
            let Some(saved) = snap
                .panes
                .iter()
                .find(|p| p.id == old.0 && p.kind == SavedKind::Terminal)
            else {
                continue;
            };
            let id = d.spawn_term(snapshot::restore_cwd(saved), (area.w, area.h))?;
            map.insert(old, id);
            let Some(pane) = d.panes.get_mut(&id) else {
                continue;
            };
            pane.title.clone_from(&saved.title);
            let PaneKind::Term { vt, pty } = &mut pane.kind else {
                continue;
            };
            if let Some(lines) = &saved.scrollback {
                let mut text = String::new();
                for l in lines {
                    text.extend(l.chars().filter(|c| !c.is_control()));
                    text.push_str("\r\n");
                }
                vt.advance(text.as_bytes());
            }
            match snapshot::relaunch(saved, &allow) {
                Relaunch::Program(argv) => {
                    pty.write(format!("{}\r", snapshot::shell_quote(&argv)).into_bytes());
                }
                Relaunch::Skipped(name) => skipped.push(name),
                Relaunch::Shell => {}
            }
        }
        if !layout.remap_panes(|old| map.get(&old).copied()) {
            return Err(io::Error::other("the snapshot has no terminal panes"));
        }
        layout.name.clone_from(&d.opts.session_name);
        d.session = layout;
        d.layout_changed = true;
        d.dirty = true;
        Ok((d, skipped))
    }

    /// The session as it is now, for saving (D-033).
    pub fn snapshot(&self) -> SessionSnapshot {
        let cfg = &self.config.session;
        let panes = self
            .panes
            .values()
            .map(|p| match &p.kind {
                PaneKind::Term { vt, pty } => {
                    let argv = pty.foreground_command();
                    let program = argv
                        .as_ref()
                        .and_then(|a| a.first())
                        .map(|a| snapshot::program_name(a).to_owned());
                    let scrollback = cfg.save_scrollback.then(|| {
                        let history = vt.history_size().min(cfg.scrollback_lines);
                        let first = -i32::try_from(history).unwrap_or(0);
                        let mut lines: Vec<String> = (first..i32::from(vt.size().1))
                            .map(|l| vt.line_text(l))
                            .collect();
                        while lines.last().is_some_and(String::is_empty) {
                            lines.pop();
                        }
                        lines
                    });
                    PaneSnapshot {
                        id: p.id.0,
                        kind: SavedKind::Terminal,
                        cwd: pty.foreground_cwd().or_else(|| p.cwd.clone()),
                        program,
                        argv: argv.filter(|a| snapshot::allowed(a, &cfg.restore_programs)),
                        title: p.title.clone(),
                        scrollback,
                    }
                }
                PaneKind::App(_) => PaneSnapshot {
                    id: p.id.0,
                    kind: SavedKind::App,
                    cwd: None,
                    program: None,
                    argv: None,
                    title: String::new(),
                    scrollback: None,
                },
            })
            .collect();
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            name: self.session.name.clone(),
            saved_at: snapshot::now_secs(),
            layout: self.session.clone(),
            panes,
        }
    }

    /// Asks `client` whether to restore saved sessions (`restore = "ask"`).
    pub fn ask_restore(&mut self, client: ClientId, question: &str) {
        if self.enter(client) {
            self.ui.palette = None;
            self.ui.prompt = Some(Prompt::confirm(Confirmed::Restore, question));
            self.dirty = true;
            self.leave();
        }
    }

    // ----- accessors (also used by tests) ------------------------------

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    pub fn focused(&self) -> Option<PaneId> {
        self.session.active().focus()
    }

    /// Decides how each image that may use pixels is shown to the current
    /// client: in pixels (returned) or, for Sixel and iTerm2 when it is not
    /// wholly visible or something moves, as blocks in its visible pieces.
    #[allow(clippy::too_many_arguments)]
    fn place_pixel_images(
        &mut self,
        grid: &mut Grid,
        placement: &Placement,
        overlays: &[Rect],
        candidates: Vec<Candidate>,
        protocol: ImageProtocol,
        bg: Rgb,
        animates: bool,
    ) -> Vec<ShownImage> {
        let moving = self.ui.drag.is_some() || animates;
        let mut shown = Vec::new();
        for c in candidates {
            // Everything drawn above the image's pane: floats higher in
            // the stack (all of them, for a tiled pane), then overlays.
            let above_from = placement
                .floats
                .iter()
                .position(|f| f.pane == c.pane)
                .map_or(0, |i| i + 1);
            let mut above: Vec<Rect> = placement.floats[above_from..]
                .iter()
                .map(|f| f.outer)
                .collect();
            above.extend_from_slice(overlays);
            let pieces = images::visible_pieces(c.place, c.clip, &above);
            if pieces.is_empty() {
                continue;
            }
            let pixels = match protocol {
                ImageProtocol::Kitty => true,
                ImageProtocol::Sixel | ImageProtocol::Iterm2 => {
                    images::whole_image_allowed(c.place, &pieces, grid.rows(), moving)
                }
                ImageProtocol::Blocks => false,
            };
            if pixels {
                shown.push(ShownImage {
                    key: c.key,
                    picture: c.picture,
                    protocol,
                    place: c.place,
                    pieces,
                });
            } else {
                let cells = self.block_cache.get(c.key, &c.picture, c.place, bg);
                images::draw_blocks(grid, &cells, c.place, &pieces);
            }
        }
        shown
    }

    /// The pixel images `client` shows after its last render.
    pub fn pixel_images(&self, client: ClientId) -> Vec<ShownImage> {
        self.ui_of(client)
            .map(|u| u.pixel_images.clone())
            .unwrap_or_default()
    }

    /// The animations in progress (for tests and diagnostics).
    pub fn animations(&self) -> &Animations {
        &self.anim
    }

    /// Terminal modes of the focused pane's program (for tests and
    /// diagnostics); `None` for app windows.
    pub fn focused_modes(&self) -> Option<termos_term::vt::VtModes> {
        match &self.panes.get(&self.focused()?)?.kind {
            PaneKind::Term { vt, .. } => Some(vt.modes()),
            PaneKind::App(_) => None,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn keys(&self) -> &KeyTables {
        &self.keys
    }

    pub fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
    }

    fn ui_of(&self, client: ClientId) -> Option<&ClientUi> {
        if self.current == Some(client) {
            Some(&self.ui)
        } else {
            self.clients.get(&client)
        }
    }

    pub fn palette_open(&self, client: ClientId) -> bool {
        self.ui_of(client).is_some_and(|u| u.palette.is_some())
    }

    pub fn prompt_open(&self, client: ClientId) -> bool {
        self.ui_of(client).is_some_and(|u| u.prompt.is_some())
    }

    pub fn whichkey_visible(&self, client: ClientId) -> bool {
        self.ui_of(client)
            .is_some_and(|u| u.whichkey_visible || u.whichkey_full)
    }

    pub fn paste_buffer(&self) -> Option<&str> {
        self.paste_buffer.as_deref()
    }

    pub fn toast(&self) -> Option<&str> {
        self.notes.toast().map(|(t, _)| t)
    }

    /// The session's name.
    pub fn name(&self) -> &str {
        &self.session.name
    }

    /// Renames the session.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.session.name = name.into();
        self.layout_changed = true;
        self.dirty = true;
    }

    /// Attached clients.
    pub fn client_ids(&self) -> Vec<ClientId> {
        let mut v: Vec<ClientId> = self.clients.keys().copied().collect();
        v.extend(self.current);
        v.sort();
        v
    }

    /// A client's terminal size.
    pub fn client_size(&self, client: ClientId) -> Option<(u16, u16)> {
        self.ui_of(client).map(|u| (u.cols, u.rows))
    }

    /// Every pane id in this session (for routing PTY events).
    pub fn has_pane(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id)
    }

    /// Takes messages for clients: `Some(id)` for one client, `None` for all.
    pub fn take_outbox(&mut self) -> Vec<(Option<ClientId>, ServerMsg)> {
        std::mem::take(&mut self.outbox)
    }

    /// Tells the desktop which sessions exist (for the palette).
    pub fn set_known_sessions(&mut self, names: Vec<String>) {
        self.known_sessions = names;
    }

    /// Plugin status widgets and palette commands.
    pub fn set_plugin_ui(&mut self, status: Vec<Cells>, commands: Vec<PluginCommand>) {
        if status != self.plugin_status {
            self.plugin_status = status;
            self.dirty = true;
        }
        self.plugin_commands = commands;
    }

    /// Something drawn by the desktop changed outside of it (a plugin
    /// redrew its window).
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    /// Opens a floating window whose app needs its own pane id.
    pub fn open_window_with(&mut self, make: impl FnOnce(PaneId) -> Box<dyn App>) -> PaneId {
        let id = self.alloc_id();
        self.insert_app(id, make(id));
        id
    }

    /// Takes requests for the server (session switching and creation).
    pub fn take_requests(&mut self) -> Vec<SessionRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Whether the session's structure changed since the last call (the
    /// server then schedules a snapshot).
    pub fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }

    /// Whether a redraw is needed (and clears the flag).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Bytes of visible pane output since the last render. The server
    /// renders small updates (typing echo) immediately and coalesces large
    /// ones (output floods) to the frame rate.
    pub fn pending_output(&self) -> usize {
        self.output_since_render
    }

    /// Set when the session ended.
    pub fn quit_reason(&self) -> Option<&str> {
        self.quit.as_deref()
    }

    // ----- geometry ---------------------------------------------------

    fn status_y(&self) -> u16 {
        match self.config.status.position {
            StatusPosition::Top => 0,
            StatusPosition::Bottom => self.rows.saturating_sub(1),
        }
    }

    /// The screen minus the status line.
    pub fn layout_area(&self) -> Rect {
        let y = match self.config.status.position {
            StatusPosition::Top => 1,
            StatusPosition::Bottom => 0,
        };
        Rect::new(0, y, self.cols, self.rows.saturating_sub(1))
    }

    fn placement(&self) -> Placement {
        self.session.active().placement(self.layout_area())
    }

    // ----- panes ------------------------------------------------------

    fn alloc_id(&mut self) -> PaneId {
        self.opts.pane_ids.next()
    }

    fn spawn_term(&mut self, cwd: Option<PathBuf>, size: (u16, u16)) -> io::Result<PaneId> {
        self.spawn_pane(cwd, size, None)
    }

    /// A new pane running `program` (with arguments), else the shell.
    fn spawn_pane(
        &mut self,
        cwd: Option<PathBuf>,
        size: (u16, u16),
        program: Option<(String, Vec<String>)>,
    ) -> io::Result<PaneId> {
        let id = self.alloc_id();
        let (w, h) = (size.0.max(1), size.1.max(1));
        let mut spec =
            SpawnSpec::shell(self.config.shell_program(self.opts.env_shell.clone()), w, h);
        if let Some((program, args)) = program {
            spec.program = program;
            spec.args = args;
        }
        spec.cwd = cwd.filter(|c| c.is_dir());
        spec.env = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERMOS".into(), std::process::id().to_string()),
            ("TERMOS_PANE".into(), id.to_string()),
        ];
        spec.env.extend(self.opts.pane_env.iter().cloned());
        let pty = PtyPane::spawn(id, &spec, self.pty_tx.clone())?;
        let mut vt: Box<dyn VtBackend> = Box::new(AlacrittyVt::new(w, h, self.config.scrollback));
        vt.set_default_colors(self.term_colors.0, self.term_colors.1);
        vt.set_cell_px(self.cell_px);
        self.panes.insert(
            id,
            Pane {
                id,
                kind: PaneKind::Term { vt, pty },
                title: String::new(),
                cwd: None,
                size: (w, h),
                bell: false,
            },
        );
        Ok(id)
    }

    fn open_app(&mut self, app: Box<dyn App>) -> PaneId {
        let id = self.alloc_id();
        self.insert_app(id, app);
        id
    }

    fn insert_app(&mut self, id: PaneId, mut app: Box<dyn App>) {
        let tx = self.pty_tx.clone();
        app.set_waker(AppWaker::new(move || {
            // A full queue already has a wakeup in it.
            let _ = tx.try_send(PtyEvent::Wake(id));
        }));
        self.panes.insert(
            id,
            Pane {
                id,
                kind: PaneKind::App(app),
                title: String::new(),
                cwd: None,
                size: (0, 0),
                bell: false,
            },
        );
        let area = self.layout_area();
        self.session.active_mut().add_float(id, area);
        self.animate(anim::Kind::FloatOpen(id), Instant::now());
        self.dirty = true;
        self.layout_changed = true;
    }

    /// A path typed for a command: relative to the focused pane's
    /// directory, like in a shell; `~` is the home directory.
    fn resolve_path(&self, text: &str) -> PathBuf {
        let path = match text.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => match home_dir() {
                Some(home) => home.join(rest.trim_start_matches(['/', '\\'])),
                None => PathBuf::from(text),
            },
            _ => PathBuf::from(text),
        };
        if path.is_absolute() {
            return path;
        }
        self.focused_cwd()
            .or_else(|| std::env::current_dir().ok())
            .map_or(path.clone(), |dir| dir.join(&path))
    }

    /// The focused pane's working directory, for new panes.
    fn focused_cwd(&self) -> Option<PathBuf> {
        let pane = self.panes.get(&self.focused()?)?;
        match &pane.kind {
            PaneKind::Term { pty, .. } => pane.cwd.clone().or_else(|| pty.foreground_cwd()),
            PaneKind::App(_) => None,
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        let active = self.session.active();
        if active.is_floating(id) {
            let area = self.layout_area();
            if let Some(f) = active.placement(area).floats.iter().find(|f| f.pane == id) {
                let outer = f.outer;
                self.animate(anim::Kind::FloatClose(outer), Instant::now());
            }
        }
        if let Some(ws_idx) = self.session.find(id) {
            let ws = &mut self.session.workspaces[ws_idx];
            ws.remove(id);
            if ws.is_empty() {
                if self.session.workspaces.len() == 1 {
                    self.quit = Some("the last pane was closed".into());
                } else {
                    self.session.remove(ws_idx);
                }
            }
        }
        if let Some(mut pane) = self.panes.remove(&id)
            && let PaneKind::Term { pty, .. } = &mut pane.kind
        {
            pty.kill();
        }
        if self.copy.as_ref().is_some_and(|c| c.pane == id) {
            self.copy = None;
        }
        if self.ui.mouse_owner == Some(id) {
            self.ui.mouse_owner = None;
        }
        self.ui.drag = None;
        self.dirty = true;
        self.layout_changed = true;
    }

    /// Shows a notification (e.g. a startup problem found by the caller).
    pub fn post(&mut self, level: Level, text: impl Into<String>) {
        self.notify(level, text);
    }

    fn notify(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        match level {
            Level::Error => tracing::error!("{text}"),
            Level::Warn => tracing::warn!("{text}"),
            Level::Info => tracing::info!("{text}"),
        }
        let now = Instant::now();
        self.notes.push(level, text, now);
        self.animate(anim::Kind::Toast, now);
        self.dirty = true;
    }

    /// Whether `ui`'s client sees animations: on in the config, and not
    /// over SSH or with reduced motion (D-042).
    fn animates(config: &Config, ui: &ClientUi) -> bool {
        config.ui.animations
            && !config.ui.reduced_motion
            && !ui.caps.remote
            && !ui.caps.reduced_motion
    }

    fn any_client_animates(&self) -> bool {
        self.clients
            .values()
            .chain(self.current.map(|_| &self.ui))
            .any(|ui| Self::animates(&self.config, ui))
    }

    /// Starts an animation, if anyone will see it (otherwise there would
    /// be frames for nobody).
    fn animate(&mut self, kind: anim::Kind, now: Instant) {
        if self.any_client_animates() {
            self.anim.start(kind, now);
        }
    }

    fn write_to_pane(&mut self, id: PaneId, bytes: Vec<u8>) {
        if let Some(Pane {
            kind: PaneKind::Term { vt, pty },
            ..
        }) = self.panes.get_mut(&id)
        {
            if vt.display_offset() > 0 {
                vt.scroll_to_bottom();
                self.dirty = true;
            }
            pty.write(bytes);
        }
    }

    fn copy_text(&mut self, text: String, source: ClipSource) {
        let n = text.chars().count();
        self.remember_copy(&text, source);
        self.outbox
            .push((self.current, ServerMsg::Clipboard(text.clone())));
        self.paste_buffer = Some(text);
        self.notify(Level::Info, format!("copied {n} characters"));
    }

    /// Adds a copy to the clipboard history.
    fn remember_copy(&self, text: &str, source: ClipSource) {
        if let Ok(mut h) = self.opts.clipboard.lock() {
            h.push(text, source);
        }
    }

    fn exit_copy_mode(&mut self) {
        if let Some(c) = self.copy.take()
            && let Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) = self.panes.get_mut(&c.pane)
        {
            vt.scroll_to_bottom();
        }
        self.dirty = true;
    }

    /// Copy mode follows the focused pane; leave it when focus moves away.
    fn after_focus_change(&mut self) {
        if let Some(c) = &self.copy
            && Some(c.pane) != self.focused()
        {
            self.exit_copy_mode();
        }
    }

    // ----- client messages --------------------------------------------

    /// Makes `client` the one being served: its UI state moves into `ui`.
    /// Returns `false` for an unknown client.
    fn enter(&mut self, client: ClientId) -> bool {
        if self.current == Some(client) {
            return true;
        }
        self.leave();
        match self.clients.remove(&client) {
            Some(ui) => {
                self.ui = ui;
                self.current = Some(client);
                true
            }
            None => false,
        }
    }

    /// Puts the served client's UI state back.
    fn leave(&mut self) {
        if let Some(client) = self.current.take() {
            let ui = std::mem::take(&mut self.ui);
            self.clients.insert(client, ui);
        }
    }

    /// Detaches a client (it disconnected or was detached).
    pub fn detach_client(&mut self, client: ClientId) {
        if self.current == Some(client) {
            self.current = None;
            self.ui = ClientUi::default();
        }
        self.clients.remove(&client);
        if self.last_active == Some(client) {
            self.last_active = self.clients.keys().next_back().copied();
        }
        self.recompute_size();
    }

    /// Rebuilds every client's theme (after a config change or a pick).
    fn refresh_themes(&mut self) {
        let cfg = &self.config.theme;
        for ui in self.clients.values_mut() {
            ui.theme = cfg.theme(ui.theme_pick, ui.caps.background);
        }
        if self.current.is_some() {
            self.ui.theme = cfg.theme(self.ui.theme_pick, self.ui.caps.background);
        }
        self.dirty = true;
    }

    /// Gives the panes the default colours and cell size of the most
    /// recently active client's terminal, so programs that ask (OSC 10/11,
    /// pixel sizes) get them and images in panes are sized to match.
    fn update_term_colors(&mut self) {
        let caps = self
            .last_active
            .and_then(|c| self.ui_of(c))
            .map(|u| &u.caps);
        let colors = default_term_colors(caps.and_then(|c| c.background));
        let cell_px = caps.and_then(|c| c.cell_px);
        if colors == self.term_colors && cell_px == self.cell_px {
            return;
        }
        let new_cell = cell_px != self.cell_px;
        self.term_colors = colors;
        self.cell_px = cell_px;
        for pane in self.panes.values_mut() {
            if let PaneKind::Term { vt, pty } = &mut pane.kind {
                vt.set_default_colors(colors.0, colors.1);
                vt.set_cell_px(cell_px);
                if new_cell {
                    let (cols, rows) = vt.size();
                    let _ = pty.resize(cols, rows, cell_px);
                }
            }
        }
    }

    /// The session size under the `window-size` policy (and the panes'
    /// default colours, which follow the same client).
    fn recompute_size(&mut self) {
        self.update_term_colors();
        let sizes: Vec<(u16, u16)> = self
            .clients
            .values()
            .chain(self.current.map(|_| &self.ui))
            .map(|u| (u.cols, u.rows))
            .collect();
        if sizes.is_empty() {
            return;
        }
        let (cols, rows) = match self.config.session.window_size {
            WindowSize::Latest => self
                .last_active
                .and_then(|c| self.ui_of(c))
                .map_or(sizes[sizes.len() - 1], |u| (u.cols, u.rows)),
            WindowSize::Smallest => (
                sizes.iter().map(|s| s.0).min().unwrap_or(80),
                sizes.iter().map(|s| s.1).min().unwrap_or(24),
            ),
            WindowSize::Largest => (
                sizes.iter().map(|s| s.0).max().unwrap_or(80),
                sizes.iter().map(|s| s.1).max().unwrap_or(24),
            ),
        };
        if (cols.max(2), rows.max(2)) != (self.cols, self.rows) {
            self.resize(cols, rows);
        }
    }

    /// Handles a message from `client`. A `Hello` attaches it.
    pub fn handle_client(&mut self, client: ClientId, msg: ClientMsg) {
        if let ClientMsg::Hello {
            caps, cols, rows, ..
        } = &msg
        {
            self.leave();
            let ui = self.clients.entry(client).or_default();
            ui.caps = caps.clone();
            ui.theme = self.config.theme.theme(ui.theme_pick, caps.background);
            ui.cols = (*cols).max(2);
            ui.rows = (*rows).max(2);
            // A fresh client must not inherit a stale title.
            ui.title_sent.clear();
            self.last_active = Some(client);
            self.recompute_size();
            self.dirty = true;
            return;
        }
        if !self.enter(client) {
            return;
        }
        if matches!(
            msg,
            ClientMsg::Key(_)
                | ClientMsg::Mouse(_)
                | ClientMsg::Paste(_)
                | ClientMsg::Resize { .. }
        ) && self.last_active != Some(client)
        {
            self.last_active = Some(client);
            self.recompute_size();
        }
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Resize { cols, rows } => {
                self.ui.cols = cols.max(2);
                self.ui.rows = rows.max(2);
                self.recompute_size();
                self.dirty = true;
            }
            ClientMsg::Key(k) => self.on_key(k),
            ClientMsg::Mouse(m) => {
                if self.config.mouse {
                    self.on_mouse(m);
                }
            }
            ClientMsg::Paste(text) => self.on_paste(&text),
            ClientMsg::Focus(focused) => {
                if let Some(id) = self.focused()
                    && let Some(Pane {
                        kind: PaneKind::Term { vt, pty },
                        ..
                    }) = self.panes.get(&id)
                    && let Some(bytes) = encode_focus(focused, &vt.modes())
                {
                    pty.write(bytes);
                }
            }
        }
        self.after_focus_change();
        self.leave();
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(2);
        self.rows = rows.max(2);
        // Floats keep their position but stay on screen.
        let area = self.layout_area();
        for ws in &mut self.session.workspaces {
            let floats: Vec<(PaneId, Rect)> = ws.floats.iter().map(|f| (f.pane, f.rect)).collect();
            for (p, r) in floats {
                ws.set_float_rect(p, r, area);
            }
        }
        self.dirty = true;
    }

    fn on_paste(&mut self, text: &str) {
        if let Some(p) = &mut self.ui.prompt {
            p.insert_str(text);
        } else if let Some(p) = &mut self.ui.palette {
            p.insert_str(text);
        } else if let Some(id) = self.focused() {
            let bytes = match self.panes.get(&id) {
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) => Some(encode_paste(text, &vt.modes())),
                _ => None,
            };
            if let Some(b) = bytes {
                self.write_to_pane(id, b);
            }
        }
        self.dirty = true;
    }

    // ----- keys -------------------------------------------------------

    fn on_key(&mut self, k: KeyEvent) {
        let now = Instant::now();
        if let Some(prompt) = &mut self.ui.prompt {
            let action = prompt.handle_key(k);
            self.dirty = true;
            match action {
                PromptAction::None => {}
                PromptAction::Cancel => {
                    let kind = self.ui.prompt.take().map(|p| p.kind);
                    if kind == Some(PromptKind::Confirm(Confirmed::Restore)) {
                        self.requests.push(SessionRequest::RestoreDeclined);
                    }
                }
                PromptAction::Submit(text) => {
                    if let Some(p) = self.ui.prompt.take() {
                        self.finish_prompt(p.kind, text);
                    }
                }
            }
            return;
        }
        if let Some(palette) = &mut self.ui.palette {
            let action = palette.handle_key(k);
            self.dirty = true;
            match action {
                PaletteAction::None => {}
                PaletteAction::Close => self.ui.palette = None,
                PaletteAction::Run(cmd) => {
                    self.ui.palette = None;
                    match cmd.parse::<Command>() {
                        Ok(c) => self.run(c),
                        Err(_) if self.plugin_commands.iter().any(|c| c.name == cmd) => {
                            self.requests
                                .push(SessionRequest::PluginCommand { name: cmd });
                        }
                        Err(e) => self.notify(Level::Error, e.to_string()),
                    }
                }
            }
            return;
        }
        if self.ui.whichkey_full {
            self.ui.whichkey_full = false;
            self.dirty = true;
            return;
        }

        match self.ui.key_state {
            KeyState::Prefix => {
                self.ui.key_state = KeyState::Normal;
                self.ui.whichkey_deadline = None;
                self.ui.whichkey_visible = false;
                self.dirty = true;
                if let Some(b) = self.keys.lookup_prefix(k) {
                    if b.repeat {
                        self.ui.key_state = KeyState::Repeat(
                            now + Duration::from_millis(self.config.repeat_time_ms),
                        );
                    }
                    self.run(b.command);
                }
                return;
            }
            KeyState::Repeat(until) => {
                if now < until
                    && let Some(b) = self.keys.lookup_prefix(k).filter(|b| b.repeat)
                {
                    self.ui.key_state =
                        KeyState::Repeat(now + Duration::from_millis(self.config.repeat_time_ms));
                    self.run(b.command);
                    return;
                }
                self.ui.key_state = KeyState::Normal;
            }
            KeyState::Normal => {}
        }

        if k == self.keys.prefix_key {
            self.ui.key_state = KeyState::Prefix;
            if self.config.whichkey.enabled {
                self.ui.whichkey_deadline =
                    Some(now + Duration::from_millis(self.config.whichkey.delay_ms));
            }
            self.dirty = true;
            return;
        }
        if let Some(b) = self.keys.lookup_root(k) {
            self.run(b.command);
            return;
        }
        if self.copy.is_some() {
            self.copy_key(k);
            return;
        }
        let Some(id) = self.focused() else { return };
        let effect = match self.panes.get_mut(&id) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => {
                let bytes = encode_key(k, &vt.modes());
                self.write_to_pane(id, bytes);
                None
            }
            Some(Pane {
                kind: PaneKind::App(app),
                ..
            }) => Some(app.handle_key(k)),
            None => None,
        };
        if let Some(e) = effect {
            self.app_effect(id, e);
        }
    }

    fn copy_key(&mut self, k: KeyEvent) {
        let Some(mut cm) = self.copy.take() else {
            return;
        };
        let result = match self.panes.get_mut(&cm.pane) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => Some(cm.handle_key(k, vt.as_mut())),
            _ => None,
        };
        self.copy = Some(cm);
        self.dirty = true;
        match result {
            Some((CopyAction::None, failed)) => {
                if failed {
                    self.notify(Level::Warn, "search: no match");
                }
            }
            Some((CopyAction::Exit, _)) | None => self.exit_copy_mode(),
            Some((CopyAction::Copy(text), _)) => {
                self.exit_copy_mode();
                self.copy_text(text, ClipSource::Selection);
            }
        }
    }

    fn app_effect(&mut self, id: PaneId, effect: AppEffect) {
        match effect {
            AppEffect::None => {}
            AppEffect::Redraw => self.dirty = true,
            AppEffect::Close => self.close_pane(id),
            AppEffect::Notify(text) => self.notify(Level::Info, text),
            AppEffect::Command(text) => match text.parse::<Command>() {
                Ok(c) => self.run(c),
                Err(e) => self.notify(Level::Error, e.to_string()),
            },
            AppEffect::Copy(text) => {
                let title = self
                    .panes
                    .get(&id)
                    .map(Pane::display_name)
                    .unwrap_or_default();
                self.copy_text(text, ClipSource::App(title));
            }
            AppEffect::PasteAndClose(text) => {
                self.close_pane(id);
                self.on_paste(&text);
            }
        }
    }

    fn finish_prompt(&mut self, kind: PromptKind, text: String) {
        match kind {
            PromptKind::RenameWorkspace(idx) => {
                if let Some(ws) = self.session.workspaces.get_mut(idx) {
                    if text.is_empty() {
                        ws.auto_name = true;
                    } else {
                        ws.name = text;
                        ws.auto_name = false;
                    }
                }
            }
            PromptKind::RenameSession => {
                if !text.is_empty() {
                    self.requests.push(SessionRequest::Rename {
                        from: self.session.name.clone(),
                        to: text,
                    });
                }
            }
            PromptKind::Confirm(Confirmed::KillPane(id)) => self.close_pane(id),
            PromptKind::Confirm(Confirmed::KillWorkspace(idx)) => {
                let panes = self
                    .session
                    .workspaces
                    .get(idx)
                    .map(Workspace::panes)
                    .unwrap_or_default();
                for p in panes {
                    self.close_pane(p);
                }
            }
            PromptKind::Confirm(Confirmed::Quit) => {
                let ids: Vec<PaneId> = self.panes.keys().copied().collect();
                for id in ids {
                    if let Some(Pane {
                        kind: PaneKind::Term { pty, .. },
                        ..
                    }) = self.panes.get_mut(&id)
                    {
                        pty.kill();
                    }
                }
                self.quit = Some("quit".into());
            }
            PromptKind::Confirm(Confirmed::Restore) => {
                self.requests.push(SessionRequest::RestoreSaved {
                    from_question: true,
                });
            }
        }
        self.dirty = true;
    }

    // ----- commands ---------------------------------------------------

    /// Runs a command on behalf of `client` (or of no client, e.g. from the
    /// control API: commands that need a client, like detach or prompts,
    /// then have no visible effect).
    pub fn run_as(&mut self, client: Option<ClientId>, cmd: Command) {
        match client {
            Some(c) if self.enter(c) => {
                self.run(cmd);
                self.leave();
            }
            _ => {
                self.leave();
                self.run(cmd);
                // Prompts opened without a client have nobody to answer.
                self.ui = ClientUi::default();
            }
        }
    }

    fn target_pane(&self, pane: Option<PaneId>) -> Result<PaneId, String> {
        match pane {
            Some(p) if self.panes.contains_key(&p) => Ok(p),
            Some(p) => Err(format!("no pane {p} in session {}", self.session.name)),
            None => self
                .focused()
                .ok_or_else(|| format!("session {} has no focused pane", self.session.name)),
        }
    }

    /// Types `text` into a pane (default: the focused one), like pasting
    /// without bracketing.
    pub fn send_text(&mut self, pane: Option<PaneId>, text: &str) -> Result<(), String> {
        let id = self.target_pane(pane)?;
        match self.panes.get(&id).map(|p| &p.kind) {
            Some(PaneKind::Term { .. }) => {
                self.write_to_pane(id, text.as_bytes().to_vec());
                Ok(())
            }
            _ => Err(format!("pane {id} is not a terminal")),
        }
    }

    /// Sends keys (tmux notation) to a pane, encoded for its program.
    pub fn send_keys(&mut self, pane: Option<PaneId>, keys: &[KeyEvent]) -> Result<(), String> {
        let id = self.target_pane(pane)?;
        let modes = match self.panes.get(&id).map(|p| &p.kind) {
            Some(PaneKind::Term { vt, .. }) => vt.modes(),
            _ => return Err(format!("pane {id} is not a terminal")),
        };
        let bytes: Vec<u8> = keys.iter().flat_map(|k| encode_key(*k, &modes)).collect();
        self.write_to_pane(id, bytes);
        Ok(())
    }

    /// Closes a pane without asking.
    pub fn kill_pane(&mut self, id: PaneId) -> bool {
        if !self.panes.contains_key(&id) {
            return false;
        }
        self.close_pane(id);
        true
    }

    /// Ends the session: every program is terminated.
    pub fn end(&mut self, reason: &str) {
        for pane in self.panes.values_mut() {
            if let PaneKind::Term { pty, .. } = &mut pane.kind {
                pty.kill();
            }
        }
        self.panes.clear();
        self.quit = Some(reason.to_owned());
    }

    /// Facts about every pane, for `pane.list`.
    pub fn pane_infos(&self) -> Vec<PaneInfo> {
        let focus = self.focused();
        let mut out = Vec::new();
        for (wi, ws) in self.session.workspaces.iter().enumerate() {
            for id in ws.panes() {
                let Some(p) = self.panes.get(&id) else {
                    continue;
                };
                let (kind, cwd) = match &p.kind {
                    PaneKind::Term { pty, .. } => {
                        ("terminal", p.cwd.clone().or_else(|| pty.foreground_cwd()))
                    }
                    PaneKind::App(_) => ("app", None),
                };
                out.push(PaneInfo {
                    id: id.0,
                    workspace: wi,
                    name: p.display_name(),
                    title: p.title.clone(),
                    kind: kind.into(),
                    cwd: cwd.map(|c| c.display().to_string()),
                    cols: p.size.0,
                    rows: p.size.1,
                    floating: ws.is_floating(id),
                    focused: Some(id) == focus && wi == self.session.active_index(),
                });
            }
        }
        out
    }

    /// Runs a command (from a key binding, the palette or an app).
    pub fn run(&mut self, cmd: Command) {
        let area = self.layout_area();
        self.dirty = true;
        self.layout_changed = true;
        match cmd {
            Command::Split(how) => {
                let cwd = self.focused_cwd();
                let estimate = (area.w / 2, area.h / 2);
                match self.spawn_term(cwd, estimate) {
                    Ok(id) => {
                        if !self.session.active_mut().add_split(id, how, area) {
                            self.close_pane(id);
                            self.notify(Level::Warn, "no space for new pane");
                        }
                    }
                    Err(e) => self.notify(Level::Error, format!("cannot start shell: {e}")),
                }
            }
            Command::NewFloat => {
                let cwd = self.focused_cwd();
                match self.spawn_term(cwd, (area.w * 7 / 10, area.h * 7 / 10)) {
                    Ok(id) => {
                        self.session.active_mut().add_float(id, area);
                        self.animate(anim::Kind::FloatOpen(id), Instant::now());
                    }
                    Err(e) => self.notify(Level::Error, format!("cannot start shell: {e}")),
                }
            }
            Command::KillPane => {
                if let Some(id) = self.focused() {
                    match self.panes.get(&id).map(|p| &p.kind) {
                        Some(PaneKind::App(_)) => self.close_pane(id),
                        Some(PaneKind::Term { .. }) => {
                            self.ui.prompt = Some(Prompt::confirm(
                                Confirmed::KillPane(id),
                                format!("kill-pane {id}"),
                            ));
                        }
                        None => {}
                    }
                }
            }
            Command::Focus(d) => {
                self.session.active_mut().focus_direction(d, area);
            }
            Command::FocusNext => self.session.active_mut().focus_cycle(true),
            Command::FocusPrev => self.session.active_mut().focus_cycle(false),
            Command::FocusLast => {
                if let Some(last) = self.session.active().last_focus() {
                    self.session.active_mut().set_focus(last);
                }
            }
            Command::SwapNext => self.session.active_mut().swap_cycle(true),
            Command::SwapPrev => self.session.active_mut().swap_cycle(false),
            Command::Resize(d, n) => self.session.active_mut().resize_focused(d, n, area),
            Command::Zoom => self.session.active_mut().toggle_zoom(),
            Command::ToggleFloat => {
                if let Some(id) = self.focused() {
                    if !self.session.active_mut().toggle_float(id, area) {
                        self.notify(Level::Warn, "no space to tile this window");
                    } else if self.session.active().is_floating(id) {
                        self.animate(anim::Kind::FloatOpen(id), Instant::now());
                    }
                }
            }
            Command::Minimize => match self.focused() {
                Some(id) if self.session.active().is_floating(id) => {
                    self.session.active_mut().toggle_minimize(id);
                }
                _ => self.notify(
                    Level::Info,
                    "only floating windows can be minimized (prefix f floats a pane)",
                ),
            },
            Command::RestoreMinimized => {
                let last = self.session.active().minimized().last().copied();
                match last {
                    Some(id) => self.session.active_mut().toggle_minimize(id),
                    None => self.notify(Level::Info, "no minimized windows"),
                }
            }
            Command::SnapFloat(snap) => match self.focused() {
                Some(id) if self.session.active().is_floating(id) => {
                    self.session.active_mut().snap(id, snap, area);
                }
                _ => self.notify(Level::Info, "only floating windows can be snapped"),
            },
            Command::LayoutNext => match self.session.active_mut().next_preset(area) {
                Some(p) => self.notify(Level::Info, format!("layout: {}", p.name())),
                None => self.notify(Level::Warn, "no layout fits"),
            },
            Command::LayoutSelect(p) => {
                if !self.session.active_mut().select_preset(p, area) {
                    self.notify(Level::Warn, format!("layout {} does not fit", p.name()));
                }
            }
            Command::WorkspaceNew => {
                let cwd = self.focused_cwd();
                match self.spawn_term(cwd, (area.w, area.h)) {
                    Ok(id) => {
                        self.session.add(Workspace::new("shell", id));
                    }
                    Err(e) => self.notify(Level::Error, format!("cannot start shell: {e}")),
                }
            }
            Command::WorkspaceNext => self.session.next(),
            Command::WorkspacePrev => self.session.prev(),
            Command::WorkspaceLast => {
                if !self.session.select_last() {
                    self.notify(Level::Info, "no previous workspace");
                }
            }
            Command::WorkspaceSelect(n) => {
                if !self.session.select(n) {
                    self.notify(Level::Info, format!("no workspace {n}"));
                }
            }
            Command::WorkspaceRename => {
                let idx = self.session.active_index();
                let name = self.session.active().name.clone();
                self.ui.prompt = Some(Prompt::rename(idx, &name));
            }
            Command::WorkspaceKill => {
                let idx = self.session.active_index();
                self.ui.prompt = Some(Prompt::confirm(
                    Confirmed::KillWorkspace(idx),
                    format!("kill-workspace {idx}"),
                ));
            }
            Command::CopyMode | Command::CopyModePageUp => {
                self.enter_copy_mode(false);
                if cmd == Command::CopyModePageUp {
                    self.copy_key(KeyEvent::plain(termos_core::input::KeyCode::PageUp));
                }
            }
            Command::Paste => match self.paste_buffer.clone() {
                Some(text) => self.on_paste(&text),
                None => self.notify(Level::Info, "the paste buffer is empty"),
            },
            Command::Palette => self.open_palette(),
            Command::Keys => {
                self.ui.whichkey_full = true;
            }
            Command::Notifications => {
                let app = NotificationCenter::new(self.notes.history());
                self.open_app(Box::new(app));
            }
            Command::Tutorial => {
                let app = Tutorial::new(&self.keys.prefix_key.to_string());
                self.open_app(Box::new(app));
            }
            Command::ReloadConfig => self.reload_config(),
            Command::ImageOpen(path) => {
                let path = self.resolve_path(&path);
                self.open_app(Box::new(termos_apps::ImageViewer::open(path)));
            }
            Command::MarkdownOpen(path) => {
                let path = self.resolve_path(&path);
                self.open_app(Box::new(termos_apps::MarkdownViewer::open(path)));
            }
            Command::FilesOpen(path) => {
                let dir = if path.is_empty() {
                    self.focused_cwd()
                        .or_else(home_dir)
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_default()
                } else {
                    self.resolve_path(&path)
                };
                self.open_app(Box::new(termos_apps::FileManager::open(dir)));
            }
            Command::FileOpen(path) => {
                use termos_apps::files::{OpenKind, open_kind};
                let path = self.resolve_path(&path);
                let shown = path.display().to_string();
                match open_kind(&path) {
                    OpenKind::Dir => self.run(Command::FilesOpen(shown)),
                    OpenKind::Image => self.run(Command::ImageOpen(shown)),
                    OpenKind::Markdown => self.run(Command::MarkdownOpen(shown)),
                    OpenKind::Other => self.run(Command::FileEdit(shown)),
                }
            }
            Command::FileEdit(path) => {
                let path = self.resolve_path(&path);
                let (program, mut args) = editor_command(|k| std::env::var(k).ok());
                args.push(path.display().to_string());
                let cwd = path.parent().map(PathBuf::from);
                let estimate = (area.w / 2, area.h);
                match self.spawn_pane(cwd, estimate, Some((program.clone(), args))) {
                    Ok(id) => {
                        if !self
                            .session
                            .active_mut()
                            .add_split(id, SplitHow::Auto, area)
                        {
                            self.close_pane(id);
                            self.notify(Level::Warn, "no space for new pane");
                        }
                    }
                    Err(e) => self.notify(Level::Error, format!("cannot start {program}: {e}")),
                }
            }
            Command::ThemeSelect(base) => {
                // The client that asked (the control API acts as the
                // session's newest client); without one, every client.
                match self.current {
                    Some(_) => self.ui.theme_pick = Some(base),
                    None => {
                        for ui in self.clients.values_mut() {
                            ui.theme_pick = Some(base);
                        }
                    }
                }
                self.refresh_themes();
                self.notify(Level::Info, format!("theme: {base}"));
            }
            Command::SendPrefix => {
                if let Some(id) = self.focused() {
                    let modes = match self.panes.get(&id) {
                        Some(Pane {
                            kind: PaneKind::Term { vt, .. },
                            ..
                        }) => Some(vt.modes()),
                        _ => None,
                    };
                    if let Some(m) = modes {
                        self.write_to_pane(id, encode_key(self.keys.prefix_key, &m));
                    }
                }
            }
            Command::Detach => {
                let session = self.session.name.clone();
                if self.current.is_some() {
                    self.outbox
                        .push((self.current, ServerMsg::Detached { session }));
                }
            }
            Command::SessionNew => {
                if let Some(client) = self.current {
                    self.requests.push(SessionRequest::New { client });
                }
            }
            Command::SessionNext | Command::SessionPrev => {
                if let Some(client) = self.current {
                    self.requests.push(SessionRequest::Cycle {
                        client,
                        forward: cmd == Command::SessionNext,
                    });
                }
            }
            Command::SessionSwitch(to) => {
                if let Some(client) = self.current {
                    self.requests.push(SessionRequest::Switch { client, to });
                }
            }
            Command::SessionRename => {
                let name = self.session.name.clone();
                self.ui.prompt = Some(Prompt::rename_session(&name));
            }
            Command::SessionRestore => {
                self.requests.push(SessionRequest::RestoreSaved {
                    from_question: false,
                });
            }
            Command::Quit => {
                self.ui.prompt = Some(Prompt::confirm(
                    Confirmed::Quit,
                    "close this session and all its panes",
                ));
            }
        }
        self.after_focus_change();
    }

    fn enter_copy_mode(&mut self, from_wheel: bool) -> bool {
        let Some(id) = self.focused() else {
            return false;
        };
        if self.copy.as_ref().is_some_and(|c| c.pane == id) {
            return true;
        }
        match self.panes.get(&id) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => {
                let mut cm = CopyMode::new(id, vt.as_ref());
                cm.exit_at_bottom = from_wheel;
                self.copy = Some(cm);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn open_palette(&mut self) {
        let mut items: Vec<PaletteItem> = Vec::new();
        for c in COMMANDS {
            let text = match c.arg {
                Some(a) => format!("{} {a}", c.name),
                None => c.name.to_owned(),
            };
            let Ok(cmd) = text.parse::<Command>() else {
                continue;
            };
            match cmd {
                Command::LayoutSelect(_)
                | Command::WorkspaceSelect(_)
                | Command::SessionSwitch(_)
                | Command::ThemeSelect(_)
                | Command::ImageOpen(_)
                | Command::MarkdownOpen(_)
                | Command::FileOpen(_)
                | Command::FileEdit(_) => continue,
                _ => {}
            }
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: self.keys.key_for(&cmd).unwrap_or_default(),
            });
        }
        for p in termos_wm::layout::Preset::ALL {
            let cmd = Command::LayoutSelect(p);
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: self.keys.key_for(&cmd).unwrap_or_default(),
            });
        }
        for b in ThemeBase::ALL {
            let cmd = Command::ThemeSelect(b);
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: self.keys.key_for(&cmd).unwrap_or_default(),
            });
        }
        for (i, ws) in self.session.workspaces.iter().enumerate() {
            let cmd = Command::WorkspaceSelect(i);
            items.push(PaletteItem {
                label: format!("Go to workspace {i}: {}", ws.name),
                command: cmd.to_string(),
                hint: self.keys.key_for(&cmd).unwrap_or_default(),
            });
        }
        for name in self
            .known_sessions
            .iter()
            .filter(|n| **n != self.session.name)
        {
            let cmd = Command::SessionSwitch(name.clone());
            items.push(PaletteItem {
                label: format!("Switch to session {name}"),
                command: cmd.to_string(),
                hint: String::new(),
            });
        }
        for c in &self.plugin_commands {
            items.push(PaletteItem {
                label: format!("{} ({})", c.description, c.plugin),
                command: c.name.clone(),
                hint: String::new(),
            });
        }
        self.ui.palette = Some(Palette::new(items));
        self.dirty = true;
    }

    /// Re-reads config.toml and applies it; a broken file keeps the
    /// current settings and reports where the problem is.
    pub fn reload_config(&mut self) {
        match Config::load(&self.opts.config_path) {
            Ok(config) => {
                let (keys, errors) = KeyTables::from_config(&config);
                self.keys = keys;
                if config.status.clock && self.clock_deadline.is_none() {
                    self.clock_deadline = Some(next_minute(Instant::now()));
                }
                if !config.status.clock {
                    self.clock_deadline = None;
                }
                if let Ok(mut h) = self.opts.clipboard.lock() {
                    h.set_limit(config.clipboard.history);
                }
                self.config = config;
                self.refresh_themes();
                self.ui.key_state = KeyState::Normal;
                for ui in self.clients.values_mut() {
                    ui.key_state = KeyState::Normal;
                }
                self.recompute_size();
                if errors.is_empty() {
                    self.notify(Level::Info, "config reloaded");
                } else {
                    for e in errors {
                        self.notify(Level::Warn, e);
                    }
                }
            }
            Err(e) => self.notify(Level::Error, format!("config not reloaded: {e}")),
        }
        self.dirty = true;
    }

    // ----- mouse ------------------------------------------------------

    fn pane_wants_mouse(&self, id: PaneId, m: &MouseEvent) -> bool {
        if m.mods.contains(Mods::SHIFT) {
            return false;
        }
        matches!(self.panes.get(&id), Some(Pane { kind: PaneKind::Term { vt, .. }, .. }) if vt.modes().mouse_report)
    }

    fn forward_mouse(&mut self, id: PaneId, m: MouseEvent) {
        let Some(rect) = self.placement().content_rect(id) else {
            return;
        };
        let rel = MouseEvent {
            x: m.x.saturating_sub(rect.x),
            y: m.y.saturating_sub(rect.y),
            ..m
        };
        match self.panes.get_mut(&id) {
            Some(Pane {
                kind: PaneKind::Term { vt, pty },
                ..
            }) => {
                if let Some(bytes) = encode_mouse(rel, &vt.modes()) {
                    pty.write(bytes);
                }
            }
            Some(Pane {
                kind: PaneKind::App(app),
                ..
            }) => {
                let e = app.handle_mouse(rel);
                self.app_effect(id, e);
            }
            None => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let area = self.layout_area();
        self.dirty = true;
        // Status line.
        if m.y == self.status_y() && self.ui.drag.is_none() {
            if let MouseKind::Down(MouseButton::Left) = m.kind {
                if let Some(idx) = self.ui.status_regions.tab_at(m.x, m.y) {
                    self.session.select(idx);
                } else if let Some(p) = self.ui.status_regions.minimized_at(m.x, m.y) {
                    self.session.active_mut().toggle_minimize(p);
                }
            }
            self.after_focus_change();
            return;
        }
        match m.kind {
            MouseKind::Down(button) => self.mouse_down(m, button, area),
            MouseKind::Drag(button) => self.mouse_drag(m, button, area),
            MouseKind::Up(button) => self.mouse_up(m, button, area),
            MouseKind::ScrollUp | MouseKind::ScrollDown => self.mouse_wheel(m),
            MouseKind::Moved | MouseKind::ScrollLeft | MouseKind::ScrollRight => {
                if let Hit::Pane { pane, .. } = self.placement().hit(m.x, m.y)
                    && self.pane_wants_mouse(pane, &m)
                {
                    self.forward_mouse(pane, m);
                }
            }
        }
        self.after_focus_change();
    }

    fn mouse_down(&mut self, m: MouseEvent, button: MouseButton, area: Rect) {
        let hit = self.placement().hit(m.x, m.y);
        if button != MouseButton::Left {
            if let Hit::Pane { pane, .. } = hit {
                if self.pane_wants_mouse(pane, &m) {
                    self.ui.mouse_owner = Some(pane);
                    self.forward_mouse(pane, m);
                } else if button == MouseButton::Middle {
                    self.session.active_mut().set_focus(pane);
                    self.run(Command::Paste);
                }
            }
            return;
        }
        match hit {
            Hit::Nothing => {}
            Hit::FloatButton(pane, Button::Close) => {
                self.session.active_mut().set_focus(pane);
                self.run(Command::KillPane);
            }
            Hit::FloatButton(pane, Button::Minimize) => {
                self.session.active_mut().toggle_minimize(pane)
            }
            Hit::FloatTitle(pane) => {
                self.session.active_mut().set_focus(pane);
                if let Some(r) = self.session.active().float_rect(pane) {
                    self.ui.drag = Some(Drag::Move {
                        pane,
                        dx: m.x.saturating_sub(r.x),
                        dy: m.y.saturating_sub(r.y),
                    });
                }
            }
            Hit::FloatEdge(pane, edge) => {
                self.session.active_mut().set_focus(pane);
                if let Some(start) = self.session.active().float_rect(pane) {
                    self.ui.drag = Some(Drag::Resize {
                        pane,
                        edge,
                        start,
                        x0: m.x,
                        y0: m.y,
                    });
                }
            }
            Hit::Separator(sep) => {
                self.ui.drag = Some(Drag::Separator {
                    path: sep.path,
                    index: sep.index,
                    dir: sep.dir,
                });
            }
            Hit::Pane { pane, x, y } => {
                self.session.active_mut().set_focus(pane);
                if self.pane_wants_mouse(pane, &m) {
                    self.ui.mouse_owner = Some(pane);
                    self.forward_mouse(pane, m);
                } else if let Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) = self.panes.get(&pane)
                {
                    let off = i32::try_from(vt.display_offset()).unwrap_or(0);
                    self.ui.drag = Some(Drag::Select {
                        pane,
                        col: x,
                        line: i32::from(y) - off,
                        started: false,
                    });
                } else {
                    self.forward_mouse(pane, m);
                }
            }
        }
        let _ = area;
    }

    fn mouse_drag(&mut self, m: MouseEvent, button: MouseButton, area: Rect) {
        let Some(drag) = self.ui.drag.clone() else {
            if let Some(owner) = self.ui.mouse_owner {
                self.forward_mouse(owner, m);
            }
            return;
        };
        if button != MouseButton::Left {
            return;
        }
        match drag {
            Drag::Move { pane, dx, dy } => {
                if let Some(r) = self.session.active().float_rect(pane) {
                    let x = m.x.saturating_sub(dx);
                    let y = m.y.saturating_sub(dy).max(area.y);
                    self.session
                        .active_mut()
                        .set_float_rect(pane, Rect::new(x, y, r.w, r.h), area);
                }
            }
            Drag::Resize {
                pane,
                edge,
                start,
                x0,
                y0,
            } => {
                let dx = i32::from(m.x) - i32::from(x0);
                let dy = i32::from(m.y) - i32::from(y0);
                let clamp16 = |v: i32| u16::try_from(v.max(0)).unwrap_or(u16::MAX);
                let (mut x, mut w, mut h) =
                    (i32::from(start.x), i32::from(start.w), i32::from(start.h));
                match edge {
                    Edge::Right => w += dx,
                    Edge::Bottom => h += dy,
                    Edge::BottomRight => {
                        w += dx;
                        h += dy;
                    }
                    Edge::Left => {
                        x += dx;
                        w -= dx;
                    }
                    Edge::BottomLeft => {
                        x += dx;
                        w -= dx;
                        h += dy;
                    }
                }
                let r = Rect::new(clamp16(x), start.y, clamp16(w), clamp16(h));
                self.session.active_mut().set_float_rect(pane, r, area);
            }
            Drag::Separator { path, index, dir } => {
                let pos = match dir {
                    Dir::Horizontal => m.x,
                    Dir::Vertical => m.y,
                };
                self.session
                    .active_mut()
                    .layout
                    .set_boundary(&path, index, pos, area);
            }
            Drag::Select {
                pane,
                col,
                line,
                started,
            } => {
                let Some(rect) = self.placement().content_rect(pane) else {
                    return;
                };
                if !started {
                    if !self.enter_copy_mode(false) {
                        return;
                    }
                    if let Some(cm) = &mut self.copy
                        && let Some(Pane {
                            kind: PaneKind::Term { vt, .. },
                            ..
                        }) = self.panes.get(&pane)
                    {
                        cm.move_to(col, line, vt.as_ref());
                        cm.begin_selection(false);
                    }
                    self.ui.drag = Some(Drag::Select {
                        pane,
                        col,
                        line,
                        started: true,
                    });
                }
                if let Some(cm) = &mut self.copy
                    && let Some(Pane {
                        kind: PaneKind::Term { vt, .. },
                        ..
                    }) = self.panes.get(&pane)
                {
                    let off = i32::try_from(vt.display_offset()).unwrap_or(0);
                    let rx = m.x.saturating_sub(rect.x).min(rect.w.saturating_sub(1));
                    let ry = i32::from(m.y.saturating_sub(rect.y).min(rect.h.saturating_sub(1)));
                    cm.move_to(rx, ry - off, vt.as_ref());
                }
            }
        }
    }

    fn mouse_up(&mut self, m: MouseEvent, button: MouseButton, area: Rect) {
        let drag = self.ui.drag.take();
        if matches!(
            drag,
            Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Separator { .. })
        ) {
            self.layout_changed = true;
        }
        if let Some(owner) = self.ui.mouse_owner.take() {
            self.forward_mouse(owner, m);
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        match drag {
            Some(Drag::Move { pane, .. }) => {
                let snap = if m.x <= area.x {
                    Some(Snap::LeftHalf)
                } else if m.x + 1 >= area.right() {
                    Some(Snap::RightHalf)
                } else if m.y <= area.y {
                    Some(Snap::Maximize)
                } else {
                    None
                };
                if let Some(s) = snap {
                    self.session.active_mut().snap(pane, s, area);
                }
            }
            Some(Drag::Select {
                started: true,
                pane,
                ..
            }) => {
                let text = match (&self.copy, self.panes.get(&pane)) {
                    (
                        Some(cm),
                        Some(Pane {
                            kind: PaneKind::Term { vt, .. },
                            ..
                        }),
                    ) => cm.selected_text(vt.as_ref()),
                    _ => None,
                };
                self.exit_copy_mode();
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    self.copy_text(t, ClipSource::Selection);
                }
            }
            _ => {}
        }
    }

    fn mouse_wheel(&mut self, m: MouseEvent) {
        let Hit::Pane { pane, .. } = self.placement().hit(m.x, m.y) else {
            return;
        };
        if self.pane_wants_mouse(pane, &m) {
            self.forward_mouse(pane, m);
            return;
        }
        let up = m.kind == MouseKind::ScrollUp;
        let (alt_scroll, is_term) = match self.panes.get(&pane) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => {
                let modes = vt.modes();
                (modes.alt_screen && modes.alternate_scroll, true)
            }
            _ => (false, false),
        };
        if !is_term {
            self.forward_mouse(pane, m);
            return;
        }
        if alt_scroll {
            // Full-screen programs without mouse support (less, man):
            // the wheel becomes arrow keys.
            let key = KeyEvent::plain(if up {
                termos_core::input::KeyCode::Up
            } else {
                termos_core::input::KeyCode::Down
            });
            let bytes = match self.panes.get(&pane) {
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) => encode_key(key, &vt.modes()),
                _ => Vec::new(),
            };
            self.write_to_pane(pane, bytes.repeat(3));
            return;
        }
        self.session.active_mut().set_focus(pane);
        if up && self.copy.as_ref().is_none_or(|c| c.pane != pane) {
            self.exit_copy_mode();
            self.enter_copy_mode(true);
        }
        let lines = if up { 3 } else { -3 };
        let exit = match (&mut self.copy, self.panes.get_mut(&pane)) {
            (
                Some(cm),
                Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }),
            ) if cm.pane == pane => cm.scroll(lines, vt.as_mut()),
            _ => false,
        };
        if exit {
            self.exit_copy_mode();
        }
    }

    // ----- PTY and timers ---------------------------------------------

    /// Handles output or exit of a pane's program.
    pub fn on_pty(&mut self, event: PtyEvent) {
        match event {
            PtyEvent::Output(id, bytes) => {
                let visible = self.placement().visible().contains(&id);
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let PaneKind::Term { vt, pty } = &mut pane.kind else {
                    return;
                };
                vt.advance(&bytes);
                for ev in vt.take_events() {
                    match ev {
                        VtEvent::Title(t) => pane.title = t,
                        VtEvent::ResetTitle => pane.title.clear(),
                        VtEvent::Bell => {
                            if visible {
                                self.outbox.push((None, ServerMsg::Bell));
                            } else {
                                pane.bell = true;
                                self.dirty = true;
                            }
                        }
                        VtEvent::Reply(bytes) => pty.write(bytes),
                        VtEvent::Clipboard(text) => {
                            let source = ClipSource::Program(pane.title.clone());
                            if let Ok(mut h) = self.opts.clipboard.lock() {
                                h.push(&text, source);
                            }
                            self.outbox.push((None, ServerMsg::Clipboard(text.clone())));
                            self.paste_buffer = Some(text);
                        }
                        VtEvent::Cwd(p) => {
                            pane.cwd = Some(p);
                            self.layout_changed = true;
                        }
                    }
                }
                if visible {
                    self.output_since_render += bytes.len();
                    self.dirty = true;
                }
            }
            PtyEvent::Wake(id) => {
                if self.panes.contains_key(&id) {
                    self.dirty = true;
                }
            }
            PtyEvent::Exited(id, code) => {
                if self.panes.contains_key(&id) {
                    self.close_pane(id);
                    if let Some(c) = code.filter(|c| *c != 0) {
                        self.notify(Level::Warn, format!("pane {id} exited with status {c}"));
                    }
                }
            }
        }
    }

    /// The next moment [`Desktop::on_timer`] must run, if any. `None` while
    /// idle with the clock disabled: no wakeups at all (D-028).
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut t: Vec<Instant> = Vec::new();
        for ui in self.clients.values().chain(self.current.map(|_| &self.ui)) {
            if let Some(d) = ui.whichkey_deadline {
                t.push(d);
            }
            if let KeyState::Repeat(until) = ui.key_state {
                t.push(until);
            }
        }
        if let Some(d) = self.notes.deadline() {
            t.push(d);
        }
        if let Some(d) = self.clock_deadline {
            t.push(d);
        }
        if let Some(d) = self.anim.deadline() {
            t.push(d);
        }
        t.extend(self.app_deadlines().into_iter().map(|(_, d)| d));
        for p in self.panes.values() {
            if let PaneKind::Term { vt, .. } = &p.kind
                && let Some(d) = vt.sync_deadline()
            {
                t.push(d);
            }
        }
        t.into_iter().min()
    }

    /// The deadlines of apps on screen; hidden apps get no timers.
    fn app_deadlines(&self) -> Vec<(PaneId, Instant)> {
        let timed: Vec<(PaneId, Instant)> = self
            .panes
            .iter()
            .filter_map(|(id, p)| match &p.kind {
                PaneKind::App(app) => app.deadline().map(|d| (*id, d)),
                PaneKind::Term { .. } => None,
            })
            .collect();
        if timed.is_empty() {
            return timed;
        }
        let visible = self.placement().visible();
        timed
            .into_iter()
            .filter(|(id, _)| visible.contains(id))
            .collect()
    }

    pub fn on_timer(&mut self, now: Instant) {
        self.leave();
        for (id, d) in self.app_deadlines() {
            if d > now {
                continue;
            }
            let effect = match self.panes.get_mut(&id) {
                Some(Pane {
                    kind: PaneKind::App(app),
                    ..
                }) => app.on_timer(now),
                _ => continue,
            };
            self.app_effect(id, effect);
        }
        for ui in self.clients.values_mut() {
            if ui.whichkey_deadline.is_some_and(|d| d <= now) {
                ui.whichkey_deadline = None;
                if ui.key_state == KeyState::Prefix {
                    ui.whichkey_visible = true;
                    self.dirty = true;
                }
            }
            if let KeyState::Repeat(until) = ui.key_state
                && until <= now
            {
                ui.key_state = KeyState::Normal;
            }
        }
        if self.notes.expire(now) {
            self.dirty = true;
        }
        if self.clock_deadline.is_some_and(|d| d <= now) {
            self.clock_deadline = Some(next_minute(now));
            self.dirty = true;
        }
        if self.anim.is_active() && !self.any_client_animates() {
            // The client that would have seen it left: no frames for
            // nobody.
            self.anim.clear();
        }
        if self.anim.deadline().is_some_and(|d| d <= now) && self.anim.tick(now) {
            self.dirty = true;
        }
        let mut flushed = false;
        for p in self.panes.values_mut() {
            if let PaneKind::Term { vt, .. } = &mut p.kind
                && vt.sync_deadline().is_some_and(|d| d <= now)
            {
                vt.flush_sync();
                flushed = true;
            }
        }
        if flushed {
            self.dirty = true;
        }
    }

    // ----- rendering --------------------------------------------------

    fn refresh_names(&mut self) {
        let names: Vec<Option<String>> = self
            .session
            .workspaces
            .iter()
            .map(|ws| {
                ws.auto_name
                    .then(|| {
                        ws.focus()
                            .and_then(|f| self.panes.get(&f))
                            .map(Pane::display_name)
                    })
                    .flatten()
            })
            .collect();
        for (ws, name) in self.session.workspaces.iter_mut().zip(names) {
            if let Some(n) = name
                && ws.name != n
            {
                ws.name = n;
                // A different program in front: worth a new snapshot.
                self.layout_changed = true;
            }
        }
    }

    fn sync_sizes(&mut self, placement: &Placement) {
        let rects = placement
            .tiled
            .iter()
            .copied()
            .chain(placement.floats.iter().map(|f| (f.pane, f.inner)));
        for (id, rect) in rects {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            let size = (rect.w.max(1), rect.h.max(1));
            if pane.size == size {
                continue;
            }
            pane.size = size;
            if let PaneKind::Term { vt, pty } = &mut pane.kind {
                vt.resize(size.0, size.1);
                // The emulator may be larger than the pane (MIN_COLS); the
                // program must see the emulator's size.
                let (cols, rows) = vt.size();
                if let Err(e) = pty.resize(cols, rows, self.cell_px) {
                    tracing::warn!("resize of pane {id} failed: {e}");
                }
            }
        }
    }

    fn status_info(&self) -> StatusInfo<'_> {
        let active = self.session.active_index();
        let last = self.session.last_index();
        let tabs = self
            .session
            .workspaces
            .iter()
            .enumerate()
            .map(|(i, ws)| TabInfo {
                index: i,
                name: ws.name.clone(),
                active: i == active,
                last: Some(i) == last,
                bell: i != active
                    && ws
                        .panes()
                        .iter()
                        .any(|p| self.panes.get(p).is_some_and(|p| p.bell)),
                zoomed: ws.zoomed().is_some(),
            })
            .collect();
        let minimized = self
            .session
            .active()
            .minimized()
            .into_iter()
            .map(|p| {
                (
                    p,
                    self.panes
                        .get(&p)
                        .map(Pane::display_name)
                        .unwrap_or_default(),
                )
            })
            .collect();
        StatusInfo {
            session: &self.session.name,
            tabs,
            minimized,
            prefix: self.ui.key_state == KeyState::Prefix,
            copy_mode: self.copy.is_some(),
            message: self.notes.toast(),
            message_reveal: None,
            clock: self
                .config
                .status
                .clock
                .then(|| jiff::Zoned::now().strftime("%H:%M").to_string()),
            widgets: &self.plugin_status,
            prompt: self
                .ui
                .prompt
                .as_ref()
                .map(|p| (p.label.as_str(), p.input.as_str())),
        }
    }

    /// Draws the whole screen as `client` sees it into `grid` (at the session
    /// size) and returns the cursor.
    pub fn render(&mut self, client: ClientId, grid: &mut Grid) -> Option<CursorState> {
        self.render_at(client, grid, Instant::now())
    }

    /// [`Desktop::render`] as of `now` (animations are a function of time;
    /// tests pass a fixed moment).
    pub fn render_at(
        &mut self,
        client: ClientId,
        grid: &mut Grid,
        now: Instant,
    ) -> Option<CursorState> {
        if !self.enter(client) {
            return None;
        }
        let cursor = self.render_current(grid, now);
        self.leave();
        cursor
    }

    fn render_current(&mut self, grid: &mut Grid, now: Instant) -> Option<CursorState> {
        if grid.cols() != self.cols || grid.rows() != self.rows {
            grid.resize(self.cols, self.rows);
        }
        grid.clear(Style::new());
        self.output_since_render = 0;
        self.refresh_names();
        let area = self.layout_area();
        let placement = self.placement();
        self.sync_sizes(&placement);
        // Animations change only what this client sees (D-042).
        let animates = Self::animates(&self.config, &self.ui) && self.anim.is_active();
        let mut placement = placement;
        if animates {
            for f in &mut placement.floats {
                if let Some(p) = self.anim.progress(anim::Kind::FloatOpen(f.pane), now) {
                    f.outer = anim::scale(f.outer, 0.5 + 0.5 * p);
                    f.inner = f.outer.inset(1);
                }
            }
        }
        let focus = self.focused();
        // The active workspace's panes have been seen: clear their bells.
        for id in self.session.active().panes() {
            if let Some(p) = self.panes.get_mut(&id) {
                p.bell = false;
            }
        }

        let titles: BTreeMap<PaneId, String> = placement
            .floats
            .iter()
            .filter_map(|f| {
                self.panes
                    .get(&f.pane)
                    .map(|p| (f.pane, p.title_for_border()))
            })
            .collect();
        let opts = ComposeOptions {
            float_border: self.config.floats.border.into(),
            shadow: self.config.floats.shadow,
        };
        // How this client shows images (ARCHITECTURE.md §5.8).
        let protocol = self
            .config
            .graphics
            .protocol
            .forced()
            .unwrap_or_else(|| self.ui.caps.image_protocol());
        let cell = self
            .ui
            .caps
            .cell_px
            .unwrap_or(termos_render::graphics::DEFAULT_CELL);
        let image_bg = self.ui.theme.palette.bg;
        let mut candidates: Vec<Candidate> = Vec::new();
        {
            let panes = &mut self.panes;
            let theme = &self.ui.theme;
            let copy = &self.copy;
            let block_cache = &mut self.block_cache;
            let candidates = &mut candidates;
            compose(
                grid,
                &placement,
                focus,
                theme,
                &opts,
                &|p| titles.get(&p).cloned().unwrap_or_default(),
                &mut |id, rect, g| {
                    let Some(pane) = panes.get_mut(&id) else {
                        return;
                    };
                    match &mut pane.kind {
                        PaneKind::Term { vt, .. } => {
                            vt.render_into(g, rect);
                            if let Some(cm) = copy.as_ref().filter(|c| c.pane == id) {
                                cm.decorate(g, rect, vt.as_ref(), theme);
                            }
                            // Images the program printed, where their
                            // anchors are now.
                            for img in vt.images() {
                                let place = Place {
                                    x: i32::from(rect.x) + i32::from(img.x),
                                    y: i32::from(rect.y) + img.y,
                                    cols: img.cols,
                                    rows: img.rows,
                                };
                                if protocol == ImageProtocol::Blocks {
                                    let cells =
                                        block_cache.get(img.key, &img.picture, place, image_bg);
                                    images::draw_blocks(g, &cells, place, &[rect]);
                                } else {
                                    candidates.push(Candidate {
                                        pane: id,
                                        key: img.key,
                                        picture: img.picture,
                                        place,
                                        clip: rect,
                                    });
                                }
                            }
                        }
                        PaneKind::App(app) => {
                            render_app(app.as_mut(), theme, g, rect);
                            for img in app.images() {
                                // The box may start above the app (a
                                // picture scrolled partly out); only its
                                // width is clipped, so it keeps its size.
                                let x = i32::from(rect.x) + i32::from(img.rect.x);
                                let y = i32::from(rect.y) + i32::from(img.rect.y)
                                    - i32::from(img.scroll_top);
                                let w = img.rect.w.min(rect.w.saturating_sub(img.rect.x));
                                let place = images::fit_place(
                                    x,
                                    y,
                                    w,
                                    img.rect.h,
                                    &img.picture,
                                    cell,
                                    img.upscale,
                                );
                                if place.visible(rect).is_empty() {
                                    continue;
                                }
                                if protocol == ImageProtocol::Blocks {
                                    let cells =
                                        block_cache.get(img.key, &img.picture, place, image_bg);
                                    images::draw_blocks(g, &cells, place, &[rect]);
                                } else {
                                    candidates.push(Candidate {
                                        pane: id,
                                        key: img.key,
                                        picture: img.picture,
                                        place,
                                        clip: rect,
                                    });
                                }
                            }
                        }
                    }
                },
            );
        }

        if animates {
            for (rect, p) in self.anim.closing(now) {
                let r = anim::scale(rect, 1.0 - 0.5 * p);
                grid.draw_box(
                    r,
                    self.config.floats.border.into(),
                    self.ui.theme.float_border,
                    None,
                );
            }
        }

        let status_y = self.status_y();
        let mut info = self.status_info();
        if animates {
            info.message_reveal = self.anim.progress(anim::Kind::Toast, now);
        }
        let (regions, prompt_cursor) = status::draw(grid, status_y, &self.ui.theme, &info);
        self.ui.status_regions = regions;

        let mut overlays: Vec<Rect> = Vec::new();
        let chrome = Chrome::of(&self.config.floats);
        if self.ui.whichkey_visible || self.ui.whichkey_full {
            overlays.extend(whichkey::draw(
                grid,
                area,
                &self.ui.theme,
                &self.keys,
                self.ui.whichkey_full,
                chrome,
            ));
        }
        if let Some(p) = &self.ui.palette {
            overlays.push(p.rect(area));
        }
        let palette_cursor = self
            .ui
            .palette
            .as_ref()
            .map(|p| p.render(grid, area, &self.ui.theme, chrome));
        self.ui.pixel_images = self.place_pixel_images(
            grid, &placement, &overlays, candidates, protocol, image_bg, animates,
        );

        let title = format!("TermOS · {}", self.session.active().name);
        if title != self.ui.title_sent {
            self.outbox
                .push((self.current, ServerMsg::Title(title.clone())));
            self.ui.title_sent = title;
        }

        let bar = |x: u16, y: u16| CursorState {
            x,
            y,
            shape: termos_render::encode::CursorShape::Bar,
            blinking: true,
        };
        if let Some(x) = prompt_cursor {
            return Some(bar(x, status_y));
        }
        if let Some((x, y)) = palette_cursor {
            return Some(bar(x, y));
        }
        if self.ui.whichkey_visible || self.ui.whichkey_full {
            return None;
        }
        let focus = focus?;
        let rect = placement.content_rect(focus)?;
        let pane = self.panes.get(&focus)?;
        let PaneKind::Term { vt, .. } = &pane.kind else {
            return None;
        };
        let (cx, cy, mut state) = if let Some(cm) = self.copy.as_ref().filter(|c| c.pane == focus) {
            let (x, y) = cm.screen_cursor(vt.as_ref())?;
            (
                x,
                y,
                CursorState {
                    x: 0,
                    y: 0,
                    shape: termos_render::encode::CursorShape::Block,
                    blinking: false,
                },
            )
        } else {
            let c = vt.cursor()?;
            (c.x, c.y, c)
        };
        if cx >= rect.w || cy >= rect.h {
            return None;
        }
        state.x = rect.x + cx;
        state.y = rect.y + cy;
        // A tiled pane's cursor must not show through a floating window.
        let covered = !self.session.active().is_floating(focus)
            && placement
                .floats
                .iter()
                .any(|f| f.outer.contains(state.x, state.y));
        (!covered).then_some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_comes_from_visual_then_editor() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            editor_command(env(&[("VISUAL", "code --wait"), ("EDITOR", "vim")])),
            ("code".to_owned(), vec!["--wait".to_owned()])
        );
        assert_eq!(
            editor_command(env(&[("VISUAL", "  "), ("EDITOR", "nano")])),
            ("nano".to_owned(), Vec::new())
        );
        let fallback = if cfg!(windows) { "notepad.exe" } else { "vi" };
        assert_eq!(editor_command(env(&[])).0, fallback);
    }
}
