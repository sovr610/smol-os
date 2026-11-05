// SPDX-License-Identifier: GPL-3.0-or-later
//! `termos harness` through the real binary (M11c, D-100), with stand-ins
//! for Claude Code and Codex on `PATH` that print their JSON lines: a goal
//! planned and run, a plan saved and run again, and where each kind of
//! task goes.

#![cfg(unix)]

mod common;

use std::path::Path;

fn script(path: &Path, body: &str) {
    common::write_script(path, &format!("#!/bin/sh\n{body}"));
}

const PLAN: &str = r#"{"summary": "Add a line", "tasks": [{"id": "t1", "kind": "coding", "title": "Write it", "prompt": "Add a line to notes.txt."}, {"id": "t2", "kind": "review", "title": "Check it", "prompt": "Check notes.txt.", "depends_on": ["t1"]}]}"#;

#[test]
fn a_goal_is_planned_run_and_audited() {
    let bin = tempfile::tempdir().expect("tempdir");
    let calls = bin.path().join("calls");
    // Claude Code plans and writes; the review goes to Codex, another
    // backend than the author (cross-review). (The scripts quote their
    // lines in '…'.)
    let result = |text: &str, error: bool| {
        serde_json::json!({"type": "result", "subtype": "success", "is_error": error, "result": text,
            "usage": {"input_tokens": 3, "output_tokens": 2}})
        .to_string()
    };
    script(
        &bin.path().join("claude"),
        &format!(
            r#"prompt="$(cat)"
echo "claude $*" >> '{calls}'
case "$prompt" in
  *"Answer with only this JSON"*) printf '%s\n' '{plan}';;
  *"task t2 "*) printf '%s\n' '{limit}'; exit 1;;
  *) echo "a new line" >> notes.txt; printf '%s\n' '{wrote}';;
esac
"#,
            calls = calls.display(),
            plan = result(PLAN, false),
            limit = result("You have hit your limit · resets 5pm", true),
            wrote = result("Added the line.", false),
        ),
    );
    script(
        &bin.path().join("codex"),
        &format!(
            r#"cat > /dev/null
echo "codex $*" >> '{calls}'
printf '%s\n' '{msg}'
"#,
            calls = calls.display(),
            msg = serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": "The line reads well."}}),
        ),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars("", vec![("PATH", path.into())]);
    let project = env.path().join("project");
    std::fs::create_dir_all(&project).expect("project");
    let p = project.display().to_string();

    // Where things would go.
    let out = env.run(&["harness", "status", "--project", &p]);
    let status = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{status}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        status.contains("ready (") && status.contains("coding → balanced: Claude Code"),
        "{status}"
    );

    // Planned and run.
    let out = env.run(&[
        "harness",
        "run",
        "--yes",
        "--project",
        &p,
        "add",
        "a",
        "line",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for want in [
        "Plan: Add a line",
        "t1   coding",
        "→ Claude Code (its default model), then Codex CLI (its default model)",
        "[t1] done in",
        "[t2] → Codex CLI",
        "[t2] done in",
        "2 of 2 tasks done",
        "The line reads well.",
    ] {
        assert!(text.contains(want), "{want:?} in:\n{text}");
    }
    assert_eq!(
        std::fs::read_to_string(project.join("notes.txt")).expect("notes"),
        "a new line\n"
    );
    let id = text
        .lines()
        .find_map(|l| l.strip_prefix("Hand-offs: `termos audit show "))
        .and_then(|l| l.strip_suffix('`'))
        .expect("the log's name")
        .to_owned();
    let out = env.run(&["audit", "show", &id]);
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(
        log.contains("handoff") || log.contains("Handoff") || log.contains("\"outcome\""),
        "{log}"
    );
    let out = env.run(&["audit", "verify"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // A plan saved, then run as it is.
    let out = env.run(&[
        "harness",
        "plan",
        "--json",
        "--project",
        &p,
        "add",
        "a",
        "line",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = env.path().join("plan.json");
    std::fs::write(&saved, &out.stdout).expect("save");
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(plan["tasks"][1]["kind"], "review");
    let out = env.run(&[
        "harness",
        "run",
        "--yes",
        "--project",
        &p,
        "--plan",
        &saved.display().to_string(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Without --yes and without a terminal, nothing runs.
    let before = std::fs::read_to_string(project.join("notes.txt")).expect("notes");
    let out = env.run(&[
        "harness",
        "run",
        "--project",
        &p,
        "--plan",
        &saved.display().to_string(),
    ]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Not run"));
    assert_eq!(
        std::fs::read_to_string(project.join("notes.txt")).expect("notes"),
        before
    );
    let calls = std::fs::read_to_string(&calls).expect("calls");
    assert!(calls.contains("--permission-mode acceptEdits"), "{calls}");
    assert!(calls.contains("--sandbox read-only"), "{calls}");
}

/// `termos harness best` (D-114): Claude Code and Codex stand-ins answer
/// one prompt, each in a worktree of its own; the project's tests run on
/// each; the attempts are listed side by side with their worktrees.
#[test]
fn best_of_n_lists_attempts_side_by_side() {
    let bin = tempfile::tempdir().expect("tempdir");
    let claude_done = serde_json::json!({"type": "result", "subtype": "success", "is_error": false,
        "result": "Wrote it.", "usage": {"input_tokens": 3, "output_tokens": 2}});
    script(
        &bin.path().join("claude"),
        &format!("cat > /dev/null\necho good > a.txt\nprintf '%s\\n' '{claude_done}'\n"),
    );
    let codex_done = serde_json::json!({"type": "item.completed", "item": {"type": "agent_message", "text": "Wrote it too."}});
    script(
        &bin.path().join("codex"),
        &format!("cat > /dev/null\necho bad > a.txt\nprintf '%s\\n' '{codex_done}'\n"),
    );
    let path = std::env::join_paths(std::iter::once(bin.path().to_owned()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH");
    let env = common::Env::with_vars("", vec![("PATH", path.into())]);
    let project = env.path().join("project");
    std::fs::create_dir_all(&project).expect("project");
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(args)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.com"]);
    std::fs::write(project.join("a.txt"), "start\n").expect("w");
    std::fs::write(
        project.join("termos.toml"),
        "[tasks]\ntest = \"grep -q good a.txt\"\n",
    )
    .expect("w");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    let p = project.display().to_string();

    let out = env.run(&[
        "harness",
        "best",
        "--with",
        "claude",
        "--project",
        &p,
        "write",
        "it",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("at least two"));

    let out = env.run(&[
        "harness",
        "best",
        "--with",
        "claude,codex",
        "--project",
        &p,
        "make",
        "a.txt",
        "good",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let row = |id: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no {id} in:\n{text}"))
            .to_owned()
    };
    let (a1, a2) = (row("a1"), row("a2"));
    assert!(
        a1.contains("Claude Code") && a1.contains("tests passed") && a1.contains("best-"),
        "{a1}"
    );
    assert!(
        a2.contains("Codex CLI") && a2.contains("tests failed") && a2.contains("+1 -1 in 1 file"),
        "{a2}"
    );
    assert!(text.contains("2 of 2 tasks done"), "{text}");
    // The attempts are worktrees, as `termos worktree` sees them.
    let list = env.run_in(&project, &["worktree", "list"]);
    let list = String::from_utf8_lossy(&list.stdout);
    assert_eq!(
        list.lines()
            .filter(|l| l.trim_start().starts_with("best-"))
            .count(),
        2,
        "{list}"
    );
    assert!(list.contains("1 commit"), "{list}");
}
