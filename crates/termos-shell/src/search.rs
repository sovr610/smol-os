// SPDX-License-Identifier: GPL-3.0-or-later
//! Search across panes (`search.all`, prefix `/`; M13a, D-118): a box like
//! the palette that finds text in every terminal pane of the session (or,
//! with `--all-sessions`, of the server), history and screen.
//!
//! - **Matching:** plain text, or a regular expression after a leading
//!   `/`; case is ignored unless the query has a capital letter.
//! - **Results:** grouped by pane, each with its line; Enter goes there in
//!   copy mode with the match selected.
//! - **How it runs:** the panes' text is copied when the box opens; each
//!   query runs on a thread over that copy, stopping at [`MAX_RESULTS`],
//!   and wakes the desktop when done.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use regex::{Regex, RegexBuilder};
use termos_core::ids::PaneId;
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use unicode_width::UnicodeWidthStr;

use crate::keys::{is_cancel, printable};
use crate::ui::{Chrome, centered, panel, truncate, width};

/// The most results one search gives.
pub const MAX_RESULTS: usize = 500;

/// One pane's text, copied when the search opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneText {
    pub session: String,
    pub pane: PaneId,
    /// How the results name it: `1:api · bash`.
    pub label: String,
    /// Its width in cells, for finding the row a match is on.
    pub cols: u16,
    /// Its lines as the program wrote them (wrapped rows joined), each with
    /// the row it starts on: 0 is the top of the screen, negative history.
    pub lines: Vec<(i32, String)>,
}

/// A match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub session: String,
    pub pane: PaneId,
    pub label: String,
    /// The row and column the match starts at, and its width in cells.
    pub row: i32,
    pub col: u16,
    pub width: u16,
    /// The line it is on, and where in it (bytes).
    pub line: String,
    pub start: usize,
    pub end: usize,
}

/// A query: plain text or a regular expression, either ignoring case.
#[derive(Debug, Clone)]
pub struct Query {
    re: Regex,
    /// The plain text, when it was not a regular expression.
    pub plain: Option<String>,
}

/// Size limit for a query's compiled form.
const REGEX_SIZE: usize = 1 << 20;

/// Parses what was typed. Empty: no query. A bad regular expression is an
/// error saying why.
pub fn parse(text: &str) -> Result<Option<Query>, String> {
    let (pattern, plain) = match text.strip_prefix('/') {
        Some(re) => (re.to_owned(), None),
        None => (regex::escape(text), Some(text.to_owned())),
    };
    let shown = plain.as_deref().unwrap_or(&text[text.len().min(1)..]);
    if shown.is_empty() {
        return Ok(None);
    }
    let fold = !shown.chars().any(char::is_uppercase);
    let re = RegexBuilder::new(&pattern)
        .case_insensitive(fold)
        .size_limit(REGEX_SIZE)
        .build()
        .map_err(|e| {
            e.to_string()
                .lines()
                .last()
                .unwrap_or("not a regular expression")
                .trim()
                .to_owned()
        })?;
    Ok(Some(Query { re, plain }))
}

/// The cells `s` takes.
fn cells(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Every match of `q` in `panes`, in their order, stopping at `limit`.
/// The flag says whether it stopped there.
pub fn search(panes: &[PaneText], q: &Query, limit: usize) -> (Vec<Hit>, bool) {
    let mut out = Vec::new();
    for p in panes {
        let cols = usize::from(p.cols.max(1));
        for (row, line) in &p.lines {
            for m in q.re.find_iter(line) {
                if m.start() == m.end() {
                    continue;
                }
                if out.len() >= limit {
                    return (out, true);
                }
                let before = cells(&line[..m.start()]);
                let down = i32::try_from(before / cols).unwrap_or(i32::MAX);
                out.push(Hit {
                    session: p.session.clone(),
                    pane: p.pane,
                    label: p.label.clone(),
                    row: row.saturating_add(down),
                    col: u16::try_from(before % cols).unwrap_or(0),
                    width: u16::try_from(cells(m.as_str())).unwrap_or(u16::MAX),
                    line: line.clone(),
                    start: m.start(),
                    end: m.end(),
                });
            }
        }
    }
    (out, false)
}

/// What the box wants after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAction {
    None,
    Close,
    /// The query changed: search again.
    Search,
    /// Go to this match.
    Go(Hit),
}

type Pending = (u64, Receiver<(Vec<Hit>, bool)>);

/// The search box of one client.
#[derive(Debug)]
pub struct SearchBox {
    pub query: String,
    pub all_sessions: bool,
    texts: Arc<Vec<PaneText>>,
    pub results: Vec<Hit>,
    /// The search stopped at [`MAX_RESULTS`].
    pub truncated: bool,
    /// Why the query is not a regular expression.
    pub error: Option<String>,
    pub selected: usize,
    /// The search running, by query number.
    pending: Option<Pending>,
    generation: u64,
}

/// A line of the list: a pane's name or one of its matches.
enum Row {
    Pane(usize),
    Hit(usize),
}

impl SearchBox {
    pub fn new(texts: Vec<PaneText>, all_sessions: bool) -> Self {
        Self {
            query: String::new(),
            all_sessions,
            texts: Arc::new(texts),
            results: Vec::new(),
            truncated: false,
            error: None,
            selected: 0,
            pending: None,
            generation: 0,
        }
    }

    /// Whether a search is still running.
    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    /// Text pasted into the query.
    pub fn insert_str(&mut self, s: &str) -> SearchAction {
        self.query.extend(s.chars().filter(|c| !c.is_control()));
        SearchAction::Search
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> SearchAction {
        if is_cancel(k) {
            return SearchAction::Close;
        }
        let n = self.results.len();
        match k.code {
            KeyCode::Enter => {
                return self
                    .results
                    .get(self.selected)
                    .cloned()
                    .map_or(SearchAction::None, SearchAction::Go);
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(n.saturating_sub(1)),
            KeyCode::Char('p') if k == KeyEvent::ctrl('p') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('n') if k == KeyEvent::ctrl('n') => {
                self.selected = (self.selected + 1).min(n.saturating_sub(1));
            }
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    return SearchAction::Search;
                }
            }
            _ => {
                if let Some(c) = printable(k) {
                    self.query.push(c);
                    return SearchAction::Search;
                }
            }
        }
        SearchAction::None
    }

    /// Starts searching for the query on a thread; `wake` runs when it is
    /// done. An empty query clears the results at once.
    pub fn start(&mut self, wake: impl FnOnce() + Send + 'static) {
        self.generation += 1;
        self.selected = 0;
        self.error = None;
        let q = match parse(&self.query) {
            Ok(Some(q)) => q,
            Ok(None) => {
                self.results.clear();
                self.truncated = false;
                self.pending = None;
                return;
            }
            Err(e) => {
                self.results.clear();
                self.truncated = false;
                self.error = Some(e);
                self.pending = None;
                return;
            }
        };
        let (tx, rx) = channel();
        let texts = Arc::clone(&self.texts);
        let run = move || {
            let _ = tx.send(search(&texts, &q, MAX_RESULTS));
            wake();
        };
        match std::thread::Builder::new()
            .name("pane-search".into())
            .spawn(run)
        {
            Ok(_) => self.pending = Some((self.generation, rx)),
            Err(e) => {
                tracing::warn!("cannot start a thread to search: {e}");
                self.error = Some("cannot search now".into());
            }
        }
    }

    /// Takes in a finished search; whether anything changed.
    pub fn poll(&mut self) -> bool {
        let Some((generation, rx)) = &self.pending else {
            return false;
        };
        match rx.try_recv() {
            Ok((results, truncated)) => {
                if *generation == self.generation {
                    self.results = results;
                    self.truncated = truncated;
                    self.selected = 0;
                }
                self.pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                true
            }
        }
    }

    /// The list: each pane's name before its first match.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut last: Option<(&str, PaneId)> = None;
        for (i, h) in self.results.iter().enumerate() {
            if last != Some((h.session.as_str(), h.pane)) {
                rows.push(Row::Pane(i));
                last = Some((h.session.as_str(), h.pane));
            }
            rows.push(Row::Hit(i));
        }
        rows
    }

    fn list_rows(area: Rect) -> u16 {
        area.h.saturating_sub(6).clamp(3, 16)
    }

    pub fn rect(area: Rect) -> Rect {
        let h = Self::list_rows(area) + 5;
        let w = area.w.saturating_sub(6).clamp(24, 96);
        let mut rect = centered(area, w, h);
        rect.y = area.y + area.h.saturating_sub(h) / 4;
        rect
    }

    /// Draws the box; returns where the cursor goes.
    pub fn render(&self, grid: &mut Grid, area: Rect, theme: &Theme, chrome: Chrome) -> (u16, u16) {
        let rect = Self::rect(area);
        let title = if self.all_sessions {
            "Search every session"
        } else {
            "Search panes"
        };
        let inner = panel(grid, rect, area, theme, title, chrome);
        let prompt_end = grid.put_str(inner.x + 1, inner.y, "/ ", theme.overlay_key, inner.right());
        let q = truncate(&self.query, inner.w.saturating_sub(4));
        let cursor_x = grid.put_str(prompt_end, inner.y, &q, theme.overlay, inner.right());
        grid.hline(
            inner.x,
            inner.right(),
            inner.y + 1,
            '─',
            theme.overlay_muted,
        );
        let list = Self::list_rows(area);
        let rows = self.rows();
        let sel_row = rows
            .iter()
            .position(|r| matches!(r, Row::Hit(i) if *i == self.selected))
            .unwrap_or(0);
        let first = sel_row.saturating_sub(usize::from(list).saturating_sub(1));
        for (n, row) in rows.iter().skip(first).take(usize::from(list)).enumerate() {
            let Ok(n) = u16::try_from(n) else { break };
            let y = inner.y + 2 + n;
            match row {
                Row::Pane(i) => {
                    let h = &self.results[*i];
                    let name = if self.all_sessions {
                        format!("{} · {}", h.session, h.label)
                    } else {
                        h.label.clone()
                    };
                    grid.put_str(
                        inner.x + 1,
                        y,
                        &truncate(&name, inner.w.saturating_sub(2)),
                        theme.overlay_title,
                        inner.right(),
                    );
                }
                Row::Hit(i) => self.draw_hit(grid, inner, y, *i, theme),
            }
        }
        let footer = if let Some(e) = &self.error {
            format!("not a regular expression: {e}")
        } else if self.searching() {
            "searching…".to_owned()
        } else if self.query.is_empty() {
            "text, or /regex · Enter goes there · Esc".to_owned()
        } else if self.results.is_empty() {
            "no match".to_owned()
        } else if self.truncated {
            format!("the first {MAX_RESULTS} matches")
        } else {
            let n = self.results.len();
            format!("{n} match{}", if n == 1 { "" } else { "es" })
        };
        let y = inner.bottom().saturating_sub(1);
        grid.put_str(
            inner.x + 1,
            y,
            &truncate(&footer, inner.w.saturating_sub(2)),
            theme.overlay_muted,
            inner.right(),
        );
        (cursor_x, inner.y)
    }

    fn draw_hit(&self, grid: &mut Grid, inner: Rect, y: u16, i: usize, theme: &Theme) {
        let h = &self.results[i];
        let selected = i == self.selected;
        let style = if selected {
            theme.overlay_selected
        } else {
            theme.overlay
        };
        if selected {
            grid.set_style(Rect::new(inner.x, y, inner.w, 1), style);
        }
        let number = format!("{:>6} ", h.row);
        let x = grid.put_str(inner.x + 1, y, &number, theme.overlay_muted, inner.right());
        // The line from a little before the match, so the match shows.
        let room = inner.right().saturating_sub(x + 1);
        let lead = h.line[..h.start]
            .char_indices()
            .rev()
            .nth(12)
            .map_or(0, |(b, _)| b);
        let from = if width(&h.line[..h.end]) <= room {
            0
        } else {
            lead
        };
        let before = h.line[from..h.start].trim_start();
        let x = grid.put_str(x, y, before, style, inner.right());
        let hit_style =
            style.attrs(termos_render::cell::Attrs::BOLD | termos_render::cell::Attrs::UNDERLINE);
        let x = grid.put_str(x, y, &h.line[h.start..h.end], hit_style, inner.right());
        grid.put_str(x, y, &h.line[h.end..], style, inner.right());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u64, cols: u16, lines: &[(i32, &str)]) -> PaneText {
        PaneText {
            session: "main".into(),
            pane: PaneId(id),
            label: format!("0:ws · pane {id}"),
            cols,
            lines: lines.iter().map(|(r, l)| (*r, (*l).to_owned())).collect(),
        }
    }

    fn found(q: &str, panes: &[PaneText]) -> Vec<(u64, i32, u16, u16)> {
        let q = parse(q).expect("valid").expect("a query");
        search(panes, &q, MAX_RESULTS)
            .0
            .into_iter()
            .map(|h| (h.pane.0, h.row, h.col, h.width))
            .collect()
    }

    #[test]
    fn plain_text_ignores_case_unless_it_has_capitals() {
        let panes = [
            pane(1, 80, &[(-2, "Error: disk full"), (0, "no error here")]),
            pane(2, 80, &[(3, "ERROR again")]),
        ];
        assert_eq!(
            found("error", &panes),
            vec![(1, -2, 0, 5), (1, 0, 3, 5), (2, 3, 0, 5)]
        );
        assert_eq!(found("Error", &panes), vec![(1, -2, 0, 5)]);
        // Regex characters are plain text.
        assert!(found("e.r", &panes).is_empty());
    }

    #[test]
    fn a_leading_slash_is_a_regular_expression() {
        let panes = [pane(1, 80, &[(0, "took 12ms then 345ms")])];
        assert_eq!(
            found("/[0-9]+ms", &panes),
            vec![(1, 0, 5, 4), (1, 0, 15, 5)]
        );
        assert!(parse("/(").is_err());
        assert!(parse("").expect("empty").is_none());
        assert!(parse("/").expect("empty").is_none());
    }

    #[test]
    fn wrapped_lines_find_their_row() {
        // A 10-column pane: the match starts on the line's third row.
        let panes = [pane(1, 10, &[(4, "aaaaaaaaaabbbbbbbbbb界 needle")])];
        assert_eq!(found("needle", &panes), vec![(1, 6, 3, 6)]);
    }

    #[test]
    fn it_stops_at_the_limit() {
        let lines: Vec<(i32, String)> = (0..10).map(|i| (i, "x x x".to_owned())).collect();
        let p = PaneText {
            lines,
            ..pane(1, 80, &[])
        };
        let q = parse("x").expect("valid").expect("query");
        let (hits, more) = search(&[p], &q, 7);
        assert_eq!(hits.len(), 7);
        assert!(more);
    }

    #[test]
    fn keys_edit_the_query_and_move_the_selection() {
        let mut b = SearchBox::new(vec![pane(1, 80, &[(0, "a b a")])], false);
        assert_eq!(b.handle_key(KeyEvent::ch('a')), SearchAction::Search);
        let (done_tx, done_rx) = channel();
        b.start(move || {
            let _ = done_tx.send(());
        });
        done_rx.recv().expect("the search finished");
        assert!(b.poll());
        assert_eq!(b.results.len(), 2);
        assert_eq!(
            b.handle_key(KeyEvent::plain(KeyCode::Down)),
            SearchAction::None
        );
        assert_eq!(b.selected, 1);
        match b.handle_key(KeyEvent::plain(KeyCode::Enter)) {
            SearchAction::Go(h) => assert_eq!(h.col, 4),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            b.handle_key(KeyEvent::plain(KeyCode::Backspace)),
            SearchAction::Search
        );
        b.start(|| {});
        assert!(b.results.is_empty());
        assert_eq!(
            b.handle_key(KeyEvent::plain(KeyCode::Esc)),
            SearchAction::Close
        );
    }

    #[test]
    fn results_are_drawn_grouped_by_pane() {
        let mut b = SearchBox::new(
            vec![
                pane(1, 80, &[(0, "cargo build"), (1, "cargo test")]),
                pane(2, 80, &[(-5, "cargo run")]),
            ],
            false,
        );
        b.query = "cargo".into();
        let q = parse(&b.query).expect("valid").expect("query");
        b.results = search(&b.texts, &q, MAX_RESULTS).0;
        let mut g = Grid::new(80, 30);
        let area = g.area();
        b.render(&mut g, area, &Theme::default(), Chrome::default());
        let text = g.to_text();
        let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in\n{text}"));
        assert!(at("0:ws · pane 1") < at("cargo build"));
        assert!(at("cargo test") < at("0:ws · pane 2"));
        assert!(at("0:ws · pane 2") < at("cargo run"));
        assert!(text.contains("3 matches"));
    }
}
