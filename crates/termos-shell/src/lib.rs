// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-shell
//!
//! The desktop shell around the window manager, and the top-level `Desktop`
//! state the server runs.
//!
//! **Status:** not started. Scheduled for M2, extended in M3 and M4
//! (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Desktop shell"):
//! - `desktop` — top-level server state: sessions, the WM, shell overlays.
//! - `palette` — command palette (Ctrl+Space) with fuzzy matching.
//! - `launcher` — app/command launcher.
//! - `whichkey` — key-hint overlay after a leader/prefix key.
//! - `bar` — status bar with built-in and plugin widgets.
//! - `notify` — notification toasts and the notification center.
//! - `tutorial` — first-run tutorial.
//! - `plugins` — host for out-of-process plugins (M3) and WASM widgets (M4).

// TODO(M2): Desktop state, command palette, which-key, status bar, notifications, first-run tutorial.
// TODO(M3): out-of-process plugin host over the JSON-RPC control API.
// TODO(M4): optional WASM runtime for status-bar widgets (behind a cargo feature).
