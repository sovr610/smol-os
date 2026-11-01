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

use termos_core::app::{App, AppEffect, render_app};
use termos_core::config::{Config, StatusPosition};
use termos_core::ids::PaneId;
use termos_core::input::{KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, ServerMsg};
use termos_render::caps::Caps;
use termos_render::cell::Style;
use termos_render::encode::CursorState;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use termos_term::input::{encode_focus, encode_key, encode_mouse, encode_paste};
use termos_term::pty::{PtyEvent, PtyPane, SpawnSpec};
use termos_term::vt::{AlacrittyVt, VtBackend, VtEvent};
use termos_wm::compose::{ComposeOptions, compose};
use termos_wm::hit::{Button, Edge, Hit};
use termos_wm::layout::Dir;
use termos_wm::workspace::{Placement, Session, Snap, Workspace};
use tokio::sync::mpsc;

use crate::apps::{NotificationCenter, Tutorial};
use crate::commands::{COMMANDS, Command};
use crate::copymode::{CopyAction, CopyMode};
use crate::keys::KeyTables;
use crate::notify::{Level, Notifications};
use crate::palette::{Palette, PaletteAction, PaletteItem};
use crate::prompt::{Confirmed, Prompt, PromptAction, PromptKind};
use crate::status::{self, StatusInfo, StatusRegions, TabInfo};
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

/// Startup settings for a desktop.
#[derive(Debug, Clone)]
pub struct DesktopOptions {
    pub session_name: String,
    pub config_path: PathBuf,
    pub paths: Paths,
    /// `$SHELL` from the environment.
    pub env_shell: Option<String>,
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

/// All server-side state.
pub struct Desktop {
    opts: DesktopOptions,
    config: Config,
    theme: Theme,
    keys: KeyTables,
    session: Session,
    panes: BTreeMap<PaneId, Pane>,
    next_id: u64,
    pty_tx: mpsc::Sender<PtyEvent>,
    cols: u16,
    rows: u16,
    caps: Caps,
    key_state: KeyState,
    whichkey_deadline: Option<Instant>,
    whichkey_visible: bool,
    whichkey_full: bool,
    palette: Option<Palette>,
    prompt: Option<Prompt>,
    copy: Option<CopyMode>,
    notes: Notifications,
    paste_buffer: Option<String>,
    drag: Option<Drag>,
    mouse_owner: Option<PaneId>,
    status_regions: StatusRegions,
    clock_deadline: Option<Instant>,
    outbox: Vec<ServerMsg>,
    dirty: bool,
    quit: Option<String>,
    title_sent: String,
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

/// Time until the next wall-clock minute.
fn next_minute(now: Instant) -> Instant {
    let z = jiff::Zoned::now();
    let into = u64::try_from(z.second()).unwrap_or(0);
    let nanos = u64::try_from(z.subsec_nanosecond()).unwrap_or(0);
    let wait = Duration::from_secs(60 - into.min(59)).saturating_sub(Duration::from_nanos(nanos));
    now + wait.max(Duration::from_millis(10))
}

impl Desktop {
    /// Creates the desktop with one workspace running the shell. Shows the
    /// tutorial on first run.
    pub fn new(
        opts: DesktopOptions,
        config: Config,
        pty_tx: mpsc::Sender<PtyEvent>,
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let theme = Theme::from_colors(config.theme.clone());
        let (keys, key_errors) = KeyTables::from_config(&config);
        let placeholder = Session::new(
            opts.session_name.clone(),
            Workspace::new("shell", PaneId(0)),
        );
        let mut d = Self {
            opts,
            theme,
            keys,
            session: placeholder,
            panes: BTreeMap::new(),
            next_id: 0,
            pty_tx,
            cols: cols.max(2),
            rows: rows.max(2),
            caps: Caps::default(),
            key_state: KeyState::Normal,
            whichkey_deadline: None,
            whichkey_visible: false,
            whichkey_full: false,
            palette: None,
            prompt: None,
            copy: None,
            notes: Notifications::new(),
            paste_buffer: None,
            drag: None,
            mouse_owner: None,
            status_regions: StatusRegions::default(),
            clock_deadline: None,
            outbox: Vec::new(),
            dirty: true,
            quit: None,
            title_sent: String::new(),
            output_since_render: 0,
            config,
        };
        let area = d.layout_area();
        let first = d.spawn_term(None, (area.w, area.h))?;
        d.session = Session::new(d.opts.session_name.clone(), Workspace::new("shell", first));
        if d.config.status.clock {
            d.clock_deadline = Some(next_minute(Instant::now()));
        }
        for e in key_errors {
            d.notify(Level::Warn, e);
        }
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

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn keys(&self) -> &KeyTables {
        &self.keys
    }

    pub fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
    }

    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    pub fn prompt_open(&self) -> bool {
        self.prompt.is_some()
    }

    pub fn whichkey_visible(&self) -> bool {
        self.whichkey_visible || self.whichkey_full
    }

    pub fn paste_buffer(&self) -> Option<&str> {
        self.paste_buffer.as_deref()
    }

    pub fn toast(&self) -> Option<&str> {
        self.notes.toast().map(|(t, _)| t)
    }

    /// Takes messages for the client (clipboard, bell, title).
    pub fn take_outbox(&mut self) -> Vec<ServerMsg> {
        std::mem::take(&mut self.outbox)
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
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    fn spawn_term(&mut self, cwd: Option<PathBuf>, size: (u16, u16)) -> io::Result<PaneId> {
        let id = self.alloc_id();
        let (w, h) = (size.0.max(1), size.1.max(1));
        let mut spec =
            SpawnSpec::shell(self.config.shell_program(self.opts.env_shell.clone()), w, h);
        spec.cwd = cwd.filter(|c| c.is_dir());
        spec.env = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERMOS".into(), std::process::id().to_string()),
            ("TERMOS_PANE".into(), id.to_string()),
        ];
        let pty = PtyPane::spawn(id, &spec, self.pty_tx.clone())?;
        let vt: Box<dyn VtBackend> = Box::new(AlacrittyVt::new(w, h, self.config.scrollback));
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
        self.dirty = true;
        id
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
        if self.mouse_owner == Some(id) {
            self.mouse_owner = None;
        }
        self.drag = None;
        self.dirty = true;
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
        self.notes.push(level, text, Instant::now());
        self.dirty = true;
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

    fn copy_text(&mut self, text: String) {
        let n = text.chars().count();
        self.outbox.push(ServerMsg::Clipboard(text.clone()));
        self.paste_buffer = Some(text);
        self.notify(Level::Info, format!("copied {n} characters"));
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

    pub fn handle_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello {
                caps, cols, rows, ..
            } => {
                self.caps = caps;
                self.resize(cols, rows);
            }
            ClientMsg::Resize { cols, rows } => self.resize(cols, rows),
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
        if let Some(p) = &mut self.prompt {
            p.insert_str(text);
        } else if let Some(p) = &mut self.palette {
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
        if let Some(prompt) = &mut self.prompt {
            let action = prompt.handle_key(k);
            self.dirty = true;
            match action {
                PromptAction::None => {}
                PromptAction::Cancel => self.prompt = None,
                PromptAction::Submit(text) => {
                    if let Some(p) = self.prompt.take() {
                        self.finish_prompt(p.kind, text);
                    }
                }
            }
            return;
        }
        if let Some(palette) = &mut self.palette {
            let action = palette.handle_key(k);
            self.dirty = true;
            match action {
                PaletteAction::None => {}
                PaletteAction::Close => self.palette = None,
                PaletteAction::Run(cmd) => {
                    self.palette = None;
                    match cmd.parse::<Command>() {
                        Ok(c) => self.run(c),
                        Err(e) => self.notify(Level::Error, e.to_string()),
                    }
                }
            }
            return;
        }
        if self.whichkey_full {
            self.whichkey_full = false;
            self.dirty = true;
            return;
        }

        match self.key_state {
            KeyState::Prefix => {
                self.key_state = KeyState::Normal;
                self.whichkey_deadline = None;
                self.whichkey_visible = false;
                self.dirty = true;
                if let Some(b) = self.keys.lookup_prefix(k) {
                    if b.repeat {
                        self.key_state = KeyState::Repeat(
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
                    self.key_state =
                        KeyState::Repeat(now + Duration::from_millis(self.config.repeat_time_ms));
                    self.run(b.command);
                    return;
                }
                self.key_state = KeyState::Normal;
            }
            KeyState::Normal => {}
        }

        if k == self.keys.prefix_key {
            self.key_state = KeyState::Prefix;
            if self.config.whichkey.enabled {
                self.whichkey_deadline =
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
                self.copy_text(text);
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
        }
        self.dirty = true;
    }

    // ----- commands ---------------------------------------------------

    /// Runs a command (from a key binding, the palette or an app).
    pub fn run(&mut self, cmd: Command) {
        let area = self.layout_area();
        self.dirty = true;
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
                    Ok(id) => self.session.active_mut().add_float(id, area),
                    Err(e) => self.notify(Level::Error, format!("cannot start shell: {e}")),
                }
            }
            Command::KillPane => {
                if let Some(id) = self.focused() {
                    match self.panes.get(&id).map(|p| &p.kind) {
                        Some(PaneKind::App(_)) => self.close_pane(id),
                        Some(PaneKind::Term { .. }) => {
                            self.prompt = Some(Prompt::confirm(
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
                if let Some(id) = self.focused()
                    && !self.session.active_mut().toggle_float(id, area)
                {
                    self.notify(Level::Warn, "no space to tile this window");
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
                self.prompt = Some(Prompt::rename(idx, &name));
            }
            Command::WorkspaceKill => {
                let idx = self.session.active_index();
                self.prompt = Some(Prompt::confirm(
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
                self.whichkey_full = true;
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
            Command::Quit => {
                self.prompt = Some(Prompt::confirm(
                    Confirmed::Quit,
                    "quit TermOS and close every pane",
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
                Command::LayoutSelect(_) | Command::WorkspaceSelect(_) => continue,
                _ => {}
            }
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: self.keys.key_for(cmd).unwrap_or_default(),
            });
        }
        for p in termos_wm::layout::Preset::ALL {
            let cmd = Command::LayoutSelect(p);
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: self.keys.key_for(cmd).unwrap_or_default(),
            });
        }
        for (i, ws) in self.session.workspaces.iter().enumerate() {
            let cmd = Command::WorkspaceSelect(i);
            items.push(PaletteItem {
                label: format!("Go to workspace {i}: {}", ws.name),
                command: cmd.to_string(),
                hint: self.keys.key_for(cmd).unwrap_or_default(),
            });
        }
        self.palette = Some(Palette::new(items));
        self.dirty = true;
    }

    /// Re-reads config.toml and applies it; a broken file keeps the
    /// current settings and reports where the problem is.
    pub fn reload_config(&mut self) {
        match Config::load(&self.opts.config_path) {
            Ok(config) => {
                let (keys, errors) = KeyTables::from_config(&config);
                self.theme = Theme::from_colors(config.theme.clone());
                self.keys = keys;
                if config.status.clock && self.clock_deadline.is_none() {
                    self.clock_deadline = Some(next_minute(Instant::now()));
                }
                if !config.status.clock {
                    self.clock_deadline = None;
                }
                self.config = config;
                self.key_state = KeyState::Normal;
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
        if m.y == self.status_y() && self.drag.is_none() {
            if let MouseKind::Down(MouseButton::Left) = m.kind {
                if let Some(idx) = self.status_regions.tab_at(m.x, m.y) {
                    self.session.select(idx);
                } else if let Some(p) = self.status_regions.minimized_at(m.x, m.y) {
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
                    self.mouse_owner = Some(pane);
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
                    self.drag = Some(Drag::Move {
                        pane,
                        dx: m.x.saturating_sub(r.x),
                        dy: m.y.saturating_sub(r.y),
                    });
                }
            }
            Hit::FloatEdge(pane, edge) => {
                self.session.active_mut().set_focus(pane);
                if let Some(start) = self.session.active().float_rect(pane) {
                    self.drag = Some(Drag::Resize {
                        pane,
                        edge,
                        start,
                        x0: m.x,
                        y0: m.y,
                    });
                }
            }
            Hit::Separator(sep) => {
                self.drag = Some(Drag::Separator {
                    path: sep.path,
                    index: sep.index,
                    dir: sep.dir,
                });
            }
            Hit::Pane { pane, x, y } => {
                self.session.active_mut().set_focus(pane);
                if self.pane_wants_mouse(pane, &m) {
                    self.mouse_owner = Some(pane);
                    self.forward_mouse(pane, m);
                } else if let Some(Pane {
                    kind: PaneKind::Term { vt, .. },
                    ..
                }) = self.panes.get(&pane)
                {
                    let off = i32::try_from(vt.display_offset()).unwrap_or(0);
                    self.drag = Some(Drag::Select {
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
        let Some(drag) = self.drag.clone() else {
            if let Some(owner) = self.mouse_owner {
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
                    self.drag = Some(Drag::Select {
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
        let drag = self.drag.take();
        if let Some(owner) = self.mouse_owner.take() {
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
                    self.copy_text(t);
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
                                self.outbox.push(ServerMsg::Bell);
                            } else {
                                pane.bell = true;
                                self.dirty = true;
                            }
                        }
                        VtEvent::Reply(bytes) => pty.write(bytes),
                        VtEvent::Clipboard(text) => {
                            self.outbox.push(ServerMsg::Clipboard(text.clone()));
                            self.paste_buffer = Some(text);
                        }
                        VtEvent::Cwd(p) => pane.cwd = Some(p),
                    }
                }
                if visible {
                    self.output_since_render += bytes.len();
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
        if let Some(d) = self.whichkey_deadline {
            t.push(d);
        }
        if let KeyState::Repeat(until) = self.key_state {
            t.push(until);
        }
        if let Some(d) = self.notes.deadline() {
            t.push(d);
        }
        if let Some(d) = self.clock_deadline {
            t.push(d);
        }
        for p in self.panes.values() {
            if let PaneKind::Term { vt, .. } = &p.kind
                && let Some(d) = vt.sync_deadline()
            {
                t.push(d);
            }
        }
        t.into_iter().min()
    }

    pub fn on_timer(&mut self, now: Instant) {
        if self.whichkey_deadline.is_some_and(|d| d <= now) {
            self.whichkey_deadline = None;
            if self.key_state == KeyState::Prefix {
                self.whichkey_visible = true;
                self.dirty = true;
            }
        }
        if let KeyState::Repeat(until) = self.key_state
            && until <= now
        {
            self.key_state = KeyState::Normal;
        }
        if self.notes.expire(now) {
            self.dirty = true;
        }
        if self.clock_deadline.is_some_and(|d| d <= now) {
            self.clock_deadline = Some(next_minute(now));
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
            if let Some(n) = name {
                ws.name = n;
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
                if let Err(e) = pty.resize(size.0, size.1) {
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
            prefix: self.key_state == KeyState::Prefix,
            copy_mode: self.copy.is_some(),
            message: self.notes.toast(),
            clock: self
                .config
                .status
                .clock
                .then(|| jiff::Zoned::now().strftime("%H:%M").to_string()),
            prompt: self
                .prompt
                .as_ref()
                .map(|p| (p.label.as_str(), p.input.as_str())),
        }
    }

    /// Draws the whole screen into `grid` and returns the cursor.
    pub fn render(&mut self, grid: &mut Grid) -> Option<CursorState> {
        if grid.cols() != self.cols || grid.rows() != self.rows {
            grid.resize(self.cols, self.rows);
        }
        grid.clear(Style::new());
        self.output_since_render = 0;
        self.refresh_names();
        let area = self.layout_area();
        let placement = self.placement();
        self.sync_sizes(&placement);
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
        {
            let panes = &mut self.panes;
            let theme = &self.theme;
            let copy = &self.copy;
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
                        }
                        PaneKind::App(app) => render_app(app.as_mut(), theme, g, rect),
                    }
                },
            );
        }

        let status_y = self.status_y();
        let info = self.status_info();
        let (regions, prompt_cursor) = status::draw(grid, status_y, &self.theme, &info);
        self.status_regions = regions;

        if self.whichkey_visible || self.whichkey_full {
            whichkey::draw(grid, area, &self.theme, &self.keys, self.whichkey_full);
        }
        let palette_cursor = self
            .palette
            .as_ref()
            .map(|p| p.render(grid, area, &self.theme));

        let title = format!("TermOS · {}", self.session.active().name);
        if title != self.title_sent {
            self.outbox.push(ServerMsg::Title(title.clone()));
            self.title_sent = title;
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
        if self.whichkey_visible || self.whichkey_full {
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
