// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-shell
//!
//! The TermOS desktop: everything the server does, tmux-style
//! (DECISIONS.md D-023).
//!
//! - [`desktop`]: the [`desktop::Desktop`] state machine: panes, input
//!   routing (prefix key, overlays, copy mode, mouse), commands, rendering.
//! - [`server`]: the session server: many sessions (one desktop each), many
//!   attached clients, the Unix socket and the JSON-RPC control API.
//! - [`commands`] and [`keys`]: named commands and the tmux-compatible key
//!   tables.
//! - [`status`], [`palette`], [`whichkey`], [`prompt`]: the status line, the
//!   command palette, key hints and status-line prompts.
//! - [`copymode`]: scrollback navigation, selection, search and copy.
//! - [`snapshot`]: saved sessions and how they are restored.
//! - [`plugins`]: the out-of-process plugin host.
//! - [`notify`] and [`apps`]: notifications, the notification center and
//!   the first-run tutorial.
//! - [`attention`] and [`integration`]: agents that wait for the user, and
//!   the shell integration that marks commands (D-115, D-116).
//! - [`hints`] and [`search`]: hint mode over a pane's links and paths,
//!   and the search across panes (D-118).

/// TermOS's command in every pane (D-085).
pub const TOS: &str = "tos";

pub mod anim;
pub mod apps;
pub mod attention;
pub mod commands;
pub mod copymode;
pub mod desktop;
#[cfg(windows)]
pub mod endsession;
pub mod hints;
pub mod images;
pub mod integration;
pub mod keys;
pub mod menu;
pub mod notify;
pub mod palette;
pub mod plugins;
pub mod prompt;
pub mod search;
pub mod server;
pub mod snapshot;
pub mod status;
pub mod ui;
pub mod wallpaper;
#[cfg(feature = "wasm-widgets")]
pub mod wasm;
pub mod whichkey;

pub use desktop::{Desktop, DesktopOptions};
