// SPDX-License-Identifier: GPL-3.0-or-later
//! Widgets for apps (ARCHITECTURE.md §9): line charts drawn in braille,
//! sparklines and meters in eighth blocks, and an image slot. They are
//! ratatui widgets, so apps lay them out like any other.

use std::sync::Arc;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect as TuiRect;
use ratatui_core::style::Style;
use ratatui_core::widgets::Widget;
use termos_render::graphics::Picture;
use termos_render::graphics::braille::Canvas;
use termos_render::grid::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::AppImage;

/// Partial blocks from the bottom, in eighths (index 1 to 8).
const UP: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Partial blocks from the left, in eighths (index 1 to 8).
const RIGHT: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

fn set(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch).set_style(style);
    }
}

fn put_str(buf: &mut Buffer, x: u16, y: u16, s: &str, max: u16, style: Style) -> u16 {
    let mut used = 0u16;
    for ch in s.chars() {
        let w =
            u16::try_from(UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]) as &str)).unwrap_or(1);
        if used + w > max {
            break;
        }
        if let Some(cell) = buf.cell_mut((x + used, y)) {
            cell.set_char(ch).set_style(style);
        }
        used += w;
    }
    used
}

/// The scale's top: `max`, or the largest value (at least `floor`).
fn top(max: Option<f64>, data: impl Iterator<Item = f64>, floor: f64) -> f64 {
    max.filter(|m| *m > 0.0)
        .unwrap_or_else(|| data.fold(floor, f64::max))
}

/// Recent values as bars, newest on the right, one value per column.
#[derive(Debug, Clone)]
pub struct Sparkline<'a> {
    pub data: &'a [f64],
    /// The top of the scale; by default the largest value shown.
    pub max: Option<f64>,
    pub style: Style,
}

impl Widget for Sparkline<'_> {
    fn render(self, area: TuiRect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let n = usize::from(area.width).min(self.data.len());
        let shown = &self.data[self.data.len() - n..];
        let top = top(self.max, shown.iter().copied(), f64::MIN_POSITIVE);
        let levels = f64::from(area.height) * 8.0;
        let x0 = area.x + area.width - n as u16;
        for (i, v) in shown.iter().enumerate() {
            let mut level = ((v.max(0.0) / top).min(1.0) * levels).round() as u32;
            // Anything above zero shows.
            if *v > 0.0 {
                level = level.max(1);
            }
            for row in 0..area.height {
                // Rows from the bottom.
                let below = u32::from(row) * 8;
                let eighths = level.saturating_sub(below).min(8) as usize;
                let y = area.y + area.height - 1 - row;
                set(buf, x0 + i as u16, y, UP[eighths], self.style);
            }
        }
    }
}

/// A horizontal bar: `label`, the bar, then `value` (e.g. "42%").
#[derive(Debug, Clone)]
pub struct Meter<'a> {
    /// 0 to 1.
    pub ratio: f64,
    pub label: &'a str,
    pub value: &'a str,
    /// Columns for the label (labels of meters stacked together line up).
    pub label_width: u16,
    pub fill: Style,
    pub empty: Style,
    pub text: Style,
}

impl Widget for Meter<'_> {
    fn render(self, area: TuiRect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let y = area.y;
        let label_w = self.label_width.min(area.width);
        put_str(buf, area.x, y, self.label, label_w, self.text);
        let value_w = (UnicodeWidthStr::width(self.value) as u16).min(area.width - label_w);
        let bar_x = area.x + label_w;
        let bar_w = area.width - label_w - value_w;
        let gap = u16::from(bar_w > 2);
        let bar_w = bar_w.saturating_sub(gap * 2);
        let eighths = (self.ratio.clamp(0.0, 1.0) * f64::from(bar_w) * 8.0).round() as u32;
        for i in 0..bar_w {
            let e = eighths.saturating_sub(u32::from(i) * 8).min(8) as usize;
            let (ch, style) = if e == 0 {
                ('·', self.empty)
            } else {
                (RIGHT[e], self.fill)
            };
            set(buf, bar_x + gap + i, y, ch, style);
        }
        put_str(
            buf,
            area.x + area.width - value_w,
            y,
            self.value,
            value_w,
            self.text,
        );
    }
}

/// One line of a [`LineChart`].
#[derive(Debug, Clone)]
pub struct Series<'a> {
    pub data: &'a [f64],
    pub style: Style,
}

/// Lines over time in braille (2×4 dots per cell), newest on the right,
/// one value per dot column. Where lines cross, the later series wins
/// the cell's colour.
#[derive(Debug, Clone)]
pub struct LineChart<'a> {
    pub series: &'a [Series<'a>],
    /// The top of the scale; by default the largest value shown.
    pub max: Option<f64>,
    /// Labels for the top and the bottom of the scale, drawn on the left.
    pub labels: Option<(&'a str, &'a str)>,
    pub label_style: Style,
}

impl Widget for LineChart<'_> {
    fn render(self, area: TuiRect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let label_w = self.labels.map_or(0, |(a, b)| {
            (UnicodeWidthStr::width(a).max(UnicodeWidthStr::width(b)) as u16 + 1)
                .min(area.width / 2)
        });
        if let Some((hi, lo)) = self.labels {
            put_str(buf, area.x, area.y, hi, label_w, self.label_style);
            put_str(
                buf,
                area.x,
                area.y + area.height - 1,
                lo,
                label_w,
                self.label_style,
            );
        }
        let plot = TuiRect::new(area.x + label_w, area.y, area.width - label_w, area.height);
        if plot.is_empty() {
            return;
        }
        let mut canvas: Canvas<usize> = Canvas::new(plot.width, plot.height);
        let (w, h) = (canvas.width_dots(), canvas.height_dots());
        let top = top(
            self.max,
            self.series.iter().flat_map(|s| {
                let n = (w as usize).min(s.data.len());
                s.data[s.data.len() - n..].iter().copied()
            }),
            f64::MIN_POSITIVE,
        );
        let dot_y = |v: f64| {
            let r = (v.max(0.0) / top).min(1.0);
            ((1.0 - r) * f64::from(h - 1)).round() as u32
        };
        for (si, s) in self.series.iter().enumerate() {
            let n = (w as usize).min(s.data.len());
            let shown = &s.data[s.data.len() - n..];
            let x0 = w - n as u32;
            let mut prev: Option<u32> = None;
            for (i, v) in shown.iter().enumerate() {
                let y = dot_y(*v);
                let x = x0 + i as u32;
                // Join to the previous point so steep changes stay a line.
                let (a, b) = prev.map_or((y, y), |p| (p.min(y), p.max(y)));
                for yy in a..=b {
                    canvas.dot(x, yy, si);
                }
                prev = Some(y);
            }
        }
        for row in 0..plot.height {
            for col in 0..plot.width {
                if let Some((ch, si)) = canvas.cell(col, row) {
                    let style = self.series.get(si).map_or(Style::default(), |s| s.style);
                    set(buf, plot.x + col, plot.y + row, ch, style);
                }
            }
        }
    }
}

/// A place for a picture in an app: blanks its cells and says where the
/// desktop should show the image (see [`crate::app::App::images`]).
#[derive(Debug, Clone)]
pub struct ImageSlot {
    pub key: u64,
    pub picture: Arc<Picture>,
    pub upscale: bool,
}

impl ImageSlot {
    /// Clears `area` in `buf` (relative to the app's area) and returns
    /// the image to report from `App::images`.
    pub fn place(&self, area: TuiRect, buf: &mut Buffer, bg: Style) -> AppImage {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                set(buf, x, y, ' ', bg);
            }
        }
        AppImage {
            key: self.key,
            picture: Arc::clone(&self.picture),
            rect: Rect::new(area.x, area.y, area.width, area.height),
            upscale: self.upscale,
            scroll_top: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn render(w: impl Widget, cols: u16, rows: u16) -> String {
        let area = TuiRect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        w.render(area, &mut buf);
        text(&buf)
    }

    #[test]
    fn sparklines_fill_from_the_bottom() {
        let data = [0.0, 1.0, 2.0, 4.0, 8.0];
        let s = Sparkline {
            data: &data,
            max: Some(8.0),
            style: Style::default(),
        };
        assert_eq!(render(s.clone(), 5, 1), " ▁▂▄█\n");
        assert_eq!(render(s.clone(), 7, 2), "      █\n   ▂▄██\n");
        // Narrower than the data: the newest values.
        assert_eq!(render(s, 2, 1), "▄█\n");
    }

    #[test]
    fn meters_show_label_bar_and_value() {
        let m = Meter {
            ratio: 0.5,
            label: "CPU",
            value: "50%",
            label_width: 4,
            fill: Style::default(),
            empty: Style::default(),
            text: Style::default(),
        };
        assert_eq!(render(m.clone(), 19, 1), "CPU  █████····· 50%\n");
        let m = Meter {
            ratio: 0.3,
            value: "30%",
            ..m
        };
        insta::assert_snapshot!("meter-partial", render(m, 24, 1));
    }

    #[test]
    fn line_charts_draw_joined_lines() {
        let ramp: Vec<f64> = (0..16).map(f64::from).collect();
        let flat = [5.0; 16];
        let series = [
            Series {
                data: &ramp,
                style: Style::default(),
            },
            Series {
                data: &flat,
                style: Style::default(),
            },
        ];
        let chart = LineChart {
            series: &series,
            max: Some(15.0),
            labels: Some(("15", "0")),
            label_style: Style::default(),
        };
        insta::assert_snapshot!("chart-ramp-and-flat", render(chart, 11, 4));
        // A spike is drawn as a vertical line, not two lone dots.
        let spike = [0.0, 0.0, 10.0, 0.0];
        let series = [Series {
            data: &spike,
            style: Style::default(),
        }];
        let chart = LineChart {
            series: &series,
            max: Some(10.0),
            labels: None,
            label_style: Style::default(),
        };
        insta::assert_snapshot!("chart-spike", render(chart, 2, 2));
    }
}
