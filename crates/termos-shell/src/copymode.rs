// SPDX-License-Identifier: GPL-3.0-or-later
//! Copy mode (tmux `copy-mode`, vi keys): move a cursor through the
//! scrollback, select, search, and copy.
//!
//! Lines are absolute: 0 is the top of the screen, negative numbers are
//! history. Columns are cells, so wide characters count twice.

use termos_core::ids::PaneId;
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::cell::Style;
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use termos_term::vt::VtBackend;
use unicode_width::UnicodeWidthChar;

use crate::keys::{is_cancel, printable};

/// What copy mode wants after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyAction {
    None,
    Exit,
    /// Copy this text (to the paste buffer and clipboard) and exit.
    Copy(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchInput {
    forward: bool,
    text: String,
}

/// Copy-mode state for one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyMode {
    pub pane: PaneId,
    pub cx: u16,
    pub cy: i32,
    anchor: Option<(u16, i32)>,
    line_mode: bool,
    search: Option<SearchInput>,
    last_search: Option<(String, bool)>,
    /// Entered with the mouse wheel: scrolling back to the bottom exits.
    pub exit_at_bottom: bool,
}

/// (start column, width, char) for each character of a line.
fn char_cols(text: &str) -> Vec<(u16, u16, char)> {
    let mut col = 0u16;
    text.chars()
        .map(|c| {
            let w = u16::try_from(UnicodeWidthChar::width(c).unwrap_or(0)).unwrap_or(1);
            let entry = (col, w, c);
            col = col.saturating_add(w);
            entry
        })
        .collect()
}

/// The text between columns `from` and `to` (inclusive).
fn slice_cols(text: &str, from: u16, to: u16) -> String {
    char_cols(text)
        .into_iter()
        .filter(|(c, w, _)| *c + (*w).max(1) > from && *c <= to)
        .map(|(_, _, ch)| ch)
        .collect()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl CopyMode {
    /// Starts at the pane's cursor (or its bottom-left corner).
    pub fn new(pane: PaneId, vt: &dyn VtBackend) -> Self {
        let off = i32::try_from(vt.display_offset()).unwrap_or(0);
        let rows = i32::from(vt.size().1);
        let (cx, cy) = vt
            .cursor()
            .map_or((0, rows - 1 - off), |c| (c.x, i32::from(c.y) - off));
        Self {
            pane,
            cx,
            cy,
            anchor: None,
            line_mode: false,
            search: None,
            last_search: None,
            exit_at_bottom: false,
        }
    }

    fn bounds(vt: &dyn VtBackend) -> (i32, i32) {
        let hist = i32::try_from(vt.history_size()).unwrap_or(0);
        (-hist, i32::from(vt.size().1) - 1)
    }

    fn clamp(&mut self, vt: &dyn VtBackend) {
        let (top, bottom) = Self::bounds(vt);
        self.cy = self.cy.clamp(top, bottom);
        self.cx = self.cx.min(vt.size().0.saturating_sub(1));
    }

    /// Scrolls the view so the cursor is visible.
    fn reveal(&self, vt: &mut dyn VtBackend) {
        let off = i32::try_from(vt.display_offset()).unwrap_or(0);
        let rows = i32::from(vt.size().1);
        let top = -off;
        let bottom = rows - 1 - off;
        if self.cy < top {
            vt.scroll(top - self.cy);
        } else if self.cy > bottom {
            vt.scroll(-(self.cy - bottom));
        }
    }

    fn line(vt: &dyn VtBackend, line: i32) -> String {
        vt.line_text(line)
    }

    fn line_end_col(vt: &dyn VtBackend, line: i32) -> u16 {
        char_cols(&Self::line(vt, line))
            .last()
            .map_or(0, |(c, _, _)| *c)
    }

    /// Normalized selection `(start, end)`, inclusive.
    pub fn selection(&self) -> Option<((u16, i32), (u16, i32))> {
        let a = self.anchor?;
        let b = (self.cx, self.cy);
        let (s, e) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };
        Some((s, e))
    }

    pub fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    /// Starts a selection at the cursor (used by mouse drags too).
    pub fn begin_selection(&mut self, line_mode: bool) {
        self.anchor = Some((self.cx, self.cy));
        self.line_mode = line_mode;
    }

    /// Moves the cursor to (`col`, `line`) (mouse).
    pub fn move_to(&mut self, col: u16, line: i32, vt: &dyn VtBackend) {
        self.cx = col;
        self.cy = line;
        self.clamp(vt);
    }

    /// Selects from `start` to `end` (columns and lines, inclusive) and
    /// scrolls them into view: a match of the search across panes (D-118).
    pub fn select_range(&mut self, start: (u16, i32), end: (u16, i32), vt: &mut dyn VtBackend) {
        self.move_to(start.0, start.1, &*vt);
        self.anchor = Some((self.cx, self.cy));
        self.line_mode = false;
        self.move_to(end.0, end.1, &*vt);
        self.reveal(vt);
    }

    /// The selected text, if there is a selection.
    pub fn selected_text(&self, vt: &dyn VtBackend) -> Option<String> {
        let (s, e) = self.selection()?;
        let mut lines = Vec::new();
        for line in s.1..=e.1 {
            let text = Self::line(vt, line);
            let piece = if self.line_mode {
                text
            } else {
                let from = if line == s.1 { s.0 } else { 0 };
                let to = if line == e.1 { e.0 } else { u16::MAX };
                slice_cols(&text, from, to)
            };
            lines.push(piece);
        }
        Some(lines.join("\n"))
    }

    fn move_word(&mut self, vt: &dyn VtBackend, forward: bool, to_end: bool) {
        let (top, bottom) = Self::bounds(vt);
        let mut line = self.cy;
        let mut chars = char_cols(&Self::line(vt, line));
        let mut idx = chars
            .iter()
            .position(|(c, w, _)| self.cx >= *c && self.cx < *c + (*w).max(1))
            .unwrap_or(chars.len());
        loop {
            if forward {
                let start_class = chars.get(idx).map(|(_, _, ch)| is_word(*ch));
                if to_end {
                    idx += 1;
                    while idx < chars.len() && chars[idx].2.is_whitespace() {
                        idx += 1;
                    }
                    let class = chars.get(idx).map(|(_, _, ch)| is_word(*ch));
                    while idx + 1 < chars.len()
                        && Some(is_word(chars[idx + 1].2)) == class
                        && !chars[idx + 1].2.is_whitespace()
                    {
                        idx += 1;
                    }
                } else {
                    while idx < chars.len()
                        && Some(is_word(chars[idx].2)) == start_class
                        && !chars[idx].2.is_whitespace()
                    {
                        idx += 1;
                    }
                    while idx < chars.len() && chars[idx].2.is_whitespace() {
                        idx += 1;
                    }
                }
                if idx < chars.len() {
                    break;
                }
                if line >= bottom {
                    idx = chars.len().saturating_sub(1);
                    break;
                }
                line += 1;
                chars = char_cols(&Self::line(vt, line));
                idx = 0;
                if chars.first().is_some_and(|(_, _, c)| !c.is_whitespace()) {
                    break;
                }
            } else {
                if idx == 0 {
                    if line <= top {
                        break;
                    }
                    line -= 1;
                    chars = char_cols(&Self::line(vt, line));
                    idx = chars.len();
                    if idx == 0 {
                        break;
                    }
                }
                idx -= 1;
                while idx > 0 && chars[idx].2.is_whitespace() {
                    idx -= 1;
                }
                let class = is_word(chars[idx].2);
                while idx > 0
                    && is_word(chars[idx - 1].2) == class
                    && !chars[idx - 1].2.is_whitespace()
                {
                    idx -= 1;
                }
                break;
            }
        }
        self.cy = line;
        self.cx = chars.get(idx).map_or(0, |(c, _, _)| *c);
    }

    /// Finds `query` starting after (forward) or before the cursor,
    /// wrapping around. Lowercase queries match case-insensitively.
    fn find(&self, vt: &dyn VtBackend, query: &str, forward: bool) -> Option<(u16, i32)> {
        if query.is_empty() {
            return None;
        }
        let smart = query.chars().all(|c| !c.is_uppercase());
        let fold = |c: char| {
            if smart {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c
            }
        };
        let needle: Vec<char> = query.chars().map(fold).collect();
        let (top, bottom) = Self::bounds(vt);
        let total = bottom - top + 1;
        let matches_in = |line: i32| -> Vec<u16> {
            let cols = char_cols(&Self::line(vt, line));
            let hay: Vec<char> = cols.iter().map(|(_, _, c)| fold(*c)).collect();
            (0..hay.len())
                .filter(|&i| hay[i..].starts_with(&needle))
                .map(|i| cols[i].0)
                .collect()
        };
        for step in 0..=total {
            let line = if forward {
                top + (self.cy - top + step).rem_euclid(total)
            } else {
                top + (self.cy - top - step).rem_euclid(total)
            };
            let mut hits = matches_in(line);
            if step == 0 {
                hits.retain(|&c| if forward { c > self.cx } else { c < self.cx });
            }
            let hit = if forward { hits.first() } else { hits.last() };
            if let Some(&c) = hit {
                return Some((c, line));
            }
        }
        None
    }

    fn run_search(&mut self, vt: &dyn VtBackend, query: &str, forward: bool) -> bool {
        match self.find(vt, query, forward) {
            Some((x, y)) => {
                self.cx = x;
                self.cy = y;
                true
            }
            None => false,
        }
    }

    /// Handles a key. Returns what to do and whether a search failed.
    pub fn handle_key(&mut self, k: KeyEvent, vt: &mut dyn VtBackend) -> (CopyAction, bool) {
        let mut search_failed = false;
        if let Some(search) = &mut self.search {
            if is_cancel(k) {
                self.search = None;
                return (CopyAction::None, false);
            }
            match k.code {
                KeyCode::Enter => {
                    let s = self.search.take().unwrap_or(SearchInput {
                        forward: true,
                        text: String::new(),
                    });
                    if !s.text.is_empty() {
                        search_failed = !self.run_search(vt, &s.text, s.forward);
                        self.last_search = Some((s.text, s.forward));
                    }
                }
                KeyCode::Backspace => {
                    search.text.pop();
                }
                _ => {
                    if let Some(c) = printable(k) {
                        search.text.push(c);
                    }
                }
            }
            self.reveal(vt);
            return (CopyAction::None, search_failed);
        }

        let rows = i32::from(vt.size().1);
        let off = i32::try_from(vt.display_offset()).unwrap_or(0);
        let (top, bottom) = Self::bounds(vt);
        let half = (rows / 2).max(1);
        match k.code {
            KeyCode::Char('q') if k.mods.is_empty() => return (CopyAction::Exit, false),
            KeyCode::Esc => {
                if self.anchor.take().is_none() {
                    return (CopyAction::Exit, false);
                }
            }
            KeyCode::Enter | KeyCode::Char('y')
                if !k.mods.contains(termos_core::input::Mods::CTRL) =>
            {
                return match self.selected_text(vt) {
                    Some(text) => (CopyAction::Copy(text), false),
                    None => (CopyAction::Exit, false),
                };
            }
            KeyCode::Char('h') | KeyCode::Left => self.cx = self.cx.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right => self.cx = self.cx.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up
                if !k.mods.contains(termos_core::input::Mods::CTRL) =>
            {
                self.cy -= 1;
            }
            KeyCode::Char('j') | KeyCode::Down
                if !k.mods.contains(termos_core::input::Mods::CTRL) =>
            {
                self.cy += 1;
            }
            KeyCode::Char('w') => self.move_word(vt, true, false),
            KeyCode::Char('e') if !k.mods.contains(termos_core::input::Mods::CTRL) => {
                self.move_word(vt, true, true);
            }
            KeyCode::Char('b') if k.mods.is_empty() => self.move_word(vt, false, false),
            KeyCode::Char('0') | KeyCode::Home => self.cx = 0,
            KeyCode::Char('^') => {
                self.cx = char_cols(&Self::line(vt, self.cy))
                    .iter()
                    .find(|(_, _, c)| !c.is_whitespace())
                    .map_or(0, |(c, _, _)| *c);
            }
            KeyCode::Char('$') | KeyCode::End => self.cx = Self::line_end_col(vt, self.cy),
            KeyCode::Char('g') => {
                self.cy = top;
                self.cx = 0;
            }
            KeyCode::Char('G') => {
                self.cy = bottom;
                self.cx = 0;
            }
            KeyCode::Char('H') => self.cy = -off,
            KeyCode::Char('M') => self.cy = -off + rows / 2,
            KeyCode::Char('L') => self.cy = -off + rows - 1,
            KeyCode::Char('u') if k == KeyEvent::ctrl('u') => self.cy -= half,
            KeyCode::Char('d') if k == KeyEvent::ctrl('d') => self.cy += half,
            KeyCode::Char('b') if k == KeyEvent::ctrl('b') => self.cy -= rows,
            KeyCode::Char('f') if k == KeyEvent::ctrl('f') => self.cy += rows,
            KeyCode::PageUp => self.cy -= rows,
            KeyCode::PageDown => self.cy += rows,
            KeyCode::Char('v' | ' ') if k.mods.is_empty() => {
                if self.anchor.is_some() && !self.line_mode {
                    self.anchor = None;
                } else {
                    self.begin_selection(false);
                }
            }
            KeyCode::Char('V') => {
                if self.anchor.is_some() && self.line_mode {
                    self.anchor = None;
                } else {
                    self.begin_selection(true);
                }
            }
            KeyCode::Char('/') => {
                self.search = Some(SearchInput {
                    forward: true,
                    text: String::new(),
                });
            }
            KeyCode::Char('?') => {
                self.search = Some(SearchInput {
                    forward: false,
                    text: String::new(),
                });
            }
            KeyCode::Char('n' | 'N') => {
                if let Some((q, fwd)) = self.last_search.clone() {
                    let dir = if k.code == KeyCode::Char('n') {
                        fwd
                    } else {
                        !fwd
                    };
                    search_failed = !self.run_search(vt, &q, dir);
                }
            }
            // Command blocks (OSC 133, D-116): the previous and next
            // prompt, and the output of the command the cursor is in.
            KeyCode::Char('[') if k.mods.is_empty() => {
                if let Some(b) = vt.blocks().iter().rev().find(|b| b.prompt_line < self.cy) {
                    self.cy = b.prompt_line;
                    self.cx = 0;
                }
            }
            KeyCode::Char(']') if k.mods.is_empty() => {
                if let Some(b) = vt.blocks().iter().find(|b| b.prompt_line > self.cy) {
                    self.cy = b.prompt_line;
                    self.cx = 0;
                }
            }
            KeyCode::Char('o') if k.mods.is_empty() => {
                let blocks = vt.blocks();
                if let Some(b) = blocks.iter().rev().find(|b| b.prompt_line <= self.cy)
                    && !b.output.is_empty()
                {
                    return (CopyAction::Copy(b.output.join("\n")), false);
                }
            }
            _ => {}
        }
        self.clamp(vt);
        self.reveal(vt);
        (CopyAction::None, search_failed)
    }

    /// Mouse wheel: scrolls the view (and the cursor with it). Returns
    /// `true` if copy mode should end (scrolled back to the bottom after
    /// being entered with the wheel).
    pub fn scroll(&mut self, lines: i32, vt: &mut dyn VtBackend) -> bool {
        vt.scroll(lines);
        let off = i32::try_from(vt.display_offset()).unwrap_or(0);
        let rows = i32::from(vt.size().1);
        self.cy = self.cy.clamp(-off, rows - 1 - off);
        self.exit_at_bottom && lines < 0 && off == 0 && self.anchor.is_none()
    }

    /// Where the terminal cursor should be, relative to the pane.
    pub fn screen_cursor(&self, vt: &dyn VtBackend) -> Option<(u16, u16)> {
        let (cols, rows) = vt.size();
        if let Some(s) = &self.search {
            let x = u16::try_from(s.text.chars().count() + 1).unwrap_or(u16::MAX);
            return Some((x.min(cols.saturating_sub(1)), rows.saturating_sub(1)));
        }
        let off = i32::try_from(vt.display_offset()).ok()?;
        let y = u16::try_from(self.cy + off).ok().filter(|y| *y < rows)?;
        Some((self.cx.min(cols.saturating_sub(1)), y))
    }

    /// Draws the selection, the position badge and the search line over
    /// the pane at `rect`.
    pub fn decorate(&self, grid: &mut Grid, rect: Rect, vt: &dyn VtBackend, theme: &Theme) {
        let off = i32::try_from(vt.display_offset()).unwrap_or(0);
        if let Some((s, e)) = self.selection() {
            for row in 0..rect.h {
                let line = i32::from(row) - off;
                if line < s.1 || line > e.1 {
                    continue;
                }
                let (from, to) = if self.line_mode {
                    (0, rect.w.saturating_sub(1))
                } else {
                    (
                        if line == s.1 { s.0 } else { 0 },
                        if line == e.1 {
                            e.0
                        } else {
                            rect.w.saturating_sub(1)
                        },
                    )
                };
                let to = to.min(rect.w.saturating_sub(1));
                if from > to {
                    continue;
                }
                let sel_bg = theme.selection.bg;
                grid.map_style(
                    Rect::new(rect.x + from, rect.y + row, to - from + 1, 1),
                    |st: Style| st.bg(sel_bg),
                );
            }
        }
        let badge = format!("[{}/{}]", off, vt.history_size());
        let bw = u16::try_from(badge.len()).unwrap_or(0);
        if rect.w > bw {
            grid.put_str(
                rect.right() - bw,
                rect.y,
                &badge,
                theme.copy_mode_badge,
                rect.right(),
            );
        }
        if let Some(s) = &self.search {
            let y = rect.bottom().saturating_sub(1);
            grid.fill(
                Rect::new(rect.x, y, rect.w, 1),
                &termos_render::cell::Cell::blank(theme.status),
            );
            let label = if s.forward { "/" } else { "?" };
            let x = grid.put_str(rect.x, y, label, theme.status_message, rect.right());
            grid.put_str(x, y, &s.text, theme.status, rect.right());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_term::vt::AlacrittyVt;

    fn vt_with(text: &str, cols: u16, rows: u16) -> AlacrittyVt {
        let mut vt = AlacrittyVt::new(cols, rows, 100);
        vt.advance(text.replace('\n', "\r\n").as_bytes());
        vt
    }

    #[test]
    fn a_range_is_selected_and_scrolled_into_view() {
        let text: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let mut vt = vt_with(&text, 20, 5);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        // "line 3" is in the history, above the screen.
        let top = -i32::try_from(vt.history_size()).expect("small");
        cm.select_range((0, top + 3), (5, top + 3), &mut vt);
        assert_eq!(cm.selected_text(&vt).as_deref(), Some("line 3"));
        assert!(cm.screen_cursor(&vt).is_some(), "scrolled into view");
    }

    fn keys(cm: &mut CopyMode, vt: &mut AlacrittyVt, keys: &str) -> CopyAction {
        let mut last = CopyAction::None;
        for spec in keys.split_whitespace() {
            last = cm.handle_key(spec.parse().expect("key"), vt).0;
        }
        last
    }

    #[test]
    fn column_slicing_handles_wide_chars() {
        assert_eq!(slice_cols("ab界cd", 1, 3), "b界");
        assert_eq!(slice_cols("ab界cd", 3, 4), "界c");
        assert_eq!(slice_cols("abc", 1, u16::MAX), "bc");
    }

    #[test]
    fn select_and_copy_across_lines() {
        let mut vt = vt_with("hello world\nsecond line\nthird", 20, 4);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        // Cursor starts at the end of "third"; go to the top-left, select
        // from "world" down one line (onto the space) and to the end of
        // the next word, as vi does.
        let action = keys(&mut cm, &mut vt, "g 0 w v j e y");
        assert_eq!(action, CopyAction::Copy("world\nsecond line".into()));
    }

    #[test]
    fn line_selection() {
        let mut vt = vt_with("one\ntwo\nthree", 10, 4);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        let action = keys(&mut cm, &mut vt, "k V k Enter");
        assert_eq!(action, CopyAction::Copy("one\ntwo".into()));
    }

    #[test]
    fn scrolls_into_history_and_back() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let mut vt = vt_with(&lines.join("\n"), 20, 5);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        keys(&mut cm, &mut vt, "g");
        assert_eq!(vt.display_offset(), vt.history_size());
        assert_eq!(vt.line_text(cm.cy), "line 0");
        keys(&mut cm, &mut vt, "G");
        assert_eq!(vt.display_offset(), 0);
        keys(&mut cm, &mut vt, "C-u");
        assert!(vt.display_offset() > 0 || cm.cy >= 0);
    }

    #[test]
    fn search_forward_backward_and_repeat() {
        let mut vt = vt_with("alpha\nbeta FOO\ngamma foo\ndelta", 20, 5);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        keys(&mut cm, &mut vt, "g");
        let (_, failed) = cm.handle_key(KeyEvent::ch('/'), &mut vt);
        assert!(!failed);
        for c in "foo".chars() {
            cm.handle_key(KeyEvent::ch(c), &mut vt);
        }
        cm.handle_key(KeyEvent::plain(KeyCode::Enter), &mut vt);
        // Lowercase query: case-insensitive, finds "FOO" first.
        assert_eq!((cm.cx, vt.line_text(cm.cy)), (5, "beta FOO".into()));
        keys(&mut cm, &mut vt, "n");
        assert_eq!((cm.cx, vt.line_text(cm.cy)), (6, "gamma foo".into()));
        keys(&mut cm, &mut vt, "N");
        assert_eq!(vt.line_text(cm.cy), "beta FOO");
        cm.handle_key(KeyEvent::ch('/'), &mut vt);
        for c in "zzz".chars() {
            cm.handle_key(KeyEvent::ch(c), &mut vt);
        }
        let (_, failed) = cm.handle_key(KeyEvent::plain(KeyCode::Enter), &mut vt);
        assert!(failed);
    }

    #[test]
    fn escape_clears_selection_then_exits() {
        let mut vt = vt_with("abc", 10, 2);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        keys(&mut cm, &mut vt, "v");
        assert!(cm.has_selection());
        assert_eq!(keys(&mut cm, &mut vt, "Escape"), CopyAction::None);
        assert!(!cm.has_selection());
        assert_eq!(keys(&mut cm, &mut vt, "Escape"), CopyAction::Exit);
        assert_eq!(keys(&mut cm, &mut vt, "q"), CopyAction::Exit);
    }

    #[test]
    fn wheel_mode_exits_at_bottom() {
        let lines: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut vt = vt_with(&lines.join("\n"), 10, 4);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        cm.exit_at_bottom = true;
        assert!(!cm.scroll(3, &mut vt));
        assert_eq!(vt.display_offset(), 3);
        assert!(cm.scroll(-3, &mut vt));
    }

    #[test]
    fn jumps_between_prompts_and_copies_a_commands_output() {
        let mut vt = AlacrittyVt::new(20, 4, 100);
        let mut session = Vec::new();
        for (cmd, out) in [("ls", "a\r\nb\r\n"), ("pwd", "/tmp\r\n"), ("true", "")] {
            session.extend_from_slice(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            session.extend_from_slice(cmd.as_bytes());
            session.extend_from_slice(b"\r\n\x1b]133;C\x07");
            session.extend_from_slice(out.as_bytes());
            session.extend_from_slice(b"\x1b]133;D;0\x07");
        }
        session.extend_from_slice(b"\x1b]133;A\x07$ ");
        vt.advance(&session);
        let mut cm = CopyMode::new(PaneId(1), &vt);
        // From the last prompt back to `true`'s, then `pwd`'s.
        keys(&mut cm, &mut vt, "[ [");
        assert_eq!(vt.line_text(cm.cy), "$ pwd");
        assert_eq!(keys(&mut cm, &mut vt, "o"), CopyAction::Copy("/tmp".into()));
        let mut cm = CopyMode::new(PaneId(1), &vt);
        keys(&mut cm, &mut vt, "g");
        assert_eq!(vt.line_text(cm.cy), "$ ls");
        assert_eq!(
            keys(&mut cm, &mut vt, "j o"),
            CopyAction::Copy("a\nb".into())
        );
        let mut cm = CopyMode::new(PaneId(1), &vt);
        keys(&mut cm, &mut vt, "g ]");
        assert_eq!(vt.line_text(cm.cy), "$ pwd");
    }
}
