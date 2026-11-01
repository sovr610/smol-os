// SPDX-License-Identifier: GPL-3.0-or-later
//! The real binary on a real PTY: raw mode, input decoding, the encoder and
//! the desktop together.

mod common;

use std::time::Duration;

use common::Termos;

const T: Duration = Duration::from_secs(20);

#[test]
fn split_type_close_and_exit() {
    let mut t = Termos::start("");
    t.wait_ready();
    t.send(b"echo e2''e-ok\r");
    t.wait_for("command output", T, |s| {
        s.lines()
            .any(|l| l.trim_end().ends_with("e2e-ok") && !l.contains("echo"))
    });

    // C-b % splits; the new pane is on the right.
    t.send(b"\x02%");
    let screen = t.wait_for("a vertical separator", T, |s| s.contains('│'));
    assert!(
        screen
            .lines()
            .last()
            .is_some_and(|l| l.starts_with("[main]"))
    );
    t.send(b"echo ri''ght\r");
    let screen = t.wait_for("output in the right pane", T, |s| {
        s.lines()
            .any(|l| l.trim_end().ends_with("right") && !l.contains("echo"))
    });
    let line = screen
        .lines()
        .find(|l| l.trim_end().ends_with("right") && !l.contains("echo"))
        .unwrap_or_default();
    assert!(line.find("right").unwrap_or(0) > 40, "{line:?}");

    // Palette via C-Space (NUL), then Escape.
    t.send(b"\x00");
    t.wait_for("the palette", T, |s| s.contains("Command palette"));
    t.send(b"\x1b");
    t.wait_for("the palette to close", T, |s| {
        !s.contains("Command palette")
    });

    // Close both shells: the session ends and the terminal is restored.
    t.send(b"exit\r");
    t.wait_for("the split to close", T, |s| !s.contains('│'));
    t.send(b"exit\r");
    let status = t.wait_exit(T);
    assert!(status.success(), "{status:?}");
    let out = t.raw_output();
    assert!(out.contains("[exited: the last pane was closed]"), "{out}");
    // Alternate screen left, raw mode off: the restore sequences were sent.
    assert!(out.contains("\x1b[?1049l"), "alternate screen not left");
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn detach_ls_attach_msg_and_kill_server() {
    let mut t = Termos::start("");
    t.wait_ready();
    t.send(b"echo befo''re-detach\r");
    t.wait_for("command output", T, |s| s.contains("before-detach"));
    t.send(b"\x02%");
    t.wait_for("the split", T, |s| s.contains('│'));

    // C-b d detaches; the client exits and says so.
    t.send(b"\x02d");
    let status = t.wait_exit(T);
    assert!(status.success(), "{status:?}");
    assert!(
        t.raw_output().contains("[detached (from session main)]"),
        "{}",
        t.raw_output()
    );

    // The session keeps running without a client.
    let ls = t.run(&["ls"]);
    assert!(ls.status.success(), "{ls:?}");
    let ls = stdout(&ls);
    assert!(ls.starts_with("main: 1 workspaces, 2 panes"), "{ls}");
    assert!(!ls.contains("attached"), "{ls}");
    let panes: serde_json::Value =
        serde_json::from_str(&stdout(&t.run(&["msg", "pane.list"]))).expect("JSON");
    assert_eq!(panes.as_array().map(Vec::len), Some(2));

    // Reattach from another terminal: the same screen.
    let t2 = t.client(&["attach"]);
    t2.wait_ready();
    t2.wait_for("the old output and the split", T, |s| {
        s.contains("before-detach") && s.contains('│')
    });
    assert!(stdout(&t.run(&["ls"])).contains("(attached)"));

    // Scripts drive it.
    let sent = t.run(&["msg", "pane.send-text", r#"{"text": "echo via-m''sg\r"}"#]);
    assert!(sent.status.success(), "{sent:?}");
    t2.wait_for("text typed by msg", T, |s| {
        s.lines()
            .any(|l| l.trim_end().ends_with("via-msg") && !l.contains("echo"))
    });
    t.run(&["msg", "notify.send", r#"{"text": "hello from msg"}"#]);
    t2.wait_for("the notification", T, |s| s.contains("hello from msg"));
    let bad = t.run(&["msg", "no.such-method"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown method"));

    // Inside a pane, termos finds its server and session through $TERMOS.
    t2.send(
        format!(
            "{} msg session.rename '{{\"name\": \"work\"}}'\r",
            env!("CARGO_BIN_EXE_termos")
        )
        .as_bytes(),
    );
    t2.wait_for("the renamed session", T, |s| {
        s.lines().last().is_some_and(|l| l.starts_with("[work]"))
    });
    let mut t2 = t2;
    t2.send(format!("{} detach\r", env!("CARGO_BIN_EXE_termos")).as_bytes());
    t2.wait_exit(T);
    assert!(t2.raw_output().contains("[detached (from session work)]"));

    // Attaching to a missing session fails cleanly; kill-server stops all.
    let t3 = t.client(&["attach", "-t", "nope"]);
    let mut t3 = t3;
    t3.wait_exit(T);
    assert!(
        t3.raw_output().contains("no session named `nope`"),
        "{}",
        t3.raw_output()
    );
    let killed = t.run(&["kill-server"]);
    assert!(killed.status.success(), "{killed:?}");
    assert!(!t.env.socket().exists(), "socket left behind");
    let ls = t.run(&["ls"]);
    assert!(!ls.status.success());
    assert!(stdout(&ls).contains("no server running"));
    // Its session was saved, so it can be restored.
    let restored = t.run(&["restore"]);
    assert!(restored.status.success(), "{restored:?}");
    assert_eq!(stdout(&restored).trim(), "restored: work");
    assert!(stdout(&t.run(&["ls"])).starts_with("work: 1 workspaces, 2 panes"));
}
