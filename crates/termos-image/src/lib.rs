// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-image
//!
//! Bare-metal TermOS images (ARCHITECTURE.md §13, D-074 to D-076):
//!
//! - `br2-external/`: the Buildroot external tree: the `termos` package,
//!   per-board configuration fragments, the BusyBox init scripts, the boot
//!   loader setup and the partition layouts.
//! - [`targets`]: the boards, each a Buildroot board defconfig plus TermOS
//!   fragments.
//! - [`build`]: downloads the pinned Buildroot, configures and checks a
//!   target, builds `termos` against its sysroot and the image.
//! - [`boot`]: the QEMU boot test: the console's ready marker, a
//!   screenshot of the desktop, the memory in use; with `--update`, an A/B
//!   update and a failed one falling back.
//! - [`keys`] and [`bundle`]: update bundles (D-078), signed with the
//!   owner's keys or development ones.
//!
//! It has no dependencies, so `cargo xtask image …` stays std-only
//! (D-020). Building and booting need a Linux host.

pub mod fragments;
pub mod keys;
pub mod screen;
pub mod serial;
pub mod targets;

#[cfg(unix)]
pub mod boot;
#[cfg(unix)]
pub mod build;
#[cfg(unix)]
pub mod bundle;
#[cfg(unix)]
pub mod qmp;

// TODO(M9): the live ISO and `termos install` (D-080).
// TODO(M9): grow the data partition to the whole disk on first boot.
