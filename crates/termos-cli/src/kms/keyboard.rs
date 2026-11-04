// SPDX-License-Identifier: GPL-3.0-or-later
//! Keys from libinput (evdev codes) through an XKB keymap into TermOS key
//! events, plus the console's own keys: the font size and switching to
//! another virtual terminal.

use std::time::Duration;

use termos_core::config::KmsConfig;
use termos_core::input::{KeyCode, KeyEvent, Mods};
use xkbcommon::xkb::{self, Keysym};

/// What a key press means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    Key(KeyEvent),
    FontBigger,
    FontSmaller,
    FontReset,
    /// Ctrl+Alt+F<n>.
    SwitchVt(i32),
    /// Ctrl+Shift+V or Shift+Insert: the console's clipboard into the
    /// focused window (D-105).
    Paste,
}

pub(super) struct Keyboard {
    keymap: xkb::Keymap,
    state: xkb::State,
    pub(super) delay: Duration,
    pub(super) interval: Duration,
}

/// XKB key codes are evdev codes plus 8.
const EVDEV_OFFSET: u32 = 8;

impl Keyboard {
    pub(super) fn new(cfg: &KmsConfig) -> Result<Self, String> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let options = Some(cfg.keyboard_options.clone()).filter(|o| !o.is_empty());
        let keymap = xkb::Keymap::new_from_names(
            &ctx,
            "",
            "",
            &cfg.keyboard_layout,
            &cfg.keyboard_variant,
            options,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| {
            format!(
                "no XKB keymap for layout `{}` variant `{}` (is xkeyboard-config installed?)",
                cfg.keyboard_layout, cfg.keyboard_variant
            )
        })?;
        let state = xkb::State::new(&keymap);
        Ok(Self {
            keymap,
            state,
            delay: Duration::from_millis(u64::from(cfg.repeat_delay.max(1))),
            interval: Duration::from_millis(1000 / u64::from(cfg.repeat_rate.clamp(1, 1000))),
        })
    }

    pub(super) fn mods(&self) -> Mods {
        let on = |name: &str| {
            self.state
                .mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE)
        };
        let mut m = Mods::empty();
        if on(xkb::MOD_NAME_SHIFT) {
            m |= Mods::SHIFT;
        }
        if on(xkb::MOD_NAME_CTRL) {
            m |= Mods::CTRL;
        }
        if on(xkb::MOD_NAME_ALT) {
            m |= Mods::ALT;
        }
        if on(xkb::MOD_NAME_LOGO) {
            m |= Mods::SUPER;
        }
        m
    }

    /// A key went down or up; a press gives what it means, and whether
    /// holding it repeats.
    pub(super) fn key(&mut self, evdev: u32, pressed: bool) -> Option<(Action, bool)> {
        let code = xkb::Keycode::new(evdev + EVDEV_OFFSET);
        // The symbol and modifiers as they were when the key went down.
        let sym = self.state.key_get_one_sym(code);
        let mods = self.mods();
        let utf32 = self.state.key_get_utf32(code);
        self.state.update_key(
            code,
            if pressed {
                xkb::KeyDirection::Down
            } else {
                xkb::KeyDirection::Up
            },
        );
        if !pressed {
            return None;
        }
        let action = action(sym, utf32, mods)?;
        let repeats = matches!(action, Action::Key(_)) && self.keymap.key_repeats(code);
        Some((action, repeats))
    }
}

/// The character a keysym types, ignoring Ctrl's transformation (Ctrl+B
/// is `b` with Ctrl, not U+0002).
fn character(sym: Keysym, utf32: u32) -> Option<char> {
    let c = char::from_u32(xkb::keysym_to_utf32(sym)).filter(|c| *c != '\0' && !c.is_control());
    c.or_else(|| char::from_u32(utf32).filter(|c| *c != '\0' && !c.is_control()))
}

/// What a keysym with modifiers means.
pub(super) fn action(sym: Keysym, utf32: u32, mods: Mods) -> Option<Action> {
    let raw = sym.raw();
    // XF86Switch_VT_1..12: what Ctrl+Alt+F<n> gives.
    if (0x1008_fe01..=0x1008_fe0c).contains(&raw) {
        return Some(Action::SwitchVt((raw - 0x1008_fe00) as i32));
    }
    let code = match sym {
        Keysym::Return | Keysym::KP_Enter => KeyCode::Enter,
        Keysym::Tab | Keysym::KP_Tab => KeyCode::Tab,
        Keysym::ISO_Left_Tab => KeyCode::BackTab,
        Keysym::BackSpace => KeyCode::Backspace,
        Keysym::Escape => KeyCode::Esc,
        Keysym::Left | Keysym::KP_Left => KeyCode::Left,
        Keysym::Right | Keysym::KP_Right => KeyCode::Right,
        Keysym::Up | Keysym::KP_Up => KeyCode::Up,
        Keysym::Down | Keysym::KP_Down => KeyCode::Down,
        Keysym::Home | Keysym::KP_Home => KeyCode::Home,
        Keysym::End | Keysym::KP_End => KeyCode::End,
        Keysym::Prior | Keysym::KP_Prior => KeyCode::PageUp,
        Keysym::Next | Keysym::KP_Next => KeyCode::PageDown,
        Keysym::Insert | Keysym::KP_Insert if mods == Mods::SHIFT => return Some(Action::Paste),
        Keysym::Insert | Keysym::KP_Insert => KeyCode::Insert,
        Keysym::Delete | Keysym::KP_Delete => KeyCode::Delete,
        _ if (Keysym::F1.raw()..=Keysym::F24.raw()).contains(&raw) => {
            KeyCode::F((raw - Keysym::F1.raw() + 1) as u8)
        }
        _ => {
            let c = character(sym, utf32)?;
            // The console's own: Ctrl+= / Ctrl++ / Ctrl+- / Ctrl+0 (without
            // Alt or Super, which programs may want).
            if mods & (Mods::CTRL | Mods::ALT | Mods::SUPER) == Mods::CTRL {
                match c {
                    '=' | '+' => return Some(Action::FontBigger),
                    '-' | '_' => return Some(Action::FontSmaller),
                    '0' => return Some(Action::FontReset),
                    _ => {}
                }
            }
            if mods & (Mods::CTRL | Mods::ALT | Mods::SUPER | Mods::SHIFT)
                == Mods::CTRL | Mods::SHIFT
                && c.eq_ignore_ascii_case(&'v')
            {
                return Some(Action::Paste);
            }
            KeyCode::Char(c)
        }
    };
    Some(Action::Key(KeyEvent::new(code, mods)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // evdev codes (linux/input-event-codes.h).
    const KEY_ESC: u32 = 1;
    const KEY_1: u32 = 2;
    const KEY_5: u32 = 6;
    const KEY_MINUS: u32 = 12;
    const KEY_EQUAL: u32 = 13;
    const KEY_TAB: u32 = 15;
    const KEY_B: u32 = 48;
    const KEY_ENTER: u32 = 28;
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_A: u32 = 30;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_LEFTALT: u32 = 56;
    const KEY_F2: u32 = 60;
    const KEY_V: u32 = 47;
    const KEY_INSERT: u32 = 110;
    const KEY_UP: u32 = 103;
    const KEY_LEFTSHIFT_UP: u32 = KEY_LEFTSHIFT;

    fn press(k: &mut Keyboard, code: u32) -> Option<Action> {
        let a = k.key(code, true).map(|(a, _)| a);
        k.key(code, false);
        a
    }

    fn keyboard() -> Keyboard {
        Keyboard::new(&KmsConfig::default()).expect("a US keymap (xkb-data)")
    }

    #[test]
    fn keys_become_key_events() {
        let mut k = keyboard();
        assert_eq!(press(&mut k, KEY_A), Some(Action::Key(KeyEvent::ch('a'))));
        assert_eq!(
            press(&mut k, KEY_ENTER),
            Some(Action::Key(KeyEvent::plain(KeyCode::Enter)))
        );
        assert_eq!(
            press(&mut k, KEY_ESC),
            Some(Action::Key(KeyEvent::plain(KeyCode::Esc)))
        );
        assert_eq!(
            press(&mut k, KEY_UP),
            Some(Action::Key(KeyEvent::plain(KeyCode::Up)))
        );
        // Shift: the shifted character, as the terminal client sends it.
        k.key(KEY_LEFTSHIFT, true);
        assert_eq!(press(&mut k, KEY_5), Some(Action::Key(KeyEvent::ch('%'))));
        assert_eq!(press(&mut k, KEY_A), Some(Action::Key(KeyEvent::ch('A'))));
        assert_eq!(
            press(&mut k, KEY_TAB),
            Some(Action::Key(KeyEvent::plain(KeyCode::BackTab)))
        );
        k.key(KEY_LEFTSHIFT_UP, false);
        // Ctrl+B is the prefix key: `b` with Ctrl, not a control character.
        k.key(KEY_LEFTCTRL, true);
        assert_eq!(press(&mut k, KEY_B), Some(Action::Key(KeyEvent::ctrl('b'))));
        assert_eq!(press(&mut k, KEY_EQUAL), Some(Action::FontBigger));
        assert_eq!(press(&mut k, KEY_MINUS), Some(Action::FontSmaller));
        assert_eq!(press(&mut k, KEY_1), Some(Action::Key(KeyEvent::ctrl('1'))));
        // Ctrl+Shift+V pastes the console's clipboard.
        k.key(KEY_LEFTSHIFT, true);
        assert_eq!(press(&mut k, KEY_V), Some(Action::Paste));
        k.key(KEY_LEFTSHIFT, false);
        // Ctrl+Alt+F2 switches to the second virtual terminal.
        k.key(KEY_LEFTALT, true);
        assert_eq!(press(&mut k, KEY_F2), Some(Action::SwitchVt(2)));
        k.key(KEY_LEFTALT, false);
        k.key(KEY_LEFTCTRL, false);
        // Shift+Insert pastes too; Insert alone is a key.
        k.key(KEY_LEFTSHIFT, true);
        assert_eq!(press(&mut k, KEY_INSERT), Some(Action::Paste));
        k.key(KEY_LEFTSHIFT, false);
        assert_eq!(
            press(&mut k, KEY_INSERT),
            Some(Action::Key(KeyEvent::plain(KeyCode::Insert)))
        );
        // Modifiers alone mean nothing; letters repeat, Shift does not.
        assert_eq!(k.key(KEY_LEFTSHIFT, true), None);
        k.key(KEY_LEFTSHIFT, false);
        assert_eq!(k.key(KEY_A, true).map(|(_, r)| r), Some(true));
        k.key(KEY_A, false);
        assert_eq!(
            press(&mut k, KEY_F2),
            Some(Action::Key(KeyEvent::plain(KeyCode::F(2))))
        );
    }

    #[test]
    fn layouts_and_repeat_come_from_the_config() {
        let cfg = KmsConfig {
            keyboard_layout: "de".into(),
            repeat_rate: 50,
            ..KmsConfig::default()
        };
        let mut k = Keyboard::new(&cfg).expect("a German keymap");
        // On a German keyboard the key right of T types `z`.
        const KEY_Y: u32 = 21;
        assert_eq!(press(&mut k, KEY_Y), Some(Action::Key(KeyEvent::ch('z'))));
        assert_eq!(k.interval, Duration::from_millis(20));
        let bad = KmsConfig {
            keyboard_layout: "no-such-layout".into(),
            ..KmsConfig::default()
        };
        assert!(Keyboard::new(&bad).is_err());
    }
}
