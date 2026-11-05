// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages between the server (the desktop) and its frontends.
//!
//! All messages are serde types so the M3 Unix-socket transport can encode
//! them with postcard unchanged (DECISIONS.md D-010, D-026).

use serde::{Deserialize, Serialize};
use termos_render::caps::{Caps, ImageProtocol};
use termos_render::diff::Span;
use termos_render::encode::CursorState;

use crate::input::decode::{KeyAlternates, KeyKind};
use crate::input::{KeyEvent, MouseEvent};

/// The frontend protocol: bumped on any incompatible change to these
/// messages. (Control clients and plugins speak JSON-RPC, versioned
/// separately by [`crate::wire::RPC_VERSION`].)
///
/// - 2: capabilities v2 (graphics, pixel sizes, background colour,
///   remote/tmux/reduced-motion flags).
/// - 3: images (`FrameUpdate::images`, `ImageData`, `ImageFree`).
/// - 4: Kitty keyboard details (`ClientMsg::KeyDetail`,
///   `ServerMsg::KeyboardFlags`).
/// - 5: system notifications (`ServerMsg::Notify`).
pub const PROTOCOL_VERSION: u32 = 5;

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
    /// Every pixel image the client shows after this frame, when that
    /// changed (`None`: same as before). Drawn after the cells.
    pub images: Option<Vec<ImagePlacement>>,
}

/// One image (or one visible piece of it) on a client's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImagePlacement {
    /// The image data (see [`ServerMsg::ImageData`]).
    pub image: u32,
    pub protocol: ImageProtocol,
    /// Top-left cell and size in cells.
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    /// The part of the image shown, in its pixels: x, y, width, height.
    pub src: [u32; 4],
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
    /// A key with more than [`ClientMsg::Key`] says: a repeat or a
    /// release, or the alternate keys a Kitty terminal reported (D-103).
    KeyDetail {
        key: KeyEvent,
        kind: KeyKind,
        alternates: KeyAlternates,
    },
}

/// Server → frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    Frame(FrameUpdate),
    /// Put text on the system clipboard (the client emits OSC 52).
    Clipboard(String),
    Bell,
    /// An agent needs the user: show a system notification where the
    /// terminal can (OSC 9, 99 or 777, D-116).
    Notify {
        title: String,
        body: String,
    },
    /// Set the outer terminal's title.
    Title(String),
    /// The session ended; the client should restore the terminal and exit.
    Exit {
        reason: String,
    },
    /// The client was detached (`C-b d`, `termos detach`); the session keeps
    /// running in the server.
    Detached {
        session: String,
    },
    /// Part of image `id` (PNG), at most 1 MiB; `last` completes it.
    ImageData {
        id: u32,
        chunk: Vec<u8>,
        last: bool,
    },
    /// Image `id` is no longer needed.
    ImageFree {
        id: u32,
    },
    /// The Kitty keyboard flags beyond disambiguation (2, 4, 8, 16) the
    /// program in this client's focused pane asked for. A client whose
    /// terminal speaks the protocol sets them there (D-103).
    KeyboardFlags(u8),
}
