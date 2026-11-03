// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in shell apps: the first-run tutorial, the notification center
//! and About TermOS. All are in-process [`App`]s shown in floating windows.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::text::{Line, Span, Text};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use termos_core::app::{App, AppEffect, tui_style};
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::theme::Theme;

use crate::keys::is_cancel;
use crate::notify::{Level, SharedHistory};

/// The paged first-run tutorial.
#[derive(Debug, Clone)]
pub struct Tutorial {
    pages: Vec<(String, String)>,
    page: usize,
}

impl Tutorial {
    /// Builds the tutorial for the configured prefix key (e.g. `C-b`).
    pub fn new(prefix: &str) -> Self {
        let p = prefix;
        let pages = vec![
            (
                "Welcome to TermOS".to_owned(),
                format!(
                    "TermOS is a desktop that lives in your terminal and works like tmux.\n\n\
                     Almost everything starts with the prefix key {p}: press it, let go, then press a \
                     command key. If you pause after {p}, a panel lists every key.\n\n\
                     Ctrl+Space opens the command palette: type a few letters of anything you want to do.\n\n\
                     Use → / n / Space for the next page, ← / p for the previous one, q to close."
                ),
            ),
            (
                "Panes".to_owned(),
                format!(
                    "{p} %   split: new pane to the right\n\
                     {p} \"   split: new pane below\n\
                     {p} ←↑↓→  move between panes (or click one)\n\
                     {p} z   zoom the pane to full screen, again to restore\n\
                     {p} x   close the pane\n\
                     {p} Space  cycle layouts: even, main-vertical, tiled…\n\n\
                     Drag the lines between panes with the mouse to resize, or hold {p} and press Ctrl+arrows."
                ),
            ),
            (
                "Floating windows".to_owned(),
                format!(
                    "{p} f   float the pane into a movable window (again to tile it)\n\
                     {p} F   open a new floating shell\n\
                     {p} m / M   minimize / restore a floating window\n\n\
                     Drag the title bar to move a window; drop it on a screen edge to snap it. Drag the \
                     edges to resize. The – and × buttons minimize and close."
                ),
            ),
            (
                "Workspaces".to_owned(),
                format!(
                    "Workspaces are separate desktops, shown as tabs in the status bar.\n\n\
                     {p} c   new workspace\n\
                     {p} n / p   next / previous\n\
                     {p} 0-9   jump to a workspace (or click its tab)\n\
                     {p} ,   rename it\n\
                     {p} &   close it"
                ),
            ),
            (
                "Copy mode".to_owned(),
                format!(
                    "{p} [ or scroll up with the mouse wheel to look back through output.\n\n\
                     Move with the vi keys (h j k l, w b, g G, Ctrl+U/D). v starts a selection, y copies it \
                     (also to your system clipboard), / searches, q leaves.\n\n\
                     You can also drag with the mouse to select and copy. {p} ] pastes; {p} = lists \
                     everything copied, to paste again."
                ),
            ),
            (
                "Make it yours".to_owned(),
                format!(
                    "Settings live in ~/.config/termos/config.toml. Run `termos config default` to see \
                     every option; changes apply as soon as you save.\n\n\
                     Reopen this tutorial any time with {p} F1, or from the palette (Ctrl+Space, \"tutorial\").\n\n\
                     Press q to start."
                ),
            ),
        ];
        Self { pages, page: 0 }
    }

    pub fn page(&self) -> usize {
        self.page
    }
}

impl App for Tutorial {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Tutorial
    }

    fn title(&self) -> String {
        format!("Tutorial {}/{}", self.page + 1, self.pages.len())
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        let last = self.pages.len().saturating_sub(1);
        match key.code {
            _ if is_cancel(key) => AppEffect::Close,
            KeyCode::Char('q') => AppEffect::Close,
            KeyCode::Right | KeyCode::Char('n' | ' ' | 'l') | KeyCode::Enter => {
                if self.page == last {
                    AppEffect::Close
                } else {
                    self.page += 1;
                    AppEffect::Redraw
                }
            }
            KeyCode::Left | KeyCode::Char('p' | 'h') | KeyCode::Backspace => {
                self.page = self.page.saturating_sub(1);
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let (title, body) = &self.pages[self.page];
        let mut lines = vec![
            Line::from(Span::styled(title.clone(), tui_style(theme.float_title))),
            Line::from(""),
        ];
        lines.extend(body.lines().map(|l| Line::from(l.to_owned())));
        let footer = format!(
            "page {}/{}   → next   ← back   q close",
            self.page + 1,
            self.pages.len()
        );
        let body_area = TuiRect::new(
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            area.height.saturating_sub(1),
        );
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(body_area, buf);
        if area.height > 0 {
            let footer_area = TuiRect::new(
                area.x + 1,
                area.bottom() - 1,
                area.width.saturating_sub(2),
                1,
            );
            Line::from(Span::styled(footer, tui_style(theme.border))).render(footer_area, buf);
        }
    }
}

/// About TermOS (D-089): the logo, the version, where TermOS keeps its
/// files, and the licence.
#[derive(Debug, Clone)]
pub struct About {
    rows: Vec<(&'static str, String)>,
}

impl About {
    /// `config` is the config file; `paths` where the rest lives.
    pub fn new(config: &std::path::Path, paths: &termos_core::paths::Paths) -> Self {
        let rows = vec![
            ("Version", env!("CARGO_PKG_VERSION").to_owned()),
            (
                "System",
                format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            ),
            ("Config", config.display().to_string()),
            ("Logs", paths.log_dir().display().to_string()),
            ("Data", paths.data_dir.display().to_string()),
            ("Licence", "GPL-3.0-or-later".to_owned()),
            ("Source", env!("CARGO_PKG_REPOSITORY").to_owned()),
        ];
        Self { rows }
    }
}

impl App for About {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::About
    }

    fn title(&self) -> String {
        "About TermOS".to_owned()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        match key.code {
            _ if is_cancel(key) => AppEffect::Close,
            KeyCode::Char('q') | KeyCode::Enter => AppEffect::Close,
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let c = &theme.colors;
        let mut y = area.y + 1;
        let lw = crate::wallpaper::LOGO_W;
        if area.width >= lw + 2 && area.height >= 12 {
            // The logo goes through the grid drawing; copy it in cell by
            // cell.
            let mut g = termos_render::grid::Grid::new(lw, 3);
            g.clear(termos_render::cell::Style::new().bg(c.overlay_bg));
            crate::wallpaper::logo(&mut g, 0, 0, theme, 1.0);
            let x0 = area.x + (area.width - lw) / 2;
            for dy in 0..3u16 {
                for dx in 0..lw {
                    if let (Some(src), Some(dst)) = (g.get(dx, dy), buf.cell_mut((x0 + dx, y + dy)))
                    {
                        dst.set_symbol(&src.symbol);
                        dst.set_style(tui_style(src.style));
                    }
                }
            }
            y += 4;
            let tagline = "a terminal-first desktop";
            let tx = area.x + area.width.saturating_sub(24) / 2;
            buf.set_string(
                tx,
                y,
                tagline,
                tui_style(termos_render::cell::Style::new().fg(c.muted)),
            );
            y += 2;
        }
        let key_w = self.rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        let key_style = tui_style(termos_render::cell::Style::new().fg(c.muted));
        let value_style = tui_style(termos_render::cell::Style::new().fg(c.fg));
        for (k, v) in &self.rows {
            if y >= area.bottom().saturating_sub(1) {
                break;
            }
            let x = area.x + 2;
            buf.set_string(x, y, format!("{k:>key_w$}"), key_style);
            let vx = x + u16::try_from(key_w).unwrap_or(0) + 2;
            let room = usize::from(area.right().saturating_sub(vx + 1));
            buf.set_stringn(vx, y, tail(v, room), room, value_style);
            y += 1;
        }
        if area.height > 0 {
            let footer = TuiRect::new(
                area.x + 1,
                area.bottom() - 1,
                area.width.saturating_sub(2),
                1,
            );
            Line::from(Span::styled("q close", tui_style(theme.border))).render(footer, buf);
        }
    }
}

/// `text` in `room` columns: whole, or `…` and its end (a long path keeps
/// its file name).
fn tail(text: &str, room: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if text.width() <= room {
        return text.to_owned();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        kept.push(ch);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// The notification center: recent notifications, newest first.
#[derive(Debug, Clone)]
pub struct NotificationCenter {
    history: SharedHistory,
    scroll: u16,
}

impl NotificationCenter {
    pub fn new(history: SharedHistory) -> Self {
        Self { history, scroll: 0 }
    }
}

impl App for NotificationCenter {
    fn icon(&self) -> termos_render::icons::Icon {
        termos_render::icons::Icon::Notifications
    }

    fn title(&self) -> String {
        "Notifications".into()
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppEffect {
        match key.code {
            _ if is_cancel(key) => AppEffect::Close,
            KeyCode::Char('q') => AppEffect::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
                AppEffect::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                AppEffect::Redraw
            }
            _ => AppEffect::None,
        }
    }

    fn render(&mut self, area: TuiRect, buf: &mut Buffer, theme: &Theme) {
        let notices = self
            .history
            .lock()
            .map(|h| h.iter().rev().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let lines: Vec<Line<'_>> = if notices.is_empty() {
            vec![Line::from(Span::styled(
                "No notifications yet.",
                tui_style(theme.border),
            ))]
        } else {
            notices
                .into_iter()
                .map(|n| {
                    let mark = match n.level {
                        Level::Info => Span::styled("•", tui_style(theme.float_title)),
                        Level::Warn => Span::styled("!", tui_style(theme.status_prefix)),
                        Level::Error => Span::styled("✗", tui_style(theme.status_prefix)),
                    };
                    Line::from(vec![
                        Span::styled(format!("{} ", n.time), tui_style(theme.border)),
                        mark,
                        Span::raw(" "),
                        Span::raw(n.text),
                    ])
                })
                .collect()
        };
        let inner = TuiRect::new(
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            area.height,
        );
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .scroll((self.scroll, 0))
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_shows_the_version_and_where_files_are() {
        let paths = termos_core::paths::Paths {
            config_dir: "/home/u/.config/termos".into(),
            state_dir: "/home/u/.local/state/termos".into(),
            runtime_dir: "/run/user/1000/termos".into(),
            data_dir: "/home/u/.local/share/termos".into(),
        };
        let mut app = About::new(
            std::path::Path::new("/home/u/.config/termos/config.toml"),
            &paths,
        );
        let area = TuiRect::new(0, 0, 60, 18);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &Theme::default());
        let text: String = (0..area.height)
            .map(|y| {
                let row: String = (0..area.width)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect();
                row.trim_end().to_owned() + "\n"
            })
            .collect();
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("config.toml") && text.contains("GPL-3.0-or-later"));
        assert!(text.contains('█'), "the logo");
        assert_eq!(tail("/a/very/long/path/config.toml", 13), "…/config.toml");
        assert_eq!(tail("short", 14), "short");
        assert_eq!(app.handle_key(KeyEvent::ch('q')), AppEffect::Close);
    }
    use std::time::Instant;
    use termos_core::app::render_app;
    use termos_render::grid::{Grid, Rect};

    use crate::notify::Notifications;

    #[test]
    fn tutorial_pages_and_close() {
        let mut t = Tutorial::new("C-b");
        assert_eq!(t.title(), "Tutorial 1/6");
        assert_eq!(
            t.handle_key(KeyEvent::plain(KeyCode::Right)),
            AppEffect::Redraw
        );
        assert_eq!(t.page(), 1);
        assert_eq!(
            t.handle_key(KeyEvent::plain(KeyCode::Left)),
            AppEffect::Redraw
        );
        for _ in 0..5 {
            t.handle_key(KeyEvent::ch('n'));
        }
        assert_eq!(t.handle_key(KeyEvent::ch('n')), AppEffect::Close);
        let mut g = Grid::new(60, 16);
        let mut t = Tutorial::new("C-a");
        render_app(&mut t, &Theme::default(), &mut g, Rect::new(0, 0, 60, 16));
        let text = g.to_text();
        assert!(text.contains("Welcome to TermOS"));
        assert!(text.contains("C-a"));
        assert!(text.contains("page 1/6"));
    }

    #[test]
    fn notification_center_lists_newest_first() {
        let mut n = Notifications::new();
        let mut app = NotificationCenter::new(n.history());
        let mut g = Grid::new(50, 5);
        render_app(&mut app, &Theme::default(), &mut g, Rect::new(0, 0, 50, 5));
        assert!(g.to_text().contains("No notifications yet."));
        n.push(Level::Info, "first", Instant::now());
        n.push(Level::Error, "second", Instant::now());
        let mut g = Grid::new(50, 5);
        render_app(&mut app, &Theme::default(), &mut g, Rect::new(0, 0, 50, 5));
        let text = g.to_text();
        let (a, b) = (text.find("second"), text.find("first"));
        assert!(a.is_some() && b.is_some() && a < b, "{text}");
        assert_eq!(app.handle_key(KeyEvent::ch('q')), AppEffect::Close);
    }
}
