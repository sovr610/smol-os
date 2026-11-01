// SPDX-License-Identifier: GPL-3.0-or-later
//! The command palette (Ctrl+Space or prefix `:`): fuzzy search over every
//! command, showing its key binding.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatchConfig, Matcher, Utf32Str};
use termos_core::input::{KeyCode, KeyEvent};
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;

use crate::keys::{is_cancel, printable};
use crate::ui::{centered, panel, truncate, width};

/// One entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// Human description.
    pub label: String,
    /// Command text to run.
    pub command: String,
    /// Key binding, if any.
    pub hint: String,
}

/// What the palette wants after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    None,
    Close,
    Run(String),
}

/// Palette state.
pub struct Palette {
    query: String,
    items: Vec<PaletteItem>,
    matches: Vec<usize>,
    selected: usize,
    matcher: Matcher,
}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("query", &self.query)
            .field("matches", &self.matches.len())
            .finish_non_exhaustive()
    }
}

impl Palette {
    pub fn new(items: Vec<PaletteItem>) -> Self {
        let mut p = Self {
            query: String::new(),
            matches: (0..items.len()).collect(),
            items,
            selected: 0,
            matcher: Matcher::new(MatchConfig::DEFAULT),
        };
        p.refilter();
        p
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The currently highlighted item.
    pub fn selected(&self) -> Option<&PaletteItem> {
        self.matches.get(self.selected).map(|&i| &self.items[i])
    }

    /// Labels of the matching items, best first.
    pub fn matching_labels(&self) -> Vec<&str> {
        self.matches
            .iter()
            .map(|&i| self.items[i].label.as_str())
            .collect()
    }

    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.matches = (0..self.items.len()).collect();
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let hay = format!("{} {}", item.label, item.command);
                    pattern
                        .score(Utf32Str::new(&hay, &mut buf), &mut self.matcher)
                        .map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.matches = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = 0;
    }

    /// Inserts pasted text into the query.
    pub fn insert_str(&mut self, s: &str) {
        self.query.extend(s.chars().filter(|c| !c.is_control()));
        self.refilter();
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> PaletteAction {
        if is_cancel(k) {
            return PaletteAction::Close;
        }
        let n = self.matches.len();
        match k.code {
            KeyCode::Enter => {
                return self.selected().map_or(PaletteAction::Close, |i| {
                    PaletteAction::Run(i.command.clone())
                });
            }
            KeyCode::Up => {
                self.selected = self.selected.checked_sub(1).unwrap_or(n.saturating_sub(1))
            }
            KeyCode::Down | KeyCode::Tab => {
                self.selected = if n == 0 { 0 } else { (self.selected + 1) % n };
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            _ if k == KeyEvent::ctrl('p') => {
                self.selected = self.selected.checked_sub(1).unwrap_or(n.saturating_sub(1));
            }
            _ if k == KeyEvent::ctrl('n') => {
                self.selected = if n == 0 { 0 } else { (self.selected + 1) % n };
            }
            _ if k == KeyEvent::ctrl('u') => {
                self.query.clear();
                self.refilter();
            }
            _ => {
                if let Some(c) = printable(k) {
                    self.query.push(c);
                    self.refilter();
                }
            }
        }
        PaletteAction::None
    }

    /// Draws the palette centred near the top of `area`; returns the cursor
    /// position (end of the query).
    /// Where the palette box goes within `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let h = self.list_rows(area) + 4;
        let w = area.w.saturating_sub(8).clamp(20, 76);
        let mut rect = centered(area, w, h);
        rect.y = area.y + area.h.saturating_sub(h) / 4;
        rect
    }

    fn list_rows(&self, area: Rect) -> u16 {
        u16::try_from(self.items.len())
            .unwrap_or(u16::MAX)
            .min(area.h.saturating_sub(6).max(3))
            .min(14)
    }

    pub fn render(&self, grid: &mut Grid, area: Rect, theme: &Theme) -> (u16, u16) {
        let list_rows = self.list_rows(area);
        let rect = self.rect(area);
        let inner = panel(grid, rect, theme, "Command palette");
        // Query line.
        let prompt_end = grid.put_str(inner.x + 1, inner.y, "> ", theme.overlay_key, inner.right());
        let q = truncate(&self.query, inner.w.saturating_sub(4));
        let cursor_x = grid.put_str(prompt_end, inner.y, &q, theme.overlay, inner.right());
        // Separator.
        grid.hline(
            inner.x,
            inner.right(),
            inner.y + 1,
            '─',
            theme.overlay_muted,
        );
        // Results, scrolled so the selection is visible.
        let first = self
            .selected
            .saturating_sub(usize::from(list_rows).saturating_sub(1));
        for (row, &idx) in self
            .matches
            .iter()
            .skip(first)
            .take(usize::from(list_rows))
            .enumerate()
        {
            let Ok(row) = u16::try_from(row) else { break };
            let y = inner.y + 2 + row;
            let item = &self.items[idx];
            let selected = first + usize::from(row) == self.selected;
            let (style, key_style) = if selected {
                (theme.overlay_selected, theme.overlay_selected)
            } else {
                (theme.overlay, theme.overlay_key)
            };
            if selected {
                grid.set_style(Rect::new(inner.x, y, inner.w, 1), style);
            }
            let hint_w = width(&item.hint);
            let label_max = inner.w.saturating_sub(hint_w + 3);
            grid.put_str(
                inner.x + 1,
                y,
                &truncate(&item.label, label_max),
                style,
                inner.right(),
            );
            if hint_w > 0 {
                let x = inner.right().saturating_sub(hint_w + 1);
                grid.put_str(x, y, &item.hint, key_style, inner.right());
            }
        }
        if self.matches.is_empty() {
            grid.put_str(
                inner.x + 1,
                inner.y + 2,
                "no matching command",
                theme.overlay_muted,
                inner.right(),
            );
        }
        (cursor_x, inner.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<PaletteItem> {
        [
            "Split: new pane to the right|pane.split-right",
            "Zoom (maximize) the pane|pane.zoom",
            "New workspace|workspace.new",
        ]
        .iter()
        .map(|s| {
            let (label, command) = s.split_once('|').expect("pair");
            PaletteItem {
                label: label.into(),
                command: command.into(),
                hint: String::new(),
            }
        })
        .collect()
    }

    #[test]
    fn fuzzy_filters_and_runs() {
        let mut p = Palette::new(items());
        assert_eq!(p.matching_labels().len(), 3);
        for c in "zoom".chars() {
            p.handle_key(KeyEvent::ch(c));
        }
        assert_eq!(p.matching_labels(), vec!["Zoom (maximize) the pane"]);
        assert_eq!(
            p.handle_key(KeyEvent::plain(KeyCode::Enter)),
            PaletteAction::Run("pane.zoom".into())
        );
    }

    #[test]
    fn matches_command_names_too() {
        let mut p = Palette::new(items());
        p.insert_str("workspace.new");
        assert_eq!(
            p.selected().map(|i| i.command.as_str()),
            Some("workspace.new")
        );
    }

    #[test]
    fn navigation_and_cancel() {
        let mut p = Palette::new(items());
        p.handle_key(KeyEvent::plain(KeyCode::Down));
        assert_eq!(p.selected().map(|i| i.command.as_str()), Some("pane.zoom"));
        p.handle_key(KeyEvent::plain(KeyCode::Up));
        p.handle_key(KeyEvent::plain(KeyCode::Up));
        assert_eq!(
            p.selected().map(|i| i.command.as_str()),
            Some("workspace.new")
        );
        assert_eq!(
            p.handle_key(KeyEvent::plain(KeyCode::Esc)),
            PaletteAction::Close
        );
    }
}
