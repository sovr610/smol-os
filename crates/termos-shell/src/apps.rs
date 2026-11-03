// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in shell apps: the first-run tutorial and the notification center.
//! Both are in-process [`App`]s shown in floating windows.

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
