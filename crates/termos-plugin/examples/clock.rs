// SPDX-License-Identifier: GPL-3.0-or-later
//! An example TermOS plugin: a clock window, a status-line widget and a
//! palette command to show or hide the window.
//!
//! Build it with `cargo build --release -p termos-plugin --example clock`,
//! then install it:
//!
//! ```text
//! ~/.config/termos/plugins/clock/
//! ├── clock          (target/release/examples/clock)
//! └── plugin.toml
//! ```
//!
//! with this `plugin.toml`:
//!
//! ```toml
//! name = "clock"
//! version = "0.1.0"
//! description = "A clock window and status widget"
//! exec = ["./clock"]
//!
//! [permissions]
//! windows = true
//! status = true
//! commands = true
//! ```
//!
//! and enable it in `config.toml` with `[plugins] enabled = ["clock"]`.
//! The time is UTC (a plugin with no dependencies has no time zone data).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use termos_plugin::{Error, Event, Line, Plugin, Span};

/// Seconds since midnight UTC, and how long until the next second.
fn now() -> (u64, Duration) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let to_next = Duration::from_secs(1)
        .saturating_sub(Duration::from_nanos(u64::from(since.subsec_nanos())));
    (since.as_secs() % 86_400, to_next)
}

fn hms(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn face(secs: u64) -> Vec<Line> {
    vec![
        Line::from(""),
        Line::Styled(vec![
            Span::new("   "),
            Span::new(hms(secs)).fg("brightcyan").bold(),
            Span::new(" UTC"),
        ]),
        Line::from(""),
        Line::Styled(vec![Span::new("   q: close").fg("brightblack")]),
    ]
}

fn main() -> Result<(), Error> {
    let mut p = Plugin::connect_from_env()?;
    p.register_command("toggle", "Show or hide the clock")?;
    let (secs, _) = now();
    let mut window = Some(p.open_window("clock", &face(secs))?);
    let mut shown_minute = None;
    loop {
        let (secs, wait) = now();
        let minute = secs / 60;
        if shown_minute != Some(minute) {
            // The status widget changes once a minute, not every second.
            shown_minute = Some(minute);
            p.set_status(vec![Span::new(format!("⏱ {}", &hms(secs)[..5]))])?;
        }
        match p.next_event_timeout(wait) {
            // A new second.
            Ok(None) => {
                if let Some(w) = window {
                    p.draw(w, &face(now().0))?;
                }
            }
            Ok(Some(Event::Command { name, .. })) if name == "toggle" => match window.take() {
                Some(w) => p.close_window(w)?,
                None => window = Some(p.open_window("clock", &face(now().0))?),
            },
            Ok(Some(Event::Key { window: w, key })) if key == "q" => {
                p.close_window(w)?;
                window = None;
            }
            Ok(Some(Event::Closed { window: w })) if Some(w) == window => window = None,
            Ok(Some(_)) => {}
            // The server stopped: so do we.
            Err(Error::Closed) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}
