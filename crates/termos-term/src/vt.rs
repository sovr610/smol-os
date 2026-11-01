// SPDX-License-Identifier: GPL-3.0-or-later
//! VT emulation behind the [`VtBackend`] trait (DECISIONS.md D-008), backed
//! by `alacritty_terminal`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as VtCell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as VtColor, CursorShape as VtShape, NamedColor, Processor, Rgb as VtRgb,
};
use termos_render::cell::{Attrs, Cell, Style};
use termos_render::color::Color;
use termos_render::encode::{CursorShape, CursorState};
use termos_render::grid::{Grid, Rect};

/// Something the emulated program asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VtEvent {
    Title(String),
    ResetTitle,
    Bell,
    /// Bytes to send back to the program (replies to queries such as DA or
    /// cursor-position reports).
    Reply(Vec<u8>),
    /// OSC 52: put text on the clipboard.
    Clipboard(String),
    /// OSC 7: the program's working directory changed.
    Cwd(PathBuf),
}

/// Terminal modes the desktop needs for routing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VtModes {
    /// Any mouse reporting mode (1000/1002/1003) is on.
    pub mouse_report: bool,
    /// Report drags (1002) — implied by `mouse_motion`.
    pub mouse_drag: bool,
    /// Report all motion (1003).
    pub mouse_motion: bool,
    /// SGR mouse encoding (1006).
    pub sgr_mouse: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub alt_screen: bool,
    /// Wheel becomes arrow keys on the alternate screen (1007).
    pub alternate_scroll: bool,
}

/// A terminal emulator for one pane.
pub trait VtBackend: Send {
    /// Feeds program output.
    fn advance(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn size(&self) -> (u16, u16);
    /// Events produced since the last call.
    fn take_events(&mut self) -> Vec<VtEvent>;
    fn modes(&self) -> VtModes;
    /// Cursor relative to the pane, if visible in the current view.
    fn cursor(&self) -> Option<CursorState>;
    /// Draws the visible screen (respecting scrollback position) at `at`.
    fn render_into(&self, grid: &mut Grid, at: Rect);
    /// Scrolls the view by `lines` (positive = back into history).
    fn scroll(&mut self, lines: i32);
    fn scroll_to_bottom(&mut self);
    /// Lines the view is scrolled back.
    fn display_offset(&self) -> usize;
    /// Lines of history above the screen.
    fn history_size(&self) -> usize;
    /// Text of a line: 0 is the top of the screen, negative is history.
    fn line_text(&self, line: i32) -> String;
    /// When a synchronized update (DEC 2026) started by the program must be
    /// flushed even if it never ends it.
    fn sync_deadline(&self) -> Option<Instant>;
    fn flush_sync(&mut self);
    /// The colours the pane's "default" foreground and background show as
    /// (the outer terminal's), for answering OSC 10/11 queries the program
    /// did not override.
    fn set_default_colors(&mut self, fg: termos_render::color::Rgb, bg: termos_render::color::Rgb);
}

#[derive(Clone, Default)]
struct Listener {
    events: Arc<Mutex<Vec<Event>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Ok(mut q) = self.events.lock() {
            q.push(event);
        }
    }
}

/// The emulator is never narrower than this. `alacritty_terminal` cannot
/// reflow a double-width character into a single column: memory grows
/// without bound (found by the `vt_advance` fuzz target). A pane drawn one
/// cell wide simply shows the first column of a two-column screen.
pub const MIN_COLS: u16 = 2;

/// [`VtBackend`] over `alacritty_terminal`.
pub struct AlacrittyVt {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    pending: Vec<VtEvent>,
    cols: u16,
    rows: u16,
    /// Default foreground and background (see
    /// [`VtBackend::set_default_colors`]).
    defaults: (VtRgb, VtRgb),
}

impl std::fmt::Debug for AlacrittyVt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlacrittyVt")
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

impl AlacrittyVt {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let (cols, rows) = (cols.max(MIN_COLS), rows.max(1));
        let listener = Listener::default();
        let config = TermConfig {
            scrolling_history: scrollback,
            ..TermConfig::default()
        };
        let size = Size {
            cols: usize::from(cols),
            rows: usize::from(rows),
        };
        Self {
            term: Term::new(config, &size, listener.clone()),
            parser: Processor::new(),
            listener,
            pending: Vec::new(),
            cols,
            rows,
            defaults: (default_rgb(256), default_rgb(257)),
        }
    }

    fn drain_listener(&mut self) {
        let events = match self.listener.events.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(_) => return,
        };
        for e in events {
            let mapped = match e {
                Event::Title(t) => Some(VtEvent::Title(t)),
                Event::ResetTitle => Some(VtEvent::ResetTitle),
                Event::Bell => Some(VtEvent::Bell),
                Event::PtyWrite(s) => Some(VtEvent::Reply(s.into_bytes())),
                Event::ClipboardStore(_, text) => Some(VtEvent::Clipboard(text)),
                Event::ColorRequest(index, format) => {
                    let rgb = self.term.colors()[index].unwrap_or(match index {
                        // Foreground and cursor, background.
                        256 | 258 => self.defaults.0,
                        257 => self.defaults.1,
                        _ => default_rgb(index),
                    });
                    Some(VtEvent::Reply(format(rgb).into_bytes()))
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = alacritty_terminal::event::WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        // Cell pixel size is unknown in hosted mode.
                        cell_width: 0,
                        cell_height: 0,
                    };
                    Some(VtEvent::Reply(format(size).into_bytes()))
                }
                // Clipboard reads are refused (OSC 52 paste is a known
                // exfiltration vector); the rest has no meaning here.
                Event::ClipboardLoad(..)
                | Event::MouseCursorDirty
                | Event::CursorBlinkingChange
                | Event::Wakeup
                | Event::Exit
                | Event::ChildExit(_) => None,
            };
            if let Some(m) = mapped {
                self.pending.push(m);
            }
        }
    }
}

/// Fallback colours for OSC 10/11/4 queries the program did not override.
fn default_rgb(index: usize) -> VtRgb {
    let rgb = match index {
        0..=255 => termos_render::color::xterm_256_to_rgb(u8::try_from(index).unwrap_or(0)),
        // Background
        257 => termos_render::color::Rgb::new(0, 0, 0),
        // Foreground, cursor and anything else
        _ => termos_render::color::Rgb::new(229, 229, 229),
    };
    VtRgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

/// Finds OSC 7 (`ESC ] 7 ; file://host/path` BEL|ST) in a chunk. Shells
/// emit the whole sequence in one write, so a sequence split across reads
/// is simply missed (the `/proc` fallback still applies on Linux).
fn scan_osc7(bytes: &[u8]) -> Option<PathBuf> {
    let start = bytes.windows(4).rposition(|w| w == b"\x1b]7;")? + 4;
    let rest = &bytes[start..];
    let end = rest
        .iter()
        .position(|&b| b == 0x07 || b == 0x1b)
        .unwrap_or(rest.len());
    let uri = std::str::from_utf8(&rest[..end]).ok()?;
    let after_scheme = uri.strip_prefix("file://")?;
    let path = &after_scheme[after_scheme.find('/')?..];
    Some(PathBuf::from(percent_decode(path)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl AlacrittyVt {
    fn map_color(&self, c: VtColor) -> Color {
        let override_at =
            |i: usize| self.term.colors()[i].map(|rgb| Color::rgb(rgb.r, rgb.g, rgb.b));
        match c {
            VtColor::Spec(rgb) => Color::rgb(rgb.r, rgb.g, rgb.b),
            VtColor::Indexed(i) => override_at(usize::from(i)).unwrap_or(Color::Indexed(i)),
            VtColor::Named(n) => {
                let idx = n as usize;
                if let Some(c) = override_at(idx) {
                    return c;
                }
                match n {
                    NamedColor::Foreground
                    | NamedColor::Background
                    | NamedColor::Cursor
                    | NamedColor::BrightForeground
                    | NamedColor::DimForeground => Color::Default,
                    NamedColor::DimBlack => Color::Indexed(0),
                    NamedColor::DimRed => Color::Indexed(1),
                    NamedColor::DimGreen => Color::Indexed(2),
                    NamedColor::DimYellow => Color::Indexed(3),
                    NamedColor::DimBlue => Color::Indexed(4),
                    NamedColor::DimMagenta => Color::Indexed(5),
                    NamedColor::DimCyan => Color::Indexed(6),
                    NamedColor::DimWhite => Color::Indexed(7),
                    _ => Color::Indexed(u8::try_from(idx).unwrap_or(7)),
                }
            }
        }
    }

    fn convert_cell(&self, c: &VtCell) -> Cell {
        let f = c.flags;
        let mut attrs = Attrs::empty();
        for (from, to) in [
            (Flags::BOLD, Attrs::BOLD),
            (Flags::DIM, Attrs::DIM),
            (Flags::ITALIC, Attrs::ITALIC),
            (Flags::UNDERLINE, Attrs::UNDERLINE),
            (Flags::DOUBLE_UNDERLINE, Attrs::DOUBLE_UNDERLINE),
            (Flags::UNDERCURL, Attrs::CURLY_UNDERLINE),
            (Flags::DOTTED_UNDERLINE, Attrs::DOTTED_UNDERLINE),
            (Flags::DASHED_UNDERLINE, Attrs::DASHED_UNDERLINE),
            (Flags::INVERSE, Attrs::REVERSE),
            (Flags::HIDDEN, Attrs::HIDDEN),
            (Flags::STRIKEOUT, Attrs::STRIKE),
        ] {
            if f.contains(from) {
                attrs |= to;
            }
        }
        let mut style = Style::new()
            .fg(self.map_color(c.fg))
            .bg(self.map_color(c.bg));
        style.attrs = attrs;
        if let Some(ul) = c.underline_color() {
            style.ul = self.map_color(ul);
        }
        let mut cell = Cell::new(c.c, style);
        if let Some(extra) = c.zerowidth() {
            for ch in extra {
                if !ch.is_control() {
                    cell.symbol.push(*ch);
                }
            }
        }
        if f.contains(Flags::WIDE_CHAR) {
            cell.width = 2;
        }
        cell
    }
}

impl VtBackend for AlacrittyVt {
    fn advance(&mut self, bytes: &[u8]) {
        if let Some(cwd) = scan_osc7(bytes) {
            self.pending.push(VtEvent::Cwd(cwd));
        }
        self.parser.advance(&mut self.term, bytes);
        self.drain_listener();
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(MIN_COLS), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Size {
            cols: usize::from(cols),
            rows: usize::from(rows),
        });
        self.drain_listener();
    }

    fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    fn take_events(&mut self) -> Vec<VtEvent> {
        self.drain_listener();
        std::mem::take(&mut self.pending)
    }

    fn modes(&self) -> VtModes {
        let m = *self.term.mode();
        VtModes {
            mouse_report: m.intersects(TermMode::MOUSE_MODE),
            mouse_drag: m.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            app_cursor: m.contains(TermMode::APP_CURSOR),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            alternate_scroll: m.contains(TermMode::ALTERNATE_SCROLL),
        }
    }

    fn cursor(&self) -> Option<CursorState> {
        if !self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return None;
        }
        let grid = self.term.grid();
        let offset = i32::try_from(grid.display_offset()).ok()?;
        let point = grid.cursor.point;
        let y = point.line.0 + offset;
        if y < 0 || y >= i32::from(self.rows) {
            return None;
        }
        let style = self.term.cursor_style();
        let shape = match style.shape {
            VtShape::Hidden => return None,
            VtShape::Underline => CursorShape::Underline,
            VtShape::Beam => CursorShape::Bar,
            VtShape::Block | VtShape::HollowBlock => CursorShape::Block,
        };
        Some(CursorState {
            x: u16::try_from(point.column.0).ok()?,
            y: u16::try_from(y).ok()?,
            shape,
            blinking: style.blinking,
        })
    }

    fn render_into(&self, grid: &mut Grid, at: Rect) {
        let g = self.term.grid();
        let offset = i32::try_from(g.display_offset()).unwrap_or(0);
        let rows = at.h.min(self.rows);
        let cols = at.w.min(self.cols);
        for y in 0..rows {
            let line = Line(i32::from(y) - offset);
            let row = &g[line];
            for x in 0..cols {
                let c = &row[Column(usize::from(x))];
                if c.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut cell = if c.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    Cell::blank(Style::new().bg(self.map_color(c.bg)))
                } else {
                    self.convert_cell(c)
                };
                if cell.width == 2 && x + 1 >= cols {
                    // Clipped wide character.
                    cell = Cell::blank(cell.style);
                }
                grid.set(at.x + x, at.y + y, cell);
            }
        }
    }

    fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.drain_listener();
    }

    fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.drain_listener();
    }

    fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    fn history_size(&self) -> usize {
        self.term.history_size()
    }

    fn line_text(&self, line: i32) -> String {
        let history = i32::try_from(self.history_size()).unwrap_or(i32::MAX);
        if line < -history || line >= i32::from(self.rows) {
            return String::new();
        }
        let row = &self.term.grid()[Line(line)];
        let mut s = String::new();
        for x in 0..usize::from(self.cols) {
            let c = &row[Column(x)];
            if c.flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            s.push(c.c);
            if let Some(extra) = c.zerowidth() {
                s.extend(extra.iter());
            }
        }
        s.trim_end().to_owned()
    }

    fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    fn flush_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.drain_listener();
    }

    fn set_default_colors(&mut self, fg: termos_render::color::Rgb, bg: termos_render::color::Rgb) {
        let vt = |c: termos_render::color::Rgb| VtRgb {
            r: c.r,
            g: c.g,
            b: c.b,
        };
        self.defaults = (vt(fg), vt(bg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(vt: &AlacrittyVt) -> String {
        let (c, r) = vt.size();
        let mut g = Grid::new(c, r);
        let area = g.area();
        vt.render_into(&mut g, area);
        g.to_text()
    }

    #[test]
    fn text_and_cursor_movement() {
        let mut vt = AlacrittyVt::new(10, 3, 100);
        vt.advance(b"hello\r\nworld\x1b[1;8Hx");
        assert_eq!(screen(&vt), "hello  x\nworld\n\n");
        let cur = vt.cursor().expect("visible");
        assert_eq!((cur.x, cur.y), (8, 0));
    }

    #[test]
    fn colour_queries_answer_the_outer_terminal_colours() {
        let mut vt = AlacrittyVt::new(10, 1, 0);
        use termos_render::color::Rgb;
        vt.set_default_colors(Rgb::new(0x11, 0x22, 0x33), Rgb::new(0xfa, 0xfa, 0xfa));
        vt.advance(b"\x1b]11;?\x1b\\\x1b]10;?\x07");
        let replies: Vec<String> = vt
            .take_events()
            .into_iter()
            .filter_map(|e| match e {
                VtEvent::Reply(r) => Some(String::from_utf8_lossy(&r).into_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b]11;rgb:fafa/fafa/fafa\x1b\\",
                "\x1b]10;rgb:1111/2222/3333\x07"
            ]
        );
        // A colour the program set itself wins.
        vt.advance(b"\x1b]11;#000000\x07\x1b]11;?\x07");
        let ev = vt.take_events();
        assert!(ev.contains(&VtEvent::Reply(b"\x1b]11;rgb:0000/0000/0000\x07".to_vec())));
    }

    #[test]
    fn colours_and_attributes_map() {
        let mut vt = AlacrittyVt::new(10, 1, 0);
        vt.advance(b"\x1b[1;31ma\x1b[0;38;2;1;2;3;48;5;200mb\x1b[0m");
        let mut g = Grid::new(10, 1);
        let area = g.area();
        vt.render_into(&mut g, area);
        let a = g.get(0, 0).expect("cell");
        assert_eq!(a.style.fg, Color::Indexed(1));
        assert!(a.style.attrs.contains(Attrs::BOLD));
        let b = g.get(1, 0).expect("cell");
        assert_eq!(b.style.fg, Color::rgb(1, 2, 3));
        assert_eq!(b.style.bg, Color::Indexed(200));
        let c = g.get(2, 0).expect("cell");
        assert_eq!(c.style, Style::new());
    }

    #[test]
    fn wide_chars_and_combining_marks() {
        let mut vt = AlacrittyVt::new(6, 1, 0);
        vt.advance("界e\u{301}".as_bytes());
        let mut g = Grid::new(6, 1);
        let area = g.area();
        vt.render_into(&mut g, area);
        assert_eq!(g.get(0, 0).map(|c| c.width), Some(2));
        assert!(g.get(1, 0).is_some_and(Cell::is_continuation));
        assert_eq!(g.get(2, 0).map(|c| c.symbol.as_str()), Some("e\u{301}"));
    }

    #[test]
    fn events_title_bell_reply_clipboard() {
        let mut vt = AlacrittyVt::new(10, 2, 0);
        vt.advance(b"\x1b]2;my title\x07\x07\x1b[6n\x1b]52;c;aGk=\x07");
        let ev = vt.take_events();
        assert!(ev.contains(&VtEvent::Title("my title".into())), "{ev:?}");
        assert!(ev.contains(&VtEvent::Bell), "{ev:?}");
        assert!(
            ev.contains(&VtEvent::Reply(b"\x1b[1;1R".to_vec())),
            "{ev:?}"
        );
        assert!(ev.contains(&VtEvent::Clipboard("hi".into())), "{ev:?}");
        assert!(vt.take_events().is_empty());
    }

    #[test]
    fn modes_track_program_requests() {
        let mut vt = AlacrittyVt::new(10, 2, 0);
        assert_eq!(
            vt.modes(),
            VtModes {
                alternate_scroll: vt.modes().alternate_scroll,
                ..VtModes::default()
            }
        );
        vt.advance(b"\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1h\x1b[?1049h\x1b[?1004h");
        let m = vt.modes();
        assert!(m.mouse_report && m.sgr_mouse && m.bracketed_paste);
        assert!(m.app_cursor && m.alt_screen && m.focus_events);
        assert!(!m.mouse_motion);
        vt.advance(b"\x1b[?25l");
        assert!(vt.cursor().is_none());
    }

    #[test]
    fn scrollback_and_line_text() {
        let mut vt = AlacrittyVt::new(8, 2, 100);
        vt.advance(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(vt.history_size(), 2);
        assert_eq!(vt.line_text(-2), "one");
        assert_eq!(vt.line_text(1), "four");
        vt.scroll(1);
        assert_eq!(vt.display_offset(), 1);
        assert_eq!(screen(&vt), "two\nthree\n");
        assert!(vt.cursor().is_none(), "cursor is off-screen while scrolled");
        vt.scroll_to_bottom();
        assert_eq!(screen(&vt), "three\nfour\n");
    }

    #[test]
    fn resize_reflows() {
        let mut vt = AlacrittyVt::new(10, 2, 10);
        vt.advance(b"abcdefgh");
        vt.resize(4, 3);
        assert_eq!(vt.size(), (4, 3));
        // The wrapped text may move partly into history; together it is all there.
        let history = i32::try_from(vt.history_size()).expect("small");
        let all: Vec<String> = (-history..3).map(|l| vt.line_text(l)).collect();
        let joined = all.join("|");
        assert!(joined.contains("abcd|efgh"), "{joined}");
    }

    #[test]
    fn wide_char_survives_one_column_resize() {
        // Minimized from a vt_advance fuzz case that exhausted memory.
        let mut vt = AlacrittyVt::new(40, 10, 50);
        vt.advance("+\u{cd75}".as_bytes());
        vt.resize(1, 1);
        assert_eq!(vt.size(), (MIN_COLS, 1));
        vt.resize(7, 7);
        let history = i32::try_from(vt.history_size()).expect("small");
        let all: String = (-history..7).map(|l| vt.line_text(l)).collect();
        assert!(all.contains('\u{cd75}'), "{all:?}");
        // The exact fuzz input, replayed the way the target does.
        let input = [
            43u8, 236, 181, 181, 28, 181, 246, 254, 252, 0, 252, 9, 0, 0, 0, 0, 254, 252, 6,
        ];
        let mut vt = AlacrittyVt::new(40, 10, 50);
        for chunk in input.split(|&b| b == 0xFE) {
            match chunk.first() {
                Some(&op) if op >= 0xF0 => {
                    let arg = chunk.get(1).copied().unwrap_or(1);
                    vt.resize(u16::from(arg % 120) + 1, u16::from(arg % 50) + 1);
                }
                _ => vt.advance(chunk),
            }
        }
        assert_eq!(vt.size(), (7, 7));
    }

    #[test]
    fn osc7_reports_cwd() {
        let mut vt = AlacrittyVt::new(10, 2, 0);
        vt.advance(b"\x1b]7;file://host/home/u/my%20dir\x07");
        assert!(
            vt.take_events()
                .contains(&VtEvent::Cwd(PathBuf::from("/home/u/my dir")))
        );
        assert_eq!(scan_osc7(b"no osc here"), None);
    }

    #[test]
    fn program_sync_updates_can_be_flushed() {
        let mut vt = AlacrittyVt::new(10, 1, 0);
        vt.advance(b"\x1b[?2026hpartial");
        assert!(vt.sync_deadline().is_some());
        assert_eq!(screen(&vt), "\n");
        vt.flush_sync();
        assert_eq!(screen(&vt), "partial\n");
        assert!(vt.sync_deadline().is_none());
    }
}
