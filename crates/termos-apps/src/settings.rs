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
    Number { min: i64, max: i64, step: i64 },
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
        }
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

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

impl App for Settings {
    fn title(&self) -> String {
        "Settings".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
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
                tui_style(base.fg(c.accent).attrs(Attrs::BOLD)),
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
                        tui_style(base.fg(c.muted)),
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
            let v = s.value(setting);
            assert!(!v.is_empty(), "{} has no default", setting.key);
            match setting.kind {
                Kind::Choice(options) => assert!(options.contains(&v.as_str()), "{v}"),
                Kind::Bool => assert!(v == "true" || v == "false"),
                Kind::Number { min, max, .. } => {
                    let n: i64 = v.parse().expect("a number");
                    assert!((min..=max).contains(&n));
                }
            }
        }
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
