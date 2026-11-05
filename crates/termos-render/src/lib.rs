// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-render
//!
//! The TermOS compositor surface and output encoding.
//!
//! - [`grid`]: the [`Grid`] of [`Cell`]s the desktop is composed into, with
//!   clipped, wide-character-aware drawing helpers (text, boxes, shadows).
//! - [`link`]: the hyperlinks cells carry (OSC 8) and their table.
//! - [`diff`]: output damage, the minimal [`Span`]s between two frames.
//! - [`encode`]: turns spans into escape sequences for one terminal, with
//!   synchronized output and colour downsampling.
//! - [`caps`], [`probe`]: terminal capability detection, from the
//!   environment and from the terminal's replies to queries.
//! - [`color`], [`theme`]: colours, palettes and the desktop's styles.
//! - `raster` (feature `raster`): the software rasterizer the KMS client
//!   draws cells with.
//!
//! See `ARCHITECTURE.md` §5 for how these fit into the rendering pipeline.

pub mod caps;
pub mod cell;
pub mod color;
pub mod diff;
pub mod encode;
pub mod graphics;
pub mod grid;
pub mod icons;
pub mod link;
pub mod probe;
#[cfg(feature = "raster")]
pub mod raster;
pub mod theme;

pub use caps::{Caps, Graphics, ImageProtocol, PxSize};
pub use cell::{Attrs, Cell, Style};
pub use color::{Color, ColorDepth, Palette, Rgb};
pub use diff::Span;
pub use encode::{CursorShape, CursorState, Encoder};
pub use grid::{BorderKind, Grid, Rect};
pub use theme::{Theme, ThemeBase, ThemeColors, ThemeConfig};
