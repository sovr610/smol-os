// SPDX-License-Identifier: GPL-3.0-or-later
//! Exposes the compilation target triple to the binary for `termos --version`.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_else(|_| String::from("unknown-target"));
    println!("cargo:rustc-env=TERMOS_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
