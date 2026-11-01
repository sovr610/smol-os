// SPDX-License-Identifier: GPL-3.0-or-later
//! Performance budgets (DECISIONS.md D-019), measured on the real binaries
//! (the client and the server daemon together): keystroke-to-screen
//! latency, idle CPU and memory. Linux only (reads /proc). `cargo xtask perf` runs this in release mode with the full
//! 60-second idle measurement.

#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use common::Termos;

/// CPU time (user + system) of `pid` in seconds. /proc reports clock ticks
/// in USER_HZ, which is 100 on Linux.
fn cpu_seconds(pid: u32) -> f64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    // Fields after the parenthesised command name.
    let rest = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    // utime and stime are fields 14 and 15 of the full line (11 and 12 here).
    (ticks(11) + ticks(12)) / 100.0
}

fn rss_mb(pid: u32) -> f64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|kb| kb.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

#[test]
fn latency_idle_cpu_and_memory_budgets() {
    let t = Termos::start("");
    t.wait_ready();
    let client = t.child.process_id().expect("pid");
    let info = t.run(&["msg", "server.info"]);
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).expect("server.info JSON");
    let server = info["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .expect("server pid");
    assert_ne!(client, server, "the server is a separate daemon");
    let cpu = || cpu_seconds(client) + cpu_seconds(server);
    // `cat` keeps the pane in the foreground; the TTY echoes each key.
    t.send(b"cat\r");
    std::thread::sleep(Duration::from_millis(500));

    let mut samples: Vec<Duration> = Vec::new();
    for i in 0..200u8 {
        if i % 60 == 59 {
            // Keep the line short so it never wraps.
            t.send(b"\r");
            std::thread::sleep(Duration::from_millis(50));
        }
        let g = t.generation();
        t.send(&[b'a' + i % 26]);
        samples.push(t.wait_change(g, Duration::from_secs(5)));
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    println!("keystroke->screen latency: p50 {p50:?}, p99 {p99:?}");
    // The 16 ms budget (D-019) applies to the optimized binary users run
    // (`cargo xtask perf`, CI's perf job). Unoptimized test builds on a
    // busy machine only get a loose regression guard.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(50)
    } else {
        Duration::from_millis(16)
    };
    assert!(p99 < budget, "p99 latency {p99:?} exceeds {budget:?}");

    let idle_secs: u64 = std::env::var("TERMOS_PERF_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    std::thread::sleep(Duration::from_millis(500));
    let before = cpu();
    std::thread::sleep(Duration::from_secs(idle_secs));
    let used = cpu() - before;
    let pct = used / idle_secs as f64 * 100.0;
    println!("idle CPU over {idle_secs}s: {used:.2}s = {pct:.3}%");
    assert!(pct < 1.0, "idle CPU {pct:.3}% exceeds 1%");

    let (rss_client, rss_server) = (rss_mb(client), rss_mb(server));
    let rss = rss_client + rss_server;
    println!("RSS: client {rss_client:.1} MB + server {rss_server:.1} MB = {rss:.1} MB");
    assert!(rss < 150.0, "RSS {rss:.1} MB exceeds 150 MB");

    // Throughput: an output flood is parsed in full but drawn at frame
    // rate, so it finishes quickly and the desktop stays responsive.
    t.send(b"\r\x04"); // finish the line, then end `cat`
    std::thread::sleep(Duration::from_millis(300));
    let start = std::time::Instant::now();
    t.send(b"seq 1 300000; echo flo''od-done\r");
    t.wait_for("the flood to finish", Duration::from_secs(60), |s| {
        s.lines().any(|l| l.trim() == "flood-done")
    });
    let flood = start.elapsed();
    println!("300000-line flood rendered in {flood:?}");
    assert!(flood < Duration::from_secs(30), "flood took {flood:?}");
}
