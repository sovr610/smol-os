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
//! - [`tasks`]: build, run and test for a project;
//! - [`accounts`], [`cli_agents`], [`conversations`]: Claude Code and
//!   Codex CLI, their accounts and conversations (D-095, D-097);
//! - [`skills`]: TermOS's skills for the coding CLIs (D-098);
//! - [`devroot`]: the dev rootfs, Debian 13 entered without root (D-110);
//! - [`packages`], [`packages_app`]: packages from mise, the dev rootfs,
//!   the host and Podman, and the package manager app (D-110);
//! - [`worktrees`]: a git worktree per task, with its ports and setup
//!   (D-114).

pub mod accounts;
pub mod check;
pub mod cli_agents;
pub mod conversations;
pub mod devroot;
pub mod editor;
pub mod lang;
pub mod langs;
pub mod mise;
pub mod packages;
pub mod packages_app;
pub mod skills;
pub mod tasks;
pub mod templates;
pub mod worktrees;
