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
use termos_core::input::decode::{KeyAlternates, KeyKind};
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
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
use termos_term::input::{encode_focus, encode_key, encode_key_event, encode_mouse, encode_paste};
use termos_term::pty::{PtyEvent, PtyPane, SpawnSpec};
use termos_term::vt::{AlacrittyVt, VtBackend, VtEvent};
use termos_wm::compose::{ComposeOptions, compose};
use termos_wm::hit::{Button, Edge, Hit};
use termos_wm::layout::Dir;
use termos_wm::workspace::{Placement, Session, Snap, SplitHow, Workspace};
use tokio::sync::mpsc;

use crate::apps::{About, NotificationCenter, Tutorial};
use crate::attention::{Attention, Hooked, Need};
use crate::commands::{COMMANDS, Command};
use crate::copymode::{CopyAction, CopyMode};
use crate::keys::KeyTables;
use crate::menu::Menu;
use crate::notify::{Level, Notifications};
use crate::palette::{Palette, PaletteItem};
use crate::plugins::{Cells, PluginCommand};
use crate::prompt::{Confirmed, Prompt, PromptAction, PromptKind};
use crate::snapshot::{self, PaneSnapshot, Relaunch, SNAPSHOT_VERSION, SavedKind, SessionSnapshot};
use crate::status::{self, StatusInfo, StatusRegions, TabInfo};
use crate::ui::Chrome;
use crate::whichkey;

mod attend;
mod find;
mod menus;
pub(crate) mod power;
mod tools;
mod wall;

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
    /// A name given with Rename or `tos title`, over the program's own.
    pub name: Option<String>,
    /// An agent CLI's window: its preset and account (D-097).
    pub agent: Option<AgentWindow>,
    /// Waiting for the user, or a turn finished and not seen (D-115).
    pub attention: Option<Attention>,
    /// The agent CLI its hooks reported, for restoring it (D-116).
    pub hooked: Option<Hooked>,
}

/// What runs in an agent CLI's window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentWindow {
    /// `claude`, `codex`, `gemini`, …
    pub preset: String,
    /// Its account, for CLIs that have them.
    pub account: Option<String>,
}

/// The agent CLI a pane runs, for its snapshot (D-115): its own window,
/// or one its hooks reported running in a shell.
fn saved_agent(p: &Pane) -> Option<snapshot::SavedAgent> {
    let hooked = p.hooked.as_ref();
    if let Some(w) = &p.agent {
        let same = hooked.filter(|h| h.cli == w.preset);
        return Some(snapshot::SavedAgent {
            cli: w.preset.clone(),
            account: w
                .account
                .clone()
                .or_else(|| same.and_then(|h| h.account.clone())),
            conversation: same.and_then(|h| h.conversation.clone()),
            window: true,
        });
    }
    let h = hooked.filter(|h| h.running)?;
    Some(snapshot::SavedAgent {
        cli: h.cli.clone(),
        account: h.account.clone(),
        conversation: h.conversation.clone(),
        window: false,
    })
}

/// An agent CLI's arguments to start with `prompt` as its first message
/// (empty: it has no such argument).
fn first_prompt_args(preset: &str, prompt: &str) -> Vec<String> {
    match preset {
        "claude" | "codex" => vec![prompt.to_owned()],
        "gemini" => vec!["--prompt-interactive".to_owned(), prompt.to_owned()],
        _ => Vec::new(),
    }
}

/// `termos agent external` for `preset`, and the window's title.
fn agent_window_args(
    preset: &str,
    account: Option<&str>,
    resume_from: Option<&str>,
) -> (Vec<String>, String) {
    let mut args = vec!["agent".to_owned(), "external".to_owned()];
    if let Some(a) = account {
        args.extend(["--account".to_owned(), a.to_owned()]);
    }
    if let Some(from) = resume_from {
        args.extend(["--resume-from".to_owned(), from.to_owned()]);
    }
    args.push(preset.to_owned());
    let title = match account {
        Some(a) if a != "default" => format!("agent: {preset} ({a})"),
        _ => format!("agent: {preset}"),
    };
    (args, title)
}

impl Pane {
    /// A short name: the one given, else the foreground program (Linux),
    /// else the title, else the app title.
    pub fn display_name(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        match &self.kind {
            PaneKind::Term { pty, .. } => pty
                .foreground_name()
                .or_else(|| (!self.title.is_empty()).then(|| short_title(&self.title)))
                .unwrap_or_else(|| "shell".into()),
            PaneKind::App(app) => app.title(),
        }
    }

    /// The icon before its title (D-088).
    pub fn icon(&self) -> termos_render::icons::Icon {
        match &self.kind {
            PaneKind::Term { .. } => termos_render::icons::Icon::Terminal,
            PaneKind::App(app) => app.icon(),
        }
    }

    /// An app waiting on something (D-090).
    fn busy(&self) -> bool {
        matches!(&self.kind, PaneKind::App(app) if app.busy())
    }

    fn title_for_border(&self) -> String {
        if let Some(n) = &self.name {
            return n.clone();
        }
        match &self.kind {
            PaneKind::Term { .. } if !self.title.is_empty() => short_title(&self.title),
            _ => self.display_name(),
        }
    }
}

/// A pane's title with a program's path cut to its name: Windows consoles
/// title themselves with the program's full path
/// (`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe` becomes
/// `powershell`; `Administrator: C:\Windows\system32\cmd.exe` becomes
/// `Administrator: cmd`). Other titles stay as they are.
fn short_title(title: &str) -> String {
    let lower = title.trim_end().to_ascii_lowercase();
    let program = [".exe", ".com", ".bat", ".cmd"]
        .iter()
        .any(|ext| lower.ends_with(ext));
    let Some(last_sep) = title.rfind(['\\', '/']) else {
        return title.to_owned();
    };
    if !program {
        return title.to_owned();
    }
    // Where the path starts: a drive (`C:\`), a UNC or rooted path, or
    // the first separator.
    let bytes = title.as_bytes();
    let start = (0..title.len())
        .find(|&i| {
            (i + 2 < bytes.len()
                && bytes[i].is_ascii_alphabetic()
                && bytes[i + 1] == b':'
                && matches!(bytes[i + 2], b'\\' | b'/'))
                || matches!(bytes[i], b'\\' | b'/')
        })
        .unwrap_or(0);
    let file = title[last_sep + 1..].trim_end();
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    format!("{}{stem}", &title[..start])
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
    /// `needs-you` or `done` when its agent wants the user (D-115).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<&'static str>,
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
    /// The `termos` program, for panes that run its subcommands (task
    /// panels, the Git UI, the editor); `None`: this program.
    pub termos_exe: Option<PathBuf>,
    /// Opens agent windows and audit logs (the `termos` binary wires it in;
    /// `None`: agents are not part of this build).
    pub launcher: Option<std::sync::Arc<dyn termos_core::app::Launcher>>,
    /// The server's socket (windows that use the control API get it).
    pub server_socket: Option<PathBuf>,
    /// Read the machine's battery for the status line (D-124); off in
    /// tests.
    pub power: bool,
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

/// What tells a key's release from another key's: the key without Shift
/// (as the terminal reported it), so a release after Shift was let go
/// still matches.
fn key_identity(k: KeyEvent, alternates: &KeyAlternates) -> KeyCode {
    match (k.code, alternates.unshifted) {
        (KeyCode::BackTab, _) => KeyCode::Tab,
        (KeyCode::Char(_), Some(u)) => KeyCode::Char(u),
        (KeyCode::Char(c), None) => KeyCode::Char(c.to_lowercase().next().unwrap_or(c)),
        (code, _) => code,
    }
}

/// A client's theme (D-088): `[theme]` for its terminal's background, with
/// the icons of `[ui]` and gradients where it shows truecolor.
fn client_theme(config: &Config, pick: Option<ThemeBase>, caps: &Caps) -> Theme {
    let mut t = config.theme.theme(pick, caps.background);
    t.icons = config.ui.icons;
    t.gradients = config.ui.gradients && caps.color == termos_render::color::ColorDepth::Truecolor;
    t
}

/// Whether a client's terminal gets the links programs print (OSC 8,
/// D-118): not the Linux console or a dumb terminal, which might print
/// them, nor the KMS console, which underlines them itself.
fn links_shown(caps: &Caps) -> bool {
    let term = caps.term.as_str();
    !(term == "linux" || term == "dumb" || term == find::CONSOLE_TERM)
}

/// What the pointer is over, for hover highlights (D-085).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Hover {
    #[default]
    None,
    /// Something on the status line (`StatusRegions::target_at`).
    Status(u32),
    FloatButton(PaneId, Button),
    /// A wallpaper shortcut (D-089).
    Wallpaper(usize),
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
    /// The Kitty keyboard flags last sent to this client (D-103).
    keyboard_sent: u8,
    /// Keys pressed into panes whose programs want releases (Kitty flag
    /// 2): the release goes to the same pane.
    keys_down: Vec<(KeyCode, PaneId)>,
    /// The open menu and its submenus (D-085).
    menu: Option<Menu>,
    /// Where the pointer was last seen, and what is under it.
    pointer: Option<(u16, u16)>,
    hover: Hover,
    /// The last left press, for double clicks.
    last_click: Option<(Instant, u16, u16)>,
    /// Where the key list's commands are drawn, for clicks.
    whichkey_regions: Vec<(Rect, Command)>,
    /// Where the wallpaper's shortcuts are drawn, for clicks.
    wallpaper_regions: Vec<(Rect, Command)>,
    /// The link under the pointer, underlined (D-118): its pane and cells
    /// there.
    link_hover: Option<LinkCells>,
    /// Hint mode (D-118).
    hints: Option<crate::hints::Hints>,
    /// The search across panes (D-118).
    search: Option<crate::search::SearchBox>,
    /// The terminal has focus (as far as it reports it).
    focused: bool,
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
            keyboard_sent: 0,
            keys_down: Vec::new(),
            menu: None,
            pointer: None,
            hover: Hover::None,
            last_click: None,
            whichkey_regions: Vec::new(),
            wallpaper_regions: Vec::new(),
            link_hover: None,
            hints: None,
            search: None,
            focused: true,
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
    /// A WASM status-line widget was clicked, at this column within it.
    WidgetClick { name: String, column: u16 },
    /// Search every session's panes for this client (`search.all
    /// --all-sessions`, D-118).
    SearchAll { client: ClientId },
    /// Take the client to a match in another session.
    Reveal {
        client: ClientId,
        hit: crate::search::Hit,
    },
    /// Save every session, then restart or shut down the image (D-124).
    PowerOff(crate::prompt::PowerOff),
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
    /// Where the shell integration scripts are, once written (`None`
    /// inside: they could not be, D-116).
    shell_scripts: Option<Option<PathBuf>>,
    /// The wallpaper picture, if one is configured (D-089).
    wall: wall::Wall,
    /// Spinners in busy windows' titles turn from here (D-090).
    spin_epoch: Instant,
    /// Messages for one client (`Some`) or all of them (`None`).
    outbox: Vec<(Option<ClientId>, ServerMsg)>,
    requests: Vec<SessionRequest>,
    /// Names of all sessions on the server (for the palette).
    known_sessions: Vec<String>,
    /// Status-line widgets and palette commands of plugins (D-034).
    plugin_status: Vec<Cells>,
    plugin_commands: Vec<PluginCommand>,
    /// WASM widgets' drawings, by name (D-051), after the plugin widgets.
    wasm_widgets: Vec<(String, Cells)>,
    /// Both, as the status line draws them.
    status_widgets: Vec<Cells>,
    /// The panel of each project task that ran (a re-run replaces it).
    task_panes: BTreeMap<String, PaneId>,
    /// Floating windows' sizes before they were maximized.
    max_restore: BTreeMap<PaneId, Rect>,
    dirty: bool,
    /// The workspace/pane structure changed since the last snapshot.
    layout_changed: bool,
    quit: Option<String>,
    /// Bytes of visible pane output since the last render.
    output_since_render: usize,
    /// Paths looked for by link detection, and when (D-118).
    path_cache: std::collections::HashMap<PathBuf, (bool, Instant)>,
    /// The id search threads wake the desktop with.
    search_wake: Option<PaneId>,
    /// Panes whose next finished command is announced
    /// (`pane.notify-when-done`), with when a pane without shell
    /// integration counts as done: 5 s after its last output (D-118).
    armed: BTreeMap<PaneId, Option<Instant>>,
    /// The drop-down terminal, and the pane that had the focus before it
    /// showed (D-120).
    quick: Option<PaneId>,
    quick_back: Option<PaneId>,
    /// Panes typed into at once (`workspace.sync`, `pane.sync`).
    sync: std::collections::BTreeSet<PaneId>,
    /// Panes being recorded.
    recordings: BTreeMap<PaneId, tools::Recording>,
    /// The battery in the status line, and whether it is low (D-124).
    battery: Option<(String, bool)>,
    /// The machine has a battery to read (once a minute).
    battery_present: bool,
    battery_deadline: Option<Instant>,
    low: power::Low,
    /// The critical battery's countdown to suspend.
    critical_at: Option<Instant>,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("session", &self.session.name)
            .field("panes", &self.panes.len())
            .finish_non_exhaustive()
    }
}

/// A link's pane and its cells there: `(row, first column, end column)`.
type LinkCells = (PaneId, Vec<(u16, u16, u16)>);

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

/// The arguments that open `file` at `line` and `column` in the editor
/// `program` (D-118): Vim and Neovim `+LINE` (or `cursor()` with a
/// column), nano `+LINE,COL`, Emacs, micro and Kakoune `+LINE:COL`, Helix,
/// Sublime Text and Zed `FILE:LINE:COL`, VS Code and its kin
/// `-g FILE:LINE:COL`; any other editor gets the file alone.
pub(crate) fn editor_args(
    program: &str,
    file: &str,
    line: Option<u32>,
    column: Option<u32>,
) -> Vec<String> {
    let name = std::path::Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    let file = file.to_owned();
    let Some(l) = line else {
        return vec![file];
    };
    let c = column.unwrap_or(1);
    match name.as_str() {
        "nvim" | "vim" | "vi" | "gvim" | "mvim" => match column {
            Some(c) => vec![format!("+call cursor({l},{c})"), file],
            None => vec![format!("+{l}"), file],
        },
        "nano" | "pico" => vec![format!("+{l},{c}"), file],
        "emacs" | "emacsclient" | "micro" | "kak" => vec![format!("+{l}:{c}"), file],
        "hx" | "helix" | "subl" | "sublime_text" | "zed" => vec![format!("{file}:{l}:{c}")],
        "code" | "code-insiders" | "codium" | "vscodium" | "cursor" | "windsurf" => {
            vec!["-g".to_owned(), format!("{file}:{l}:{c}")]
        }
        _ => vec![file],
    }
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
            shell_scripts: None,
            wall: wall::Wall::default(),
            spin_epoch: Instant::now(),
            outbox: Vec::new(),
            requests: Vec::new(),
            known_sessions: Vec::new(),
            plugin_status: Vec::new(),
            plugin_commands: Vec::new(),
            wasm_widgets: Vec::new(),
            status_widgets: Vec::new(),
            task_panes: BTreeMap::new(),
            max_restore: BTreeMap::new(),
            dirty: true,
            layout_changed: true,
            quit: None,
            output_since_render: 0,
            path_cache: std::collections::HashMap::new(),
            search_wake: None,
            armed: BTreeMap::new(),
            quick: None,
            quick_back: None,
            sync: std::collections::BTreeSet::new(),
            recordings: BTreeMap::new(),
            battery: None,
            battery_present: false,
            battery_deadline: None,
            low: power::Low::Fine,
            critical_at: None,
            config,
        };
        if d.config.status.clock {
            d.clock_deadline = Some(next_minute(Instant::now()));
        }
        if d.opts.power {
            d.battery_present = !termos_hw::power::read().batteries.is_empty();
            d.read_battery(Instant::now());
        }
        for e in key_errors {
            d.notify(Level::Warn, e);
        }
        d.sync_wallpaper();
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
        let first = d.spawn_term(d.opts.cwd.clone(), d.first_size(area))?;
        let ws = d.new_workspace(first, area);
        d.session = Session::new(d.opts.session_name.clone(), ws);
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
    /// directory, the allowlisted programs typed into their shells, and the
    /// app windows that said how to reopen (D-104). Returns the desktop and
    /// the names of programs that were running but not restarted.
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
        let restore_agents = d.config.session.restore_agents;
        // Restored programs are typed into the pane's shell, quoted for it.
        let shell = snapshot::ShellKind::of(&d.config.shell_program(d.opts.env_shell.clone()));
        let area = d.layout_area();
        let mut map = std::collections::HashMap::new();
        let mut skipped = Vec::new();
        for old in layout.all_panes() {
            let Some(saved) = snap.panes.iter().find(|p| p.id == old.0) else {
                continue;
            };
            if saved.kind == SavedKind::App {
                // An app window that can come back (D-104) does, in its
                // place; others are left out of the layout.
                if let Some(reopen) = &saved.reopen {
                    match d.restore_app(reopen) {
                        Ok(id) => {
                            map.insert(old, id);
                        }
                        Err(e) => tracing::warn!("not restoring `{reopen}`: {e}"),
                    }
                }
                continue;
            }
            // An agent's own window comes back as that agent, continuing
            // its conversation (D-115).
            if let Some(agent) = saved.agent.as_ref().filter(|a| a.window && restore_agents)
                && let Some(exe) = d.termos_exe()
            {
                let args = snapshot::agent_args(agent);
                match d.spawn_pane(
                    snapshot::restore_cwd(saved),
                    (area.w, area.h),
                    Some((exe.display().to_string(), args)),
                ) {
                    Ok(id) => {
                        map.insert(old, id);
                        if let Some(p) = d.panes.get_mut(&id) {
                            p.title.clone_from(&saved.title);
                            p.agent = Some(AgentWindow {
                                preset: agent.cli.clone(),
                                account: agent.account.clone(),
                            });
                        }
                        continue;
                    }
                    Err(e) => tracing::warn!("not restoring {}: {e}", agent.cli),
                }
            }
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
            // An agent that ran in this shell is typed into it again
            // (`tos agent external …`), so the shell is there after it.
            if let Some(agent) = saved.agent.as_ref().filter(|a| !a.window && restore_agents) {
                let mut argv = vec![crate::TOS.to_owned()];
                argv.extend(snapshot::agent_args(agent));
                match snapshot::command_line(&argv, shell) {
                    Some(line) => pty.write(format!("{line}\r").into_bytes()),
                    None => skipped.push(agent.cli.clone()),
                }
                continue;
            }
            match snapshot::relaunch(saved, &allow) {
                Relaunch::Program(argv) => match snapshot::command_line(&argv, shell) {
                    Some(line) => pty.write(format!("{line}\r").into_bytes()),
                    None => skipped.push(snapshot::program_name(&argv[0]).to_owned()),
                },
                Relaunch::Skipped(name) => skipped.push(name),
                Relaunch::Shell => {}
            }
        }
        // The drop-down terminal is one again (D-120).
        d.quick = snap.quick.and_then(|old| map.get(&PaneId(old)).copied());
        if !layout.remap_panes(|old| map.get(&old).copied()) {
            return Err(io::Error::other("the snapshot has no panes to restore"));
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
                        reopen: None,
                        agent: saved_agent(p),
                    }
                }
                // Built-in apps say how to open them again (D-104).
                PaneKind::App(app) => PaneSnapshot {
                    id: p.id.0,
                    kind: SavedKind::App,
                    cwd: None,
                    program: None,
                    argv: None,
                    title: app.title(),
                    scrollback: None,
                    reopen: app.reopen(),
                    agent: None,
                },
            })
            .collect();
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            name: self.session.name.clone(),
            saved_at: snapshot::now_secs(),
            layout: self.session.clone(),
            panes,
            quick: self.quick.map(|p| p.0),
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
            if pixels && c.pixels_only {
                // The text version beneath gives way to the picture.
                for p in &pieces {
                    for y in p.y..p.bottom() {
                        for x in p.x..p.right() {
                            if let Some(cell) = grid.get_mut(x, y) {
                                let style = cell.style;
                                *cell = termos_render::cell::Cell::blank(style);
                            }
                        }
                    }
                }
            } else if c.pixels_only {
                continue;
            }
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
    /// Whether `id` is one of this desktop's panes (or its wallpaper's
    /// decode, which wakes it the same way).
    pub fn has_pane(&self, id: PaneId) -> bool {
        self.panes.contains_key(&id) || self.is_wallpaper(id)
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
            self.rebuild_status_widgets();
        }
        self.plugin_commands = commands;
    }

    fn rebuild_status_widgets(&mut self) {
        self.status_widgets = self
            .plugin_status
            .iter()
            .cloned()
            .chain(self.wasm_widgets.iter().map(|(_, c)| c.clone()))
            .collect();
        self.dirty = true;
    }

    /// The WASM widgets' drawings.
    pub fn set_wasm_widgets(&mut self, drawn: Vec<(String, Cells)>) {
        if drawn != self.wasm_widgets {
            self.wasm_widgets = drawn;
            self.rebuild_status_widgets();
        }
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

    /// A new workspace around its first terminal: on the desktop, the
    /// terminal floats over the wallpaper; otherwise it fills the screen
    /// (`[ui] start`, D-091).
    fn new_workspace(&self, first: PaneId, area: Rect) -> Workspace {
        let mut ws = Workspace::new("shell", first);
        if self.config.ui.start == termos_core::config::StartLook::Desktop {
            ws.toggle_float(first, area);
        }
        ws
    }

    /// The size a workspace's first terminal will have.
    fn first_size(&self, area: Rect) -> (u16, u16) {
        match self.config.ui.start {
            termos_core::config::StartLook::Desktop => (area.w * 7 / 10, area.h * 7 / 10),
            termos_core::config::StartLook::Terminal => (area.w, area.h),
        }
    }

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
        let shell = program.is_none();
        if let Some((program, args)) = program {
            spec.program = program;
            spec.args = args;
        }
        // The shell marks its prompts and commands (D-115, D-116).
        let injection = if shell && self.config.shell.integration {
            crate::integration::Shell::of(&spec.program)
                .zip(self.shell_scripts())
                .map(|(kind, dir)| {
                    crate::integration::inject(kind, &dir, &|k| std::env::var(k).ok())
                })
        } else {
            None
        };
        spec.cwd = cwd.filter(|c| c.is_dir());
        spec.env = vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERMOS".into(), std::process::id().to_string()),
            ("TERMOS_PANE".into(), id.to_string()),
        ];
        spec.env.extend(self.opts.pane_env.iter().cloned());
        if let Some(inj) = injection {
            spec.args = inj.args;
            spec.env.extend(inj.env);
        }
        // A pane in a worktree gets its name, branch and ports (D-114).
        if let (Some(launcher), Some(dir)) = (self.opts.launcher.clone(), spec.cwd.clone()) {
            let ctx = termos_core::app::LaunchContext {
                config: &self.config,
                paths: &self.opts.paths,
                cwd: Some(&dir),
                socket: self.opts.server_socket.as_deref(),
                session: &self.session.name,
            };
            spec.env.extend(launcher.worktree_env(&dir, &ctx));
        }
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
                name: None,
                agent: None,
                cwd: None,
                size: (w, h),
                bell: false,
                attention: None,
                hooked: None,
            },
        );
        Ok(id)
    }

    /// The shell integration scripts' folder, written on first use.
    fn shell_scripts(&mut self) -> Option<PathBuf> {
        self.shell_scripts
            .get_or_insert_with(|| {
                let dir = crate::integration::dir(&self.opts.paths.data_dir);
                match crate::integration::install(&dir) {
                    Ok(()) => Some(dir),
                    Err(e) => {
                        tracing::warn!(
                            "shells start without TermOS's integration: cannot write {}: {e}",
                            dir.display()
                        );
                        None
                    }
                }
            })
            .clone()
    }

    fn open_app(&mut self, app: Box<dyn App>) -> PaneId {
        let id = self.alloc_id();
        self.insert_app(id, app);
        id
    }

    /// The built-in app window `cmd` opens, made but not placed; `None`
    /// for commands that open no such window. Saved sessions use it to
    /// bring app windows back where they were (D-104).
    fn build_app(&self, cmd: &Command) -> Option<Box<dyn App>> {
        Some(match cmd {
            Command::ImageOpen(path) => {
                Box::new(termos_apps::ImageViewer::open(self.resolve_path(path)))
            }
            Command::MarkdownOpen(path) => {
                Box::new(termos_apps::MarkdownViewer::open(self.resolve_path(path)))
            }
            Command::FilesOpen(path) => {
                let dir = if path.is_empty() {
                    self.focused_cwd()
                        .or_else(home_dir)
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_default()
                } else {
                    self.resolve_path(path)
                };
                Box::new(termos_apps::Files::open(
                    dir,
                    termos_apps::FilesOptions {
                        bookmarks: self.config.files.bookmarks.clone(),
                        two_pane: self.config.files.two_pane,
                        config_path: Some(self.opts.config_path.clone()),
                        state_dir: Some(self.opts.paths.state_dir.clone()),
                        drives: self.opts.power,
                    },
                ))
            }
            Command::MonitorOpen => {
                let mut m = termos_apps::SystemMonitor::open();
                m.set_confirm(self.config.processes.confirm);
                Box::new(m)
            }
            Command::ProcessesOpen(pid) => Box::new(termos_apps::SystemMonitor::processes(
                pid.parse().ok(),
                self.config.processes.confirm,
            )),
            Command::NetworkOpen => Box::new(termos_apps::NetworkPage::open()),
            Command::SettingsOpen => {
                Box::new(termos_apps::Settings::open(self.opts.config_path.clone()))
            }
            Command::ClipboardOpen => Box::new(termos_apps::ClipboardManager::new(
                std::sync::Arc::clone(&self.opts.clipboard),
            )),
            _ => return None,
        })
    }

    /// An app window for the saved command `reopen`, registered under a
    /// new id but not placed (the restored layout places it).
    fn restore_app(&mut self, reopen: &str) -> Result<PaneId, String> {
        let cmd: Command = reopen
            .parse()
            .map_err(|e: crate::commands::CommandError| e.0)?;
        let app = match &cmd {
            Command::CenterOpen | Command::UsageOpen | Command::PackagesOpen => {
                let kind = match cmd {
                    Command::CenterOpen => termos_core::app::LaunchKind::Center,
                    Command::UsageOpen => termos_core::app::LaunchKind::Usage,
                    _ => termos_core::app::LaunchKind::Packages,
                };
                self.launcher_app(kind, "")?
            }
            _ => self
                .build_app(&cmd)
                .ok_or_else(|| format!("`{reopen}` opens no app window"))?,
        };
        let id = self.alloc_id();
        self.register_app(id, app);
        Ok(id)
    }

    fn insert_app(&mut self, id: PaneId, app: Box<dyn App>) {
        self.register_app(id, app);
        let area = self.layout_area();
        self.session.active_mut().add_float(id, area);
        self.animate(anim::Kind::FloatOpen(id), Instant::now());
        self.dirty = true;
        self.layout_changed = true;
    }

    /// Adds an app pane, with a waker for its background work.
    fn register_app(&mut self, id: PaneId, mut app: Box<dyn App>) {
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
                name: None,
                agent: None,
                cwd: None,
                size: (0, 0),
                bell: false,
                attention: None,
                hooked: None,
            },
        );
    }

    /// `file.open-system PATH`: the system's own program for a file, on
    /// this machine (D-122): `xdg-open`, `open` or `explorer`, the path as
    /// one argument.
    fn open_system(&mut self, path: &std::path::Path) {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer.exe"
        } else {
            "xdg-open"
        };
        let spawned = std::process::Command::new(program)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => self.notify(Level::Error, format!("cannot run {program}: {e}")),
        }
    }

    /// The program `file.edit` runs (before the file): `$VISUAL` or
    /// `$EDITOR`, or `termos edit`, which sets up TermOS's Neovim or Helix
    /// (`[dev] editor`, D-054).
    fn edit_command(&self) -> (String, Vec<String>) {
        use termos_core::config::EditorChoice;
        let env = |k: &str| std::env::var(k).ok();
        let has_env = ["VISUAL", "EDITOR"]
            .iter()
            .any(|k| env(k).is_some_and(|v| !v.trim().is_empty()));
        match self.config.dev.editor {
            EditorChoice::Env => editor_command(env),
            EditorChoice::Auto if has_env => editor_command(env),
            _ => match self.termos_exe() {
                Some(exe) => (exe.display().to_string(), vec!["edit".to_owned()]),
                None => editor_command(env),
            },
        }
    }

    fn termos_exe(&self) -> Option<PathBuf> {
        self.opts
            .termos_exe
            .clone()
            .or_else(|| std::env::current_exe().ok())
    }

    /// A panel at the bottom running `termos task NAME`; a task's earlier
    /// panel goes.
    fn open_task(&mut self, name: &str, area: Rect) {
        let Some(exe) = self.termos_exe() else {
            self.notify(Level::Error, "cannot find the termos program");
            return;
        };
        if let Some(old) = self.task_panes.remove(name)
            && self.panes.contains_key(&old)
        {
            self.close_pane(old);
        }
        let cwd = self.focused_cwd();
        let args = vec!["task".to_owned(), name.to_owned(), "--wait".to_owned()];
        let program = exe.display().to_string();
        match self.spawn_pane(cwd, (area.w, area.h / 3), Some((program, args))) {
            Ok(id) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.title = format!("task: {name}");
                }
                if self
                    .session
                    .active_mut()
                    .add_split(id, SplitHow::Down, area)
                {
                    self.task_panes.insert(name.to_owned(), id);
                } else {
                    self.close_pane(id);
                    self.notify(Level::Warn, "no space for the task panel");
                }
            }
            Err(e) => self.notify(Level::Error, format!("cannot run the task: {e}")),
        }
    }

    /// A window from the launcher (agents, audit logs), in the focused
    /// pane's folder.
    /// A window the launcher opens (agents, the command center), not yet
    /// placed.
    fn launcher_app(
        &self,
        kind: termos_core::app::LaunchKind,
        arg: &str,
    ) -> Result<Box<dyn App>, String> {
        let cwd = self
            .focused_cwd()
            .or_else(|| self.opts.cwd.clone())
            .or_else(|| std::env::current_dir().ok());
        self.launcher_app_in(kind, arg, cwd)
    }

    /// [`Desktop::launcher_app`] for the folder `cwd`.
    fn launcher_app_in(
        &self,
        kind: termos_core::app::LaunchKind,
        arg: &str,
        cwd: Option<PathBuf>,
    ) -> Result<Box<dyn App>, String> {
        let Some(launcher) = self.opts.launcher.clone() else {
            return Err("AI agents are not part of this build".into());
        };
        let ctx = termos_core::app::LaunchContext {
            config: &self.config,
            paths: &self.opts.paths,
            cwd: cwd.as_deref(),
            socket: self.opts.server_socket.as_deref(),
            session: &self.session.name,
        };
        launcher.open(kind, arg, &ctx)
    }

    fn launch(&mut self, kind: termos_core::app::LaunchKind, arg: &str) {
        match self.launcher_app(kind, arg) {
            Ok(app) => {
                let id = self.open_app(app);
                // Agent windows are working windows: give them most of the
                // screen.
                let area = self.layout_area();
                let rect = Rect::new(
                    area.x + area.w / 20,
                    area.y + area.h / 20,
                    area.w - area.w / 10,
                    area.h - area.h / 10,
                );
                self.session.active_mut().set_float_rect(id, rect, area);
            }
            Err(e) => self.notify(Level::Error, e),
        }
    }

    /// A launch context for the launcher's calls.
    fn launch_context(
        &self,
    ) -> Option<(
        std::sync::Arc<dyn termos_core::app::Launcher>,
        Option<PathBuf>,
    )> {
        let launcher = self.opts.launcher.clone()?;
        let cwd = self
            .focused_cwd()
            .or_else(|| self.opts.cwd.clone())
            .or_else(|| std::env::current_dir().ok());
        Some((launcher, cwd))
    }

    /// The agent CLIs' accounts (D-097).
    pub(crate) fn agent_accounts(&self) -> Vec<termos_core::app::AccountInfo> {
        let Some((launcher, cwd)) = self.launch_context() else {
            return Vec::new();
        };
        let ctx = termos_core::app::LaunchContext {
            config: &self.config,
            paths: &self.opts.paths,
            cwd: cwd.as_deref(),
            socket: self.opts.server_socket.as_deref(),
            session: &self.session.name,
        };
        launcher.accounts(&ctx)
    }

    /// `account.use CLI NAME`.
    fn use_account(&mut self, arg: &str) {
        let Some((cli, name)) = arg.split_once(' ') else {
            self.notify(
                Level::Error,
                "`account.use` needs a CLI and an account: claude work",
            );
            return;
        };
        let Some((launcher, cwd)) = self.launch_context() else {
            self.notify(Level::Error, "accounts are not part of this build");
            return;
        };
        let ctx = termos_core::app::LaunchContext {
            config: &self.config,
            paths: &self.opts.paths,
            cwd: cwd.as_deref(),
            socket: self.opts.server_socket.as_deref(),
            session: &self.session.name,
        };
        match launcher.use_account(cli.trim(), name.trim(), &ctx) {
            Ok(said) => self.notify(Level::Info, said),
            Err(e) => self.notify(Level::Error, e),
        }
    }

    /// An external CLI agent (`termos agent external NAME`) in a float.
    fn open_external_agent(&mut self, name: &str, area: Rect) {
        // `claude@work`: as that account.
        let (name, chosen) = match name.split_once('@') {
            Some((n, a)) => (n, Some(a.to_owned())),
            None => (name, None),
        };
        let account = chosen.or_else(|| {
            self.agent_accounts()
                .into_iter()
                .find(|a| a.cli == name && a.active)
                .map(|a| a.name)
        });
        let cwd = self.focused_cwd();
        self.open_agent_window(name, account, None, area, cwd);
    }

    /// An agent window for `preset` as `account`, continuing `resume_from`'s
    /// conversation in this folder when given.
    fn open_agent_window(
        &mut self,
        preset: &str,
        account: Option<String>,
        resume_from: Option<&str>,
        area: Rect,
        cwd: Option<PathBuf>,
    ) -> Option<PaneId> {
        let (args, title) = agent_window_args(preset, account.as_deref(), resume_from);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let id = self.open_termos_window_in(&args, &title, area, cwd)?;
        if let Some(p) = self.panes.get_mut(&id) {
            p.agent = Some(AgentWindow {
                preset: preset.to_owned(),
                account,
            });
        }
        Some(id)
    }

    /// An agent CLI as the account in use, in a tiled pane in `cwd` (a
    /// worktree's first pane).
    fn spawn_agent_pane(
        &mut self,
        preset: &str,
        cwd: Option<PathBuf>,
        size: (u16, u16),
        prompt: Option<&str>,
    ) -> io::Result<PaneId> {
        let account = self
            .agent_accounts()
            .into_iter()
            .find(|a| a.cli == preset && a.active)
            .map(|a| a.name);
        let exe = self
            .termos_exe()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no termos program"))?;
        let (mut args, title) = agent_window_args(preset, account.as_deref(), None);
        if let Some(p) = prompt {
            args.extend(first_prompt_args(preset, p));
        }
        let id = self.spawn_pane(cwd, size, Some((exe.display().to_string(), args)))?;
        if let Some(p) = self.panes.get_mut(&id) {
            p.title = title;
            p.agent = Some(AgentWindow {
                preset: preset.to_owned(),
                account,
            });
        }
        Ok(id)
    }

    /// `worktree.open NAME`: the worktree as a workspace of its own, named
    /// after it: its agent on the left, a shell on the right, its servers
    /// below the shell, each in its folder with its ports (D-114). A
    /// worktree already open is selected.
    fn open_worktree(&mut self, name: &str, area: Rect) {
        self.open_worktree_with(name, area, None);
    }

    /// [`Desktop::open_worktree`], its agent starting with `prompt`.
    fn open_worktree_with(&mut self, name: &str, area: Rect, prompt: Option<&str>) {
        let Some((launcher, cwd)) = self.launch_context() else {
            self.notify(Level::Error, "worktrees are not part of this build");
            return;
        };
        let ctx = termos_core::app::LaunchContext {
            config: &self.config,
            paths: &self.opts.paths,
            cwd: cwd.as_deref(),
            socket: self.opts.server_socket.as_deref(),
            session: &self.session.name,
        };
        let layout = match launcher.worktree(name, &ctx) {
            Ok(l) => l,
            Err(e) => {
                self.notify(Level::Error, e);
                return;
            }
        };
        if let Some(idx) = self
            .session
            .workspaces
            .iter()
            .position(|w| w.name == layout.name)
        {
            self.session.select(idx);
            return;
        }
        let dir = Some(layout.path.clone());
        let half = (area.w / 2, area.h);
        let first = match layout.agent.as_deref() {
            // TermOS's own agent: a window, working in the worktree.
            Some("termos") => self
                .launcher_app_in(termos_core::app::LaunchKind::Agent, "", dir.clone())
                .map(|app| self.open_app(app))
                .map_err(io::Error::other),
            Some(preset) => self.spawn_agent_pane(preset, dir.clone(), half, prompt),
            None => self.spawn_term(dir.clone(), half),
        };
        let first = match first {
            Ok(id) => id,
            Err(e) => {
                self.notify(Level::Error, format!("cannot open {}: {e}", layout.name));
                return;
            }
        };
        // The built-in `worktree` layout (D-120): the agent, a shell on
        // its right, each server below the one before. Commands are typed
        // into shells, so they can be run again after they stop.
        let mut panes = vec![
            termos_core::layouts::PaneSpec {
                focus: true,
                ..Default::default()
            },
            termos_core::layouts::PaneSpec {
                split: Some(termos_core::layouts::Split::Right),
                ..Default::default()
            },
        ];
        panes.extend(
            layout
                .servers
                .iter()
                .map(|cmd| termos_core::layouts::PaneSpec {
                    split: Some(termos_core::layouts::Split::Down),
                    command: Some(cmd.clone()),
                    ..Default::default()
                }),
        );
        let spec = termos_core::layouts::WorkspaceSpec {
            name: Some(layout.name.clone()),
            cwd: None,
            pane: panes,
        };
        self.build_workspace(&spec, &layout.path, &layout.name, Some(first));
    }

    /// A review's comments for the agent of a worktree (D-114): pasted into
    /// its agent pane and sent, or, with none running, the first prompt of
    /// a new one there (in the worktree's workspace, opened if need be).
    fn tell_agent(&mut self, worktree: &str, dir: &std::path::Path, text: &str) {
        let area = self.layout_area();
        let in_workspace: Vec<PaneId> = self
            .session
            .workspaces
            .iter()
            .filter(|w| w.name == worktree)
            .flat_map(Workspace::panes)
            .collect();
        let is_agent = |p: &Pane| p.agent.is_some() && matches!(p.kind, PaneKind::Term { .. });
        let cwd_of = |p: &Pane| match &p.kind {
            PaneKind::Term { pty, .. } => p.cwd.clone().or_else(|| pty.foreground_cwd()),
            PaneKind::App(_) => None,
        };
        // Folders compared with links resolved (macOS's /var is
        // /private/var).
        let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        let dir = canon(dir);
        let running = in_workspace
            .into_iter()
            .find(|id| self.panes.get(id).is_some_and(is_agent))
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|(_, p)| {
                        is_agent(p) && cwd_of(p).is_some_and(|c| canon(&c).starts_with(&dir))
                    })
                    .map(|(id, _)| *id)
            });
        if let Some(id) = running {
            let bytes = match self.panes.get(&id).map(|p| &p.kind) {
                Some(PaneKind::Term { vt, .. }) => encode_paste(text, &vt.modes()),
                _ => return,
            };
            self.write_to_pane(id, bytes);
            self.write_to_pane(id, b"\r".to_vec());
            if let Some(w) = self.session.find(id) {
                self.session.select(w);
                self.session.active_mut().set_focus(id);
            }
            self.notify(
                Level::Info,
                format!("sent the review to {worktree}'s agent"),
            );
            return;
        }
        // None running: one with the comments as its first message.
        let agent = self.launch_context().and_then(|(launcher, cwd)| {
            let ctx = termos_core::app::LaunchContext {
                config: &self.config,
                paths: &self.opts.paths,
                cwd: cwd.as_deref(),
                socket: self.opts.server_socket.as_deref(),
                session: &self.session.name,
            };
            launcher.worktree(worktree, &ctx).ok()?.agent
        });
        let Some(preset) = agent.filter(|a| !first_prompt_args(a, "").is_empty()) else {
            self.copy_text(text.to_owned(), ClipSource::App("review".into()));
            self.notify(
                Level::Info,
                "no agent CLI for this worktree: the comments are on the clipboard to paste",
            );
            return;
        };
        if let Some(w) = self
            .session
            .workspaces
            .iter()
            .position(|w| w.name == worktree)
        {
            self.session.select(w);
            match self.spawn_agent_pane(
                &preset,
                Some(dir.to_owned()),
                (area.w / 2, area.h),
                Some(text),
            ) {
                Ok(id) => {
                    if !self
                        .session
                        .active_mut()
                        .add_split(id, SplitHow::Auto, area)
                    {
                        self.session.active_mut().add_float(id, area);
                    }
                }
                Err(e) => self.notify(Level::Error, format!("cannot start {preset}: {e}")),
            }
        } else {
            self.open_worktree_with(worktree, area, Some(text));
        }
    }

    /// `agent.swap ACCOUNT`: the focused agent window continues its
    /// conversation as another account, in its place (D-097).
    fn swap_agent(&mut self, account: &str, area: Rect) {
        let Some(id) = self.focused() else {
            return;
        };
        let Some(agent) = self.panes.get(&id).and_then(|p| p.agent.clone()) else {
            self.notify(
                Level::Error,
                "`agent.swap` works in a Claude Code or Codex window",
            );
            return;
        };
        if !self
            .agent_accounts()
            .iter()
            .any(|a| a.cli == agent.preset && a.name == account)
        {
            self.notify(
                Level::Error,
                format!("{} has no account named {account}", agent.preset),
            );
            return;
        }
        let from = agent.account.clone().unwrap_or_else(|| "default".into());
        let rect = self.session.active().float_rect(id);
        let cwd = self.focused_cwd();
        // The old window ends first, so its conversation is complete.
        self.kill_pane(id);
        if let Some(new) = self.open_agent_window(
            &agent.preset,
            Some(account.to_owned()),
            Some(&from),
            area,
            cwd,
        ) && let Some(r) = rect
        {
            self.session.active_mut().set_float_rect(new, r, area);
        }
    }

    /// `termos ARGS` in a float titled `title`, in the focused pane's
    /// folder.
    fn open_termos_window(&mut self, args: &[&str], title: &str, area: Rect) -> Option<PaneId> {
        let cwd = self.focused_cwd();
        self.open_termos_window_in(args, title, area, cwd)
    }

    /// `termos ARGS` in a float titled `title`, in `cwd`.
    fn open_termos_window_in(
        &mut self,
        args: &[&str],
        title: &str,
        area: Rect,
        cwd: Option<PathBuf>,
    ) -> Option<PaneId> {
        let Some(exe) = self.termos_exe() else {
            self.notify(Level::Error, "cannot find the termos program");
            return None;
        };
        let program = exe.display().to_string();
        let args = args.iter().map(|a| (*a).to_owned()).collect();
        match self.spawn_pane(
            cwd,
            (area.w * 9 / 10, area.h * 9 / 10),
            Some((program, args)),
        ) {
            Ok(id) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.title = title.to_owned();
                }
                self.session.active_mut().add_float(id, area);
                self.animate(anim::Kind::FloatOpen(id), Instant::now());
                Some(id)
            }
            Err(e) => {
                self.notify(Level::Error, format!("cannot start {title}: {e}"));
                None
            }
        }
    }

    /// lazygit in a floating window, in the focused pane's folder.
    fn open_git(&mut self, area: Rect) {
        let Some(exe) = self.termos_exe() else {
            self.notify(Level::Error, "cannot find the termos program");
            return;
        };
        let cwd = self.focused_cwd();
        let program = exe.display().to_string();
        match self.spawn_pane(
            cwd,
            (area.w * 9 / 10, area.h * 9 / 10),
            Some((program, vec!["git".to_owned()])),
        ) {
            Ok(id) => {
                self.session.active_mut().add_float(id, area);
                self.animate(anim::Kind::FloatOpen(id), Instant::now());
            }
            Err(e) => self.notify(Level::Error, format!("cannot start the Git UI: {e}")),
        }
    }

    /// A floating window running `termos update install`, which checks
    /// for a newer release, asks before installing it, and waits for a key
    /// so its last words can be read (D-077).
    fn open_update(&mut self, area: Rect) {
        let Some(exe) = self.termos_exe() else {
            self.notify(Level::Error, "cannot find the termos program");
            return;
        };
        let cwd = self.focused_cwd();
        let args = ["update", "install", "--wait"].map(str::to_owned).to_vec();
        match self.spawn_pane(
            cwd,
            (area.w * 7 / 10, area.h / 2),
            Some((exe.display().to_string(), args)),
        ) {
            Ok(id) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.title = "updates".into();
                }
                self.session.active_mut().add_float(id, area);
                self.animate(anim::Kind::FloatOpen(id), Instant::now());
            }
            Err(e) => self.notify(Level::Error, format!("cannot start termos update: {e}")),
        }
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
        self.armed.remove(&id);
        self.sync.remove(&id);
        self.stop_recording(id);
        if self.quick == Some(id) {
            self.quick = None;
        }
        if self.quick_back == Some(id) {
            self.quick_back = None;
        }
        for ui in self
            .clients
            .values_mut()
            .chain(std::iter::once(&mut self.ui))
        {
            if ui.hints.as_ref().is_some_and(|h| h.pane == id) {
                ui.hints = None;
            }
            if ui.link_hover.as_ref().is_some_and(|(p, _)| *p == id) {
                ui.link_hover = None;
            }
        }
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

    /// Input for a pane's program: the user (or a script) answered it,
    /// so it no longer waits (D-115).
    fn write_to_pane(&mut self, id: PaneId, bytes: Vec<u8>) {
        if let Some(p) = self.panes.get_mut(&id)
            && p.attention.take().is_some()
        {
            self.dirty = true;
        }
        self.write_quietly(id, bytes);
    }

    /// Bytes for a pane's program that are not an answer (the wheel's
    /// arrow keys).
    fn write_quietly(&mut self, id: PaneId, bytes: Vec<u8>) {
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
        self.copy_quietly(text, source);
        self.notify(Level::Info, format!("copied {n} characters"));
    }

    /// [`Desktop::copy_text`] without its message.
    fn copy_quietly(&mut self, text: String, source: ClipSource) {
        self.remember_copy(&text, source);
        self.outbox
            .push((self.current, ServerMsg::Clipboard(text.clone())));
        self.paste_buffer = Some(text);
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
        for ui in self.clients.values_mut() {
            ui.theme = client_theme(&self.config, ui.theme_pick, &ui.caps);
        }
        if self.current.is_some() {
            self.ui.theme = client_theme(&self.config, self.ui.theme_pick, &self.ui.caps);
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
            ui.theme = client_theme(&self.config, ui.theme_pick, caps);
            ui.cols = (*cols).max(2);
            ui.rows = (*rows).max(2);
            // A fresh client must not inherit a stale title or keyboard.
            ui.title_sent.clear();
            ui.keyboard_sent = 0;
            ui.keys_down.clear();
            // The logo, for a moment (D-089).
            if self.config.ui.splash && Self::animates(&self.config, ui) {
                self.anim.start(anim::Kind::Splash(client), Instant::now());
            }
            self.last_active = Some(client);
            self.recompute_size();
            self.dirty = true;
            return;
        }
        if !self.enter(client) {
            return;
        }
        // Any input ends the splash at once; the input still counts.
        let input = match &msg {
            ClientMsg::Key(_) | ClientMsg::KeyDetail { .. } | ClientMsg::Paste(_) => true,
            ClientMsg::Mouse(m) => !matches!(m.kind, MouseKind::Moved),
            _ => false,
        };
        if input && self.anim.stop(anim::Kind::Splash(client)) {
            self.dirty = true;
        }
        if matches!(
            msg,
            ClientMsg::Key(_)
                | ClientMsg::KeyDetail { .. }
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
            ClientMsg::Key(k) => self.on_key(k, KeyKind::Press, &KeyAlternates::default()),
            ClientMsg::KeyDetail {
                key,
                kind: KeyKind::Release,
                alternates,
            } => self.on_key_release(key, &alternates),
            ClientMsg::KeyDetail {
                key,
                kind,
                alternates,
            } => self.on_key(key, kind, &alternates),
            ClientMsg::Mouse(m) => {
                if self.config.mouse {
                    self.on_mouse(m);
                }
            }
            ClientMsg::Paste(text) => self.on_paste(&text),
            ClientMsg::Focus(focused) => {
                self.ui.focused = focused;
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
        } else if self.ui.search.is_some() {
            self.search_paste(text);
        } else if self.ui.hints.is_some() {
            // Hint mode takes keys, not text.
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
                self.sync_paste(id, text);
            } else if let Some(Pane {
                kind: PaneKind::App(app),
                ..
            }) = self.panes.get_mut(&id)
            {
                let effect = app.handle_paste(text);
                self.app_effect(id, effect);
            }
        }
        self.dirty = true;
    }

    // ----- keys -------------------------------------------------------

    /// A key press or repeat; `kind` and `alternates` matter only to a
    /// program in a pane that asked for them (Kitty flags 2 and 4).
    fn on_key(&mut self, k: KeyEvent, kind: KeyKind, alternates: &KeyAlternates) {
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
        if self.menu_key(k) {
            return;
        }
        if let Some(palette) = &mut self.ui.palette {
            let action = palette.handle_key(k);
            self.palette_action(action);
            return;
        }
        if self.ui.search.is_some() {
            self.search_key(k);
            return;
        }
        if self.ui.hints.is_some() {
            self.hint_key(k);
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
                let modes = vt.modes();
                let bytes = encode_key_event(k, kind, alternates, &modes);
                if modes.kitty_flags & 2 != 0 {
                    let key = key_identity(k, alternates);
                    let down = &mut self.ui.keys_down;
                    down.retain(|(c, _)| *c != key);
                    // A release lost on the way must not grow this.
                    if down.len() >= 32 {
                        down.remove(0);
                    }
                    down.push((key, id));
                }
                self.write_to_pane(id, bytes);
                // Typed into several panes at once (D-120).
                self.sync_key(id, k, kind, alternates);
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

    /// A key release: to the pane that got the press, if its program
    /// asked for releases.
    fn on_key_release(&mut self, k: KeyEvent, alternates: &KeyAlternates) {
        let key = key_identity(k, alternates);
        let Some(i) = self.ui.keys_down.iter().position(|(c, _)| *c == key) else {
            return;
        };
        let (_, pane) = self.ui.keys_down.remove(i);
        let bytes = match self.panes.get(&pane) {
            Some(Pane {
                kind: PaneKind::Term { vt, .. },
                ..
            }) => encode_key_event(k, KeyKind::Release, alternates, &vt.modes()),
            _ => return,
        };
        if !bytes.is_empty() {
            self.write_to_pane(pane, bytes);
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
            AppEffect::TellAgent {
                worktree,
                dir,
                text,
            } => {
                // The review is done; the agent's pane is what to watch now.
                self.close_pane(id);
                self.tell_agent(&worktree, &dir, &text);
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
            PromptKind::RenamePane(id) => {
                let _ = self.rename_pane(id, &text);
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
            PromptKind::Confirm(Confirmed::Power(what)) => self.power_off(what),
            PromptKind::Confirm(Confirmed::Restore) => {
                self.requests.push(SessionRequest::RestoreSaved {
                    from_question: true,
                });
            }
            PromptKind::SnippetParam { snippet, values } => {
                self.snippet_answer(snippet, values, text);
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

    /// The last `lines` lines of a terminal pane's text (history and
    /// screen), without the empty lines below the last one written.
    pub fn read_pane(&self, pane: Option<PaneId>, lines: usize) -> Result<Vec<String>, String> {
        let id = self.target_pane(pane)?;
        let Some(PaneKind::Term { vt, .. }) = self.panes.get(&id).map(|p| &p.kind) else {
            return Err(format!("pane {id} is not a terminal"));
        };
        // Lines as the program wrote them, so nothing (a key, say) is cut
        // in two where the pane wrapped it.
        let mut out = vt.logical_lines();
        while out.last().is_some_and(String::is_empty) {
            out.pop();
        }
        let skip = out.len().saturating_sub(lines);
        Ok(out.split_off(skip))
    }

    /// The command blocks a pane's shell marked (OSC 133, D-116), oldest
    /// first.
    pub fn blocks(&self, pane: Option<PaneId>) -> Result<Vec<termos_term::blocks::Block>, String> {
        let id = self.target_pane(pane)?;
        match self.panes.get(&id).map(|p| &p.kind) {
            Some(PaneKind::Term { vt, .. }) => Ok(vt.blocks()),
            _ => Err(format!("pane {id} is not a terminal")),
        }
    }

    /// The last command in a pane: the newest block with a command line,
    /// or with `finished`, the newest that finished (a pane reading its
    /// own, whose newest is the reading command).
    pub fn last_command(
        &self,
        pane: Option<PaneId>,
        finished: bool,
    ) -> Result<Option<termos_term::blocks::Block>, String> {
        use termos_term::blocks::BlockState;
        Ok(self
            .blocks(pane)?
            .into_iter()
            .rev()
            .find(|b| !b.command.is_empty() && (!finished || b.state == BlockState::Finished)))
    }

    /// `pane.copy-output`: the focused pane's last finished command's
    /// output to the clipboard.
    fn copy_last_output(&mut self) {
        let last = self.last_command(None, true).ok().flatten();
        match last {
            Some(b) if !b.output.is_empty() => {
                let n = b.output.len();
                let cmd = crate::attention::short(&b.command, 40);
                self.copy_quietly(b.output.join("\n"), ClipSource::Selection);
                self.notify(
                    Level::Info,
                    format!(
                        "copied {n} line{} of `{cmd}`'s output",
                        if n == 1 { "" } else { "s" }
                    ),
                );
            }
            Some(b) => self.notify(
                Level::Info,
                format!(
                    "`{}` printed nothing",
                    crate::attention::short(&b.command, 40)
                ),
            ),
            None => self.notify(
                Level::Info,
                "no command here to copy from (its shell does not mark commands)",
            ),
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
                    waiting: p.attention.as_ref().map(|a| match a.need {
                        Need::You => "needs-you",
                        Need::Done => "done",
                        Need::Failed => "failed",
                    }),
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
            Command::MaximizeToggle => self.toggle_maximize(),
            Command::PaneRename => self.prompt_pane_name(),
            Command::Menu => self.open_start_menu(),
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
                match self.spawn_term(cwd, self.first_size(area)) {
                    Ok(id) => {
                        let ws = self.new_workspace(id, area);
                        self.session.add(ws);
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
            Command::AboutOpen => {
                let app = About::new(&self.opts.config_path, &self.opts.paths);
                self.open_app(Box::new(app));
            }
            Command::ReloadConfig => self.reload_config(),
            Command::ImageOpen(_) | Command::MarkdownOpen(_) | Command::FilesOpen(_) => {
                if let Some(app) = self.build_app(&cmd) {
                    self.open_app(app);
                }
            }
            Command::Task(name) => self.open_task(&name, area),
            Command::GitOpen => self.open_git(area),
            Command::MonitorOpen | Command::NetworkOpen | Command::ProcessesOpen(_) => {
                if let Some(app) = self.build_app(&cmd) {
                    self.open_app(app);
                }
            }
            Command::UpdateOpen => self.open_update(area),
            Command::SettingsOpen | Command::ClipboardOpen => {
                if let Some(app) = self.build_app(&cmd) {
                    self.open_app(app);
                }
            }
            Command::ClipboardClear => {
                if let Ok(mut h) = self.opts.clipboard.lock() {
                    h.clear();
                }
                self.notify(Level::Info, "clipboard history cleared (pins kept)");
            }
            Command::AgentOpen(arg) => self.launch(termos_core::app::LaunchKind::Agent, &arg),
            Command::AgentResume(arg) => {
                self.launch(termos_core::app::LaunchKind::AgentResume, &arg)
            }
            Command::AuditOpen(arg) => self.launch(termos_core::app::LaunchKind::Audit, &arg),
            Command::CenterOpen => self.launch(termos_core::app::LaunchKind::Center, ""),
            Command::PackagesOpen => self.launch(termos_core::app::LaunchKind::Packages, ""),
            Command::PkgRun(args) => {
                let mut a = vec!["pkg"];
                a.extend(args.split_whitespace());
                a.push("--wait");
                self.open_termos_window(&a, &format!("packages: {args}"), area);
            }
            Command::DevrootCreate => {
                self.open_termos_window(
                    &["devroot", "create", "--wait"],
                    "dev rootfs: create",
                    area,
                );
            }
            Command::DevrootEnter => {
                self.open_termos_window(&["devroot", "enter"], "dev rootfs", area);
            }
            Command::WorktreeNew(title) => {
                let mut args = vec!["worktree", "new", "--open"];
                if !title.is_empty() {
                    args.extend(["--title", title.as_str()]);
                }
                self.open_termos_window(&args, "new worktree", area);
            }
            Command::WorktreeOpen(name) => self.open_worktree(&name, area),
            Command::HarnessBest(arg) => {
                self.launch(termos_core::app::LaunchKind::HarnessBest, &arg);
            }
            Command::AgentWaiting => self.jump_to_waiting(),
            Command::PromptPrev | Command::PromptNext => {
                if self.enter_copy_mode(false) {
                    let c = if cmd == Command::PromptPrev { '[' } else { ']' };
                    self.copy_key(KeyEvent::plain(termos_core::input::KeyCode::Char(c)));
                }
            }
            Command::CopyOutput => self.copy_last_output(),
            Command::Hints(kind) => self.start_hints(&kind),
            Command::SearchAll(all) => self.start_search(all),
            Command::NotifyWhenDone => self.arm_when_done(),
            Command::LayoutOpen(arg) => self.open_layout(&arg),
            Command::LayoutSave(name) => self.save_layout(&name),
            Command::QuickToggle => self.toggle_quick(),
            Command::WorkspaceSync => self.toggle_workspace_sync(),
            Command::PaneSync => self.toggle_pane_sync(),
            Command::SnippetRun(name) => self.run_snippet(&name),
            Command::SshOpen(host) => self.open_ssh(&host),
            Command::PaneRecord => self.toggle_record(),
            Command::SessionLock => self.lock_screen(),
            Command::SystemSuspend => self.suspend(),
            Command::SystemRestart => self.ask_power_off(crate::prompt::PowerOff::Restart),
            Command::SystemPowerOff => self.ask_power_off(crate::prompt::PowerOff::PowerOff),
            Command::DisplayBrightness(d) => self.change_brightness(d),
            Command::ClockZone(z) => self.set_time_zone(&z),
            Command::WorktreeReview(name) => {
                // A review of it already open comes forward instead.
                let open = (!name.is_empty())
                    .then(|| format!("worktree.review {name}"))
                    .and_then(|want| {
                        self.panes.iter().find_map(|(id, p)| match &p.kind {
                            PaneKind::App(app)
                                if app.reopen().as_deref() == Some(want.as_str()) =>
                            {
                                Some(*id)
                            }
                            _ => None,
                        })
                    });
                match open.and_then(|id| Some((id, self.session.find(id)?))) {
                    Some((id, w)) => {
                        self.session.select(w);
                        self.session.active_mut().set_focus(id);
                    }
                    None => self.launch(termos_core::app::LaunchKind::WorktreeReview, &name),
                }
            }
            Command::WorktreeRun(rest) => {
                let mut args = vec!["worktree", "--wait"];
                args.extend(rest.split_whitespace());
                self.open_termos_window(&args, &format!("worktree {rest}"), area);
            }
            Command::AgentExternal(name) => self.open_external_agent(&name, area),
            Command::AgentInstall(name) => {
                self.open_termos_window(
                    &["agent", "install", &name, "--wait"],
                    &format!("install {name}"),
                    area,
                );
            }
            Command::AccountUse(arg) => self.use_account(&arg),
            Command::UsageOpen => self.launch(termos_core::app::LaunchKind::Usage, ""),
            Command::HarnessOpen(goal) => {
                self.launch(termos_core::app::LaunchKind::Harness, &goal);
            }
            Command::AccountTask(task) => {
                let mut args = vec!["account", "--wait"];
                args.extend(task.split_whitespace());
                self.open_termos_window(&args, &format!("account {task}"), area);
            }
            Command::AgentSwap(account) => self.swap_agent(&account, area),
            Command::AgentUpdate => {
                self.open_termos_window(&["agent", "update", "--wait"], "agent updates", area);
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
                // `PATH:LINE[:COL]` opens the editor at the line (D-118),
                // unless that is itself the name of a file.
                let (file, line, column) = if self.resolve_path(&path).exists() {
                    (path.as_str(), None, None)
                } else {
                    termos_term::links::split_line(&path)
                };
                let path = self.resolve_path(file);
                let (program, mut args) = self.edit_command();
                let editor = if args.first().is_some_and(|a| a == "edit")
                    && self
                        .termos_exe()
                        .is_some_and(|e| e.as_os_str() == program.as_str())
                {
                    // `termos edit` starts TermOS's Neovim or Helix.
                    if self.config.dev.editor == termos_core::config::EditorChoice::Helix {
                        "hx".to_owned()
                    } else {
                        "nvim".to_owned()
                    }
                } else {
                    program.clone()
                };
                args.extend(editor_args(
                    &editor,
                    &path.display().to_string(),
                    line,
                    column,
                ));
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
            Command::FileOpenSystem(path) => {
                let path = self.resolve_path(&path);
                self.open_system(&path);
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
                | Command::FileEdit(_)
                | Command::FileOpenSystem(_)
                | Command::LayoutOpen(_)
                | Command::LayoutSave(_)
                | Command::SnippetRun(_)
                | Command::SshOpen(_) => continue,
                // `task <name>` is typed; build, run and test are listed.
                Command::Task(ref t) if !matches!(t.as_str(), "build" | "run" | "test") => continue,
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
        // Layouts, saved commands and SSH hosts by name (D-120).
        let named: Vec<Command> = self
            .layout_names()
            .into_iter()
            .map(Command::LayoutOpen)
            .chain(
                self.all_snippets()
                    .into_iter()
                    .map(|s| Command::SnippetRun(s.name)),
            )
            .chain(self.ssh_hosts().into_iter().map(Command::SshOpen))
            .collect();
        for cmd in named {
            items.push(PaletteItem {
                label: cmd.description(),
                command: cmd.to_string(),
                hint: String::new(),
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
                self.sync_wallpaper();
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
        // Menus, the palette and the key list take the mouse first.
        if self.menu_mouse(m) {
            self.ui.pointer = Some((m.x, m.y));
            return;
        }
        if self.ui.palette.is_some() && self.palette_mouse(m) {
            return;
        }
        if self.whichkey_mouse(m) {
            return;
        }
        // A press anywhere closes the search box and hint mode.
        if matches!(m.kind, MouseKind::Down(_))
            && (self.ui.search.is_some() || self.ui.hints.is_some())
        {
            self.ui.search = None;
            self.ui.hints = None;
            self.dirty = true;
            return;
        }
        // Moving the pointer redraws only when what it is over changes.
        if m.kind == MouseKind::Moved && self.ui.drag.is_none() {
            self.link_hover_moved(&m);
            self.pointer_moved(m);
            return;
        }
        // Ctrl+click on a link or a path opens it (D-118).
        if m.kind == MouseKind::Down(MouseButton::Left)
            && self.ui.drag.is_none()
            && self.link_click(&m)
        {
            return;
        }
        self.dirty = true;
        // Status line.
        if m.y == self.status_y() && self.ui.drag.is_none() {
            if let MouseKind::Down(MouseButton::Right) = m.kind {
                self.status_context(m);
            } else if let MouseKind::Down(MouseButton::Left) = m.kind {
                let double = self.double_click(&m);
                if self.ui.status_regions.start_at(m.x, m.y) {
                    self.open_start_menu();
                } else if self.ui.status_regions.new_tab_at(m.x, m.y) {
                    self.run(Command::WorkspaceNew);
                } else if self.ui.status_regions.message_at(m.x, m.y) {
                    self.run(Command::Notifications);
                } else if let Some(idx) = self.ui.status_regions.tab_at(m.x, m.y) {
                    self.session.select(idx);
                    if double {
                        self.run(Command::WorkspaceRename);
                    }
                } else if let Some(p) = self.ui.status_regions.minimized_at(m.x, m.y) {
                    self.session.active_mut().toggle_minimize(p);
                } else if let Some((i, column)) = self.ui.status_regions.widget_at(m.x, m.y)
                    && let Some((name, _)) = i
                        .checked_sub(self.plugin_status.len())
                        .and_then(|j| self.wasm_widgets.get(j))
                {
                    self.requests.push(SessionRequest::WidgetClick {
                        name: name.clone(),
                        column,
                    });
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
            match hit {
                Hit::Pane { pane, .. } if self.pane_wants_mouse(pane, &m) => {
                    self.ui.mouse_owner = Some(pane);
                    self.forward_mouse(pane, m);
                }
                // Right-click (Shift+right-click over programs that use
                // the mouse): what can be done here.
                _ if button == MouseButton::Right => {
                    self.layout_context(m, &hit);
                }
                Hit::Pane { pane, .. } if button == MouseButton::Middle => {
                    self.session.active_mut().set_focus(pane);
                    self.run(Command::Paste);
                }
                _ => {}
            }
            return;
        }
        let double = self.double_click(&m);
        match hit {
            Hit::Nothing => {
                if let Some((_, command)) = self.wallpaper_at(m.x, m.y) {
                    self.run(command);
                }
            }
            Hit::FloatButton(pane, Button::Close) => {
                self.session.active_mut().set_focus(pane);
                self.run(Command::KillPane);
            }
            Hit::FloatButton(pane, Button::Minimize) => {
                self.session.active_mut().toggle_minimize(pane)
            }
            Hit::FloatTitle(pane) => {
                self.session.active_mut().set_focus(pane);
                if double {
                    self.toggle_maximize();
                    return;
                }
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
            self.write_quietly(pane, bytes.repeat(3));
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
                let mut notices = Vec::new();
                let mut finished = Vec::new();
                for ev in vt.take_events() {
                    match ev {
                        VtEvent::Notify(n) => notices.push(n),
                        VtEvent::Finished(f) => finished.push(f),
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
                            // The program, else the pane's title.
                            let name = pty.foreground_name().unwrap_or_else(|| pane.title.clone());
                            let source = ClipSource::Program(name);
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
                for n in notices {
                    self.program_notice(id, &n.title, &n.body);
                }
                if self.armed.contains_key(&id) {
                    self.armed_output(id);
                }
                if self.recordings.contains_key(&id) {
                    self.record_output(id, &bytes);
                }
                for f in finished {
                    self.command_finished(id, f);
                }
                if visible {
                    self.output_since_render += bytes.len();
                    self.dirty = true;
                }
            }
            PtyEvent::Wake(id) => {
                if self.is_wallpaper(id) {
                    self.wallpaper_woke();
                } else if self.is_search_wake(id) {
                    self.search_woke();
                } else if self.panes.contains_key(&id) {
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
        t.extend(self.battery_deadline);
        t.extend(self.critical_at);
        if let Some(d) = self.armed_deadline() {
            t.push(d);
        }
        if let Some(d) = self.anim.deadline() {
            t.push(d);
        }
        t.extend(self.app_deadlines().into_iter().map(|(_, d)| d));
        if self.spinning() {
            // The next turn, on the spinner's own beat.
            let step = termos_render::icons::SPINNER_FRAME;
            let since = Instant::now().saturating_duration_since(self.spin_epoch);
            let n = u32::try_from(since.as_millis() / step.as_millis()).unwrap_or(u32::MAX);
            t.push(self.spin_epoch + step * n.saturating_add(1));
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

    /// Whether a busy window is on screen for a client that animates: its
    /// spinner turns, so frames are due (D-090). Otherwise no timer.
    fn spinning(&self) -> bool {
        if !self.any_client_animates() {
            return false;
        }
        let busy: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.busy())
            .map(|(id, _)| *id)
            .collect();
        if busy.is_empty() {
            return false;
        }
        let visible = self.placement().visible();
        busy.iter().any(|id| visible.contains(id))
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
        if self.battery_deadline.is_some_and(|d| d <= now) {
            self.read_battery(now);
        }
        self.critical_timer(now);
        if self.armed_deadline().is_some_and(|d| d <= now) {
            self.armed_timer(now);
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
        if self.spinning() {
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
        let mut resized = Vec::new();
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
                resized.push((id, cols, rows));
            }
        }
        for (id, cols, rows) in resized {
            self.record_resize(id, cols, rows);
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
                attention: {
                    let needs: Vec<Need> = ws
                        .panes()
                        .iter()
                        .filter_map(|p| self.panes.get(p)?.attention.as_ref().map(|a| a.need))
                        .collect();
                    if needs.contains(&Need::You) {
                        Some(Need::You)
                    } else if i != active && needs.contains(&Need::Failed) {
                        Some(Need::Failed)
                    } else if i != active && needs.contains(&Need::Done) {
                        Some(Need::Done)
                    } else {
                        None
                    }
                },
            })
            .collect();
        let minimized = self
            .session
            .active()
            .minimized()
            .into_iter()
            // The hidden drop-down terminal is not a minimized window.
            .filter(|p| !self.is_quick(*p))
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
            clock: self.config.status.clock.then(|| {
                jiff::Zoned::now()
                    .strftime(self.config.clock.time_format.pattern())
                    .to_string()
            }),
            battery: self.battery.clone(),
            widgets: &self.status_widgets,
            prompt: self
                .ui
                .prompt
                .as_ref()
                .map(|p| (p.label.as_str(), p.input.as_str())),
            start_button: true,
            pointer: self.status_pointer(),
            sync: !self.sync.is_empty(),
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
        // Links to the terminal around TermOS, where it shows them (D-118).
        let links = self.config.ui.links && links_shown(&self.ui.caps);
        grid.links_mut().set_enabled(links);
        let cursor = self.render_current(grid, now);
        grid.forget_unused_links();
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
        // The active workspace's panes have been seen: clear their bells
        // and finished turns.
        for id in self.session.active().panes() {
            if let Some(p) = self.panes.get_mut(&id) {
                p.bell = false;
                if p.attention.as_ref().is_some_and(|a| a.need.quiet()) {
                    p.attention = None;
                }
            }
        }

        // Apps take in finished work first, so titles and spinners are
        // current.
        for id in placement.floats.iter().map(|f| f.pane) {
            if let Some(Pane {
                kind: PaneKind::App(app),
                ..
            }) = self.panes.get_mut(&id)
            {
                app.catch_up();
            }
        }
        // A busy window's icon turns into a spinner (still where this
        // client does not animate).
        let frame = if Self::animates(&self.config, &self.ui) {
            let ticks = now.saturating_duration_since(self.spin_epoch).as_millis()
                / termos_render::icons::SPINNER_FRAME.as_millis();
            usize::try_from(ticks).unwrap_or(0)
        } else {
            0
        };
        let set = self.ui.theme.icons;
        let titles: BTreeMap<PaneId, String> = placement
            .floats
            .iter()
            .filter_map(|f| {
                self.panes.get(&f.pane).map(|p| {
                    let icon = if p.busy() {
                        format!("{} ", termos_render::icons::spinner(set, frame))
                    } else {
                        p.icon().prefix(set)
                    };
                    (f.pane, format!("{icon}{}", p.title_for_border()))
                })
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
        // No tiled panes: the wallpaper shows (D-089).
        self.ui.wallpaper_regions = if placement.tiled.is_empty() {
            self.draw_wallpaper(grid, area, &mut candidates, protocol, cell, image_bg)
        } else {
            Vec::new()
        };
        {
            let panes = &mut self.panes;
            let theme = &self.ui.theme;
            let copy = &self.copy;
            let hints = &self.ui.hints;
            let link_hover = &self.ui.link_hover;
            let block_cache = &mut self.block_cache;
            let candidates = &mut candidates;
            let waiting: Vec<PaneId> = panes
                .values()
                .filter(|p| p.attention.as_ref().is_some_and(|a| a.need == Need::You))
                .map(|p| p.id)
                .collect();
            let ring_style = Style::new().fg(theme.colors.warn);
            // Panes typed into at once are ringed in the accent (D-120).
            let synced = self.sync.clone();
            let sync_style = Style::new().fg(theme.colors.accent);
            let recording: Vec<PaneId> = self.recordings.keys().copied().collect();
            compose(
                grid,
                &placement,
                focus,
                theme,
                &opts,
                &|p| titles.get(&p).cloned().unwrap_or_default(),
                &|p| {
                    waiting
                        .contains(&p)
                        .then_some(ring_style)
                        .or_else(|| (synced.len() > 1 && synced.contains(&p)).then_some(sync_style))
                },
                &mut |id, rect, g| {
                    let Some(pane) = panes.get_mut(&id) else {
                        return;
                    };
                    let needs_you = waiting.contains(&id);
                    match &mut pane.kind {
                        PaneKind::Term { vt, .. } => {
                            vt.render_into(g, rect);
                            if let Some(cm) = copy.as_ref().filter(|c| c.pane == id) {
                                cm.decorate(g, rect, vt.as_ref(), theme);
                            }
                            // The link under the pointer, and hint mode's
                            // labels (D-118).
                            if let Some((_, cells)) = link_hover.as_ref().filter(|(p, _)| *p == id)
                            {
                                for &(row, x0, x1) in cells {
                                    if row < rect.h && x0 < rect.w {
                                        let w = x1.min(rect.w) - x0;
                                        g.map_style(
                                            Rect::new(rect.x + x0, rect.y + row, w, 1),
                                            |s| s.attrs(termos_render::cell::Attrs::UNDERLINE),
                                        );
                                    }
                                }
                            }
                            if let Some(h) = hints.as_ref().filter(|h| h.pane == id) {
                                h.draw(g, rect, theme);
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
                                        pixels_only: false,
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
                                if img.pixels_only && protocol == ImageProtocol::Blocks {
                                    // The app's text version stays.
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
                                        pixels_only: img.pixels_only,
                                    });
                                }
                            }
                        }
                    }
                    // A badge at the top right of a pane that waits for
                    // the user (D-115): it shows where a ring cannot (a
                    // pane alone in its workspace).
                    if recording.contains(&id) {
                        let label = " \u{25cf} REC ";
                        let style = Style::new()
                            .fg(theme.colors.status_bg)
                            .bg(theme.colors.error)
                            .attrs(termos_render::cell::Attrs::BOLD);
                        g.put_str(rect.x, rect.y, label, style, rect.right());
                    }
                    if needs_you {
                        let label = " \u{25cf} needs you ";
                        let w = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
                        if rect.w >= w + 2 {
                            let style = Style::new()
                                .fg(theme.colors.status_bg)
                                .bg(theme.colors.warn)
                                .attrs(termos_render::cell::Attrs::BOLD);
                            g.put_str(rect.right() - w, rect.y, label, style, rect.right());
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

        self.draw_hover(grid, &placement);
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
            let (rect, regions) = whichkey::draw(
                grid,
                area,
                &self.ui.theme,
                &self.keys,
                self.ui.whichkey_full,
                chrome,
            );
            overlays.extend(rect);
            self.ui.whichkey_regions = regions;
        } else {
            self.ui.whichkey_regions.clear();
        }
        if let Some(p) = &self.ui.palette {
            overlays.push(p.rect(area));
        }
        let palette_cursor = self
            .ui
            .palette
            .as_ref()
            .map(|p| p.render(grid, area, &self.ui.theme, chrome));
        let search_cursor = self.ui.search.as_ref().map(|s| {
            overlays.push(crate::search::SearchBox::rect(area));
            s.render(grid, area, &self.ui.theme, chrome)
        });
        if let Some(m) = &self.ui.menu {
            overlays.extend(m.rects());
            m.render(grid, &self.ui.theme, chrome);
        }
        if animates
            && let Some(p) = self
                .current
                .and_then(|c| self.anim.linear(anim::Kind::Splash(c), now))
        {
            let screen = Rect::new(0, 0, grid.cols(), grid.rows());
            let border = self.config.floats.border.into();
            overlays.extend(crate::wallpaper::splash(
                grid,
                screen,
                &self.ui.theme,
                border,
                p,
            ));
        }
        self.ui.pixel_images = self.place_pixel_images(
            grid, &placement, &overlays, candidates, protocol, image_bg, animates,
        );

        let title = format!("TermOS · {}", self.session.active().name);
        if title != self.ui.title_sent {
            self.outbox
                .push((self.current, ServerMsg::Title(title.clone())));
            self.ui.title_sent = title;
        }
        // The keyboard flags the focused program asked for, so the
        // client can ask its terminal for them (D-103).
        let keyboard = self
            .focused()
            .and_then(|id| match self.panes.get(&id).map(|p| &p.kind) {
                Some(PaneKind::Term { vt, .. }) => Some(vt.modes().kitty_flags & (2 | 4 | 8 | 16)),
                _ => None,
            })
            .unwrap_or(0);
        if keyboard != self.ui.keyboard_sent {
            self.outbox
                .push((self.current, ServerMsg::KeyboardFlags(keyboard)));
            self.ui.keyboard_sent = keyboard;
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
        if self.ui.menu.is_some() {
            return None;
        }
        if let Some((x, y)) = palette_cursor {
            return Some(bar(x, y));
        }
        if let Some((x, y)) = search_cursor {
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
    fn program_paths_in_titles_become_names() {
        let cases = [
            (
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                "powershell",
            ),
            (
                r"Administrator: C:\Windows\system32\cmd.exe",
                "Administrator: cmd",
            ),
            (r"C:\Program Files\PowerShell\7\pwsh.exe", "pwsh"),
            ("/usr/bin/fish", "/usr/bin/fish"),
            ("user@host: ~/src", "user@host: ~/src"),
            ("vim notes.txt", "vim notes.txt"),
            ("htop", "htop"),
        ];
        for (title, short) in cases {
            assert_eq!(short_title(title), short, "{title}");
        }
    }

    #[test]
    fn editors_open_files_at_their_line() {
        let at = |p: &str, l: Option<u32>, c: Option<u32>| editor_args(p, "/a/b.rs", l, c);
        assert_eq!(at("nvim", Some(3), None), vec!["+3", "/a/b.rs"]);
        assert_eq!(
            at("/usr/bin/vim", Some(3), Some(7)),
            vec!["+call cursor(3,7)", "/a/b.rs"]
        );
        assert_eq!(at("hx", Some(3), Some(7)), vec!["/a/b.rs:3:7"]);
        assert_eq!(at("code", Some(3), None), vec!["-g", "/a/b.rs:3:1"]);
        assert_eq!(at("Code.exe", Some(3), None), vec!["-g", "/a/b.rs:3:1"]);
        assert_eq!(at("nano", Some(3), Some(2)), vec!["+3,2", "/a/b.rs"]);
        assert_eq!(at("ed", Some(3), None), vec!["/a/b.rs"]);
        assert_eq!(at("nvim", None, None), vec!["/a/b.rs"]);
    }

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
