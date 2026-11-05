// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings: the common options of `config.toml`, changed with the arrow
//! keys. Each change is written into the file itself (keeping its
//! comments and layout), checked by parsing the result as a config first,
//! and then the config is reloaded.

use std::path::{Path, PathBuf};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, tui_style};
use termos_core::config::{Config, DEFAULT_CONFIG};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;
use toml_edit::{DocumentMut, Item, Table, Value};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Choice(&'static [&'static str]),
    Number {
        min: i64,
        max: i64,
        step: i64,
    },
    /// Not a setting: Enter runs this desktop command.
    Action(&'static str),
    /// The image's time zone (`/etc/localtime`): Enter opens a list to
    /// search (D-124).
    Zone,
}

#[derive(Debug, Clone, Copy)]
struct Setting {
    group: &'static str,
    /// The table (`""` for top-level keys) and the key.
    table: &'static str,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

const fn s(
    group: &'static str,
    table: &'static str,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: Kind,
) -> Setting {
    Setting {
        group,
        table,
        key,
        label,
        help,
        kind,
    }
}

const SETTINGS: &[Setting] = &[
    s(
        "Appearance",
        "theme",
        "base",
        "Theme",
        "auto follows each terminal's background",
        Kind::Choice(&["auto", "dark", "light", "high-contrast"]),
    ),
    s(
        "Appearance",
        "floats",
        "border",
        "Window borders",
        "floating windows, the palette and which-key",
        Kind::Choice(&["rounded", "square", "heavy", "double"]),
    ),
    s(
        "Appearance",
        "floats",
        "shadow",
        "Drop shadows",
        "under floating windows and panels",
        Kind::Bool,
    ),
    s(
        "Appearance",
        "ui",
        "start",
        "Start on",
        "new sessions and workspaces: the desktop, or a full-screen terminal",
        Kind::Choice(&["desktop", "terminal"]),
    ),
    s(
        "Appearance",
        "ui",
        "icons",
        "Icons",
        "nerd needs a Nerd Font in your terminal",
        Kind::Choice(&["unicode", "nerd", "ascii", "none"]),
    ),
    s(
        "Appearance",
        "ui",
        "gradients",
        "Gradients",
        "in title bars, on terminals with true colour",
        Kind::Bool,
    ),
    s(
        "Appearance",
        "ui",
        "wallpaper",
        "Wallpaper",
        "behind empty workspaces; or a picture's path in the config file",
        Kind::Choice(&["pattern", "none"]),
    ),
    s(
        "Appearance",
        "ui",
        "splash",
        "Splash screen",
        "the logo for a moment when you attach",
        Kind::Bool,
    ),
    s(
        "Appearance",
        "ui",
        "animations",
        "Animations",
        "never over SSH",
        Kind::Bool,
    ),
    s(
        "Appearance",
        "ui",
        "reduced-motion",
        "Reduced motion",
        "no animations for any terminal",
        Kind::Bool,
    ),
    s(
        "Appearance",
        "graphics",
        "protocol",
        "Images",
        "auto picks the best each terminal can show",
        Kind::Choice(&["auto", "kitty", "sixel", "iterm2", "blocks"]),
    ),
    s(
        "Status line",
        "status",
        "position",
        "Position",
        "",
        Kind::Choice(&["bottom", "top"]),
    ),
    s(
        "Status line",
        "status",
        "clock",
        "Clock",
        "redraws once a minute",
        Kind::Bool,
    ),
    s(
        "Input",
        "",
        "mouse",
        "Mouse",
        "click to focus, drag to resize and move",
        Kind::Bool,
    ),
    s(
        "Input",
        "whichkey",
        "enabled",
        "Key hints",
        "list the keys after a pause on the prefix",
        Kind::Bool,
    ),
    s(
        "Input",
        "whichkey",
        "delay-ms",
        "Key hints after (ms)",
        "",
        Kind::Number {
            min: 0,
            max: 5000,
            step: 100,
        },
    ),
    s(
        "Sessions",
        "session",
        "restore",
        "Restore after a crash",
        "saved sessions when the server starts",
        Kind::Choice(&["ask", "always", "never"]),
    ),
    s(
        "Sessions",
        "session",
        "save-scrollback",
        "Save scrollback",
        "pane text in snapshots (may hold secrets)",
        Kind::Bool,
    ),
    s(
        "Developer",
        "dev",
        "editor",
        "Editor",
        "auto: $VISUAL/$EDITOR, else TermOS's Neovim",
        Kind::Choice(&["auto", "termos-nvim", "helix", "env"]),
    ),
    s(
        "Clipboard",
        "clipboard",
        "history",
        "History entries",
        "kept in memory only; 0 keeps none",
        Kind::Number {
            min: 0,
            max: 500,
            step: 10,
        },
    ),
    s(
        "Command center",
        "center",
        "keep-days",
        "History kept (days)",
        "AI usage, tool calls and the machine's load",
        Kind::Number {
            min: 1,
            max: 365,
            step: 1,
        },
    ),
    s(
        "Command center",
        "center",
        "sample",
        "Record the machine",
        "CPU, memory and network, also while it is closed",
        Kind::Choice(&["1m", "5m", "off"]),
    ),
    s(
        "Command center",
        "center",
        "cli-usage",
        "CLI agents' usage",
        "Claude Code and Codex tokens from their logs, not text",
        Kind::Bool,
    ),
    s(
        "Lock and power",
        "lock",
        "after-minutes",
        "Lock after (minutes)",
        "without input; 0 never (TermOS images)",
        Kind::Number {
            min: 0,
            max: 120,
            step: 5,
        },
    ),
    s(
        "Lock and power",
        "lock",
        "at-start",
        "Lock at start",
        "the console asks for the password like a login",
        Kind::Bool,
    ),
    s(
        "Lock and power",
        "power",
        "lid",
        "Closing the lid",
        "lock, then suspend; or only lock",
        Kind::Choice(&["suspend", "lock", "nothing"]),
    ),
    s(
        "Lock and power",
        "power",
        "suspend-after-minutes",
        "Suspend after (minutes)",
        "on battery, without input; 0 never",
        Kind::Number {
            min: 0,
            max: 240,
            step: 5,
        },
    ),
    s(
        "Lock and power",
        "power",
        "critical",
        "Suspend at (battery %)",
        "after a 30-second countdown; 0 never",
        Kind::Number {
            min: 0,
            max: 20,
            step: 1,
        },
    ),
    s(
        "Clock",
        "clock",
        "time-format",
        "Time format",
        "the status line's clock",
        Kind::Choice(&["24h", "12h"]),
    ),
    s(
        "Clock",
        "",
        "",
        "Time zone",
        "type to search the zones (TermOS images)",
        Kind::Zone,
    ),
    s(
        "System",
        "",
        "",
        "Updates",
        "check for a newer TermOS and install it (TermOS machines)",
        Kind::Action("update.open"),
    ),
];

fn lookup<'a>(doc: &'a DocumentMut, table: &str, key: &str) -> Option<&'a Item> {
    if table.is_empty() {
        doc.get(key)
    } else {
        doc.get(table)?.get(key)
    }
}

/// A value as shown and compared: `true`, `rounded`, `500`.
fn shown(item: &Item) -> Option<String> {
    let v = item.as_value()?;
    Some(match v {
        Value::Boolean(b) => b.value().to_string(),
        Value::String(s) => s.value().clone(),
        Value::Integer(i) => i.value().to_string(),
        _ => return None,
    })
}

pub struct Settings {
    path: PathBuf,
    /// The user's file (`None` when it cannot be read or parsed).
    doc: Result<DocumentMut, String>,
    defaults: DocumentMut,
    selected: usize,
    message: Option<(String, bool)>,
    /// The time zone list, while it is open.
    zones: Option<ZonePicker>,
    /// Where the zones are (the image's zoneinfo; tests use their own).
    zoneinfo: PathBuf,
}

/// The time zone list: every zone, a search, a choice.
#[derive(Debug, Clone, Default)]
struct ZonePicker {
    all: Vec<String>,
    query: String,
    selected: usize,
}

impl ZonePicker {
    /// The zones the search matches: every word of it, ignoring case, `_`
    /// and `/` read as spaces.
    fn shown(&self) -> Vec<&str> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.all
            .iter()
            .map(String::as_str)
            .filter(|z| {
                let z = z.to_lowercase().replace(['_', '/'], " ");
                words.iter().all(|w| z.contains(w.as_str()))
            })
            .collect()
    }
}

/// Whether this is a TermOS image (where TermOS sets the time zone).
fn on_image() -> bool {
    Path::new("/etc/termos/board").exists()
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Settings {
    pub fn open(path: PathBuf) -> Self {
        let doc = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<DocumentMut>()
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        };
        Self {
            path,
            doc,
            // The commented example holds every default.
            defaults: DEFAULT_CONFIG.parse().unwrap_or_default(),
            selected: 0,
            message: None,
            zones: None,
            zoneinfo: PathBuf::from(termos_core::timezones::ZONEINFO),
        }
    }

    /// Opens the time zone list (TermOS images: elsewhere the host system
    /// keeps the time).
    fn open_zones(&mut self, image: bool) -> AppEffect {
        if !image {
            self.message = Some((
                "the time zone is the host system's: TermOS sets it on its own images".into(),
                false,
            ));
            return AppEffect::Redraw;
        }
        let all = termos_core::timezones::zones(&self.zoneinfo);
        if all.is_empty() {
            self.message = Some((format!("no zones in {}", self.zoneinfo.display()), true));
            return AppEffect::Redraw;
        }
        self.zones = Some(ZonePicker {
            all,
            ..ZonePicker::default()
        });
        AppEffect::Redraw
    }

    /// A key while the time zone list is open.
    fn zone_key(&mut self, key: KeyEvent) -> AppEffect {
        let Some(p) = self.zones.as_mut() else {
            return AppEffect::None;
        };
        match key.code {
            KeyCode::Esc => self.zones = None,
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => p.selected = (p.selected + 1).min(p.shown().len().saturating_sub(1)),
            KeyCode::Backspace => {
                p.query.pop();
                p.selected = 0;
            }
            KeyCode::Char(c)
                if key.mods.is_empty() || key.mods == termos_core::input::Mods::SHIFT =>
            {
                p.query.push(c);
                p.selected = 0;
            }
            KeyCode::Enter => {
                let Some(zone) = p.shown().get(p.selected).map(|z| (*z).to_owned()) else {
                    return AppEffect::None;
                };
                self.zones = None;
                self.message = Some((format!("time zone: {zone}"), false));
                return AppEffect::Command(format!("clock.zone {zone}"));
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    /// The current value of a setting: the file's, else the default.
    fn value(&self, s: &Setting) -> String {
        self.doc
            .as_ref()
            .ok()
            .and_then(|d| lookup(d, s.table, s.key))
            .and_then(shown)
            .or_else(|| lookup(&self.defaults, s.table, s.key).and_then(shown))
            .unwrap_or_default()
    }

    /// Steps the selected setting forward or back.
    fn change(&mut self, forward: bool) -> AppEffect {
        let s = SETTINGS[self.selected];
        if s.kind == Kind::Zone {
            return if forward {
                self.open_zones(on_image())
            } else {
                AppEffect::None
            };
        }
        if let Kind::Action(command) = s.kind {
            return if forward {
                AppEffect::Command(command.to_owned())
            } else {
                AppEffect::None
            };
        }
        let current = self.value(&s);
        let new: Value = match s.kind {
            Kind::Bool => Value::from(current != "true"),
            Kind::Choice(options) => {
                let i = options.iter().position(|o| *o == current).unwrap_or(0);
                let n = options.len();
                let j = if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                Value::from(options[j])
            }
            Kind::Number { min, max, step } => {
                let v: i64 = current.parse().unwrap_or(min);
                let v = if forward { v + step } else { v - step };
                Value::from(v.clamp(min, max))
            }
            Kind::Action(_) | Kind::Zone => return AppEffect::None,
        };
        self.save(&s, new)
    }

    fn save(&mut self, s: &Setting, value: Value) -> AppEffect {
        let Ok(doc) = &self.doc else {
            return AppEffect::None;
        };
        let mut edited = doc.clone();
        if s.table.is_empty() {
            edited[s.key] = Item::Value(value);
        } else {
            if !edited.contains_table(s.table) {
                let mut t = Table::new();
                t.set_implicit(false);
                edited.insert(s.table, Item::Table(t));
            }
            edited[s.table][s.key] = Item::Value(value);
        }
        let text = edited.to_string();
        // Never write a file TermOS would refuse.
        if let Err(e) = Config::parse(&text, &self.path) {
            self.message = Some((format!("not saved: {e}"), true));
            return AppEffect::Redraw;
        }
        match write_atomically(&self.path, &text) {
            Ok(()) => {
                self.doc = Ok(edited);
                self.message = Some((format!("saved to {}", self.path.display()), false));
                AppEffect::Command("config.reload".into())
            }
            Err(e) => {
                self.message = Some((format!("cannot save {}: {e}", self.path.display()), true));
                AppEffect::Redraw
            }
        }
    }

    fn select(&mut self, i: usize) -> AppEffect {
        let i = i.min(SETTINGS.len() - 1);
        if i == self.selected {
            return AppEffect::None;
        }
        self.selected = i;
        self.message = None;
        AppEffect::Redraw
    }

    /// The screen rows of the settings (with group headings in between):
    /// `(row, setting index)`.
    fn rows() -> Vec<(u16, usize)> {
        let mut out = Vec::new();
        let mut row = 0u16;
        let mut group = "";
        for (i, s) in SETTINGS.iter().enumerate() {
            if s.group != group {
                if !group.is_empty() {
                    row += 1;
                }
                group = s.group;
                row += 1;
            }
            out.push((row, i));
            row += 1;
        }
        out
    }
}

/// The time zone list: the search on top, the matching zones below.
fn render_zones(p: &ZonePicker, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
    let c = &theme.colors;
    let w = usize::from(area.width.saturating_sub(2));
    buf.set_stringn(
        area.x + 1,
        area.y,
        format!("Time zone: {}▏", p.query),
        w,
        tui_style(Style::new().fg(c.fg).attrs(Attrs::BOLD)),
    );
    let shown = p.shown();
    let body = usize::from(area.height.saturating_sub(3));
    let first = p.selected.saturating_sub(body.saturating_sub(1));
    for (i, z) in shown.iter().enumerate().skip(first).take(body) {
        let Ok(y) = u16::try_from(i - first) else {
            break;
        };
        let y = area.y + 1 + y;
        let style = if i == p.selected {
            c.selected(Style::new().bg(c.selection_bg))
        } else {
            Style::new().fg(c.fg)
        };
        if i == p.selected {
            buf.set_style(TuiRect::new(area.x, y, area.width, 1), tui_style(style));
        }
        buf.set_stringn(area.x + 3, y, z, w.saturating_sub(2), tui_style(style));
    }
    buf.set_stringn(
        area.x + 1,
        area.bottom() - 1,
        format!(
            "{} of {} zones · type to search · ↑↓ choose · Enter sets it · Esc back",
            shown.len(),
            p.all.len()
        ),
        w,
        tui_style(Style::new().fg(c.muted)),
    );
}

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl App for Settings {
    fn reopen(&self) -> Option<String> {
        Some("settings.open".to_owned())
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Settings
    }

    fn title(&self) -> String {
        "Settings".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if self.zones.is_some() {
            return self.zone_key(key);
        }
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                self.select(self.selected.saturating_sub(1))
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                self.change(true)
            }
            KeyCode::Char('h') | KeyCode::Left => self.change(false),
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::Down(MouseButton::Left) => {
                let hit = Self::rows()
                    .into_iter()
                    .find(|(row, _)| *row == event.y)
                    .map(|(_, i)| i);
                match hit {
                    Some(i) if i == self.selected => self.change(true),
                    Some(i) => self.select(i),
                    None => AppEffect::None,
                }
            }
            MouseKind::ScrollDown => self.select(self.selected + 1),
            MouseKind::ScrollUp => self.select(self.selected.saturating_sub(1)),
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 3 || area.width < 20 {
            return;
        }
        let c = &theme.colors;
        if let Err(e) = &self.doc {
            buf.set_stringn(
                area.x + 1,
                area.y,
                e,
                usize::from(area.width - 1),
                tui_style(Style::new().fg(c.error)),
            );
            buf.set_stringn(
                area.x + 1,
                area.y + 1,
                "Fix the file by hand (termos config check shows where), then reopen Settings.",
                usize::from(area.width - 1),
                tui_style(Style::new().fg(c.muted)),
            );
            return;
        }
        if let Some(p) = &self.zones {
            render_zones(p, area, buf, theme);
            return;
        }
        let label_w = SETTINGS
            .iter()
            .map(|s| UnicodeWidthStr::width(s.label))
            .max()
            .unwrap_or(10)
            + 4;
        let rows = Self::rows();
        let body_h = area.height.saturating_sub(2);
        // Keep the selected row in view.
        let sel_row = rows
            .iter()
            .find(|(_, i)| *i == self.selected)
            .map_or(0, |(r, _)| *r);
        let offset = sel_row.saturating_sub(body_h.saturating_sub(2));
        let mut group = "";
        for (row, i) in &rows {
            let s = &SETTINGS[*i];
            if s.group != group {
                group = s.group;
                let gy = row.checked_sub(1).and_then(|r| r.checked_sub(offset));
                if let Some(gy) = gy.filter(|y| *y < body_h) {
                    buf.set_string(
                        area.x + 1,
                        area.y + gy,
                        s.group,
                        tui_style(Style::new().fg(c.fg).attrs(Attrs::BOLD)),
                    );
                }
            }
            let Some(y) = row.checked_sub(offset).filter(|y| *y < body_h) else {
                continue;
            };
            let y = area.y + y;
            let selected = *i == self.selected;
            let base = if selected {
                Style::new().bg(c.selection_bg)
            } else {
                Style::new()
            };
            if selected {
                buf.set_style(TuiRect::new(area.x, y, area.width, 1), tui_style(base));
            }
            buf.set_stringn(area.x + 3, y, s.label, label_w, tui_style(base.fg(c.fg)));
            let value = match s.kind {
                Kind::Bool if self.value(s) == "true" => "on".to_owned(),
                Kind::Bool => "off".to_owned(),
                Kind::Action(_) => "open".to_owned(),
                Kind::Zone => termos_core::timezones::current_zone(Path::new("/etc/localtime"))
                    .unwrap_or_else(|| "the system's".to_owned()),
                _ => self.value(s),
            };
            let value = if selected {
                format!("‹ {value} ›")
            } else {
                format!("  {value}  ")
            };
            let vx = area.x + 3 + u16::try_from(label_w).unwrap_or(20);
            buf.set_string(
                vx,
                y,
                &value,
                tui_style(
                    if selected {
                        c.selected(base)
                    } else {
                        base.fg(c.accent)
                    }
                    .attrs(Attrs::BOLD),
                ),
            );
            if selected && !s.help.is_empty() {
                let hx =
                    vx + u16::try_from(UnicodeWidthStr::width(value.as_str()) + 2).unwrap_or(0);
                if hx < area.right() {
                    buf.set_stringn(
                        hx,
                        y,
                        s.help,
                        usize::from(area.right() - hx),
                        tui_style(c.selected(base).attrs(Attrs::ITALIC)),
                    );
                }
            }
        }
        let (line, style) = match &self.message {
            Some((m, true)) => (m.clone(), Style::new().fg(c.error)),
            Some((m, false)) => (m.clone(), Style::new().fg(c.muted)),
            None => (
                "↑↓ choose · ←→ change · saved at once · q close".to_owned(),
                Style::new().fg(c.muted),
            ),
        };
        buf.set_stringn(
            area.x + 1,
            area.bottom() - 1,
            line,
            usize::from(area.width - 1),
            tui_style(style),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_has_a_default_and_parses() {
        let s = Settings::open(PathBuf::from("/nonexistent/config.toml"));
        for setting in SETTINGS {
            if let Kind::Action(command) = setting.kind {
                assert!(!command.is_empty());
                continue;
            }
            if setting.kind == Kind::Zone {
                continue;
            }
            let v = s.value(setting);
            assert!(!v.is_empty(), "{} has no default", setting.key);
            match setting.kind {
                Kind::Choice(options) => assert!(options.contains(&v.as_str()), "{v}"),
                Kind::Bool => assert!(v == "true" || v == "false"),
                Kind::Number { min, max, .. } => {
                    let n: i64 = v.parse().expect("a number");
                    assert!((min..=max).contains(&n));
                }
                Kind::Action(_) | Kind::Zone => {}
            }
        }
    }

    #[test]
    fn the_time_zone_list_searches_and_sets() {
        let dir = tempfile::tempdir().expect("dir");
        for z in [
            "Europe/Paris",
            "America/New_York",
            "America/Argentina/Buenos_Aires",
            "Etc/UTC",
        ] {
            let f = dir.path().join(z);
            std::fs::create_dir_all(f.parent().expect("parent")).expect("mkdir");
            std::fs::write(f, "TZif2").expect("write");
        }
        let mut s = Settings::open(PathBuf::from("/nonexistent/config.toml"));
        s.zoneinfo = dir.path().to_owned();
        let key = |c| KeyEvent::new(c, termos_core::input::Mods::empty());
        // Elsewhere than an image, the host system keeps the time.
        assert_eq!(s.open_zones(false), AppEffect::Redraw);
        assert!(s.zones.is_none());
        s.open_zones(true);
        assert_eq!(s.zones.as_ref().map(|p| p.shown().len()), Some(4));
        for c in "new york".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.zones.as_ref().map(ZonePicker::shown),
            Some(vec!["America/New_York"])
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            AppEffect::Command("clock.zone America/New_York".into())
        );
        assert!(s.zones.is_none());
        s.open_zones(true);
        for c in "AIRES".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.zones.as_ref().map(ZonePicker::shown),
            Some(vec!["America/Argentina/Buenos_Aires"])
        );
        s.handle_key(key(KeyCode::Esc));
        assert!(s.zones.is_none());
    }

    #[test]
    fn updates_open_from_settings() {
        let mut s = Settings::open(PathBuf::from("/nonexistent/config.toml"));
        let last = SETTINGS.len() - 1;
        s.select(last);
        assert_eq!(
            s.handle_key(KeyEvent::new(
                KeyCode::Enter,
                termos_core::input::Mods::empty()
            )),
            AppEffect::Command("update.open".into())
        );
        assert_eq!(
            s.handle_key(KeyEvent::new(
                KeyCode::Left,
                termos_core::input::Mods::empty()
            )),
            AppEffect::None
        );
    }

    #[test]
    fn changes_keep_comments_and_are_valid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my settings\nprefix = \"C-a\"\n\n[status]\n# where the bar goes\nposition = \"bottom\"\n").expect("write");
        let mut s = Settings::open(path.clone());
        // Theme: auto → dark.
        assert_eq!(
            s.handle_key(KeyEvent::plain(KeyCode::Right)),
            AppEffect::Command("config.reload".into())
        );
        // Down to the status position: bottom → top.
        let pos = SETTINGS
            .iter()
            .position(|x| x.key == "position")
            .expect("listed");
        s.selected = pos;
        let _ = s.handle_key(KeyEvent::plain(KeyCode::Right));
        // Mouse is a top-level key: on → off.
        s.selected = SETTINGS
            .iter()
            .position(|x| x.key == "mouse")
            .expect("listed");
        let _ = s.handle_key(KeyEvent::plain(KeyCode::Enter));
        let text = std::fs::read_to_string(&path).expect("saved");
        assert!(
            text.contains("# my settings") && text.contains("# where the bar goes"),
            "{text}"
        );
        let config = Config::parse(&text, &path).expect("valid");
        assert_eq!(config.prefix.to_string(), "C-a");
        assert!(!config.mouse);
        assert_eq!(
            config.status.position,
            termos_core::config::StatusPosition::Top
        );
        assert_eq!(config.theme.base, termos_render::theme::ThemeBase::Dark);
        // Numbers step and stay in range.
        s.selected = SETTINGS
            .iter()
            .position(|x| x.key == "history")
            .expect("listed");
        for _ in 0..100 {
            let _ = s.handle_key(KeyEvent::plain(KeyCode::Left));
        }
        assert_eq!(s.value(&SETTINGS[s.selected]), "0");
        let snap = crate::testing::render_text(&mut s, 80, 26);
        // The path differs per run; the window does not.
        let snap: String = snap
            .lines()
            .map(|l| {
                if l.contains("saved to") {
                    " saved".to_owned()
                } else {
                    l.to_owned()
                }
            })
            .map(|l| format!("{l}\n"))
            .collect();
        insta::assert_snapshot!("settings-window", snap);
    }

    #[test]
    fn a_broken_file_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "prefix = [broken").expect("write");
        let mut s = Settings::open(path.clone());
        assert_eq!(
            s.handle_key(KeyEvent::plain(KeyCode::Right)),
            AppEffect::None
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "prefix = [broken"
        );
        let text = crate::testing::render_text(&mut s, 80, 4);
        assert!(text.contains("Fix the file by hand"), "{text}");
    }
}
