// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal frontend: owns the user's terminal, turns its input into
//! [`ClientMsg`]s and server frames into escape sequences.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::ClientEnd;
use termos_render::caps::Caps;
use termos_render::encode::{Encoder, osc52_copy, set_title};
use termos_render::probe::Replies;

use crate::term_input::{self, Incoming, KITTY_POP, KITTY_PUSH, MODES_OFF, MODES_ON};

/// Puts the terminal into desktop mode and restores it on drop (also when
/// unwinding from a panic).
pub(crate) struct TerminalGuard {
    kitty_keyboard: bool,
}

impl TerminalGuard {
    /// Raw mode (and VT input mode on Windows), then the capability probe
    /// (when `probe` gives a timeout), then desktop mode. Returns the
    /// probe's replies.
    pub(crate) fn enter(
        out: &mut impl Write,
        probe: Option<Duration>,
    ) -> io::Result<(Self, Replies)> {
        terminal::enable_raw_mode()?;
        if let Err(e) = term_input::enable_vt_input() {
            let _ = terminal::disable_raw_mode();
            return Err(e);
        }
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
        crossterm::execute!(out, EnterAlternateScreen)?;
        out.write_all(MODES_ON)?;
        if kitty_keyboard {
            out.write_all(KITTY_PUSH)?;
        }
        out.flush()?;
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
        let _ = out.write_all(KITTY_POP);
    }
    let _ = out.write_all(MODES_OFF);
    let _ = crossterm::execute!(out, LeaveAlternateScreen);
    // Reset attributes, show the cursor and restore the default cursor shape.
    let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[0 q");
    let _ = out.flush();
    term_input::restore_console();
    let _ = terminal::disable_raw_mode();
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.kitty_keyboard);
    }
}

/// Terminal size changes: SIGWINCH on Unix; on Windows they come with the
/// console's input records instead.
struct Resizes {
    #[cfg(unix)]
    winch: Option<tokio::signal::unix::Signal>,
}

impl Resizes {
    fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                winch: Some(signal(SignalKind::window_change())?),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    async fn next(&mut self) {
        #[cfg(unix)]
        if let Some(winch) = &mut self.winch {
            if winch.recv().await.is_some() {
                return;
            }
            self.winch = None;
        }
        std::future::pending::<()>().await;
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
/// `kitty_keyboard`: the terminal speaks the Kitty keyboard protocol (the
/// client pushed flag 1 on entering), so the flags a pane's program asks
/// for can be set there too.
pub(crate) async fn run(
    mut end: ClientEnd,
    caps: Caps,
    kitty_keyboard: bool,
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
    let notify_style = caps.notify_style(std::env::var("TERM_PROGRAM").ok().as_deref());
    let mut images = crate::images::ImageStore::default();
    let mut input = term_input::Reader::start(false)?;
    let mut resizes = Resizes::new()?;
    let mut size = (cols, rows);
    let resized = |end: &ClientEnd, size: &mut (u16, u16)| -> io::Result<()> {
        let now = terminal::size()?;
        if now != *size {
            *size = now;
            send(
                end,
                ClientMsg::Resize {
                    cols: now.0,
                    rows: now.1,
                },
            )?;
        }
        Ok(())
    };
    // Kitty keeps images until told to drop them.
    let leave = |images: &crate::images::ImageStore, out: &mut dyn Write| {
        let _ = out.write_all(images.cleanup());
        let _ = out.flush();
    };
    loop {
        tokio::select! {
            incoming = input.next() => match incoming {
                Some(Incoming::Input(i)) => {
                    if let Some(msg) = term_input::to_msg(i) {
                        send(&end, msg)?;
                    }
                }
                Some(Incoming::Resized) => resized(&end, &mut size)?,
                Some(Incoming::Raw(_)) => {}
                Some(Incoming::Failed(e)) => return Err(e),
                Some(Incoming::Closed) | None => {
                    return Ok(Ended::Exited("the terminal closed".into()));
                }
            },
            () = resizes.next() => resized(&end, &mut size)?,
            msg = end.rx.recv() => match msg {
                Some(ServerMsg::Frame(f)) => {
                    if let Some(links) = f.links {
                        encoder.set_links(links);
                    }
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
                Some(ServerMsg::KeyboardFlags(flags)) => {
                    if kitty_keyboard {
                        out.write_all(&term_input::kitty_set(flags))?;
                        out.flush()?;
                    }
                }
                Some(ServerMsg::Bell) => {
                    out.write_all(b"\x07")?;
                    out.flush()?;
                }
                Some(ServerMsg::Notify { title, body }) => {
                    // As a system notification, where the terminal shows
                    // them (D-116).
                    if let Some(style) = notify_style {
                        out.write_all(&termos_render::caps::notify_sequence(style, &title, &body))?;
                        out.flush()?;
                    }
                }
                Some(ServerMsg::Open { url }) => crate::opener::open(&url),
                // The host system locks and sleeps a terminal's machine
                // (D-123); the desktop sends these only to a console.
                Some(ServerMsg::Lock | ServerMsg::Suspend | ServerMsg::Reconfigure) => {}
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
