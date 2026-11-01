// SPDX-License-Identifier: GPL-3.0-or-later
//! # termos-image
//!
//! Bare-metal TermOS images.
//!
//! **Status:** not started. Scheduled for M8, extended in M9 (see `ROADMAP.md`).
//!
//! This crate will hold the Buildroot external tree (`br2-external/`: defconfigs
//! for x86_64 UEFI+BIOS, generic aarch64 UEFI, Raspberry Pi 5/4/3/Zero 2 W,
//! board files, kernel fragments, rootfs overlay) and the Rust code that drives
//! image builds and the QEMU boot tests.
//! See `ARCHITECTURE.md`, "Bare-metal image".

// TODO(M8): Buildroot external tree, image build driver, QEMU boot tests for x86_64 and aarch64 virt.
// TODO(M9): installer and atomic A/B updates.
