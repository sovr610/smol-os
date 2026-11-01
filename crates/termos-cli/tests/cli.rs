// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke tests for the `termos` binary.

use std::process::{Command, Output};

fn termos(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_termos"))
        .args(args)
        .output()
        .expect("failed to spawn the termos binary")
}

#[test]
fn long_version_reports_version_and_target() {
    let out = termos(&["--version"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("version output is UTF-8");
    let expected = format!(
        "termos {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("TERMOS_BUILD_TARGET")
    );
    assert_eq!(stdout.trim_end(), expected);
}

#[test]
fn short_version_reports_version() {
    let out = termos(&["-V"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("version output is UTF-8");
    assert_eq!(
        stdout.trim_end(),
        format!("termos {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_describes_termos() {
    let out = termos(&["--help"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("help output is UTF-8");
    assert!(stdout.contains("terminal-first desktop environment"));
    assert!(stdout.contains("Usage: termos"));
}

#[test]
fn no_arguments_prints_usage_and_fails() {
    let out = termos(&[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("usage output is UTF-8");
    assert!(stderr.contains("Usage: termos"));
}

#[test]
fn unknown_argument_is_rejected() {
    let out = termos(&["--definitely-not-a-flag"]);
    assert_eq!(out.status.code(), Some(2));
}
