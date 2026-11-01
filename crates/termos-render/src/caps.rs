// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal capability detection (ARCHITECTURE.md §5.7).
//!
//! [`Caps::from_env`] guesses from environment variables; the client then
//! refines that with the replies to active queries ([`crate::probe`]) via
//! [`Caps::apply_replies`], and sends the result in its handshake.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::color::{ColorDepth, Rgb};
use crate::probe::Replies;

/// A size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PxSize {
    pub width: u32,
    pub height: u32,
}

/// Image protocols a terminal understands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Graphics {
    pub kitty: bool,
    pub sixel: bool,
    pub iterm2: bool,
}

/// How images are drawn for one client, best first (ARCHITECTURE.md
/// §5.8). Blocks are drawn into the cell grid and work everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageProtocol {
    Kitty,
    Sixel,
    Iterm2,
    Blocks,
}

impl ImageProtocol {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Iterm2 => "iterm2",
            Self::Blocks => "blocks",
        }
    }
}

impl fmt::Display for ImageProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ImageProtocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "kitty" => Ok(Self::Kitty),
            "sixel" => Ok(Self::Sixel),
            "iterm2" | "iterm" => Ok(Self::Iterm2),
            "blocks" | "none" | "off" => Ok(Self::Blocks),
            other => Err(format!(
                "unknown image protocol `{other}` (expected kitty, sixel, iterm2 or blocks)"
            )),
        }
    }
}

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
    /// Image protocols the terminal answered for (or the environment
    /// implies).
    pub graphics: Graphics,
    /// `TERMOS_GRAPHICS`: the protocol to use regardless of detection.
    pub graphics_override: Option<ImageProtocol>,
    /// One cell, in pixels.
    pub cell_px: Option<PxSize>,
    /// The text area, in pixels.
    pub window_px: Option<PxSize>,
    /// The terminal's background colour (OSC 11), for the `auto` theme.
    pub background: Option<Rgb>,
    /// The terminal's name and version (XTVERSION).
    pub terminal: Option<String>,
    /// Running inside tmux or screen: no Kitty or iTerm2 images, which
    /// they would not pass on.
    pub in_tmux: bool,
    /// The client runs over SSH: no animations, longer probe timeout.
    pub remote: bool,
    /// The user asked for reduced motion (`TERMOS_REDUCED_MOTION=1`).
    pub reduced_motion: bool,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            color: ColorDepth::Ansi256,
            sync_output: true,
            kitty_keyboard: false,
            term: String::new(),
            graphics: Graphics::default(),
            graphics_override: None,
            cell_px: None,
            window_px: None,
            background: None,
            terminal: None,
            in_tmux: false,
            remote: false,
            reduced_motion: false,
        }
    }
}

impl Caps {
    /// Detects capabilities from environment variables. `get` looks a
    /// variable up (pass `|k| std::env::var(k).ok()` in production).
    ///
    /// `TERMOS_COLOR` (`truecolor` | `256` | `16`), `TERMOS_SYNC=0` and
    /// `TERMOS_GRAPHICS` (`kitty` | `sixel` | `iterm2` | `blocks`) override
    /// the heuristics.
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
        } else if cfg!(windows) && lower_term.is_empty() {
            // Windows consoles set no TERM; every console that runs TermOS
            // (VT processing, Windows 10 1809+) shows 24-bit colour.
            ColorDepth::Truecolor
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

        let in_tmux = get("TMUX").is_some()
            || lower_term.starts_with("tmux")
            || lower_term.starts_with("screen");
        // WezTerm speaks the iTerm2 protocol too; both say so in
        // TERM_PROGRAM, which tmux would hide.
        let iterm2 = !in_tmux && matches!(term_program.as_str(), "iTerm.app" | "WezTerm");
        let truthy =
            |k: &str| get(k).is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"));
        Self {
            color,
            sync_output: get("TERMOS_SYNC").as_deref() != Some("0"),
            kitty_keyboard: false,
            term,
            graphics: Graphics {
                iterm2,
                ..Graphics::default()
            },
            graphics_override: get("TERMOS_GRAPHICS").and_then(|v| v.parse().ok()),
            in_tmux,
            remote: get("SSH_CONNECTION").is_some()
                || get("SSH_CLIENT").is_some()
                || get("SSH_TTY").is_some(),
            reduced_motion: truthy("TERMOS_REDUCED_MOTION"),
            ..Self::default()
        }
    }

    /// Adds what the terminal answered to the probe.
    pub fn apply_replies(&mut self, r: &Replies) {
        if let Some(v) = &r.xtversion {
            let lower = v.to_ascii_lowercase();
            if lower.starts_with("tmux") || lower.starts_with("screen") {
                self.in_tmux = true;
            }
            if lower.starts_with("iterm2") || lower.starts_with("wezterm") {
                self.graphics.iterm2 = true;
            }
            self.terminal = Some(v.clone());
        }
        // tmux answers DA1 itself, so Sixel there means tmux passes it on.
        self.graphics.sixel = r.sixel();
        self.graphics.kitty = r.kitty_graphics == Some(true);
        if self.in_tmux {
            self.graphics.kitty = false;
            self.graphics.iterm2 = false;
        }
        if let Some(sync) = r.sync_output {
            self.sync_output &= sync;
        }
        self.kitty_keyboard = r.kitty_keyboard.is_some();
        self.cell_px = r.cell_px.or(self.cell_px);
        self.window_px = r.window_px.or(self.window_px);
        self.background = r.background.or(self.background);
    }

    /// Fills in the cell size from the text-area size when the terminal
    /// answered only that.
    pub fn derive_cell_px(&mut self, cols: u16, rows: u16) {
        if self.cell_px.is_none()
            && let Some(w) = self.window_px
            && cols > 0
            && rows > 0
        {
            let (cw, ch) = (w.width / u32::from(cols), w.height / u32::from(rows));
            if cw > 0 && ch > 0 {
                self.cell_px = Some(PxSize {
                    width: cw,
                    height: ch,
                });
            }
        }
    }

    /// The best image protocol for this terminal.
    pub fn image_protocol(&self) -> ImageProtocol {
        if let Some(p) = self.graphics_override {
            return p;
        }
        if self.graphics.kitty {
            ImageProtocol::Kitty
        } else if self.graphics.sixel {
            ImageProtocol::Sixel
        } else if self.graphics.iterm2 {
            ImageProtocol::Iterm2
        } else {
            ImageProtocol::Blocks
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
        let no_term = if cfg!(windows) {
            ColorDepth::Truecolor
        } else {
            ColorDepth::Ansi16
        };
        assert_eq!(detect(&[]).color, no_term);
    }

    /// Probe replies per terminal. tmux's is recorded (`termos debug caps
    /// --raw` in a detached tmux 3.4 session, a build with Sixel); the
    /// others are rebuilt from each terminal's documented reply formats
    /// until recordings replace them.
    /// A terminal's name, its environment and its reply bytes.
    type Fixture = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [u8],
    );

    const FIXTURES: &[Fixture] = &[
        (
            "kitty",
            &[("TERM", "xterm-kitty"), ("KITTY_WINDOW_ID", "1")],
            b"\x1bP>|kitty(0.35.2)\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?2026;2$y\x1b[?0u\
              \x1b[6;20;10t\x1b[4;800;1200t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;c",
        ),
        (
            "wezterm",
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "WezTerm")],
            b"\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\x1b_Gi=31;OK\x1b\\\
              \x1b[?2026;2$y\x1b[?0u\x1b[6;19;9t\x1b[4;760;1440t\
              \x1b]11;rgb:0000/0000/0000\x07\x1b[?65;4;6;18;22c",
        ),
        (
            "foot",
            &[("TERM", "foot")],
            b"\x1bP>|foot(1.17.2)\x1b\\\x1b[?2026;2$y\x1b[?0u\x1b[6;18;9t\
              \x1b[4;720;1280t\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;4;22c",
        ),
        (
            "iterm2",
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "iTerm.app")],
            b"\x1bP>|iTerm2 3.5.0\x1b\\\x1b[?2026;2$y\x1b[6;32;16t\
              \x1b[4;1600;2560t\x1b]11;rgb:0000/0000/0000\x07\x1b[?62;4c",
        ),
        (
            "xterm",
            &[("TERM", "xterm")],
            b"\x1bP>|XTerm(390)\x1b\\\x1b[?2026;0$y\x1b[6;17;9t\x1b[4;408;720t\
              \x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?64;1;2;4;6;9;15;16;17;18;21;22;28c",
        ),
        (
            "windows-terminal",
            &[("WT_SESSION", "0f8a"), ("TERM", "")],
            b"\x1b[?2026;2$y\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\\
              \x1b[?61;4;6;7;14;21;22;23;24;28;32;42c",
        ),
        (
            "gnome-terminal",
            &[
                ("TERM", "xterm-256color"),
                ("VTE_VERSION", "7600"),
                ("COLORTERM", "truecolor"),
            ],
            b"\x1b[4;720;1280t\x1b]11;rgb:1717/1717/1717\x1b\\\x1b[?65;1;9c",
        ),
        (
            "tmux",
            &[
                ("TERM", "tmux-256color"),
                ("TMUX", "/tmp/tmux-1000/default,1,0"),
            ],
            b"\x1bP>|tmux 3.4\x1b\\\x1b[6;32;16t\x1b[4;1280;1920t\x1b[?1;2;4c",
        ),
    ];

    fn probed(name: &str) -> Caps {
        let (_, env, reply) = FIXTURES
            .iter()
            .find(|(n, _, _)| *n == name)
            .expect("fixture");
        let mut caps = detect(env);
        let r = crate::probe::parse_replies(reply);
        assert!(r.complete(), "{name}: DA1 ends every probe");
        assert!(r.other.is_empty() && r.partial.is_empty(), "{name}: {r:?}");
        caps.apply_replies(&r);
        caps
    }

    #[test]
    fn terminals_from_their_replies() {
        let px = |width, height| Some(PxSize { width, height });
        let expect: &[(&str, ImageProtocol, Option<PxSize>, bool)] = &[
            ("kitty", ImageProtocol::Kitty, px(10, 20), true),
            ("wezterm", ImageProtocol::Kitty, px(9, 19), true),
            ("foot", ImageProtocol::Sixel, px(9, 18), true),
            ("iterm2", ImageProtocol::Sixel, px(16, 32), false),
            ("xterm", ImageProtocol::Sixel, px(9, 17), false),
            ("windows-terminal", ImageProtocol::Sixel, None, false),
            ("gnome-terminal", ImageProtocol::Blocks, None, false),
            // tmux passes Sixel on, and only Sixel.
            ("tmux", ImageProtocol::Sixel, px(16, 32), false),
        ];
        for &(name, protocol, cell, kitty_keyboard) in expect {
            let caps = probed(name);
            assert_eq!(caps.image_protocol(), protocol, "{name}");
            assert_eq!(caps.cell_px, cell, "{name}");
            assert_eq!(caps.kitty_keyboard, kitty_keyboard, "{name}");
            // A detached tmux has no outer terminal to ask for colours.
            assert_eq!(caps.background.is_some(), name != "tmux", "{name}: OSC 11");
        }
        assert!(probed("wezterm").graphics.iterm2 && probed("wezterm").graphics.sixel);
        assert!(probed("iterm2").graphics.iterm2);
        assert!(
            !probed("xterm").sync_output,
            "xterm does not know mode 2026"
        );
        let tmux = probed("tmux");
        assert!(
            tmux.in_tmux && tmux.sync_output,
            "no DECRQM reply: unchanged"
        );
        assert_eq!(tmux.terminal.as_deref(), Some("tmux 3.4"));
        let mut gnome = probed("gnome-terminal");
        gnome.derive_cell_px(128, 40);
        assert_eq!(gnome.cell_px, px(10, 18), "from the text-area size");
        assert_eq!(
            probed("windows-terminal").background,
            Some(Rgb::new(12, 12, 12))
        );
    }

    #[test]
    fn inside_tmux_no_kitty_or_iterm2_even_if_answered() {
        let mut caps = detect(&[("TERM", "screen-256color"), ("TERM_PROGRAM", "iTerm.app")]);
        assert!(caps.in_tmux && !caps.graphics.iterm2);
        caps.apply_replies(&crate::probe::parse_replies(
            b"\x1b_Gi=31;OK\x1b\\\x1b[?62;4c",
        ));
        assert!(!caps.graphics.kitty);
        assert_eq!(caps.image_protocol(), ImageProtocol::Sixel);
    }

    #[test]
    fn session_flags_and_graphics_override() {
        let caps = detect(&[
            ("SSH_CONNECTION", "10.0.0.2 51000 10.0.0.1 22"),
            ("TERMOS_REDUCED_MOTION", "1"),
            ("TERMOS_GRAPHICS", "blocks"),
            ("TERM", "xterm-kitty"),
        ]);
        assert!(caps.remote && caps.reduced_motion);
        let mut caps = caps;
        caps.apply_replies(&crate::probe::parse_replies(
            b"\x1b_Gi=31;OK\x1b\\\x1b[?62c",
        ));
        assert!(caps.graphics.kitty);
        assert_eq!(
            caps.image_protocol(),
            ImageProtocol::Blocks,
            "the override wins"
        );
        assert!(!detect(&[]).remote);
        assert!("sixel".parse::<ImageProtocol>().is_ok());
        assert!("png".parse::<ImageProtocol>().is_err());
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
