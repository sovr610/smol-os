// SPDX-License-Identifier: GPL-3.0-or-later
//! The clipboard manager (prefix `=`, like tmux's choose-buffer): what was
//! copied inside TermOS, newest first. Enter pastes an entry into the pane
//! you came from; entries can be copied again, pinned and deleted.
//!
//! The history lives in the server's memory only (D-050).

use std::time::SystemTime;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, tui_style};
use termos_core::clipboard::{ClipEntry, SharedClipboard};
use termos_core::input::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Browse,
    Filter,
    ConfirmClear,
}

pub struct ClipboardManager {
    history: SharedClipboard,
    /// The entries shown (a copy taken each time it is drawn).
    shown: Vec<ClipEntry>,
    selected: usize,
    top: usize,
    filter: String,
    mode: Mode,
    rows: u16,
}

impl std::fmt::Debug for ClipboardManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardManager")
            .field("entries", &self.shown.len())
            .finish_non_exhaustive()
    }
}

/// "now", "5m", "3h", "2d".
fn age(at: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(at).map_or(0, |d| d.as_secs());
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// The first line, with a mark when there is more.
fn summary(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").replace('\t', "  ");
    let more = text.lines().count().saturating_sub(1);
    if more == 0 {
        first
    } else {
        format!("{first} ⏎ +{more} lines")
    }
}

fn fit(text: &str, max: usize) -> String {
    if UnicodeWidthStr::width(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

impl ClipboardManager {
    pub fn new(history: SharedClipboard) -> Self {
        let mut m = Self {
            history,
            shown: Vec::new(),
            selected: 0,
            top: 0,
            filter: String::new(),
            mode: Mode::Browse,
            rows: 0,
        };
        m.refresh();
        m
    }

    fn refresh(&mut self) {
        let keep = self.shown.get(self.selected).map(|e| e.id);
        let filter = self.filter.to_lowercase();
        self.shown = match self.history.lock() {
            Ok(h) => h
                .entries()
                .filter(|e| filter.is_empty() || e.text.to_lowercase().contains(&filter))
                .cloned()
                .collect(),
            Err(_) => Vec::new(),
        };
        self.selected = keep
            .and_then(|id| self.shown.iter().position(|e| e.id == id))
            .unwrap_or(self.selected)
            .min(self.shown.len().saturating_sub(1));
    }

    fn current(&self) -> Option<&ClipEntry> {
        self.shown.get(self.selected)
    }

    fn with_history(&mut self, f: impl FnOnce(&mut termos_core::clipboard::ClipboardHistory)) {
        if let Ok(mut h) = self.history.lock() {
            f(&mut h);
        }
        self.refresh();
    }

    fn select(&mut self, i: usize) -> AppEffect {
        let i = i.min(self.shown.len().saturating_sub(1));
        if i == self.selected {
            return AppEffect::None;
        }
        self.selected = i;
        AppEffect::Redraw
    }

    fn browse_key(&mut self, key: KeyEvent) -> AppEffect {
        if !key.mods.is_empty() {
            return AppEffect::None;
        }
        let page = usize::from(self.rows.max(2) - 1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::PageDown => self.select(self.selected + page),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Enter => match self.current() {
                Some(e) => AppEffect::PasteAndClose(e.text.clone()),
                None => AppEffect::None,
            },
            KeyCode::Char('c') => match self.current() {
                Some(e) => {
                    let text = e.text.clone();
                    self.selected = 0;
                    AppEffect::Copy(text)
                }
                None => AppEffect::None,
            },
            KeyCode::Char('p') => {
                let Some(id) = self.current().map(|e| e.id) else {
                    return AppEffect::None;
                };
                self.with_history(|h| {
                    h.toggle_pin(id);
                });
                AppEffect::Redraw
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let Some(id) = self.current().map(|e| e.id) else {
                    return AppEffect::None;
                };
                self.with_history(|h| {
                    h.remove(id);
                });
                AppEffect::Redraw
            }
            KeyCode::Char('D') => {
                self.mode = Mode::ConfirmClear;
                AppEffect::Redraw
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                AppEffect::Redraw
            }
            KeyCode::Char('q') | KeyCode::Esc => AppEffect::Close,
            _ => AppEffect::None,
        }
    }
}

impl App for ClipboardManager {
    fn title(&self) -> String {
        "Clipboard".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        match self.mode {
            Mode::Browse => self.browse_key(key),
            Mode::Filter => {
                match key.code {
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.mode = Mode::Browse;
                    }
                    KeyCode::Enter => self.mode = Mode::Browse,
                    KeyCode::Backspace => {
                        self.filter.pop();
                    }
                    KeyCode::Char(c) if key.mods.is_empty() => self.filter.push(c),
                    _ => return AppEffect::None,
                }
                self.refresh();
                AppEffect::Redraw
            }
            Mode::ConfirmClear => {
                self.mode = Mode::Browse;
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    self.with_history(|h| h.clear());
                }
                AppEffect::Redraw
            }
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> AppEffect {
        match event.kind {
            MouseKind::ScrollDown => self.select(self.selected + 3),
            MouseKind::ScrollUp => self.select(self.selected.saturating_sub(3)),
            MouseKind::Down(MouseButton::Left) => {
                let i = self.top + usize::from(event.y);
                if i >= self.shown.len() {
                    AppEffect::None
                } else if i == self.selected {
                    self.handle_key(KeyEvent::plain(KeyCode::Enter))
                } else {
                    self.select(i)
                }
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        self.refresh();
        if area.height < 3 || area.width < 10 {
            return;
        }
        let c = &theme.colors;
        let now = SystemTime::now();
        // The list on top, the selected entry below it.
        let list_h = if area.height >= 12 {
            area.height / 2
        } else {
            area.height - 1
        };
        self.rows = list_h;
        let rows = usize::from(list_h);
        if self.selected < self.top {
            self.top = self.selected;
        } else if rows > 0 && self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        if self.shown.is_empty() {
            let text = if self.filter.is_empty() {
                "Nothing copied yet. Copy in copy mode (prefix [) or by selecting with the mouse."
            } else {
                "(no match)"
            };
            buf.set_stringn(
                area.x + 1,
                area.y,
                fit(text, usize::from(area.width - 1)),
                usize::from(area.width - 1),
                tui_style(Style::new().fg(c.muted)),
            );
        }
        for (i, e) in self.shown.iter().enumerate().skip(self.top).take(rows) {
            let y = area.y + u16::try_from(i - self.top).unwrap_or(0);
            let selected = i == self.selected;
            let bg = |s: Style| if selected { s.bg(c.selection_bg) } else { s };
            if selected {
                buf.set_style(
                    TuiRect::new(area.x, y, area.width, 1),
                    tui_style(Style::new().bg(c.selection_bg)),
                );
            }
            let right = format!("{}  {}", e.source, age(e.at, now));
            let right_w = u16::try_from(UnicodeWidthStr::width(right.as_str()))
                .unwrap_or(0)
                .min(area.width / 2);
            let pin = if e.pinned { "◆ " } else { "  " };
            buf.set_string(area.x, y, pin, tui_style(bg(Style::new().fg(c.accent))));
            let text_w = usize::from(area.width.saturating_sub(right_w + 4));
            buf.set_stringn(
                area.x + 2,
                y,
                fit(&summary(&e.text), text_w),
                text_w,
                tui_style(bg(Style::new().fg(c.fg))),
            );
            let rx = area.right().saturating_sub(right_w + 1);
            buf.set_stringn(
                rx,
                y,
                fit(&right, usize::from(right_w)),
                usize::from(right_w),
                tui_style(bg(Style::new().fg(c.muted))),
            );
        }
        // The selected entry in full (as much as fits).
        let detail_y = area.y + list_h;
        let detail_h = area.height - list_h - 1;
        if detail_h > 0
            && let Some(e) = self.current()
        {
            buf.set_string(
                area.x,
                detail_y,
                "─".repeat(usize::from(area.width)),
                tui_style(Style::new().fg(c.border)),
            );
            let size = if e.truncated {
                format!(" {} bytes (cut to 1 MiB) ", e.text.len())
            } else {
                format!(" {} characters ", e.text.chars().count())
            };
            buf.set_string(
                area.x + 2,
                detail_y,
                &size,
                tui_style(Style::new().fg(c.muted)),
            );
            for (i, line) in e.text.lines().take(usize::from(detail_h - 1)).enumerate() {
                let y = detail_y + 1 + u16::try_from(i).unwrap_or(0);
                buf.set_stringn(
                    area.x + 1,
                    y,
                    line.replace('\t', "    "),
                    usize::from(area.width - 1),
                    tui_style(Style::new().fg(c.fg)),
                );
            }
        }
        let (line, style) = match &self.mode {
            Mode::Filter => (format!("/{}▏", self.filter), Style::new().fg(c.fg)),
            Mode::ConfirmClear => (
                "Forget every entry that is not pinned? y/n".to_owned(),
                Style::new().fg(c.warn).attrs(Attrs::BOLD),
            ),
            Mode::Browse => (
                "enter paste · c copy again · p pin · d delete · D clear · / filter · q close"
                    .to_owned(),
                Style::new().fg(c.muted),
            ),
        };
        let y = area.bottom() - 1;
        buf.set_stringn(
            area.x + 1,
            y,
            fit(&line, usize::from(area.width - 1)),
            usize::from(area.width - 1),
            tui_style(style),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_core::clipboard::{ClipSource, ClipboardHistory};

    fn manager() -> (ClipboardManager, SharedClipboard) {
        let h = ClipboardHistory::shared(10);
        if let Ok(mut g) = h.lock() {
            g.push("first copy", ClipSource::Selection);
            g.push(
                "line one\nline two\nline three",
                ClipSource::Program("vim".into()),
            );
            g.push("cargo build --release", ClipSource::App("Files".into()));
        }
        (ClipboardManager::new(std::sync::Arc::clone(&h)), h)
    }

    #[test]
    fn the_window() {
        let (mut m, _) = manager();
        insta::assert_snapshot!(
            "clipboard-window",
            crate::testing::render_text(&mut m, 60, 12)
        );
    }

    #[test]
    fn paste_copy_pin_delete_and_clear() {
        let (mut m, h) = manager();
        let key = |c: char| KeyEvent::plain(KeyCode::Char(c));
        assert_eq!(
            m.handle_key(KeyEvent::plain(KeyCode::Enter)),
            AppEffect::PasteAndClose("cargo build --release".into())
        );
        let _ = m.handle_key(key('j'));
        assert_eq!(
            m.handle_key(key('c')),
            AppEffect::Copy("line one\nline two\nline three".into())
        );
        // Pin the oldest, then clear: only the pin stays.
        let _ = m.handle_key(key('G'));
        let _ = m.handle_key(key('p'));
        let _ = m.handle_key(key('D'));
        let _ = m.handle_key(key('y'));
        let texts: Vec<String> = h
            .lock()
            .map(|g| g.entries().map(|e| e.text.clone()).collect())
            .unwrap_or_default();
        assert_eq!(texts, ["first copy"]);
        let _ = m.handle_key(key('d'));
        assert!(h.lock().is_ok_and(|g| g.is_empty()));
    }

    #[test]
    fn filtering() {
        let (mut m, _) = manager();
        let _ = m.handle_key(KeyEvent::plain(KeyCode::Char('/')));
        for c in "two".chars() {
            let _ = m.handle_key(KeyEvent::plain(KeyCode::Char(c)));
        }
        assert_eq!(m.shown.len(), 1);
        assert!(m.shown[0].text.starts_with("line one"));
        assert_eq!(age(SystemTime::UNIX_EPOCH, SystemTime::UNIX_EPOCH), "now");
        let later = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(7200);
        assert_eq!(age(SystemTime::UNIX_EPOCH, later), "2h");
    }
}
