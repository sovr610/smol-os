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
    assert!(stdout.contains("config"));
}

#[test]
fn desktop_needs_a_terminal() {
    // Without arguments termos starts the desktop, which needs a TTY.
    let out = Command::new(env!("CARGO_BIN_EXE_termos"))
        .env_remove("TERMOS")
        .output()
        .expect("spawn");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("UTF-8");
    assert!(stderr.contains("must be a terminal"), "{stderr}");
}

#[test]
fn refuses_to_nest_without_force() {
    let out = Command::new(env!("CARGO_BIN_EXE_termos"))
        .env("TERMOS", "123")
        .output()
        .expect("spawn");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("UTF-8");
    assert!(stderr.contains("already running inside TermOS"), "{stderr}");
}

#[test]
fn config_default_is_valid_and_check_reports_locations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = termos(&["config", "default"]);
    assert!(out.status.success());
    let good = dir.path().join("good.toml");
    std::fs::write(&good, &out.stdout).expect("write");
    let out = termos(&["config", "check", good.to_str().expect("utf-8 path")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).ends_with(": ok\n"));

    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "mouse = true\nprefix = \"C-\"\n").expect("write");
    let out = termos(&["config", "check", bad.to_str().expect("utf-8 path")]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("bad.toml:2:"), "{text}");

    let unknown = dir.path().join("unknown.toml");
    std::fs::write(&unknown, "[keys.prefix]\n\"v\" = \"pane.explode\"\n").expect("write");
    let out = termos(&["config", "check", unknown.to_str().expect("utf-8 path")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("pane.explode"));
}

#[test]
fn unknown_argument_is_rejected() {
    let out = termos(&["--definitely-not-a-flag"]);
    assert_eq!(out.status.code(), Some(2));
}
