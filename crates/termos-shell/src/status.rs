// SPDX-License-Identifier: GPL-3.0-or-later
//! The tmux-style status line: session, workspace tabs, minimized windows,
//! and on the right the prefix/copy-mode badges, messages and a clock.

use termos_core::ids::PaneId;
use termos_render::cell::Cell;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;

use crate::notify::Level;
use crate::ui::{truncate, width};

/// One workspace tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabInfo {
    pub index: usize,
    pub name: String,
    pub active: bool,
    /// The previously active workspace (tmux `-` flag).
    pub last: bool,
    /// A pane rang the bell while the workspace was in the background.
    pub bell: bool,
    pub zoomed: bool,
}

/// Everything the status line shows.
#[derive(Debug, Clone, Default)]
pub struct StatusInfo<'a> {
    pub session: &'a str,
    pub tabs: Vec<TabInfo>,
    pub minimized: Vec<(PaneId, String)>,
    pub prefix: bool,
    pub copy_mode: bool,
    pub message: Option<(&'a str, Level)>,
    pub clock: Option<String>,
    /// An open prompt: (label, input). Replaces the whole line, like tmux.
    pub prompt: Option<(&'a str, &'a str)>,
}

/// Clickable regions of the drawn status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusRegions {
    pub y: u16,
    /// `(x_start, x_end_exclusive, workspace index)`.
    pub tabs: Vec<(u16, u16, usize)>,
    pub minimized: Vec<(u16, u16, PaneId)>,
}

impl StatusRegions {
    pub fn tab_at(&self, x: u16, y: u16) -> Option<usize> {
        (y == self.y)
            .then(|| {
                self.tabs
                    .iter()
                    .find(|(a, b, _)| x >= *a && x < *b)
                    .map(|t| t.2)
            })
            .flatten()
    }

    pub fn minimized_at(&self, x: u16, y: u16) -> Option<PaneId> {
        (y == self.y)
            .then(|| {
                self.minimized
                    .iter()
                    .find(|(a, b, _)| x >= *a && x < *b)
                    .map(|t| t.2)
            })
            .flatten()
    }
}

/// Draws the status line on row `y`. Returns the clickable regions and,
/// when a prompt is open, the cursor column.
pub fn draw(
    grid: &mut Grid,
    y: u16,
    theme: &Theme,
    info: &StatusInfo<'_>,
) -> (StatusRegions, Option<u16>) {
    let cols = grid.cols();
    let row = Rect::new(0, y, cols, 1);
    grid.fill(row, &Cell::blank(theme.status));
    let mut regions = StatusRegions {
        y,
        ..StatusRegions::default()
    };
    if let Some((label, input)) = info.prompt {
        let x = grid.put_str(0, y, label, theme.status_message, cols);
        let x = grid.put_str(x, y, input, theme.status, cols);
        return (regions, Some(x.min(cols.saturating_sub(1))));
    }

    // Right side first, so the left side can be truncated to fit.
    let mut right: Vec<(String, termos_render::cell::Style)> = Vec::new();
    if info.copy_mode {
        right.push((" COPY ".into(), theme.copy_mode_badge));
    }
    if info.prefix {
        right.push((" PREFIX ".into(), theme.status_prefix));
    }
    if let Some((msg, level)) = info.message {
        let style = match level {
            Level::Info => theme.status_message,
            Level::Warn | Level::Error => theme.status_prefix,
        };
        right.push((format!(" {} ", truncate(msg, cols / 2)), style));
    }
    if let Some(clock) = &info.clock {
        right.push((format!(" {clock} "), theme.status));
    }
    let right_w: u16 = right.iter().map(|(s, _)| width(s)).sum();
    let left_limit = cols.saturating_sub(right_w);

    let mut x = grid.put_str(
        0,
        y,
        &format!("[{}] ", info.session),
        theme.status_message,
        left_limit,
    );
    for tab in &info.tabs {
        let mut flag = String::new();
        if tab.active {
            flag.push('*');
        } else if tab.last {
            flag.push('-');
        }
        if tab.bell {
            flag.push('!');
        }
        if tab.zoomed {
            flag.push('Z');
        }
        let text = format!(" {}:{}{} ", tab.index, truncate(&tab.name, 16), flag);
        let style = if tab.active {
            theme.status_tab_active
        } else {
            theme.status_tab
        };
        let start = x;
        x = grid.put_str(x, y, &text, style, left_limit);
        regions.tabs.push((start, x, tab.index));
        if x >= left_limit {
            break;
        }
    }
    for (pane, name) in &info.minimized {
        if x >= left_limit {
            break;
        }
        let start = x + 1;
        x = grid.put_str(
            start,
            y,
            &format!("▾{}", truncate(name, 12)),
            theme.status_message,
            left_limit,
        );
        regions.minimized.push((start, x, *pane));
    }

    let mut rx = cols.saturating_sub(right_w);
    for (text, style) in &right {
        rx = grid.put_str(rx, y, text, *style, cols);
    }
    (regions, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_badges_and_regions() {
        let mut g = Grid::new(60, 1);
        let info = StatusInfo {
            session: "main",
            tabs: vec![
                TabInfo {
                    index: 0,
                    name: "bash".into(),
                    active: false,
                    last: true,
                    bell: true,
                    zoomed: false,
                },
                TabInfo {
                    index: 1,
                    name: "nvim".into(),
                    active: true,
                    last: false,
                    bell: false,
                    zoomed: true,
                },
            ],
            minimized: vec![(PaneId(4), "htop".into())],
            prefix: true,
            clock: Some("12:34".into()),
            ..StatusInfo::default()
        };
        let (regions, cursor) = draw(&mut g, 0, &Theme::default(), &info);
        assert_eq!(cursor, None);
        let text = g.row_text(0);
        assert!(
            text.starts_with("[main]  0:bash-!  1:nvim*Z  ▾htop"),
            "{text:?}"
        );
        assert!(text.trim_end().ends_with("PREFIX  12:34"), "{text:?}");
        assert_eq!(regions.tab_at(9, 0), Some(0));
        assert_eq!(regions.tab_at(20, 0), Some(1));
        assert_eq!(regions.minimized_at(30, 0), Some(PaneId(4)));
        assert_eq!(regions.tab_at(9, 1), None);
    }

    #[test]
    fn prompt_replaces_the_line() {
        let mut g = Grid::new(40, 1);
        let info = StatusInfo {
            session: "main",
            prompt: Some(("(rename-workspace) ", "web")),
            ..StatusInfo::default()
        };
        let (_, cursor) = draw(&mut g, 0, &Theme::default(), &info);
        assert_eq!(g.row_text(0).trim_end(), "(rename-workspace) web");
        assert_eq!(cursor, Some(22));
    }
}
