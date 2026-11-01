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
