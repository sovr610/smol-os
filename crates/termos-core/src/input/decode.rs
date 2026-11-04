// SPDX-License-Identifier: GPL-3.0-or-later
//! Turns the bytes a terminal sends into input events (D-102):
//! - **keys:** legacy bytes, xterm's `CSI 1 ; mods X` and `CSI n ; mods ~`
//!   forms, SS3 keys, the Kitty keyboard protocol (`CSI code ; mods u`,
//!   with event types and alternate keys) and xterm's `modifyOtherKeys`;
//! - **mouse reports:** SGR, urxvt and X10;
//! - **focus reports** and **bracketed pastes**.
//!
//! Replies to queries that arrive late (DA1, `CSI ? u`, DECRQM, OSC, DCS
//! and APC strings) are read and dropped, so they never become typed keys.
//!
//! The decoder is incremental: bytes may be split anywhere. A lone ESC or
//! an unfinished sequence waits for more input. The caller calls
//! [`Decoder::flush`] when nothing more arrived within
//! [`Decoder::timeout`]; the pending bytes then count as keys, and ESC
//! alone is the Escape key.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{KeyCode, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};

/// How long a lone ESC (or an unfinished key sequence) waits for the rest.
pub const ESC_TIMEOUT: Duration = Duration::from_millis(50);
/// How long an unfinished reply string (OSC, DCS, APC) waits for the rest
/// once it has content, since replies over SSH can arrive in pieces.
pub const STRING_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a bracketed paste may go silent before what arrived is
/// delivered and the paste ends, so a lost end marker cannot swallow the
/// keyboard.
pub const PASTE_TIMEOUT: Duration = Duration::from_secs(1);
/// A longer paste is delivered in parts of at most this many bytes.
pub const MAX_PASTE_PART: usize = 4 * 1024 * 1024;

/// A CSI sequence longer than this is malformed and dropped.
const MAX_CSI: usize = 256;
/// A reply string longer than this is skipped without being kept.
const MAX_STRING: usize = 64 * 1024;

const PASTE_END: &[u8] = b"\x1b[201~";

/// Press, repeat or release (the Kitty protocol's event types, flag 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// What a Kitty terminal reports about a key beyond [`KeyEvent`] (flag 4,
/// alternate keys): the key without Shift, the character it types with
/// Shift, and the key in the same place on a US layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct KeyAlternates {
    pub unshifted: Option<char>,
    pub shifted: Option<char>,
    pub base: Option<char>,
}

/// One decoded input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Key {
        key: KeyEvent,
        kind: KeyKind,
        alternates: KeyAlternates,
    },
    Mouse(MouseEvent),
    /// A bracketed paste (or one part of a long one).
    Paste(String),
    /// The terminal gained (`true`) or lost focus.
    Focus(bool),
}

impl Input {
    /// A key press with no extra information.
    pub fn press(key: KeyEvent) -> Self {
        Self::Key {
            key,
            kind: KeyKind::Press,
            alternates: KeyAlternates::default(),
        }
    }
}

/// Incremental decoder of terminal input bytes.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Bytes not decoded yet: at most one unfinished sequence.
    buf: Vec<u8>,
    /// Inside a bracketed paste: the text so far.
    paste: Option<Vec<u8>>,
    /// Skipping a reply string that grew past [`MAX_STRING`].
    skipping: Option<StringKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    /// OSC: ends with BEL or ST.
    Osc,
    /// DCS or APC: ends with ST.
    Other,
}

/// What one step of decoding found at the start of the buffer.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// `n` bytes were read, giving this event (or nothing to report).
    Event(usize, Option<Input>),
    /// `n` bytes were read and a bracketed paste starts.
    PasteStart(usize),
    /// The sequence is not finished yet.
    Incomplete,
    /// A reply string too long to keep: skip until it ends.
    LongString(StringKind),
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes `bytes`, appending the events they finish to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        self.buf.extend_from_slice(bytes);
        self.run(out, false);
    }

    /// How long to wait for more input before calling [`Self::flush`], if
    /// something is pending.
    pub fn timeout(&self) -> Option<Duration> {
        if self.paste.is_some() {
            Some(PASTE_TIMEOUT)
        } else if self.skipping.is_some() || is_string_with_content(&self.buf) {
            Some(STRING_TIMEOUT)
        } else if self.buf.is_empty() {
            None
        } else {
            Some(ESC_TIMEOUT)
        }
    }

    /// Settles whatever is pending, because no more input arrived within
    /// [`Self::timeout`]: ESC alone is the Escape key, ESC and a key is
    /// Alt with that key, an unfinished reply string is dropped and an
    /// unfinished paste is delivered as it is.
    pub fn flush(&mut self, out: &mut Vec<Input>) {
        if let Some(mut paste) = self.paste.take() {
            paste.append(&mut self.buf);
            if !paste.is_empty() {
                out.push(Input::Paste(String::from_utf8_lossy(&paste).into_owned()));
            }
            return;
        }
        if self.skipping.take().is_some() {
            self.buf.clear();
            return;
        }
        self.run(out, true);
    }

    fn run(&mut self, out: &mut Vec<Input>, flushing: bool) {
        let mut pos = 0;
        while pos < self.buf.len() {
            if let Some(paste) = &mut self.paste {
                let rest = &self.buf[pos..];
                if let Some(i) = find(rest, PASTE_END) {
                    paste.extend_from_slice(&rest[..i]);
                    pos += i + PASTE_END.len();
                    paste_parts(paste, out);
                    if !paste.is_empty() {
                        out.push(Input::Paste(String::from_utf8_lossy(paste).into_owned()));
                    }
                    self.paste = None;
                    continue;
                }
                // Keep what may be the start of the end marker.
                let take = rest.len() - partial_suffix(rest, PASTE_END);
                paste.extend_from_slice(&rest[..take]);
                pos += take;
                paste_parts(paste, out);
                break;
            }
            if let Some(kind) = self.skipping {
                let rest = &self.buf[pos..];
                match string_end(rest, kind) {
                    Some(n) => {
                        pos += n;
                        self.skipping = None;
                        continue;
                    }
                    None => {
                        // Keep a final ESC: it may start the terminator.
                        pos += rest.len() - usize::from(rest.last() == Some(&0x1b));
                        break;
                    }
                }
            }
            match parse(&self.buf[pos..]) {
                Step::Event(n, event) => {
                    pos += n;
                    out.extend(event);
                }
                Step::PasteStart(n) => {
                    pos += n;
                    self.paste = Some(Vec::new());
                }
                Step::LongString(kind) => {
                    self.skipping = Some(kind);
                    // Past the introducer, which cannot be the terminator.
                    pos += 2;
                }
                Step::Incomplete if flushing => pos += settle(&self.buf[pos..], out),
                Step::Incomplete => break,
            }
        }
        self.buf.drain(..pos);
    }
}

/// Delivers full parts of a long paste, never splitting a character.
fn paste_parts(paste: &mut Vec<u8>, out: &mut Vec<Input>) {
    while paste.len() >= MAX_PASTE_PART {
        let cut = char_boundary(&paste[..MAX_PASTE_PART]).max(1);
        let part: Vec<u8> = paste.drain(..cut).collect();
        out.push(Input::Paste(String::from_utf8_lossy(&part).into_owned()));
    }
}

/// Reads an unfinished sequence as keys, because nothing more came.
/// Returns the bytes consumed.
fn settle(b: &[u8], out: &mut Vec<Input>) -> usize {
    if b[0] != 0x1b {
        // A UTF-8 character cut short: dropped.
        return 1;
    }
    match b.get(1) {
        None => {
            out.push(Input::press(KeyEvent::plain(KeyCode::Esc)));
            1
        }
        // A reply string cut short: dropped.
        Some(b'P' | b']' | b'_') if b.len() > 2 => b.len(),
        Some(0x1b) => {
            out.push(Input::press(KeyEvent::new(KeyCode::Esc, Mods::ALT)));
            2
        }
        Some(&c) if c.is_ascii() => {
            if let Some(key) = legacy_key(c) {
                out.push(Input::press(with_alt(key)));
            }
            2
        }
        // ESC and a UTF-8 character cut short.
        Some(_) => b.len(),
    }
}

/// Decodes the item at the start of `b` (which is not empty).
fn parse(b: &[u8]) -> Step {
    if b[0] == 0x1b {
        return parse_esc(b);
    }
    match plain(b) {
        Some((n, key)) => Step::Event(n, key.map(Input::press)),
        None => Step::Incomplete,
    }
}

fn parse_esc(b: &[u8]) -> Step {
    let Some(&c) = b.get(1) else {
        return Step::Incomplete;
    };
    match c {
        b'[' => parse_csi(b),
        b'O' => parse_ss3(b),
        b'P' | b']' | b'_' => parse_string(b),
        0x1b => match b.get(2) {
            None => Step::Incomplete,
            // Alt with a key that is itself a sequence (rxvt: ESC ESC [ A).
            Some(b'[' | b'O') => match parse_esc(&b[1..]) {
                Step::Event(n, Some(Input::Key { key, kind, .. })) => Step::Event(
                    n + 1,
                    Some(Input::Key {
                        key: with_alt(key),
                        kind,
                        alternates: KeyAlternates::default(),
                    }),
                ),
                Step::Event(n, other) => Step::Event(n + 1, other),
                Step::PasteStart(n) => Step::PasteStart(n + 1),
                other => other,
            },
            Some(_) => Step::Event(
                2,
                Some(Input::press(KeyEvent::new(KeyCode::Esc, Mods::ALT))),
            ),
        },
        _ => match plain(&b[1..]) {
            Some((n, key)) => Step::Event(n + 1, key.map(|k| Input::press(with_alt(k)))),
            None => Step::Incomplete,
        },
    }
}

/// A legacy key at the start of `b`: one byte, or one UTF-8 character.
/// `None` if the character is cut short; `Some((n, None))` for bytes that
/// are no key.
fn plain(b: &[u8]) -> Option<(usize, Option<KeyEvent>)> {
    let c = b[0];
    if c.is_ascii() {
        return Some((1, legacy_key(c)));
    }
    let len = match c {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some((1, None)),
    };
    let have = b.len().min(len);
    if b[1..have].iter().any(|&x| x & 0xc0 != 0x80) {
        return Some((1, None));
    }
    if have < len {
        return None;
    }
    match std::str::from_utf8(&b[..len]) {
        Ok(s) => Some((len, s.chars().next().map(KeyEvent::ch))),
        Err(_) => Some((len, None)),
    }
}

/// The key one ASCII byte types in raw mode.
fn legacy_key(c: u8) -> Option<KeyEvent> {
    Some(match c {
        0x00 => KeyEvent::ctrl(' '),
        b'\t' => KeyEvent::plain(KeyCode::Tab),
        b'\r' => KeyEvent::plain(KeyCode::Enter),
        0x1b => KeyEvent::plain(KeyCode::Esc),
        0x7f => KeyEvent::plain(KeyCode::Backspace),
        // Ctrl+letter, including Ctrl+H (0x08) and Ctrl+J (0x0a, a newline
        // in raw mode).
        0x01..=0x1a => KeyEvent::ctrl(char::from(b'a' + c - 1)),
        0x1c..=0x1f => KeyEvent::ctrl(char::from(b'4' + c - 0x1c)),
        0x20..=0x7e => KeyEvent::ch(char::from(c)),
        _ => return None,
    })
}

fn with_alt(key: KeyEvent) -> KeyEvent {
    KeyEvent::new(key.code, key.mods | Mods::ALT)
}

/// `CSI` sequences: keys, mouse and focus reports, the paste start, and
/// replies (dropped).
fn parse_csi(b: &[u8]) -> Step {
    let mut i = 2;
    let private = match b.get(i) {
        Some(&p @ 0x3c..=0x3f) => {
            i += 1;
            Some(p)
        }
        Some(_) => None,
        None => return Step::Incomplete,
    };
    // Too long to be anything: dropped at the same byte however the
    // input was split.
    let params_start = i;
    while let Some(&c) = b.get(i) {
        if i >= MAX_CSI {
            return Step::Event(i, None);
        }
        if !(0x30..=0x3f).contains(&c) {
            break;
        }
        i += 1;
    }
    let params_end = i;
    while let Some(&c) = b.get(i) {
        if i >= MAX_CSI {
            return Step::Event(i, None);
        }
        if !(0x20..=0x2f).contains(&c) {
            break;
        }
        i += 1;
    }
    let Some(&fin) = b.get(i) else {
        return Step::Incomplete;
    };
    if !(0x40..=0x7e).contains(&fin) {
        // Malformed: drop what came so far, and decode from this byte.
        return Step::Event(i, None);
    }
    let n = i + 1;
    let intermediates = &b[params_end..i];
    let params = Params::parse(&b[params_start..params_end]);
    // X10 mouse: `CSI M` and three raw bytes.
    if fin == b'M' && private.is_none() && params_start == params_end && intermediates.is_empty() {
        if b.len() < n + 3 {
            return Step::Incomplete;
        }
        let v = |k: usize| u32::from(b[n + k]).saturating_sub(32);
        let event = mouse(v(0), v(1), v(2), false).map(Input::Mouse);
        return Step::Event(n + 3, event);
    }
    if !intermediates.is_empty() {
        return Step::Event(n, None);
    }
    match private {
        Some(b'<') if fin == b'M' || fin == b'm' => {
            let event = mouse(
                params.get(0, 0),
                params.get(1, 0),
                params.get(2, 0),
                fin == b'm',
            )
            .map(Input::Mouse);
            Step::Event(n, event)
        }
        // A reply to a query (DA1, `CSI ? u`, ...).
        Some(_) => Step::Event(n, None),
        None => csi_key(&params, fin, n),
    }
}

fn csi_key(params: &Params, fin: u8, n: usize) -> Step {
    let mods = params.get(1, 0);
    let kind = event_kind(params.get(1, 1));
    let key = |code: KeyCode| Step::Event(n, key_event(code, mods, kind, KeyAlternates::default()));
    match fin {
        b'A' => key(KeyCode::Up),
        b'B' => key(KeyCode::Down),
        b'C' => key(KeyCode::Right),
        b'D' => key(KeyCode::Left),
        b'H' => key(KeyCode::Home),
        b'F' => key(KeyCode::End),
        b'P' => key(KeyCode::F(1)),
        b'Q' => key(KeyCode::F(2)),
        b'S' => key(KeyCode::F(4)),
        // F3 in xterm; with other numbers, a cursor position report.
        b'R' if params.get(0, 0) <= 1 => key(KeyCode::F(3)),
        b'Z' => key(KeyCode::BackTab),
        b'I' if params.is_empty() => Step::Event(n, Some(Input::Focus(true))),
        b'O' if params.is_empty() => Step::Event(n, Some(Input::Focus(false))),
        // urxvt mouse: `CSI b ; x ; y M`, with b offset by 32.
        b'M' if params.len() >= 3 => {
            let event = mouse(
                params.get(0, 0).saturating_sub(32),
                params.get(1, 0),
                params.get(2, 0),
                false,
            );
            Step::Event(n, event.map(Input::Mouse))
        }
        b'~' => match params.get(0, 0) {
            200 => Step::PasteStart(n),
            // xterm's modifyOtherKeys: `CSI 27 ; mods ; code ~`.
            27 if params.len() >= 3 => Step::Event(
                n,
                kitty_key(
                    params.get(2, 0),
                    &KeyAlternates::default(),
                    mods,
                    KeyKind::Press,
                    None,
                ),
            ),
            code => match tilde_key(code) {
                Some(code) => key(code),
                None => Step::Event(n, None),
            },
        },
        b'u' => {
            let alt = |k: usize| {
                Some(params.get(0, k))
                    .filter(|&c| c != 0 && params.has(0, k))
                    .and_then(char::from_u32)
            };
            let alternates = KeyAlternates {
                unshifted: None,
                shifted: alt(1),
                base: alt(2),
            };
            // The text the key types (flag 16): its first character.
            let text = params
                .has(2, 0)
                .then(|| params.get(2, 0))
                .and_then(char::from_u32)
                .filter(|c| !c.is_control());
            Step::Event(
                n,
                kitty_key(params.get(0, 0), &alternates, mods, kind, text),
            )
        }
        _ => Step::Event(n, None),
    }
}

/// The key of `CSI n ~`.
fn tilde_key(n: u32) -> Option<KeyCode> {
    Some(match n {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F(u8::try_from(n - 10).ok()?),
        17..=21 => KeyCode::F(u8::try_from(n - 11).ok()?),
        23..=26 => KeyCode::F(u8::try_from(n - 12).ok()?),
        28 | 29 => KeyCode::F(u8::try_from(n - 13).ok()?),
        31..=34 => KeyCode::F(u8::try_from(n - 14).ok()?),
        _ => return None,
    })
}

/// A key of the Kitty protocol's `CSI code u` form; `text` is what it
/// types, when the terminal says (flag 16).
fn kitty_key(
    code: u32,
    alternates: &KeyAlternates,
    mods: u32,
    kind: KeyKind,
    text: Option<char>,
) -> Option<Input> {
    let key = match code {
        9 => KeyCode::Tab,
        13 | 57414 => KeyCode::Enter,
        27 => KeyCode::Esc,
        127 | 8 => KeyCode::Backspace,
        // The keypad.
        57399..=57408 => KeyCode::Char(char::from_digit(code - 57399, 10)?),
        57409 => KeyCode::Char('.'),
        57410 => KeyCode::Char('/'),
        57411 => KeyCode::Char('*'),
        57412 => KeyCode::Char('-'),
        57413 => KeyCode::Char('+'),
        57415 => KeyCode::Char('='),
        57416 => KeyCode::Char(','),
        57417 => KeyCode::Left,
        57418 => KeyCode::Right,
        57419 => KeyCode::Up,
        57420 => KeyCode::Down,
        57421 => KeyCode::PageUp,
        57422 => KeyCode::PageDown,
        57423 => KeyCode::Home,
        57424 => KeyCode::End,
        57425 => KeyCode::Insert,
        57426 => KeyCode::Delete,
        57376..=57398 => KeyCode::F(u8::try_from(code - 57376 + 13).ok()?),
        // Other functional keys (locks, media, modifiers alone) are not
        // keys the desktop or its programs use.
        57344..=63743 => return None,
        _ => {
            let c = char::from_u32(code).filter(|c| !c.is_control())?;
            let shift = decode_mods(mods).contains(Mods::SHIFT);
            let typed = match (text, alternates.shifted) {
                (Some(t), _) => t,
                (None, Some(s)) if shift => s,
                (None, _) if shift => single_upper(c),
                (None, _) => c,
            };
            // With Shift held, `shifted` is what it typed, so whoever
            // encodes the key again knows Shift was part of it.
            let alternates = KeyAlternates {
                unshifted: Some(c),
                shifted: shift.then_some(typed),
                base: alternates.base,
            };
            return key_event(KeyCode::Char(typed), mods, kind, alternates);
        }
    };
    let key = if key == KeyCode::Tab && decode_mods(mods).contains(Mods::SHIFT) {
        KeyCode::BackTab
    } else {
        key
    };
    key_event(key, mods, kind, *alternates)
}

fn single_upper(c: char) -> char {
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// The modifier bits of a parameter: 1 + shift(1) + alt(2) + ctrl(4) +
/// super(8) + hyper(16) + meta(32) + caps lock(64) + num lock(128).
fn decode_mods(param: u32) -> Mods {
    let bits = param.saturating_sub(1);
    let mut mods = Mods::empty();
    if bits & 1 != 0 {
        mods |= Mods::SHIFT;
    }
    if bits & (2 | 32) != 0 {
        mods |= Mods::ALT;
    }
    if bits & 4 != 0 {
        mods |= Mods::CTRL;
    }
    if bits & 8 != 0 {
        mods |= Mods::SUPER;
    }
    mods
}

fn event_kind(param: u32) -> KeyKind {
    match param {
        2 => KeyKind::Repeat,
        3 => KeyKind::Release,
        _ => KeyKind::Press,
    }
}

fn key_event(code: KeyCode, mods: u32, kind: KeyKind, alternates: KeyAlternates) -> Option<Input> {
    let mods = decode_mods(mods);
    // Ctrl+@ and Ctrl+Space are one key in terminals (both send NUL).
    let key =
        if code == KeyCode::Char('@') && mods.contains(Mods::CTRL) && !mods.contains(Mods::ALT) {
            KeyEvent::ctrl(' ')
        } else {
            KeyEvent::new(code, mods)
        };
    Some(Input::Key {
        key,
        kind,
        alternates,
    })
}

/// `ESC O x`: cursor and F1–F4 keys in application mode, and the keypad.
fn parse_ss3(b: &[u8]) -> Step {
    let mut i = 2;
    // Some terminals put a modifier digit here (`ESC O 5 A`).
    let mut mods = 0;
    while let Some(&d @ b'0'..=b'9') = b.get(i) {
        mods = mods * 10 + u32::from(d - b'0');
        i += 1;
        if i > 4 {
            return Step::Event(i, None);
        }
    }
    let Some(&fin) = b.get(i) else {
        return Step::Incomplete;
    };
    let code = match fin {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        b'M' => KeyCode::Enter,
        b'X' => KeyCode::Char('='),
        b'j' => KeyCode::Char('*'),
        b'k' => KeyCode::Char('+'),
        b'l' => KeyCode::Char(','),
        b'm' => KeyCode::Char('-'),
        b'n' => KeyCode::Char('.'),
        b'o' => KeyCode::Char('/'),
        b'p'..=b'y' => KeyCode::Char(char::from(b'0' + (fin - b'p'))),
        _ => return Step::Event(i + 1, None),
    };
    Step::Event(
        i + 1,
        key_event(code, mods, KeyKind::Press, KeyAlternates::default()),
    )
}

/// OSC, DCS and APC strings: replies, read and dropped.
fn parse_string(b: &[u8]) -> Step {
    let kind = if b[1] == b']' {
        StringKind::Osc
    } else {
        StringKind::Other
    };
    match string_end(&b[2..], kind) {
        // ESC P (or ] or _) and at once another sequence: Alt and a key
        // typed just before something else, not a reply.
        Some(0) => Step::Event(2, legacy_key(b[1]).map(|k| Input::press(with_alt(k)))),
        Some(n) => Step::Event(n + 2, None),
        None if b.len() > MAX_STRING => Step::LongString(kind),
        None => Step::Incomplete,
    }
}

/// Where a string's terminator ends in `b`: ST (`ESC \`), BEL for OSC, or
/// an ESC that starts something else (the string was cut short; that ESC
/// is not consumed).
fn string_end(b: &[u8], kind: StringKind) -> Option<usize> {
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x07 if kind == StringKind::Osc => return Some(i + 1),
            0x1b => {
                return match b.get(i + 1) {
                    Some(b'\\') => Some(i + 2),
                    Some(_) => Some(i),
                    None => None,
                };
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether `b` is an unfinished reply string that has content already.
fn is_string_with_content(b: &[u8]) -> bool {
    b.len() > 2 && b[0] == 0x1b && matches!(b[1], b'P' | b']' | b'_')
}

/// A mouse report: button bits `cb`, 1-based `x` and `y`, and whether it
/// is an SGR release (`m`).
fn mouse(cb: u32, x: u32, y: u32, release: bool) -> Option<MouseEvent> {
    // Buttons 8 to 11 (back, forward, ...) are not used.
    if cb & 128 != 0 {
        return None;
    }
    let button = |low: u32| match low {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    };
    let low = cb & 3;
    let kind = if cb & 64 != 0 {
        match low {
            0 => MouseKind::ScrollUp,
            1 => MouseKind::ScrollDown,
            2 => MouseKind::ScrollLeft,
            _ => MouseKind::ScrollRight,
        }
    } else if cb & 32 != 0 {
        match button(low) {
            Some(b) => MouseKind::Drag(b),
            None => MouseKind::Moved,
        }
    } else if release {
        MouseKind::Up(button(low).unwrap_or(MouseButton::Left))
    } else {
        match button(low) {
            Some(b) => MouseKind::Down(b),
            // X10 and urxvt releases do not say which button.
            None => MouseKind::Up(MouseButton::Left),
        }
    };
    let mut mods = Mods::empty();
    if cb & 4 != 0 {
        mods |= Mods::SHIFT;
    }
    if cb & 8 != 0 {
        mods |= Mods::ALT;
    }
    if cb & 16 != 0 {
        mods |= Mods::CTRL;
    }
    let cell = |v: u32| u16::try_from(v.saturating_sub(1)).unwrap_or(u16::MAX);
    Some(MouseEvent {
        kind,
        x: cell(x),
        y: cell(y),
        mods,
    })
}

/// CSI parameters: `;`-separated, each with `:`-separated parts.
#[derive(Debug, Default)]
struct Params {
    /// (value, given) per part.
    params: Vec<Vec<(u32, bool)>>,
}

impl Params {
    fn parse(b: &[u8]) -> Self {
        if b.is_empty() {
            return Self::default();
        }
        let params = b
            .split(|&c| c == b';')
            .map(|p| {
                p.split(|&c| c == b':')
                    .map(|part| {
                        let value = part.iter().fold(0u32, |v, &d| {
                            if d.is_ascii_digit() {
                                v.saturating_mul(10).saturating_add(u32::from(d - b'0'))
                            } else {
                                v
                            }
                        });
                        (value, !part.is_empty())
                    })
                    .collect()
            })
            .collect();
        Self { params }
    }

    fn is_empty(&self) -> bool {
        self.params.is_empty()
    }

    fn len(&self) -> usize {
        self.params.len()
    }

    /// Part `part` of parameter `i`, 0 if absent.
    fn get(&self, i: usize, part: usize) -> u32 {
        self.params
            .get(i)
            .and_then(|p| p.get(part))
            .map_or(0, |&(v, _)| v)
    }

    fn has(&self, i: usize, part: usize) -> bool {
        self.params
            .get(i)
            .and_then(|p| p.get(part))
            .is_some_and(|&(_, given)| given)
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The length of the longest end of `hay` that starts `needle`.
fn partial_suffix(hay: &[u8], needle: &[u8]) -> usize {
    (1..needle.len().min(hay.len() + 1))
        .rev()
        .find(|&k| hay.ends_with(&needle[..k]))
        .unwrap_or(0)
}

/// Where to cut `b` so no UTF-8 character is split: before a final
/// character that is not complete yet.
fn char_boundary(b: &[u8]) -> usize {
    let len = b.len();
    for back in 1..=len.min(4) {
        let c = b[len - back];
        if c & 0xc0 == 0x80 {
            continue;
        }
        let need = match c {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if need > back { len - back } else { len };
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<Input> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        d.feed(bytes, &mut out);
        d.flush(&mut out);
        out
    }

    fn keys(bytes: &[u8]) -> Vec<KeyEvent> {
        decode(bytes)
            .into_iter()
            .filter_map(|i| match i {
                Input::Key { key, .. } => Some(key),
                _ => None,
            })
            .collect()
    }

    fn k(spec: &str) -> KeyEvent {
        spec.parse().expect("valid key spec")
    }

    #[test]
    fn legacy_bytes() {
        assert_eq!(keys(b"a%N"), vec![k("a"), k("%"), k("N")]);
        assert_eq!(
            keys(b"\r\t\x7f\x02\x00\x08\n"),
            vec![
                k("Enter"),
                k("Tab"),
                k("BSpace"),
                k("C-b"),
                k("C-Space"),
                k("C-h"),
                k("C-j")
            ]
        );
        assert_eq!(keys(b"\x1c\x1f"), vec![k("C-4"), k("C-7")]);
        assert_eq!(keys("é€😀".as_bytes()), vec![k("é"), k("€"), k("😀")]);
        assert_eq!(keys(b"\x1b"), vec![k("Escape")]);
        assert_eq!(
            keys(b"\x1bx\x1b\r\x1b\x02"),
            vec![k("M-x"), k("M-Enter"), k("C-M-b")]
        );
        assert_eq!(keys("\x1bé".as_bytes()), vec![k("M-é")]);
        assert_eq!(keys(b"\x1b\x1b"), vec![k("M-Escape")]);
        // Invalid UTF-8 is skipped.
        assert_eq!(keys(b"\xffa\xc3"), vec![k("a")]);
    }

    #[test]
    fn xterm_keys() {
        let table: &[(&[u8], &str)] = &[
            (b"\x1b[A", "Up"),
            (b"\x1bOB", "Down"),
            (b"\x1b[1;5C", "C-Right"),
            (b"\x1b[1;3D", "M-Left"),
            (b"\x1b[1;2H", "S-Home"),
            (b"\x1b[F", "End"),
            (b"\x1b[2~", "IC"),
            (b"\x1b[3;5~", "C-DC"),
            (b"\x1b[5~", "PPage"),
            (b"\x1b[6~", "NPage"),
            (b"\x1b[1~", "Home"),
            (b"\x1b[4~", "End"),
            (b"\x1bOP", "F1"),
            (b"\x1b[1;5P", "C-F1"),
            (b"\x1bOR", "F3"),
            (b"\x1b[13~", "F3"),
            (b"\x1b[15~", "F5"),
            (b"\x1b[24;2~", "S-F12"),
            (b"\x1b[Z", "BTab"),
            (b"\x1b\x1b[A", "M-Up"),
            (b"\x1b[27;5;9~", "C-Tab"),
            (b"\x1bOM", "Enter"),
            (b"\x1bOq", "1"),
        ];
        for (bytes, want) in table {
            assert_eq!(keys(bytes), vec![k(want)], "{bytes:?}");
        }
    }

    #[test]
    fn kitty_keys() {
        let table: &[(&[u8], &str)] = &[
            (b"\x1b[27u", "Escape"),
            (b"\x1b[27;3u", "M-Escape"),
            (b"\x1b[97;5u", "C-a"),
            (b"\x1b[97;6u", "C-a"),
            (b"\x1b[97;4u", "M-A"),
            (b"\x1b[97;7u", "C-M-a"),
            (b"\x1b[13;2u", "S-Enter"),
            (b"\x1b[9;2u", "BTab"),
            (b"\x1b[127;5u", "C-BSpace"),
            (b"\x1b[32;5u", "C-Space"),
            (b"\x1b[64;5u", "C-Space"),
            (b"\x1b[49:33;4u", "M-!"),
            (b"\x1b[57399;5u", "C-0"),
            (b"\x1b[57414u", "Enter"),
            (b"\x1b[57376u", "F13"),
            (b"\x1b[1;5:2A", "C-Up"),
        ];
        for (bytes, want) in table {
            assert_eq!(keys(bytes), vec![k(want)], "{bytes:?}");
        }
        // The text the terminal reports wins (flag 16); Shift is kept in
        // `shifted` even when the terminal gave no shifted key.
        assert_eq!(keys(b"\x1b[97;;229u"), vec![k("å")]);
        assert_eq!(
            decode(b"\x1b[97;6u"),
            vec![Input::Key {
                key: k("C-a"),
                kind: KeyKind::Press,
                alternates: KeyAlternates {
                    unshifted: Some('a'),
                    shifted: Some('A'),
                    base: None,
                },
            }]
        );
        // Modifier keys alone and lock keys are not keys here.
        assert!(decode(b"\x1b[57441;2u\x1b[57358u").is_empty());
        let release = decode(b"\x1b[97;1:3u");
        assert_eq!(
            release,
            vec![Input::Key {
                key: k("a"),
                kind: KeyKind::Release,
                alternates: KeyAlternates {
                    unshifted: Some('a'),
                    ..KeyAlternates::default()
                },
            }]
        );
        let alternates = decode(b"\x1b[1092:1060:97;2u");
        assert_eq!(
            alternates,
            vec![Input::Key {
                key: k("Ф"),
                kind: KeyKind::Press,
                alternates: KeyAlternates {
                    unshifted: Some('ф'),
                    shifted: Some('Ф'),
                    base: Some('a'),
                },
            }]
        );
    }

    #[test]
    fn mouse_reports() {
        let m = |kind, x, y, mods| Input::Mouse(MouseEvent { kind, x, y, mods });
        assert_eq!(
            decode(b"\x1b[<0;3;4M\x1b[<0;3;4m\x1b[<32;5;6M\x1b[<35;1;1M\x1b[<65;2;2M\x1b[<20;1;1M"),
            vec![
                m(MouseKind::Down(MouseButton::Left), 2, 3, Mods::empty()),
                m(MouseKind::Up(MouseButton::Left), 2, 3, Mods::empty()),
                m(MouseKind::Drag(MouseButton::Left), 4, 5, Mods::empty()),
                m(MouseKind::Moved, 0, 0, Mods::empty()),
                m(MouseKind::ScrollDown, 1, 1, Mods::empty()),
                m(
                    MouseKind::Down(MouseButton::Left),
                    0,
                    0,
                    Mods::SHIFT | Mods::CTRL
                ),
            ]
        );
        // urxvt and X10.
        assert_eq!(
            decode(b"\x1b[34;10;5M\x1b[M\x20\x21\x22"),
            vec![
                m(MouseKind::Down(MouseButton::Right), 9, 4, Mods::empty()),
                m(MouseKind::Down(MouseButton::Left), 0, 1, Mods::empty()),
            ]
        );
    }

    #[test]
    fn focus_paste_and_replies() {
        assert_eq!(
            decode(b"\x1b[I\x1b[O"),
            vec![Input::Focus(true), Input::Focus(false)]
        );
        assert_eq!(
            decode(b"x\x1b[200~line\r\x1b[Atwo\x1b[201~y"),
            vec![
                Input::press(k("x")),
                Input::Paste("line\r\x1b[Atwo".into()),
                Input::press(k("y")),
            ]
        );
        // Late replies never become keys.
        let replies: &[u8] = b"\x1b[?62;4c\x1b[?1u\x1b[?2026;2$y\x1b[4;600;800t\
            \x1b]11;rgb:0000/0000/0000\x07\x1b]11;rgb:1/2/3\x1b\\\
            \x1bP>|kitty(0.40)\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?1;2c";
        assert_eq!(decode(replies), vec![]);
        let mut with_key = replies.to_vec();
        with_key.push(b'q');
        assert_eq!(keys(&with_key), vec![k("q")]);
    }

    #[test]
    fn waits_for_the_rest_and_settles_on_timeout() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        d.feed(b"\x1b", &mut out);
        assert!(out.is_empty());
        assert_eq!(d.timeout(), Some(ESC_TIMEOUT));
        d.feed(b"[", &mut out);
        d.feed(b"1;5", &mut out);
        assert!(out.is_empty());
        d.feed(b"A", &mut out);
        assert_eq!(out, vec![Input::press(k("C-Up"))]);
        assert_eq!(d.timeout(), None);

        // Settled: ESC [ cut short is Alt+[ and the rest typed.
        out.clear();
        d.feed(b"\x1b[1", &mut out);
        d.flush(&mut out);
        assert_eq!(out, vec![Input::press(k("M-[")), Input::press(k("1"))]);

        // A reply string waits longer once it has content, and is dropped
        // if it never ends; ESC ] alone is Alt+].
        out.clear();
        d.feed(b"\x1b]11;rgb:", &mut out);
        assert_eq!(d.timeout(), Some(STRING_TIMEOUT));
        d.flush(&mut out);
        assert!(out.is_empty());
        d.feed(b"\x1b]", &mut out);
        d.flush(&mut out);
        assert_eq!(out, vec![Input::press(k("M-]"))]);
        assert_eq!(d.timeout(), None);
    }

    #[test]
    fn pastes_in_pieces() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        d.feed(b"\x1b[200~ab", &mut out);
        assert_eq!(d.timeout(), Some(PASTE_TIMEOUT));
        d.feed(b"c\x1b[20", &mut out);
        assert!(out.is_empty());
        d.feed(b"1~", &mut out);
        assert_eq!(out, vec![Input::Paste("abc".into())]);

        // A paste whose end never comes is delivered when it goes quiet,
        // and the keyboard works again.
        out.clear();
        d.feed(b"\x1b[200~lost", &mut out);
        d.flush(&mut out);
        d.feed(b"k", &mut out);
        assert_eq!(out, vec![Input::Paste("lost".into()), Input::press(k("k"))]);

        // Long pastes come in parts, never splitting a character.
        out.clear();
        let mut big = b"\x1b[200~".to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_PASTE_PART - 1));
        big.extend("é".as_bytes());
        big.extend(b"tail\x1b[201~");
        d.feed(&big, &mut out);
        let text: String = out
            .iter()
            .map(|i| match i {
                Input::Paste(t) => t.as_str(),
                other => panic!("not a paste: {other:?}"),
            })
            .collect();
        assert!(out.len() >= 2);
        assert!(text.ends_with("xétail"), "{}", &text[text.len() - 8..]);
        assert!(!text.contains('\u{fffd}'));
    }

    #[test]
    fn long_strings_are_skipped() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        let mut bytes = b"\x1bP".to_vec();
        bytes.extend(std::iter::repeat_n(b'z', MAX_STRING + 10));
        d.feed(&bytes, &mut out);
        assert!(d.buf.is_empty(), "a long string is not kept");
        d.feed(b"zz\x1b", &mut out);
        d.feed(b"\\ok", &mut out);
        assert_eq!(out, vec![Input::press(k("o")), Input::press(k("k"))]);
        // Alt+P just before another sequence is a key.
        assert_eq!(keys(b"\x1bP\x1b[A"), vec![k("M-P"), k("Up")]);
    }

    #[test]
    fn long_sequences_end_at_the_same_place_however_split() {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', 300));
        bytes.push(b'A');
        let whole = decode(&bytes);
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for chunk in bytes.chunks(100) {
            d.feed(chunk, &mut out);
        }
        d.flush(&mut out);
        assert_eq!(out, whole);
        assert_eq!(whole.last(), Some(&Input::press(k("A"))));
    }

    #[test]
    fn helpers() {
        assert_eq!(partial_suffix(b"abc\x1b[2", PASTE_END), 3);
        assert_eq!(partial_suffix(b"abc", PASTE_END), 0);
        assert_eq!(char_boundary("aé".as_bytes()), 3);
        assert_eq!(char_boundary(&"aé".as_bytes()[..2]), 1);
        assert_eq!(char_boundary(b"ab"), 2);
    }

    proptest::proptest! {
        /// However the bytes are split, the events are the same.
        #[test]
        fn splitting_does_not_change_events(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..200),
            cuts in proptest::collection::vec(0usize..200, 0..8),
        ) {
            let whole = decode(&bytes);
            let mut d = Decoder::new();
            let mut out = Vec::new();
            let mut cuts: Vec<usize> = cuts.into_iter().filter(|&c| c <= bytes.len()).collect();
            cuts.sort_unstable();
            let mut at = 0;
            for c in cuts {
                d.feed(&bytes[at..c], &mut out);
                at = c;
            }
            d.feed(&bytes[at..], &mut out);
            d.flush(&mut out);
            proptest::prop_assert_eq!(whole, out);
        }

        /// Sequences made of the pieces terminals send decode without
        /// panicking, the same however they are split, and leave nothing
        /// pending after a flush.
        #[test]
        fn sequences_settle(parts in proptest::collection::vec(
            proptest::sample::select(vec![
                &b"\x1b"[..], b"[", b"O", b"P", b"]", b"_", b"\\", b"<", b"?", b";", b":",
                b"1", b"5", b"27", b"200", b"201", b"~", b"u", b"M", b"m", b"A", b"\x07",
                b"a", "é".as_bytes(), b"\xe2",
            ]),
            0..200,
        ), step in 1usize..64) {
            let bytes: Vec<u8> = parts.concat();
            let settle = |chunks: &mut dyn Iterator<Item = &[u8]>| {
                let mut d = Decoder::new();
                let mut out = Vec::new();
                for chunk in chunks {
                    d.feed(chunk, &mut out);
                }
                d.flush(&mut out);
                // A paste interrupted by the flush ends there; anything
                // else is settled at once.
                if d.timeout().is_some() {
                    d.flush(&mut out);
                }
                (out, d.timeout())
            };
            let (whole, pending) = settle(&mut std::iter::once(&bytes[..]));
            proptest::prop_assert_eq!(pending, None);
            let (split, _) = settle(&mut bytes.chunks(step));
            proptest::prop_assert_eq!(whole, split);
        }
    }
}
