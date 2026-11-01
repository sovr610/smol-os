// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal capability detection.
//!
//! M1 uses environment heuristics only. Active queries (DA1, XTVERSION,
//! Kitty graphics, OSC 11) arrive with graphics support in M4.

use serde::{Deserialize, Serialize};

use crate::color::ColorDepth;

/// What the outer terminal can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    pub color: ColorDepth,
    /// Wrap frames in DEC mode 2026 synchronized updates. Terminals that do
    /// not know the mode ignore it, so this is on unless disabled.
    pub sync_output: bool,
    /// The terminal accepted the Kitty keyboard protocol (set by the client
    /// after its query).
    pub kitty_keyboard: bool,
    /// `$TERM` of the outer terminal, for diagnostics.
    pub term: String,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            color: ColorDepth::Ansi256,
            sync_output: true,
            kitty_keyboard: false,
            term: String::new(),
        }
    }
}

// TODO(M4): active capability queries (DA1, XTVERSION, Kitty graphics, DECRQM 2026, OSC 11, cell size).

impl Caps {
    /// Detects capabilities from environment variables. `get` looks a
    /// variable up (pass `|k| std::env::var(k).ok()` in production).
    ///
    /// `TERMOS_COLOR` (`truecolor` | `256` | `16`) and `TERMOS_SYNC=0`
    /// override the heuristics.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let term = get("TERM").unwrap_or_default();
        let colorterm = get("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        let term_program = get("TERM_PROGRAM").unwrap_or_default();
        let lower_term = term.to_ascii_lowercase();

        let truecolor_terms = [
            "kitty",
            "alacritty",
            "foot",
            "wezterm",
            "ghostty",
            "contour",
            "rio",
            "direct",
        ];
        let truecolor_programs = [
            "iTerm.app",
            "WezTerm",
            "vscode",
            "ghostty",
            "Hyper",
            "Tabby",
            "rio",
        ];

        let mut color = if colorterm == "truecolor"
            || colorterm == "24bit"
            || truecolor_terms.iter().any(|t| lower_term.contains(t))
            || truecolor_programs.contains(&term_program.as_str())
            || get("KITTY_WINDOW_ID").is_some()
            || get("WT_SESSION").is_some()
        {
            ColorDepth::Truecolor
        } else if lower_term.contains("256color") {
            ColorDepth::Ansi256
        } else if lower_term.is_empty()
            || lower_term == "dumb"
            || lower_term == "linux"
            || lower_term == "vt100"
            || lower_term == "xterm"
            || lower_term == "screen"
        {
            ColorDepth::Ansi16
        } else {
            ColorDepth::Ansi256
        };

        match get("TERMOS_COLOR").as_deref() {
            Some("truecolor" | "24bit") => color = ColorDepth::Truecolor,
            Some("256") => color = ColorDepth::Ansi256,
            Some("16") => color = ColorDepth::Ansi16,
            _ => {}
        }

        Self {
            color,
            sync_output: get("TERMOS_SYNC").as_deref() != Some("0"),
            kitty_keyboard: false,
            term,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)]) -> Caps {
        Caps::from_env(|k| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn colour_depth_heuristics() {
        assert_eq!(
            detect(&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]).color,
            ColorDepth::Truecolor
        );
        assert_eq!(
            detect(&[("TERM", "xterm-kitty")]).color,
            ColorDepth::Truecolor
        );
        assert_eq!(
            detect(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "iTerm.app")]).color,
            ColorDepth::Truecolor
        );
        assert_eq!(
            detect(&[("TERM", "tmux-256color")]).color,
            ColorDepth::Ansi256
        );
        assert_eq!(detect(&[("TERM", "linux")]).color, ColorDepth::Ansi16);
        assert_eq!(detect(&[]).color, ColorDepth::Ansi16);
    }

    #[test]
    fn overrides_win() {
        let caps = detect(&[
            ("TERM", "xterm-kitty"),
            ("TERMOS_COLOR", "16"),
            ("TERMOS_SYNC", "0"),
        ]);
        assert_eq!(caps.color, ColorDepth::Ansi16);
        assert!(!caps.sync_output);
    }
}
