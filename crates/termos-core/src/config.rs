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
use termos_render::theme::ThemeConfig;

use crate::input::KeyEvent;

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
    /// Program for new panes; default `$SHELL`, then `/bin/sh`.
    pub shell: Option<String>,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: KeyEvent::ctrl('b'),
            mouse: true,
            shell: None,
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
        }
    }
}

/// Motion (D-042).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct UiConfig {
    /// Animate floats opening and closing and toasts. Off anyway for
    /// clients over SSH and with `reduced-motion`.
    pub animations: bool,
    /// No animations for any client (also per terminal:
    /// `TERMOS_REDUCED_MOTION=1`).
    pub reduced_motion: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            animations: true,
            reduced_motion: false,
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

/// Out-of-process plugins (D-034). Only plugins listed here are started.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct PluginsConfig {
    pub enabled: Vec<String>,
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
        toml::from_str(src).map_err(|e| {
            let (line, col) = e.span().map_or((1, 1), |span| line_col(src, span.start));
            ConfigError::Parse {
                path: path.to_owned(),
                line,
                col,
                message: e.message().trim().to_owned(),
            }
        })
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
fn line_col(src: &str, pos: usize) -> (usize, usize) {
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

[plugins]
enabled = []             # plugin directory names under ~/.config/termos/plugins

[theme]
# auto | dark | light | high-contrast. "auto" is dark or light to match each
# terminal's background. Switch while running: palette → "theme".
base = "auto"
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

[ui]
animations = true        # floats and toasts; never over SSH
reduced-motion = false   # no animations at all (or TERMOS_REDUCED_MOTION=1)

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
            shell: Some("fish".into()),
            ..Config::default()
        };
        assert_eq!(c.shell_program(Some("/bin/zsh".into())), "fish");
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
}
