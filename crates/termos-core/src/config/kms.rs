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
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use std::path::Path;

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
