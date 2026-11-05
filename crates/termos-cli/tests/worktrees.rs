// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos worktree` with real git (M12a, D-114): made with its ports and
//! setup, reviewed, committed, merged and archived, restored, discarded;
//! pull requests through a stand-in `gh`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A project with one commit on `main`, an ignored `.env` and a setup
/// that writes its port.
fn project(env: &common::Env) -> PathBuf {
    let root = env.path().join("proj");
    std::fs::create_dir_all(&root).expect("mkdir");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.com"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("a.txt"), "one\n").expect("write");
    std::fs::write(root.join(".gitignore"), ".env\nport.txt\n").expect("write");
    std::fs::write(root.join(".env"), "TOKEN=fixture\n").expect("write");
    let setup = if cfg!(windows) {
        "echo %PORT%> port.txt"
    } else {
        "echo $PORT > port.txt"
    };
    std::fs::write(
        root.join("termos.toml"),
        format!("[worktree]\nsetup = {setup:?}\n"),
    )
    .expect("write");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    root
}

fn run(env: &common::Env, dir: &Path, args: &[&str]) -> String {
    let out = env.run_in(dir, args);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "termos {}: {text}", args.join(" "));
    text
}

fn fails(env: &common::Env, dir: &Path, args: &[&str]) -> String {
    let out = env.run_in(dir, args);
    assert!(
        !out.status.success(),
        "termos {} should fail",
        args.join(" ")
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const CONFIG: &str = "[worktrees]\nport-range = [47000, 47999]\nports = 4\n";

#[test]
fn worktrees_are_made_merged_archived_and_restored() {
    let env = common::Env::new(CONFIG);
    let root = project(&env);
    let out = run(&env, &root, &["worktree", "new", "--title", "Add docs"]);
    assert!(
        out.contains("worktree add-docs on termos/add-docs (from main)"),
        "{out}"
    );
    assert!(out.contains("copied .env"), "{out}");
    let wt = env
        .paths()
        .data_dir
        .join("worktrees")
        .read_dir()
        .expect("worktrees")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("the repository's folder")
        .join("add-docs");
    assert!(wt.join(".env").exists(), "the ignored file came along");
    // The setup ran there, with the worktree's first port.
    let port: u16 = std::fs::read_to_string(wt.join("port.txt"))
        .expect("port.txt")
        .trim()
        .parse()
        .expect("a port");
    assert!((47000..=47999).contains(&port));
    let envs = run(&env, &wt, &["worktree", "env"]);
    assert!(envs.contains(&format!("PORT={port}")), "{envs}");
    assert!(envs.contains("TERMOS_PORT_3="), "{envs}");
    assert!(envs.contains("TERMOS_WORKTREE=add-docs"), "{envs}");

    let list = run(&env, &root, &["worktree", "list"]);
    assert!(
        list.contains("add-docs") && list.contains("no changes"),
        "{list}"
    );
    // A second one: its own ports, its own name.
    let out = run(&env, &root, &["worktree", "new", "--no-setup"]);
    assert!(out.contains("worktree task-1"), "{out}");
    assert!(fails(&env, &root, &["worktree", "new", "task-1"]).contains("already has a worktree"));

    // Review: an edit and a new file, then a commit.
    std::fs::write(wt.join("a.txt"), "one\ntwo\n").expect("write");
    std::fs::write(wt.join("docs.md"), "# Docs\n").expect("write");
    let status = run(&env, &root, &["worktree", "status", "add-docs"]);
    assert!(status.contains("M a.txt  +1 -0"), "{status}");
    assert!(status.contains("? docs.md  +1 -0"), "{status}");
    let diff = run(&env, &wt, &["worktree", "diff"]);
    assert!(diff.contains("+two") && diff.contains("+# Docs"), "{diff}");
    assert!(fails(&env, &wt, &["worktree", "merge"]).contains("not committed"));
    run(&env, &wt, &["worktree", "commit", "-m", "Docs"]);

    // Merged into main, archived, its branch gone.
    let out = run(&env, &root, &["worktree", "merge", "add-docs"]);
    assert!(
        out.contains("main moved forward") && out.contains("archived add-docs"),
        "{out}"
    );
    assert!(root.join("docs.md").exists());
    assert!(!wt.exists());
    let list = run(&env, &root, &["worktree", "list"]);
    assert!(!list.contains("add-docs"), "{list}");
    let all = run(&env, &root, &["worktree", "list", "--all"]);
    assert!(
        all.contains("add-docs") && all.contains("archived"),
        "{all}"
    );

    // Restored from what was kept, then discarded and forgotten.
    let out = run(&env, &root, &["worktree", "restore", "add-docs"]);
    assert!(out.contains("restored add-docs"), "{out}");
    assert!(wt.join("docs.md").exists() && wt.join(".env").exists());
    std::fs::write(wt.join("scratch.txt"), "x").expect("write");
    assert!(fails(&env, &root, &["worktree", "archive", "add-docs"]).contains("untracked"));
    run(&env, &root, &["worktree", "discard", "add-docs", "--yes"]);
    assert!(!wt.exists());
    run(&env, &root, &["worktree", "forget", "add-docs"]);
    let all = run(&env, &root, &["worktree", "list", "--all"]);
    assert!(!all.contains("add-docs") && all.contains("task-1"), "{all}");
    assert!(fails(&env, env.path(), &["worktree", "status"]).contains("not in a worktree"));
}

#[cfg(unix)]
#[test]
fn pull_requests_go_through_gh_or_the_forge_page() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let gh = bin.path().join("gh");
    common::write_script(
        &gh,
        &format!(
            "#!/bin/sh\nprintf '%s|' \"$@\" > '{}'\necho https://github.com/owner/repo/pull/7\n",
            calls.display()
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars(CONFIG, vec![("PATH", path.into())]);
    let root = project(&env);
    // A GitHub remote whose pushes land in a local bare repository.
    let bare = env.path().join("remote.git");
    git(
        env.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &bare.to_string_lossy(),
        ],
    );
    let url = "https://github.com/owner/repo.git";
    git(&root, &["remote", "add", "origin", url]);
    git(
        &root,
        &[
            "config",
            &format!("url.{}.pushInsteadOf", bare.display()),
            url,
        ],
    );
    git(&root, &["push", "-q", "origin", "main"]);

    run(
        &env,
        &root,
        &[
            "worktree",
            "new",
            "fix",
            "--title",
            "Fix the bug",
            "--no-setup",
        ],
    );
    assert!(fails(&env, &root, &["worktree", "pr", "fix"]).contains("no commits"));
    let wt = run(&env, &root, &["worktree", "status", "fix"])
        .lines()
        .nth(2)
        .expect("its folder")
        .trim()
        .to_owned();
    let wt = PathBuf::from(wt);
    std::fs::write(wt.join("fix.txt"), "fixed\n").expect("write");
    run(&env, &wt, &["worktree", "commit", "-m", "Fix it"]);
    let out = run(&env, &root, &["worktree", "pr", "fix", "--draft"]);
    assert!(
        out.contains("pull request https://github.com/owner/repo/pull/7"),
        "{out}"
    );
    let args = std::fs::read_to_string(&calls).expect("gh ran");
    assert!(
        args.starts_with(
            "pr|create|--base|main|--head|termos/fix|--title|Fix the bug|--body|Fix the bug"
        ),
        "{args}"
    );
    assert!(
        args.contains("- Fix it") && args.ends_with("--draft|"),
        "{args}"
    );
    assert!(
        git(&bare, &["branch", "--list", "termos/fix"]).contains("termos/fix"),
        "pushed"
    );
    let list = run(&env, &root, &["worktree", "list"]);
    assert!(
        list.contains("https://github.com/owner/repo/pull/7"),
        "{list}"
    );

    // Without gh: pushed, and the page to open one. Only git is on
    // `PATH`, so no real gh (CI runners have one) is ever reached.
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let only_git = tempfile::tempdir().expect("tempdir");
    std::os::unix::fs::symlink(&real_git, only_git.path().join("git")).expect("link");
    std::fs::write(wt.join("more.txt"), "more\n").expect("write");
    run(&env, &wt, &["worktree", "commit", "-m", "More"]);
    let out = env
        .command_in(&root, &["worktree", "pr", "fix"])
        .env("PATH", only_git.path())
        .output()
        .expect("termos");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.contains("https://github.com/owner/repo/compare/main...termos/fix?expand=1"),
        "{out}"
    );
}

/// In the desktop: a worktree opens as a workspace named after it, its
/// server started and every pane there given its ports; its review window
/// lists what changed.
#[cfg(unix)]
#[test]
fn a_worktree_opens_as_a_workspace_with_its_ports() {
    use std::time::Duration;
    const T: Duration = Duration::from_secs(30);
    let env = common::Env::new(&format!("{CONFIG}agent = \"none\"\n"));
    let root = project(&env);
    std::fs::write(
        root.join("termos.toml"),
        "[worktree]\nservers = \"echo serving-on-$PORT\"\n",
    )
    .expect("write");
    git(&root, &["commit", "-qam", "servers"]);
    run(&env, &root, &["worktree", "new", "wt1", "--no-setup"]);
    let port = run(&env, &root, &["worktree", "env", "wt1"])
        .lines()
        .find_map(|l| l.strip_prefix("PORT=").map(str::to_owned))
        .expect("PORT");

    let t = common::Termos::start_in(std::sync::Arc::clone(&env));
    t.wait_ready();
    assert!(
        env.run(&["msg", "worktree.open", r#"{"arg": "wt1"}"#])
            .status
            .success()
    );
    t.wait_for("its server, with its port", T, |s| {
        s.contains(&format!("serving-on-{port}")) && s.contains("wt1")
    });
    // The shell beside it has the ports too.
    t.send(b"echo shell-port-$TERMOS_PORT_0\r");
    t.wait_for("the shell's port", T, |s| {
        s.contains(&format!("shell-port-{port}"))
    });
    // Opening it again selects it rather than making another.
    assert!(
        env.run(&["msg", "worktree.open", r#"{"arg": "wt1"}"#])
            .status
            .success()
    );
    let tabs = t.screen().lines().last().unwrap_or_default().to_owned();
    assert_eq!(tabs.matches("wt1").count(), 1, "{tabs}");

    // A change, then its review.
    let wt = run(&env, &root, &["worktree", "status", "wt1"])
        .lines()
        .nth(1)
        .expect("its folder")
        .trim()
        .to_owned();
    std::fs::write(PathBuf::from(wt).join("a.txt"), "one\nreviewed\n").expect("write");
    assert!(
        env.run(&["msg", "worktree.review", r#"{"arg": "wt1"}"#])
            .status
            .success()
    );
    t.wait_for("the review window", T, |s| {
        s.contains("Review: wt1") && s.contains("M a.txt") && s.contains("+ reviewed")
    });
}

/// Review comments reach the worktree's agent: typed into its pane while
/// it runs, else its first message when a new one starts there.
#[cfg(unix)]
#[test]
fn review_comments_reach_the_agent() {
    use std::time::Duration;
    const T: Duration = Duration::from_secs(30);
    let bin = tempfile::tempdir().expect("tempdir");
    let heard = bin.path().join("heard");
    let claude = bin.path().join("claude");
    common::write_script(
        &claude,
        &format!(
            "#!/bin/sh\necho \"started: $*\" >> '{h}'\necho stand-in claude\nwhile IFS= read -r line; do echo \"heard: $line\" >> '{h}'; done\n",
            h = heard.display()
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars(
        &format!("{CONFIG}agent = \"claude\"\n[agents.skills]\ninstall = false\n"),
        vec![("PATH", path.into())],
    );
    let root = project(&env);
    run(&env, &root, &["worktree", "new", "wt2", "--no-setup"]);
    let wt = run(&env, &root, &["worktree", "status", "wt2"])
        .lines()
        .nth(1)
        .expect("its folder")
        .trim()
        .to_owned();
    std::fs::write(PathBuf::from(&wt).join("a.txt"), "one\nchanged\n").expect("write");
    let log = || std::fs::read_to_string(&heard).unwrap_or_default();
    let wait = |what: &str, ok: &dyn Fn(&str) -> bool| {
        let end = std::time::Instant::now() + T;
        while !ok(&log()) {
            assert!(
                std::time::Instant::now() < end,
                "timed out waiting for {what}: {}",
                log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    let t = common::Termos::start_in(std::sync::Arc::clone(&env));
    t.wait_ready();
    assert!(
        env.run(&["msg", "worktree.open", r#"{"arg": "wt2"}"#])
            .status
            .success()
    );
    t.wait_for("the agent", T, |s| s.contains("stand-in claude"));
    wait("the agent's start", &|l| l.starts_with("started: "));

    // A comment on the changed file, sent: typed into the running agent.
    let comment = |t: &common::Termos, text: &str| {
        assert!(
            env.run(&["msg", "worktree.review", r#"{"arg": "wt2"}"#])
                .status
                .success()
        );
        t.wait_for("the review", T, |s| {
            s.contains("Review: wt2") && s.contains("M a.txt")
        });
        t.send(b"c");

        t.wait_for("the comment line", T, |s| s.contains("Comment on a.txt"));
        t.send(text.as_bytes());
        t.send(b"\r");
        t.wait_for("the comment", T, |s| s.contains("[Send to agent (1)]"));
        t.send(b"s");
    };
    comment(&t, "please keep it short");
    t.wait_for("the review closed", T, |s| !s.contains("Review: wt2"));
    wait("the comment in the agent", &|l| {
        l.contains("heard: Review comments on your changes in this worktree (branch termos/wt2):")
            && l.contains("heard: - a.txt: please keep it short")
    });

    // No agent running: a new one there starts with them.
    let panes = env.run(&["msg", "pane.list"]);
    let list: serde_json::Value = serde_json::from_slice(&panes.stdout).expect("json");
    let agent = list
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            list.get("panes")
                .and_then(|p| p.as_array())
                .into_iter()
                .flatten(),
        )
        .find(|p| {
            p["title"]
                .as_str()
                .is_some_and(|t| t.starts_with("agent: claude"))
        })
        .and_then(|p| p["id"].as_u64())
        .unwrap_or_else(|| panic!("no agent pane in {list}"));
    assert!(
        env.run(&["msg", "pane.kill", &format!(r#"{{"pane": {agent}}}"#)])
            .status
            .success()
    );
    comment(&t, "and add a test");
    wait("a new agent with them", &|l| {
        l.rsplit("started: ").next().is_some_and(|last| {
            last.starts_with("Review comments") && last.contains("- a.txt: and add a test")
        })
    });
}
