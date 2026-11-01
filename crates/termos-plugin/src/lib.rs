// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-plugin
//!
//! Out-of-process plugins for TermOS (DECISIONS.md D-034): the protocol
//! types and a small blocking SDK.
//!
//! A plugin is any program the server launches from a `plugin.toml`
//! manifest. It connects back to the session socket named by
//! `TERMOS_SOCKET`, says `hello` with the `TERMOS_PLUGIN_TOKEN` it was
//! given, and then speaks newline-delimited JSON-RPC 2.0:
//!
//! - plugin → server requests: `window.open`, `window.draw`,
//!   `window.close`, `status.set`, `command.register`, `notify.send`;
//! - server → plugin notifications: `window.key`, `window.mouse`,
//!   `window.resize`, `window.closed`, `command.invoked`.
//!
//! Any language can implement that; this crate does it for Rust:
//!
//! ```no_run
//! use termos_plugin::{Event, Line, Plugin};
//!
//! let mut p = Plugin::connect_from_env()?;
//! let w = p.open_window("hello", &[Line::from("Hello from a plugin")])?;
//! p.register_command("greet", "Say hello")?;
//! while let Some(event) = p.next_event()? {
//!     match event {
//!         Event::Command { name, .. } if name == "greet" => p.notify("hello!")?,
//!         Event::Closed { window } if window == w => break,
//!         _ => {}
//!     }
//! }
//! # Ok::<(), termos_plugin::Error>(())
//! ```
//!
//! The crate depends only on serde (and windows-sys on Windows), so
//! plugins stay small.

mod conn;
pub mod proto;
mod sdk;

pub use proto::{Line, Span};
pub use sdk::{Error, Event, Plugin};
