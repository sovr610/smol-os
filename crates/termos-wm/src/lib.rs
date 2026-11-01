// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-wm
//!
//! The TermOS window manager: "tmux grown into a desktop".
//!
//! **Status:** not started. Scheduled for M2 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Window manager"):
//! - `tree` — session → workspaces → a floating layer plus a tiling tree of
//!   containers (splits, tabs, stacks).
//! - `layout` — master-stack, grid and BSP layouts.
//! - `floating` — move/resize, snapping to edges and halves/quarters,
//!   minimize/maximize, z-order.
//! - `input` — routing keys and mouse (drag, resize, focus) to the WM or to
//!   the focused window, translating coordinates for child apps.
//! - `compose` — drawing windows, borders, title bars and drop shadows into the
//!   render grid in z-order.

// TODO(M2): window tree, floating + tiled layouts, workspaces, mouse move/resize, compositor.
