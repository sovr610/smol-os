// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-ai
//!
//! The TermOS agent runtime (ARCHITECTURE.md §11, D-058 to D-064).
//!
//! - [`provider`]: the [`provider::Provider`] trait: Anthropic's Messages
//!   API and the OpenAI-compatible family (OpenAI, Kimi, Ollama, vLLM, LM
//!   Studio, OpenRouter, TermOS's own router), our own thin clients over
//!   one [`sse`] parser. Models are listed, never hardcoded.
//! - [`agent`]: a session's loop on its own thread: stream, ask, run,
//!   repeat; cancellation, retries, token and cost accounting.
//! - [`tools`]: files, code search, PTY-backed commands, fetch, MCP.
//! - [`policy`]: the permission engine; [`sandbox`]: bubblewrap and
//!   Landlock; [`audit`]: the hash-chained log; [`redact`]: keeping
//!   secrets out of what is written down.
//! - [`mcp`]: MCP servers over stdio (sandboxed) and streamable HTTP.
//! - [`secrets`]: API keys in the OS keyring or an age file.
//! - [`history`]: saved conversations; [`external`]: CLI agent presets.
//! - [`ui`]: the agent window, the audit viewer, and the launcher the
//!   desktop opens them with.

pub mod agent;
pub mod audit;
pub mod external;
pub mod history;
pub mod mcp;
pub mod policy;
pub mod provider;
pub mod redact;
pub mod sandbox;
pub mod secrets;
pub mod sse;
pub mod tools;
pub mod ui;
