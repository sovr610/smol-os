// SPDX-License-Identifier: GPL-3.0-or-later
//! The console's lock screen (M13d, D-123, D-124).
//!
//! Drawn full screen over nothing of the desktop: the clock, the date and
//! a password field. While it shows, the console sends the server no input
//! and does not switch virtual terminals. The password is the system
//! user's, checked by `sudo termos system check-password`; after three
//! wrong ones each try waits longer.
//!
//! The same screen asks for a password at first boot (typed twice; Esc
//! skips, and the lock then says that anyone can unlock).
//!
//! This module is the state and the drawing; the console runs the checks.

use std::time::{Duration, Instant};

use secrecy::{ExposeSecret as _, SecretString};
use termos_core::input::{KeyCode, KeyEvent, Mods};
use termos_render::cell::{Cell, Style};
use termos_render::color::Color;
use termos_render::grid::{Grid, Rect};

/// The longest password the field takes.
const MAX_INPUT: usize = 256;

/// What the screen is for.
#[derive(Debug)]
pub(super) enum Mode {
    /// Locked: asking for the password.
    Unlock,
    /// First boot: choosing a password; `first` is the first of the two
    /// entries once typed.
    Choose { first: Option<SecretString> },
}

/// What the console must do after a key.
#[derive(Debug)]
pub(super) enum Step {
    Nothing,
    /// Draw again.
    Redraw,
    /// Check this password.
    Check(SecretString),
    /// Save this password (chosen twice).
    Save(SecretString),
    /// First boot: no password.
    Skip,
}

pub(super) struct Lock {
    pub(super) mode: Mode,
    input: SecretString,
    /// Shown under the field: a wrong password, a wait, a mismatch.
    pub(super) message: Option<String>,
    failures: u32,
    /// After repeated failures: no tries before this.
    wait_until: Option<Instant>,
    /// A check is running.
    pub(super) checking: bool,
    /// Whether the user has a password (asked when the lock starts).
    pub(super) has_password: Option<bool>,
}

/// How long to wait after `failures` wrong passwords: nothing for the
/// first three, then 5 s, doubling, at most five minutes.
pub(super) fn wait_after(failures: u32) -> Option<Duration> {
    if failures < 3 {
        return None;
    }
    let secs = 5u64.saturating_mul(1 << (failures - 3).min(6));
    Some(Duration::from_secs(secs.min(300)))
}

impl Lock {
    pub(super) fn new(mode: Mode) -> Self {
        Self {
            mode,
            input: SecretString::from(String::new()),
            message: None,
            failures: 0,
            wait_until: None,
            checking: false,
            has_password: None,
        }
    }

    fn set_input(&mut self, s: String) {
        self.input = SecretString::from(s);
    }

    /// How many characters are typed (the field shows that many dots).
    pub(super) fn typed(&self) -> usize {
        self.input.expose_secret().chars().count()
    }

    /// When the screen must be drawn again by itself: the end of a wait.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.wait_until
    }

    /// The wait is over.
    pub(super) fn on_timer(&mut self, now: Instant) -> bool {
        if self.wait_until.is_some_and(|t| t <= now) {
            self.wait_until = None;
            self.message = None;
            return true;
        }
        false
    }

    /// A key on the lock screen.
    pub(super) fn key(&mut self, k: KeyEvent, now: Instant) -> Step {
        if self.checking {
            return Step::Nothing;
        }
        let plain = k.mods & (Mods::CTRL | Mods::ALT | Mods::SUPER) == Mods::empty();
        match k.code {
            KeyCode::Char(c) if plain => {
                let mut s = self.input.expose_secret().to_owned();
                if s.len() + c.len_utf8() <= MAX_INPUT {
                    s.push(c);
                }
                self.set_input(s);
                Step::Redraw
            }
            KeyCode::Char('u') if k.mods == Mods::CTRL => {
                self.set_input(String::new());
                Step::Redraw
            }
            KeyCode::Backspace => {
                let mut s = self.input.expose_secret().to_owned();
                s.pop();
                self.set_input(s);
                Step::Redraw
            }
            KeyCode::Esc => match self.mode {
                Mode::Choose { .. } => Step::Skip,
                Mode::Unlock => {
                    self.set_input(String::new());
                    Step::Redraw
                }
            },
            KeyCode::Enter => self.enter(now),
            _ => Step::Nothing,
        }
    }

    fn enter(&mut self, now: Instant) -> Step {
        let typed = std::mem::replace(&mut self.input, SecretString::from(String::new()));
        match &mut self.mode {
            Mode::Unlock => {
                if let Some(t) = self.wait_until
                    && t > now
                {
                    return Step::Redraw;
                }
                self.checking = true;
                self.message = Some("Checking…".into());
                Step::Check(typed)
            }
            Mode::Choose { first } => {
                if typed.expose_secret().is_empty() {
                    self.message = Some("Type a password, or press Esc to have none.".into());
                    return Step::Redraw;
                }
                match first.take() {
                    None => {
                        *first = Some(typed);
                        self.message = None;
                        Step::Redraw
                    }
                    Some(a) if a.expose_secret() == typed.expose_secret() => Step::Save(typed),
                    Some(_) => {
                        self.message = Some("The two differ: type it again.".into());
                        Step::Redraw
                    }
                }
            }
        }
    }

    /// The check's answer: `false` counts a failure.
    pub(super) fn checked(&mut self, ok: bool, now: Instant) {
        self.checking = false;
        if ok {
            self.failures = 0;
            self.message = None;
            return;
        }
        self.failures += 1;
        self.wait_until = wait_after(self.failures).map(|d| now + d);
        self.message = Some(match self.wait_until {
            Some(t) => format!(
                "Wrong password. Wait {} s before the next try.",
                t.saturating_duration_since(now).as_secs()
            ),
            None => "Wrong password.".into(),
        });
    }

    /// Draws the screen into `grid` (the console's size). `time` and
    /// `date` are the clock's text.
    pub(super) fn draw(&self, grid: &mut Grid, time: &str, date: &str) {
        let (cols, rows) = (grid.cols(), grid.rows());
        let plain = Style::new();
        let dim = Style::new().fg(Color::Indexed(8));
        let accent = Style::new().fg(Color::Indexed(12)).bold();
        let warn = Style::new().fg(Color::Indexed(11));
        grid.fill(Rect::new(0, 0, cols, rows), &Cell::blank(plain));
        let mut lines: Vec<(String, Style)> = vec![
            (time.to_owned(), accent),
            (date.to_owned(), dim),
            (String::new(), plain),
        ];
        match &self.mode {
            Mode::Unlock => {
                lines.push(("Locked".into(), plain));
                lines.push((String::new(), plain));
                let dots = "●".repeat(self.typed().min(32));
                lines.push((format!("Password: {dots}▏"), plain));
                if self.has_password == Some(false) {
                    lines.push((String::new(), plain));
                    lines.push((
                        "No password is set: Enter unlocks. Set one with `termos password`.".into(),
                        warn,
                    ));
                }
            }
            Mode::Choose { first } => {
                lines.push(("Choose a password for the lock screen".into(), plain));
                lines.push((
                    "It is the system user's password, asked when TermOS locks.".into(),
                    dim,
                ));
                lines.push((String::new(), plain));
                let label = if first.is_some() {
                    "Again:    "
                } else {
                    "Password: "
                };
                let dots = "●".repeat(self.typed().min(32));
                lines.push((format!("{label}{dots}▏"), plain));
                lines.push((String::new(), plain));
                lines.push(("Enter: next   Esc: no password".into(), dim));
            }
        }
        if let Some(m) = &self.message {
            lines.push((String::new(), plain));
            lines.push((m.clone(), warn));
        }
        let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let top = rows.saturating_sub(n) / 2;
        for (i, (text, style)) in lines.iter().enumerate() {
            let Ok(i) = u16::try_from(i) else { break };
            let y = top + i;
            if y >= rows {
                break;
            }
            let w = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            let x = cols.saturating_sub(w) / 2;
            grid.put_str(x, y, text, *style, cols);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> KeyEvent {
        KeyEvent::ch(c)
    }

    fn enter() -> KeyEvent {
        KeyEvent::plain(KeyCode::Enter)
    }

    fn row(g: &Grid, y: u16) -> String {
        g.row(y)
            .iter()
            .map(Cell::display_symbol)
            .collect::<String>()
            .trim()
            .to_owned()
    }

    #[test]
    fn waits_grow_after_three_failures() {
        assert_eq!(wait_after(2), None);
        assert_eq!(wait_after(3), Some(Duration::from_secs(5)));
        assert_eq!(wait_after(4), Some(Duration::from_secs(10)));
        assert_eq!(wait_after(7), Some(Duration::from_secs(80)));
        assert_eq!(wait_after(50), Some(Duration::from_secs(300)));
    }

    #[test]
    fn typing_checking_and_waiting() {
        let now = Instant::now();
        let mut l = Lock::new(Mode::Unlock);
        for c in "pw".chars() {
            assert!(matches!(l.key(ch(c), now), Step::Redraw));
        }
        assert_eq!(l.typed(), 2);
        assert!(matches!(
            l.key(KeyEvent::plain(KeyCode::Backspace), now),
            Step::Redraw
        ));
        assert!(matches!(l.key(ch('x'), now), Step::Redraw));
        let Step::Check(given) = l.key(enter(), now) else {
            panic!("a check");
        };
        assert_eq!(given.expose_secret(), "px");
        assert_eq!(l.typed(), 0, "the field empties");
        // Keys wait for the answer.
        assert!(matches!(l.key(ch('a'), now), Step::Nothing));
        l.checked(false, now);
        assert_eq!(l.message.as_deref(), Some("Wrong password."));
        for _ in 0..2 {
            assert!(matches!(l.key(enter(), now), Step::Check(_)));
            l.checked(false, now);
        }
        // The third failure: a wait.
        assert_eq!(l.deadline(), Some(now + Duration::from_secs(5)));
        assert!(
            matches!(l.key(enter(), now), Step::Redraw),
            "no try while waiting"
        );
        assert!(l.on_timer(now + Duration::from_secs(5)));
        assert!(matches!(
            l.key(enter(), now + Duration::from_secs(5)),
            Step::Check(_)
        ));
        l.checked(true, now);
        assert_eq!(l.message, None);
        // Ctrl keys type nothing.
        assert!(matches!(l.key(KeyEvent::ctrl('a'), now), Step::Nothing));
    }

    #[test]
    fn choosing_a_password_at_first_boot() {
        let now = Instant::now();
        let mut l = Lock::new(Mode::Choose { first: None });
        assert!(matches!(l.key(enter(), now), Step::Redraw), "empty");
        for c in "abc".chars() {
            l.key(ch(c), now);
        }
        assert!(matches!(l.key(enter(), now), Step::Redraw));
        for c in "abd".chars() {
            l.key(ch(c), now);
        }
        assert!(matches!(l.key(enter(), now), Step::Redraw));
        assert_eq!(l.message.as_deref(), Some("The two differ: type it again."));
        // After a mismatch both are typed again.
        for c in "abc".chars() {
            l.key(ch(c), now);
        }
        assert!(matches!(l.key(enter(), now), Step::Redraw));
        for c in "abc".chars() {
            l.key(ch(c), now);
        }
        let Step::Save(p) = l.key(enter(), now) else {
            panic!("saved");
        };
        assert_eq!(p.expose_secret(), "abc");
    }

    #[test]
    fn the_screen_shows_dots_never_the_password() {
        let now = Instant::now();
        let mut l = Lock::new(Mode::Unlock);
        for c in "secret".chars() {
            l.key(ch(c), now);
        }
        let mut g = Grid::new(80, 24);
        l.draw(&mut g, "14:05", "Thursday 5 November");
        let all: String = (0..24).map(|y| row(&g, y)).collect::<Vec<_>>().join("\n");
        assert!(all.contains("14:05") && all.contains("Locked"), "{all}");
        assert!(all.contains("Password: ●●●●●●"), "{all}");
        assert!(!all.contains("secret"));
        l.has_password = Some(false);
        l.draw(&mut g, "14:05", "");
        let all: String = (0..24).map(|y| row(&g, y)).collect::<Vec<_>>().join("\n");
        assert!(all.contains("No password is set"), "{all}");
        let mut c = Lock::new(Mode::Choose { first: None });
        assert!(matches!(
            c.key(KeyEvent::plain(KeyCode::Esc), now),
            Step::Skip
        ));
    }
}
