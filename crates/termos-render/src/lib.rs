// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-render
//!
//! The TermOS compositor surface and output backends.
//!
//! **Status:** not started. Scheduled for M1, extended in M4 and M8
//! (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Rendering pipeline"):
//! - `grid` — the `Grid` of `Cell`s (grapheme, width, fg/bg/underline colour,
//!   attributes, hyperlink) that the compositor draws into.
//! - `damage` — diffing consecutive frames into minimal row spans.
//! - `color` — 24-bit colour with downsampling to 256/16 colours.
//! - `theme` — colour tokens and theme definitions.
//! - `caps` — terminal capability detection (env + active queries).
//! - `backend::terminal` — encodes damage as escape sequences via crossterm,
//!   wrapped in synchronized-output (DEC 2026) brackets.
//! - `backend::headless` — in-memory backend for tests and snapshot tests.
//! - `backend::kms` — DRM/KMS framebuffer backend with its own font
//!   rasterizer (bare-metal mode).

// TODO(M1): Grid/Cell, frame diffing, the crossterm terminal backend and the headless backend.
// TODO(M4): capability detection, image protocols (Kitty, Sixel, iTerm2, block fallback), themes.
// TODO(M8): DRM/KMS backend with font rasterizer and adjustable font size.
