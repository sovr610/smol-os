// SPDX-License-Identifier: GPL-3.0-or-later
//! `[kms]`: the bare-metal console, `termos --kms` (M8, D-073).

use std::path::PathBuf;

use serde::Deserialize;

/// The font sizes the console draws, in pixels (termos-render's raster
/// clamps to the same).
pub const FONT_SIZES: std::ops::RangeInclusive<f32> = 6.0..=96.0;

fn font_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    let v = f32::deserialize(d)?;
    if FONT_SIZES.contains(&v) {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format!(
            "font-size is {v}; the console draws {} to {} pixels",
            FONT_SIZES.start(),
            FONT_SIZES.end()
        )))
    }
}

/// `[kms]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct KmsConfig {
    /// Font size in pixels at 96 dpi; scaled up on dense screens. Changed
    /// while running with Ctrl+= and Ctrl+- (Ctrl+0 resets it). Between
    /// [`FONT_SIZES`]' ends, the console's own limits.
    #[serde(deserialize_with = "font_size")]
    pub font_size: f32,
    /// A TrueType/OpenType file; none: the built-in DejaVu Sans Mono.
    pub font: Option<PathBuf>,
    /// Its bold face; none: the regular face, emboldened.
    pub bold_font: Option<PathBuf>,
    /// Fonts tried for characters the main font lacks (CJK, symbols).
    pub fallback_fonts: Vec<PathBuf>,
    /// The keyboard layout (XKB names, as in `setxkbmap`): "us", "de", "fr"…
    pub keyboard_layout: String,
    /// The layout's variant: "dvorak", "nodeadkeys"…
    pub keyboard_variant: String,
    /// XKB options: "ctrl:nocaps,compose:ralt"…
    pub keyboard_options: String,
    /// Milliseconds a key is held before it repeats.
    pub repeat_delay: u32,
    /// Repeats per second while held.
    pub repeat_rate: u32,
    /// The bell flashes a frame around the screen for a moment; `false`:
    /// the bell does nothing.
    pub visual_bell: bool,
    /// The bell also beeps, through the sound server (M13e, D-128).
    pub audible_bell: bool,
    /// With several displays: each its own workspace (`extend`), or the
    /// same picture on all (`mirror`) (D-127, D-128).
    pub displays: DisplaysMode,
    /// `[[kms.display]]`: one table per display to set up; displays not
    /// listed get their preferred mode, on.
    pub display: Vec<DisplayConfig>,
}

/// `[kms] displays`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplaysMode {
    #[default]
    Extend,
    Mirror,
}

/// `[[kms.display]]`: one display, by its connector's name.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DisplayConfig {
    /// The connector, as the kernel names it: `HDMI-A-1`, `eDP-1`,
    /// `Virtual-1`.
    pub name: String,
    /// `1920x1080`, `1920x1080@60`, or `preferred` (the default).
    #[serde(default = "preferred", deserialize_with = "mode")]
    pub mode: String,
    /// Text size relative to the display's density (1.0 = as measured;
    /// none: as measured).
    #[serde(default)]
    pub scale: Option<f32>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The display the first workspace, the keyboard and the pointer start
    /// on; it comes first, leftmost.
    #[serde(default)]
    pub primary: bool,
}

fn preferred() -> String {
    "preferred".into()
}

fn yes() -> bool {
    true
}

fn mode<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s == "preferred" || parse_mode(&s).is_some() {
        Ok(s)
    } else {
        Err(serde::de::Error::custom(format!(
            "mode is {s:?}: give WIDTHxHEIGHT, WIDTHxHEIGHT@HZ or \"preferred\""
        )))
    }
}

/// `1920x1080` or `1920x1080@60`: (width, height, refresh).
pub fn parse_mode(s: &str) -> Option<(u16, u16, Option<u32>)> {
    let (size, hz) = match s.split_once('@') {
        Some((size, hz)) => (
            size,
            Some(hz.trim().parse::<u32>().ok().filter(|h| *h > 0)?),
        ),
        None => (s, None),
    };
    let (w, h) = size.split_once('x')?;
    let w: u16 = w.trim().parse().ok().filter(|w| *w > 0)?;
    let h: u16 = h.trim().parse().ok().filter(|h| *h > 0)?;
    Some((w, h, hz))
}

impl Default for KmsConfig {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            font: None,
            bold_font: None,
            fallback_fonts: Vec::new(),
            keyboard_layout: "us".into(),
            keyboard_variant: String::new(),
            keyboard_options: String::new(),
            repeat_delay: 400,
            repeat_rate: 25,
            visual_bell: true,
            audible_bell: false,
            displays: DisplaysMode::Extend,
            display: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use std::path::Path;

    #[test]
    fn displays_are_configured_by_name() {
        let c = Config::parse(
            "[kms]\ndisplays = \"mirror\"\naudible-bell = true\n\
             [[kms.display]]\nname = \"HDMI-A-1\"\nmode = \"1920x1080@60\"\nprimary = true\n\
             [[kms.display]]\nname = \"eDP-1\"\nenabled = false\nscale = 1.5\n",
            Path::new("c.toml"),
        )
        .expect("ok");
        assert_eq!(c.kms.displays, super::DisplaysMode::Mirror);
        assert!(c.kms.audible_bell);
        assert_eq!(c.kms.display.len(), 2);
        assert_eq!(c.kms.display[0].mode, "1920x1080@60");
        assert!(c.kms.display[0].primary && c.kms.display[0].enabled);
        assert_eq!(c.kms.display[1].mode, "preferred");
        assert_eq!(c.kms.display[1].scale, Some(1.5));
        assert!(!c.kms.display[1].enabled);
        assert!(
            Config::parse(
                "[[kms.display]]\nname = \"x\"\nmode = \"big\"\n",
                Path::new("c.toml")
            )
            .is_err()
        );
        assert_eq!(super::parse_mode("2560x1440"), Some((2560, 1440, None)));
        assert_eq!(super::parse_mode("800x600@75"), Some((800, 600, Some(75))));
        for bad in ["", "x", "0x600", "800x600@", "800x600@0", "800y600"] {
            assert_eq!(super::parse_mode(bad), None, "{bad}");
        }
    }

    #[test]
    fn font_sizes_are_checked() {
        let ok = Config::parse("[kms]\nfont-size = 24.0\n", Path::new("c.toml")).expect("ok");
        assert_eq!(ok.kms.font_size, 24.0);
        for bad in ["0.0", "-3.0", "200.0", "nan"] {
            let e = Config::parse(&format!("[kms]\nfont-size = {bad}\n"), Path::new("c.toml"))
                .expect_err(bad)
                .to_string();
            assert!(
                e.contains("6 to 96 pixels") && e.starts_with("c.toml:2:"),
                "{e}"
            );
        }
    }
}
