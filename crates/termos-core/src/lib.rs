// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-core
//!
//! The foundation every other TermOS crate builds on: the server event loop,
//! the in-process IPC bus, the client/server wire protocol, configuration
//! loading and well-known paths.
//!
//! **Status:** not started. Scheduled across M1–M3 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Process model" and "IPC"):
//! - `event` — the tokio-based server loop and frame scheduler (event-driven,
//!   no fixed tick, so an idle desktop makes no wakeups).
//! - `bus` — typed in-process publish/subscribe between subsystems.
//! - `transport` — the `Transport` trait connecting frontends to the server:
//!   an in-memory channel in M1, a Unix socket in M3.
//! - `proto` — versioned handshake, postcard-encoded frame/input messages and
//!   the JSON-RPC 2.0 control API.
//! - `app` — the in-process `App` trait implemented by built-in apps, agent
//!   windows and devkit panels.
//! - `config` — TOML configuration with validation and hot reload.
//! - `paths` — XDG config/state/runtime directories and socket locations.

// TODO(M1): event loop, in-memory Transport and the frame/input protocol types.
// TODO(M2): the `App` trait, TOML config loading with span-accurate errors, hot reload via `notify`.
// TODO(M3): Unix-socket transport with SO_PEERCRED checks and the JSON-RPC control API.
