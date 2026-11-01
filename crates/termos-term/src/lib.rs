// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-term
//!
//! Terminal panes: programs on pseudo-terminals, emulated so their output
//! can be composited into the desktop.
//!
//! - [`pty`]: spawning, resizing and talking to a child on a PTY
//!   (DECISIONS.md D-025).
//! - [`vt`]: the [`vt::VtBackend`] trait and its `alacritty_terminal`
//!   implementation (D-008).
//! - [`input`]: encoding keys, mouse, paste and focus for the child.
//!
//! The VT input path is fuzzed by `fuzz/fuzz_targets/vt_advance.rs`.

pub mod graphics;
pub mod input;
pub mod intercept;
mod kbguard;
pub mod pty;
pub mod vt;

pub use pty::{PtyEvent, PtyPane, SpawnSpec};
pub use vt::{AlacrittyVt, VtBackend, VtEvent, VtModes};
