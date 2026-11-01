// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal frontend: owns the user's terminal, turns its input into
//! [`ClientMsg`]s and server frames into escape sequences.

use std::io::{self, Write};

use crossterm::event::{
    self as ct, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture,
    EnableBracketedPaste, EnableFocusChange, EnableMouseCapture, Event, EventStream, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use termos_core::input::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::ClientEnd;
use termos_render::caps::Caps;
use termos_render::encode::{Encoder, osc52_copy, set_title};

/// Puts the terminal into desktop mode and restores it on drop (also when
/// unwinding from a panic).
pub(crate) struct TerminalGuard {
    kitty_keyboard: bool,
}

impl TerminalGuard {
    pub(crate) fn enter(out: &mut impl Write) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let kitty_keyboard = matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        crossterm::execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            EnableFocusChange
        )?;
        if kitty_keyboard {
            crossterm::execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        Ok(Self { kitty_keyboard })
    }

    pub(crate) fn kitty_keyboard(&self) -> bool {
        self.kitty_keyboard
    }
}

/// Undoes everything [`TerminalGuard::enter`] did; safe to call twice.
pub(crate) fn restore_terminal(kitty_keyboard: bool) {
    let mut out = io::stdout();
    if kitty_keyboard {
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(
        out,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    // Reset attributes, show the cursor and restore the default cursor shape.
    let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[0 q");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.kitty_keyboard);
    }
}

fn mods(m: KeyModifiers) -> Mods {
    let mut out = Mods::empty();
    if m.contains(KeyModifiers::SHIFT) {
        out |= Mods::SHIFT;
    }
    if m.contains(KeyModifiers::CONTROL) {
        out |= Mods::CTRL;
    }
    if m.intersects(KeyModifiers::ALT | KeyModifiers::META) {
        out |= Mods::ALT;
    }
    if m.contains(KeyModifiers::SUPER) {
        out |= Mods::SUPER;
    }
    out
}

fn button(b: ct::MouseButton) -> MouseButton {
    match b {
        ct::MouseButton::Left => MouseButton::Left,
        ct::MouseButton::Right => MouseButton::Right,
        ct::MouseButton::Middle => MouseButton::Middle,
    }
}

/// Converts a crossterm event; `None` for events the desktop ignores.
pub(crate) fn convert(event: Event) -> Option<ClientMsg> {
    match event {
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return None;
            }
            let code = match k.code {
                ct::KeyCode::Char(c) => KeyCode::Char(c),
                ct::KeyCode::Null => return Some(ClientMsg::Key(KeyEvent::ctrl(' '))),
                ct::KeyCode::Enter => KeyCode::Enter,
                ct::KeyCode::Tab => KeyCode::Tab,
                ct::KeyCode::BackTab => KeyCode::BackTab,
                ct::KeyCode::Backspace => KeyCode::Backspace,
                ct::KeyCode::Esc => KeyCode::Esc,
                ct::KeyCode::Left => KeyCode::Left,
                ct::KeyCode::Right => KeyCode::Right,
                ct::KeyCode::Up => KeyCode::Up,
                ct::KeyCode::Down => KeyCode::Down,
                ct::KeyCode::Home => KeyCode::Home,
                ct::KeyCode::End => KeyCode::End,
                ct::KeyCode::PageUp => KeyCode::PageUp,
                ct::KeyCode::PageDown => KeyCode::PageDown,
                ct::KeyCode::Insert => KeyCode::Insert,
                ct::KeyCode::Delete => KeyCode::Delete,
                ct::KeyCode::F(n) => KeyCode::F(n),
                _ => return None,
            };
            Some(ClientMsg::Key(KeyEvent::new(code, mods(k.modifiers))))
        }
        Event::Mouse(m) => {
            let kind = match m.kind {
                ct::MouseEventKind::Down(b) => MouseKind::Down(button(b)),
                ct::MouseEventKind::Up(b) => MouseKind::Up(button(b)),
                ct::MouseEventKind::Drag(b) => MouseKind::Drag(button(b)),
                ct::MouseEventKind::Moved => MouseKind::Moved,
                ct::MouseEventKind::ScrollUp => MouseKind::ScrollUp,
                ct::MouseEventKind::ScrollDown => MouseKind::ScrollDown,
                ct::MouseEventKind::ScrollLeft => MouseKind::ScrollLeft,
                ct::MouseEventKind::ScrollRight => MouseKind::ScrollRight,
            };
            Some(ClientMsg::Mouse(MouseEvent {
                kind,
                x: m.column,
                y: m.row,
                mods: mods(m.modifiers),
            }))
        }
        Event::Paste(text) => Some(ClientMsg::Paste(text)),
        Event::Resize(cols, rows) => Some(ClientMsg::Resize { cols, rows }),
        Event::FocusGained => Some(ClientMsg::Focus(true)),
        Event::FocusLost => Some(ClientMsg::Focus(false)),
    }
}

/// Runs the frontend until the server says the session ended. Returns the
/// reason.
pub(crate) async fn run(
    mut end: ClientEnd,
    caps: Caps,
    out: &mut impl Write,
) -> io::Result<String> {
    let (cols, rows) = terminal::size()?;
    let send = |end: &ClientEnd, msg: ClientMsg| {
        end.tx
            .send(msg)
            .map_err(|_| io::Error::other("the desktop server stopped"))
    };
    send(
        &end,
        ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            caps: caps.clone(),
            cols,
            rows,
        },
    )?;
    let mut encoder = Encoder::new(&caps);
    let mut events = EventStream::new();
    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => {
                    if let Some(msg) = convert(e) {
                        send(&end, msg)?;
                    }
                }
                Some(Err(e)) => return Err(e),
                None => return Ok("the terminal closed".into()),
            },
            msg = end.rx.recv() => match msg {
                Some(ServerMsg::Frame(f)) => {
                    let bytes = encoder.frame(f.cols, &f.spans, f.cursor, f.clear);
                    out.write_all(bytes)?;
                    out.flush()?;
                }
                Some(ServerMsg::Clipboard(text)) => {
                    out.write_all(&osc52_copy(&text))?;
                    out.flush()?;
                }
                Some(ServerMsg::Bell) => {
                    out.write_all(b"\x07")?;
                    out.flush()?;
                }
                Some(ServerMsg::Title(t)) => {
                    out.write_all(&set_title(&t))?;
                    out.flush()?;
                }
                Some(ServerMsg::Exit { reason }) => return Ok(reason),
                Some(ServerMsg::Detached { session }) => {
                    return Ok(format!("detached (from session {session})"));
                }
                None => return Ok("the desktop server stopped".into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: ct::KeyCode, m: KeyModifiers) -> Event {
        Event::Key(ct::KeyEvent::new(code, m))
    }

    #[test]
    fn keys_convert_and_normalize() {
        assert_eq!(
            convert(key(ct::KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(ClientMsg::Key(KeyEvent::ctrl('b')))
        );
        assert_eq!(
            convert(key(ct::KeyCode::Char('%'), KeyModifiers::SHIFT)),
            Some(ClientMsg::Key(KeyEvent::ch('%')))
        );
        assert_eq!(
            convert(key(ct::KeyCode::Null, KeyModifiers::NONE)),
            Some(ClientMsg::Key(KeyEvent::ctrl(' ')))
        );
        assert_eq!(
            convert(key(ct::KeyCode::Left, KeyModifiers::ALT)),
            Some(ClientMsg::Key(KeyEvent::new(KeyCode::Left, Mods::ALT)))
        );
        let release = Event::Key(ct::KeyEvent::new_with_kind(
            ct::KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(convert(release), None);
    }

    #[test]
    fn mouse_resize_paste_focus() {
        let m = Event::Mouse(ct::MouseEvent {
            kind: ct::MouseEventKind::Drag(ct::MouseButton::Left),
            column: 3,
            row: 4,
            modifiers: KeyModifiers::SHIFT,
        });
        assert_eq!(
            convert(m),
            Some(ClientMsg::Mouse(MouseEvent {
                kind: MouseKind::Drag(MouseButton::Left),
                x: 3,
                y: 4,
                mods: Mods::SHIFT,
            }))
        );
        assert_eq!(
            convert(Event::Resize(80, 24)),
            Some(ClientMsg::Resize { cols: 80, rows: 24 })
        );
        assert_eq!(
            convert(Event::Paste("x".into())),
            Some(ClientMsg::Paste("x".into()))
        );
        assert_eq!(convert(Event::FocusLost), Some(ClientMsg::Focus(false)));
    }
}
