// SPDX-License-Identifier: GPL-3.0-or-later
//! The tmux-style status line: the start button, session, workspace tabs
//! and a `+` for a new one, minimized windows, and on the right the
//! prefix/copy-mode badges, messages and a clock. What is under the
//! pointer is highlighted (D-085).

use termos_core::ids::PaneId;
use termos_render::cell::Cell;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;

use termos_render::icons::Icon;

use crate::notify::Level;
use crate::ui::{cut, truncate, width};

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
    /// An agent there needs the user (`You`), or finished a turn not
    /// seen yet (`Done`) (D-115).
    pub attention: Option<crate::attention::Need>,
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
    /// How much of the message is shown while it slides in (0 to 1;
    /// `None` is all of it).
    pub message_reveal: Option<f32>,
    pub clock: Option<String>,
    /// The battery (`▮ 84%`, `⚡ 62%`), and whether it is low (D-124).
    pub battery: Option<(String, bool)>,
    /// The volume (`◁ 45%`, `◁ muted`) on an image with sound (D-128).
    pub volume: Option<String>,
    /// Plugin widgets (styled runs; default colours mean the status line's).
    pub widgets: &'a [Vec<(String, termos_render::cell::Style)>],
    /// An open prompt: (label, input). Replaces the whole line, like tmux.
    pub prompt: Option<(&'a str, &'a str)>,
    /// Draw the start button (the TermOS menu) at the left.
    pub start_button: bool,
    /// The pointer's column when it is on the status line.
    pub pointer: Option<u16>,
    /// Keys go to several panes at once (D-120).
    pub sync: bool,
}

/// Clickable regions of the drawn status line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusRegions {
    pub y: u16,
    /// `(x_start, x_end_exclusive, workspace index)`.
    pub tabs: Vec<(u16, u16, usize)>,
    pub minimized: Vec<(u16, u16, PaneId)>,
    /// `(x_start, x_end_exclusive, widget index)`: where each widget is.
    pub widgets: Vec<(u16, u16, usize)>,
    /// The start button.
    pub start: Option<(u16, u16)>,
    /// The `+` after the tabs (a new workspace).
    pub new_tab: Option<(u16, u16)>,
    /// The message shown at the right.
    pub message: Option<(u16, u16)>,
}

fn within(span: Option<(u16, u16)>, x: u16) -> bool {
    span.is_some_and(|(a, b)| x >= a && x < b)
}

impl StatusRegions {
    pub fn start_at(&self, x: u16, y: u16) -> bool {
        y == self.y && within(self.start, x)
    }

    pub fn new_tab_at(&self, x: u16, y: u16) -> bool {
        y == self.y && within(self.new_tab, x)
    }

    pub fn message_at(&self, x: u16, y: u16) -> bool {
        y == self.y && within(self.message, x)
    }

    /// Something clickable at (`x`, `y`), as an id that changes when the
    /// pointer moves from one thing to another (for hover highlights).
    pub fn target_at(&self, x: u16, y: u16) -> Option<u32> {
        if y != self.y {
            return None;
        }
        if within(self.start, x) {
            return Some(1);
        }
        if within(self.new_tab, x) {
            return Some(2);
        }
        if within(self.message, x) {
            return Some(3);
        }
        if let Some(i) = self.tab_at(x, y) {
            return Some(100 + u32::try_from(i).unwrap_or(u32::MAX - 100));
        }
        self.minimized_at(x, y)
            .map(|p| 1_000 + u32::try_from(p.0 % 1_000_000).unwrap_or(0))
    }

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

    /// The widget at (`x`, `y`) and the column within it.
    pub fn widget_at(&self, x: u16, y: u16) -> Option<(usize, u16)> {
        (y == self.y)
            .then(|| {
                self.widgets
                    .iter()
                    .find(|(a, b, _)| x >= *a && x < *b)
                    .map(|(a, _, i)| (*i, x - a))
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

/// A status-line segment on `color` with a sheen (truecolor only; flat
/// otherwise, D-088). Its text is in the status line's background colour.
fn fade(grid: &mut Grid, theme: &Theme, rect: Rect, color: termos_render::color::Color) {
    if theme.gradients {
        let (from, to) = theme.sheen(color, theme.colors.status_bg);
        theme.paint(grid, rect, from, to);
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
    let mut message_index = None;
    if info.copy_mode {
        right.push((" COPY ".into(), theme.copy_mode_badge));
    }
    if info.prefix {
        right.push((" PREFIX ".into(), theme.status_prefix));
    }
    if info.sync {
        right.push((
            " SYNC ".into(),
            termos_render::cell::Style::new()
                .fg(theme.colors.status_bg)
                .bg(theme.colors.accent),
        ));
    }
    if let Some((msg, level)) = info.message {
        let style = match level {
            Level::Info => theme.status_message,
            Level::Warn | Level::Error => theme.status_prefix,
        };
        let text = format!(" {} ", truncate(msg, cols / 2));
        let text = match info.message_reveal {
            Some(f) if f < 1.0 => {
                // In range: a fraction of a u16.
                let keep = (f32::from(width(&text)) * f.clamp(0.0, 1.0)).round() as u16;
                cut(&text, keep)
            }
            _ => text,
        };
        message_index = Some(right.len());
        right.push((text, style));
    }
    // Which entries of `right` each widget is (for clicks).
    let mut widget_spans: Vec<(usize, usize)> = Vec::new();
    for widget in info.widgets {
        right.push((" ".into(), theme.status));
        let first = right.len();
        for (text, style) in widget {
            let mut st = *style;
            if st.fg == termos_render::color::Color::Default {
                st.fg = theme.status.fg;
            }
            if st.bg == termos_render::color::Color::Default {
                st.bg = theme.status.bg;
            }
            right.push((text.clone(), st));
        }
        widget_spans.push((first, right.len()));
        right.push((" ".into(), theme.status));
    }
    if let Some(volume) = &info.volume {
        right.push((format!(" {volume} "), theme.status));
    }
    if let Some((battery, low)) = &info.battery {
        let style = if *low {
            theme.status_prefix
        } else {
            theme.status
        };
        right.push((format!(" {battery} "), style));
    }
    if let Some(clock) = &info.clock {
        right.push((format!(" {clock} "), theme.status));
    }
    let right_w: u16 = right.iter().map(|(s, _)| width(s)).sum();
    let left_limit = cols.saturating_sub(right_w);

    let hovered = |a: u16, b: u16| info.pointer.is_some_and(|p| p >= a && p < b);
    let mut x = 0;
    if info.start_button {
        let glyph = Icon::Menu.glyph(theme.icons);
        let label = match (glyph.is_empty(), cols >= 80) {
            (false, true) => format!(" {glyph} Menu "),
            (false, false) => format!(" {glyph} "),
            (true, _) => " Menu ".to_owned(),
        };
        let end = width(&label).min(left_limit);
        let hover = hovered(0, end);
        let style = if hover {
            theme.status_tab_active
        } else {
            theme.status_prefix
        };
        x = grid.put_str(0, y, &label, style, left_limit);
        regions.start = Some((0, x));
        if !hover {
            fade(grid, theme, Rect::new(0, y, x, 1), theme.colors.warn);
        }
        x = grid.put_str(x, y, " ", theme.status, left_limit);
    }
    let mut x = grid.put_str(
        x,
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
        // An agent waiting: a dot; a finished turn: a tick (D-115).
        match tab.attention {
            Some(crate::attention::Need::You) => flag.push('\u{25cf}'),
            Some(crate::attention::Need::Done) => flag.push('\u{2713}'),
            Some(crate::attention::Need::Failed) => flag.push('\u{2717}'),
            None => {}
        }
        let text = format!(" {}:{}{} ", tab.index, truncate(&tab.name, 16), flag);
        let mut style = if tab.active || hovered(x, x + width(&text)) {
            theme.status_tab_active
        } else {
            theme.status_tab
        };
        if tab.attention == Some(crate::attention::Need::You) && !tab.active {
            style = style.fg(theme.colors.warn);
        }
        if tab.attention == Some(crate::attention::Need::Failed) && !tab.active {
            style = style.fg(theme.colors.error);
        }
        let start = x;
        x = grid.put_str(x, y, &text, style, left_limit);
        if tab.active {
            fade(
                grid,
                theme,
                Rect::new(start, y, x - start, 1),
                theme.colors.accent,
            );
        }
        regions.tabs.push((start, x, tab.index));
        if x >= left_limit {
            break;
        }
    }
    if info.start_button && x + 3 <= left_limit {
        let style = if hovered(x, x + 3) {
            theme.status_tab_active
        } else {
            theme.status_tab
        };
        let start = x;
        x = grid.put_str(x, y, " + ", style, left_limit);
        regions.new_tab = Some((start, x));
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
    let mut starts = Vec::with_capacity(right.len() + 1);
    for (text, style) in &right {
        starts.push(rx);
        rx = grid.put_str(rx, y, text, *style, cols);
    }
    starts.push(rx);
    if let Some(i) = message_index {
        regions.message = Some((starts[i], starts[i + 1]));
    }
    for (i, (first, end)) in widget_spans.into_iter().enumerate() {
        if first < end {
            regions.widgets.push((starts[first], starts[end], i));
        }
    }
    (regions, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_render::icons::IconSet;

    #[test]
    fn the_start_button_the_plus_and_hover() {
        let theme = Theme::default();
        let tab = TabInfo {
            index: 0,
            name: "sh".into(),
            active: true,
            last: false,
            bell: false,
            attention: None,
            zoomed: false,
        };
        let mut info = StatusInfo {
            session: "main",
            tabs: vec![tab],
            message: Some(("built", Level::Info)),
            start_button: true,
            ..StatusInfo::default()
        };
        let mut g = Grid::new(100, 1);
        let (r, _) = draw(&mut g, 0, &theme, &info);
        let text = g.row_text(0);
        assert!(text.starts_with(" ◆ Menu  [main]  0:sh*  + "), "{text:?}");
        assert!(r.start_at(1, 0) && !r.start_at(9, 0));
        let plus = u16::try_from(text.find(" + ").expect("+")).expect("narrow") - 2;
        assert!(r.new_tab_at(plus + 1, 0), "{r:?}");
        let msg = u16::try_from(unicode_width::UnicodeWidthStr::width(
            &text[..text.find("built").expect("msg")],
        ))
        .expect("narrow");
        assert!(r.message_at(msg, 0));
        // Each thing has its own hover target; empty space has none.
        let targets = [
            r.target_at(1, 0),
            r.target_at(plus + 1, 0),
            r.target_at(msg, 0),
        ];
        assert!(targets.iter().all(Option::is_some));
        assert!(targets[0] != targets[1] && targets[1] != targets[2]);
        assert_eq!(r.target_at(60, 0), None);
        // The pointer over `+` draws it like the active tab.
        info.pointer = Some(plus + 1);
        let mut h = Grid::new(100, 1);
        draw(&mut h, 0, &theme, &info);
        assert_eq!(
            h.get(plus + 1, 0).map(|c| c.style),
            Some(theme.status_tab_active)
        );
        assert_eq!(g.get(plus + 1, 0).map(|c| c.style), Some(theme.status_tab));
    }

    #[test]
    fn icon_sets_and_gradients() {
        let tab = TabInfo {
            index: 0,
            name: "sh".into(),
            active: true,
            last: false,
            bell: false,
            attention: None,
            zoomed: false,
        };
        let info = StatusInfo {
            session: "main",
            tabs: vec![tab],
            start_button: true,
            ..StatusInfo::default()
        };
        let mut out = String::new();
        for (icons, gradients) in [
            (IconSet::Unicode, true),
            (IconSet::Nerd, false),
            (IconSet::Ascii, false),
            (IconSet::None, false),
        ] {
            let theme = Theme {
                icons,
                gradients,
                ..Theme::default()
            };
            let mut g = Grid::new(40, 1);
            let (r, _) = draw(&mut g, 0, &theme, &info);
            assert!(r.start_at(1, 0), "{icons}: the button is still there");
            out.push_str(&format!("{icons} gradients={gradients}\n"));
            out.push_str(&g.to_styled_text());
        }
        insta::assert_snapshot!(out);
    }

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
                    attention: None,
                    zoomed: false,
                },
                TabInfo {
                    index: 1,
                    name: "nvim".into(),
                    active: true,
                    last: false,
                    bell: false,
                    attention: None,
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
    fn the_volume_and_battery_sit_before_the_clock() {
        let mut g = Grid::new(40, 1);
        let info = StatusInfo {
            session: "main",
            clock: Some("12:34".into()),
            battery: Some(("▮ 15%".into(), true)),
            volume: Some("◁ 40%".into()),
            ..StatusInfo::default()
        };
        draw(&mut g, 0, &Theme::default(), &info);
        assert!(
            g.row_text(0).trim_end().ends_with("◁ 40%  ▮ 15%  12:34"),
            "{:?}",
            g.row_text(0)
        );
    }

    #[test]
    fn plugin_widgets_sit_before_the_clock() {
        let mut g = Grid::new(40, 1);
        let widgets = vec![vec![
            ("cpu ".to_owned(), termos_render::cell::Style::new()),
            ("3%".to_owned(), termos_render::cell::Style::new().bold()),
        ]];
        let info = StatusInfo {
            session: "main",
            clock: Some("12:34".into()),
            widgets: &widgets,
            ..StatusInfo::default()
        };
        draw(&mut g, 0, &Theme::default(), &info);
        assert!(
            g.row_text(0).trim_end().ends_with("cpu 3%  12:34"),
            "{:?}",
            g.row_text(0)
        );
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
