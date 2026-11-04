// SPDX-License-Identifier: GPL-3.0-or-later
//! `tos` end to end (D-085): typed into a real pane's shell (`/bin/sh`, or
//! `cmd.exe` on Windows), found on the pane's `PATH`, acting on that pane
//! through the real server.

mod common;

use std::time::{Duration, Instant};

use serde_json::Value;

const T: Duration = Duration::from_secs(20);

fn panes(env: &common::Env) -> Vec<Value> {
    let out = env.run(&["msg", "pane.list"]);
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// Types `line` and Enter into the first pane of the session.
fn type_line(env: &common::Env, pane: u64, line: &str) {
    let params = serde_json::json!({ "pane": pane, "text": format!("{line}\r") }).to_string();
    let out = env.run(&["msg", "pane.send-text", &params]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + T;
    while !ok() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn tos_in_a_pane_acts_on_it() {
    let env = common::Env::new("");
    let out = env.run(&["new", "-d"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait("the first pane", || !panes(&env).is_empty());
    let first = panes(&env)[0]["id"].as_u64().expect("an id");
    // The server put `tos` next to its data.
    let link = env
        .paths()
        .data_dir
        .join("bin")
        .join(if cfg!(windows) { "tos.exe" } else { "tos" });
    wait("the tos link", || link.exists());

    // A name for this pane.
    type_line(&env, first, "tos title build-box");
    wait("the pane's name", || {
        panes(&env)
            .iter()
            .any(|p| p["id"] == first && p["name"] == "build-box")
    });
    // Copy what is piped in, paste it into a file.
    let out_file = env.path().join("pasted.txt");
    type_line(
        &env,
        first,
        if cfg!(windows) {
            "echo copied-by-tos| tos copy"
        } else {
            "echo copied-by-tos | tos copy"
        },
    );
    type_line(
        &env,
        first,
        &format!("tos paste > \"{}\"", out_file.display()),
    );
    wait("the paste", || {
        std::fs::read_to_string(&out_file).is_ok_and(|t| t.trim() == "copied-by-tos")
    });
    // Any termos command: `tos ls` lists this server's sessions.
    let ls_file = env.path().join("ls.txt");
    type_line(&env, first, &format!("tos ls > \"{}\"", ls_file.display()));
    wait("tos ls", || {
        std::fs::read_to_string(&ls_file).is_ok_and(|t| t.contains("main:"))
    });
    // A split beside this pane, with a program in it.
    type_line(&env, first, "tos split right");
    wait("a second pane", || panes(&env).len() == 2);
    let pause = if cfg!(windows) {
        "ping -n 30 127.0.0.1"
    } else {
        "sleep 30"
    };
    type_line(&env, first, &format!("tos run --float {pause}"));
    wait("a floating pane", || {
        panes(&env).iter().any(|p| p["floating"] == true)
    });
    // A program in a pane of its own, its id printed, its text read back
    // with the key it shows masked (what the agent skills do, D-098), even
    // where the pane wrapped it.
    let id_file = env.path().join("id.txt");
    let says = if cfg!(windows) {
        "cmd /k echo pane-says-hi sk-ant-fixture0123456789abcdefghijklmnopqrstuvwxyz0123456789TAILEND"
    } else {
        "sh -c 'echo pane-says-hi sk-ant-fixture0123456789abcdefghijklmnopqrstuvwxyz0123456789TAILEND; sleep 30'"
    };
    type_line(
        &env,
        first,
        &format!("tos run -p {says} > \"{}\"", id_file.display()),
    );
    wait("the new pane's id", || {
        std::fs::read_to_string(&id_file).is_ok_and(|t| t.trim().parse::<u64>().is_ok())
    });
    let id: u64 = std::fs::read_to_string(&id_file)
        .expect("id")
        .trim()
        .parse()
        .expect("a number");
    assert!(
        panes(&env).iter().any(|p| p["id"] == id),
        "a pane with that id"
    );
    let read_file = env.path().join("read.txt");
    let mut read = String::new();
    wait("tos read", || {
        type_line(
            &env,
            first,
            &format!("tos read {id} -n 20 > \"{}\"", read_file.display()),
        );
        std::thread::sleep(Duration::from_millis(300));
        read = std::fs::read_to_string(&read_file).unwrap_or_default();
        read.contains("pane-says-hi [redacted]")
    });
    assert!(
        !read.contains("sk-ant-fixture") && !read.contains("TAILEND"),
        "{read}"
    );
    let list_file = env.path().join("panes.txt");
    type_line(
        &env,
        first,
        &format!("tos panes > \"{}\"", list_file.display()),
    );
    wait("tos panes", || {
        std::fs::read_to_string(&list_file).is_ok_and(|t| {
            t.lines().any(|l| l.starts_with(&format!("*{first} "))) && t.lines().count() >= 4
        })
    });

    // The harness window, in this pane's folder (D-100).
    type_line(&env, first, "tos harness");
    wait("the harness window", || {
        panes(&env)
            .iter()
            .any(|p| p["kind"] == "app" && p["name"] == "Harness")
    });

    // Outside a pane it says what it needs.
    let out = std::process::Command::new(&link)
        .arg("notify")
        .arg("hello")
        .env_remove("TERMOS")
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("works in a TermOS pane"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And its help lists the verbs.
    let out = std::process::Command::new(&link)
        .arg("--help")
        .output()
        .expect("runs");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("split") && help.contains("notify"), "{help}");
}
