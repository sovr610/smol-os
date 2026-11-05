// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening a web or mail address on the client's machine: the server
//! sends `ServerMsg::Open` when the user Ctrl+clicks a link (M13a, D-118).
//!
//! The address goes to the system's opener as one argument, never through
//! a shell: `xdg-open` on Linux and the BSDs, `open` on macOS, `explorer`
//! on Windows.

use std::process::{Command, Stdio};

/// The schemes a client opens; the server sends no others (file links
/// open in TermOS itself), and the client checks again.
const SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// The program that opens addresses here.
fn opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

/// Whether the client may open `url`.
pub(crate) fn may_open(url: &str) -> bool {
    termos_render::link::allowed(url)
        && termos_render::link::scheme(url).is_some_and(|s| SCHEMES.contains(&s.as_str()))
}

/// Starts the opener on `url` and returns at once; a thread reaps it.
pub(crate) fn open(url: &str) {
    if !may_open(url) {
        tracing::warn!("not opening an address with a scheme the client does not open");
        return;
    }
    let program = opener();
    match Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::warn!("cannot run {program} to open a link: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_addresses_are_opened() {
        assert!(may_open("https://example.com/a b"));
        assert!(may_open("mailto:me@example.com"));
        assert!(!may_open("file:///etc/passwd"));
        assert!(!may_open("javascript:alert(1)"));
        assert!(!may_open("-https://x"));
        assert!(!may_open("https://x/\u{1b}"));
    }
}
