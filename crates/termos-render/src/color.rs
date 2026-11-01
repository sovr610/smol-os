// SPDX-License-Identifier: GPL-3.0-or-later
//! Colours: the terminal default, the 256-colour palette, and 24-bit RGB,
//! plus downsampling for terminals with fewer colours.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Linear blend from `self` toward `other`; `t` is clamped to `0.0..=1.0`.
    pub fn blend(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| -> u8 {
            let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
            // In range by construction: a convex combination of two u8 values.
            v.round().clamp(0.0, 255.0) as u8
        };
        Rgb::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
        )
    }

    fn dist2(self, other: Rgb) -> u32 {
        let d = |a: u8, b: u8| {
            let x = i32::from(a) - i32::from(b);
            (x * x).unsigned_abs()
        };
        // Weighted for perceived brightness (cheap "redmean"-style weights).
        2 * d(self.r, other.r) + 4 * d(self.g, other.g) + 3 * d(self.b, other.b)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// A cell colour as terminals understand it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Color {
    /// The terminal's default foreground/background.
    #[default]
    Default,
    /// An entry of the 256-colour palette (0–15 are the theme's ANSI colours).
    Indexed(u8),
    /// A 24-bit colour.
    Rgb(Rgb),
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color::Rgb(Rgb::new(r, g, b))
    }

    /// Resolves to RGB using `palette` for the default and ANSI colours.
    pub fn resolve(self, palette: &Palette, is_fg: bool) -> Rgb {
        match self {
            Color::Default => {
                if is_fg {
                    palette.fg
                } else {
                    palette.bg
                }
            }
            Color::Indexed(i) if i < 16 => palette.ansi[usize::from(i)],
            Color::Indexed(i) => xterm_256_to_rgb(i),
            Color::Rgb(rgb) => rgb,
        }
    }

    /// Converts to what a terminal with `depth` colours can display.
    pub fn downsample(self, depth: ColorDepth) -> Color {
        match (self, depth) {
            (Color::Default, _) | (_, ColorDepth::Truecolor) => self,
            (Color::Indexed(i), ColorDepth::Ansi256) => Color::Indexed(i),
            (Color::Rgb(rgb), ColorDepth::Ansi256) => Color::Indexed(rgb_to_256(rgb)),
            (Color::Indexed(i), ColorDepth::Ansi16) if i < 16 => self,
            (Color::Indexed(i), ColorDepth::Ansi16) => {
                Color::Indexed(rgb_to_16(xterm_256_to_rgb(i)))
            }
            (Color::Rgb(rgb), ColorDepth::Ansi16) => Color::Indexed(rgb_to_16(rgb)),
        }
    }
}

/// Error for an unparseable colour string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseColorError(pub String);

impl fmt::Display for ParseColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid colour `{}`: expected `default`, `#rrggbb`, `colourN`/`colorN` (0-255) or an ANSI name such as `red` or `brightblue`",
            self.0
        )
    }
}

impl std::error::Error for ParseColorError {}

const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightblack",
    "brightred",
    "brightgreen",
    "brightyellow",
    "brightblue",
    "brightmagenta",
    "brightcyan",
    "brightwhite",
];

impl FromStr for Color {
    type Err = ParseColorError;

    /// Accepts `default`, `#rrggbb`, `colourN`/`colorN`, and ANSI names
    /// (`red`, `brightblue`, …), case-insensitively, like tmux.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseColorError(s.to_owned());
        let lower = s.trim().to_ascii_lowercase();
        if lower == "default" {
            return Ok(Color::Default);
        }
        if let Some(hex) = lower.strip_prefix('#') {
            if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(err());
            }
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| err());
            return Ok(Color::rgb(byte(0)?, byte(2)?, byte(4)?));
        }
        if let Some(n) = lower
            .strip_prefix("colour")
            .or_else(|| lower.strip_prefix("color"))
        {
            return n.parse::<u8>().map(Color::Indexed).map_err(|_| err());
        }
        ANSI_NAMES
            .iter()
            .position(|name| *name == lower)
            .and_then(|i| u8::try_from(i).ok())
            .map(Color::Indexed)
            .ok_or_else(err)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::Default => f.write_str("default"),
            Color::Indexed(i) => write!(f, "colour{i}"),
            Color::Rgb(rgb) => rgb.fmt(f),
        }
    }
}

impl TryFrom<String> for Color {
    type Error = ParseColorError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// How many colours an output can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorDepth {
    Ansi16,
    Ansi256,
    Truecolor,
}

/// The default colours plus the 16 ANSI colours, used to resolve
/// `Color::Default`/`Color::Indexed` to RGB (e.g. for drop shadows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub ansi: [Rgb; 16],
}

/// xterm's default values for the 16 ANSI colours.
pub const XTERM_ANSI: [Rgb; 16] = [
    Rgb::new(0, 0, 0),
    Rgb::new(205, 0, 0),
    Rgb::new(0, 205, 0),
    Rgb::new(205, 205, 0),
    Rgb::new(0, 0, 238),
    Rgb::new(205, 0, 205),
    Rgb::new(0, 205, 205),
    Rgb::new(229, 229, 229),
    Rgb::new(127, 127, 127),
    Rgb::new(255, 0, 0),
    Rgb::new(0, 255, 0),
    Rgb::new(255, 255, 0),
    Rgb::new(92, 92, 255),
    Rgb::new(255, 0, 255),
    Rgb::new(0, 255, 255),
    Rgb::new(255, 255, 255),
];

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The RGB value of a 256-palette entry (16–255 per xterm; 0–15 per xterm defaults).
pub fn xterm_256_to_rgb(i: u8) -> Rgb {
    match i {
        0..=15 => XTERM_ANSI[usize::from(i)],
        16..=231 => {
            let n = i - 16;
            Rgb::new(
                CUBE_LEVELS[usize::from(n / 36)],
                CUBE_LEVELS[usize::from((n / 6) % 6)],
                CUBE_LEVELS[usize::from(n % 6)],
            )
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            Rgb::new(v, v, v)
        }
    }
}

/// Nearest entry in the 6×6×6 cube or the grey ramp (16–255).
pub fn rgb_to_256(rgb: Rgb) -> u8 {
    let level = |v: u8| -> u8 {
        let mut best = 0;
        for (idx, l) in CUBE_LEVELS.iter().enumerate() {
            if v.abs_diff(*l) < v.abs_diff(CUBE_LEVELS[usize::from(best)]) {
                best = u8::try_from(idx).unwrap_or(0);
            }
        }
        best
    };
    let cube = 16 + 36 * level(rgb.r) + 6 * level(rgb.g) + level(rgb.b);
    let avg = (u16::from(rgb.r) + u16::from(rgb.g) + u16::from(rgb.b)) / 3;
    let grey_step = u8::try_from(((avg.saturating_sub(8)) / 10).min(23)).unwrap_or(23);
    let grey = 232 + grey_step;
    if xterm_256_to_rgb(grey).dist2(rgb) < xterm_256_to_rgb(cube).dist2(rgb) {
        grey
    } else {
        cube
    }
}

/// Nearest of the 16 ANSI colours (using xterm's default values).
pub fn rgb_to_16(rgb: Rgb) -> u8 {
    let mut best = 0usize;
    for (i, c) in XTERM_ANSI.iter().enumerate() {
        if c.dist2(rgb) < XTERM_ANSI[best].dist2(rgb) {
            best = i;
        }
    }
    u8::try_from(best).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tmux_style_colours() {
        assert_eq!("default".parse(), Ok(Color::Default));
        assert_eq!("#ff8000".parse(), Ok(Color::rgb(255, 128, 0)));
        assert_eq!("colour231".parse(), Ok(Color::Indexed(231)));
        assert_eq!("color7".parse(), Ok(Color::Indexed(7)));
        assert_eq!("BrightBlue".parse(), Ok(Color::Indexed(12)));
        assert!("#12345".parse::<Color>().is_err());
        assert!("colour256".parse::<Color>().is_err());
        assert!("mauve".parse::<Color>().is_err());
    }

    #[test]
    fn display_round_trips() {
        for c in [Color::Default, Color::Indexed(42), Color::rgb(1, 2, 3)] {
            assert_eq!(c.to_string().parse(), Ok(c));
        }
    }

    #[test]
    fn cube_and_grey_lookups() {
        assert_eq!(xterm_256_to_rgb(16), Rgb::new(0, 0, 0));
        assert_eq!(xterm_256_to_rgb(231), Rgb::new(255, 255, 255));
        assert_eq!(xterm_256_to_rgb(196), Rgb::new(255, 0, 0));
        assert_eq!(xterm_256_to_rgb(232), Rgb::new(8, 8, 8));
        assert_eq!(xterm_256_to_rgb(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn downsampling_finds_nearest() {
        assert_eq!(rgb_to_256(Rgb::new(255, 0, 0)), 196);
        assert_eq!(rgb_to_256(Rgb::new(128, 128, 128)), 244);
        assert_eq!(rgb_to_16(Rgb::new(250, 10, 10)), 9);
        assert_eq!(rgb_to_16(Rgb::new(10, 10, 10)), 0);
        assert_eq!(
            Color::rgb(0, 0, 0).downsample(ColorDepth::Ansi16),
            Color::Indexed(0)
        );
        assert_eq!(
            Color::Indexed(196).downsample(ColorDepth::Ansi16),
            Color::Indexed(9)
        );
        assert_eq!(
            Color::Default.downsample(ColorDepth::Ansi16),
            Color::Default
        );
    }

    #[test]
    fn blend_is_linear() {
        let a = Rgb::new(200, 100, 0);
        assert_eq!(a.blend(Rgb::BLACK, 0.5), Rgb::new(100, 50, 0));
        assert_eq!(a.blend(Rgb::BLACK, 0.0), a);
        assert_eq!(a.blend(Rgb::BLACK, 2.0), Rgb::BLACK);
    }
}
