// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-term
//!
//! Terminal windows: spawning programs on a PTY and emulating a VT so their
//! output can be composited into a TermOS window.
//!
//! **Status:** not started. Scheduled for M1 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Terminal windows", and
//! `DECISIONS.md` D-008):
//! - `pty` — PTY creation, child spawning, resize (TIOCSWINSZ), async I/O.
//! - `vt` — the `VtBackend` trait and its `alacritty_terminal` implementation:
//!   VT state, scrollback, damage, title/cwd/bell events.
//! - `blit` — copying VT cells into the render grid.
//!
//! The VT input path will be fuzzed with cargo-fuzz from M1 onward.

// TODO(M1): PTY spawning, VtBackend over alacritty_terminal, blitting into the grid, VT fuzz target.
// TODO(M4): intercept Kitty graphics / Sixel sequences so images work inside PTY windows.
