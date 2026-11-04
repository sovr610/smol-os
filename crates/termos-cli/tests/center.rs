// SPDX-License-Identifier: GPL-3.0-or-later
//! The command center end to end (D-093): its data source against a real
//! server (sessions and panes, Go, Close, ending a real process, the
//! history of what was done), and on Unix its window in a real client,
//! where a click and `y` close a pane.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use termos_center::data::{Gather, Live, Range};
use termos_core::config::Config;
use termos_core::journal::{Event, Journal};

const T: Duration = Duration::from_secs(20);

fn panes(env: &common::Env) -> Vec<Value> {
    let out = env.run(&["msg", "pane.list"]);
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn pane_ids(env: &common::Env) -> Vec<u64> {
    panes(env)
        .iter()
        .filter_map(|p| p.get("id").and_then(Value::as_u64))
        .collect()
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + T;
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn its_actions_reach_the_server_and_the_system() {
    let t = common::Termos::start("");
    t.wait_ready();
    let env = &t.env;
    assert!(env.run(&["msg", "pane.split-right"]).status.success());
    wait("two panes", || pane_ids(env).len() == 2);
    let ids = pane_ids(env);

    let paths = env.paths();
    let journal = Arc::new(Journal::new(&paths.state_dir, 7));
    let mut live = Live::new(
        Some(env.socket()),
        Arc::clone(&journal),
        paths.clone(),
        Config::default(),
    );
    let snap = live.snapshot(Range::Hour, true);
    assert_eq!(snap.control_error, None);
    assert_eq!(snap.sessions.len(), 1);
    assert_eq!(snap.sessions[0].name, "main");
    assert_eq!(snap.panes.len(), 2);
    assert!(snap.sample.is_some(), "the machine was read");
    assert!(snap.log_file.is_some(), "the server's log");

    // Go: the first pane comes to the front.
    live.go_to_pane(ids[0]).expect("go");
    wait("the first pane focused", || {
        panes(env).iter().any(|p| {
            p.get("id").and_then(Value::as_u64) == Some(ids[0])
                && p.get("focused").and_then(Value::as_bool) == Some(true)
        })
    });
    // Close: the second pane goes.
    let said = live.close_pane(ids[1]).expect("close");
    assert!(said.contains("closed pane"), "{said}");
    wait("the pane closed", || pane_ids(env) == [ids[0]]);
    assert!(live.close_pane(999).is_err(), "no such pane");

    // End: a real process ends.
    let mut child = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/c", "ping -n 60 127.0.0.1 >nul"])
            .spawn()
    } else {
        std::process::Command::new("sleep").arg("60").spawn()
    }
    .expect("spawn");
    live.end_process(child.id(), "sleeper").expect("ended");
    let status = child.wait().expect("wait");
    assert!(!status.success(), "{status:?}");

    // What was done is in the history.
    let done: Vec<String> = journal
        .read(0)
        .into_iter()
        .filter_map(|r| match r.event {
            Event::Action { what } => Some(what),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 2, "{done:?}");
    assert!(done[0].starts_with("closed pane"));
    assert!(done[1].starts_with("ended process"));
    let again = live.snapshot(Range::Hour, true);
    assert_eq!(again.history.actions.len(), 2, "shown under History");
}

/// The window in a real client: its tabs, the server's panes, and a pane
/// closed with a click and `y`.
#[cfg(unix)]
#[test]
fn the_window_shows_panes_and_closes_one_with_a_click() {
    let t = common::Termos::start("");
    t.wait_ready();
    let env = &t.env;
    assert!(env.run(&["msg", "pane.split-right"]).status.success());
    wait("two panes", || pane_ids(env).len() == 2);
    let second = pane_ids(env)[1];

    assert!(env.run(&["msg", "center.open"]).status.success());
    t.wait_for("the command center", T, |s| {
        s.contains("Command center") && s.contains("Overview") && s.contains("System")
    });
    t.send(b"3");
    let screen = t.wait_for("its panes", T, |s| {
        s.contains("Sessions and panes") && s.contains(&format!("%{second}"))
    });
    // A click on that pane's Close, then yes.
    let (x, y) = screen
        .lines()
        .enumerate()
        .find_map(|(y, l)| {
            if !l.contains(&format!("%{second} ")) {
                return None;
            }
            let i = l.find("[Close]")?;
            Some((l[..i].chars().count(), y))
        })
        .unwrap_or_else(|| panic!("no Close for %{second}:\n{screen}"));
    let (cx, cy) = (x + 2, y + 1);
    t.send(format!("\x1b[<0;{cx};{cy}M\x1b[<0;{cx};{cy}m").as_bytes());
    t.wait_for("the question", T, |s| {
        s.contains(&format!("Close pane %{second}"))
    });
    t.send(b"y");
    wait("the pane closed", || !pane_ids(env).contains(&second));
}
