// SPDX-License-Identifier: GPL-3.0-or-later
//! Several accounts of each agent CLI (M11a, D-096, D-097), with the real
//! `termos` and stand-ins for Claude Code and Codex that record how they
//! were run (their folder variable and their arguments).

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

fn stand_in(dir: &Path, name: &str, var: &str, calls: &Path) {
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\necho \"{name} ${{{var}:-default}} $*\" >> '{}'\n",
            calls.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

struct World {
    _bin: tempfile::TempDir,
    calls: PathBuf,
    env: std::sync::Arc<common::Env>,
}

fn world() -> World {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    stand_in(bin.path(), "claude", "CLAUDE_CONFIG_DIR", &calls);
    stand_in(bin.path(), "codex", "CODEX_HOME", &calls);
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars("", vec![("PATH", path.into())]);
    // The user's own logins and settings, from before TermOS.
    let home = env.path();
    std::fs::create_dir_all(home.join(".claude/skills/mine")).expect("mkdir");
    std::fs::write(home.join(".claude/settings.json"), "{}").expect("write");
    std::fs::write(home.join(".claude/.credentials.json"), "LOGIN").expect("write");
    std::fs::create_dir_all(home.join(".codex")).expect("mkdir");
    std::fs::write(
        home.join(".codex/config.toml"),
        "model = \"fixture-model\"\n",
    )
    .expect("write");
    World {
        _bin: bin,
        calls,
        env,
    }
}

fn run(env: &common::Env, args: &[&str]) -> String {
    let out = env.run(args);
    assert!(
        out.status.success(),
        "termos {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn accounts_are_profiles_with_their_own_folders() {
    let w = world();
    let env = &w.env;
    let list = run(env, &["account", "list", "claude"]);
    assert!(list.contains("● default"), "{list}");

    let added = run(
        env,
        &[
            "account",
            "add",
            "claude",
            "work",
            "--label",
            "Work (Max)",
            "--no-sign-in",
        ],
    );
    assert!(added.contains("added Claude Code (Work (Max))"), "{added}");
    let data = env.paths().data_dir;
    let work = data.join("accounts/claude/work");
    assert!(work.join("settings.json").exists(), "settings shared");
    assert!(work.join("skills/mine").exists(), "skills shared");
    assert!(
        !work.join(".credentials.json").exists(),
        "the login is its own"
    );
    // TermOS's MCP server registered in the new account's own settings.
    let calls = std::fs::read_to_string(&w.calls).expect("claude ran");
    let add = calls
        .lines()
        .find(|l| l.contains("mcp add"))
        .expect("mcp add");
    assert!(
        add.starts_with(&format!(
            "claude {} mcp add --scope user termos -- ",
            work.display()
        )),
        "{calls}"
    );

    run(env, &["account", "use", "claude", "work"]);
    let list = run(env, &["account", "list"]);
    assert!(list.contains("● work"), "{list}");
    assert!(list.contains("  default"), "{list}");
    assert!(list.contains("Codex CLI\n  ● default"), "per CLI: {list}");

    run(
        env,
        &[
            "account",
            "budget",
            "claude",
            "work",
            "--five-hour",
            "2M",
            "--week",
            "15M",
        ],
    );
    let index = std::fs::read_to_string(data.join("accounts/accounts.toml")).expect("index");
    assert!(index.contains("budget-5h = 2000000"), "{index}");
    assert!(index.contains("budget-week = 15000000"), "{index}");

    // Codex: the config is copied and gets TermOS's server; the
    // default's stays as it was.
    run(env, &["account", "add", "codex", "second", "--no-sign-in"]);
    let second =
        std::fs::read_to_string(data.join("accounts/codex/second/config.toml")).expect("cfg");
    assert!(second.starts_with("model = \"fixture-model\""), "{second}");
    assert!(second.contains("[mcp_servers.termos]"), "{second}");
    let default = std::fs::read_to_string(env.path().join(".codex/config.toml")).expect("cfg");
    assert!(!default.contains("termos"), "{default}");

    // Signing in runs the CLI's own sign-in in that folder.
    run(env, &["account", "sign-in", "codex", "second"]);
    let calls = std::fs::read_to_string(&w.calls).expect("calls");
    assert!(
        calls.contains(&format!(
            "codex {} login",
            data.join("accounts/codex/second").display()
        )),
        "{calls}"
    );

    // Removing needs a confirmation, and the default stays.
    assert!(
        !env.run(&["account", "remove", "claude", "work"])
            .status
            .success()
    );
    run(env, &["account", "remove", "claude", "work", "--yes"]);
    assert!(!work.exists());
    let list = run(env, &["account", "list", "claude"]);
    assert!(
        list.contains("● default") && !list.contains("work"),
        "{list}"
    );
    assert!(
        !env.run(&["account", "remove", "claude", "default", "--yes"])
            .status
            .success()
    );
    assert!(env.path().join(".claude/.credentials.json").exists());
    assert!(
        !env.run(&["account", "add", "claude", "../escape", "--no-sign-in"])
            .status
            .success()
    );
}
