// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform details for tests that drive a real shell in a pane: `/bin/sh`
//! on Unix, `cmd.exe` on Windows.

#![allow(dead_code, unreachable_pub)]

/// The shell panes run in tests.
pub const SHELL: &str = if cfg!(windows) { "cmd.exe" } else { "/bin/sh" };

/// The `shell = …` config line for [`SHELL`].
pub fn shell_config() -> String {
    format!("shell = {SHELL:?}\n")
}

/// Environment for test panes. On Windows it makes cmd's prompt `$ `, so
/// prompt checks (a line ending in `$` or `#`) work on every platform.
pub fn pane_env() -> Vec<(String, String)> {
    if cfg!(windows) {
        vec![("PROMPT".into(), "$$$S".into())]
    } else {
        Vec::new()
    }
}

/// An `echo` that prints `a` and `b` joined, while the command line itself
/// does not contain the joined word (so tests can tell output from the
/// echoed command).
pub fn echo_split(a: &str, b: &str) -> String {
    if cfg!(windows) {
        format!("echo {a}^{b}")
    } else {
        format!("echo {a}''{b}")
    }
}

/// A command printing `line1` to `line{n}`, one per line.
pub fn print_lines(n: u32) -> String {
    if cfg!(windows) {
        format!("for /L %i in (1,1,{n}) do @echo line%i")
    } else {
        let list: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        format!("for i in {}; do echo line$i; done", list.join(" "))
    }
}

/// The interpreter for the Python test plugin.
pub const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };
