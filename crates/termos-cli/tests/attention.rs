// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent attention and resume (M12b, D-115, D-116) with the real binary:
//! hooks registered in each CLI's settings, a hook's event marking its
//! pane, `tos read --command` reading a shell's last command, and an
//! agent window continuing its conversation.

mod common;

use std::path::Path;
use std::time::Duration;

const T: Duration = Duration::from_secs(30);

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `termos ARGS` with the test's folder as home on every platform
/// (Windows finds the CLIs' folders through `USERPROFILE`, not `HOME`).
fn run_home(env: &common::Env, args: &[&str]) -> std::process::Output {
    env.command_in(env.path(), args)
        .env("USERPROFILE", env.path())
        .output()
        .expect("termos runs")
}

#[test]
fn hooks_are_added_to_every_cli_and_taken_out() {
    let env = common::Env::new("");
    let out = run_home(&env, &["agent", "hooks"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let home = env.path();
    let claude = home.join(".claude/settings.json");
    let codex = home.join(".codex/hooks.json");
    let gemini = home.join(".gemini/settings.json");
    for (file, cli) in [(&claude, "claude"), (&codex, "codex"), (&gemini, "gemini")] {
        let t = read(file);
        assert!(
            t.contains(&format!("agent event --cli {cli}")),
            "{}: {t}",
            file.display()
        );
    }
    assert!(read(&claude).contains("PermissionRequest") && read(&claude).contains("\"Stop\""));
    assert!(read(&gemini).contains("AfterAgent"));
    assert!(text.contains("Codex asks you to trust them"), "{text}");
    // Again: nothing to change.
    let again = run_home(&env, &["agent", "hooks", "claude"]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("has TermOS's hooks"));
    // Out again, the files' other settings kept.
    let out = run_home(&env, &["agent", "hooks", "--remove"]);
    assert!(out.status.success());
    for file in [&claude, &codex, &gemini] {
        assert!(!read(file).contains("agent event"), "{}", file.display());
    }
}

/// Outside TermOS a hook does nothing, quietly and successfully.
#[test]
fn a_hook_outside_termos_does_nothing() {
    let env = common::Env::new("");
    let mut cmd = env.command_in(env.path(), &["agent", "event", "--cli", "claude"]);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn");
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().expect("stdin");
        stdin
            .write_all(br#"{"hook_event_name":"Stop"}"#)
            .expect("write");
    }
    let out = child.wait_with_output().expect("waits");
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

/// A hook in a pane marks it: the toast, the tab's badge; typing into the
/// pane answers it.
#[test]
fn a_hooks_event_marks_its_pane_until_answered() {
    let env = common::Env::new("[notifications]\nsystem = false\n");
    let t = common::Termos::start_in(std::sync::Arc::clone(&env));
    t.wait_ready();
    let exe = env!("CARGO_BIN_EXE_termos");
    let line = if cfg!(windows) {
        format!(
            "echo {{\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"session_id\":\"s1\"}} | \"{exe}\" agent event --cli claude\r"
        )
    } else {
        format!(
            "echo '{{\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"session_id\":\"s1\"}}' | '{exe}' agent event --cli claude\r"
        )
    };
    t.send(line.as_bytes());
    let screen = t.wait_for("the agent's mark", T, |s| s.contains("\u{25cf} needs you"));
    let tabs = screen.lines().last().unwrap_or_default().to_owned();
    assert!(tabs.contains('\u{25cf}'), "{tabs}");
    // An answer clears it.
    t.send(b"\r");
    t.wait_for("the mark to go", T, |s| !s.contains("\u{25cf} needs you"));
}

/// `tos read --command` gives a pane's last command, its output and
/// status (bash 4.4 or later marks them with TermOS's integration).
#[cfg(unix)]
#[test]
fn tos_reads_the_last_command() {
    let Some(bash) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("bash"))
        .find(|p| p.is_file())
    else {
        eprintln!("skipped: no bash");
        return;
    };
    let v = std::process::Command::new(&bash)
        .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
        .output()
        .expect("bash");
    if String::from_utf8_lossy(&v.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or(0)
        < 404
    {
        eprintln!("skipped: bash older than 4.4");
        return;
    }
    let env = common::Env::new(&format!(
        "[shell]\nprogram = {:?}\n",
        bash.display().to_string()
    ));
    let t = common::Termos::start_in(std::sync::Arc::clone(&env));
    t.wait_ready();
    t.send(b"echo alpha; echo beta; false\r");
    t.wait_for("the command", T, |s| s.contains("beta"));
    t.send(b"tos read --command\r");
    let screen = t.wait_for("tos's answer", T, |s| s.contains("[exit status 1]"));
    let text = screen.replace(' ', "");
    assert!(
        text.contains("$echoalpha;echobeta;false\nalpha\nbeta\n[exitstatus1]"),
        "{screen}"
    );
}

/// An agent window restored after a restart continues its conversation:
/// the one its hooks reported, when it is still there.
#[cfg(unix)]
#[test]
fn an_agent_window_continues_its_conversation() {
    let bin = tempfile::tempdir().expect("tempdir");
    common::write_script(
        &bin.path().join("claude"),
        "#!/bin/sh\necho \"claude got: $*\"\n",
    );
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let env = common::Env::with_vars(
        "[agents.external.claude]\nsandbox = false\n",
        vec![("PATH", path.into())],
    );
    let work = env.path().join("work");
    std::fs::create_dir_all(&work).expect("mkdir");
    let work = work.canonicalize().expect("real path");
    let folder = env
        .path()
        .join(".claude/projects")
        .join(termos_devkit::conversations::project_folder(&work));
    std::fs::create_dir_all(&folder).expect("mkdir");
    std::fs::write(folder.join("older.jsonl"), "{}\n").expect("write");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(folder.join("newest.jsonl"), "{}\n").expect("write");
    let run = |args: &[&str]| {
        let out = env.run_in(&work, args);
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    // The one reported.
    let text = run(&[
        "agent",
        "external",
        "--resume",
        "--conversation",
        "older",
        "claude",
    ]);
    assert!(
        text.contains(&format!(
            "claude got: --resume {}",
            folder.join("older.jsonl").display()
        )),
        "{text}"
    );
    // Gone: the folder's latest.
    let text = run(&[
        "agent",
        "external",
        "--resume",
        "--conversation",
        "gone",
        "claude",
    ]);
    assert!(text.contains("newest.jsonl"), "{text}");
    // The window added TermOS's hooks to the account.
    assert!(read(&env.path().join(".claude/settings.json")).contains("agent event --cli claude"));
}
