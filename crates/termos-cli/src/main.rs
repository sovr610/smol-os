// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos`: the TermOS command-line entry point.
//!
//! Over the milestones this binary grows into the desktop server, the
//! attaching client and every `termos <subcommand>` (see `ROADMAP.md`).
//! As of M0 it only reports its version and help.

use clap::Parser;

/// Long version string, e.g. `0.0.0 (x86_64-unknown-linux-gnu)`.
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TERMOS_BUILD_TARGET"),
    ")"
);

/// TermOS: a terminal-first desktop environment.
#[derive(Debug, Parser)]
#[command(
    name = "termos",
    version,
    long_version = LONG_VERSION,
    arg_required_else_help = true
)]
struct Cli {}

fn main() {
    // TODO(M1): run the hosted desktop (server + terminal client) when invoked without a subcommand.
    let _cli = Cli::parse();
}
