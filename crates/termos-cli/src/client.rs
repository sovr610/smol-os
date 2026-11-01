// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal frontend: owns the user's terminal, turns its input into
//! [`ClientMsg`]s and server frames into escape sequences.

use std::io::{self, Write};
use std::time::Duration;

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
use termos_render::probe::Replies;

/// Puts the terminal into desktop mode and restores it on drop (also when
/// unwinding from a panic).
pub(crate) struct TerminalGuard {
    kitty_keyboard: bool,
}

impl TerminalGuard {
    /// Raw mode, then the capability probe (when `probe` gives a
    /// timeout), then desktop mode. Returns the probe's replies.
    pub(crate) fn enter(
        out: &mut impl Write,
        probe: Option<Duration>,
    ) -> io::Result<(Self, Replies)> {
        terminal::enable_raw_mode()?;
        let replies = match probe {
            Some(timeout) => match crate::probe::run(out, timeout) {
                Ok((r, _)) => r,
                Err(e) => {
                    tracing::warn!("capability probe failed: {e}");
                    Replies::default()
                }
            },
            None => Replies::default(),
        };
        let kitty_keyboard = replies.kitty_keyboard.is_some();
        crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
        // The Windows console delivers input as key events, so pastes
        // arrive as typed keys there.
        // TODO(M9): VT input mode on Windows, for bracketed paste and the Kitty keyboard protocol.
        if cfg!(not(windows)) {
            crossterm::execute!(out, EnableBracketedPaste)?;
        }
        crossterm::execute!(out, EnableFocusChange)?;
        if kitty_keyboard {
            crossterm::execute!(
                out,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        Ok((Self { kitty_keyboard }, replies))
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
    if cfg!(not(windows)) {
        let _ = crossterm::execute!(out, DisableBracketedPaste);
    }
    let _ = crossterm::execute!(
        out,
        DisableFocusChange,
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
            let mut mods = mods(k.modifiers);
            // Ctrl+@ and Ctrl+Space are one key in terminals (both send
            // NUL). A ConPTY turns NUL into the key that types it, Ctrl+
            // Shift+2 on US layouts, which crossterm reports as `2`. Not
            // with Alt: Ctrl+Alt+@ is AltGr typing `@` on Windows.
            let ctrl_only = mods.contains(Mods::CTRL) && !mods.contains(Mods::ALT);
            let nul = matches!(code, KeyCode::Char('\0'))
                || (ctrl_only && matches!(code, KeyCode::Char('@')))
                || (cfg!(windows)
                    && ctrl_only
                    && mods.contains(Mods::SHIFT)
                    && matches!(code, KeyCode::Char('2')));
            if nul {
                let keep = mods & Mods::ALT;
                return Some(ClientMsg::Key(KeyEvent::new(
                    KeyCode::Char(' '),
                    Mods::CTRL | keep,
                )));
            }
            // Windows reports AltGr as Ctrl+Alt along with the character it
            // produced ('@', '{', '€' on many layouts): that is a plain
            // character, not a shortcut.
            if cfg!(windows)
                && let KeyCode::Char(c) = code
                && mods.contains(Mods::CTRL | Mods::ALT)
                && !c.is_ascii_alphanumeric()
                && !c.is_control()
            {
                mods.remove(Mods::CTRL | Mods::ALT);
            }
            Some(ClientMsg::Key(KeyEvent::new(code, mods)))
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

/// How an attached client ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Detached from this session; it keeps running.
    Detached(String),
    /// The session or the server ended, for this reason.
    Exited(String),
}

/// Runs the frontend until the server detaches it or the session ends.
pub(crate) async fn run(
    mut end: ClientEnd,
    caps: Caps,
    typeahead: Vec<ClientMsg>,
    out: &mut impl Write,
) -> io::Result<Ended> {
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
    for msg in typeahead {
        send(&end, msg)?;
    }
    let mut encoder = Encoder::new(&caps);
    let mut images = crate::images::ImageStore::default();
    let mut events = EventStream::new();
    // Kitty keeps images until told to drop them.
    let leave = |images: &crate::images::ImageStore, out: &mut dyn Write| {
        let _ = out.write_all(images.cleanup());
        let _ = out.flush();
    };
    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => {
                    if let Some(msg) = convert(e) {
                        send(&end, msg)?;
                    }
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(Ended::Exited("the terminal closed".into())),
            },
            msg = end.rx.recv() => match msg {
                Some(ServerMsg::Frame(f)) => {
                    let pixels = images.frame(f.images.as_deref(), f.clear);
                    let bytes =
                        encoder.frame_with_images(f.cols, &f.spans, f.cursor, f.clear, &pixels);
                    out.write_all(bytes)?;
                    out.flush()?;
                }
                Some(ServerMsg::ImageData { id, chunk, last }) => images.data(id, chunk, last),
                Some(ServerMsg::ImageFree { id }) => {
                    let bytes = images.free(id);
                    if !bytes.is_empty() {
                        out.write_all(&bytes)?;
                        out.flush()?;
                    }
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
                Some(ServerMsg::Exit { reason }) => {
                    leave(&images, out);
                    return Ok(Ended::Exited(reason));
                }
                Some(ServerMsg::Detached { session }) => {
                    leave(&images, out);
                    return Ok(Ended::Detached(session));
                }
                None => {
                    leave(&images, out);
                    return Ok(Ended::Exited("the server closed the connection".into()));
                }
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
        // NUL in its other spellings is Ctrl+Space too.
        for k in [
            key(
                ct::KeyCode::Char('@'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            key(ct::KeyCode::Char('@'), KeyModifiers::CONTROL),
            key(ct::KeyCode::Char('\0'), KeyModifiers::NONE),
        ] {
            assert_eq!(convert(k), Some(ClientMsg::Key(KeyEvent::ctrl(' '))));
        }
        assert_eq!(
            convert(key(ct::KeyCode::Char('@'), KeyModifiers::NONE)),
            Some(ClientMsg::Key(KeyEvent::ch('@')))
        );
        let ctrl_shift_2 = convert(key(
            ct::KeyCode::Char('2'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        if cfg!(windows) {
            assert_eq!(ctrl_shift_2, Some(ClientMsg::Key(KeyEvent::ctrl(' '))));
        } else {
            assert_ne!(ctrl_shift_2, Some(ClientMsg::Key(KeyEvent::ctrl(' '))));
        }
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
