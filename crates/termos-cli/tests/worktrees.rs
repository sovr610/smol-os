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
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    let gh = bin.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s|' \"$@\" > '{}'\necho https://github.com/owner/repo/pull/7\n",
            calls.display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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
