// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-devkit
//!
//! The developer environment (ARCHITECTURE.md §10, D-053):
//! - [`mise`]: finding mise, or fetching a pinned, checksummed one;
//! - [`langs`]: the languages TermOS sets up, with their language servers
//!   and debuggers;
//! - [`lang`]: `termos lang install|use|list|remove` over mise;
//! - [`editor`]: TermOS's Neovim and Helix, with the installed servers
//!   and debuggers.

pub mod editor;
pub mod lang;
pub mod langs;
pub mod mise;

// TODO(M5): project templates, build/run/test panels, Git UI.
// TODO(M5): dev rootfs, Podman and the package manager UI (M5b).
