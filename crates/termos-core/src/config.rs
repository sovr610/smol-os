// SPDX-License-Identifier: GPL-3.0-or-later
//! `config.toml`: parsing, validation and precise error locations.
//!
//! Key bindings are parsed here only as far as their key notation; command
//! names are validated by the shell, which owns the command registry.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use termos_render::grid::BorderKind;
use termos_render::theme::ThemeColors;

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
    pub theme: ThemeColors,
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
            theme: ThemeColors::default(),
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

    /// The program new panes run.
    pub fn shell_program(&self, env_shell: Option<String>) -> String {
        self.shell
            .clone()
            .or(env_shell)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| String::from("/bin/sh"))
    }
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

# Colours: "default", "#rrggbb", "colourN" (0-255) or names like "brightblue".
[theme]
accent = "#6cb6ff"
fg = "#d0d7e2"
muted = "#7d8590"
border = "#444c56"
status-bg = "#1c2128"
status-fg = "#adbac7"
overlay-bg = "#22272e"
selection-bg = "#2d4a6e"
warn = "#e5a54b"

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
        assert_eq!(c.shell_program(None), "/bin/sh");
        let c = Config {
            shell: Some("fish".into()),
            ..Config::default()
        };
        assert_eq!(c.shell_program(Some("/bin/zsh".into())), "fish");
    }

    #[test]
    fn line_col_math() {
        assert_eq!(line_col("abc\ndef", 0), (1, 1));
        assert_eq!(line_col("abc\ndef", 5), (2, 2));
    }
}
