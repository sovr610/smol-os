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
    assert!(added.contains("added Claude Code · Work (Max)"), "{added}");
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

/// `claude` in a TermOS window is TermOS's shim: it runs the real
/// program as the account in use, one pinned for the window, or the
/// user's own folder.
#[test]
fn the_shim_runs_the_cli_as_the_account() {
    let w = world();
    let env = &w.env;
    // TermOS makes the shim when it starts, as the CLI is on PATH.
    let t = common::Termos::start_in(std::sync::Arc::clone(env));
    t.wait_ready();
    let data = env.paths().data_dir;
    let shim = data.join("bin/claude");
    assert!(
        std::fs::read_link(&shim).is_ok_and(|p| p.ends_with("termos")),
        "{} is a link to termos",
        shim.display()
    );
    assert!(data.join("bin/codex").exists(), "codex is on PATH too");

    let run_shim = |vars: &[(&str, &str)]| {
        let mut cmd = std::process::Command::new(&shim);
        cmd.current_dir(env.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("TERMOS_ACCOUNT_CLAUDE");
        for (k, v) in env.vars() {
            cmd.env(k, v);
        }
        for (k, v) in vars {
            cmd.env(k, v);
        }
        cmd.arg("--version").output().expect("ran")
    };
    let last = || {
        std::fs::read_to_string(&w.calls)
            .expect("calls")
            .lines()
            .last()
            .expect("a call")
            .to_owned()
    };

    assert!(run_shim(&[]).status.success());
    assert_eq!(last(), "claude default --version");
    run(env, &["account", "add", "claude", "work", "--no-sign-in"]);
    run(env, &["account", "use", "claude", "work"]);
    assert!(run_shim(&[]).status.success());
    let work = data.join("accounts/claude/work");
    assert_eq!(last(), format!("claude {} --version", work.display()));
    // Pinned for a window, or the user's own folder.
    assert!(
        run_shim(&[("TERMOS_ACCOUNT_CLAUDE", "default")])
            .status
            .success()
    );
    assert_eq!(last(), "claude default --version");
    assert!(run_shim(&[("CLAUDE_CONFIG_DIR", "/mine")]).status.success());
    assert_eq!(last(), "claude /mine --version");
    let out = run_shim(&[("TERMOS_ACCOUNT_CLAUDE", "nope")]);
    assert_eq!(out.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no Claude Code account named nope"));
}

/// Switching an agent window's account carries its conversation: the
/// newest one for the folder is copied into the other account, and the
/// CLI starts resuming it there.
#[test]
fn a_conversation_continues_under_another_account() {
    let w = world();
    let env = &w.env;
    run(env, &["account", "add", "claude", "work", "--no-sign-in"]);
    let here = env.path().canonicalize().expect("canonical");
    let folder: String = here
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mine = env.path().join(".claude/projects").join(&folder);
    std::fs::create_dir_all(&mine).expect("mkdir");
    std::fs::write(mine.join("conv-1.jsonl"), "{\"type\":\"user\"}\n").expect("write");

    run(
        env,
        &[
            "agent",
            "external",
            "--account",
            "work",
            "--resume-from",
            "default",
            "claude",
        ],
    );
    let data = env.paths().data_dir;
    let carried = data
        .join("accounts/claude/work/projects")
        .join(&folder)
        .join("conv-1.jsonl");
    assert!(carried.exists(), "copied into the other account");
    assert!(mine.join("conv-1.jsonl").exists(), "and kept in the first");
    let calls = std::fs::read_to_string(&w.calls).expect("calls");
    let last = calls.lines().last().expect("ran");
    assert_eq!(
        last,
        format!(
            "claude {} --resume {}",
            data.join("accounts/claude/work").display(),
            carried.display()
        )
    );

    // Nothing to continue in a folder without a conversation: a new one.
    let other = tempfile::tempdir().expect("tempdir");
    let out = env.run_in(
        other.path(),
        &[
            "agent",
            "external",
            "--account",
            "work",
            "--resume-from",
            "default",
            "claude",
        ],
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("starting a new one"));
    let calls = std::fs::read_to_string(&w.calls).expect("calls");
    assert!(
        calls.lines().last().expect("ran").ends_with("claude/work "),
        "{calls}"
    );
}

/// Claude Code's plan limits reach TermOS through its status line: TermOS's
/// command keeps them per account and still shows the user's own line.
#[test]
fn the_status_line_keeps_claude_codes_limits() {
    use std::io::Write as _;
    let w = world();
    let env = &w.env;
    let settings = env.path().join(".claude/settings.json");
    std::fs::write(
        &settings,
        r#"{"model":"fixture-model","statusLine":{"type":"command","command":"echo mine"}}"#,
    )
    .expect("write");
    run(env, &["account", "add", "claude", "work", "--no-sign-in"]);
    let said = run(env, &["account", "statusline", "claude", "on"]);
    // One file: the new account's settings are a link to it.
    assert_eq!(
        said.matches("put TermOS's status line").count(),
        1,
        "{said}"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    assert!(
        v["statusLine"]["command"]
            .as_str()
            .is_some_and(|c| c.ends_with("termos agent statusline")),
        "{v}"
    );
    assert_eq!(v["model"], "fixture-model");

    let data = env.paths().data_dir;
    let work = data.join("accounts/claude/work");
    let input = r#"{"model":{"display_name":"Fixture"},"rate_limits":{"five_hour":{"used_percentage":42.5,"resets_at":1800000000},"seven_day":{"used_percentage":10}}}"#;
    let status = |dir: Option<&std::path::Path>| {
        let mut cmd = env.command_in(env.path(), &["agent", "statusline"]);
        cmd.env_remove("CLAUDE_CONFIG_DIR");
        if let Some(d) = dir {
            cmd.env("CLAUDE_CONFIG_DIR", d);
        }
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("ran");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // The user's own line still shows; the limits are kept for the account.
    assert_eq!(status(Some(&work)).trim(), "mine");
    let state = env.paths().state_dir;
    let kept =
        std::fs::read_to_string(state.join("accounts/limits/claude-work.json")).expect("kept");
    assert!(kept.contains("42.5"), "{kept}");
    status(None);
    assert!(state.join("accounts/limits/claude-default.json").exists());

    // Off: their line is back as it was.
    run(env, &["account", "statusline", "claude", "off"]);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(v["statusLine"]["command"], "echo mine");
    // Without a line of their own, TermOS prints a short one.
    std::fs::write(&settings, "{}").expect("write");
    run(env, &["account", "statusline", "claude", "on"]);
    let line = status(Some(&work));
    assert!(
        line.contains("Fixture") && line.contains("5 h 42%") && line.contains("week 10%"),
        "{line}"
    );
    assert!(
        !env.run(&["account", "statusline", "codex", "on"])
            .status
            .success()
    );
}
