// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-ai
//!
//! The TermOS agent runtime.
//!
//! **Status:** not started. Scheduled for M6 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "AI agent runtime"):
//! - `provider` — the `Provider` trait (model listing, streaming chat, tool
//!   calls) with Anthropic, OpenAI, Moonshot Kimi, generic OpenAI-compatible
//!   and local implementations. Model names are never hardcoded.
//! - `agent` — the agent loop, conversation history, token/cost accounting.
//! - `tools` — read/write files, run commands, search code.
//! - `policy` — per-agent allowlists, confirmation prompts, audit log.
//! - `sandbox` — bubblewrap / landlock / namespace isolation for tool calls.
//! - `mcp` — Model Context Protocol client.
//! - `secrets` — API keys in the OS keyring or an encrypted store.

// TODO(M6): providers, streaming agent windows, sandboxed tools, permission engine, audit log, MCP, secrets.
