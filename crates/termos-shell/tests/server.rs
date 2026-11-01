// SPDX-License-Identifier: GPL-3.0-or-later
//! The server loop over the in-process transport: frames are incremental
//! (only changed cells are sent) and an idle desktop sends nothing.

use std::time::Duration;

use termos_core::config::Config;
use termos_core::input::KeyEvent;
use termos_core::paths::Paths;
use termos_core::proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use termos_core::transport::{ClientEnd, in_process};
use termos_render::caps::Caps;
use termos_render::diff::{apply, cell_count};
use termos_render::grid::Grid;
use termos_shell::server::{ServerChannels, run};
use termos_shell::{Desktop, DesktopOptions};
use tokio::sync::mpsc;

const COLS: u16 = 80;
const ROWS: u16 = 24;

fn start() -> (ClientEnd, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths {
        config_dir: dir.path().join("config"),
        state_dir: dir.path().join("state"),
        runtime_dir: dir.path().join("run"),
    };
    let config = Config::parse(
        "shell = \"/bin/sh\"\n[tutorial]\nshow-on-first-run = false\n[status]\nclock = false\n",
        &paths.config_file(),
    )
    .expect("config");
    let (pty_tx, pty_rx) = mpsc::channel(1024);
    let desktop = Desktop::new(
        DesktopOptions {
            session_name: "srv".into(),
            config_path: paths.config_file(),
            paths,
            env_shell: None,
            pane_ids: termos_core::ids::PaneIds::new(),
            pane_env: Vec::new(),
        },
        config,
        pty_tx,
        COLS,
        ROWS,
    )
    .expect("desktop");
    let (client, server) = in_process();
    tokio::spawn(run(
        desktop,
        ServerChannels {
            client: server,
            pty_rx,
            reload_rx: None,
        },
    ));
    client
        .tx
        .send(ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            caps: Caps::default(),
            cols: COLS,
            rows: ROWS,
        })
        .expect("server alive");
    (client, dir)
}

/// Applies frames until `pred(screen)` holds; returns the cells sent.
async fn until(
    client: &mut ClientEnd,
    mirror: &mut Grid,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> usize {
    let mut cells = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if pred(&mirror.to_text()) {
            return cells;
        }
        let msg = tokio::time::timeout_at(deadline, client.rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}:\n{}", mirror.to_text()));
        match msg {
            Some(ServerMsg::Frame(f)) => {
                if f.clear {
                    mirror.resize(f.cols, f.rows);
                }
                cells += cell_count(&f.spans);
                apply(mirror, &f.spans);
            }
            Some(_) => {}
            None => panic!("server ended"),
        }
    }
}

#[tokio::test]
async fn frames_are_incremental_and_idle_is_silent() {
    let (mut client, _dir) = start();
    let mut mirror = Grid::new(COLS, ROWS);
    for c in "echo rea''dy\r".chars() {
        let key = if c == '\r' {
            KeyEvent::plain(termos_core::input::KeyCode::Enter)
        } else {
            KeyEvent::ch(c)
        };
        client.tx.send(ClientMsg::Key(key)).expect("alive");
    }
    until(&mut client, &mut mirror, "ready", |s| {
        s.lines()
            .any(|l| l.trim_end().ends_with("ready") && !l.contains("echo"))
    })
    .await;
    // Let the prompt settle, then make sure nothing else arrives: no
    // timers, no redraws while idle.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(m) = client.rx.try_recv() {
        if let ServerMsg::Frame(f) = m {
            apply(&mut mirror, &f.spans);
        }
    }
    let idle = tokio::time::timeout(Duration::from_millis(500), client.rx.recv()).await;
    assert!(idle.is_err(), "idle desktop sent {idle:?}");

    // One keystroke echoed by the shell changes exactly one cell.
    client
        .tx
        .send(ClientMsg::Key(KeyEvent::ch('Q')))
        .expect("alive");
    let cells = until(&mut client, &mut mirror, "echoed Q", |s| {
        s.lines().last().is_some_and(|_| s.contains('Q'))
    })
    .await;
    assert_eq!(
        cells, 1,
        "a single echoed character should be a one-cell frame"
    );
}

#[tokio::test]
async fn resize_sends_a_full_frame_and_quit_ends_the_session() {
    let (mut client, _dir) = start();
    let mut mirror = Grid::new(COLS, ROWS);
    until(&mut client, &mut mirror, "status line", |s| {
        s.contains("[srv]")
    })
    .await;
    client
        .tx
        .send(ClientMsg::Resize { cols: 60, rows: 20 })
        .expect("alive");
    let mut saw_full = false;
    while !saw_full {
        match tokio::time::timeout(Duration::from_secs(5), client.rx.recv()).await {
            Ok(Some(ServerMsg::Frame(f))) => saw_full = f.clear && f.cols == 60 && f.rows == 20,
            Ok(Some(_)) => {}
            other => panic!("no full frame after resize: {other:?}"),
        }
    }
    // Quit through the palette, confirming with y.
    client
        .tx
        .send(ClientMsg::Key(KeyEvent::ctrl(' ')))
        .expect("alive");
    for c in "session.quit".chars() {
        client
            .tx
            .send(ClientMsg::Key(KeyEvent::ch(c)))
            .expect("alive");
    }
    client
        .tx
        .send(ClientMsg::Key(KeyEvent::plain(
            termos_core::input::KeyCode::Enter,
        )))
        .expect("alive");
    client
        .tx
        .send(ClientMsg::Key(KeyEvent::ch('y')))
        .expect("alive");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), client.rx.recv()).await {
            Ok(Some(ServerMsg::Exit { reason })) => {
                assert_eq!(reason, "quit");
                break;
            }
            Ok(Some(_)) => {}
            other => panic!("no exit: {other:?}"),
        }
    }
}
