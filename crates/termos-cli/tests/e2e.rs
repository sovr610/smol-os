// SPDX-License-Identifier: GPL-3.0-or-later
//! The real binary on a real PTY: raw mode, input decoding, the encoder and
//! the desktop together.

mod common;

use std::time::Duration;

use common::Termos;

const T: Duration = Duration::from_secs(20);

/// An `echo` of `a` and `b` joined whose command line does not contain the
/// joined word.
fn echo_split(a: &str, b: &str) -> String {
    if cfg!(windows) {
        format!("echo {a}^{b}\r")
    } else {
        format!("echo {a}''{b}\r")
    }
}

#[test]
fn split_type_close_and_exit() {
    let mut t = Termos::start("");
    t.wait_ready();
    t.send(echo_split("e2", "e-ok").as_bytes());
    t.wait_for("command output", T, |s| {
        s.lines()
            .any(|l| l.trim_end().ends_with("e2e-ok") && !l.contains("echo"))
    });

    // C-b % splits; the new pane is on the right.
    t.send(b"\x02%");
    let screen = t.wait_for("a vertical separator", T, |s| s.contains('│'));
    assert!(screen.lines().last().is_some_and(|l| l.contains("[main]")));
    t.send(echo_split("ri", "ght").as_bytes());
    // The output is in the right-hand cell of a row (after the separator).
    t.wait_for("output in the right pane", T, |s| {
        s.lines().any(|l| {
            l.split('│')
                .nth(1)
                .is_some_and(|cell| is_output(cell, "right"))
        })
    });

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
    let out = t.wait_output(T, "[exited: the last pane was closed]");
    // Alternate screen left, raw mode off: the restore sequences were sent.
    // (A ConPTY renders the screen itself instead of passing them on.)
    if cfg!(unix) {
        assert!(out.contains("\x1b[?1049l"), "alternate screen not left");
    }
}

/// Whether some pane has a line that is `text`, possibly after a prompt
/// (typeahead), but not the command that printed it. Rows hold several
/// panes side by side, separated by `│`.
fn in_a_pane(screen: &str, text: &str) -> bool {
    screen
        .lines()
        .any(|l| l.split('│').any(|cell| is_output(cell, text)))
}

/// Whether one pane's part of a row is `text`, possibly after a prompt.
fn is_output(cell: &str, text: &str) -> bool {
    cell.trim().strip_suffix(text).is_some_and(|before| {
        let before = before.trim_end();
        before.is_empty() || before.ends_with(['$', '#'])
    })
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn detach_ls_attach_msg_and_kill_server() {
    let mut t = Termos::start("");
    t.wait_ready();
    t.send(echo_split("befo", "re-detach").as_bytes());
    t.wait_for("command output", T, |s| s.contains("before-detach"));
    t.send(b"\x02%");
    t.wait_for("the split", T, |s| s.contains('│'));

    // C-b d detaches; the client exits and says so.
    t.send(b"\x02d");
    let status = t.wait_exit(T);
    assert!(status.success(), "{status:?}");
    t.wait_output(T, "[detached (from session main)]");

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
    let mut t2 = t.client(&["attach"]);
    t2.wait_ready();
    t2.wait_for("the old output and the split", T, |s| {
        s.contains("before-detach") && s.contains('│')
    });
    assert!(stdout(&t.run(&["ls"])).contains("(attached)"));

    // Scripts drive it.
    let text = serde_json::json!({ "text": echo_split("via-m", "sg") }).to_string();
    let sent = t.run(&["msg", "pane.send-text", &text]);
    assert!(sent.status.success(), "{sent:?}");
    t2.wait_for("text typed by msg", T, |s| in_a_pane(s, "via-msg"));
    t.run(&["msg", "notify.send", r#"{"text": "hello from msg"}"#]);
    t2.wait_for("the notification", T, |s| s.contains("hello from msg"));
    let bad = t.run(&["msg", "no.such-method"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown method"));

    // Inside a pane, termos finds its server and session through $TERMOS.
    let rename = if cfg!(windows) {
        r#"msg session.rename "{\"name\": \"work\"}""#
    } else {
        r#"msg session.rename '{"name": "work"}'"#
    };
    t2.send(format!("{} {rename}\r", env!("CARGO_BIN_EXE_termos")).as_bytes());
    t2.wait_for("the renamed session", T, |s| {
        s.lines().last().is_some_and(|l| l.contains("[work]"))
    });
    t2.send(format!("{} detach\r", env!("CARGO_BIN_EXE_termos")).as_bytes());
    t2.wait_exit(T);
    t2.wait_output(T, "[detached (from session work)]");

    // Attaching to a missing session fails cleanly; kill-server stops all.
    let mut t3 = t.client(&["attach", "-t", "nope"]);
    t3.wait_exit(T);
    t3.wait_output(T, "no session named `nope`");
    let killed = t.run(&["kill-server"]);
    assert!(killed.status.success(), "{killed:?}");
    assert!(!t.env.server_alive(), "endpoint left behind");
    let ls = t.run(&["ls"]);
    assert!(!ls.status.success());
    assert!(stdout(&ls).contains("no server running"));
    // Its session was saved, so it can be restored.
    let restored = t.run(&["restore"]);
    assert!(restored.status.success(), "{restored:?}");
    assert_eq!(stdout(&restored).trim(), "restored: work");
    assert!(stdout(&t.run(&["ls"])).starts_with("work: 1 workspaces, 2 panes"));
}

/// The terminal says it speaks Kitty graphics (the harness answers the
/// probe like kitty): an image opened in the viewer is uploaded and
/// placed; the palette over it changes the placements; detaching frees
/// the terminal's images.
#[cfg(unix)]
#[test]
fn kitty_images_reach_the_terminal() {
    let mut t = Termos::start("");
    t.wait_ready();
    let path = t.env.path().join("dots.png");
    let pic =
        termos_render::graphics::Picture::from_rgba(40, 20, vec![90; 40 * 20 * 4]).expect("valid");
    std::fs::write(&path, pic.to_png()).expect("write");
    let arg = serde_json::json!({ "arg": path.display().to_string() }).to_string();
    let opened = t.run(&["msg", "image.open", &arg]);
    assert!(opened.status.success(), "{opened:?}");
    let out = t.wait_output(T, "\x1b_Ga=p,i=1,");
    assert!(out.contains("\x1b_Ga=t,f=100,i=1,q=2"), "uploaded as PNG");
    assert_eq!(out.matches("a=t,").count(), 1, "uploaded once");

    t.send(b"\x00");
    t.wait_for("the palette", T, |s| s.contains("Command palette"));
    t.wait_output(T, "\x1b_Ga=d,d=i,i=1,");
    t.send(b"\x1b");
    t.wait_for("the palette to close", T, |s| {
        !s.contains("Command palette")
    });

    t.send(b"\x02d");
    t.wait_exit(T);
    let out = t.wait_output(T, "\x1b_Ga=d,d=A,q=2");
    assert_eq!(out.matches("a=t,").count(), 1, "never uploaded again");
}
