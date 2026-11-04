// SPDX-License-Identifier: GPL-3.0-or-later
//! Asks the terminal what it can do (ARCHITECTURE.md §5.7): writes the
//! query batch, then reads replies straight from stdin until the DA1 reply
//! arrives or the timeout passes. Runs once at attach, in raw mode, before
//! anything else reads the terminal.

use std::io::{self, Write};
use std::time::Duration;

use termos_core::input::{KeyCode, KeyEvent};
use termos_core::proto::ClientMsg;
use termos_render::probe::Replies;

/// How long to wait for replies: terminals answer in a few milliseconds
/// locally; over SSH the round trip dominates.
pub(crate) fn timeout(remote: bool) -> Duration {
    Duration::from_millis(if remote { 800 } else { 200 })
}

/// Whether to probe at all: `TERMOS_PROBE=0` turns it off (for terminals
/// that print queries they don't understand).
pub(crate) fn enabled(get: impl Fn(&str) -> Option<String>) -> bool {
    get("TERMOS_PROBE").as_deref() != Some("0")
}

/// Sends the queries and collects replies. The terminal must be in raw
/// mode (and, on Windows, VT input mode). Returns the raw bytes too, for
/// `termos debug caps --raw`.
pub(crate) fn run(out: &mut impl Write, timeout: Duration) -> io::Result<(Replies, Vec<u8>)> {
    use std::time::Instant;

    use termos_render::probe::{QUERIES, parse_replies};

    use crate::term_input::{Piece, Source as _, Wait};

    let (mut source, _stop) = crate::term_input::open()?;
    out.write_all(QUERIES.as_bytes())?;
    out.flush()?;
    let deadline = Instant::now() + timeout;
    let mut raw = Vec::new();
    let mut pieces = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match source.wait(Some(left))? {
            Wait::Ready => {}
            Wait::Timeout | Wait::Stopped => break,
        }
        let open = source.read(&mut pieces)?;
        for piece in pieces.drain(..) {
            if let Piece::Bytes(b) = piece {
                raw.extend_from_slice(&b);
            }
        }
        if !open || parse_replies(&raw).complete() {
            break;
        }
    }
    Ok((parse_replies(&raw), raw))
}

/// Keys typed while the probe ran, as key messages. Plain characters,
/// Enter, Tab, Backspace and Ctrl+letter survive; escape sequences (arrow
/// keys and the like) are dropped, since they cannot be told apart from
/// a reply cut short.
pub(crate) fn typeahead(r: &Replies) -> Vec<ClientMsg> {
    let mut bytes = r.other.clone();
    if !r.complete() {
        bytes.extend_from_slice(&r.partial);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut keys = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let key = match c {
            '\x1b' => {
                // Skip the rest of the sequence.
                if chars.next_if(|&n| n == '[' || n == 'O').is_some() {
                    while chars.next_if(|n| !('@'..='~').contains(n)).is_some() {}
                    chars.next();
                } else {
                    chars.next();
                }
                continue;
            }
            '\r' | '\n' => KeyEvent::plain(KeyCode::Enter),
            '\t' => KeyEvent::plain(KeyCode::Tab),
            '\x7f' | '\x08' => KeyEvent::plain(KeyCode::Backspace),
            '\x01'..='\x1a' => KeyEvent::ctrl(char::from(b'a' + (c as u8 - 1))),
            c if c.is_control() => continue,
            c => KeyEvent::ch(c),
        };
        keys.push(ClientMsg::Key(key));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use termos_render::probe::parse_replies;

    #[test]
    fn typed_keys_survive_the_probe() {
        let r = parse_replies(b"ls\r\x1b[A\x03\x1b[?62c");
        assert_eq!(
            typeahead(&r),
            vec![
                ClientMsg::Key(KeyEvent::ch('l')),
                ClientMsg::Key(KeyEvent::ch('s')),
                ClientMsg::Key(KeyEvent::plain(KeyCode::Enter)),
                ClientMsg::Key(KeyEvent::ctrl('c')),
            ]
        );
        // A reply cut short by the timeout is not typed.
        let r = parse_replies(b"x\x1b[?6");
        assert_eq!(typeahead(&r), vec![ClientMsg::Key(KeyEvent::ch('x'))]);
    }

    #[test]
    fn switches() {
        assert!(timeout(true) > timeout(false));
        assert!(!enabled(|k| (k == "TERMOS_PROBE").then(|| "0".to_owned())));
        assert!(enabled(|_| None));
    }
}
