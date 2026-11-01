// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages between the server (the desktop) and its frontends.
//!
//! All messages are serde types so the M3 Unix-socket transport can encode
//! them with postcard unchanged (DECISIONS.md D-010, D-026).

use serde::{Deserialize, Serialize};
use termos_render::caps::Caps;
use termos_render::diff::Span;
use termos_render::encode::CursorState;

use crate::input::{KeyEvent, MouseEvent};

/// Bumped on any incompatible change to these messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// A frame for one client: the cells that changed since the last frame this
/// client received, plus where the cursor goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameUpdate {
    pub cols: u16,
    pub rows: u16,
    /// Erase the screen before applying `spans` (first frame, resize).
    pub clear: bool,
    pub spans: Vec<Span>,
    /// `None` hides the cursor.
    pub cursor: Option<CursorState>,
}

/// Frontend → server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on every connection.
    Hello {
        version: u32,
        caps: Caps,
        cols: u16,
        rows: u16,
    },
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Bracketed paste from the outer terminal.
    Paste(String),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// The outer terminal gained (`true`) or lost focus.
    Focus(bool),
}

/// Server → frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    Frame(FrameUpdate),
    /// Put text on the system clipboard (the client emits OSC 52).
    Clipboard(String),
    Bell,
    /// Set the outer terminal's title.
    Title(String),
    /// The session ended; the client should restore the terminal and exit.
    Exit {
        reason: String,
    },
}
