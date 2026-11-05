// SPDX-License-Identifier: GPL-3.0-or-later
//! Hint mode (`pane.hints`, prefix `u`; M13a, D-118): every link, path,
//! hash or address in view gets a label of one or two letters, home row
//! first. Typing a label opens what it marks; with Shift it is copied, with
//! Ctrl pasted into the pane (as kitty's hints and WezTerm's quick select
//! do). Esc leaves.
//!
//! The labels go from the bottom of the pane up, so the newest output gets
//! the shortest reach.

use termos_core::ids::PaneId;
use termos_core::input::{KeyCode, KeyEvent, Mods};
use termos_render::cell::{Attrs, Style};
use termos_render::grid::{Grid, Rect};
use termos_render::theme::Theme;
use termos_term::links::Match;

use crate::keys::is_cancel;

/// What typing a label does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintMode {
    Open,
    /// Shift and the label.
    Copy,
    /// Ctrl and the label.
    Paste,
}

/// What the desktop does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HintAction {
    /// Waiting for more of a label.
    None,
    /// Leave hint mode.
    Exit,
    /// A label was typed: act on its match and leave.
    Act(HintMode, Match),
}

/// Labels for `n` matches from `alphabet`: one letter each while there
/// are enough letters, else all of one length (so no label starts
/// another).
pub fn labels(n: usize, alphabet: &str) -> Vec<String> {
    let letters: Vec<char> = alphabet.chars().collect();
    let k = letters.len().max(1);
    let mut len = 1usize;
    let mut room = k;
    while room < n && len < 4 {
        len += 1;
        room = room.saturating_mul(k);
    }
    (0..n.min(room))
        .map(|mut i| {
            let mut label = vec![' '; len];
            for slot in label.iter_mut().rev() {
                *slot = letters.get(i % k).copied().unwrap_or('?');
                i /= k;
            }
            label.into_iter().collect()
        })
        .collect()
}

/// Hint mode over one pane, for one client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hints {
    pub pane: PaneId,
    /// The matches and their labels, top of the pane first.
    pub marks: Vec<(String, Match)>,
    /// What has been typed of a label.
    pub typed: String,
    pub mode: HintMode,
}

impl Hints {
    /// Labels `matches` (in reading order); `None` when there are none.
    pub fn new(pane: PaneId, matches: Vec<Match>, alphabet: &str) -> Option<Self> {
        if matches.is_empty() {
            return None;
        }
        let names = labels(matches.len(), alphabet);
        // The bottom match gets the first label.
        let n = matches.len();
        let marks = matches
            .into_iter()
            .enumerate()
            .filter_map(|(i, m)| names.get(n - 1 - i).map(|l| (l.clone(), m)))
            .collect();
        Some(Self {
            pane,
            marks,
            typed: String::new(),
            mode: HintMode::Open,
        })
    }

    /// A key while hint mode is on.
    pub fn handle_key(&mut self, k: KeyEvent) -> HintAction {
        if is_cancel(k) {
            return HintAction::Exit;
        }
        if k.code == KeyCode::Backspace {
            self.typed.pop();
            if self.typed.is_empty() {
                self.mode = HintMode::Open;
            }
            return HintAction::None;
        }
        let KeyCode::Char(c) = k.code else {
            return HintAction::None;
        };
        if k.mods.intersects(Mods::ALT | Mods::SUPER) {
            return HintAction::None;
        }
        let (letter, mode) = if k.mods.contains(Mods::CTRL) {
            (c.to_ascii_lowercase(), Some(HintMode::Paste))
        } else if c.is_ascii_uppercase() {
            (c.to_ascii_lowercase(), Some(HintMode::Copy))
        } else {
            (c, None)
        };
        let mut typed = self.typed.clone();
        typed.push(letter);
        if !self.marks.iter().any(|(l, _)| l.starts_with(&typed)) {
            // Not part of any label: ignored.
            return HintAction::None;
        }
        if let Some(m) = mode {
            self.mode = m;
        }
        self.typed = typed;
        match self.marks.iter().find(|(l, _)| *l == self.typed) {
            Some((_, m)) => HintAction::Act(self.mode, m.clone()),
            None => HintAction::None,
        }
    }

    /// Draws the labels over the pane at `rect`: each match underlined,
    /// its label (what is left of it to type) over its first cells.
    pub fn draw(&self, grid: &mut Grid, rect: Rect, theme: &Theme) {
        let label_style = Style::new()
            .fg(theme.colors.status_bg)
            .bg(theme.colors.warn)
            .attrs(Attrs::BOLD);
        for (label, m) in &self.marks {
            if !label.starts_with(&self.typed) {
                continue;
            }
            for &(row, x0, x1) in &m.cells {
                if row >= rect.h || x0 >= rect.w {
                    continue;
                }
                let w = x1.min(rect.w) - x0;
                grid.map_style(Rect::new(rect.x + x0, rect.y + row, w, 1), |s| {
                    s.attrs(Attrs::UNDERLINE)
                });
            }
            let (x, y) = m.start();
            if y < rect.h && x < rect.w {
                let rest = &label[self.typed.len()..];
                grid.put_str(rect.x + x, rect.y + y, rest, label_style, rect.right());
            }
        }
        let help: &[&str] = match self.mode {
            HintMode::Open => &[
                " hints: a label opens · Shift copies · Ctrl pastes · Esc ",
                " hints · Esc ",
            ],
            HintMode::Copy => &[" hints: copy "],
            HintMode::Paste => &[" hints: paste "],
        };
        let width = |t: &str| u16::try_from(t.chars().count()).unwrap_or(u16::MAX);
        if let Some(help) = help.iter().copied().find(|t| width(*t) < rect.w)
            && rect.h > 1
        {
            let w = width(help);
            grid.put_str(
                rect.right() - w,
                rect.bottom() - 1,
                help,
                theme.copy_mode_badge,
                rect.right(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_term::links::{Kind, Target};

    fn m(text: &str, y: u16, x: u16) -> Match {
        let w = u16::try_from(text.len()).expect("short");
        Match {
            kind: Kind::Url,
            text: text.into(),
            target: Target::Url(text.into()),
            cells: vec![(y, x, x + w)],
        }
    }

    #[test]
    fn labels_are_one_letter_until_they_run_out() {
        assert_eq!(labels(3, "asd"), vec!["a", "s", "d"]);
        assert_eq!(
            labels(4, "asd"),
            vec!["aa", "as", "ad", "sa"],
            "all two letters, so none starts another"
        );
        assert_eq!(labels(0, "asd"), Vec::<String>::new());
        assert_eq!(labels(100, "as").len(), 16, "at most four letters");
    }

    #[test]
    fn the_bottom_match_gets_the_first_label() {
        let h =
            Hints::new(PaneId(1), vec![m("top", 0, 0), m("bottom", 5, 0)], "as").expect("matches");
        assert_eq!(h.marks[0].0, "s");
        assert_eq!(h.marks[1].0, "a");
        assert!(Hints::new(PaneId(1), Vec::new(), "as").is_none());
    }

    #[test]
    fn keys_pick_a_label_and_what_to_do_with_it() {
        let many: Vec<Match> = (0..4).map(|i| m("x", i, 0)).collect();
        let mut h = Hints::new(PaneId(1), many.clone(), "asd").expect("matches");
        // Labels: bottom-up aa, as, ad, sa.
        assert_eq!(h.handle_key(KeyEvent::ch('z')), HintAction::None);
        assert_eq!(h.typed, "");
        assert_eq!(h.handle_key(KeyEvent::ch('a')), HintAction::None);
        assert_eq!(
            h.handle_key(KeyEvent::ch('S')),
            HintAction::Act(HintMode::Copy, many[2].clone())
        );
        let mut h = Hints::new(PaneId(1), many.clone(), "asd").expect("matches");
        h.handle_key(KeyEvent::ctrl('s'));
        assert_eq!(
            h.handle_key(KeyEvent::ch('a')),
            HintAction::Act(HintMode::Paste, many[0].clone())
        );
        let mut h = Hints::new(PaneId(1), many, "asd").expect("matches");
        h.handle_key(KeyEvent::ch('a'));
        h.handle_key(KeyEvent::plain(KeyCode::Backspace));
        assert_eq!(h.typed, "");
        assert_eq!(
            h.handle_key(KeyEvent::plain(KeyCode::Esc)),
            HintAction::Exit
        );
    }

    #[test]
    fn labels_are_drawn_over_their_matches() {
        let h = Hints::new(
            PaneId(1),
            vec![m("https://a", 0, 2), m("https://b", 1, 0)],
            "asdf",
        )
        .expect("matches");
        let mut g = Grid::new(30, 3);
        g.put_str(2, 0, "https://a", Style::new(), 30);
        g.put_str(0, 1, "https://b", Style::new(), 30);
        let area = g.area();
        h.draw(&mut g, area, &Theme::default());
        assert!(
            g.row_text(0).starts_with("  sttps://a"),
            "{}",
            g.row_text(0)
        );
        assert!(g.row_text(1).starts_with("attps://b"), "{}", g.row_text(1));
        assert!(
            g.get(5, 0)
                .is_some_and(|c| c.style.attrs.contains(Attrs::UNDERLINE))
        );
        assert!(g.row_text(2).contains("hints"));
    }
}
