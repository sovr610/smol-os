// SPDX-License-Identifier: GPL-3.0-or-later
//! Themes: a few base colours (user-configurable) from which every UI style
//! is derived.

use serde::{Deserialize, Deserializer};

use crate::cell::{Attrs, Style};
use crate::color::{Color, Palette, Rgb, XTERM_ANSI};

fn color_from_str<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(serde::de::Error::custom)
}

/// The configurable base colours (`[theme]` in config.toml).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ThemeColors {
    #[serde(deserialize_with = "color_from_str")]
    pub accent: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub fg: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub muted: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub border: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub status_bg: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub status_fg: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub overlay_bg: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub selection_bg: Color,
    #[serde(deserialize_with = "color_from_str")]
    pub warn: Color,
}

impl Default for ThemeColors {
    /// The built-in dark theme.
    fn default() -> Self {
        Self {
            accent: Color::rgb(0x6c, 0xb6, 0xff),
            fg: Color::rgb(0xd0, 0xd7, 0xe2),
            muted: Color::rgb(0x7d, 0x85, 0x90),
            border: Color::rgb(0x44, 0x4c, 0x56),
            status_bg: Color::rgb(0x1c, 0x21, 0x28),
            status_fg: Color::rgb(0xad, 0xba, 0xc7),
            overlay_bg: Color::rgb(0x22, 0x27, 0x2e),
            selection_bg: Color::rgb(0x2d, 0x4a, 0x6e),
            warn: Color::rgb(0xe5, 0xa5, 0x4b),
        }
    }
}

/// Every style the desktop draws with.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub colors: ThemeColors,
    /// Used to resolve default/ANSI colours to RGB (drop shadows).
    pub palette: Palette,
    pub border: Style,
    pub border_active: Style,
    pub float_border: Style,
    pub float_title: Style,
    pub shadow: Rgb,
    pub shadow_amount: f32,
    pub status: Style,
    pub status_tab: Style,
    pub status_tab_active: Style,
    pub status_prefix: Style,
    pub status_message: Style,
    pub overlay: Style,
    pub overlay_border: Style,
    pub overlay_title: Style,
    pub overlay_selected: Style,
    pub overlay_key: Style,
    pub overlay_muted: Style,
    pub selection: Style,
    pub copy_mode_badge: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_colors(ThemeColors::default())
    }
}

impl Theme {
    pub fn from_colors(c: ThemeColors) -> Self {
        let on = |fg: Color, bg: Color| Style::new().fg(fg).bg(bg);
        // The terminal's real default colours are unknown until M4 queries
        // them (OSC 10/11); assume a dark background.
        let base = Palette {
            fg: Rgb::new(0xd0, 0xd7, 0xe2),
            bg: Rgb::new(0x10, 0x14, 0x18),
            ansi: XTERM_ANSI,
        };
        Self {
            palette: Palette {
                fg: c.fg.resolve(&base, true),
                ..base
            },
            border: Style::new().fg(c.border),
            border_active: Style::new().fg(c.accent),
            float_border: Style::new().fg(c.accent),
            float_title: Style::new().fg(c.accent).attrs(Attrs::BOLD),
            shadow: Rgb::BLACK,
            shadow_amount: 0.6,
            status: on(c.status_fg, c.status_bg),
            status_tab: on(c.status_fg, c.status_bg),
            status_tab_active: on(c.status_bg, c.accent).attrs(Attrs::BOLD),
            status_prefix: on(c.status_bg, c.warn).attrs(Attrs::BOLD),
            status_message: on(c.accent, c.status_bg).attrs(Attrs::BOLD),
            overlay: on(c.fg, c.overlay_bg),
            overlay_border: on(c.accent, c.overlay_bg),
            overlay_title: on(c.accent, c.overlay_bg).attrs(Attrs::BOLD),
            overlay_selected: on(c.overlay_bg, c.accent),
            overlay_key: on(c.warn, c.overlay_bg).attrs(Attrs::BOLD),
            overlay_muted: on(c.muted, c.overlay_bg),
            selection: Style::new().bg(c.selection_bg),
            copy_mode_badge: on(c.status_bg, c.warn).attrs(Attrs::BOLD),
            colors: c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        theme: ThemeColors,
    }

    #[test]
    fn partial_overrides_keep_defaults() {
        let w: Wrapper = toml::from_str("[theme]\naccent = \"colour208\"\n").expect("valid");
        assert_eq!(w.theme.accent, Color::Indexed(208));
        assert_eq!(w.theme.fg, ThemeColors::default().fg);
        let t = Theme::from_colors(w.theme);
        assert_eq!(t.border_active.fg, Color::Indexed(208));
    }

    #[test]
    fn bad_colours_and_keys_are_rejected() {
        assert!(toml::from_str::<Wrapper>("[theme]\naccent = \"nope\"\n").is_err());
        assert!(toml::from_str::<Wrapper>("[theme]\nunknown = \"red\"\n").is_err());
    }
}
