// SPDX-License-Identifier: GPL-3.0-or-later
//! Transport-neutral input events and tmux-style key notation.
//!
//! Key notation follows tmux: `C-b` (Ctrl), `M-Left` (Alt/Meta), `S-Up`
//! (Shift), named keys such as `Enter`, `Escape`, `Space`, `Tab`, `BTab`,
//! `BSpace`, `PPage`/`PageUp`, `NPage`/`PageDown`, `Home`, `End`, `IC`
//! (Insert), `DC` (Delete), `F1`–`F12`, and any single character (`%`, `"`).

use std::fmt;
use std::str::FromStr;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Modifier keys.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const CTRL = 1 << 1;
        const ALT = 1 << 2;
        const SUPER = 1 << 3;
    }
}

/// A key, independent of the terminal protocol it arrived in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

/// A key press with modifiers.
///
/// Invariant (see [`KeyEvent::new`]): for `Char` keys the shift state is
/// already part of the character (`'%'`, `'N'`), so `SHIFT` is never set on
/// them, and Ctrl-letters are lowercase. Two events that mean the same key
/// therefore compare equal regardless of how the terminal reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Mods,
}

impl KeyEvent {
    /// Builds a normalized key event.
    pub fn new(code: KeyCode, mods: Mods) -> Self {
        let mut mods = mods;
        let code = match code {
            KeyCode::Char(c) => {
                mods.remove(Mods::SHIFT);
                if mods.contains(Mods::CTRL) && c.is_ascii_uppercase() {
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            KeyCode::BackTab => {
                mods.remove(Mods::SHIFT);
                KeyCode::BackTab
            }
            other => other,
        };
        Self { code, mods }
    }

    pub fn plain(code: KeyCode) -> Self {
        Self::new(code, Mods::empty())
    }

    pub fn ch(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }

    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), Mods::CTRL)
    }
}

/// Error for an invalid key specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySpecError {
    pub spec: String,
    pub reason: &'static str,
}

impl fmt::Display for KeySpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid key `{}`: {}", self.spec, self.reason)
    }
}

impl std::error::Error for KeySpecError {}

const NAMED: &[(&str, KeyCode)] = &[
    ("Enter", KeyCode::Enter),
    ("Escape", KeyCode::Esc),
    ("Esc", KeyCode::Esc),
    ("Space", KeyCode::Char(' ')),
    ("Tab", KeyCode::Tab),
    ("BTab", KeyCode::BackTab),
    ("BSpace", KeyCode::Backspace),
    ("Backspace", KeyCode::Backspace),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("PPage", KeyCode::PageUp),
    ("PageUp", KeyCode::PageUp),
    ("NPage", KeyCode::PageDown),
    ("PageDown", KeyCode::PageDown),
    ("IC", KeyCode::Insert),
    ("Insert", KeyCode::Insert),
    ("DC", KeyCode::Delete),
    ("Delete", KeyCode::Delete),
];

impl FromStr for KeyEvent {
    type Err = KeySpecError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let err = |reason| KeySpecError {
            spec: spec.to_owned(),
            reason,
        };
        if spec.is_empty() {
            return Err(err("empty"));
        }
        let mut mods = Mods::empty();
        let mut rest = spec;
        // Modifier prefixes; a lone "-" or "C--" (Ctrl+minus) must survive.
        loop {
            let bytes = rest.as_bytes();
            if bytes.len() > 2 && bytes[1] == b'-' {
                let m = match bytes[0] {
                    b'C' => Mods::CTRL,
                    b'M' => Mods::ALT,
                    b'S' => Mods::SHIFT,
                    b'D' => Mods::SUPER,
                    _ => break,
                };
                if mods.contains(m) {
                    return Err(err("modifier repeated"));
                }
                mods |= m;
                rest = &rest[2..];
            } else {
                break;
            }
        }
        let mut chars = rest.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyCode::Char(c),
            _ => {
                if let Some((_, code)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(rest)) {
                    *code
                } else if let Some(n) = rest.strip_prefix('F').or_else(|| rest.strip_prefix('f')) {
                    match n.parse::<u8>() {
                        Ok(n @ 1..=24) => KeyCode::F(n),
                        _ => return Err(err("function keys are F1 to F24")),
                    }
                } else {
                    return Err(err("unknown key name"));
                }
            }
        };
        if let KeyCode::Char(c) = code
            && c.is_control()
        {
            return Err(err("control characters are written as C-<key>"));
        }
        Ok(KeyEvent::new(code, mods))
    }
}

impl fmt::Display for KeyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (m, p) in [
            (Mods::CTRL, "C-"),
            (Mods::ALT, "M-"),
            (Mods::SHIFT, "S-"),
            (Mods::SUPER, "D-"),
        ] {
            if self.mods.contains(m) {
                f.write_str(p)?;
            }
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            code => {
                let name = NAMED
                    .iter()
                    .find(|(_, c)| *c == code)
                    .map_or("?", |(n, _)| *n);
                f.write_str(name)
            }
        }
    }
}

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// What happened with the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

/// A mouse event at a cell position (0-based, screen coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub x: u16,
    pub y: u16,
    pub mods: Mods,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> KeyEvent {
        s.parse().expect("valid key spec")
    }

    #[test]
    fn parses_tmux_notation() {
        assert_eq!(k("C-b"), KeyEvent::ctrl('b'));
        assert_eq!(k("C-B"), KeyEvent::ctrl('b'));
        assert_eq!(k("%"), KeyEvent::ch('%'));
        assert_eq!(k("\""), KeyEvent::ch('"'));
        assert_eq!(k("-"), KeyEvent::ch('-'));
        assert_eq!(k("C--"), KeyEvent::ctrl('-'));
        assert_eq!(k("N"), KeyEvent::ch('N'));
        assert_eq!(k("C-Space"), KeyEvent::ctrl(' '));
        assert_eq!(k("M-Left"), KeyEvent::new(KeyCode::Left, Mods::ALT));
        assert_eq!(
            k("C-M-Up"),
            KeyEvent::new(KeyCode::Up, Mods::CTRL | Mods::ALT)
        );
        assert_eq!(k("S-Tab"), KeyEvent::plain(KeyCode::Tab).with_shift());
        assert_eq!(k("BTab"), KeyEvent::plain(KeyCode::BackTab));
        assert_eq!(k("F12"), KeyEvent::plain(KeyCode::F(12)));
        assert_eq!(k("PPage"), KeyEvent::plain(KeyCode::PageUp));
        assert_eq!(k("escape"), KeyEvent::plain(KeyCode::Esc));
    }

    #[test]
    fn rejects_bad_specs() {
        for bad in ["", "C-", "Foo", "F0", "F99", "C-C-a", "\u{7}"] {
            assert!(bad.parse::<KeyEvent>().is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "C-b", "%", "C-Space", "M-Left", "C-M-Up", "F5", "Enter", "BTab", "N", "Space",
        ] {
            assert_eq!(k(s).to_string(), s);
            assert_eq!(k(&k(s).to_string()), k(s));
        }
    }

    #[test]
    fn normalization_makes_equivalent_reports_equal() {
        // A terminal may report '%' as Shift+'%' and Ctrl+B as Ctrl+'B'.
        assert_eq!(
            KeyEvent::new(KeyCode::Char('%'), Mods::SHIFT),
            KeyEvent::ch('%')
        );
        assert_eq!(
            KeyEvent::new(KeyCode::Char('B'), Mods::CTRL | Mods::SHIFT),
            KeyEvent::ctrl('b')
        );
    }

    impl KeyEvent {
        fn with_shift(mut self) -> Self {
            self.mods |= Mods::SHIFT;
            self
        }
    }
}
