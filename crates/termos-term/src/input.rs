// SPDX-License-Identifier: GPL-3.0-or-later
//! Encoding desktop input for a child program, the way xterm would send it.

use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};

use crate::vt::VtModes;

/// xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(mods: Mods) -> u8 {
    1 + u8::from(mods.contains(Mods::SHIFT))
        + 2 * u8::from(mods.contains(Mods::ALT))
        + 4 * u8::from(mods.contains(Mods::CTRL))
}

/// The bytes a key press sends to a program in legacy (xterm) encoding.
pub fn encode_key(key: KeyEvent, modes: &VtModes) -> Vec<u8> {
    let mods = key.mods;
    let has_mods = mods.intersects(Mods::SHIFT | Mods::ALT | Mods::CTRL);
    let csi_mod = |final_byte: char| -> Vec<u8> {
        format!("\x1b[1;{}{final_byte}", modifier_param(mods)).into_bytes()
    };
    let cursor = |final_byte: char| -> Vec<u8> {
        if has_mods {
            csi_mod(final_byte)
        } else if modes.app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if has_mods {
            format!("\x1b[{n};{}~", modifier_param(mods)).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt_prefix = |mut bytes: Vec<u8>| -> Vec<u8> {
        if mods.contains(Mods::ALT) {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    match key.code {
        KeyCode::Char(c) => {
            let bytes = if mods.contains(Mods::CTRL) {
                ctrl_byte(c).map_or_else(|| c.to_string().into_bytes(), |b| vec![b])
            } else {
                c.to_string().into_bytes()
            };
            alt_prefix(bytes)
        }
        KeyCode::Enter => alt_prefix(vec![b'\r']),
        KeyCode::Tab => {
            if mods.contains(Mods::SHIFT) {
                b"\x1b[Z".to_vec()
            } else {
                alt_prefix(vec![b'\t'])
            }
        }
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            let b = if mods.contains(Mods::CTRL) {
                0x08
            } else {
                0x7f
            };
            alt_prefix(vec![b])
        }
        KeyCode::Esc => alt_prefix(vec![0x1b]),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let final_byte = char::from(b'P' + (n - 1));
            if has_mods {
                csi_mod(final_byte)
            } else {
                format!("\x1bO{final_byte}").into_bytes()
            }
        }
        KeyCode::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                13 => 25,
                14 => 26,
                15 => 28,
                16 => 29,
                17 => 31,
                18 => 32,
                19 => 33,
                20 => 34,
                _ => return Vec::new(),
            };
            tilde(code)
        }
    }
}

// TODO(M4): Kitty keyboard protocol encoding for child programs that enable it (CSI > u).

/// The control byte for Ctrl+`c`, as xterm sends it.
fn ctrl_byte(c: char) -> Option<u8> {
    let b = u8::try_from(u32::from(c)).ok()?;
    match b {
        b'a'..=b'z' => Some(b - b'a' + 1),
        b'@' | b' ' | b'2' => Some(0),
        b'[' | b'3' => Some(0x1b),
        b'\\' | b'4' => Some(0x1c),
        b']' | b'5' => Some(0x1d),
        b'^' | b'6' => Some(0x1e),
        b'_' | b'/' | b'7' | b'-' => Some(0x1f),
        b'?' | b'8' => Some(0x7f),
        _ => None,
    }
}

/// The bytes a mouse event sends to a program that enabled mouse reporting,
/// or `None` if the program did not ask for this kind of event. `event`
/// coordinates must be relative to the pane.
pub fn encode_mouse(event: MouseEvent, modes: &VtModes) -> Option<Vec<u8>> {
    if !modes.mouse_report {
        return None;
    }
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0u16,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match event.kind {
        MouseKind::Down(b) => (button_code(b), false),
        MouseKind::Up(b) => (button_code(b), true),
        MouseKind::Drag(b) if modes.mouse_drag => (button_code(b) + 32, false),
        MouseKind::Moved if modes.mouse_motion => (3 + 32, false),
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
        MouseKind::ScrollLeft => (66, false),
        MouseKind::ScrollRight => (67, false),
        MouseKind::Drag(_) | MouseKind::Moved => return None,
    };
    if event.mods.contains(Mods::SHIFT) {
        code += 4;
    }
    if event.mods.contains(Mods::ALT) {
        code += 8;
    }
    if event.mods.contains(Mods::CTRL) {
        code += 16;
    }
    let (x, y) = (u32::from(event.x) + 1, u32::from(event.y) + 1);
    if modes.sgr_mouse {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    // Legacy X10/normal encoding: releases do not say which button, and
    // coordinates beyond 223 cannot be expressed.
    if x > 223 || y > 223 {
        return None;
    }
    let code = if release { 3 + (code & !3) } else { code };
    let byte = |v: u32| u8::try_from(v + 32).unwrap_or(255);
    Some(vec![
        0x1b,
        b'[',
        b'M',
        byte(u32::from(code)),
        byte(x),
        byte(y),
    ])
}

/// Bytes for pasted text: bracketed if the program asked for it (with any
/// embedded end marker removed, so a paste cannot inject commands), newlines
/// as carriage returns otherwise.
pub fn encode_paste(text: &str, modes: &VtModes) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        let safe = normalized.replace("\x1b[201~", "");
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(safe.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

/// Focus in/out report, if the program enabled focus events.
pub fn encode_focus(focused: bool, modes: &VtModes) -> Option<Vec<u8>> {
    modes.focus_events.then(|| {
        if focused {
            b"\x1b[I".to_vec()
        } else {
            b"\x1b[O".to_vec()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &str) -> KeyEvent {
        spec.parse().expect("valid key")
    }

    fn enc(spec: &str) -> Vec<u8> {
        encode_key(key(spec), &VtModes::default())
    }

    #[test]
    fn printable_and_control_keys() {
        assert_eq!(enc("a"), b"a");
        assert_eq!(enc("%"), b"%");
        assert_eq!(
            encode_key(KeyEvent::ch('é'), &VtModes::default()),
            "é".as_bytes()
        );
        assert_eq!(enc("C-a"), [0x01]);
        assert_eq!(enc("C-c"), [0x03]);
        assert_eq!(enc("C-Space"), [0x00]);
        assert_eq!(enc("C-["), [0x1b]);
        assert_eq!(enc("C-?"), [0x7f]);
        assert_eq!(enc("M-x"), b"\x1bx");
        assert_eq!(enc("C-M-a"), b"\x1b\x01");
        assert_eq!(enc("Enter"), b"\r");
        assert_eq!(enc("Tab"), b"\t");
        assert_eq!(enc("BTab"), b"\x1b[Z");
        assert_eq!(enc("BSpace"), [0x7f]);
        assert_eq!(enc("Escape"), [0x1b]);
    }

    #[test]
    fn cursor_keys_respect_application_mode_and_modifiers() {
        assert_eq!(enc("Up"), b"\x1b[A");
        let app = VtModes {
            app_cursor: true,
            ..VtModes::default()
        };
        assert_eq!(encode_key(key("Up"), &app), b"\x1bOA");
        assert_eq!(enc("C-Left"), b"\x1b[1;5D");
        assert_eq!(enc("S-Right"), b"\x1b[1;2C");
        assert_eq!(enc("M-Home"), b"\x1b[1;3H");
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(enc("PPage"), b"\x1b[5~");
        assert_eq!(enc("C-DC"), b"\x1b[3;5~");
        assert_eq!(enc("F1"), b"\x1bOP");
        assert_eq!(enc("S-F1"), b"\x1b[1;2P");
        assert_eq!(enc("F5"), b"\x1b[15~");
        assert_eq!(enc("F12"), b"\x1b[24~");
    }

    fn mouse(kind: MouseKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            x,
            y,
            mods: Mods::empty(),
        }
    }

    #[test]
    fn mouse_reporting_modes() {
        let off = VtModes::default();
        assert_eq!(
            encode_mouse(mouse(MouseKind::Down(MouseButton::Left), 0, 0), &off),
            None
        );
        let sgr = VtModes {
            mouse_report: true,
            sgr_mouse: true,
            ..VtModes::default()
        };
        assert_eq!(
            encode_mouse(mouse(MouseKind::Down(MouseButton::Left), 4, 2), &sgr),
            Some(b"\x1b[<0;5;3M".to_vec())
        );
        assert_eq!(
            encode_mouse(mouse(MouseKind::Up(MouseButton::Right), 4, 2), &sgr),
            Some(b"\x1b[<2;5;3m".to_vec())
        );
        assert_eq!(
            encode_mouse(mouse(MouseKind::ScrollDown, 0, 0), &sgr),
            Some(b"\x1b[<65;1;1M".to_vec())
        );
        // Drags only when 1002/1003 is on.
        assert_eq!(
            encode_mouse(mouse(MouseKind::Drag(MouseButton::Left), 1, 1), &sgr),
            None
        );
        let drag = VtModes {
            mouse_drag: true,
            ..sgr
        };
        assert_eq!(
            encode_mouse(mouse(MouseKind::Drag(MouseButton::Left), 1, 1), &drag),
            Some(b"\x1b[<32;2;2M".to_vec())
        );
        let legacy = VtModes {
            mouse_report: true,
            ..VtModes::default()
        };
        assert_eq!(
            encode_mouse(mouse(MouseKind::Down(MouseButton::Left), 0, 0), &legacy),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            encode_mouse(mouse(MouseKind::Up(MouseButton::Left), 0, 0), &legacy),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
    }

    #[test]
    fn paste_is_bracketed_and_sanitized() {
        let plain = VtModes::default();
        assert_eq!(encode_paste("a\nb", &plain), b"a\rb");
        let br = VtModes {
            bracketed_paste: true,
            ..VtModes::default()
        };
        assert_eq!(
            encode_paste("x\x1b[201~rm -rf /\n", &br),
            b"\x1b[200~xrm -rf /\r\x1b[201~"
        );
    }

    #[test]
    fn focus_reports_only_when_enabled() {
        assert_eq!(encode_focus(true, &VtModes::default()), None);
        let m = VtModes {
            focus_events: true,
            ..VtModes::default()
        };
        assert_eq!(encode_focus(false, &m), Some(b"\x1b[O".to_vec()));
    }
}
