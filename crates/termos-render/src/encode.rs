// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal encoder: turns damage spans into escape sequences.
//!
//! The encoder tracks the outer terminal's cursor position and current SGR
//! state, so a frame only emits the cursor moves and style changes it needs.
//! Each frame is wrapped in synchronized-update brackets (DEC mode 2026) when
//! enabled, so terminals that support it never show a half-drawn frame.
//! Linked cells are written inside OSC 8, so the outer terminal shows and
//! opens the links programs printed (M13a, D-118).

use std::collections::HashMap;
use std::io::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::caps::Caps;
use crate::cell::{Attrs, Style};
use crate::color::{Color, ColorDepth};
use crate::diff::Span;

/// Cursor shapes (DECSCUSR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Where and how to show the cursor after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CursorState {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// Encodes frames for one terminal.
#[derive(Debug)]
pub struct Encoder {
    depth: ColorDepth,
    sync: bool,
    /// The SGR state the terminal is in; `None` = unknown.
    style: Option<Style>,
    /// Where the terminal's cursor is; `None` = unknown (e.g. pending wrap).
    pos: Option<(u16, u16)>,
    cursor_style: Option<(CursorShape, bool)>,
    cols: u16,
    buf: Vec<u8>,
    /// The addresses of the link ids in cells (the server's table).
    links: HashMap<u32, String>,
    /// The link the terminal is writing inside (OSC 8), 0 for none.
    open_link: u32,
}

impl Encoder {
    pub fn new(caps: &Caps) -> Self {
        Self {
            depth: caps.color,
            sync: caps.sync_output,
            style: None,
            pos: None,
            cursor_style: None,
            cols: 0,
            buf: Vec::with_capacity(64 * 1024),
            links: HashMap::new(),
            open_link: 0,
        }
    }

    /// The link table the next frames' cells refer to (it replaces the
    /// one before).
    pub fn set_links(&mut self, links: Vec<(u32, String)>) {
        self.links = links.into_iter().collect();
    }

    /// Forgets everything known about the terminal state (after a resize or
    /// when another program may have written to it).
    pub fn invalidate(&mut self) {
        self.style = None;
        self.pos = None;
        self.cursor_style = None;
    }

    /// Encodes one frame. With `clear`, the screen is erased first (use with
    /// full spans). `cols` is the screen width, needed to track auto-wrap.
    pub fn frame(
        &mut self,
        cols: u16,
        spans: &[Span],
        cursor: Option<CursorState>,
        clear: bool,
    ) -> &[u8] {
        self.frame_with_images(cols, spans, cursor, clear, &[])
    }

    /// [`Encoder::frame`] plus pixel-image output (`images`), written after
    /// the cells so images lie on top, and before the cursor is placed.
    pub fn frame_with_images(
        &mut self,
        cols: u16,
        spans: &[Span],
        cursor: Option<CursorState>,
        clear: bool,
        images: &[u8],
    ) -> &[u8] {
        self.cols = cols;
        self.buf.clear();
        if self.sync {
            self.buf.extend_from_slice(b"\x1b[?2026h");
        }
        // Hide the cursor while drawing.
        self.buf.extend_from_slice(b"\x1b[?25l");
        if clear {
            self.buf.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.style = Some(Style::new());
            self.pos = None;
        }
        for span in spans {
            self.span(span);
        }
        self.link_to(0);
        if !images.is_empty() {
            self.buf.extend_from_slice(images);
            // Image output moves the cursor (and iTerm2's saves and restores
            // the pen): forget both.
            self.pos = None;
            self.style = None;
        }
        if let Some(c) = cursor {
            self.move_to(c.x, c.y);
            if self.cursor_style != Some((c.shape, c.blinking)) {
                let n = match (c.shape, c.blinking) {
                    (CursorShape::Block, true) => 1,
                    (CursorShape::Block, false) => 2,
                    (CursorShape::Underline, true) => 3,
                    (CursorShape::Underline, false) => 4,
                    (CursorShape::Bar, true) => 5,
                    (CursorShape::Bar, false) => 6,
                };
                let _ = write!(self.buf, "\x1b[{n} q");
                self.cursor_style = Some((c.shape, c.blinking));
            }
            self.buf.extend_from_slice(b"\x1b[?25h");
        }
        if self.sync {
            self.buf.extend_from_slice(b"\x1b[?2026l");
        }
        &self.buf
    }

    fn span(&mut self, span: &Span) {
        for (i, cell) in span.cells.iter().enumerate() {
            if cell.is_continuation() {
                // Covered by the preceding wide character.
                continue;
            }
            let Ok(offset) = u16::try_from(i) else { break };
            let mut x = span.col.saturating_add(offset);
            if self.pos != Some((x, span.row)) {
                self.move_to(x, span.row);
            }
            self.link_to(cell.link);
            self.set_style(cell.style);
            let sym = cell.display_symbol();
            for ch in sym.chars() {
                let ch = if ch.is_control() { ' ' } else { ch };
                let mut tmp = [0u8; 4];
                self.buf
                    .extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
            }
            let w = u16::from(cell.width.max(1));
            x += w;
            // Writing into the last column leaves the terminal in the
            // "pending wrap" state; force an explicit move next time.
            self.pos = if x >= self.cols {
                None
            } else {
                Some((x, span.row))
            };
        }
    }

    /// Writes the next cells inside link `id` (0: outside any link).
    fn link_to(&mut self, id: u32) {
        let id = if self.links.contains_key(&id) { id } else { 0 };
        if id == self.open_link {
            return;
        }
        if self.open_link != 0 {
            self.buf.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        if let Some(url) = self.links.get(&id) {
            // The server keeps only addresses without control characters.
            let url: String = url.chars().filter(|c| !c.is_control()).collect();
            let _ = write!(self.buf, "\x1b]8;;{url}\x1b\\");
        }
        self.open_link = id;
    }

    fn move_to(&mut self, x: u16, y: u16) {
        let _ = write!(self.buf, "\x1b[{};{}H", u32::from(y) + 1, u32::from(x) + 1);
        self.pos = Some((x, y));
    }

    fn set_style(&mut self, style: Style) {
        if self.style == Some(style) {
            return;
        }
        // Reset and set everything: simpler than a minimal SGR delta and only
        // a few bytes longer.
        self.buf.extend_from_slice(b"\x1b[0");
        let a = style.attrs;
        let flags: [(Attrs, &[u8]); 8] = [
            (Attrs::BOLD, b";1"),
            (Attrs::DIM, b";2"),
            (Attrs::ITALIC, b";3"),
            (Attrs::BLINK, b";5"),
            (Attrs::REVERSE, b";7"),
            (Attrs::HIDDEN, b";8"),
            (Attrs::STRIKE, b";9"),
            (Attrs::UNDERLINE, b";4"),
        ];
        for (flag, code) in flags {
            if a.contains(flag) {
                self.buf.extend_from_slice(code);
            }
        }
        if !a.contains(Attrs::UNDERLINE) {
            for (flag, code) in [
                (Attrs::DOUBLE_UNDERLINE, &b";4:2"[..]),
                (Attrs::CURLY_UNDERLINE, b";4:3"),
                (Attrs::DOTTED_UNDERLINE, b";4:4"),
                (Attrs::DASHED_UNDERLINE, b";4:5"),
            ] {
                if a.contains(flag) {
                    self.buf.extend_from_slice(code);
                    break;
                }
            }
        }
        self.color(style.fg.downsample(self.depth), 30, 90, 38);
        self.color(style.bg.downsample(self.depth), 40, 100, 48);
        if style.ul != Color::Default && self.depth != ColorDepth::Ansi16 {
            match style.ul.downsample(self.depth) {
                Color::Rgb(c) => {
                    let _ = write!(self.buf, ";58;2;{};{};{}", c.r, c.g, c.b);
                }
                Color::Indexed(i) => {
                    let _ = write!(self.buf, ";58;5;{i}");
                }
                Color::Default => {}
            }
        }
        self.buf.push(b'm');
        self.style = Some(style);
    }

    fn color(&mut self, color: Color, base: u8, bright_base: u8, extended: u8) {
        match color {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => {
                let _ = write!(self.buf, ";{}", base + i);
            }
            Color::Indexed(i) if i < 16 => {
                let _ = write!(self.buf, ";{}", bright_base + i - 8);
            }
            Color::Indexed(i) => {
                let _ = write!(self.buf, ";{extended};5;{i}");
            }
            Color::Rgb(c) => {
                let _ = write!(self.buf, ";{extended};2;{};{};{}", c.r, c.g, c.b);
            }
        }
    }
}

/// OSC 52: ask the terminal to put `text` on the system clipboard.
pub fn osc52_copy(text: &str) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07").into_bytes()
}

/// OSC 0/2: set the outer terminal's window title.
pub fn set_title(title: &str) -> Vec<u8> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]2;{clean}\x07").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{diff, full};
    use crate::grid::Grid;

    fn caps(depth: ColorDepth) -> Caps {
        Caps {
            color: depth,
            sync_output: false,
            ..Caps::default()
        }
    }

    fn parser(rows: u16, cols: u16) -> vt100::Parser {
        let nz = |v: u16| std::num::NonZeroU16::new(v).expect("non-zero size");
        vt100::Parser::new(nz(rows), nz(cols), 0)
    }

    fn trimmed(p: &vt100::Parser) -> String {
        let text = p.screen().contents();
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(enc: &mut Encoder, g: &Grid, spans: &[Span]) -> vt100::Parser {
        let mut p = parser(g.rows(), g.cols());
        p.process(enc.frame(g.cols(), spans, None, true));
        p
    }

    #[test]
    fn plain_text_round_trips() {
        let mut g = Grid::new(10, 2);
        g.put_str(0, 0, "hello", Style::new(), 10);
        g.put_str(3, 1, "world", Style::new(), 10);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        let p = render(&mut enc, &g, &full(&g));
        assert_eq!(trimmed(&p), "hello\n   world");
    }

    #[test]
    fn styles_are_encoded() {
        let mut g = Grid::new(4, 1);
        let s = Style::new()
            .fg(Color::rgb(1, 2, 3))
            .bg(Color::Indexed(4))
            .bold();
        g.put_str(0, 0, "ab", s, 4);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        let p = render(&mut enc, &g, &full(&g));
        let cell = p.screen().cell(0, 1).expect("cell exists");
        assert_eq!(cell.fgcolor(), vt100::Color::Rgb(1, 2, 3));
        assert_eq!(cell.bgcolor(), vt100::Color::Idx(4));
        assert!(cell.bold());
    }

    #[test]
    fn downsamples_for_16_colour_terminals() {
        let mut g = Grid::new(1, 1);
        g.put_str(0, 0, "x", Style::new().fg(Color::rgb(250, 0, 0)), 1);
        let mut enc = Encoder::new(&caps(ColorDepth::Ansi16));
        let out = enc.frame(1, &full(&g), None, false).to_vec();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains(";91m"), "{text:?}");
        assert!(!text.contains("38;2"), "{text:?}");
    }

    #[test]
    fn last_column_forces_cursor_move() {
        let mut g = Grid::new(3, 2);
        g.put_str(0, 0, "abc", Style::new(), 3);
        g.put_str(0, 1, "def", Style::new(), 3);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        let p = render(&mut enc, &g, &full(&g));
        assert_eq!(trimmed(&p), "abc\ndef");
    }

    #[test]
    fn incremental_frames_only_touch_changes() {
        let a = Grid::new(20, 5);
        let mut b = a.clone();
        b.put_str(7, 2, "z", Style::new(), 20);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        enc.frame(20, &full(&a), None, true);
        let out = enc.frame(20, &diff(&a, &b), None, false).to_vec();
        // hide cursor + one move + one char, no clears or full redraws.
        assert_eq!(out, b"\x1b[?25l\x1b[3;8Hz");
    }

    #[test]
    fn cursor_is_positioned_and_shown() {
        let g = Grid::new(5, 2);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        let cursor = CursorState {
            x: 2,
            y: 1,
            shape: CursorShape::Bar,
            blinking: false,
        };
        let out = enc.frame(5, &full(&g), Some(cursor), true).to_vec();
        let mut p = parser(2, 5);
        p.process(&out);
        assert_eq!(p.screen().cursor_position(), (1, 2));
        assert!(!p.screen().hide_cursor());
        assert!(String::from_utf8_lossy(&out).contains("\x1b[6 q"));
    }

    #[test]
    fn sync_brackets_wrap_frames() {
        let g = Grid::new(1, 1);
        let mut enc = Encoder::new(&Caps {
            sync_output: true,
            ..caps(ColorDepth::Truecolor)
        });
        let out = enc.frame(1, &full(&g), None, false).to_vec();
        assert!(out.starts_with(b"\x1b[?2026h"));
        assert!(out.ends_with(b"\x1b[?2026l"));
    }

    fn linked(text: &str, links: &[u32]) -> Grid {
        let mut g = Grid::new(u16::try_from(text.len()).expect("short"), 1);
        g.put_str(0, 0, text, Style::new(), 80);
        for (x, id) in links.iter().enumerate() {
            if let Some(c) = g.get_mut(u16::try_from(x).expect("short"), 0) {
                c.link = *id;
            }
        }
        g
    }

    #[test]
    fn a_linked_run_is_one_osc8() {
        let g = linked("abcd", &[0, 1, 1, 0]);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        enc.set_links(vec![(1, "https://example.com/x".into())]);
        let out = enc.frame(4, &full(&g), None, false).to_vec();
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[?25l\x1b[1;1H\x1b[0ma\x1b]8;;https://example.com/x\x1b\\bc\x1b]8;;\x1b\\d"
        );
    }

    #[test]
    fn a_link_that_changes_closes_and_reopens() {
        let g = linked("abc", &[1, 2, 2]);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        enc.set_links(vec![(1, "https://a".into()), (2, "https://b".into())]);
        let out = enc.frame(3, &full(&g), None, false).to_vec();
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[?25l\x1b[1;1H\x1b]8;;https://a\x1b\\\x1b[0ma\x1b]8;;\x1b\\\x1b]8;;https://b\x1b\\bc\x1b]8;;\x1b\\"
        );
        // The link is closed before the frame ends, so nothing else the
        // terminal writes is linked.
        let mut p = parser(1, 3);
        p.process(&out);
        assert_eq!(trimmed(&p), "abc");
    }

    #[test]
    fn unknown_link_ids_write_no_link() {
        let g = linked("ab", &[7, 7]);
        let mut enc = Encoder::new(&caps(ColorDepth::Truecolor));
        let out = enc.frame(2, &full(&g), None, false).to_vec();
        assert!(!String::from_utf8_lossy(&out).contains("]8;"));
    }

    #[test]
    fn osc_helpers() {
        assert_eq!(osc52_copy("hi"), b"\x1b]52;c;aGk=\x07");
        assert_eq!(set_title("a\x1bb"), b"\x1b]2;ab\x07");
    }
}
