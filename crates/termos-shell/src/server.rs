// SPDX-License-Identifier: GPL-3.0-or-later
//! The server event loop.
//!
//! Event-driven: the loop sleeps until a client message, PTY output, a
//! config change, or the desktop's next deadline. When something changed it
//! renders, at most once per [`FRAME_INTERVAL`] (output floods are parsed
//! continuously but drawn at frame rate), and sends the client only the
//! cells that differ from its last frame (D-026).

use std::time::{Duration, Instant};

use termos_core::proto::{ClientMsg, FrameUpdate, ServerMsg};
use termos_core::transport::ServerEnd;
use termos_render::diff::{diff, full};
use termos_render::encode::CursorState;
use termos_render::grid::Grid;
use termos_term::pty::PtyEvent;
use tokio::sync::mpsc;

use crate::desktop::Desktop;

/// Minimum time between frames (~120 fps cap).
pub const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// Event sources for the loop.
#[derive(Debug)]
pub struct ServerChannels {
    pub client: ServerEnd,
    pub pty_rx: mpsc::Receiver<PtyEvent>,
    /// Config-file change notifications (absent if watching failed).
    pub reload_rx: Option<mpsc::UnboundedReceiver<()>>,
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}

async fn recv_reload(rx: &mut Option<mpsc::UnboundedReceiver<()>>) -> Option<()> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Runs the desktop until the session ends or the client goes away.
/// Returns the reason.
pub async fn run(mut desktop: Desktop, mut ch: ServerChannels) -> String {
    let mut current = Grid::new(desktop.cols(), desktop.rows());
    let mut previous: Option<Grid> = None;
    let mut last_cursor: Option<CursorState> = None;
    let mut last_frame: Option<Instant> = None;
    let mut frame_due: Option<Instant> = None;
    let mut hello = false;

    loop {
        let timer = earliest(desktop.next_deadline(), frame_due);
        tokio::select! {
            msg = ch.client.rx.recv() => match msg {
                Some(m) => {
                    if matches!(m, ClientMsg::Hello { .. } | ClientMsg::Resize { .. }) {
                        // Size changed: next frame is a full redraw.
                        previous = None;
                        hello = true;
                    }
                    desktop.handle_client(m);
                }
                None => return "the client disconnected".into(),
            },
            Some(ev) = ch.pty_rx.recv() => {
                desktop.on_pty(ev);
                // Drain what is already queued so a burst becomes one frame.
                while let Ok(ev) = ch.pty_rx.try_recv() {
                    desktop.on_pty(ev);
                }
            },
            r = recv_reload(&mut ch.reload_rx) => match r {
                Some(()) => desktop.reload_config(),
                None => ch.reload_rx = None,
            },
            () = sleep_until(timer) => desktop.on_timer(Instant::now()),
        }

        for msg in desktop.take_outbox() {
            if ch.client.tx.send(msg).is_err() {
                return "the client disconnected".into();
            }
        }
        if let Some(reason) = desktop.quit_reason() {
            let reason = reason.to_owned();
            let _ = ch.client.tx.send(ServerMsg::Exit {
                reason: reason.clone(),
            });
            return reason;
        }
        if !hello {
            // Nothing to draw until the client says how big it is.
            continue;
        }

        let now = Instant::now();
        if desktop.take_dirty() {
            // Draw now, unless the last frame was less than an interval ago.
            let due = last_frame.map_or(now, |t| (t + FRAME_INTERVAL).max(now));
            frame_due = Some(frame_due.map_or(due, |d| d.min(due)));
        }
        if frame_due.is_some_and(|d| d <= now) {
            frame_due = None;
            last_frame = Some(now);
            let cursor = desktop.render(&mut current);
            let (spans, clear) = match &previous {
                Some(prev) if prev.cols() == current.cols() && prev.rows() == current.rows() => {
                    (diff(prev, &current), false)
                }
                _ => (full(&current), true),
            };
            if clear || !spans.is_empty() || cursor != last_cursor {
                let frame = FrameUpdate {
                    cols: current.cols(),
                    rows: current.rows(),
                    clear,
                    spans,
                    cursor,
                };
                if ch.client.tx.send(ServerMsg::Frame(frame)).is_err() {
                    return "the client disconnected".into();
                }
            }
            last_cursor = cursor;
            match &mut previous {
                Some(prev) => std::mem::swap(prev, &mut current),
                None => previous = Some(current.clone()),
            }
        }
    }
}
