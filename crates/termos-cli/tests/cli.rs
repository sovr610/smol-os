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

/// `termos lang` drives mise: here a stand-in script that logs its
/// arguments and answers like mise (TERMOS_MISE points at it).
#[cfg(unix)]
#[test]
fn lang_commands_drive_mise() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let fake = dir.path().join("mise");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo '2026.10.14 linux-x64 (2026-10-18)';;\n\
             ls) echo '{{\"go\": [{{\"version\": \"1.23.2\", \"install_path\": \"/m/go\", \"active\": true}}]}}';;\n\
             *) echo \"$*\" >> '{}';;\n\
             esac\n",
            log.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_termos"))
            .args(args)
            .env("TERMOS_MISE", &fake)
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .output()
            .expect("runs")
    };
    let out = run(&["lang", "install", "go@1.23"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = std::fs::read_to_string(&log).expect("mise was called");
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "use --global go@1.23",
            "install go:golang.org/x/tools/gopls@latest",
            "install go:github.com/go-delve/delve/cmd/dlv@latest",
        ]
    );
    let out = run(&["lang", "list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let go = text
        .lines()
        .find(|l| l.starts_with("go "))
        .expect("a go row");
    assert!(
        go.contains("1.23.2*") && go.contains("gopls (not installed)"),
        "{go}"
    );
    let out = run(&["lang", "install", "cobol"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no language \"cobol\""));
}

/// `termos edit` sets up TermOS's Neovim (config written, the pinned
/// version through mise) and becomes it: here the stand-in mise.
#[cfg(unix)]
#[test]
fn edit_starts_termos_neovim_through_mise() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("calls.log");
    let fake = dir.path().join("mise");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo '2026.10.14 linux-x64';;\n\
             ls) echo '{{\"neovim\": [{{\"version\": \"0.12.6\", \"install_path\": \"/m/nvim\"}}]}}';;\n\
             *) echo \"NVIM_APPNAME=$NVIM_APPNAME $*\" >> '{}';;\n\
             esac\n",
            log.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let cfg = dir.path().join("cfg");
    let out = Command::new(env!("CARGO_BIN_EXE_termos"))
        .args(["edit", "main.go"])
        .env("TERMOS_MISE", &fake)
        .env("XDG_CONFIG_HOME", &cfg)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .output()
        .expect("runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = std::fs::read_to_string(&log).expect("mise ran the editor");
    assert_eq!(
        calls.trim(),
        "NVIM_APPNAME=termos-nvim exec neovim@0.12.6 -- nvim main.go"
    );
    assert!(cfg.join("termos-nvim/init.lua").is_file());
    assert!(cfg.join("termos-nvim/lua/termos/generated.lua").is_file());
}
