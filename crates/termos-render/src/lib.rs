// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-render
//!
//! The TermOS compositor surface and output encoding.
//!
//! - [`grid`]: the [`Grid`] of [`Cell`]s the desktop is composed into, with
//!   clipped, wide-character-aware drawing helpers (text, boxes, shadows).
//! - [`diff`]: output damage, the minimal [`Span`]s between two frames.
//! - [`encode`]: turns spans into escape sequences for one terminal, with
//!   synchronized output and colour downsampling.
//! - [`caps`], [`probe`]: terminal capability detection, from the
//!   environment and from the terminal's replies to queries.
//! - [`color`], [`theme`]: colours, palettes and the desktop's styles.
//!
//! See `ARCHITECTURE.md` §5 for how these fit into the rendering pipeline.

pub mod caps;
pub mod cell;
pub mod color;
pub mod diff;
pub mod encode;
pub mod grid;
pub mod probe;
pub mod theme;

pub use caps::{Caps, Graphics, ImageProtocol, PxSize};
pub use cell::{Attrs, Cell, Style};
pub use color::{Color, ColorDepth, Palette, Rgb};
pub use diff::Span;
pub use encode::{CursorShape, CursorState, Encoder};
pub use grid::{BorderKind, Grid, Rect};
pub use theme::{Theme, ThemeColors};

// TODO(M4): image protocols (Kitty, Sixel, iTerm2, block/braille fallback) with occlusion handling.
// TODO(M8): DRM/KMS backend with font rasterizer and adjustable font size.
