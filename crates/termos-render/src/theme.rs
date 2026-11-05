// SPDX-License-Identifier: GPL-3.0-or-later
//! Themes: a few base colours from which every UI style is derived.
//!
//! Three presets (dark, light, high contrast); `[theme]` in config.toml
//! picks one (`base`, default `auto`: dark or light to match the
//! terminal's background) and can override any colour.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer};

use crate::cell::{Attrs, Style};
use crate::color::{Color, Palette, Rgb, XTERM_ANSI};
use crate::grid::{Grid, Rect};
use crate::icons::IconSet;

fn opt_color<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Color>, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map(Some).map_err(serde::de::Error::custom)
}

/// The base colours of a theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub accent: Color,
    pub fg: Color,
    pub muted: Color,
    pub border: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub overlay_bg: Color,
    pub selection_bg: Color,
    pub warn: Color,
    /// Errors and critical levels (a full disk, a hot CPU).
    pub error: Color,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::dark()
    }
}

impl ThemeColors {
    /// A selected row's style: the text in `fg` whatever its colour was
    /// (accents, muted text and warnings lose contrast on the selection),
    /// on `selection_bg`.
    pub fn selected(&self, style: crate::cell::Style) -> crate::cell::Style {
        style.fg(self.fg).bg(self.selection_bg)
    }

    /// The dark preset.
    pub const fn dark() -> Self {
        Self {
            accent: Color::rgb(0x6c, 0xb6, 0xff),
            fg: Color::rgb(0xd0, 0xd7, 0xe2),
            muted: Color::rgb(0x8b, 0x94, 0x9e),
            border: Color::rgb(0x44, 0x4c, 0x56),
            status_bg: Color::rgb(0x1c, 0x21, 0x28),
            status_fg: Color::rgb(0xad, 0xba, 0xc7),
            overlay_bg: Color::rgb(0x22, 0x27, 0x2e),
            selection_bg: Color::rgb(0x2d, 0x4a, 0x6e),
            warn: Color::rgb(0xe5, 0xa5, 0x4b),
            error: Color::rgb(0xf4, 0x70, 0x67),
        }
    }

    /// The light preset, for terminals with a light background.
    pub const fn light() -> Self {
        Self {
            accent: Color::rgb(0x08, 0x60, 0xca),
            fg: Color::rgb(0x1f, 0x23, 0x28),
            muted: Color::rgb(0x59, 0x63, 0x6e),
            border: Color::rgb(0xb0, 0xb8, 0xc1),
            status_bg: Color::rgb(0xea, 0xee, 0xf2),
            status_fg: Color::rgb(0x1f, 0x23, 0x28),
            overlay_bg: Color::rgb(0xf6, 0xf8, 0xfa),
            selection_bg: Color::rgb(0xb6, 0xe3, 0xff),
            warn: Color::rgb(0x8a, 0x5c, 0x00),
            error: Color::rgb(0xcf, 0x22, 0x2e),
        }
    }

    /// The high-contrast preset: white and black with saturated accents,
    /// every text colour at least 7:1 against its background (WCAG AAA).
    /// Selected rows draw their text in `fg` ([`ThemeColors::selected`]),
    /// the one colour that keeps 7:1 on a visible selection.
    pub const fn high_contrast() -> Self {
        Self {
            accent: Color::rgb(0xff, 0xd7, 0x00),
            fg: Color::rgb(0xff, 0xff, 0xff),
            muted: Color::rgb(0xd0, 0xd0, 0xd0),
            border: Color::rgb(0xc0, 0xc0, 0xc0),
            status_bg: Color::rgb(0x00, 0x00, 0x00),
            status_fg: Color::rgb(0xff, 0xff, 0xff),
            overlay_bg: Color::rgb(0x00, 0x00, 0x00),
            selection_bg: Color::rgb(0x00, 0x3d, 0xa5),
            warn: Color::rgb(0xff, 0x9e, 0x3d),
            error: Color::rgb(0xff, 0x6b, 0x6b),
        }
    }
}

/// Which preset a theme starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeBase {
    /// Dark or light, to match the terminal's background.
    #[default]
    Auto,
    Dark,
    Light,
    HighContrast,
}

impl ThemeBase {
    pub const ALL: [ThemeBase; 4] = [Self::Auto, Self::Dark, Self::Light, Self::HighContrast];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
        }
    }

    /// `auto` becomes dark or light: light when the terminal's background
    /// is light, dark when it is dark or unknown.
    pub fn resolve(self, background: Option<Rgb>) -> ThemeBase {
        match self {
            Self::Auto if background.is_some_and(is_light) => Self::Light,
            Self::Auto => Self::Dark,
            other => other,
        }
    }

    fn colors(self) -> ThemeColors {
        match self {
            Self::Auto | Self::Dark => ThemeColors::dark(),
            Self::Light => ThemeColors::light(),
            Self::HighContrast => ThemeColors::high_contrast(),
        }
    }
}

impl fmt::Display for ThemeBase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ThemeBase {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|b| b.name() == s.trim())
            .ok_or_else(|| {
                format!("unknown theme `{s}` (expected auto, dark, light or high-contrast)")
            })
    }
}

/// Whether a background colour is light (Rec. 709 luma above half).
pub fn is_light(bg: Rgb) -> bool {
    let luma = 0.2126 * f32::from(bg.r) + 0.7152 * f32::from(bg.g) + 0.0722 * f32::from(bg.b);
    luma > 127.5
}

/// `[theme]` in config.toml: a preset plus colour overrides.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ThemeConfig {
    pub base: ThemeBase,
    /// A theme file to start from, `themes/NAME.toml` beside the config
    /// (the gallery's themes, D-126); the colours set here win over it.
    pub name: Option<String>,
    #[serde(deserialize_with = "opt_color")]
    pub accent: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub fg: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub muted: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub border: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub status_bg: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub status_fg: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub overlay_bg: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub selection_bg: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub warn: Option<Color>,
    #[serde(deserialize_with = "opt_color")]
    pub error: Option<Color>,
}

impl ThemeConfig {
    /// Fills what this config leaves unset from a theme file's: each
    /// colour, and the base while this one is `auto`.
    pub fn fill_from(&mut self, file: &ThemeConfig) {
        if self.base == ThemeBase::Auto {
            self.base = file.base;
        }
        for (mine, theirs) in [
            (&mut self.accent, file.accent),
            (&mut self.fg, file.fg),
            (&mut self.muted, file.muted),
            (&mut self.border, file.border),
            (&mut self.status_bg, file.status_bg),
            (&mut self.status_fg, file.status_fg),
            (&mut self.overlay_bg, file.overlay_bg),
            (&mut self.selection_bg, file.selection_bg),
            (&mut self.warn, file.warn),
            (&mut self.error, file.error),
        ] {
            if mine.is_none() {
                *mine = theirs;
            }
        }
    }

    /// The theme for a terminal with this background, from `base` (or
    /// `base_override`, a theme picked at run time) plus the overrides.
    pub fn theme(&self, base_override: Option<ThemeBase>, background: Option<Rgb>) -> Theme {
        let base = base_override.unwrap_or(self.base).resolve(background);
        let p = base.colors();
        let colors = ThemeColors {
            accent: self.accent.unwrap_or(p.accent),
            fg: self.fg.unwrap_or(p.fg),
            muted: self.muted.unwrap_or(p.muted),
            border: self.border.unwrap_or(p.border),
            status_bg: self.status_bg.unwrap_or(p.status_bg),
            status_fg: self.status_fg.unwrap_or(p.status_fg),
            overlay_bg: self.overlay_bg.unwrap_or(p.overlay_bg),
            selection_bg: self.selection_bg.unwrap_or(p.selection_bg),
            warn: self.warn.unwrap_or(p.warn),
            error: self.error.unwrap_or(p.error),
        };
        Theme::with_background(colors, base, background)
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
    /// The preset this theme was built from (never `Auto`).
    pub base: ThemeBase,
    /// Icons for apps, files and menus (`[ui] icons`, D-087).
    pub icons: IconSet,
    /// Colour gradients in the chrome: only for terminals with truecolor,
    /// flat colours otherwise (D-088).
    pub gradients: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_colors(ThemeColors::default())
    }
}

impl Theme {
    /// A dark theme from these colours, for an unknown dark background.
    pub fn from_colors(c: ThemeColors) -> Self {
        Self::with_background(c, ThemeBase::Dark, None)
    }

    /// A theme from these colours. `background` is the terminal's real
    /// background (OSC 11), which drop shadows blend toward; without it a
    /// typical background for the preset is assumed.
    pub fn with_background(c: ThemeColors, base: ThemeBase, background: Option<Rgb>) -> Self {
        let on = |fg: Color, bg: Color| Style::new().fg(fg).bg(bg);
        let light = base == ThemeBase::Light;
        let palette = Palette {
            fg: if light {
                Rgb::new(0x1f, 0x23, 0x28)
            } else {
                Rgb::new(0xd0, 0xd7, 0xe2)
            },
            bg: background.unwrap_or(if light {
                Rgb::new(0xff, 0xff, 0xff)
            } else {
                Rgb::new(0x10, 0x14, 0x18)
            }),
            ansi: XTERM_ANSI,
        };
        Self {
            palette: Palette {
                fg: c.fg.resolve(&palette, true),
                ..palette
            },
            border: Style::new().fg(c.border),
            border_active: Style::new().fg(c.accent),
            float_border: Style::new().fg(c.accent),
            float_title: Style::new().fg(c.accent).attrs(Attrs::BOLD),
            shadow: Rgb::BLACK,
            // A light background needs a lighter touch to read as shadow.
            shadow_amount: if light { 0.25 } else { 0.6 },
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
            base,
            icons: IconSet::default(),
            gradients: false,
        }
    }

    /// `c` as RGB, for blending.
    pub fn rgb(&self, c: Color) -> Rgb {
        c.resolve(&self.palette, false)
    }

    /// Fills the background of `rect` with a gradient from `from` at the
    /// left to `to` at the right when gradients are on, and with `from`
    /// alone otherwise (as given, so 256- and 16-colour terminals keep
    /// their palette). The cells' text and foreground stay.
    pub fn paint(&self, grid: &mut Grid, rect: Rect, from: Color, to: Color) {
        let (a, b) = (self.rgb(from), self.rgb(to));
        let steps = rect.w.saturating_sub(1).max(1);
        for dx in 0..rect.w {
            let bg = if self.gradients {
                Color::Rgb(a.blend(b, f32::from(dx) / f32::from(steps)))
            } else {
                from
            };
            for y in rect.y..rect.bottom() {
                if let Some(cell) = grid.get_mut(rect.x + dx, y) {
                    cell.style.bg = bg;
                }
            }
        }
    }

    /// A gradient for a bar of `bg` carrying text in `text` (D-088): from
    /// `bg`, moving away from the text's lightness, so the text reads at
    /// least as well at the far end as at the start.
    pub fn sheen(&self, bg: Color, text: Color) -> (Color, Color) {
        let b = self.rgb(bg);
        let away = if b.luminance() >= self.rgb(text).luminance() {
            Rgb::new(0xff, 0xff, 0xff)
        } else {
            Rgb::BLACK
        };
        (bg, Color::Rgb(b.blend(away, 0.35)))
    }

    /// The title bar of the focused window: the accent, with a sheen.
    pub fn title_bar(&self) -> (Color, Color) {
        self.sheen(self.colors.accent, self.colors.overlay_bg)
    }

    /// Text on [`Theme::title_bar`]: the panel colour, bold, which reads
    /// on every preset's accent.
    pub fn title_text(&self) -> Style {
        Style::new().fg(self.colors.overlay_bg).attrs(Attrs::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        theme: ThemeConfig,
    }

    #[test]
    fn partial_overrides_keep_defaults() {
        let w: Wrapper = toml::from_str("[theme]\naccent = \"colour208\"\n").expect("valid");
        assert_eq!(w.theme.base, ThemeBase::Auto);
        let t = w.theme.theme(None, None);
        assert_eq!(t.border_active.fg, Color::Indexed(208));
        assert_eq!(t.colors.fg, ThemeColors::dark().fg);
    }

    #[test]
    fn bad_colours_and_keys_are_rejected() {
        assert!(toml::from_str::<Wrapper>("[theme]\naccent = \"nope\"\n").is_err());
        assert!(toml::from_str::<Wrapper>("[theme]\nunknown = \"red\"\n").is_err());
        assert!(toml::from_str::<Wrapper>("[theme]\nbase = \"pink\"\n").is_err());
    }

    #[test]
    fn auto_follows_the_background() {
        let cfg = ThemeConfig::default();
        let white = Some(Rgb::new(0xfa, 0xfa, 0xfa));
        let black = Some(Rgb::new(0x10, 0x10, 0x10));
        assert_eq!(cfg.theme(None, white).base, ThemeBase::Light);
        assert_eq!(cfg.theme(None, black).base, ThemeBase::Dark);
        assert_eq!(cfg.theme(None, None).base, ThemeBase::Dark, "unknown: dark");
        // A picked theme wins over auto, and a fixed base ignores the
        // background.
        assert_eq!(
            cfg.theme(Some(ThemeBase::HighContrast), white).base,
            ThemeBase::HighContrast
        );
        let fixed = ThemeConfig {
            base: ThemeBase::Dark,
            ..ThemeConfig::default()
        };
        assert_eq!(fixed.theme(None, white).base, ThemeBase::Dark);
        // Shadows blend toward the real background.
        assert_eq!(
            cfg.theme(None, white).palette.bg,
            Rgb::new(0xfa, 0xfa, 0xfa)
        );
    }

    #[test]
    fn names_round_trip() {
        for b in ThemeBase::ALL {
            assert_eq!(b.name().parse::<ThemeBase>(), Ok(b));
        }
        assert!("solarized".parse::<ThemeBase>().is_err());
    }

    /// WCAG contrast ratio of two colours.
    fn contrast(a: Color, b: Color) -> f32 {
        let lum = |c: Color| {
            let Color::Rgb(Rgb { r, g, b }) = c else {
                panic!("presets use RGB colours")
            };
            let lin = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.039_28 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn gradients_never_lose_contrast() {
        for base in ThemeBase::ALL {
            let mut t = ThemeConfig {
                base,
                ..ThemeConfig::default()
            }
            .theme(None, None);
            t.gradients = true;
            let c = t.colors.clone();
            for (bg, text) in [
                (c.accent, c.overlay_bg),
                (c.accent, c.status_bg),
                (c.warn, c.status_bg),
            ] {
                let (from, to) = t.sheen(bg, text);
                let start = t.rgb(from).contrast(t.rgb(text));
                let end = t.rgb(to).contrast(t.rgb(text));
                assert!(end >= start, "{base}: {start:.2} then {end:.2}");
                assert_ne!(from, to, "{base}: a visible gradient");
            }
        }
    }

    #[test]
    fn high_contrast_meets_aaa() {
        let c = ThemeColors::high_contrast();
        for (fg, bg, what) in [
            (c.fg, c.overlay_bg, "text on overlays"),
            (c.muted, c.overlay_bg, "muted text"),
            (c.accent, c.overlay_bg, "accent"),
            (c.warn, c.overlay_bg, "warnings"),
            (c.error, c.overlay_bg, "errors"),
            (c.status_fg, c.status_bg, "status line"),
            (c.status_bg, c.accent, "active tab"),
            (c.fg, c.selection_bg, "selected text"),
        ] {
            assert!(contrast(fg, bg) >= 7.0, "{what}: {:.1}", contrast(fg, bg));
        }
    }

    /// Every text colour on every background it is drawn on: 7:1 (AAA) in
    /// the high-contrast preset, 4.5:1 (AA) in the others; selected rows
    /// use `fg` (M9's accessibility review).
    #[test]
    fn every_preset_meets_its_level() {
        for (name, c, level) in [
            ("dark", ThemeColors::dark(), 4.5),
            ("light", ThemeColors::light(), 4.5),
            ("high-contrast", ThemeColors::high_contrast(), 7.0),
        ] {
            for (fg, what) in [
                (c.fg, "text"),
                (c.muted, "muted text"),
                (c.accent, "accent"),
                (c.warn, "warnings"),
                (c.error, "errors"),
                (c.status_fg, "status text"),
            ] {
                for (bg, on) in [(c.overlay_bg, "overlays"), (c.status_bg, "the status line")] {
                    let r = contrast(fg, bg);
                    assert!(r >= level, "{name}: {what} on {on}: {r:.2}");
                }
            }
            let sel = c.selected(crate::cell::Style::new().fg(c.error));
            let r = contrast(sel.fg, sel.bg);
            assert!(r >= level, "{name}: selected rows: {r:.2}");
        }
    }
}
