// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-devkit
//!
//! Developer environment integration.
//!
//! **Status:** not started. Scheduled for M5 (see `ROADMAP.md`).
//!
//! Planned modules (see `ARCHITECTURE.md`, "Developer environment"):
//! - `lang` — `termos lang install <lang>` and per-project versions, wrapping
//!   mise rather than reimplementing it.
//! - `editor` — the preconfigured Neovim default (Helix selectable) with LSP
//!   servers for every supported language and debugging via DAP.
//! - `devroot` — the Debian-based dev rootfs used on bare metal for compilers
//!   and system packages.
//! - `panels` — Git UI, project templates, build/run/test panels, Podman.

// TODO(M5): mise-backed toolchain manager, Neovim + LSP/DAP setup, dev rootfs, Git/build panels.
