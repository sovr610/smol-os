// SPDX-License-Identifier: GPL-3.0-or-later
//! `config.toml`: parsing, validation and precise error locations.
//!
//! Key bindings are parsed here only as far as their key notation; command
//! names are validated by the shell, which owns the command registry.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use termos_render::caps::ImageProtocol;
use termos_render::grid::BorderKind;
use termos_render::icons::IconSet;
use termos_render::theme::ThemeConfig;

use crate::input::KeyEvent;

pub mod agents;
pub mod harness;
pub mod kms;
pub mod models;
pub mod worktrees;

pub use agents::{AgentsConfig, SecretsConfig};
pub use harness::HarnessConfig;
pub use kms::KmsConfig;
pub use models::ModelsConfig;
pub use worktrees::WorktreesConfig;

/// The whole configuration. Every field has a default, so an empty or
/// missing file is valid.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// The prefix key (tmux notation).
    #[serde(deserialize_with = "de_key")]
    pub prefix: KeyEvent,
    /// Mouse support (click to focus, drag to resize/move, wheel scroll).
    pub mouse: bool,
    /// Program for new panes; default `$SHELL`, then `/bin/sh`. Either
    /// `shell = "/bin/zsh"` or a `[shell]` table that can also turn
    /// TermOS's shell integration off (D-115).
    pub shell: ShellConfig,
    /// Scrollback lines kept per pane.
    pub scrollback: usize,
    /// How long a repeatable binding (resize) stays active after the
    /// prefix, in milliseconds (tmux `repeat-time`).
    pub repeat_time_ms: u64,
    pub keys: KeysConfig,
    pub status: StatusConfig,
    pub whichkey: WhichKeyConfig,
    pub tutorial: TutorialConfig,
    pub floats: FloatConfig,
    pub theme: ThemeConfig,
    pub ui: UiConfig,
    pub graphics: GraphicsConfig,
    pub session: SessionConfig,
    pub plugins: PluginsConfig,
    pub clipboard: ClipboardConfig,
    /// WASM status-line widgets by name (D-013, D-051).
    pub widgets: std::collections::BTreeMap<String, WidgetConfig>,
    pub dev: DevConfig,
    /// AI agents (M6, D-059).
    pub agents: AgentsConfig,
    /// Where API keys are kept (D-058).
    pub secrets: SecretsConfig,
    /// Local models (M7, D-065).
    pub models: ModelsConfig,
    /// The bare-metal console (M8, D-073).
    pub kms: KmsConfig,
    /// The command center's history (M10c, D-092).
    pub center: CenterConfig,
    /// Notifications outside TermOS (M12b, D-115) and when long commands
    /// finish (M13a, D-118).
    pub notifications: NotificationsConfig,
    /// Hint mode's labels (M13a, D-118).
    pub hints: HintsConfig,
    /// The drop-down terminal (M13b, D-120).
    pub quick: QuickConfig,
    /// Pane recordings (M13b, D-120).
    pub record: RecordConfig,
    /// Saved commands (M13b, D-120).
    pub snippets: Vec<crate::snippets::Snippet>,
    /// The file manager (M13c, D-122).
    pub files: FilesConfig,
    /// The process manager (M13c, D-122).
    pub processes: ProcessesConfig,
    /// The lock screen on TermOS images (M13d, D-124).
    pub lock: LockConfig,
    /// Battery, brightness, the lid and idle suspend (M13d, D-124).
    pub power: PowerConfig,
    /// Time sync and the status clock's format (M13d, D-124).
    pub clock: ClockConfig,
    /// The gallery of themes, layouts and widgets (M13g, D-126).
    pub gallery: GalleryConfig,
    /// The coding harness (M11c, D-099).
    pub harness: HarnessConfig,
    /// A git worktree per task (M12a, D-113).
    pub worktrees: WorktreesConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: KeyEvent::ctrl('b'),
            mouse: true,
            shell: ShellConfig::default(),
            scrollback: 10_000,
            repeat_time_ms: 500,
            keys: KeysConfig::default(),
            status: StatusConfig::default(),
            whichkey: WhichKeyConfig::default(),
            tutorial: TutorialConfig::default(),
            floats: FloatConfig::default(),
            theme: ThemeConfig::default(),
            ui: UiConfig::default(),
            graphics: GraphicsConfig::default(),
            session: SessionConfig::default(),
            plugins: PluginsConfig::default(),
            clipboard: ClipboardConfig::default(),
            widgets: std::collections::BTreeMap::new(),
            dev: DevConfig::default(),
            agents: AgentsConfig::default(),
            secrets: SecretsConfig::default(),
            models: ModelsConfig::default(),
            kms: KmsConfig::default(),
            center: CenterConfig::default(),
            notifications: NotificationsConfig::default(),
            hints: HintsConfig::default(),
            quick: QuickConfig::default(),
            record: RecordConfig::default(),
            snippets: Vec::new(),
            files: FilesConfig::default(),
            processes: ProcessesConfig::default(),
            lock: LockConfig::default(),
            power: PowerConfig::default(),
            clock: ClockConfig::default(),
            gallery: GalleryConfig::default(),
            harness: HarnessConfig::default(),
            worktrees: WorktreesConfig::default(),
        }
    }
}

/// Binding overrides: key (tmux notation) → command, or `"none"` to unbind.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysConfig {
    /// Bindings that follow the prefix key.
    #[serde(deserialize_with = "de_bindings")]
    pub prefix: Vec<(KeyEvent, String)>,
    /// Bindings without the prefix (tmux `bind -n`).
    #[serde(deserialize_with = "de_bindings")]
    pub root: Vec<(KeyEvent, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusPosition {
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct StatusConfig {
    pub position: StatusPosition,
    /// Show a clock (wakes the desktop once a minute; D-028).
    pub clock: bool,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            position: StatusPosition::Bottom,
            clock: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WhichKeyConfig {
    pub enabled: bool,
    /// Pause after the prefix before the hint overlay appears.
    pub delay_ms: u64,
}

impl Default for WhichKeyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TutorialConfig {
    pub show_on_first_run: bool,
}

impl Default for TutorialConfig {
    fn default() -> Self {
        Self {
            show_on_first_run: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BorderStyle {
    #[default]
    Rounded,
    Square,
    Heavy,
    Double,
}

impl From<BorderStyle> for BorderKind {
    fn from(b: BorderStyle) -> Self {
        match b {
            BorderStyle::Rounded => BorderKind::Rounded,
            BorderStyle::Square => BorderKind::Square,
            BorderStyle::Heavy => BorderKind::Heavy,
            BorderStyle::Double => BorderKind::Double,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FloatConfig {
    pub shadow: bool,
    pub border: BorderStyle,
}

impl Default for FloatConfig {
    fn default() -> Self {
        Self {
            shadow: true,
            border: BorderStyle::Rounded,
        }
    }
}

/// How a session's size follows its clients (tmux `window-size`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSize {
    /// The most recently active client's size.
    #[default]
    Latest,
    /// The smallest attached client (nothing is ever clipped).
    Smallest,
    /// The largest attached client (smaller clients see a clipped view).
    Largest,
}

/// What a fresh server does with saved sessions (D-033).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestorePolicy {
    /// Restore without asking.
    Always,
    /// Ask in the status line of the first attached client.
    #[default]
    Ask,
    /// Keep snapshots but restore only on request (`termos restore`).
    Never,
}

/// Sessions: sizing, snapshots and restore.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SessionConfig {
    pub window_size: WindowSize,
    pub restore: RestorePolicy,
    /// Programs that are started again when a session is restored (by
    /// command name). Anything else running at save time is only listed.
    pub restore_programs: Vec<String>,
    /// Also save each pane's scrollback text (stored with mode 0600).
    pub save_scrollback: bool,
    /// How many lines of scrollback to save per pane.
    pub scrollback_lines: usize,
    /// Snapshots are written this long after the last change.
    pub snapshot_delay_ms: u64,
    /// Claude Code, Codex and Gemini CLI come back in their panes,
    /// continuing their conversations (D-115).
    pub restore_agents: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            window_size: WindowSize::Latest,
            restore: RestorePolicy::Ask,
            restore_programs: ["vim", "nvim", "less", "man", "htop", "top"]
                .map(String::from)
                .to_vec(),
            save_scrollback: false,
            scrollback_lines: 2000,
            snapshot_delay_ms: 2000,
            restore_agents: true,
        }
    }
}

/// What empty parts of the desktop show (`[ui] wallpaper`, D-087).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Wallpaper {
    /// A pattern drawn from the theme's colours.
    #[default]
    Pattern,
    None,
    /// A picture (shown in pixels where the terminal can, in blocks
    /// elsewhere).
    Picture(PathBuf),
}

impl<'de> Deserialize<'de> for Wallpaper {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "pattern" => Wallpaper::Pattern,
            "none" | "" => Wallpaper::None,
            path => Wallpaper::Picture(PathBuf::from(path)),
        })
    }
}

/// How a new session or workspace starts (`[ui] start`, D-091).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartLook {
    /// On the desktop: the wallpaper, with the terminal in a floating
    /// window.
    #[default]
    Desktop,
    /// One terminal filling the screen, as in tmux.
    Terminal,
}

/// The shell new panes start (`shell`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    /// The program; `None`: `$SHELL`, then the platform's default.
    pub program: Option<String>,
    /// Start bash, zsh, fish and PowerShell with TermOS's integration
    /// script, which marks prompts, commands and their exit status (OSC
    /// 133) after loading your own configuration (D-115, D-116).
    pub integration: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            program: None,
            integration: true,
        }
    }
}

impl<'de> Deserialize<'de> for ShellConfig {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "kebab-case")]
        struct Table {
            program: Option<String>,
            integration: Option<bool>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Program(String),
            Table(Table),
        }
        match Raw::deserialize(d).map_err(|_| {
            serde::de::Error::custom(
                "`shell` is a program (`shell = \"/bin/zsh\"`) or a table with `program` and `integration`",
            )
        })? {
            Raw::Program(p) => Ok(Self {
                program: Some(p),
                integration: true,
            }),
            Raw::Table(t) => Ok(Self {
                program: t.program,
                integration: t.integration.unwrap_or(true),
            }),
        }
    }
}

/// Notifications outside TermOS (`[notifications]`, D-115).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct NotificationsConfig {
    /// Send "needs you" to the terminal TermOS runs in (OSC 9, 99 or
    /// 777), which may show it as a system notification.
    pub system: bool,
    /// A command that ran at least this many seconds and finishes where
    /// no one is looking says so (D-118); 0 turns it off.
    pub long_command: u64,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            system: true,
            long_command: 15,
        }
    }
}

/// The file manager (`[files]`, D-122).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FilesConfig {
    /// Folders in the sidebar and the `'` list (`b` adds the one shown).
    pub bookmarks: Vec<PathBuf>,
    /// Open with two panes side by side.
    pub two_pane: bool,
}

/// The process manager (`[processes]`, D-122).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProcessesConfig {
    /// Ask before ending, killing, pausing or renicing a process.
    pub confirm: bool,
}

impl Default for ProcessesConfig {
    fn default() -> Self {
        Self { confirm: true }
    }
}

/// The lock screen (`[lock]`, D-124). Only TermOS images lock: in a
/// terminal, the host system's lock does (D-123).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LockConfig {
    /// Lock after this many minutes without input (0: never).
    pub after_minutes: u32,
    /// Start locked, so the console asks for the password like a login.
    pub at_start: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            after_minutes: 10,
            at_start: false,
        }
    }
}

/// What closing a laptop's lid does (`[power] lid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LidAction {
    /// Lock, then suspend.
    #[default]
    Suspend,
    Lock,
    Nothing,
}

/// Battery, brightness, the lid and idle (`[power]`, D-124).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct PowerConfig {
    pub lid: LidAction,
    /// On battery, suspend after this many minutes without input (0:
    /// never).
    pub suspend_after_minutes: u32,
    /// At this charge (percent) the image suspends, after a 30-second
    /// countdown (0: never). A toast comes at 20% and a notification at
    /// 10% before it.
    pub critical: u8,
    /// The screen's brightness at start, in percent (none: as it was).
    pub brightness: Option<u8>,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            lid: LidAction::Suspend,
            suspend_after_minutes: 20,
            critical: 5,
            brightness: None,
        }
    }
}

/// How the status clock shows the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum TimeFormat {
    /// `14:05`.
    #[default]
    #[serde(rename = "24h")]
    H24,
    /// `2:05 PM`.
    #[serde(rename = "12h")]
    H12,
}

impl TimeFormat {
    /// The `strftime` pattern for the status clock.
    pub fn pattern(self) -> &'static str {
        match self {
            TimeFormat::H24 => "%H:%M",
            TimeFormat::H12 => "%-I:%M %p",
        }
    }
}

/// Time sync and the clock (`[clock]`, D-124).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ClockConfig {
    /// NTP servers the image's time sync asks (BusyBox `ntpd`).
    pub ntp_servers: Vec<String>,
    pub time_format: TimeFormat,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            ntp_servers: vec!["pool.ntp.org".into()],
            time_format: TimeFormat::H24,
        }
    }
}

/// The gallery (`[gallery]`, D-126).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct GalleryConfig {
    /// Where the signed index is (an `https` address, or a local file for
    /// a gallery's maintainers). Its entries are checked against the keys
    /// built into TermOS wherever it comes from.
    pub index: String,
}

impl Default for GalleryConfig {
    fn default() -> Self {
        Self {
            index: GALLERY_INDEX.to_owned(),
        }
    }
}

/// The gallery's index on GitHub.
pub const GALLERY_INDEX: &str =
    "https://raw.githubusercontent.com/sovr610/termos-gallery/main/index.json";

/// Whether `name` can be an NTP server's name or address: letters,
/// digits, dots, hyphens and colons (IPv6), as `ntpd -p` takes them.
pub fn valid_ntp_server(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

/// The drop-down terminal (`[quick]`, D-120).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct QuickConfig {
    /// Its height, a share of the screen: `"40%"`.
    #[serde(deserialize_with = "de_share")]
    pub height: u8,
    /// A key that opens and hides it without the prefix (none by default).
    #[serde(deserialize_with = "de_opt_key")]
    pub key: Option<KeyEvent>,
}

impl Default for QuickConfig {
    fn default() -> Self {
        Self {
            height: 40,
            key: None,
        }
    }
}

/// Pane recordings (`[record]`, D-120).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct RecordConfig {
    /// Where recordings go (default `~/.local/state/termos/recordings`).
    pub dir: Option<PathBuf>,
}

fn de_share<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let s = String::deserialize(d)?;
    crate::layouts::parse_size(&s)
        .map(|f| (f * 100.0).round().clamp(10.0, 100.0) as u8)
        .ok_or_else(|| serde::de::Error::custom(format!("{s:?} is not a share like \"40%\"")))
}

fn de_opt_key<'de, D: Deserializer<'de>>(d: D) -> Result<Option<KeyEvent>, D::Error> {
    let s = String::deserialize(d)?;
    if s.trim().is_empty() || s.trim() == "none" {
        return Ok(None);
    }
    s.parse().map(Some).map_err(serde::de::Error::custom)
}

/// Hint mode (`[hints]`, D-118).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct HintsConfig {
    /// The letters labels are made of, the easiest first: at least two,
    /// lowercase, none twice.
    #[serde(deserialize_with = "de_alphabet")]
    pub alphabet: String,
}

impl Default for HintsConfig {
    fn default() -> Self {
        Self {
            alphabet: String::from("asdfghjkl"),
        }
    }
}

fn de_alphabet<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    let mut seen = Vec::new();
    for c in s.chars() {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() {
            return Err(serde::de::Error::custom(format!(
                "hint letters must be lowercase letters or digits, not {c:?}"
            )));
        }
        if seen.contains(&c) {
            return Err(serde::de::Error::custom(format!(
                "{c:?} is in the hint letters twice"
            )));
        }
        seen.push(c);
    }
    if seen.len() < 2 {
        return Err(serde::de::Error::custom("hints need at least two letters"));
    }
    Ok(s)
}

/// Motion and looks (D-042, D-087, D-088).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct UiConfig {
    /// Animate floats opening and closing and toasts. Off anyway for
    /// clients over SSH and with `reduced-motion`.
    pub animations: bool,
    /// No animations for any client (also per terminal:
    /// `TERMOS_REDUCED_MOTION=1`).
    pub reduced_motion: bool,
    /// Icons for apps, files and menus: unicode, nerd, ascii or none.
    pub icons: IconSet,
    /// Colour gradients in title bars and the status line (truecolor
    /// terminals only; flat colours elsewhere).
    pub gradients: bool,
    /// Behind empty parts of the desktop: `pattern`, `none` or a picture.
    pub wallpaper: Wallpaper,
    /// The TermOS logo for a moment when a terminal attaches (only where
    /// animations are on).
    pub splash: bool,
    /// New sessions and workspaces start on the desktop or as a terminal.
    pub start: StartLook,
    /// Links programs print (OSC 8) are passed to the terminal TermOS
    /// runs in, which shows and opens them (D-118).
    pub links: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            animations: true,
            reduced_motion: false,
            icons: IconSet::default(),
            gradients: true,
            wallpaper: Wallpaper::default(),
            splash: true,
            start: StartLook::default(),
            links: true,
        }
    }
}

/// How images are shown (ARCHITECTURE.md §5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct GraphicsConfig {
    /// `auto` picks the best protocol each terminal supports.
    pub protocol: ProtocolSetting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProtocolSetting {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Iterm2,
    Blocks,
}

impl ProtocolSetting {
    /// The protocol to force, if any.
    pub const fn forced(self) -> Option<ImageProtocol> {
        match self {
            Self::Auto => None,
            Self::Kitty => Some(ImageProtocol::Kitty),
            Self::Sixel => Some(ImageProtocol::Sixel),
            Self::Iterm2 => Some(ImageProtocol::Iterm2),
            Self::Blocks => Some(ImageProtocol::Blocks),
        }
    }
}

/// The clipboard history (D-050): in memory only, never saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ClipboardConfig {
    /// How many copies to remember; 0 remembers none.
    pub history: usize,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            history: crate::clipboard::DEFAULT_ENTRIES,
        }
    }
}

/// How often the machine is sampled for the command center's history
/// (`[center] sample`, D-092).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum SampleEvery {
    #[default]
    #[serde(rename = "1m")]
    OneMinute,
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "off")]
    Off,
}

impl SampleEvery {
    /// The interval; `None` when off.
    pub fn interval(self) -> Option<std::time::Duration> {
        match self {
            Self::OneMinute => Some(std::time::Duration::from_secs(60)),
            Self::FiveMinutes => Some(std::time::Duration::from_secs(300)),
            Self::Off => None,
        }
    }
}

/// The command center (M10c, D-092, D-093).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct CenterConfig {
    /// Days of history kept (1 to 365).
    pub keep_days: u32,
    /// How often CPU, memory and network are recorded, also while the
    /// command center is closed: "1m", "5m" or "off".
    pub sample: SampleEvery,
    /// Claude Code's and Codex CLI's own usage: token counts, models and
    /// times read from their session logs, never their text (D-094).
    pub cli_usage: bool,
}

impl Default for CenterConfig {
    fn default() -> Self {
        Self {
            keep_days: 7,
            sample: SampleEvery::OneMinute,
            cli_usage: true,
        }
    }
}

/// The developer environment (M5a, D-054).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct DevConfig {
    /// The editor `file.edit` and `termos edit` start.
    pub editor: EditorChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditorChoice {
    /// `$VISUAL` or `$EDITOR` when set, else TermOS's Neovim.
    #[default]
    Auto,
    /// TermOS's Neovim, with the language servers `termos lang` installed.
    TermosNvim,
    /// Helix, with the same servers.
    Helix,
    /// Always `$VISUAL` or `$EDITOR`.
    Env,
}

/// One WASM status-line widget (`[widgets.<name>]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct WidgetConfig {
    /// The component (`.wasm`); relative paths are relative to the config
    /// file's directory.
    pub path: PathBuf,
    /// Folders the widget may read (nothing else on disk).
    #[serde(default)]
    pub dirs: Vec<PathBuf>,
    /// Text for the widget's `init` (its own options).
    #[serde(default)]
    pub settings: std::collections::BTreeMap<String, String>,
    /// The most columns it may take in the status line.
    #[serde(default = "default_widget_width")]
    pub max_width: u16,
}

fn default_widget_width() -> u16 {
    24
}

/// Out-of-process plugins (D-034). Only plugins listed here are started.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct PluginsConfig {
    pub enabled: Vec<String>,
    /// Run plugins in the agent sandbox where there is one (D-064): their
    /// own data folder writable, the network only with the manifest's
    /// `network` permission.
    pub sandbox: bool,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: Vec::new(),
            sandbox: true,
        }
    }
}

fn de_key<'de, D: Deserializer<'de>>(d: D) -> Result<KeyEvent, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

fn de_bindings<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(KeyEvent, String)>, D::Error> {
    let raw = std::collections::BTreeMap::<String, String>::deserialize(d)?;
    raw.into_iter()
        .map(|(k, v)| {
            k.parse::<KeyEvent>()
                .map(|key| (key, v))
                .map_err(serde::de::Error::custom)
        })
        .collect()
}

/// A configuration problem, with a location when there is one.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: cannot read: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}:{line}:{col}: {message}", path.display())]
    Parse {
        path: PathBuf,
        line: usize,
        col: usize,
        message: String,
    },
}

impl Config {
    /// Parses config text; `path` is only used in error messages.
    pub fn parse(src: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut cfg: Self = toml::from_str(src).map_err(|e| {
            let (line, col) = e.span().map_or((1, 1), |span| line_col(src, span.start));
            ConfigError::Parse {
                path: path.to_owned(),
                line,
                col,
                message: e.message().trim().to_owned(),
            }
        })?;
        cfg.load_theme_file(path);
        Ok(cfg)
    }

    /// `[theme] name`: the colours of `themes/NAME.toml` beside the
    /// config file, under the ones the config sets (D-126). A missing or
    /// broken theme file leaves the preset, with a warning in the log.
    fn load_theme_file(&mut self, path: &Path) {
        let Some(name) = self.theme.name.clone() else {
            return;
        };
        let plain = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if !plain {
            tracing::warn!("[theme] name = {name:?} is not a theme's name");
            return;
        }
        let file = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("themes")
            .join(format!("{name}.toml"));
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ThemeFile {
            theme: ThemeConfig,
        }
        match std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<ThemeFile>(&t).map_err(|e| e.message().to_owned()))
        {
            Ok(f) => self.theme.fill_from(&f.theme),
            Err(e) => tracing::warn!("theme {name} ({}): {e}", file.display()),
        }
    }

    /// Loads `path`; a missing file means the defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(src) => Self::parse(&src, path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// The program new panes run: the config's `shell`, else `$SHELL`
    /// (Unix only), else [`default_shell`].
    pub fn shell_program(&self, env_shell: Option<String>) -> String {
        self.shell
            .program
            .clone()
            .or(env_shell)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(default_shell)
    }
}

/// The shell for new panes when nothing names one: `/bin/sh` on Unix;
/// on Windows PowerShell 7, else Windows PowerShell, else `%COMSPEC%`
/// (cmd).
pub fn default_shell() -> String {
    if cfg!(windows) {
        windows_default_shell(
            std::env::var_os("PATH").as_deref(),
            std::env::var("COMSPEC").ok(),
            |p| p.is_file(),
        )
    } else {
        String::from("/bin/sh")
    }
}

/// The Windows default shell, found on `path` (the `PATH` value).
pub fn windows_default_shell(
    path: Option<&std::ffi::OsStr>,
    comspec: Option<String>,
    is_file: impl Fn(&Path) -> bool,
) -> String {
    for exe in ["pwsh.exe", "powershell.exe"] {
        let found = path
            .into_iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join(exe))
            .find(|p| is_file(p));
        if let Some(p) = found {
            return p.to_string_lossy().into_owned();
        }
    }
    comspec
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| String::from("cmd.exe"))
}

/// 1-based line and column of byte offset `pos` in `src`.
pub(crate) fn line_col(src: &str, pos: usize) -> (usize, usize) {
    let before = &src[..pos.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(before.chars().count(), |nl| {
        before[nl + 1..].chars().count()
    }) + 1;
    (line, col)
}

/// A commented example config, printed by `termos config default`.
pub const DEFAULT_CONFIG: &str = r##"# TermOS configuration (~/.config/termos/config.toml).
# Every setting is optional; the values below are the defaults.
# Changes are picked up automatically while TermOS runs (or press prefix r).

# The prefix key, in tmux notation (C- = Ctrl, M- = Alt, S- = Shift).
prefix = "C-b"

# Click to focus, drag borders to resize, drag float titles to move, wheel to scroll.
mouse = true

# Program for new panes. Default: $SHELL, then /bin/sh.
# shell = "/bin/zsh"
# Or as a table, which can also turn off the integration that marks bash's,
# zsh's, fish's and PowerShell's prompts and commands (docs/attention.md):
# [shell]
# program = "/bin/zsh"
# integration = true

# Lines of scrollback per pane.
scrollback = 10000

# How long repeatable keys (prefix C-Arrow resize) keep working without the prefix.
repeat-time-ms = 500

[status]
position = "bottom"   # or "top"
clock = true          # wakes TermOS once a minute

[whichkey]
enabled = true        # show available keys if you pause after the prefix
delay-ms = 600

[tutorial]
show-on-first-run = true

[floats]
shadow = true
border = "rounded"    # rounded | square | heavy | double

[session]
window-size = "latest"   # latest | smallest | largest: which client sets the size
restore = "ask"          # always | ask | never: saved sessions after a crash/reboot
restore-programs = ["vim", "nvim", "less", "man", "htop", "top"]  # re-run on restore
save-scrollback = false  # also save pane text (stored readable only by you)
scrollback-lines = 2000
snapshot-delay-ms = 2000
restore-agents = true    # Claude Code, Codex and Gemini CLI continue their conversation

[notifications]
system = true            # "needs you" as a system notification where the terminal shows them
long-command = 15        # seconds: a longer command that finishes out of sight says so (0: off)

[hints]
alphabet = "asdfghjkl"   # hint mode's labels (prefix u), the easiest letters first

[quick]
height = "40%"           # the drop-down terminal (prefix `), from the top
# key = "F12"            # a key that opens it without the prefix

[record]
# dir = "~/recordings"   # pane recordings (prefix R); default ~/.local/state/termos/recordings

[files]
bookmarks = []           # folders in Files' sidebar (b adds the one shown, ' lists them)
two-pane = false         # open Files with two panes (Tab switches, F5 copies, F6 moves)

[processes]
confirm = true           # ask before ending, killing, pausing or renicing a process

# TermOS images only (docs/bare-metal.md); in a terminal, the host system
# locks, sleeps and keeps time.
[lock]
after-minutes = 10       # lock after this long without input (0: never)
at-start = false         # start locked, like a login

[power]
lid = "suspend"          # suspend | lock | nothing, when a laptop's lid closes
suspend-after-minutes = 20   # on battery, suspend after this long without input (0: never)
critical = 5             # percent: suspend after a 30-second countdown (0: never)
# brightness = 70        # percent, at start

[clock]
ntp-servers = ["pool.ntp.org"]   # the image's time sync
time-format = "24h"      # 24h | 12h, for the status clock

[gallery]
# Themes, layouts and widgets, signed (termos gallery list; docs/gallery.md).
index = "https://raw.githubusercontent.com/sovr610/termos-gallery/main/index.json"

# Saved commands, in the palette as "snippet: NAME". {param} and
# {param:default} are asked for. Do not put secrets in them.
# [[snippets]]
# name = "Tail a service's log"
# command = "journalctl -fu {service:termos}"
# run = true             # press Enter after typing it
# where = "pane"         # pane | new-pane | float

[plugins]
enabled = []             # plugin directory names under ~/.config/termos/plugins
sandbox = true           # in the agent sandbox where there is one (Linux)

[center]
# The command center's history: AI usage, tool calls and the machine's load.
keep-days = 7            # days kept on disk (1 to 365)
sample = "1m"            # CPU, memory and network every "1m" or "5m", or "off"
cli-usage = true         # Claude Code's and Codex's tokens from their logs (numbers only)

[clipboard]
# Copies remembered for the clipboard manager (prefix =), newest first.
# Kept in memory only: never written to disk. 0 remembers none.
history = 50

[dev]
# The editor for files opened from TermOS (Files, `termos edit`):
#   auto        $VISUAL or $EDITOR when set, else TermOS's Neovim
#   termos-nvim TermOS's Neovim, with the language servers `termos lang`
#               installed (your ~/.config/nvim is not touched)
#   helix       Helix, with the same servers
#   env         always $VISUAL or $EDITOR
editor = "auto"

[agents]
# AI agents in windows (see docs/agents.md): palette → "agent", or prefix a.
default-profile = "default"   # the profile agent.open uses
history = true                # keep conversations (mode 0600) to resume them
# Providers. API keys are never written here: name them as keyring:<name>
# and store them with `termos secret set <name>`. Models are listed from
# each provider when a window opens.
# [agents.providers.anthropic]
# kind = "anthropic"          # anthropic | openai | kimi | openai-compatible | local
# key = "keyring:anthropic"
# base-url = "https://api.anthropic.com"
# max-tokens = 32000
# [agents.providers.anthropic.prices."<model id>"]   # US$ per million tokens
# input = 3.0
# output = 15.0
# Profiles: which provider and model, and what the agent may do. Each tool
# class is "ask", "allow" or "deny". Deleting, force-pushing, installing,
# writing outside the workspace and anything on the network always ask.
# [agents.profiles.default]
# provider = "anthropic"
# model = ""                  # empty: pick from the provider's list
# tools = { read = "allow", write = "ask", command = "ask", network = "ask", mcp = "ask" }
# allow-commands = ["cargo test*", "git status", "git diff*"]
# deny-commands = []
# domains = ["docs.rs"]       # fetch_url; empty: any domain, after asking
# mcp-servers = []
# sandbox = "auto"            # auto | required | off
# MCP servers, started in the sandbox (command) or reached over HTTP (url).
# [agents.mcp.files]
# command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "."]
# External CLI agents in terminal windows (built in: claude, codex, gemini,
# aider, opencode). sandbox = true confines them to the workspace.
# [agents.external.claude]
# command = ["claude"]
# sandbox = true
# write-paths = ["~/.claude", "~/.claude.json"]
# TermOS's MCP server for Claude Code and Codex CLI (`termos agent install`
# registers it; their CLI asks you before each call).
# [agents.termos-mcp]
# register = true
# deny = []                   # e.g. ["read_pane", "send_to_pane"]
# deny-commands = ["session.quit", "client.detach", "workspace.kill", "pane.kill"]
# Their hooks, which mark an agent that needs you or finished (docs/attention.md).
# [agents.hooks]
# register = true

# The harness (docs/harness.md): one goal planned into tasks over Claude
# Code, Codex and API models. `termos harness status` shows the routes.
# [harness]
# order = ["claude", "codex"]   # then every API backend
# fallback = true               # try the next backend when one fails
# cross-review = true           # reviews go first to another backend
# parallel = 3                  # tasks at the same time (editing ones in worktrees)
# worktrees = true              # in git: each edit in its own worktree, result on a branch
# budget-usd = 0                # API spend per plan (0: no limit)
# auto-run = 0                  # plans this small start without asking
# allow-commands = []           # besides the project's build/test/lint tasks
# [harness.backends.claude]
# models = { frontier = "…", balanced = "…", fast = "…" }   # docs/model-guide.md
# [harness.routes]
# coding = "balanced"           # planning/review: frontier; search/small-edits/docs: fast

# A git worktree per task (docs/worktrees.md): its own branch, folder, ports
# and workspace of panes. A project's setup is `[worktree]` in its termos.toml.
# [worktrees]
# root = "~/worktrees"          # default: worktrees in TermOS's data folder
# branch-prefix = "termos/"
# port-range = [20000, 29999]   # handed out in blocks
# ports = 10                    # per worktree: PORT, TERMOS_PORT_0 ... _9
# agent = "auto"                # auto | claude | codex | gemini | termos | none
# archive-merged = true         # remove the folder and branch after a merge

# TermOS's skills, which teach Claude Code, Codex and the other coding CLIs
# to use TermOS (`termos skills status`). TermOS installs and updates them.
# [agents.skills]
# install = true

[secrets]
# Where API keys live: auto | keyring | file. auto uses the secrets file if
# there is one, else the OS keyring when it works, else the file (an age
# file locked with a passphrase you choose).
store = "auto"

[models]
# Local models (see docs/models.md): `termos model pull`, then the agents'
# "local" provider or `termos models serve`.
context = 4096           # tokens of context llama.cpp gets
llama-cpp = "auto"       # auto | cpu | vulkan | cuda | rocm | metal
# llama-cpp-version = "b11960"   # the llama.cpp build to install (default: tested one)
port = 0                 # the router's port; 0 picks a free one
# cache = "~/.cache/huggingface/hub"   # shared with huggingface_hub
# cache-limit = "100G"   # `termos model gc` keeps the cache under this
# token = "keyring:huggingface"        # gated or private models (or HF_TOKEN)
# llama-server = "/opt/llama.cpp/build/bin/llama-server"   # your own build
# memory-budget = "24G"  # loaded models at most (default: 90% of RAM + VRAM)

[kms]
# The bare-metal console (`termos --kms`, see docs/bare-metal.md).
font-size = 16.0         # pixels at 96 dpi; Ctrl+= / Ctrl+- / Ctrl+0 while running
keyboard-layout = "us"   # XKB layout, as for setxkbmap
keyboard-variant = ""
keyboard-options = ""    # e.g. "ctrl:nocaps,compose:ralt"
repeat-delay = 400       # ms before a held key repeats
repeat-rate = 25         # repeats per second
# font = "/usr/share/fonts/myfont.ttf"   # default: built-in DejaVu Sans Mono
# bold-font = "/usr/share/fonts/myfont-bold.ttf"
# fallback-fonts = ["/usr/share/fonts/noto/NotoSansCJK-Regular.ttc"]

# WASM status-line widgets (see docs/widgets.md). Each runs sandboxed: no
# network, no files except the folders in `dirs` (read-only).
# [widgets.pomodoro]
# path = "widgets/pomodoro.wasm"   # relative to this file's folder
# settings = { minutes = "25" }
# dirs = []
# max-width = 24

[theme]
# auto | dark | light | high-contrast. "auto" is dark or light to match each
# terminal's background. Switch while running: palette → "theme".
base = "auto"
# name = "nord"          # a theme file, themes/NAME.toml beside this one (the gallery's)
# Override any colour of the preset: "default", "#rrggbb", "colourN"
# (0-255) or names like "brightblue".
# accent = "#6cb6ff"
# fg = "#d0d7e2"
# muted = "#7d8590"
# border = "#444c56"
# status-bg = "#1c2128"
# status-fg = "#adbac7"
# overlay-bg = "#22272e"
# selection-bg = "#2d4a6e"
# warn = "#e5a54b"
# error = "#f47067"

[ui]
animations = true        # floats and toasts; never over SSH
reduced-motion = false   # no animations at all (or TERMOS_REDUCED_MOTION=1)
icons = "unicode"        # unicode | nerd (needs a Nerd Font) | ascii | none
gradients = true         # colour gradients in title bars (truecolor terminals)
wallpaper = "pattern"    # pattern | none | a picture's path
splash = true            # the logo for a moment when you attach
start = "desktop"        # desktop (a floating terminal on the wallpaper) | terminal (full screen)
links = true             # links programs print (OSC 8) shown and opened by your terminal

[graphics]
protocol = "auto"        # auto | kitty | sixel | iterm2 | blocks

# Extra or changed bindings. Values are command names (see the palette,
# Ctrl+Space); "none" removes a default binding.
[keys.prefix]
# "v" = "pane.split-right"
# "%" = "none"

[keys.root]
# "M-Left" = "pane.focus-left"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<Config, ConfigError> {
        Config::parse(src, Path::new("config.toml"))
    }

    #[test]
    fn empty_config_is_default() {
        assert_eq!(parse("").expect("valid"), Config::default());
    }

    #[test]
    fn default_template_matches_defaults() {
        assert_eq!(parse(DEFAULT_CONFIG).expect("valid"), Config::default());
    }

    #[test]
    fn overrides_parse() {
        let c = parse(
            r#"
prefix = "C-a"
mouse = false
[status]
position = "top"
[keys.prefix]
"v" = "pane.split-right"
"%" = "none"
[keys.root]
"M-Left" = "pane.focus-left"
"#,
        )
        .expect("valid");
        assert_eq!(c.prefix, KeyEvent::ctrl('a'));
        assert!(!c.mouse);
        assert_eq!(c.status.position, StatusPosition::Top);
        assert!(
            c.keys
                .prefix
                .contains(&(KeyEvent::ch('v'), "pane.split-right".into()))
        );
        assert_eq!(c.keys.root.len(), 1);
    }

    #[test]
    fn errors_point_at_the_problem() {
        let err = parse("mouse = true\nprefix = \"C-\"\n").expect_err("bad key");
        let msg = err.to_string();
        assert!(msg.starts_with("config.toml:2:"), "{msg}");
        assert!(msg.contains("invalid key"), "{msg}");

        let err = parse("[status]\nposition = \"middle\"\n").expect_err("bad enum");
        assert!(err.to_string().starts_with("config.toml:2:"), "{err}");

        let err = parse("bogus = 1\n").expect_err("unknown key");
        assert!(err.to_string().contains("bogus"), "{err}");
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = Config::load(&dir.path().join("nope.toml")).expect("defaults");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn shell_fallbacks() {
        let c = Config::default();
        assert_eq!(c.shell_program(Some("/bin/zsh".into())), "/bin/zsh");
        assert_eq!(c.shell_program(None), default_shell());
        if cfg!(unix) {
            assert_eq!(default_shell(), "/bin/sh");
        }
        let c = Config {
            shell: ShellConfig {
                program: Some("fish".into()),
                integration: true,
            },
            ..Config::default()
        };
        assert_eq!(c.shell_program(Some("/bin/zsh".into())), "fish");
    }

    #[test]
    fn shell_as_a_program_or_a_table() {
        let c = Config::parse("shell = \"/bin/zsh\"\n", Path::new("c.toml")).expect("parses");
        assert_eq!(c.shell.program.as_deref(), Some("/bin/zsh"));
        assert!(c.shell.integration);
        let c =
            Config::parse("[shell]\nintegration = false\n", Path::new("c.toml")).expect("parses");
        assert_eq!(c.shell.program, None);
        assert!(!c.shell.integration);
        let c =
            Config::parse("[shell]\nprogram = \"fish\"\n", Path::new("c.toml")).expect("parses");
        assert_eq!(c.shell.program.as_deref(), Some("fish"));
        assert!(c.shell.integration);
        let e = Config::parse("[shell]\nintegrate = false\n", Path::new("c.toml"))
            .expect_err("unknown key");
        assert!(e.to_string().contains("`shell` is a program"), "{e}");
        let c = Config::parse(
            "[notifications]\nlong-command = 0\n[hints]\nalphabet = \"jk\"\n[ui]\nlinks = false\n",
            Path::new("c.toml"),
        )
        .expect("valid");
        assert_eq!(c.notifications.long_command, 0);
        assert_eq!(c.hints.alphabet, "jk");
        assert!(!c.ui.links);
        for bad in ["\"a\"", "\"aa\"", "\"aB\"", "\"a;\""] {
            let src = format!("[hints]\nalphabet = {bad}\n");
            assert!(Config::parse(&src, Path::new("c.toml")).is_err(), "{bad}");
        }
        let c = Config::parse(
            "[quick]\nheight = \"25%\"\nkey = \"F12\"\n[record]\ndir = \"/r\"\n[[snippets]]\nname = \"a\"\ncommand = \"ls {dir:.}\"\n",
            Path::new("c.toml"),
        )
        .expect("valid");
        assert_eq!(c.quick.height, 25);
        assert!(c.quick.key.is_some());
        assert_eq!(c.record.dir, Some(PathBuf::from("/r")));
        assert_eq!(c.snippets.len(), 1);
        assert!(Config::parse("[quick]\nheight = \"lots\"\n", Path::new("c.toml")).is_err());
        let c = Config::parse("[notifications]\nsystem = false\n", Path::new("c.toml"))
            .expect("parses");
        assert!(!c.notifications.system);
        assert!(Config::default().notifications.system);
    }

    #[test]
    fn windows_shell_order() {
        let dirs = [PathBuf::from("/w/sys"), PathBuf::from("/w/ps7")];
        let path = std::env::join_paths(&dirs).expect("joinable");
        let has =
            |names: &'static [&'static str]| move |p: &Path| names.iter().any(|n| p.ends_with(n));
        let pick = |names: &'static [&'static str], comspec: Option<&str>| {
            windows_default_shell(Some(&path), comspec.map(str::to_owned), has(names))
        };
        assert!(pick(&["ps7/pwsh.exe", "sys/powershell.exe"], None).ends_with("pwsh.exe"));
        assert!(pick(&["sys/powershell.exe"], None).ends_with("powershell.exe"));
        assert_eq!(
            pick(&[], Some(r"C:\Windows\cmd.exe")),
            r"C:\Windows\cmd.exe"
        );
        assert_eq!(pick(&[], None), "cmd.exe");
    }

    #[test]
    fn line_col_math() {
        assert_eq!(line_col("abc\ndef", 0), (1, 1));
        assert_eq!(line_col("abc\ndef", 5), (2, 2));
    }

    #[test]
    fn lock_power_and_clock() {
        let c = parse(
            "[lock]\nafter-minutes = 0\nat-start = true\n\
             [power]\nlid = \"lock\"\ncritical = 0\nbrightness = 40\n\
             [clock]\nntp-servers = [\"time.example.org\"]\ntime-format = \"12h\"\n",
        )
        .expect("valid");
        assert_eq!(c.lock.after_minutes, 0);
        assert!(c.lock.at_start);
        assert_eq!(c.power.lid, LidAction::Lock);
        assert_eq!(c.power.suspend_after_minutes, 20);
        assert_eq!(c.power.critical, 0);
        assert_eq!(c.power.brightness, Some(40));
        assert_eq!(c.clock.ntp_servers, ["time.example.org"]);
        assert_eq!(c.clock.time_format.pattern(), "%-I:%M %p");
        assert!(parse("[power]\nlid = \"hibernate\"\n").is_err());
        assert!(parse("[clock]\ntime-format = \"25h\"\n").is_err());
        assert!(valid_ntp_server("pool.ntp.org"));
        assert!(valid_ntp_server("2001:db8::1"));
        assert!(!valid_ntp_server("-q"));
        assert!(!valid_ntp_server("a b"));
        assert!(!valid_ntp_server(""));
    }
}
