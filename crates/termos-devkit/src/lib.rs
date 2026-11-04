// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-devkit
//!
//! The developer environment (ARCHITECTURE.md §10, D-053):
//! - [`mise`]: finding mise, or fetching a pinned, checksummed one;
//! - [`langs`]: the languages TermOS sets up, with their language servers
//!   and debuggers;
//! - [`lang`]: `termos lang install|use|list|remove` over mise;
//! - [`check`]: `termos lang check`, a language from install to debugger;
//! - [`editor`]: TermOS's Neovim and Helix, with the installed servers
//!   and debuggers;
//! - [`templates`]: `termos create <template> [dir]`;
//! - [`tasks`]: build, run and test for a project.

pub mod accounts;
pub mod check;
pub mod cli_agents;
pub mod editor;
pub mod lang;
pub mod langs;
pub mod mise;
pub mod tasks;
pub mod templates;

// TODO(M5b): dev rootfs, Podman and the package manager UI.
