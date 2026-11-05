// SPDX-License-Identifier: GPL-3.0-or-later
//! The Gallery window (M13g, D-126): the signed index's themes, layouts
//! and widgets, to browse and install. The index loads and each install
//! runs on a thread; nothing runs while the window waits.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use termos_core::app::{App, AppEffect, AppWaker, tui_style};
use termos_core::input::{KeyCode, KeyEvent, Mods};
use termos_render::cell::{Attrs, Style};
use termos_render::theme::Theme;

use crate::gallery::{self, Entry, Index, Installed, Kind, Places};

/// What a background job finished.
enum Done {
    Index(Result<Index, String>),
    Installed(Result<String, String>, Option<Kind>),
}

pub struct GalleryWindow {
    url: String,
    places: Places,
    index: Option<Result<Index, String>>,
    installed: BTreeMap<String, Installed>,
    query: String,
    selected: usize,
    message: Option<(String, bool)>,
    /// Asked: remove this entry?
    confirm_remove: Option<String>,
    job: Option<Receiver<Done>>,
    waker: Option<AppWaker>,
    /// The config changed: the desktop must reload it.
    reload: bool,
}

impl std::fmt::Debug for GalleryWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GalleryWindow")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl GalleryWindow {
    pub fn open(url: String, places: Places) -> Self {
        let installed = gallery::installed(&places);
        let mut w = Self {
            url,
            places,
            index: None,
            installed,
            query: String::new(),
            selected: 0,
            message: None,
            confirm_remove: None,
            job: None,
            waker: None,
            reload: false,
        };
        w.load();
        w
    }

    fn spawn(&mut self, f: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = channel();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("termos-gallery".into())
            .spawn(move || {
                let _ = tx.send(f());
                if let Some(w) = waker {
                    w.wake();
                }
            });
        match spawned {
            Ok(_) => self.job = Some(rx),
            Err(e) => self.message = Some((format!("cannot start: {e}"), true)),
        }
    }

    fn load(&mut self) {
        let url = self.url.clone();
        self.spawn(move || Done::Index(gallery::load(&url).map_err(|e| e.to_string())));
    }

    /// The entries the search shows.
    fn shown(&self) -> Vec<&Entry> {
        match &self.index {
            Some(Ok(i)) => i
                .entries
                .iter()
                .filter(|e| e.matches(&self.query))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn selected_entry(&self) -> Option<Entry> {
        self.shown().get(self.selected).map(|e| (*e).clone())
    }

    fn install(&mut self, e: Entry) {
        let places = self.places.clone();
        self.message = Some((format!("installing {}…", e.name), false));
        let kind = e.kind();
        self.spawn(move || {
            let r = gallery::fetch(&e.url, gallery::MAX_FILE)
                .and_then(|b| gallery::install(&e, &b, &places))
                .map(|_| format!("installed {} {}", e.kind, e.name))
                .map_err(|err| err.to_string());
            Done::Installed(r, kind)
        });
    }
}

impl App for GalleryWindow {
    fn title(&self) -> String {
        "Gallery".into()
    }

    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Apps
    }

    fn reopen(&self) -> Option<String> {
        Some("gallery.open".into())
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn set_waker(&mut self, waker: AppWaker) {
        self.waker = Some(waker);
    }

    fn catch_up(&mut self) {
        let Some(rx) = &self.job else { return };
        let Ok(done) = rx.try_recv() else { return };
        self.job = None;
        match done {
            Done::Index(r) => {
                self.index = Some(r);
                self.selected = 0;
            }
            Done::Installed(Ok(text), kind) => {
                self.message = Some((text, false));
                self.installed = gallery::installed(&self.places);
                if kind == Some(Kind::Widget) {
                    self.reload = true;
                }
            }
            Done::Installed(Err(e), _) => self.message = Some((e, true)),
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.reload.then(Instant::now)
    }

    fn on_timer(&mut self, _now: Instant) -> AppEffect {
        if std::mem::take(&mut self.reload) {
            return AppEffect::Command("config.reload".into());
        }
        AppEffect::None
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        if let Some(name) = self.confirm_remove.take() {
            if key.code == KeyCode::Char('y') {
                match gallery::remove(&name, &self.places) {
                    Ok(_) => {
                        self.message = Some((format!("removed {name}"), false));
                        self.installed = gallery::installed(&self.places);
                        return AppEffect::Command("config.reload".into());
                    }
                    Err(e) => self.message = Some((e.to_string(), true)),
                }
            }
            return AppEffect::Redraw;
        }
        let plain = key.mods.is_empty() || key.mods == Mods::SHIFT;
        match key.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.selected = 0;
            }
            KeyCode::Esc => return AppEffect::Close,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.shown().len().saturating_sub(1));
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Enter => {
                if self.job.is_some() {
                    return AppEffect::None;
                }
                match self.selected_entry() {
                    Some(e) => self.install(e),
                    None => return AppEffect::None,
                }
            }
            KeyCode::Char('u') if key.mods == Mods::CTRL => {
                let Some(e) = self.selected_entry() else {
                    return AppEffect::None;
                };
                return match gallery::use_theme(&e.name, &self.places) {
                    Ok(()) => {
                        self.message = Some((format!("theme {} in use", e.name), false));
                        AppEffect::Command("config.reload".into())
                    }
                    Err(err) => {
                        self.message = Some((err.to_string(), true));
                        AppEffect::Redraw
                    }
                };
            }
            KeyCode::Char('d') if key.mods == Mods::CTRL => {
                if let Some(e) = self.selected_entry()
                    && self.installed.contains_key(&e.name)
                {
                    self.message = Some((format!("remove {}? (y)", e.name), false));
                    self.confirm_remove = Some(e.name);
                }
            }
            KeyCode::Char('r') if key.mods == Mods::CTRL => self.load(),
            KeyCode::Char(c) if plain => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => return AppEffect::None,
        }
        AppEffect::Redraw
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        if area.height < 4 || area.width < 20 {
            return;
        }
        let c = &theme.colors;
        let w = usize::from(area.width.saturating_sub(2));
        buf.set_stringn(
            area.x + 1,
            area.y,
            format!("Search: {}▏", self.query),
            w,
            tui_style(Style::new().fg(c.fg).attrs(Attrs::BOLD)),
        );
        let body = area.height.saturating_sub(3);
        match &self.index {
            None => {
                buf.set_stringn(
                    area.x + 1,
                    area.y + 2,
                    "Loading the gallery's index…",
                    w,
                    tui_style(Style::new().fg(c.muted)),
                );
            }
            Some(Err(e)) => {
                buf.set_stringn(
                    area.x + 1,
                    area.y + 2,
                    e,
                    w,
                    tui_style(Style::new().fg(c.error)),
                );
            }
            Some(Ok(_)) => {
                let shown = self.shown();
                if shown.is_empty() {
                    buf.set_stringn(
                        area.x + 1,
                        area.y + 2,
                        "Nothing matches.",
                        w,
                        tui_style(Style::new().fg(c.muted)),
                    );
                }
                let first = self
                    .selected
                    .saturating_sub(usize::from(body).saturating_sub(1));
                for (i, e) in shown.iter().enumerate().skip(first).take(usize::from(body)) {
                    let Ok(y) = u16::try_from(i - first) else {
                        break;
                    };
                    let y = area.y + 1 + y;
                    let mark = match self.installed.get(&e.name) {
                        Some(inst) if inst.version == e.version => "✓",
                        Some(_) => "↑",
                        None => " ",
                    };
                    let text = format!(
                        "{mark} {:<18} {:<7} {:<8} {}",
                        e.name, e.kind, e.version, e.summary
                    );
                    let selected = i == self.selected;
                    let style = if selected {
                        c.selected(Style::new().bg(c.selection_bg))
                    } else {
                        Style::new().fg(c.fg)
                    };
                    if selected {
                        buf.set_style(TuiRect::new(area.x, y, area.width, 1), tui_style(style));
                    }
                    buf.set_stringn(area.x + 1, y, text, w, tui_style(style));
                }
            }
        }
        let (line, style) = match &self.message {
            Some((m, true)) => (m.clone(), Style::new().fg(c.error)),
            Some((m, false)) => (m.clone(), Style::new().fg(c.muted)),
            None => (
                "type to search · Enter install · Ctrl+U use a theme · Ctrl+D remove · Ctrl+R reload · Esc close"
                    .to_owned(),
                Style::new().fg(c.muted),
            ),
        };
        buf.set_stringn(area.x + 1, area.bottom() - 1, line, w, tui_style(style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gallery::tests::{THEME, fixture_keys, signed_index};

    #[test]
    fn the_window_lists_searches_and_marks_installed() {
        let dir = tempfile::tempdir().expect("dir");
        let places = Places {
            config_file: dir.path().join("config.toml"),
            layouts_dir: dir.path().join("layouts"),
            state_dir: dir.path().join("state"),
        };
        let index = gallery::verify(&signed_index(), &fixture_keys()).expect("verifies");
        let nord = index.get("nord").expect("nord").clone();
        gallery::install(&nord, THEME.as_bytes(), &places).expect("installed");
        let mut w = GalleryWindow::open("/nonexistent/index.json".into(), places);
        // The load fails (no such file); a checked index takes its place.
        while w.job.is_some() {
            w.catch_up();
        }
        assert!(matches!(w.index, Some(Err(_))));
        w.index = Some(Ok(index));
        w.installed = gallery::installed(&w.places);
        let text = crate::testing::render_text(&mut w, 100, 8);
        assert!(text.contains("✓ nord"), "{text}");
        assert!(text.contains("clock") && text.contains("widget"), "{text}");
        for c in "lay".chars() {
            w.handle_key(KeyEvent::ch(c));
        }
        let text = crate::testing::render_text(&mut w, 100, 8);
        assert!(text.contains("dev") && !text.contains("nord"), "{text}");
        assert_eq!(w.selected_entry().map(|e| e.name), Some("dev".into()));
        assert_eq!(
            w.handle_key(KeyEvent::plain(KeyCode::Esc)),
            AppEffect::Redraw
        );
        assert_eq!(
            w.handle_key(KeyEvent::plain(KeyCode::Esc)),
            AppEffect::Close
        );
    }
}
