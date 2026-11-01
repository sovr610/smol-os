// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-wm
//!
//! The TermOS window manager, tmux-style (DECISIONS.md D-023, D-024):
//!
//! - [`layout`]: the tiling tree of panes with one-cell separators, splits,
//!   BSP auto-splits, resizing, directional focus and preset arrangements
//!   (even-horizontal, even-vertical, main-vertical = master-stack,
//!   main-horizontal, tiled = grid).
//! - [`workspace`]: workspaces (tmux "windows") with a floating layer
//!   (z-order, minimize, snapping, zoom), and the [`workspace::Session`].
//! - [`hit`]: mouse hit testing (separators, float title bars, edges and
//!   buttons, pane content).
//! - [`compose`]: drawing it all, with junction-aware separators, an accent
//!   on the focused pane's borders, and floats with rounded borders and
//!   drop shadows.
//!
//! This crate is pure logic: it never touches a PTY or the terminal. Pane
//! content is drawn through a callback.

pub mod compose;
pub mod hit;
pub mod layout;
pub mod workspace;

pub use compose::{ComposeOptions, compose};
pub use hit::{Button, Edge, Hit};
pub use layout::{Dir, Direction, Layout, Preset};
pub use workspace::{Floating, Placement, Session, Snap, SplitHow, Workspace};
