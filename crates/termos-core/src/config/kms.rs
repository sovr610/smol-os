// SPDX-License-Identifier: GPL-3.0-or-later
//! `[kms]`: the bare-metal console, `termos --kms` (M8, D-073).

use std::path::PathBuf;

use serde::Deserialize;

/// `[kms]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct KmsConfig {
    /// Font size in pixels at 96 dpi; scaled up on dense screens. Changed
    /// while running with Ctrl+= and Ctrl+- (Ctrl+0 resets it).
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
