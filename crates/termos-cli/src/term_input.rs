// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads the user's terminal on a thread of its own and decodes it with
//! [`Decoder`] (D-102). Every platform takes the same path: the terminal's
//! bytes. On Unix they are read from stdin. On Windows the console runs in
//! VT input mode (`ENABLE_VIRTUAL_TERMINAL_INPUT`), so its key records carry
//! those same bytes, and bracketed paste and the Kitty keyboard protocol
//! work there too.
//!
//! The thread blocks until input arrives or the reader is dropped, so an
//! idle terminal costs nothing; it waits with a timeout only while a
//! sequence is unfinished (a lone ESC, say).

use std::io;
use std::thread::JoinHandle;
use std::time::Duration;

use termos_core::input::decode::{Decoder, Input, KeyAlternates, KeyKind};
use termos_core::input::{KeyCode, KeyEvent, MouseEvent};
use termos_core::proto::ClientMsg;
use tokio::sync::mpsc;

/// Terminal modes the client turns on: mouse reports (button, drag, all
/// motion; urxvt and SGR encodings), bracketed paste and focus reports.
pub(crate) const MODES_ON: &[u8] =
    b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h\x1b[?2004h\x1b[?1004h";
/// The inverse of [`MODES_ON`], in reverse order.
pub(crate) const MODES_OFF: &[u8] =
    b"\x1b[?1004l\x1b[?2004l\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
/// Kitty keyboard protocol: push flag 1 (disambiguate), and pop it.
pub(crate) const KITTY_PUSH: &[u8] = b"\x1b[>1u";
pub(crate) const KITTY_POP: &[u8] = b"\x1b[<1u";

/// What the reader thread delivers.
#[derive(Debug)]
pub(crate) enum Incoming {
    Input(Input),
    /// The terminal changed size (Windows; Unix has SIGWINCH).
    Resized,
    /// The bytes as they came, before decoding (for `termos debug keys`).
    Raw(Vec<u8>),
    /// The terminal closed.
    Closed,
    Failed(io::Error),
}

/// One thing read from the platform.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Bytes(Vec<u8>),
    /// A console mouse record (Windows consoles that do not send VT mouse
    /// reports).
    #[cfg_attr(not(windows), allow(dead_code))]
    Mouse(MouseEvent),
    /// A console resize record (Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    Resized,
}

/// What waiting for input ended with.
pub(crate) enum Wait {
    Ready,
    Timeout,
    Stopped,
}

/// The platform's terminal input.
pub(crate) trait Source: Send {
    /// Waits until input is ready, the reader is stopped, or `timeout`
    /// (if any) passes.
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Wait>;
    /// Reads what is ready into `pieces`; `false` at the end of input.
    fn read(&mut self, pieces: &mut Vec<Piece>) -> io::Result<bool>;
}

/// The terminal's input source, and the handle that stops waiting on it.
pub(crate) fn open() -> io::Result<(impl Source, Stop)> {
    sys::open()
}

/// Stops the reader thread's wait when dropped.
pub(crate) struct Stop {
    _inner: sys::Stop,
}

/// Reads and decodes the terminal on a thread until dropped.
pub(crate) struct Reader {
    rx: mpsc::UnboundedReceiver<Incoming>,
    stop: Option<Stop>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Starts reading; with `raw`, the undecoded bytes are delivered too.
    pub(crate) fn start(raw: bool) -> io::Result<Self> {
        let (source, stop) = open()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let thread = std::thread::Builder::new()
            .name("termos-input".into())
            .spawn(move || pump(source, &tx, raw))?;
        Ok(Self {
            rx,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub(crate) async fn next(&mut self) -> Option<Incoming> {
        self.rx.recv().await
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn pump(mut source: impl Source, tx: &mpsc::UnboundedSender<Incoming>, raw: bool) {
    let mut decoder = Decoder::new();
    let mut pieces = Vec::new();
    let mut events = Vec::new();
    let deliver = |events: &mut Vec<Input>| {
        events
            .drain(..)
            .all(|e| tx.send(Incoming::Input(e)).is_ok())
    };
    loop {
        let open = match source.wait(decoder.timeout()) {
            Ok(Wait::Stopped) => return,
            Ok(Wait::Timeout) => {
                decoder.flush(&mut events);
                true
            }
            Ok(Wait::Ready) => match source.read(&mut pieces) {
                Ok(open) => open,
                Err(e) => {
                    let _ = tx.send(Incoming::Failed(e));
                    return;
                }
            },
            Err(e) => {
                let _ = tx.send(Incoming::Failed(e));
                return;
            }
        };
        for piece in pieces.drain(..) {
            match piece {
                Piece::Bytes(bytes) => {
                    decoder.feed(&bytes, &mut events);
                    if raw && tx.send(Incoming::Raw(bytes)).is_err() {
                        return;
                    }
                }
                Piece::Mouse(m) => events.push(Input::Mouse(m)),
                Piece::Resized => {
                    if !deliver(&mut events) || tx.send(Incoming::Resized).is_err() {
                        return;
                    }
                }
            }
        }
        if !open {
            decoder.flush(&mut events);
            if deliver(&mut events) {
                let _ = tx.send(Incoming::Closed);
            }
            return;
        }
        if !deliver(&mut events) {
            return;
        }
    }
}

/// The message an input event sends to the server, if any.
pub(crate) fn to_msg(input: Input) -> Option<ClientMsg> {
    match input {
        Input::Key { key, kind, .. } => {
            // Releases come only from a Kitty terminal asked for them
            // (flag 2), which the client does not ask for.
            (kind != KeyKind::Release).then_some(ClientMsg::Key(key))
        }
        Input::Mouse(m) => Some(ClientMsg::Mouse(m)),
        Input::Paste(text) => Some(ClientMsg::Paste(text)),
        Input::Focus(f) => Some(ClientMsg::Focus(f)),
    }
}

/// One line about an input event, for `termos debug keys`: keys in tmux
/// notation, and no pasted text.
pub(crate) fn describe(input: &Input) -> String {
    match input {
        Input::Key {
            key,
            kind,
            alternates,
        } => {
            let mut s = format!("key {key}");
            match kind {
                KeyKind::Press => {}
                KeyKind::Repeat => s.push_str(" (repeat)"),
                KeyKind::Release => s.push_str(" (release)"),
            }
            if *alternates != KeyAlternates::default() {
                let show = |c: Option<char>| c.map_or_else(|| "-".to_owned(), |c| c.to_string());
                s.push_str(&format!(
                    "  [unshifted {}, shifted {}, base {}]",
                    show(alternates.unshifted),
                    show(alternates.shifted),
                    show(alternates.base)
                ));
            }
            s
        }
        Input::Mouse(m) => {
            let mods = KeyEvent::new(KeyCode::F(1), m.mods).to_string();
            let mods = mods.strip_suffix("F1").unwrap_or_default();
            format!("mouse {mods}{:?} at column {}, row {}", m.kind, m.x, m.y)
        }
        Input::Paste(text) => format!("paste of {} characters", text.chars().count()),
        Input::Focus(true) => "focus in".to_owned(),
        Input::Focus(false) => "focus out".to_owned(),
    }
}

/// Turns the console into VT input mode (Windows); nothing elsewhere.
/// [`restore_console`] undoes it.
pub(crate) fn enable_vt_input() -> io::Result<()> {
    sys::enable_vt_input()
}

/// Gives the console back the input mode it had (Windows); safe to call
/// twice, and a no-op elsewhere.
pub(crate) fn restore_console() {
    sys::restore_console();
}

/// Windows console records as the decoder's input. Plain data, so the
/// rules are tested on every platform.
#[cfg(any(windows, test))]
pub(crate) mod records {
    use termos_core::input::{Mods, MouseButton, MouseEvent, MouseKind};

    use super::Piece;

    // The values of the Windows constants these rules read.
    const RIGHT_ALT: u32 = 0x1;
    const LEFT_ALT: u32 = 0x2;
    const RIGHT_CTRL: u32 = 0x4;
    const LEFT_CTRL: u32 = 0x8;
    const SHIFT: u32 = 0x10;
    const LEFT_BUTTON: u32 = 0x1;
    const RIGHT_BUTTON: u32 = 0x2;
    const MIDDLE_BUTTON: u32 = 0x4;
    const MOUSE_MOVED: u32 = 0x1;
    const DOUBLE_CLICK: u32 = 0x2;
    const MOUSE_WHEELED: u32 = 0x4;
    const MOUSE_HWHEELED: u32 = 0x8;

    /// Collects the text of key records as UTF-8 bytes.
    #[derive(Debug, Default)]
    pub(crate) struct Records {
        bytes: Vec<u8>,
        /// A UTF-16 high surrogate waiting for its pair.
        high: Option<u16>,
        /// The mouse buttons held at the last mouse record.
        buttons: u32,
    }

    impl Records {
        /// A key record. In VT input mode the console turns keys into the
        /// characters of their sequences: key-down records whose character
        /// is the text. A record with no character is a key that types
        /// nothing (Shift alone) unless it has no virtual key either: then
        /// it carries the NUL of Ctrl+Space.
        pub(crate) fn key(&mut self, down: bool, repeat: u16, vk: u16, unit: u16) {
            if !down || (unit == 0 && vk != 0) {
                return;
            }
            for _ in 0..repeat.max(1) {
                self.unit(unit);
            }
        }

        fn unit(&mut self, unit: u16) {
            let c = match unit {
                0xd800..=0xdbff => {
                    self.high = Some(unit);
                    return;
                }
                0xdc00..=0xdfff => match self.high.take() {
                    Some(high) => char::decode_utf16([high, unit]).next().and_then(Result::ok),
                    None => None,
                },
                _ => {
                    self.high = None;
                    char::from_u32(u32::from(unit))
                }
            };
            if let Some(c) = c {
                let mut buf = [0u8; 4];
                self.bytes
                    .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }

        /// A mouse record, for consoles that send these instead of VT
        /// mouse reports. `x` and `y` are 0-based cells.
        pub(crate) fn mouse(
            &mut self,
            pieces: &mut Vec<Piece>,
            (x, y): (i16, i16),
            buttons: u32,
            control: u32,
            flags: u32,
        ) {
            self.flush(pieces);
            let held = buttons & (LEFT_BUTTON | RIGHT_BUTTON | MIDDLE_BUTTON);
            let first = |b: u32| {
                if b & LEFT_BUTTON != 0 {
                    Some(MouseButton::Left)
                } else if b & RIGHT_BUTTON != 0 {
                    Some(MouseButton::Right)
                } else if b & MIDDLE_BUTTON != 0 {
                    Some(MouseButton::Middle)
                } else {
                    None
                }
            };
            // The wheel's distance is the high word, signed.
            let wheel = (buttons >> 16) as u16 as i16;
            let kind = if flags & MOUSE_WHEELED != 0 {
                Some(if wheel > 0 {
                    MouseKind::ScrollUp
                } else {
                    MouseKind::ScrollDown
                })
            } else if flags & MOUSE_HWHEELED != 0 {
                Some(if wheel > 0 {
                    MouseKind::ScrollRight
                } else {
                    MouseKind::ScrollLeft
                })
            } else if flags & MOUSE_MOVED != 0 {
                Some(first(held).map_or(MouseKind::Moved, MouseKind::Drag))
            } else if flags == 0 || flags & DOUBLE_CLICK != 0 {
                let pressed = held & !self.buttons;
                let released = self.buttons & !held;
                first(pressed)
                    .map(MouseKind::Down)
                    .or_else(|| first(released).map(MouseKind::Up))
            } else {
                None
            };
            if flags & (MOUSE_WHEELED | MOUSE_HWHEELED) == 0 {
                self.buttons = held;
            }
            let Some(kind) = kind else { return };
            let mut mods = Mods::empty();
            if control & SHIFT != 0 {
                mods |= Mods::SHIFT;
            }
            if control & (LEFT_CTRL | RIGHT_CTRL) != 0 {
                mods |= Mods::CTRL;
            }
            if control & (LEFT_ALT | RIGHT_ALT) != 0 {
                mods |= Mods::ALT;
            }
            let cell = |v: i16| u16::try_from(v).unwrap_or(0);
            pieces.push(Piece::Mouse(MouseEvent {
                kind,
                x: cell(x),
                y: cell(y),
                mods,
            }));
        }

        /// A resize record.
        pub(crate) fn resized(&mut self, pieces: &mut Vec<Piece>) {
            self.flush(pieces);
            pieces.push(Piece::Resized);
        }

        /// Moves the text collected so far into `pieces`.
        pub(crate) fn flush(&mut self, pieces: &mut Vec<Piece>) {
            if !self.bytes.is_empty() {
                pieces.push(Piece::Bytes(std::mem::take(&mut self.bytes)));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn key_records_become_bytes() {
            let mut r = Records::default();
            let mut pieces = Vec::new();
            // An arrow key as VT input mode delivers it: ESC [ A.
            for unit in [0x1b, u16::from(b'['), u16::from(b'A')] {
                r.key(true, 1, 0, unit);
            }
            // Key-ups and Shift alone type nothing; Ctrl+Space is NUL.
            r.key(false, 1, 0x41, u16::from(b'a'));
            r.key(true, 1, 0x10, 0);
            r.key(true, 1, 0, 0);
            // A repeated key, and an emoji as a surrogate pair.
            r.key(true, 2, 0x41, u16::from(b'a'));
            r.key(true, 1, 0, 0xd83d);
            r.key(true, 1, 0, 0xde00);
            // A lone low surrogate is dropped.
            r.key(true, 1, 0, 0xde00);
            r.flush(&mut pieces);
            assert_eq!(
                pieces,
                vec![Piece::Bytes("\x1b[A\0aa😀".as_bytes().to_vec())]
            );
        }

        #[test]
        fn mouse_records() {
            let mut r = Records::default();
            let mut pieces = Vec::new();
            r.key(true, 1, 0, u16::from(b'x'));
            r.mouse(&mut pieces, (3, 4), LEFT_BUTTON, LEFT_CTRL, 0);
            r.mouse(&mut pieces, (5, 4), LEFT_BUTTON, 0, MOUSE_MOVED);
            r.mouse(&mut pieces, (5, 4), 0, 0, 0);
            r.mouse(&mut pieces, (6, 6), 0, 0, MOUSE_MOVED);
            r.mouse(&mut pieces, (0, 0), 120 << 16, 0, MOUSE_WHEELED);
            r.mouse(&mut pieces, (0, 0), 0xff88 << 16, SHIFT, MOUSE_WHEELED);
            r.resized(&mut pieces);
            let m = |kind, x, y, mods| Piece::Mouse(MouseEvent { kind, x, y, mods });
            assert_eq!(
                pieces,
                vec![
                    Piece::Bytes(b"x".to_vec()),
                    m(MouseKind::Down(MouseButton::Left), 3, 4, Mods::CTRL),
                    m(MouseKind::Drag(MouseButton::Left), 5, 4, Mods::empty()),
                    m(MouseKind::Up(MouseButton::Left), 5, 4, Mods::empty()),
                    m(MouseKind::Moved, 6, 6, Mods::empty()),
                    m(MouseKind::ScrollUp, 0, 0, Mods::empty()),
                    m(MouseKind::ScrollDown, 0, 0, Mods::SHIFT),
                    Piece::Resized,
                ]
            );
        }
    }
}

#[cfg(unix)]
mod sys {
    use std::io;
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    use super::{Piece, Source, Wait};

    /// Stdin, and the socket whose closing stops the wait.
    pub(super) struct Stdin {
        wake: UnixStream,
    }

    /// Dropping it closes the other end of the wake socket.
    pub(super) struct Stop {
        _end: UnixStream,
    }

    pub(super) fn open() -> io::Result<(Stdin, super::Stop)> {
        let (wake, end) = UnixStream::pair()?;
        let stop = super::Stop {
            _inner: Stop { _end: end },
        };
        Ok((Stdin { wake }, stop))
    }

    impl Source for Stdin {
        fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Wait> {
            let stdin = io::stdin();
            let fd = stdin.as_fd();
            let timeout = timeout
                .map(Timespec::try_from)
                .transpose()
                .map_err(io::Error::other)?;
            let mut fds = [
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(&self.wake, PollFlags::IN),
            ];
            loop {
                match poll(&mut fds, timeout.as_ref()) {
                    Ok(0) => return Ok(Wait::Timeout),
                    Ok(_) => break,
                    Err(rustix::io::Errno::INTR) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if !fds[1].revents().is_empty() {
                return Ok(Wait::Stopped);
            }
            Ok(Wait::Ready)
        }

        fn read(&mut self, pieces: &mut Vec<Piece>) -> io::Result<bool> {
            let stdin = io::stdin();
            let mut buf = [0u8; 4096];
            match rustix::io::read(stdin.as_fd(), &mut buf) {
                Ok(0) => Ok(false),
                Ok(n) => {
                    pieces.push(Piece::Bytes(buf[..n].to_vec()));
                    Ok(true)
                }
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => Ok(true),
                Err(e) => Err(e.into()),
            }
        }
    }

    pub(super) fn enable_vt_input() -> io::Result<()> {
        Ok(())
    }

    pub(super) fn restore_console() {}
}

#[cfg(windows)]
mod sys {
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
        ENABLE_PROCESSED_INPUT, ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_WINDOW_INPUT, GetConsoleMode, GetNumberOfConsoleInputEvents, GetStdHandle,
        INPUT_RECORD, KEY_EVENT, MOUSE_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE, SetConsoleMode,
        WINDOW_BUFFER_SIZE_EVENT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
    };

    use super::records::Records;
    use super::{Piece, Source, Wait};

    /// The console's input mode before [`enable_vt_input`].
    static SAVED_MODE: Mutex<Option<u32>> = Mutex::new(None);

    /// A handle kept as an integer, so it can move to the reader thread.
    fn handle(h: usize) -> HANDLE {
        h as HANDLE
    }

    /// The console input handle (stdin); not owned, never closed.
    #[allow(unsafe_code)]
    fn console_input() -> io::Result<usize> {
        // SAFETY: GetStdHandle has no preconditions.
        let h = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        // SAFETY: `mode` is a valid place for the result; a handle that is
        // not a console makes the call fail.
        let ok = unsafe { GetConsoleMode(h, &mut mode) };
        if h.is_null() || ok == 0 {
            return Err(io::Error::other("stdin is not a console"));
        }
        Ok(h as usize)
    }

    /// An event object, closed when the last owner drops it.
    struct Event(usize);

    impl Drop for Event {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateEventW and is closed once.
            unsafe { CloseHandle(handle(self.0)) };
        }
    }

    /// The console input, and the event that stops the wait.
    pub(super) struct Console {
        input: usize,
        stop: Arc<Event>,
        records: Records,
    }

    /// Sets the stop event when dropped.
    pub(super) struct Stop(Arc<Event>);

    impl Drop for Stop {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the event handle is open while `self.0` lives.
            unsafe { SetEvent(handle((self.0).0)) };
        }
    }

    #[allow(unsafe_code)]
    pub(super) fn open() -> io::Result<(Console, super::Stop)> {
        let input = console_input()?;
        // SAFETY: default security, manual reset, not signaled, no name.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let stop = Arc::new(Event(event as usize));
        Ok((
            Console {
                input,
                stop: Arc::clone(&stop),
                records: Records::default(),
            },
            super::Stop { _inner: Stop(stop) },
        ))
    }

    impl Source for Console {
        #[allow(unsafe_code)]
        fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Wait> {
            let handles = [handle(self.input), handle(self.stop.0)];
            let ms = timeout.map_or(INFINITE, |t| {
                u32::try_from(t.as_millis())
                    .unwrap_or(INFINITE - 1)
                    .clamp(1, INFINITE - 1)
            });
            // SAFETY: two open handles, waiting for either.
            let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, ms) };
            match r {
                WAIT_OBJECT_0 => Ok(Wait::Ready),
                r if r == WAIT_OBJECT_0 + 1 => Ok(Wait::Stopped),
                WAIT_TIMEOUT => Ok(Wait::Timeout),
                _ => Err(io::Error::last_os_error()),
            }
        }

        #[allow(unsafe_code)]
        fn read(&mut self, pieces: &mut Vec<Piece>) -> io::Result<bool> {
            let input = handle(self.input);
            let mut count = 0u32;
            // SAFETY: an open console handle and a valid out pointer.
            if unsafe { GetNumberOfConsoleInputEvents(input, &mut count) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                // Signalled with nothing to read: let the console settle
                // rather than spin.
                std::thread::sleep(Duration::from_millis(5));
                return Ok(true);
            }
            let mut buf = [INPUT_RECORD::default(); 128];
            let mut got = 0u32;
            let len = u32::try_from(buf.len()).expect("128 fits in u32");
            // SAFETY: the buffer holds `len` records; at least one is
            // waiting, so this does not block.
            if unsafe { ReadConsoleInputW(input, buf.as_mut_ptr(), len, &mut got) } == 0 {
                return Err(io::Error::last_os_error());
            }
            for r in &buf[..usize::try_from(got).unwrap_or(0).min(buf.len())] {
                match u32::from(r.EventType) {
                    KEY_EVENT => {
                        // SAFETY: the event type says which member is set.
                        let k = unsafe { r.Event.KeyEvent };
                        // SAFETY: both members of the character union are
                        // plain integers; the W API fills UnicodeChar.
                        let unit = unsafe { k.uChar.UnicodeChar };
                        self.records
                            .key(k.bKeyDown != 0, k.wRepeatCount, k.wVirtualKeyCode, unit);
                    }
                    MOUSE_EVENT => {
                        // SAFETY: the event type says which member is set.
                        let m = unsafe { r.Event.MouseEvent };
                        self.records.mouse(
                            pieces,
                            (m.dwMousePosition.X, m.dwMousePosition.Y),
                            m.dwButtonState,
                            m.dwControlKeyState,
                            m.dwEventFlags,
                        );
                    }
                    WINDOW_BUFFER_SIZE_EVENT => self.records.resized(pieces),
                    // Focus and menu records are for the console's own use.
                    _ => {}
                }
            }
            self.records.flush(pieces);
            Ok(true)
        }
    }

    #[allow(unsafe_code)]
    pub(super) fn enable_vt_input() -> io::Result<()> {
        let input = handle(console_input()?);
        let mut mode = 0;
        // SAFETY: an open console handle and a valid out pointer.
        if unsafe { GetConsoleMode(input, &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if let Ok(mut saved) = SAVED_MODE.lock() {
            saved.get_or_insert(mode);
        }
        let vt = (mode
            | ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_WINDOW_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_EXTENDED_FLAGS)
            & !(ENABLE_QUICK_EDIT_MODE
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT);
        // SAFETY: an open console handle; any mode value is accepted or
        // refused.
        if unsafe { SetConsoleMode(input, vt) } == 0 {
            return Err(io::Error::other(format!(
                "this console has no VT input mode (Windows 10 1809 or later is needed): {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    pub(super) fn restore_console() {
        let Some(mode) = SAVED_MODE.lock().ok().and_then(|mut s| s.take()) else {
            return;
        };
        if let Ok(input) = console_input() {
            // SAFETY: an open console handle and the mode it had before.
            unsafe { SetConsoleMode(handle(input), mode) };
        }
    }
}

#[cfg(test)]
mod tests {
    use termos_core::input::{Mods, MouseButton, MouseKind};

    use super::*;

    #[test]
    fn events_become_messages_and_lines() {
        let key = |kind| Input::Key {
            key: KeyEvent::ctrl('b'),
            kind,
            alternates: KeyAlternates::default(),
        };
        assert_eq!(
            to_msg(key(KeyKind::Press)),
            Some(ClientMsg::Key(KeyEvent::ctrl('b')))
        );
        assert_eq!(
            to_msg(key(KeyKind::Repeat)),
            Some(ClientMsg::Key(KeyEvent::ctrl('b')))
        );
        assert_eq!(to_msg(key(KeyKind::Release)), None);
        assert_eq!(
            to_msg(Input::Paste("x".into())),
            Some(ClientMsg::Paste("x".into()))
        );
        assert_eq!(to_msg(Input::Focus(true)), Some(ClientMsg::Focus(true)));

        assert_eq!(describe(&key(KeyKind::Release)), "key C-b (release)");
        let mouse = Input::Mouse(MouseEvent {
            kind: MouseKind::Down(MouseButton::Left),
            x: 3,
            y: 4,
            mods: Mods::CTRL,
        });
        assert_eq!(describe(&mouse), "mouse C-Down(Left) at column 3, row 4");
        assert_eq!(
            describe(&Input::Paste("secret".into())),
            "paste of 6 characters"
        );
        let alt = Input::Key {
            key: KeyEvent::ch('Ф'),
            kind: KeyKind::Press,
            alternates: KeyAlternates {
                unshifted: Some('ф'),
                shifted: Some('Ф'),
                base: Some('a'),
            },
        };
        assert_eq!(describe(&alt), "key Ф  [unshifted ф, shifted Ф, base a]");
    }
}
