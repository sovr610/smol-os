// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-center
//!
//! The command center (M10c, D-084, D-092, D-093): one window over the AI
//! side of TermOS (agent sessions, tokens and cost, tool calls and their
//! decisions, the audit chain, local models, MCP servers, CLI agents) and
//! the system (processes, sessions and panes, hardware, network, logs,
//! updates), live and over the history the journal keeps, with actions:
//! stop an agent, approve or deny its call, end a process, close or go to
//! a pane, open the related window or log.
//!
//! - [`sampler`]: records the machine's load in the journal every minute.
//! - [`data`]: what the window shows, gathered on a thread of its own, and
//!   the history's charts and totals.
//! - [`app`]: the window.

pub mod app;
pub mod data;
pub mod sampler;
pub mod theirs;
pub mod usage;
pub mod usage_data;

pub use app::CommandCenter;
