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

/// TermOS's skills (M11b, D-098): put where Claude Code (every account)
/// and the shared Agent Skills folder's CLIs read them when the server
/// starts, never over the user's own, and kept off once removed.
#[test]
fn the_skills_follow_the_clis_and_their_accounts() {
    let w = world();
    let env = &w.env;
    let home = env.path().to_owned();
    // A skill of the user's with one of TermOS's names.
    let theirs = home.join(".agents/skills/termos-dev");
    std::fs::create_dir_all(&theirs).expect("mkdir");
    std::fs::write(theirs.join("SKILL.md"), "their own").expect("write");

    // The server installs them as it starts: both CLIs are on PATH.
    let out = env.run(&["new", "-d"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for dir in [".claude/skills", ".agents/skills"] {
        let skill = home.join(dir).join("termos-agents/SKILL.md");
        assert!(skill.is_file(), "{}", skill.display());
        let text = std::fs::read_to_string(&skill).expect("read");
        assert!(text.starts_with("---\nname: termos-agents\n"), "{text}");
        assert!(text.contains("termos agent ask"));
    }
    assert!(home.join(".claude/skills/termos/.termos-skill").is_file());
    assert_eq!(
        std::fs::read_to_string(theirs.join("SKILL.md")).expect("read"),
        "their own"
    );
    assert!(
        home.join(".claude/skills/mine").is_dir(),
        "the user's skill stays"
    );

    // A new account links the default's skills: one folder for both.
    run(env, &["account", "add", "claude", "work", "--no-sign-in"]);
    let status = run(env, &["skills", "status"]);
    assert!(
        status.contains("Claude Code (default), Claude Code · work"),
        "{status}"
    );
    assert!(
        status.contains("a skill of yours has this name"),
        "{status}"
    );
    assert!(
        home.join("data/termos/accounts/claude/work/skills/termos/SKILL.md")
            .is_file()
    );

    // Removed, they stay removed when the server starts again.
    let removed = run(env, &["skills", "remove"]);
    assert!(removed.contains("removed"), "{removed}");
    assert!(!home.join(".claude/skills/termos").exists());
    assert!(theirs.join("SKILL.md").is_file(), "never theirs");
    run(env, &["kill-server"]);
    run(env, &["new", "-d"]);
    assert!(!home.join(".claude/skills/termos").exists());
    let status = run(env, &["skills", "status"]);
    assert!(status.contains("turned it off"), "{status}");

    // Back on request; `show` prints one.
    let installed = run(env, &["skills", "install"]);
    assert!(installed.contains("installed"), "{installed}");
    assert!(home.join(".claude/skills/termos/SKILL.md").is_file());
    let shown = run(env, &["skills", "show", "termos"]);
    assert!(shown.contains("tos run -p"), "{shown}");
    run(env, &["kill-server"]);
}

/// Sign-in and API keys (M11d, D-112): the CLIs' own sign-in commands run
/// in the account's folder with inherited credentials dropped; Codex keeps
/// its key itself; Claude Code's goes to TermOS's secrets file and is
/// named, never shown.
#[test]
fn sign_in_and_keys_go_through_the_clis_own_commands() {
    use termos_ai::secrets::{Backend, SecretStore};
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let write = |name: &str, body: String| {
        let p = bin.path().join(name);
        std::fs::write(&p, body).expect("write");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    let c = calls.display();
    write(
        "claude",
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n\
             \"auth login\") echo \"claude ${{CLAUDE_CONFIG_DIR:-default}} auth login $3 key=${{ANTHROPIC_API_KEY:-none}} oauth=${{CLAUDE_CODE_OAUTH_TOKEN:-none}}\" >> '{c}'; echo 'Login successful.'; exit 0;;\n\
             \"auth status\") echo '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"work@example.com\",\"subscriptionType\":\"max\"}}'; exit 0;;\n\
             esac\necho \"claude $*\" >> '{c}'\n"
        ),
    );
    write(
        "codex",
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n\
             \"login --with-api-key\") read k; echo \"codex ${{CODEX_HOME:-default}} with-api-key $k\" >> '{c}'; echo 'API key saved.' >&2; exit 0;;\n\
             \"login --device-auth\") echo \"codex ${{CODEX_HOME:-default}} device-auth\" >> '{c}'; exit 0;;\n\
             \"login status\") echo 'API key configured - sk-proj-***abcde' >&2; exit 0;;\n\
             \"logout \") echo \"codex ${{CODEX_HOME:-default}} logout\" >> '{c}'; exit 0;;\n\
             esac\necho \"codex $*\" >> '{c}'\n"
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars(
        "[secrets]\nstore = \"file\"\n",
        vec![
            ("PATH", path.into()),
            // Inherited credentials that must not reach a TermOS account.
            ("ANTHROPIC_API_KEY", "sk-ant-inherited".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN", "inherited-token".into()),
        ],
    );
    let data = env.paths().data_dir;
    let log = || std::fs::read_to_string(&calls).unwrap_or_default();

    run(&env, &["account", "add", "claude", "work", "--no-sign-in"]);
    let out = run(&env, &["account", "sign-in", "claude", "work"]);
    assert!(
        out.contains("signed in: Claude Max · work@example.com"),
        "{out}"
    );
    let work = data.join("accounts/claude/work");
    assert!(
        log().contains(&format!(
            "claude {} auth login  key=none oauth=none",
            work.display()
        )),
        "{}",
        log()
    );
    run(&env, &["account", "sign-in", "claude", "work", "--console"]);
    assert!(log().contains("auth login --console"), "{}", log());
    // `default` keeps the environment as it is (as its windows do), and
    // the sign-in says what comes before it.
    let out = env.run(&["account", "sign-in", "claude", "default"]);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("your environment sets ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN")
            && !said.contains("sk-ant-inherited"),
        "{said}"
    );
    assert!(
        log().contains("claude default auth login  key=sk-ant-inherited oauth=inherited-token"),
        "{}",
        log()
    );

    run(&env, &["account", "add", "codex", "work", "--no-sign-in"]);
    let codex_work = data.join("accounts/codex/work");
    run(&env, &["account", "sign-in", "codex", "work", "--device"]);
    assert!(log().contains(&format!("codex {} device-auth", codex_work.display())));
    // Codex's key: piped to `codex login --with-api-key`, and only named.
    let mut cmd = env.command_in(env.path(), &["account", "key", "codex", "work"]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(b"sk-proj-fixture\n").expect("write");
    }
    let out = child.wait_with_output().expect("wait");
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{shown}");
    assert!(
        shown.contains("signed in: API key") && !shown.contains("sk-proj"),
        "{shown}"
    );
    assert!(log().contains(&format!(
        "codex {} with-api-key sk-proj-fixture",
        codex_work.display()
    )));
    let status = run(&env, &["account", "status", "codex"]);
    assert!(
        status.contains("work         signed in: API key"),
        "{status}"
    );

    // Claude Code's key: typed into a terminal, kept in the secrets file.
    let store = SecretStore::with_backend(Backend::File, termos_ai::secrets::file_path(&data))
        .with_work_factor(4);
    store
        .create(secrecy::SecretString::from("pass phrase".to_owned()))
        .expect("create");
    let t = common::Termos::start_in(std::sync::Arc::clone(&env));
    t.wait_ready();
    let k = t.client(&["account", "key", "claude", "work"]);
    k.wait_for("the key prompt", std::time::Duration::from_secs(20), |s| {
        s.contains("API key for Claude Code · work")
    });
    k.send(b"sk-ant-fixture-key\r");
    k.wait_for(
        "the passphrase prompt",
        std::time::Duration::from_secs(20),
        |s| s.contains("Passphrase for"),
    );
    k.send(b"pass phrase\r");
    let screen = k.wait_for("the key stored", std::time::Duration::from_secs(20), |s| {
        s.contains("now uses this API key")
    });
    assert!(screen.contains("account.claude.work") && screen.contains("choose Yes"));
    assert!(!screen.contains("sk-ant-fixture-key"), "{screen}");
    let reopened = SecretStore::with_backend(Backend::File, termos_ai::secrets::file_path(&data));
    reopened
        .unlock(secrecy::SecretString::from("pass phrase".to_owned()))
        .expect("unlock");
    {
        use secrecy::ExposeSecret as _;
        assert_eq!(
            reopened
                .get("account.claude.work")
                .expect("kept")
                .expose_secret(),
            "sk-ant-fixture-key"
        );
    }
    let index = std::fs::read_to_string(data.join("accounts/accounts.toml")).expect("index");
    assert!(
        index.contains("api-key = \"account.claude.work\""),
        "{index}"
    );
    assert!(!index.contains("sk-ant"), "{index}");
    let status = run(&env, &["account", "status", "claude", "work"]);
    assert!(
        status.contains("API key (TermOS's secret store)"),
        "{status}"
    );
    // A sign-in would be shadowed by the key: refused until it is removed.
    let refused = env.run(&["account", "sign-in", "claude", "work"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--remove"));
}

/// Gemini CLI's accounts (M11d, D-112): a home of its own (`.gemini` in
/// it), settings copied with TermOS's server registered, file logins, and
/// its sign-in chosen in advance.
#[test]
fn gemini_accounts_have_homes_of_their_own() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let gemini = bin.path().join("gemini");
    std::fs::write(
        &gemini,
        format!(
            "#!/bin/sh\necho \"gemini home=${{GEMINI_CLI_HOME:-default}} files=${{GEMINI_FORCE_FILE_STORAGE:-no}} browser=${{NO_BROWSER:-yes}} key=${{GEMINI_API_KEY:-none}} $*\" >> '{}'\n",
            calls.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&gemini, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars(
        "",
        vec![
            ("PATH", path.into()),
            ("GEMINI_API_KEY", "inherited-key".into()),
        ],
    );
    let user = env.path().join(".gemini");
    std::fs::create_dir_all(&user).expect("mkdir");
    std::fs::write(
        user.join("settings.json"),
        "{\"ui\":{\"theme\":\"Dracula\"}}",
    )
    .expect("write");
    std::fs::write(user.join("GEMINI.md"), "be brief").expect("write");
    std::fs::write(user.join("oauth_creds.json"), "LOGIN").expect("write");

    let added = run(&env, &["account", "add", "gemini", "work", "--no-sign-in"]);
    assert!(added.contains("Gemini CLI · work"), "{added}");
    let data = env.paths().data_dir;
    let home = data.join("accounts/gemini/work");
    let settings = std::fs::read_to_string(home.join(".gemini/settings.json")).expect("settings");
    assert!(settings.contains("Dracula"), "copied: {settings}");
    assert!(
        settings.contains("\"termos\""),
        "TermOS's server: {settings}"
    );
    assert!(
        home.join(".gemini/GEMINI.md").exists(),
        "instructions shared"
    );
    assert!(
        !home.join(".gemini/oauth_creds.json").exists(),
        "never the login"
    );
    assert!(
        home.join(".agents").exists(),
        "Agent Skills from the user's home"
    );
    let status = run(&env, &["account", "status", "gemini"]);
    assert!(
        status.contains("default      signed in: Google account"),
        "{status}"
    );
    assert!(status.contains("work         not signed in"), "{status}");

    // Its sign-in: the Google account chosen in advance, in its home.
    run(&env, &["account", "sign-in", "gemini", "work"]);
    let settings = std::fs::read_to_string(home.join(".gemini/settings.json")).expect("settings");
    assert!(
        settings.contains("\"selectedType\": \"oauth-personal\""),
        "{settings}"
    );
    let log = std::fs::read_to_string(&calls).expect("calls");
    assert!(
        log.contains(&format!(
            "gemini home={} files=true browser=true key=none",
            home.display()
        )),
        "inherited key dropped, no browser here: {log}"
    );
    std::fs::write(
        home.join(".gemini/google_accounts.json"),
        "{\"active\":\"work@example.com\",\"old\":[]}",
    )
    .expect("write");
    let status = run(&env, &["account", "status", "gemini", "work"]);
    assert!(
        status.contains("signed in: Google account · work@example.com"),
        "{status}"
    );
}
