// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-core
//!
//! Shared foundations for the TermOS server and its frontends:
//!
//! - [`input`]: transport-neutral key/mouse events and tmux key notation.
//! - [`proto`]: the messages between server and frontends.
//! - [`transport`]: the channel pair a frontend and the server talk over
//!   (in-process in M1; a socket adapter bridges to it in M3).
//! - [`config`] and [`watch`]: `config.toml` with precise errors and hot
//!   reload.
//! - [`paths`]: XDG directories. [`log`]: file logging.
//! - [`app`]: the in-process [`app::App`] trait for windows drawn by Rust
//!   code with ratatui widgets.
//!
//! The server event loop lives with the desktop state in `termos-shell`;
//! see `ARCHITECTURE.md` §3 and §7.

pub mod app;
pub mod config;
pub mod input;
pub mod log;
pub mod paths;
pub mod proto;
pub mod transport;
pub mod watch;
