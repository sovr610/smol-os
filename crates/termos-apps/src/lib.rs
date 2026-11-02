// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-apps
//!
//! Built-in TermOS apps. Each implements the in-process `App` trait and draws
//! with ratatui widgets into its window.
//!
//! - [`image_viewer`]: images from a folder, in pixels where the terminal
//!   can show them (M4a).
//! - [`markdown`]: markdown documents, with their local images.
//!
//! Planned (see `ARCHITECTURE.md`, "Apps and plugins"): file manager,
//! system monitor (CPU/GPU/RAM/temps), markdown viewer, settings and
//! clipboard manager (M4b); package manager UI (M5).

pub mod image_viewer;
pub mod markdown;

pub use image_viewer::ImageViewer;
pub use markdown::MarkdownViewer;

// TODO(M4): file manager, system monitor, settings, clipboard manager.
// TODO(M5): package manager UI over mise, the dev rootfs and the host package manager.
