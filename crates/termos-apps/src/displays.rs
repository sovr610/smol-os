// SPDX-License-Identifier: GPL-3.0-or-later
//! Displays (M13e, D-128): the screens a TermOS image's console drives,
//! in the order they sit left to right, each on or off, in which mode and
//! at which text scale; and whether further displays extend the desktop
//! (each its own workspace) or mirror the first.
//!
//! The page shows the connectors sysfs lists and the `[[kms.display]]`
//! entries of `config.toml`. Each change is written there (comments
//! kept, checked as a config first) and the config reloaded: the console
//! then plans its displays again (switching between mirror and extend
//! starts it again).

use std::path::{Path, PathBuf};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, tui_style};
use termos_core::config::{Config, DisplayConfig, DisplaysMode};
use termos_core::input::{KeyCode, KeyEvent};
use termos_hw::sysfs::DrmConnector;
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;
use toml_edit::{ArrayOfTables, Item, Table, value};

/// The scales `s` steps through (none: as the display's density gives).
const SCALES: [Option<f32>; 8] = [
    None,
    Some(1.0),
    Some(1.25),
    Some(1.5),
    Some(1.75),
    Some(2.0),
    Some(2.5),
    Some(3.0),
];

/// One display on the page.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    /// What the card says (none: configured but not plugged in).
    connector: Option<DrmConnector>,
    cfg: DisplayConfig,
}

impl Row {
    /// The modes `←` and `→` step through.
    fn modes(&self) -> Vec<String> {
        let mut m = vec!["preferred".to_owned()];
        if let Some(c) = &self.connector {
            m.extend(c.modes.iter().cloned());
        }
        if !m.contains(&self.cfg.mode) {
            m.push(self.cfg.mode.clone());
        }
        m
    }
}

fn default_display(name: &str) -> DisplayConfig {
    DisplayConfig {
        name: name.to_owned(),
        mode: "preferred".into(),
        scale: None,
        enabled: true,
        primary: false,
    }
}

pub struct DisplaysPage {
    config: PathBuf,
    /// Where sysfs is (`/`; tests use their own tree).
    root: PathBuf,
    rows: Vec<Row>,
    mirror: bool,
    selected: usize,
    message: Option<(String, bool)>,
}

impl std::fmt::Debug for DisplaysPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplaysPage")
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl DisplaysPage {
    /// The page for this machine's displays and `config`.
    pub fn open(config: PathBuf) -> Self {
        Self::with_root(config, PathBuf::from("/"))
    }

    pub fn with_root(config: PathBuf, root: PathBuf) -> Self {
        let mut page = Self {
            config,
            root,
            rows: Vec::new(),
            mirror: false,
            selected: 0,
            message: None,
        };
        page.read();
        page
    }

    /// Reads the connectors and the config again.
    fn read(&mut self) {
        let config = match Config::load(&self.config) {
            Ok(c) => c,
            Err(e) => {
                self.message = Some((format!("{e}"), true));
                Config::default()
            }
        };
        self.mirror = config.kms.displays == DisplaysMode::Mirror;
        self.rows = rows(
            &termos_hw::sysfs::drm_connectors(&self.root),
            &config.kms.display,
        );
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    /// Writes the page into the config, then asks for a reload.
    fn save(&mut self, what: String) -> AppEffect {
        let (rows, mirror) = (&self.rows, self.mirror);
        let saved = crate::config_edit::edit(&self.config, |doc| {
            let kms = crate::config_edit::table(doc, "kms");
            kms.insert("displays", value(if mirror { "mirror" } else { "extend" }));
            let mut list = ArrayOfTables::new();
            for r in rows {
                let mut t = Table::new();
                t.insert("name", value(r.cfg.name.as_str()));
                if r.cfg.mode != "preferred" {
                    t.insert("mode", value(r.cfg.mode.as_str()));
                }
                if let Some(s) = r.cfg.scale {
                    t.insert("scale", value(f64::from(s)));
                }
                if !r.cfg.enabled {
                    t.insert("enabled", value(false));
                }
                if r.cfg.primary {
                    t.insert("primary", value(true));
                }
                list.push(t);
            }
            kms.insert("display", Item::ArrayOfTables(list));
        });
        match saved {
            Ok(()) => {
                self.message = Some((what, false));
                AppEffect::Command("config.reload".into())
            }
            Err(e) => {
                self.message = Some((e, true));
                AppEffect::Redraw
            }
        }
    }

    fn shown_name(&self) -> String {
        self.rows
            .get(self.selected)
            .map(|r| r.cfg.name.clone())
            .unwrap_or_default()
    }
}

/// The page's rows: the primary first, then the configured ones in their
/// order, then the other connected ones (the order the console uses).
fn rows(connectors: &[DrmConnector], configs: &[DisplayConfig]) -> Vec<Row> {
    let mut out: Vec<Row> = configs
        .iter()
        .map(|cfg| Row {
            connector: connectors.iter().find(|c| c.name == cfg.name).cloned(),
            cfg: cfg.clone(),
        })
        .collect();
    out.sort_by_key(|r| !r.cfg.primary);
    for c in connectors.iter().filter(|c| c.connected) {
        if !out.iter().any(|r| r.cfg.name == c.name) {
            out.push(Row {
                connector: Some(c.clone()),
                cfg: default_display(&c.name),
            });
        }
    }
    out
}

fn scale_text(s: Option<f32>) -> String {
    match s {
        None => "auto".into(),
        Some(s) => format!("{s}×"),
    }
}

impl App for DisplaysPage {
    fn reopen(&self) -> Option<String> {
        Some("displays.open".to_owned())
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Display
    }

    fn title(&self) -> String {
        "Displays".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        let i = self.selected;
        let name = self.shown_name();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => AppEffect::Close,
            KeyCode::Up => {
                self.selected = i.saturating_sub(1);
                AppEffect::Redraw
            }
            KeyCode::Down => {
                self.selected = (i + 1).min(self.rows.len().saturating_sub(1));
                AppEffect::Redraw
            }
            KeyCode::Char('r') => {
                self.read();
                self.message = Some(("read again".into(), false));
                AppEffect::Redraw
            }
            KeyCode::Char('x') => {
                self.mirror = !self.mirror;
                let what = if self.mirror {
                    "mirror: every display shows the first (the console starts again)"
                } else {
                    "extend: each display shows its own workspace (the console starts again)"
                };
                self.save(what.into())
            }
            _ if self.rows.is_empty() => AppEffect::None,
            KeyCode::Char(' ') | KeyCode::Enter => {
                let on = !self.rows[i].cfg.enabled;
                self.rows[i].cfg.enabled = on;
                self.save(format!("{name} {}", if on { "on" } else { "off" }))
            }
            KeyCode::Left | KeyCode::Right => {
                let modes = self.rows[i].modes();
                let at = modes
                    .iter()
                    .position(|m| *m == self.rows[i].cfg.mode)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Right {
                    (at + 1) % modes.len()
                } else {
                    (at + modes.len() - 1) % modes.len()
                };
                self.rows[i].cfg.mode = modes[next].clone();
                self.save(format!("{name}: {}", modes[next]))
            }
            KeyCode::Char('s') => {
                let at = SCALES
                    .iter()
                    .position(|s| *s == self.rows[i].cfg.scale)
                    .unwrap_or(0);
                let next = SCALES[(at + 1) % SCALES.len()];
                self.rows[i].cfg.scale = next;
                self.save(format!("{name}: text {}", scale_text(next)))
            }
            KeyCode::Char('p') => {
                for r in &mut self.rows {
                    r.cfg.primary = false;
                }
                let mut row = self.rows.remove(i);
                row.cfg.primary = true;
                row.cfg.enabled = true;
                self.rows.insert(0, row);
                self.selected = 0;
                self.save(format!("{name} is the primary display"))
            }
            KeyCode::Char('[' | ']') => {
                let to = if key.code == KeyCode::Char('[') {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|t| *t < self.rows.len())
                };
                let Some(to) = to else {
                    return AppEffect::None;
                };
                self.rows.swap(i, to);
                // The primary stays first.
                for (k, r) in self.rows.iter_mut().enumerate() {
                    r.cfg.primary = r.cfg.primary && k == 0;
                }
                self.selected = to;
                self.save(format!("{name} moved"))
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 5 || area.width < 30 {
            return;
        }
        let c = &theme.colors;
        let width = usize::from(area.width.saturating_sub(2));
        let bottom = area.y + area.height - 2;
        let line = |buf: &mut Buffer, y: u16, x: u16, text: &str, style: Style| {
            if y < bottom {
                buf.set_stringn(
                    area.x + x,
                    y,
                    text,
                    width.saturating_sub(usize::from(x)),
                    tui_style(style),
                );
            }
        };
        let mut y = area.y;
        let several = if self.mirror {
            "Mirrored: every display shows the first"
        } else {
            "Extended: each display shows its own workspace"
        };
        line(buf, y, 1, several, Style::new().fg(c.fg).attrs(Attrs::BOLD));
        y += 2;
        if self.rows.is_empty() {
            line(
                buf,
                y,
                3,
                "No displays found (the console's displays: TermOS images)",
                Style::new().fg(c.muted),
            );
        } else {
            line(buf, y, 3, "Display", Style::new().fg(c.muted));
            line(buf, y, 17, "State", Style::new().fg(c.muted));
            line(buf, y, 28, "Mode", Style::new().fg(c.muted));
            line(buf, y, 42, "Text", Style::new().fg(c.muted));
            y += 1;
        }
        for (i, r) in self.rows.iter().enumerate() {
            let selected = i == self.selected;
            let paint = |colour| {
                if selected {
                    c.selected(Style::new())
                } else {
                    Style::new().fg(colour)
                }
            };
            if selected && y < bottom {
                buf.set_style(
                    TuiRect::new(area.x, y, area.width, 1),
                    tui_style(paint(c.fg)),
                );
            }
            let plugged = r.connector.as_ref().is_some_and(|c| c.connected);
            let state = match (plugged, r.cfg.enabled) {
                (false, _) => "unplugged",
                (true, true) => "on",
                (true, false) => "off",
            };
            line(
                buf,
                y,
                1,
                if r.cfg.primary { "●" } else { " " },
                paint(c.accent),
            );
            line(buf, y, 3, &r.cfg.name, paint(c.fg));
            line(
                buf,
                y,
                17,
                state,
                paint(if plugged { c.accent } else { c.muted }),
            );
            line(buf, y, 28, &r.cfg.mode, paint(c.fg));
            line(buf, y, 42, &scale_text(r.cfg.scale), paint(c.fg));
            y += 1;
        }
        if !self.rows.is_empty() {
            y += 1;
            line(
                buf,
                y,
                1,
                "Left to right as listed; ● the primary, where the first workspace starts.",
                Style::new().fg(c.muted),
            );
        }
        let (msg, err) = self
            .message
            .as_ref()
            .map_or(("", false), |(m, e)| (m.as_str(), *e));
        buf.set_stringn(
            area.x + 1,
            bottom,
            msg,
            width,
            tui_style(Style::new().fg(if err { c.error } else { c.muted })),
        );
        buf.set_stringn(
            area.x + 1,
            bottom + 1,
            "Space on/off · ←→ mode · s text · p primary · [ ] move · x mirror/extend · Esc",
            width,
            tui_style(Style::new().fg(c.muted)),
        );
    }
}

/// Whether `config` drives this display on the console (for tests and the
/// Settings page).
pub fn configured(config: &Path, name: &str) -> Option<DisplayConfig> {
    Config::load(config)
        .ok()?
        .kms
        .display
        .into_iter()
        .find(|d| d.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::input::Mods;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Mods::empty())
    }

    /// A machine with a laptop panel, an HDMI screen and an empty port.
    fn machine() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let drm = dir.path().join("sys/class/drm");
        for (c, status, modes) in [
            ("card0-eDP-1", "connected", "2560x1600\n1920x1200\n"),
            ("card0-HDMI-A-1", "connected", "1920x1080\n1280x720\n"),
            ("card0-DP-1", "disconnected", ""),
        ] {
            let d = drm.join(c);
            std::fs::create_dir_all(&d).expect("mkdir");
            std::fs::write(d.join("status"), status).expect("write");
            std::fs::write(d.join("modes"), modes).expect("write");
        }
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "# my settings\n[kms]\nfont-size = 18\n").expect("write");
        (dir, config)
    }

    #[test]
    fn the_page_lists_the_displays() {
        let (dir, config) = machine();
        let mut page = DisplaysPage::with_root(config, dir.path().to_owned());
        insta::assert_snapshot!(
            "displays-page",
            crate::testing::render_text(&mut page, 80, 10)
        );
    }

    #[test]
    fn changes_are_written_to_the_config() {
        let (dir, config) = machine();
        let mut page = DisplaysPage::with_root(config.clone(), dir.path().to_owned());
        // HDMI (listed first, by name): the primary, at 720p, text 1×.
        assert_eq!(
            page.handle_key(key(KeyCode::Char('p'))),
            AppEffect::Command("config.reload".into())
        );
        page.handle_key(key(KeyCode::Right));
        page.handle_key(key(KeyCode::Right));
        page.handle_key(key(KeyCode::Char('s')));
        // The laptop panel off.
        page.handle_key(key(KeyCode::Down));
        page.handle_key(key(KeyCode::Char(' ')));
        let text = std::fs::read_to_string(&config).expect("saved");
        assert!(
            text.starts_with("# my settings\n[kms]\nfont-size = 18\n"),
            "{text}"
        );
        let c = Config::load(&config).expect("valid");
        assert_eq!(c.kms.font_size, 18.0);
        assert_eq!(c.kms.displays, DisplaysMode::Extend);
        let names: Vec<&str> = c.kms.display.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["HDMI-A-1", "eDP-1"]);
        let hdmi = &c.kms.display[0];
        assert!(hdmi.primary && hdmi.enabled);
        assert_eq!(hdmi.mode, "1280x720");
        assert_eq!(hdmi.scale, Some(1.0));
        assert!(!c.kms.display[1].enabled);
        assert_eq!(configured(&config, "eDP-1").map(|d| d.enabled), Some(false));
        // Moving the panel first takes the primary's place, not its role.
        page.handle_key(key(KeyCode::Char('[')));
        let c = Config::load(&config).expect("valid");
        assert_eq!(c.kms.display[0].name, "eDP-1");
        assert!(c.kms.display.iter().all(|d| !d.primary));
        // Mirror.
        page.handle_key(key(KeyCode::Char('x')));
        assert_eq!(
            Config::load(&config).expect("valid").kms.displays,
            DisplaysMode::Mirror
        );
        // An unplugged display that is configured stays listed.
        std::fs::write(
            dir.path().join("sys/class/drm/card0-HDMI-A-1/status"),
            "disconnected",
        )
        .expect("write");
        page.handle_key(key(KeyCode::Char('r')));
        let shown = crate::testing::render_text(&mut page, 80, 10);
        assert!(shown.contains("HDMI-A-1      unplugged"), "{shown}");
    }
}
