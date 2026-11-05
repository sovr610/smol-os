// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-core
//!
//! Shared foundations for the TermOS server and its frontends:
//!
//! - [`ids`]: identifiers such as [`ids::PaneId`].
//! - [`input`]: transport-neutral key/mouse events and tmux key notation.
//! - [`proto`]: the messages between server and frontends.
//! - [`wire`]: the session-socket format: handshake, length-prefixed
//!   postcard frames, JSON-RPC 2.0.
//! - [`transport`]: the channel pair a frontend and the server talk over,
//!   in-process or bridged to the socket, and control calls.
//! - [`config`] and [`watch`]: `config.toml` with precise errors and hot
//!   reload.
//! - [`local`]: local IPC endpoints (Unix sockets, Windows named pipes)
//!   with peer identity.
//! - [`paths`]: XDG directories. [`log`]: file logging.
//! - [`app`]: the in-process [`app::App`] trait for windows drawn by Rust
//!   code with ratatui widgets.
//!
//! The server event loop lives with the desktop state in `termos-shell`;
//! see `ARCHITECTURE.md` §3 and §7.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod ids;
pub mod input;
pub mod journal;
pub mod local;
pub mod log;
pub mod models_endpoint;
pub mod paths;
pub mod proto;
pub mod secret_env;
pub mod transport;
pub mod watch;
pub mod widgets;
pub mod wire;
